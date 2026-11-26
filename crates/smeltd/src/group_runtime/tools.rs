use super::*;
// ============================================================================
// 内存态顾问式文件锁（shared_leased 模式，按 (group_id, runtime_id, repo_root) 隔离）
// ============================================================================

pub(crate) fn acquire_file_lease(
    group_id: &str,
    member: &GroupMemberRecord,
    repo_root: &str,
    path_globs: Vec<String>,
    reason: String,
    ttl_sec: Option<u64>,
) -> Result<GroupLeaseDto, String> {
    if path_globs.is_empty() {
        return Err("path_globs can't be empty".to_string());
    }
    let now = now_ms();
    let ttl_ms = ttl_sec
        .map(|s| ((s as i64).saturating_mul(1000)).clamp(30_000, MAX_LEASE_TTL_MS))
        .unwrap_or(DEFAULT_LEASE_TTL_MS);
    let mut state = lock_state();
    state.leases.retain(|l| l.expires_at_ms > now);
    for existing in &state.leases {
        if existing.group_id == group_id
            && existing.runtime_id == member.runtime_id
            && existing.repo_root == repo_root
            && existing.holder_member_id != member.id
            && globs_overlap(&existing.path_globs, &path_globs)
        {
            return Err(format!(
                "File lease conflict with @{} (globs: {:?}, reason: {}, expires_at_ms: {})",
                existing.holder_alias, existing.path_globs, existing.reason, existing.expires_at_ms
            ));
        }
    }
    // 同成员续期或更新
    if let Some(existing) = state.leases.iter_mut().find(|l| {
        l.group_id == group_id
            && l.runtime_id == member.runtime_id
            && l.repo_root == repo_root
            && l.holder_member_id == member.id
            && l.path_globs == path_globs
    }) {
        existing.expires_at_ms = now + ttl_ms;
        if !reason.is_empty() {
            existing.reason = reason;
        }
        return Ok(lease_to_dto(existing));
    }
    let lease = MemoryFileLease {
        id: short_id("gl"),
        group_id: group_id.to_string(),
        runtime_id: member.runtime_id.clone(),
        repo_root: repo_root.to_string(),
        holder_member_id: member.id.clone(),
        holder_alias: member.alias.clone(),
        path_globs,
        reason,
        expires_at_ms: now + ttl_ms,
    };
    let dto = lease_to_dto(&lease);
    state.leases.push(lease);
    Ok(dto)
}

pub(crate) fn release_file_lease(params: GroupLeaseReleaseParams) -> Result<(), ControlError> {
    let mut state = lock_state();
    let before = state.leases.len();
    state
        .leases
        .retain(|l| !(l.group_id == params.group_id && l.id == params.lease_id));
    if state.leases.len() == before {
        return Err(err(ControlErrorCode::NotFound, "Lease not found"));
    }
    Ok(())
}

pub(crate) fn list_active_leases(group_id: &str) -> Vec<GroupLeaseDto> {
    let now = now_ms();
    let mut state = lock_state();
    state.leases.retain(|l| l.expires_at_ms > now);
    state
        .leases
        .iter()
        .filter(|l| l.group_id == group_id)
        .map(lease_to_dto)
        .collect()
}

fn globs_overlap(a: &[String], b: &[String]) -> bool {
    for ga in a {
        let pa = glob_prefix(ga);
        for gb in b {
            let pb = glob_prefix(gb);
            if pa.is_empty() || pb.is_empty() || pa.starts_with(pb) || pb.starts_with(pa) {
                return true;
            }
        }
    }
    false
}

fn glob_prefix(glob: &str) -> &str {
    let trimmed = glob.trim().trim_start_matches("./");
    match trimmed.find(['*', '?', '[']) {
        Some(idx) => trimmed[..idx].trim_end_matches('/'),
        None => trimmed.trim_end_matches('/'),
    }
}

// ============================================================================
// Agent 侧工具统一分发（MCP / Pi / CLI 三通道共用同一份逻辑）
// ============================================================================

/// 判断某会话是否属于至少一个未归档群（含作为远端群主成员的附着），供 `--peer-mcp` / Pi 动态暴露工具。
pub(crate) fn session_in_any_group(session_id: &str) -> bool {
    if lock_state()
        .remote_attachments
        .iter()
        .any(|att| att.session_id == session_id)
    {
        return true;
    }
    group_store()
        .ok()
        .and_then(|s| s.find_active_members_by_session(None, session_id).ok())
        .is_some_and(|list| !list.is_empty())
}

