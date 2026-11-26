use super::*;
// ============================================================================
// 跨机器零信任白名单、Push-Only 产物沙箱、防攻破校验与短连接兜底
// ============================================================================

pub(crate) fn upsert_remote_grant(
    params: GroupRemoteGrantUpsertParams,
) -> Result<RemoteGroupGrantDto, ControlError> {
    let store = group_store()?;
    let now = now_ms();
    let rec = RemoteGroupMemberGrantRecord {
        host_runtime_id: params.host_runtime_id,
        group_id: params.group_id,
        member_id: params.member_id,
        local_session_id: params.local_session_id,
        allow_remote_full_access: params.allow_remote_full_access,
        created_at_ms: now,
    };
    store
        .upsert_remote_grant(&rec)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    // 同步登记内存态远端附着，使本地 MCP/Pi 立即可见群工具
    let mut state = lock_state();
    state.remote_attachments.retain(|a| {
        !(a.host_runtime_id == rec.host_runtime_id
            && a.group_id == rec.group_id
            && a.member_id == rec.member_id)
    });
    state.remote_attachments.push(RemoteGroupAttachment {
        host_runtime_id: rec.host_runtime_id.clone(),
        group_id: rec.group_id.clone(),
        member_id: rec.member_id.clone(),
        alias: rec.member_id.clone(),
        session_id: rec.local_session_id.clone(),
    });
    drop(state);
    if let Some(shared) = crate::protocol::SERVER.get() {
        spawn_outbound_group_attach(
            rec.host_runtime_id.clone(),
            std::sync::Arc::clone(&shared.acp_sessions),
            std::sync::Arc::clone(&shared.event_hub),
        );
    }
    Ok(RemoteGroupGrantDto {
        host_runtime_id: rec.host_runtime_id,
        group_id: rec.group_id,
        member_id: rec.member_id,
        local_session_id: rec.local_session_id,
        allow_remote_full_access: rec.allow_remote_full_access,
        created_at_ms: rec.created_at_ms,
    })
}

pub(crate) fn revoke_remote_grant(
    params: GroupRemoteGrantRevokeParams,
) -> Result<(), ControlError> {
    let store = group_store()?;
    store
        .revoke_remote_grant(&params.host_runtime_id, &params.group_id, &params.member_id)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    lock_state().remote_attachments.retain(|a| {
        !(a.host_runtime_id == params.host_runtime_id
            && a.group_id == params.group_id
            && a.member_id == params.member_id)
    });
    Ok(())
}

/// 本机用户跨机发消息签发的一次性 nonce 有效期（5 分钟）。
pub(super) const LOCAL_NONCE_TTL_MS: i64 = 5 * 60 * 1000;
/// 本机未消费 nonce 内存上限，防止异常堆积。
const MAX_LOCAL_NONCES: usize = 1024;

/// 签发一次性 `local_user_origin_nonce`（当本机人类向远端群主发消息时登记，5 分钟过期）。
#[allow(dead_code)]
pub(crate) fn issue_local_user_origin_nonce() -> String {
    let now = now_ms();
    let nonce = format!("unonce-{}", uuid::Uuid::new_v4().simple());
    let mut state = lock_state();
    state.local_user_nonces.retain(|_, exp| *exp > now);
    if state.local_user_nonces.len() >= MAX_LOCAL_NONCES
        && let Some(oldest_key) = state
            .local_user_nonces
            .iter()
            .min_by_key(|(_, exp)| **exp)
            .map(|(k, _)| k.clone())
    {
        state.local_user_nonces.remove(&oldest_key);
    }
    state
        .local_user_nonces
        .insert(nonce.clone(), now + LOCAL_NONCE_TTL_MS);
    nonce
}

pub(super) fn consume_local_user_origin_nonce(nonce: &str, now: i64) -> bool {
    let mut state = lock_state();
    state.local_user_nonces.retain(|_, exp| *exp > now);
    state
        .local_user_nonces
        .remove(nonce)
        .is_some_and(|exp| exp > now)
}

