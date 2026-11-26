use super::*;
// ============================================================================
// 消息提交、路由、熔断与权限放行（Commit-before-Publish）
// ============================================================================

pub(crate) fn list_messages(
    params: GroupMessageListParams,
) -> Result<GroupMessageListResult, ControlError> {
    let store = group_store()?;
    let members = store
        .list_members(&params.group_id, true)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    let member_map: HashMap<String, &GroupMemberRecord> =
        members.iter().map(|m| (m.id.clone(), m)).collect();
    let (items, has_more) = store
        .list_messages(
            &params.group_id,
            &GroupMessageQuery {
                after_seq: params.after_seq,
                before_seq: params.before_seq,
                thread_root_seq: params.thread_root_seq,
                limit: params.limit.unwrap_or(50),
                viewer_member_id: None,
            },
        )
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    let messages = items
        .into_iter()
        .map(|m| message_to_dto(&store, &m, &member_map))
        .collect();
    Ok(GroupMessageListResult { messages, has_more })
}

/// 群消息提交参数（替代 16 参函数签名，保持调用点自描述）。
pub(super) struct PostMessageRequest<'a> {
    pub(super) group_id: &'a str,
    pub(super) sender_type: GroupSenderType,
    pub(super) sender_member_id: Option<&'a str>,
    pub(super) kind: GroupMessageKind,
    pub(super) content: String,
    pub(super) explicit_mentions: Vec<String>,
    pub(super) reply_to_seq: Option<i64>,
    pub(super) thread_root_seq: Option<i64>,
    pub(super) attachments: Vec<GroupAttachment>,
    pub(super) idempotency_key: Option<String>,
    pub(super) pinned: bool,
    pub(super) auto_bridged: bool,
    pub(super) source_chain_depth: Option<u32>,
}