/// 统一执行群工具（本机来源 `session_id` 由 `PeerSocket` 或 `peer_pid` 进程树认定，绝不信任调用参数）。
pub(crate) fn dispatch_agent_group_tool(
    op: &str,
    caller_session_id: &str,
    args: &Value,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Value {
    dispatch_agent_group_tool_for_runtime(
        op,
        None,
        caller_session_id,
        args,
        acp_sessions,
        subscribers,
    )
}

/// 统一执行群工具，支持本机成员（`caller_runtime_id = None`）与跨机器远端成员（`Some(runtime_id)`）。
pub(crate) fn dispatch_agent_group_tool_for_runtime(
    op: &str,
    caller_runtime_id: Option<&str>,
    caller_session_id: &str,
    args: &Value,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Value {
    match dispatch_agent_group_tool_inner(
        op,
        caller_runtime_id,
        caller_session_id,
        args,
        acp_sessions,
        subscribers,
    ) {
        Ok((text, details)) => json!({ "ok": true, "text": text, "details": details }),
        Err(error) => json!({ "ok": false, "error": error, "text": error }),
    }
}

fn dispatch_agent_group_tool_inner(
    op: &str,
    caller_runtime_id: Option<&str>,
    caller_session_id: &str,
    args: &Value,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(String, Value), String> {
    let store = group_store().map_err(|e| e.message)?;
    dispatch_agent_group_tool_on(
        &store,
        op,
        caller_runtime_id,
        caller_session_id,
        args,
        acp_sessions,
        subscribers,
    )
}

pub(super) fn dispatch_agent_group_tool_on(
    store: &GroupStore,
    op: &str,
    caller_runtime_id: Option<&str>,
    caller_session_id: &str,
    args: &Value,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(String, Value), String> {
    let requested_group_id = args
        .get("group_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // 若本机调用方会话归属远端群主：先在本机完成 Push-Only 产物校验/登记（不泄露本机原始路径），再透传回群主
    if caller_runtime_id.is_none()
        && let Some(att) = find_remote_attachment(caller_session_id, requested_group_id)
    {
        let mut forwarded_args = args.clone();
        if matches!(op, "group_post" | "group_blind_submit")
            && args.get("artifacts").is_some()
            && let Some(obj) = forwarded_args.as_object_mut()
        {
            let mut published = parse_and_register_published_artifacts(
                &att.group_id,
                caller_session_id,
                args.get("artifacts"),
            )?;
            let self_rt = crate::runtime_identity::runtime_id()
                .as_deref()
                .map(ToOwned::to_owned);
            for item in &mut published {
                item.runtime_id = self_rt.clone();
                item.ref_path = None;
            }
            obj.insert(
                "artifacts".into(),
                serde_json::to_value(published).unwrap_or_else(|_| json!([])),
            );
        }
        return forward_remote_member_tool_call(&att, op, &forwarded_args);
    }

    let memberships = store
        .find_active_members_by_session(caller_runtime_id, caller_session_id)
        .map_err(|e| e.to_string())?;
    let member = match requested_group_id {
        Some(gid) => memberships
            .into_iter()
            .find(|m| m.group_id == gid)
            .ok_or_else(|| format!("This conversation is not an active member of group `{gid}`"))?,
        None => {
            if memberships.len() == 1 {
                memberships.into_iter().next().unwrap()
            } else if memberships.is_empty() {
                return Err("This conversation does not belong to any active group".to_string());
            } else {
                let ids: Vec<_> = memberships.iter().map(|m| m.group_id.as_str()).collect();
                return Err(format!(
                    "This conversation belongs to multiple groups ({}); please pass group_id",
                    ids.join(", ")
                ));
            }
        }
    };

    match op {
        "group_members" => {
            let members = store
                .list_members(&member.group_id, false)
                .map_err(|e| e.to_string())?;
            let dtos: Vec<AgentGroupMemberDto> = members
                .iter()
                .map(|m| member_to_dto(m, acp_sessions))
                .collect();
            let summary = dtos
                .iter()
                .map(|m| {
                    format!(
                        "- @{} [{}] ({:?}, {:?}, cwd={})",
                        m.alias,
                        m.agent_label.as_deref().unwrap_or("agent"),
                        m.role_kind,
                        m.live_status,
                        m.cwd.as_deref().unwrap_or("-")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok((
                summary,
                json!({ "group_id": member.group_id, "members": dtos }),
            ))
        }
        "group_read" => {
            let after_seq = args.get("after_seq").and_then(Value::as_i64);
            let before_seq = args.get("before_seq").and_then(Value::as_i64);
            let thread_root_seq = args.get("thread_root_seq").and_then(Value::as_i64);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(30)
                .clamp(1, 100) as usize;
            let (msgs, has_more) = store
                .list_messages(
                    &member.group_id,
                    &GroupMessageQuery {
                        after_seq,
                        before_seq,
                        thread_root_seq,
                        limit,
                        viewer_member_id: Some(member.id.clone()),
                    },
                )
                .map_err(|e| e.to_string())?;
            if let Some(last) = msgs.last() {
                let _ = store.advance_member_cursors(&member.id, None, Some(last.seq), false);
            }
            let members = store
                .list_members(&member.group_id, true)
                .map_err(|e| e.to_string())?;
            let member_map: HashMap<String, &GroupMemberRecord> =
                members.iter().map(|m| (m.id.clone(), m)).collect();
            let text = if msgs.is_empty() {
                "（无更多群消息）".to_string()
            } else {
                format_delta_messages_with_folding(&msgs, &member_map, &[])
            };
            let dtos: Vec<AgentGroupMessageDto> = msgs
                .iter()
                .map(|m| message_to_dto(&store, m, &member_map))
                .collect();
            Ok((
                text,
                json!({ "group_id": member.group_id, "messages": dtos, "has_more": has_more }),
            ))
        }
        "group_post" => {
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mentions: Vec<String> = args
                .get("mentions")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let kind = args
                .get("kind")
                .and_then(Value::as_str)
                .and_then(GroupMessageKind::parse)
                .unwrap_or(GroupMessageKind::Chat);
            let reply_to_seq = args.get("reply_to_seq").and_then(Value::as_i64);
            let thread_root_seq = args.get("thread_root_seq").and_then(Value::as_i64);
            let idempotency_key = args
                .get("toolCallId")
                .and_then(Value::as_str)
                .map(|id| format!("{}:{id}", member.id));
            let attachments = if caller_runtime_id.is_some() {
                parse_remote_published_attachments(caller_runtime_id, args.get("artifacts"))?
            } else {
                parse_and_register_published_artifacts(
                    &member.group_id,
                    caller_session_id,
                    args.get("artifacts"),
                )?
            };
            let source_depth = lock_state()
                .active_turns
                .get(caller_session_id)
                .map(|t| t.chain_depth);
            let dto = post_message_inner(
                store,
                PostMessageRequest {
                    group_id: &member.group_id,
                    sender_type: GroupSenderType::Member,
                    sender_member_id: Some(&member.id),
                    kind,
                    content,
                    explicit_mentions: mentions,
                    reply_to_seq,
                    thread_root_seq,
                    attachments,
                    idempotency_key,
                    pinned: kind == GroupMessageKind::Decision,
                    auto_bridged: false,
                    source_chain_depth: source_depth,
                },
                acp_sessions,
                subscribers,
            )
            .map_err(|e| e.message)?;
            Ok((
                format!("已发送到群 {}（seq={}）", member.group_id, dto.seq),
                json!({ "group_id": member.group_id, "seq": dto.seq, "message_id": dto.id }),
            ))
        }
        "group_yield" => {
            let group = store
                .get_group(&member.group_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "Group not found".to_string())?;
            let _ = store.advance_member_cursors(
                &member.id,
                None,
                Some((group.next_seq - 1).max(0)),
                false,
            );
            if let Some(turn) = lock_state().active_turns.get_mut(caller_session_id)
                && turn.group_id == member.group_id
            {
                turn.posted_or_yielded = true;
            }
            let reason = args
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("acknowledged");
            Ok((
                format!("已确认静默结束本轮（{reason}）"),
                json!({ "group_id": member.group_id, "yielded": true }),
            ))
        }
        "group_blind_submit" => {
            let prompt_seq = args
                .get("prompt_seq")
                .and_then(Value::as_i64)
                .ok_or_else(|| "group_blind_submit requires prompt_seq".to_string())?;
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let attachments = if caller_runtime_id.is_some() {
                parse_remote_published_attachments(caller_runtime_id, args.get("artifacts"))?
            } else {
                parse_and_register_published_artifacts(
                    &member.group_id,
                    caller_session_id,
                    args.get("artifacts"),
                )?
            };
            let (dto, revealed) = submit_blind_entry(
                &store,
                &member.group_id,
                &member.id,
                prompt_seq,
                content,
                attachments,
                false,
                acp_sessions,
                subscribers,
            )
            .map_err(|e| e.message)?;
            let text = if revealed {
                format!("盲评已提交（seq={}），全员已交齐并自动揭晓。", dto.seq)
            } else {
                format!("盲评已提交（seq={}），等待其他成员交卷后揭晓。", dto.seq)
            };
            Ok((
                text,
                json!({ "group_id": member.group_id, "seq": dto.seq, "revealed": revealed }),
            ))
        }
        "group_task_update" => {
            let seq = args
                .get("seq")
                .and_then(Value::as_i64)
                .ok_or_else(|| "group_task_update requires seq".to_string())?;
            let status_raw = args
                .get("task_status")
                .and_then(Value::as_str)
                .ok_or_else(|| "group_task_update requires task_status".to_string())?;
            let task_status = GroupTaskStatus::parse(status_raw)
                .ok_or_else(|| format!("Invalid task_status: {status_raw}"))?;
            let dto = update_task_status(
                GroupTaskUpdateParams {
                    group_id: member.group_id.clone(),
                    seq,
                    task_status,
                },
                acp_sessions,
                subscribers,
            )
            .map_err(|e| e.message)?;
            Ok((
                format!("任务 #{} 状态已更新为 {}", dto.seq, task_status.as_str()),
                json!({ "group_id": member.group_id, "seq": dto.seq, "task_status": task_status }),
            ))
        }
        "group_lease_acquire" => {
            let globs: Vec<String> = args
                .get("path_globs")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let reason = args
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let ttl_sec = args.get("ttl_sec").and_then(Value::as_u64);
            let (cwd, _) = resolve_session_cwd_and_branch(&member.session_id);
            let lease = acquire_file_lease(
                &member.group_id,
                &member,
                cwd.as_deref().unwrap_or_default(),
                globs,
                reason,
                ttl_sec,
            )?;
            Ok((
                format!("已获取文件锁 {}（{:?}）", lease.id, lease.path_globs),
                serde_json::to_value(&lease).unwrap_or_default(),
            ))
        }
        "group_lease_release" => {
            let lease_id = args
                .get("lease_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "group_lease_release requires lease_id".to_string())?;
            release_file_lease(GroupLeaseReleaseParams {
                group_id: member.group_id.clone(),
                lease_id: lease_id.to_string(),
            })
            .map_err(|e| e.message)?;
            Ok((
                format!("已释放文件锁 {lease_id}"),
                json!({ "lease_id": lease_id }),
            ))
        }
        "group_artifact_fetch" => {
            let artifact_id = args
                .get("artifact_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "group_artifact_fetch requires artifact_id".to_string())?;
            let text = match read_published_artifact(&member.group_id, artifact_id) {
                Ok(text) => text,
                Err(local_err) => {
                    if let Ok(Some(origin_rt)) =
                        store.find_attachment_runtime_id(&member.group_id, artifact_id)
                    {
                        let reply = crate::peer_nodes::call_node_group_deliver(
                            &origin_rt,
                            json!({
                                "op": "artifact_read",
                                "group_id": member.group_id,
                                "artifact_id": artifact_id,
                            }),
                        )?;
                        reply
                            .get("content")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                            .ok_or_else(|| {
                                format!("Remote node `{origin_rt}` returned empty artifact content")
                            })?
                    } else {
                        return Err(local_err);
                    }
                }
            };
            Ok((text, json!({ "artifact_id": artifact_id })))
        }
        "group_worktree_merge" => {
            let source_alias = args
                .get("source_alias")
                .and_then(Value::as_str)
                .ok_or_else(|| "group_worktree_merge requires source_alias".to_string())?;
            let members = store
                .list_members(&member.group_id, false)
                .map_err(|e| e.to_string())?;
            let normalized = validate_member_alias(source_alias)?;
            let source = members
                .iter()
                .find(|m| m.alias == normalized)
                .ok_or_else(|| format!("Member @{normalized} not found"))?;
            let target = match args.get("target").and_then(Value::as_str) {
                Some("base") => GroupMergeTarget::Base,
                _ => GroupMergeTarget::Integration,
            };
            let strategy = match args.get("strategy").and_then(Value::as_str) {
                Some("squash") => GroupMergeStrategy::Squash,
                Some("cherry_pick") => GroupMergeStrategy::CherryPick,
                _ => GroupMergeStrategy::Merge,
            };
            let res = merge_member_worktree(GroupWorktreeMergeParams {
                group_id: member.group_id.clone(),
                member_id: source.id.clone(),
                target,
                strategy,
            })
            .map_err(|e| e.message)?;
            Ok((
                format!(
                    "已将 @{} 合并到 {}（commit {}）",
                    source.alias, res.target_branch, res.commit_sha
                ),
                serde_json::to_value(&res).unwrap_or_default(),
            ))
        }
        other => Err(format!("Unknown group tool: {other}")),
    }
}