/// 零信任核心门卫：校验来自远端群主 `caller_runtime_id` 的下行指令（详见 `docs/agent-groups.md` §7.3）。
pub(crate) fn handle_inbound_remote_deliver(
    caller_runtime_id: &str,
    caller_inbound: bool,
    payload: &Value,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<Value, ControlError> {
    let store = group_store()?;
    handle_inbound_remote_deliver_on(
        &store,
        caller_runtime_id,
        caller_inbound,
        payload,
        acp_sessions,
        subscribers,
    )
}

pub(super) fn handle_inbound_remote_deliver_on(
    store: &GroupStore,
    caller_runtime_id: &str,
    caller_inbound: bool,
    payload: &Value,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<Value, ControlError> {
    let frame_op = payload
        .get("op")
        .and_then(Value::as_str)
        .unwrap_or_default();

    // 1. inbound=0 硬禁止所有远端控制面帧
    if !caller_inbound
        && matches!(
            frame_op,
            "spawn_or_wake" | "worktree_merge" | "worktree_apply_patch"
        )
    {
        return Err(err(
            ControlErrorCode::PermissionDenied,
            format!(
                "Remote host `{caller_runtime_id}` is not permitted to invoke control frame `{frame_op}` (inbound=0)"
            ),
        ));
    }

    match frame_op {
        "artifact_read" => {
            // 严禁按路径读本机文件，只允许读本机显式发布的 published_artifact_id
            if payload.get("path").is_some() || payload.get("ref_path").is_some() {
                return Err(err(
                    ControlErrorCode::PermissionDenied,
                    "Arbitrary path reads from remote nodes are forbidden; pass published artifact_id only",
                ));
            }
            let group_id = payload
                .get("group_id")
                .and_then(Value::as_str)
                .ok_or_else(|| err(ControlErrorCode::InvalidParams, "missing group_id"))?;
            let artifact_id = payload
                .get("artifact_id")
                .and_then(Value::as_str)
                .ok_or_else(|| err(ControlErrorCode::InvalidParams, "missing artifact_id"))?;
            let content = read_published_artifact(group_id, artifact_id)
                .map_err(|e| err(ControlErrorCode::PermissionDenied, e))?;
            Ok(json!({ "ok": true, "content": content }))
        }
        "deliver_wake" => {
            let group_id = payload
                .get("group_id")
                .and_then(Value::as_str)
                .ok_or_else(|| err(ControlErrorCode::InvalidParams, "missing group_id"))?;
            let member_id = payload
                .get("member_id")
                .and_then(Value::as_str)
                .ok_or_else(|| err(ControlErrorCode::InvalidParams, "missing member_id"))?;
            let text = payload
                .get("envelope")
                .and_then(Value::as_str)
                .ok_or_else(|| err(ControlErrorCode::InvalidParams, "missing envelope"))?;
            let delivery_id = payload
                .get("delivery_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| short_id("gd"));

            // 必须命中本机白名单授权表
            let grant = store
                .get_remote_grant(caller_runtime_id, group_id, member_id)
                .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
                .ok_or_else(|| {
                    err(
                        ControlErrorCode::PermissionDenied,
                        "No local remote_group_member_grant for this remote host/group/member",
                    )
                })?;

            // 速率限流防被攻破远端刷爆本机模型额度
            enforce_remote_wake_rate_limit(caller_runtime_id, &grant.local_session_id)?;

            // 绝不信任远端声称的 trigger_posture = User，除非携带本机签发且未过期的一次性 nonce
            let now = now_ms();
            let verified_local_user = payload
                .get("local_user_origin_nonce")
                .and_then(Value::as_str)
                .is_some_and(|nonce| consume_local_user_origin_nonce(nonce, now));

            let target_posture = session_approval_posture(&grant.local_session_id, acp_sessions);
            if !verified_local_user
                && matches!(
                    target_posture,
                    ApprovalPosture::FullAccess | ApprovalPosture::Unknown
                )
                && !grant.allow_remote_full_access
            {
                return Err(err(
                    ControlErrorCode::PermissionDenied,
                    "Remote wake blocked: target local session is FullAccess and allow_remote_full_access is disabled on this machine",
                ));
            }

            ensure_local_session_awake(&grant.local_session_id, acp_sessions, subscribers)?;
            let slot = acp_sessions.get(&grant.local_session_id).ok_or_else(|| {
                err(
                    ControlErrorCode::NotFound,
                    format!("Local session not active: {}", grant.local_session_id),
                )
            })?;
            let action = AcpUserAction::Prompt {
                text: text.to_string(),
                images: Vec::new(),
                delivery_id: Some(delivery_id),
            };
            acp_sessions
                .with_current(&grant.local_session_id, &slot, |session| {
                    crate::acp_host::apply_acp_user_action(session, action, subscribers)
                })
                .ok_or_else(|| err(ControlErrorCode::NotFound, "Local session closed"))?
                .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
            Ok(json!({ "ok": true, "delivered": true }))
        }
        other => Err(err(
            ControlErrorCode::InvalidParams,
            format!("Unsupported node.group_deliver op: {other}"),
        )),
    }
}

/// 群主侧处理来自远端成员节点的 `node.group_call`（严格校验 `Principal::Peer` 与 `agent_group_member` 白名单）。
pub(crate) fn handle_inbound_node_group_call(
    caller_runtime_id: &str,
    payload: &Value,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<Value, ControlError> {
    let store = group_store()?;
    let group_id = payload
        .get("group_id")
        .and_then(Value::as_str)
        .ok_or_else(|| err(ControlErrorCode::InvalidParams, "missing group_id"))?;
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| err(ControlErrorCode::InvalidParams, "missing session_id"))?;
    let op = payload
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| err(ControlErrorCode::InvalidParams, "missing op"))?;
    let memberships = store
        .find_active_members_by_session(Some(caller_runtime_id), session_id)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    if !memberships.iter().any(|m| m.group_id == group_id) {
        return Err(err(
            ControlErrorCode::PermissionDenied,
            "Remote node session is not an active member of this group",
        ));
    }
    let args = payload.get("args").cloned().unwrap_or_else(|| json!({}));
    Ok(dispatch_agent_group_tool_for_runtime(
        op,
        Some(caller_runtime_id),
        session_id,
        &args,
        acp_sessions,
        subscribers,
    ))
}

fn enforce_remote_wake_rate_limit(
    host_runtime_id: &str,
    local_session_id: &str,
) -> Result<(), ControlError> {
    let now = now_ms();
    let cutoff = now - 60_000;
    let mut state = lock_state();
    let bucket = state
        .remote_wake_window
        .entry((host_runtime_id.to_string(), local_session_id.to_string()))
        .or_default();
    while bucket.front().is_some_and(|&ts| ts < cutoff) {
        bucket.pop_front();
    }
    if bucket.len() >= REMOTE_WAKE_RATE_LIMIT_PER_MIN {
        return Err(err(
            ControlErrorCode::TemporarilyUnavailable,
            "Remote wake rate limit exceeded for this local session",
        ));
    }
    bucket.push_back(now);
    Ok(())
}

/// 群主侧接收来自远端成员节点已校验发布的附件元数据（不访问群主本机磁盘，并绑定来源 `runtime_id`）。
pub(super) fn parse_remote_published_attachments(
    caller_runtime_id: Option<&str>,
    raw: Option<&Value>,
) -> Result<Vec<GroupAttachment>, String> {
    let Some(items) = raw.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let mut att: GroupAttachment =
            serde_json::from_value(item.clone()).map_err(|e| format!("Invalid artifact: {e}"))?;
        if att.id.trim().is_empty() {
            att.id = short_id("gart");
        }
        if att.runtime_id.is_none() {
            att.runtime_id = caller_runtime_id.map(ToOwned::to_owned);
        }
        // 绝不把远端磁盘绝对路径落进群主消息，防止敏感路径泄露
        att.ref_path = None;
        out.push(att);
    }
    Ok(out)
}

/// Push-Only 产物发布：校验 `realpath` 严格位于成员工作区内、拦截敏感路径并登记白名单。
pub(super) fn parse_and_register_published_artifacts(
    group_id: &str,
    caller_session_id: &str,
    raw: Option<&Value>,
) -> Result<Vec<GroupAttachment>, String> {
    let Some(items) = raw.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let (allowed_cwd, _) = resolve_session_cwd_and_branch(caller_session_id);
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let mut att: GroupAttachment =
            serde_json::from_value(item.clone()).map_err(|e| format!("Invalid artifact: {e}"))?;
        if att.id.trim().is_empty() {
            att.id = short_id("gart");
        }
        if let Some(ref_path) = att.ref_path.as_deref().filter(|s| !s.is_empty()) {
            let canonical = validate_artifact_path(ref_path, allowed_cwd.as_deref())?;
            lock_state().published_artifacts.insert(
                att.id.clone(),
                PublishedArtifactEntry {
                    canonical_path: canonical,
                    start_line: att.start_line,
                    end_line: att.end_line,
                    group_id: group_id.to_string(),
                },
            );
        }
        out.push(att);
    }
    Ok(out)
}