pub(crate) fn post_user_message(
    params: GroupMessagePostParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<AgentGroupMessageDto, ControlError> {
    let store = group_store()?;
    post_message_inner(
        &store,
        PostMessageRequest {
            group_id: &params.group_id,
            sender_type: GroupSenderType::User,
            sender_member_id: None,
            kind: params.kind,
            content: params.content,
            explicit_mentions: params.mentions,
            reply_to_seq: params.reply_to_seq,
            thread_root_seq: params.thread_root_seq,
            attachments: params.attachments,
            idempotency_key: params.idempotency_key,
            pinned: params.pinned,
            auto_bridged: false,
            source_chain_depth: None,
        },
        acp_sessions,
        subscribers,
    )
}

pub(super) fn post_message_inner(
    store: &GroupStore,
    req: PostMessageRequest<'_>,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<AgentGroupMessageDto, ControlError> {
    let PostMessageRequest {
        group_id,
        sender_type,
        sender_member_id,
        kind,
        content,
        explicit_mentions,
        reply_to_seq,
        thread_root_seq,
        attachments,
        idempotency_key,
        pinned,
        auto_bridged,
        source_chain_depth,
    } = req;
    if content.trim().is_empty() {
        return Err(err(
            ControlErrorCode::InvalidParams,
            "Message content can't be empty",
        ));
    }
    let group = store
        .get_group(group_id)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| {
            err(
                ControlErrorCode::NotFound,
                format!("Group not found: {group_id}"),
            )
        })?;
    if group.archived_at_ms.is_some() {
        return Err(err(
            ControlErrorCode::FailedPrecondition,
            "Group is archived",
        ));
    }
    let members = store
        .list_members(group_id, false)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    let member_map: HashMap<String, &GroupMemberRecord> =
        members.iter().map(|m| (m.id.clone(), m)).collect();
    let alias_map: HashMap<String, &GroupMemberRecord> =
        members.iter().map(|m| (m.alias.clone(), m)).collect();

    // 1. 解析目标成员集合（按 GroupMode + auto_bridge 护栏）
    let mut target_specs: Vec<(&GroupMemberRecord, GroupMentionKind)> = Vec::new();
    let mut seen_targets: HashSet<String> = HashSet::new();

    let allow_mentions = !auto_bridged || group.auto_bridge_mentions;
    if allow_mentions
        && !matches!(
            kind,
            GroupMessageKind::BlindSubmission | GroupMessageKind::System
        )
    {
        let parsed = extract_mentions(&content);
        let mut requested_all = parsed.all;
        let mut requested_aliases = parsed.aliases;
        for raw in explicit_mentions {
            let trimmed = raw.trim().trim_start_matches('@').to_ascii_lowercase();
            if trimmed == "all" {
                requested_all = true;
            } else if !trimmed.is_empty() {
                requested_aliases.push(trimmed);
            }
        }
        if requested_all {
            for m in &members {
                if Some(m.id.as_str()) != sender_member_id && seen_targets.insert(m.id.clone()) {
                    target_specs.push((m, GroupMentionKind::All));
                }
            }
        }
        for alias in requested_aliases {
            if let Some(m) = alias_map.get(&alias)
                && Some(m.id.as_str()) != sender_member_id
                && seen_targets.insert(m.id.clone())
            {
                // Coordinator 模式下若关闭横向互聊，非 coordinator 只能 @ coordinator
                if group.mode == "coordinator"
                    && !group.allow_worker_cross_talk
                    && sender_type == GroupSenderType::Member
                    && sender_member_id != group.coordinator_member_id.as_deref()
                    && Some(m.id.as_str()) != group.coordinator_member_id.as_deref()
                {
                    continue;
                }
                target_specs.push((m, GroupMentionKind::Direct));
            }
        }
        // Coordinator 模式：人类不 @ 任何人 → 默认唤醒 Coordinator
        if target_specs.is_empty()
            && group.mode == "coordinator"
            && sender_type == GroupSenderType::User
            && let Some(coord_id) = group.coordinator_member_id.as_deref()
            && let Some(coord) = member_map.get(coord_id)
        {
            target_specs.push((coord, GroupMentionKind::CoordinatorDefault));
        }
    }

    // 2. 链预算、深度与乒乓熔断计算
    let now = now_ms();
    let (chain_id, next_depth, mention_rows) = {
        let mut state = lock_state();
        if sender_type == GroupSenderType::User {
            // 人类发话：刷新 chain_id，重置跳数预算，抢占清空旧链未发排队
            let new_chain_id = short_id("chn");
            state.pending_wakes.retain(|(gid, _), _| gid != group_id);
            state.chains.insert(
                group_id.to_string(),
                ChainBudget {
                    chain_id: new_chain_id.clone(),
                    hops_left: group.max_chain_hops,
                    pair_counts: HashMap::new(),
                    approved_targets: HashSet::new(),
                },
            );
            let rows = target_specs
                .iter()
                .map(|(target, mkind)| {
                    let (status, reason) = if target.muted {
                        (GroupDeliveryStatus::Skipped, Some("muted".to_string()))
                    } else if group.paused {
                        (
                            GroupDeliveryStatus::Queued,
                            Some("group_paused".to_string()),
                        )
                    } else {
                        (GroupDeliveryStatus::Queued, None)
                    };
                    NewGroupMention {
                        target_member_id: target.id.clone(),
                        mention_kind: mkind.as_str().to_string(),
                        delivery_status: status.as_str().to_string(),
                        delivery_id: Some(short_id("gd")),
                        status_reason: reason,
                        updated_at_ms: now,
                    }
                })
                .collect::<Vec<_>>();
            (Some(new_chain_id), 0u32, rows)
        } else {
            // Agent 发言：检查深度、跳数预算、乒乓计数与三态权限水位线
            let depth = source_chain_depth.unwrap_or(0).saturating_add(1);
            let sender_posture = sender_member_id
                .and_then(|sid| member_map.get(sid))
                .map(|m| member_approval_posture(m, acp_sessions))
                .unwrap_or(ApprovalPosture::Unknown);
            let budget = state
                .chains
                .entry(group_id.to_string())
                .or_insert_with(|| ChainBudget {
                    chain_id: short_id("chn"),
                    hops_left: group.max_chain_hops,
                    pair_counts: HashMap::new(),
                    approved_targets: HashSet::new(),
                });
            let chain_id = budget.chain_id.clone();
            let mut rows = Vec::with_capacity(target_specs.len());
            for (target, mkind) in &target_specs {
                let target_posture = member_approval_posture(target, acp_sessions);
                let (status, reason) = if target.muted {
                    (GroupDeliveryStatus::Skipped, Some("muted".to_string()))
                } else if depth > group.max_chain_depth {
                    (
                        GroupDeliveryStatus::Skipped,
                        Some("max_depth_reached".to_string()),
                    )
                } else if budget.hops_left == 0 {
                    (
                        GroupDeliveryStatus::Skipped,
                        Some("hops_exhausted".to_string()),
                    )
                } else if let Some(sender_id) = sender_member_id
                    && *budget
                        .pair_counts
                        .get(&normalized_pair(sender_id, &target.id))
                        .unwrap_or(&0)
                        >= group.max_pair_ping_pong
                {
                    (
                        GroupDeliveryStatus::Skipped,
                        Some("ping_pong_tripped".to_string()),
                    )
                } else if needs_permission_approval(sender_posture, target_posture)
                    && !budget.approved_targets.contains(&target.id)
                {
                    (
                        GroupDeliveryStatus::PendingApproval,
                        Some("permission_escalation".to_string()),
                    )
                } else {
                    budget.hops_left = budget.hops_left.saturating_sub(1);
                    if let Some(sender_id) = sender_member_id {
                        *budget
                            .pair_counts
                            .entry(normalized_pair(sender_id, &target.id))
                            .or_insert(0) += 1;
                    }
                    (GroupDeliveryStatus::Queued, None)
                };
                rows.push(NewGroupMention {
                    target_member_id: target.id.clone(),
                    mention_kind: mkind.as_str().to_string(),
                    delivery_status: status.as_str().to_string(),
                    delivery_id: Some(short_id("gd")),
                    status_reason: reason,
                    updated_at_ms: now,
                });
            }
            (Some(chain_id), depth, rows)
        }
    };

    // 3. Commit-before-Publish：单事务原子落盘消息 + mention 行
    let attachments_json = serde_json::to_vec(&attachments).unwrap_or_else(|_| b"[]".to_vec());
    let task_status = if kind == GroupMessageKind::Task {
        Some(GroupTaskStatus::Open.as_str().to_string())
    } else {
        None
    };
    let msg_id = short_id("gmsg");
    let (stored_msg, stored_mentions, is_replay) = store
        .append_message_with_mentions(
            &NewGroupMessage {
                id: msg_id,
                group_id: group_id.to_string(),
                sender_type: sender_type.as_str().to_string(),
                sender_member_id: sender_member_id.map(ToOwned::to_owned),
                kind: kind.as_str().to_string(),
                content,
                attachments_json,
                reply_to_seq,
                thread_root_seq,
                blind_deadline_at_ms: None,
                blind_revealed_at_ms: None,
                task_status,
                chain_id: chain_id.clone(),
                chain_depth: next_depth,
                auto_bridged,
                pinned,
                idempotency_key,
                created_at_ms: now,
            },
            &mention_rows,
        )
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;

    // 发送成员推进自身已读游标并标记本轮已发言（阻止回合结束重复桥接）
    if let Some(sender_id) = sender_member_id {
        let _ = store.advance_member_cursors(
            sender_id,
            Some(stored_msg.seq),
            Some(stored_msg.seq),
            false,
        );
        if let Some(sender_rec) = member_map.get(sender_id)
            && let Some(turn) = lock_state().active_turns.get_mut(&sender_rec.session_id)
            && turn.group_id == group_id
        {
            turn.posted_or_yielded = true;
        }
    }

    let dto = message_to_dto_with_mentions(&stored_msg, &stored_mentions, &member_map);
    if is_replay {
        return Ok(dto);
    }

    // 4. 事务已提交，推送事件并按 Delta Coalescing 派发唤醒
    notify_message_appended(
        subscribers,
        json!({
            "event": "message_appended",
            "group_id": group_id,
            "message": dto,
        }),
    );
    notify_groups_changed(subscribers, acp_sessions);

    if !group.paused {
        for mention in &stored_mentions {
            if mention.delivery_status != "queued" {
                continue;
            }
            if let Some(target) = member_map.get(&mention.target_member_id) {
                enqueue_wake_slot(
                    group_id,
                    &target.id,
                    stored_msg.id.clone(),
                    stored_msg.seq,
                    "mentioned",
                    None,
                    chain_id.clone(),
                    next_depth,
                );
                if target.runtime_id.is_none() {
                    let _ =
                        try_dispatch_local_wake(store, group_id, target, acp_sessions, subscribers);
                } else {
                    // 远端成员：若无活跃流，先记为 offline_pending，重连自动补发
                    let _ = dispatch_remote_wake_or_mark_offline(
                        store,
                        group_id,
                        target,
                        &mention.message_id,
                        acp_sessions,
                    );
                }
            }
        }
    }

    Ok(dto)
}

