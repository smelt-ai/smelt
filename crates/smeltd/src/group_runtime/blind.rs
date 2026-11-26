use super::*;
// ============================================================================
// 并发盲评（Blind Round）：栅栏隔离 + 全员交卷自动揭晓
// ============================================================================

pub(crate) fn start_blind_round(
    params: GroupBlindStartParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<AgentGroupMessageDto, ControlError> {
    let store = group_store()?;
    if params.prompt.trim().is_empty() {
        return Err(err(
            ControlErrorCode::InvalidParams,
            "Blind prompt can't be empty",
        ));
    }
    if params.participant_member_ids.is_empty() {
        return Err(err(
            ControlErrorCode::InvalidParams,
            "Blind round requires at least one participant",
        ));
    }
    let group = store
        .get_group(&params.group_id)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| {
            err(
                ControlErrorCode::NotFound,
                format!("Group not found: {}", params.group_id),
            )
        })?;
    let members = store
        .list_members(&params.group_id, false)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    let member_map: HashMap<String, &GroupMemberRecord> =
        members.iter().map(|m| (m.id.clone(), m)).collect();

    let now = now_ms();
    let deadline = params
        .deadline_sec
        .map(|s| now.saturating_add((s as i64).saturating_mul(1000)));
    let mut mention_rows = Vec::new();
    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    for pid in &params.participant_member_ids {
        if !seen.insert(pid.clone()) {
            continue;
        }
        let target = member_map.get(pid).ok_or_else(|| {
            err(
                ControlErrorCode::NotFound,
                format!("Blind participant not found in group: {pid}"),
            )
        })?;
        mention_rows.push(NewGroupMention {
            target_member_id: target.id.clone(),
            mention_kind: GroupMentionKind::BlindInvite.as_str().to_string(),
            delivery_status: GroupDeliveryStatus::Queued.as_str().to_string(),
            delivery_id: Some(short_id("gd")),
            status_reason: None,
            updated_at_ms: now,
        });
        targets.push(*target);
    }

    let (stored_msg, stored_mentions, is_replay) = store
        .append_message_with_mentions(
            &NewGroupMessage {
                id: short_id("gmsg"),
                group_id: group.id.clone(),
                sender_type: GroupSenderType::User.as_str().to_string(),
                sender_member_id: None,
                kind: GroupMessageKind::BlindPrompt.as_str().to_string(),
                content: params.prompt,
                attachments_json: b"[]".to_vec(),
                reply_to_seq: None,
                thread_root_seq: None,
                blind_deadline_at_ms: deadline,
                blind_revealed_at_ms: None,
                task_status: None,
                chain_id: Some(short_id("chn")),
                chain_depth: 0,
                auto_bridged: false,
                pinned: false,
                idempotency_key: params.idempotency_key,
                created_at_ms: now,
            },
            &mention_rows,
        )
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;

    let dto = message_to_dto_with_mentions(&stored_msg, &stored_mentions, &member_map);
    if is_replay {
        return Ok(dto);
    }

    notify_message_appended(
        subscribers,
        json!({
            "event": "blind_round_started",
            "group_id": group.id,
            "message": dto,
        }),
    );
    notify_groups_changed(subscribers, acp_sessions);

    if !group.paused {
        for target in targets {
            enqueue_wake_slot(
                &group.id,
                &target.id,
                stored_msg.id.clone(),
                stored_msg.seq,
                "blind_round",
                Some(stored_msg.seq),
                stored_msg.chain_id.clone(),
                0,
            );
            if target.runtime_id.is_none() {
                let _ =
                    try_dispatch_local_wake(&store, &group.id, target, acp_sessions, subscribers);
            } else {
                let _ = dispatch_remote_wake_or_mark_offline(
                    &store,
                    &group.id,
                    target,
                    &stored_msg.id,
                    acp_sessions,
                );
            }
        }
    }
    Ok(dto)
}