pub(super) fn validate_artifact_path(
    raw_path: &str,
    allowed_cwd: Option<&str>,
) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(raw_path)
        .map_err(|e| format!("Artifact path `{raw_path}` cannot be resolved: {e}"))?;
    if let Some(cwd) = allowed_cwd.filter(|s| !s.is_empty()) {
        let canonical_cwd = std::fs::canonicalize(cwd)
            .map_err(|e| format!("Allowed cwd `{cwd}` cannot be resolved: {e}"))?;
        if !canonical.starts_with(&canonical_cwd) {
            return Err(format!(
                "Artifact path `{raw_path}` escapes member workspace `{cwd}`"
            ));
        }
    }
    let lowered = canonical.to_string_lossy().to_ascii_lowercase();
    for forbidden in [
        ".env",
        ".git/config",
        "iroh-secret",
        "id_rsa",
        "id_ed25519",
        "/.smelt/",
    ] {
        if lowered.contains(forbidden) {
            return Err(format!(
                "Artifact path `{raw_path}` hits forbidden sensitive path pattern `{forbidden}`"
            ));
        }
    }
    Ok(canonical)
}

pub(super) fn read_published_artifact(group_id: &str, artifact_id: &str) -> Result<String, String> {
    let entry = lock_state()
        .published_artifacts
        .get(artifact_id)
        .cloned()
        .ok_or_else(|| format!("Artifact `{artifact_id}` is not published on this node"))?;
    if entry.group_id != group_id {
        return Err("Artifact does not belong to this group".to_string());
    }
    let bytes = std::fs::read(&entry.canonical_path)
        .map_err(|e| format!("Failed to read artifact `{artifact_id}`: {e}"))?;
    let text = String::from_utf8_lossy(&bytes);
    let sliced = match (entry.start_line, entry.end_line) {
        (Some(s), Some(e)) if s >= 1 && e >= s => text
            .lines()
            .skip((s - 1) as usize)
            .take((e - s + 1) as usize)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => text.into_owned(),
    };
    if sliced.len() > MAX_ARTIFACT_BYTES {
        let cut = sliced.floor_char_boundary(MAX_ARTIFACT_BYTES);
        Ok(format!(
            "{}\n... [truncated at {cut} bytes]",
            &sliced[..cut],
        ))
    } else {
        Ok(sliced)
    }
}