/// 三态权限水位线判定：是否需要人类放行卡。
pub(crate) fn needs_permission_approval(sender: ApprovalPosture, target: ApprovalPosture) -> bool {
    match sender {
        ApprovalPosture::FullAccess => false,
        ApprovalPosture::RequiresApproval | ApprovalPosture::Unknown => {
            !matches!(target, ApprovalPosture::RequiresApproval)
        }
    }
}

pub(crate) fn approve_mention(
    params: GroupMentionDecisionParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), ControlError> {
    let store = group_store()?;
    let now = now_ms();
    let updated = store
        .update_mention_status(
            &params.message_id,
            &params.target_member_id,
            GroupDeliveryStatus::Queued.as_str(),
            None,
            None,
            now,
        )
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    if !updated {
        return Err(err(ControlErrorCode::NotFound, "Pending mention not found"));
    }
    if params.allow_chain {
        let mut state = lock_state();
        if let Some(budget) = state.chains.get_mut(&params.group_id) {
            budget
                .approved_targets
                .insert(params.target_member_id.clone());
        }
    }
    if let Ok(Some(target)) = store.get_member(&params.target_member_id) {
        enqueue_wake_slot(
            &params.group_id,
            &target.id,
            params.message_id.clone(),
            0,
            "approved",
            None,
            None,
            0,
        );
        if target.runtime_id.is_none() {
            let _ = try_dispatch_local_wake(
                &store,
                &params.group_id,
                &target,
                acp_sessions,
                subscribers,
            );
        }
    }
    notify_groups_changed(subscribers, acp_sessions);
    Ok(())
}