pub(crate) fn submit_blind_entry(
    store: &GroupStore,
    group_id: &str,
    member_id: &str,
    prompt_seq: i64,
    content: String,
    attachments: Vec<GroupAttachment>,
    auto_bridged: bool,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(AgentGroupMessageDto, bool), ControlError> {
    let prompt = store
        .get_message_by_seq(group_id, prompt_seq)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| {
            err(
                ControlErrorCode::NotFound,
                format!("Blind prompt seq={prompt_seq} not found"),
            )
        })?;
    if prompt.kind != "blind_prompt" {
        return Err(err(
            ControlErrorCode::InvalidParams,
            format!("Message seq={prompt_seq} is not a blind_prompt"),
        ));
    }
    let now = now_ms();
    let already_revealed = prompt.blind_revealed_at_ms;
    let members = store
        .list_members(group_id, false)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    let member_map: HashMap<String, &GroupMemberRecord> =
        members.iter().map(|m| (m.id.clone(), m)).collect();

    let (stored_msg, _, _) = store
        .append_message_with_mentions(
            &NewGroupMessage {
                id: short_id("gmsg"),
                group_id: group_id.to_string(),
                sender_type: GroupSenderType::Member.as_str().to_string(),
                sender_member_id: Some(member_id.to_string()),
                kind: GroupMessageKind::BlindSubmission.as_str().to_string(),
                content,
                attachments_json: serde_json::to_vec(&attachments)
                    .unwrap_or_else(|_| b"[]".to_vec()),
                reply_to_seq: Some(prompt_seq),
                thread_root_seq: None,
                blind_deadline_at_ms: None,
                blind_revealed_at_ms: already_revealed,
                task_status: None,
                chain_id: prompt.chain_id.clone(),
                chain_depth: 1,
                auto_bridged,
                pinned: false,
                idempotency_key: Some(format!("blind:{prompt_seq}:{member_id}")),
                created_at_ms: now,
            },
            &[],
        )
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;

    let _ = store.update_mention_status(
        &prompt.id,
        member_id,
        GroupDeliveryStatus::Submitted.as_str(),
        None,
        None,
        now,
    );
    if let Some(m) = member_map.get(member_id)
        && let Some(turn) = lock_state().active_turns.get_mut(&m.session_id)
        && turn.group_id == group_id
    {
        turn.posted_or_yielded = true;
    }

    // 检查是否全员交齐（allSettled）→ 自动原子揭晓
    let mentions = store
        .list_mentions_for_message(&prompt.id)
        .unwrap_or_default();
    let all_settled = !mentions.is_empty()
        && mentions.iter().all(|m| {
            matches!(
                m.delivery_status.as_str(),
                "submitted" | "failed" | "skipped" | "rejected"
            )
        });
    let mut revealed_now = false;
    if all_settled && already_revealed.is_none() {
        let submissions = store
            .reveal_blind_prompt(group_id, prompt_seq, now)
            .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
        revealed_now = true;
        let sub_dtos: Vec<AgentGroupMessageDto> = submissions
            .iter()
            .map(|s| message_to_dto(store, s, &member_map))
            .collect();
        notify_message_appended(
            subscribers,
            json!({
                "event": "blind_round_revealed",
                "group_id": group_id,
                "prompt_seq": prompt_seq,
                "submissions": sub_dtos,
            }),
        );
    } else {
        notify_message_appended(
            subscribers,
            json!({
                "event": "blind_submission_received",
                "group_id": group_id,
                "prompt_seq": prompt_seq,
                "member_id": member_id,
            }),
        );
    }
    notify_groups_changed(subscribers, acp_sessions);
    let dto = message_to_dto(store, &stored_msg, &member_map);
    Ok((dto, revealed_now))
}

pub(crate) fn reveal_blind_round(
    params: GroupBlindRevealParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupBlindRevealResult, ControlError> {
    let store = group_store()?;
    let members = store
        .list_members(&params.group_id, true)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    let member_map: HashMap<String, &GroupMemberRecord> =
        members.iter().map(|m| (m.id.clone(), m)).collect();
    let revealed = store
        .reveal_blind_prompt(&params.group_id, params.prompt_seq, now_ms())
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    let submissions: Vec<AgentGroupMessageDto> = revealed
        .iter()
        .map(|s| message_to_dto(&store, s, &member_map))
        .collect();
    notify_message_appended(
        subscribers,
        json!({
            "event": "blind_round_revealed",
            "group_id": params.group_id,
            "prompt_seq": params.prompt_seq,
            "submissions": submissions,
        }),
    );
    notify_groups_changed(subscribers, acp_sessions);
    Ok(GroupBlindRevealResult {
        prompt_seq: params.prompt_seq,
        submissions,
    })
}