pub(super) fn dispatch_remote_wake_or_mark_offline(
    store: &GroupStore,
    group_id: &str,
    member: &GroupMemberRecord,
    message_id: &str,
    _acp_sessions: &AcpSessions,
) -> Result<(), ControlError> {
    let Some(remote_id) = member.runtime_id.as_deref() else {
        return Ok(());
    };
    let Some(wake) = lock_state()
        .pending_wakes
        .get(&(group_id.to_string(), member.id.clone()))
        .cloned()
    else {
        return Ok(());
    };
    let Some(group) = store
        .get_group(group_id)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
    else {
        return Ok(());
    };
    let host_name = smelt_sys::hostname().unwrap_or_else(|| "remote-host".to_string());
    let (envelope, _max_seq) = build_delta_envelope(
        store,
        &group,
        member,
        &wake,
        Some(&host_name),
        _acp_sessions,
    )?;
    let delivery_id = short_id("gd");
    let payload = json!({
        "op": "deliver_wake",
        "group_id": group_id,
        "member_id": member.id,
        "envelope": envelope,
        "delivery_id": delivery_id,
    });
    // 优先走该远端节点主动建立的反向附着 QUIC 长流（`inbound=0` / NAT 后无需入站权限）；
    // 若无长流，再降级到短连接控制调用 `node.group_deliver`。
    let attached_tx = lock_state().attached_peer_streams.get(remote_id).cloned();
    let delivered = if let Some(tx) = attached_tx {
        tx.send(payload.clone())
            .map(|_| json!({ "ok": true, "via": "node_group_attach" }))
            .map_err(|_| "attached reverse stream closed".to_string())
    } else {
        crate::peer_nodes::call_node_group_deliver(remote_id, payload)
    };
    match delivered {
        Ok(_) => {
            lock_state()
                .pending_wakes
                .remove(&(group_id.to_string(), member.id.clone()));
            let _ = store.update_mention_status(
                message_id,
                &member.id,
                GroupDeliveryStatus::Delivered.as_str(),
                Some(&delivery_id),
                None,
                now_ms(),
            );
            Ok(())
        }
        Err(reason) => {
            lock_state().remote_live_status.insert(
                (remote_id.to_string(), member.session_id.clone()),
                GroupMemberLiveStatus::Offline,
            );
            let _ = store.update_mention_status(
                message_id,
                &member.id,
                GroupDeliveryStatus::OfflinePending.as_str(),
                Some(&delivery_id),
                Some(&reason),
                now_ms(),
            );
            Ok(())
        }
    }
}