pub(crate) fn reject_mention(
    params: GroupMentionDecisionParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), ControlError> {
    let store = group_store()?;
    let updated = store
        .update_mention_status(
            &params.message_id,
            &params.target_member_id,
            GroupDeliveryStatus::Rejected.as_str(),
            None,
            params.reason.as_deref().or(Some("rejected_by_user")),
            now_ms(),
        )
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    if !updated {
        return Err(err(ControlErrorCode::NotFound, "Pending mention not found"));
    }
    notify_groups_changed(subscribers, acp_sessions);
    Ok(())
}

// ============================================================================
// Delta Coalescing 唤醒派发 + 回合结束自动桥接（Auto-Bridge）
// ============================================================================

#[allow(clippy::too_many_arguments)]
pub(super) fn enqueue_wake_slot(
    group_id: &str,
    member_id: &str,
    message_id: String,
    seq: i64,
    trigger_label: &str,
    blind_prompt_seq: Option<i64>,
    chain_id: Option<String>,
    chain_depth: u32,
) {
    let mut state = lock_state();
    let key = (group_id.to_string(), member_id.to_string());
    state
        .pending_wakes
        .entry(key)
        .and_modify(|slot| {
            if !slot.trigger_message_ids.contains(&message_id) {
                slot.trigger_message_ids.push(message_id.clone());
            }
            if seq > 0 && !slot.trigger_seqs.contains(&seq) {
                slot.trigger_seqs.push(seq);
            }
            if blind_prompt_seq.is_some() {
                slot.blind_prompt_seq = blind_prompt_seq;
                slot.trigger_label = trigger_label.to_string();
            }
            slot.chain_id = chain_id.clone().or_else(|| slot.chain_id.clone());
            slot.chain_depth = slot.chain_depth.max(chain_depth);
        })
        .or_insert_with(|| PendingGroupWake {
            group_id: group_id.to_string(),
            member_id: member_id.to_string(),
            trigger_message_ids: vec![message_id],
            trigger_seqs: if seq > 0 { vec![seq] } else { Vec::new() },
            trigger_label: trigger_label.to_string(),
            blind_prompt_seq,
            chain_id,
            chain_depth,
        });
}

fn is_local_session_busy(session_id: &str, acp_sessions: &AcpSessions) -> bool {
    let Some(slot) = acp_sessions.get(session_id) else {
        return false;
    };
    let reduced = slot.value.reduced.lock();
    reduced.replaying_history
        || !matches!(reduced.phase, smelt_core::daemon_state::DaemonPhase::Idle)
}