/// 群主侧处理 `op=node_group_attach`：接受已配对成员节点的主动反向附着流，
/// 下发 `attach_sync`、冲刷离线待投递消息，并在单条双向 QUIC 流上持续处理成员 RPC 与状态更新。
pub(crate) fn handle_node_group_attach<C: crate::client_conn::ClientSocket>(
    conn: C,
    reader: std::io::BufReader<C>,
    principal: &crate::devices::Principal,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let crate::devices::Principal::Peer { runtime_id, .. } = principal else {
        let mut conn = conn;
        let _ = smelt_runtime_client::protocol::write_json_line(
            &mut conn,
            &json!({ "ok": false, "error": "node_group_attach requires a paired peer principal" }),
        );
        return;
    };
    let Ok(store) = group_store() else {
        return;
    };
    handle_node_group_attach_on(&store, runtime_id, conn, reader, acp_sessions, subscribers);
}

pub(super) fn handle_node_group_attach_on<C: crate::client_conn::ClientSocket>(
    store: &GroupStore,
    peer_runtime_id: &str,
    mut conn: C,
    mut reader: std::io::BufReader<C>,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    use std::io::BufRead as _;

    // 收集该远端节点在群主上的全部活跃成员映射
    let mut synced_members = Vec::new();
    if let Ok(groups) = store.list_groups(false) {
        for group in groups {
            if let Ok(members) = store.list_members(&group.id, false) {
                for m in members {
                    if m.runtime_id.as_deref() == Some(peer_runtime_id) {
                        synced_members.push(json!({
                            "group_id": m.group_id,
                            "member_id": m.id,
                            "alias": m.alias,
                            "session_id": m.session_id,
                        }));
                    }
                }
            }
        }
    }
    let _ = smelt_runtime_client::protocol::write_json_line(
        &mut conn,
        &json!({
            "op": "attach_sync",
            "ok": true,
            "members": synced_members,
        }),
    );

    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    lock_state()
        .attached_peer_streams
        .insert(peer_runtime_id.to_string(), tx.clone());

    let writer_handle = std::thread::spawn(move || {
        while let Ok(frame) = rx.recv() {
            if smelt_runtime_client::protocol::write_json_line(&mut conn, &frame).is_err() {
                break;
            }
        }
    });

    // 重连即刻冲刷 offline_pending 投递
    if let Ok(pending) = store.list_pending_mentions(None) {
        for mention in pending {
            if mention.delivery_status != "offline_pending" {
                continue;
            }
            if let Ok(Some(member)) = store.get_member(&mention.target_member_id)
                && member.runtime_id.as_deref() == Some(peer_runtime_id)
            {
                enqueue_wake_slot(
                    &mention.group_id,
                    &member.id,
                    mention.message_id.clone(),
                    0,
                    "reconnected",
                    None,
                    None,
                    0,
                );
                let _ = dispatch_remote_wake_or_mark_offline(
                    store,
                    &mention.group_id,
                    &member,
                    &mention.message_id,
                    acp_sessions,
                );
            }
        }
    }

    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && let Ok(frame) = serde_json::from_str::<Value>(trimmed)
        {
            match frame.get("op").and_then(Value::as_str).unwrap_or_default() {
                "member_rpc" => {
                    let req_id = frame.get("request_id").cloned().unwrap_or(Value::Null);
                    let reply = handle_inbound_node_group_call(
                        peer_runtime_id,
                        &frame,
                        acp_sessions,
                        subscribers,
                    )
                    .unwrap_or_else(|e| json!({ "ok": false, "error": e.message }));
                    let _ = tx.send(json!({
                        "op": "member_rpc_reply",
                        "request_id": req_id,
                        "reply": reply,
                    }));
                }
                "member_status" => {
                    if let (Some(sid), Some(raw_status)) = (
                        frame.get("session_id").and_then(Value::as_str),
                        frame.get("live_status").and_then(Value::as_str),
                    ) {
                        let status = match raw_status {
                            "running" => GroupMemberLiveStatus::Running,
                            "waiting_approval" => GroupMemberLiveStatus::WaitingApproval,
                            "offline" => GroupMemberLiveStatus::Offline,
                            _ => GroupMemberLiveStatus::Idle,
                        };
                        lock_state()
                            .remote_live_status
                            .insert((peer_runtime_id.to_string(), sid.to_string()), status);
                        notify_groups_changed(subscribers, acp_sessions);
                    }
                }
                "turn_ended" => {
                    if let Some(sid) = frame.get("session_id").and_then(Value::as_str) {
                        let reply = frame
                            .get("reply")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        on_member_turn_ended(sid, reply, None, acp_sessions, subscribers);
                    }
                }
                _ => {}
            }
        }
        line.clear();
    }

    lock_state().attached_peer_streams.remove(peer_runtime_id);
    drop(tx);
    let _ = writer_handle.join();
}

/// 成员侧处理 `op=node_group_link`（仅允许 `inbound=1` 对端直连，每帧同样经过零信任门卫）。
pub(crate) fn handle_node_group_link<C: crate::client_conn::ClientSocket>(
    mut conn: C,
    mut reader: std::io::BufReader<C>,
    principal: &crate::devices::Principal,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    use std::io::BufRead as _;

    let crate::devices::Principal::Peer {
        runtime_id,
        inbound: true,
        ..
    } = principal
    else {
        let _ = smelt_runtime_client::protocol::write_json_line(
            &mut conn,
            &json!({ "ok": false, "error": "node_group_link requires an inbound-allowed peer" }),
        );
        return;
    };
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && let Ok(frame) = serde_json::from_str::<Value>(trimmed)
        {
            let reply =
                handle_inbound_remote_deliver(runtime_id, true, &frame, acp_sessions, subscribers)
                    .unwrap_or_else(|e| json!({ "ok": false, "error": e.message }));
            if smelt_runtime_client::protocol::write_json_line(&mut conn, &reply).is_err() {
                break;
            }
        }
        line.clear();
    }
}

/// 成员节点向远端群主主动发起 `op=node_group_attach` 反向附着流（完全事件驱动，零轮询）。
/// 所有下行帧强制以 `caller_inbound = false` 通过零信任门卫校验。
pub(crate) fn spawn_outbound_group_attach(
    host_runtime_id: String,
    acp_sessions: AcpSessions,
    subscribers: EventHubHandle,
) {
    {
        let mut state = lock_state();
        if !state
            .active_outbound_attaches
            .insert(host_runtime_id.clone())
        {
            return;
        }
    }
    std::thread::spawn(move || {
        use std::io::BufRead as _;

        if let Ok(mut stream) = crate::peer_nodes::open_peer_stream(&host_runtime_id)
            && smelt_runtime_client::protocol::write_json_line(
                &mut stream,
                &json!({ "op": "node_group_attach" }),
            )
            .is_ok()
        {
            let mut reader = std::io::BufReader::new(stream);
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) {
                let trimmed = line.trim();
                if !trimmed.is_empty()
                    && let Ok(frame) = serde_json::from_str::<Value>(trimmed)
                {
                    let op = frame.get("op").and_then(Value::as_str).unwrap_or_default();
                    if op == "attach_sync" {
                        apply_attach_sync(&host_runtime_id, &frame);
                    } else {
                        // 反向附着流上收到的所有下行帧一律按 inbound=false 零信任门卫校验
                        let _ = handle_inbound_remote_deliver(
                            &host_runtime_id,
                            false,
                            &frame,
                            &acp_sessions,
                            &subscribers,
                        );
                    }
                }
                line.clear();
            }
        }
        lock_state()
            .active_outbound_attaches
            .remove(&host_runtime_id);
    });
}