pub(super) fn ensure_local_session_awake(
    session_id: &str,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), ControlError> {
    if acp_sessions.get(session_id).is_none()
        && let Some(server) = crate::protocol::SERVER.get()
    {
        crate::acp_host::ensure_conversation_runtime(
            session_id,
            acp_sessions,
            &server.workspace_directory,
            subscribers,
        )
        .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
    }
    if let Some(slot) = acp_sessions.get(session_id) {
        crate::acp_idle::wake_suspended_acp_session(session_id, &slot, acp_sessions, subscribers)
            .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
        crate::acp_idle::wait_for_acp_ready(&slot.value, crate::acp_idle::ACP_WAKE_READY_TIMEOUT)
            .map_err(|e| err(ControlErrorCode::OperationFailed, e))?;
    }
    Ok(())
}

/// 若本机成员当前空闲，立即弹出合并槽位、按增量游标打包 `<group-message>` 并异步投递。
pub(super) fn try_dispatch_local_wake(
    store: &GroupStore,
    group_id: &str,
    member: &GroupMemberRecord,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), ControlError> {
    if is_local_session_busy(&member.session_id, acp_sessions) {
        // 成员正忙：留在 pending_wakes 槽位里合并后续增量，回合结束由 drain_pending_wake 驱动
        return Ok(());
    }
    let Some(wake) = lock_state()
        .pending_wakes
        .remove(&(group_id.to_string(), member.id.clone()))
    else {
        return Ok(());
    };
    let store = store.clone();
    let member = member.clone();
    let acp_sessions = acp_sessions.clone();
    let subscribers = subscribers.clone();
    std::thread::spawn(move || {
        if let Err(error) =
            deliver_local_wake_now(&store, &member, &wake, &acp_sessions, &subscribers)
        {
            crate::dlog(&format!(
                "[group] deliver wake to {} (@{}) failed: {}",
                member.session_id, member.alias, error.message
            ));
        }
    });
    Ok(())
}

pub(super) fn deliver_local_wake_now(
    store: &GroupStore,
    member: &GroupMemberRecord,
    wake: &PendingGroupWake,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), ControlError> {
    let Some(group) = store
        .get_group(&wake.group_id)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
    else {
        return Ok(());
    };
    if group.paused || group.archived_at_ms.is_some() {
        return Ok(());
    }
    let mark_failed = |reason: &str| {
        lock_state().active_turns.remove(&member.session_id);
        let now = now_ms();
        for msg_id in &wake.trigger_message_ids {
            let _ = store.update_mention_status(
                msg_id,
                &member.id,
                GroupDeliveryStatus::Failed.as_str(),
                None,
                Some(reason),
                now,
            );
        }
        notify_groups_changed(subscribers, acp_sessions);
    };
    if let Err(error) = ensure_local_session_awake(&member.session_id, acp_sessions, subscribers) {
        mark_failed(&error.message);
        return Err(error);
    }
    let (envelope, max_seq) =
        build_delta_envelope(store, &group, member, wake, None, acp_sessions)?;
    let delivery_id = short_id("gd");
    {
        lock_state().active_turns.insert(
            member.session_id.clone(),
            ActiveGroupTurn {
                group_id: group.id.clone(),
                member_id: member.id.clone(),
                trigger_seqs: wake.trigger_seqs.clone(),
                blind_prompt_seq: wake.blind_prompt_seq,
                chain_id: wake.chain_id.clone(),
                chain_depth: wake.chain_depth,
                posted_or_yielded: false,
            },
        );
    }
    let Some(slot) = acp_sessions.get(&member.session_id) else {
        let error = err(
            ControlErrorCode::NotFound,
            format!("ACP session not found: {}", member.session_id),
        );
        mark_failed(&error.message);
        return Err(error);
    };
    let action = AcpUserAction::Prompt {
        text: envelope,
        images: Vec::new(),
        delivery_id: Some(delivery_id.clone()),
    };
    let send_res = acp_sessions
        .with_current(&member.session_id, &slot, |session| {
            crate::acp_host::apply_acp_user_action(session, action, subscribers)
        })
        .ok_or_else(|| err(ControlErrorCode::NotFound, "ACP session closed"))
        .and_then(|res| res.map_err(|e| err(ControlErrorCode::OperationFailed, e)));
    if let Err(error) = send_res {
        mark_failed(&error.message);
        return Err(error);
    }

    let now = now_ms();
    let _ = store.advance_member_cursors(&member.id, Some(max_seq), None, true);
    for (idx, msg_id) in wake.trigger_message_ids.iter().enumerate() {
        let status = if idx + 1 == wake.trigger_message_ids.len() {
            GroupDeliveryStatus::Delivered
        } else {
            GroupDeliveryStatus::Coalesced
        };
        let _ = store.update_mention_status(
            msg_id,
            &member.id,
            status.as_str(),
            Some(&delivery_id),
            None,
            now,
        );
    }
    notify_groups_changed(subscribers, acp_sessions);
    Ok(())
}

/// 打包增量 `<group-message>` 信封（带硬上限折叠、首次入群 Roster Briefing 和未完成任务摘要）。
pub(super) fn build_delta_envelope(
    store: &GroupStore,
    group: &GroupRecord,
    member: &GroupMemberRecord,
    wake: &PendingGroupWake,
    remote_origin_node: Option<&str>,
    acp_sessions: &AcpSessions,
) -> Result<(String, i64), ControlError> {
    let members = store
        .list_members(&group.id, false)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    let member_map: HashMap<String, &GroupMemberRecord> =
        members.iter().map(|m| (m.id.clone(), m)).collect();
    let (delta_msgs, _) = store
        .list_messages(
            &group.id,
            &GroupMessageQuery {
                after_seq: Some(member.last_delivered_seq),
                limit: 200,
                viewer_member_id: Some(member.id.clone()),
                ..Default::default()
            },
        )
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;

    let first_seq = delta_msgs
        .first()
        .map(|m| m.seq)
        .unwrap_or(group.next_seq - 1);
    let last_seq = delta_msgs
        .last()
        .map(|m| m.seq)
        .unwrap_or(group.next_seq - 1);
    let seq_range = if first_seq == last_seq {
        format!("{last_seq}")
    } else {
        format!("{first_seq}..{last_seq}")
    };

    let messages_block =
        format_delta_messages_with_folding(&delta_msgs, &member_map, &wake.trigger_seqs);
    let roster_block = if !member.briefing_sent {
        let lines = members
            .iter()
            .map(|m| {
                let role = if m.role_title.is_empty() {
                    m.role_kind.as_str()
                } else {
                    m.role_title.as_str()
                };
                format!("- @{} ({role})", m.alias)
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(lines)
    } else {
        None
    };

    let open_tasks = store.list_open_tasks(&group.id).unwrap_or_default();
    let active_tasks_block = if open_tasks.is_empty() {
        None
    } else {
        let lines = open_tasks
            .iter()
            .take(12)
            .map(|t| {
                let status = t.task_status.as_deref().unwrap_or("open");
                format!(
                    "- [#{}] ({status}): {}",
                    t.seq,
                    truncate_preview(&t.content, 120)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(lines)
    };

    let hops_left = lock_state()
        .chains
        .get(&group.id)
        .map(|b| b.hops_left)
        .unwrap_or(group.max_chain_hops);
    let mcp_connected = session_has_mcp_or_pi(&member.session_id, acp_sessions);
    let (cwd, branch) = resolve_session_cwd_and_branch(&member.session_id);
    let envelope = format_group_message_envelope(&GroupEnvelopeInput {
        group_id: &group.id,
        group_title: &group.title,
        mode: GroupMode::parse(&group.mode).unwrap_or(GroupMode::Mention),
        seq_range: &seq_range,
        trigger: &wake.trigger_label,
        hops_left,
        recipient_alias: &member.alias,
        recipient_role: if member.role_title.is_empty() {
            &member.role_kind
        } else {
            &member.role_title
        },
        recipient_cwd: cwd.as_deref(),
        recipient_branch: branch.as_deref(),
        remote_origin_node,
        mcp_connected,
        blind_prompt_seq: wake.blind_prompt_seq,
        roster_block: roster_block.as_deref(),
        role_prompt: (!member.briefing_sent && !member.role_prompt.is_empty())
            .then_some(member.role_prompt.as_str()),
        active_tasks_block: active_tasks_block.as_deref(),
        messages_block: &messages_block,
    });
    Ok((envelope, last_seq))
}

pub(super) fn format_delta_messages_with_folding(
    msgs: &[GroupMessageRecord],
    member_map: &HashMap<String, &GroupMemberRecord>,
    trigger_seqs: &[i64],
) -> String {
    let total_bytes: usize = msgs.iter().map(|m| m.content.len() + 64).sum();
    let need_fold = msgs.len() > DELTA_MAX_MESSAGES || total_bytes > DELTA_MAX_BYTES;
    let mut out = Vec::new();
    let keep_tail_from = msgs.len().saturating_sub(8);
    let mut folded_start: Option<i64> = None;
    let mut folded_end: i64 = 0;
    let mut folded_count: usize = 0;

    let flush_folded = |out: &mut Vec<String>,
                        start: &mut Option<i64>,
                        end: i64,
                        count: &mut usize| {
        if let Some(s) = start.take() {
            out.push(format!(
                "[已折叠 #{s}..#{end} 共 {} 条未点名你的讨论，如需细节请调用 group_read(after_seq={}, before_seq={})]",
                *count,
                s - 1,
                end + 1
            ));
            *count = 0;
        }
    };

    for (idx, msg) in msgs.iter().enumerate() {
        let keep = !need_fold
            || idx >= keep_tail_from
            || msg.pinned
            || msg.kind == "decision"
            || msg.kind == "blind_prompt"
            || trigger_seqs.contains(&msg.seq);
        if !keep {
            if folded_start.is_none() {
                folded_start = Some(msg.seq);
            }
            folded_end = msg.seq;
            folded_count += 1;
            continue;
        }
        flush_folded(&mut out, &mut folded_start, folded_end, &mut folded_count);
        let sender = match msg.sender_type.as_str() {
            "user" => "user (用户)".to_string(),
            "system" => "system".to_string(),
            _ => msg
                .sender_member_id
                .as_deref()
                .and_then(|id| member_map.get(id))
                .map(|m| format!("@{}", m.alias))
                .unwrap_or_else(|| "member".to_string()),
        };
        out.push(format!(
            "[seq={} · {} · {}]\n{}",
            msg.seq, sender, msg.kind, msg.content
        ));
    }
    flush_folded(&mut out, &mut folded_start, folded_end, &mut folded_count);
    out.join("\n\n")
}

/// 由 `acp_host` 回合结束事件调用：
/// 1. 若该会话刚完成一个群唤醒回合且模型忘了调 `group_post`/`group_blind_submit`/`group_yield`，执行自动桥接兜底；
/// 2. 检查是否有在忙期间合并积压的 `PendingGroupWake`，立即事件驱动派发下一批增量。
pub(crate) fn on_member_turn_ended(
    session_id: &str,
    reply_text: String,
    outcome: Option<AcpTurnOutcome>,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let active_turn = lock_state().active_turns.remove(session_id);
    if let Some(turn) = active_turn
        && !turn.posted_or_yielded
        && let Ok(store) = group_store()
    {
        let body = match (
            (!reply_text.trim().is_empty()).then_some(reply_text),
            outcome,
        ) {
            (Some(text), _) => Some(text),
            (None, Some(AcpTurnOutcome::Failed)) => {
                Some("（本轮执行失败，未产出群回复，请点击「看过程」查看私有会话详情）".to_string())
            }
            _ => None,
        };
        if let Some(text) = body {
            if let Some(prompt_seq) = turn.blind_prompt_seq {
                let _ = submit_blind_entry(
                    &store,
                    &turn.group_id,
                    &turn.member_id,
                    prompt_seq,
                    text,
                    Vec::new(),
                    true,
                    acp_sessions,
                    subscribers,
                );
            } else {
                let _ = post_message_inner(
                    &store,
                    PostMessageRequest {
                        group_id: &turn.group_id,
                        sender_type: GroupSenderType::Member,
                        sender_member_id: Some(&turn.member_id),
                        kind: GroupMessageKind::Chat,
                        content: text,
                        explicit_mentions: Vec::new(),
                        reply_to_seq: turn.trigger_seqs.last().copied(),
                        thread_root_seq: None,
                        attachments: Vec::new(),
                        idempotency_key: None,
                        pinned: false,
                        auto_bridged: true,
                        source_chain_depth: Some(turn.chain_depth),
                    },
                    acp_sessions,
                    subscribers,
                );
            }
        }
    }

    // 派发该会话在忙期间积压的 Delta Coalescing 槽位
    drain_pending_wakes_for_session(session_id, acp_sessions, subscribers);
}

fn drain_pending_wakes_for_session(
    session_id: &str,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let Ok(store) = group_store() else {
        return;
    };
    let Ok(memberships) = store.find_active_members_by_session(None, session_id) else {
        return;
    };
    for member in memberships {
        let has_pending = lock_state()
            .pending_wakes
            .contains_key(&(member.group_id.clone(), member.id.clone()));
        if has_pending {
            let _ = try_dispatch_local_wake(
                &store,
                &member.group_id,
                &member,
                acp_sessions,
                subscribers,
            );
        }
    }
}