fn apply_attach_sync(host_runtime_id: &str, frame: &Value) {
    let Some(members) = frame.get("members").and_then(Value::as_array) else {
        return;
    };
    let mut state = lock_state();
    state
        .remote_attachments
        .retain(|a| a.host_runtime_id != host_runtime_id);
    for item in members {
        if let (Some(group_id), Some(member_id), Some(session_id)) = (
            item.get("group_id").and_then(Value::as_str),
            item.get("member_id").and_then(Value::as_str),
            item.get("session_id").and_then(Value::as_str),
        ) {
            state.remote_attachments.push(RemoteGroupAttachment {
                host_runtime_id: host_runtime_id.to_string(),
                group_id: group_id.to_string(),
                member_id: member_id.to_string(),
                alias: item
                    .get("alias")
                    .and_then(Value::as_str)
                    .unwrap_or(member_id)
                    .to_string(),
                session_id: session_id.to_string(),
            });
        }
    }
}

pub(super) fn find_remote_attachment(
    session_id: &str,
    group_id: Option<&str>,
) -> Option<RemoteGroupAttachment> {
    let state = lock_state();
    state
        .remote_attachments
        .iter()
        .find(|a| a.session_id == session_id && group_id.is_none_or(|gid| a.group_id == gid))
        .cloned()
}

pub(super) fn forward_remote_member_tool_call(
    att: &RemoteGroupAttachment,
    op: &str,
    args: &Value,
) -> Result<(String, Value), String> {
    let payload = json!({
        "group_id": att.group_id,
        "session_id": att.session_id,
        "op": op,
        "args": args,
    });
    let reply = crate::peer_nodes::call_node_group_call(&att.host_runtime_id, payload)?;
    let ok = reply.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let text = reply
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if ok {
        Ok((text, reply.get("details").cloned().unwrap_or_default()))
    } else {
        Err(reply
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("remote group call failed")
            .to_string())
    }
}
