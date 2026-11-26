//! 群组消息路由、熔断器、权限水位线、审批放行、Auto-Bridge 与 `peer_messages` 投递桥接。

use std::collections::HashMap;

use smelt_core::acp_session::{PeerDelivery, PeerMessageKind};
use smelt_core::agent_group::{
    GroupDeliveryStatus, GroupMentionKind, GroupMessageAppendedEvent, GroupMessageKind,
    GroupMessageSenderType, GroupMessageView, GroupMode, GroupTaskStatus, MemberRoleKind,
    WakeMessageEntry, WakeRosterEntry, deterministic_group_delivery_id, extract_mentions,
    format_group_wake_body,
};
use smelt_core::agent_kind::ApprovalPosture;
use smelt_core::control_api::{
    GroupMemberWakeParams, GroupMemberWakeResult, GroupMentionActionParams,
    GroupMentionActionResult, GroupMessagePostParams,
};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};
use smelt_store::{
    AgentGroupMemberRecord, AgentGroupMessageQuery, AgentGroupMessageRecord, AgentGroupRecord,
    NewAgentGroupMention, NewAgentGroupMessage, Store,
};

use super::{
    ChainBudget, default_store, hydrate_single_message, lock_state, map_store_error,
    notify_groups_changed, now_ms,
};
use crate::{AcpSessions, EventHubHandle};

const WAKE_DELTA_LIMIT: usize = 30;

/// 用户往群里发消息：开启新链并重置本群熔断计数，但绝不清空其他成员已排队的未送达唤醒。
pub(crate) fn post_user_message(
    params: GroupMessagePostParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupMessageView, ControlError> {
    let store = default_store()?;
    post_user_message_with_store(&store, params, acp_sessions, subscribers)
}

pub(crate) fn post_user_message_with_store(
    store: &Store,
    params: GroupMessagePostParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupMessageView, ControlError> {
    let content = params.content.trim().to_string();
    if content.is_empty() {
        return Err(ControlError::new(
            ControlErrorCode::InvalidParams,
            "Message content cannot be empty",
        ));
    }
    let group = store
        .get_agent_group(&params.group_id)
        .map_err(map_store_error)?
        .ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::NotFound,
                format!("Group not found: {}", params.group_id),
            )
        })?;
    if group.archived_at_ms.is_some() {
        return Err(ControlError::new(
            ControlErrorCode::FailedPrecondition,
            "Cannot post to an archived group",
        ));
    }

    let members = store
        .list_agent_group_members(&group.id, true)
        .map_err(map_store_error)?;
    let active_members: Vec<&AgentGroupMemberRecord> = members
        .iter()
        .filter(|m| m.removed_at_ms.is_none())
        .collect();

    let chain_id = format!("ch-{}", uuid::Uuid::new_v4().simple());
    {
        let mut state = lock_state();
        state
            .chain_budgets
            .insert(group.id.clone(), ChainBudget::fresh(chain_id.clone()));
    }

    let thread_root_seq = resolve_thread_root(store, &group.id, params.reply_to_seq)?;
    let msg_id = format!("gmsg-{}", uuid::Uuid::new_v4().simple());
    let now = now_ms();
    let targets = resolve_user_targets(&group, &active_members, &content);
    let new_mentions: Vec<NewAgentGroupMention> = targets
        .into_iter()
        .map(|(member, kind)| {
            let (delivery_status, status_reason) = if member.muted {
                (
                    GroupDeliveryStatus::Skipped.as_str().to_string(),
                    Some("member_muted".to_string()),
                )
            } else {
                (GroupDeliveryStatus::Queued.as_str().to_string(), None)
            };
            NewAgentGroupMention {
                target_member_id: member.id.clone(),
                mention_kind: kind.as_str().to_string(),
                delivery_status,
                delivery_id: deterministic_group_delivery_id(&msg_id, &member.id),
                status_reason,
                updated_at_ms: now,
            }
        })
        .collect();

    let task_status = if params.kind == GroupMessageKind::Task {
        Some(
            params
                .task_status
                .unwrap_or(GroupTaskStatus::Open)
                .as_str()
                .to_string(),
        )
    } else {
        None
    };

    let (record, mention_records, inserted) = store
        .append_agent_group_message_with_mentions(
            &NewAgentGroupMessage {
                id: msg_id,
                group_id: group.id.clone(),
                sender_type: GroupMessageSenderType::User.as_str().to_string(),
                sender_member_id: None,
                kind: params.kind.as_str().to_string(),
                content,
                reply_to_seq: params.reply_to_seq,
                thread_root_seq,
                task_status,
                chain_id: Some(chain_id),
                chain_depth: 0,
                auto_bridged: false,
                pinned: false,
                idempotency_key: params.idempotency_key,
                created_at_ms: now,
            },
            &new_mentions,
        )
        .map_err(map_store_error)?;

    let member_map: HashMap<&str, &AgentGroupMemberRecord> =
        members.iter().map(|m| (m.id.as_str(), m)).collect();
    let view = hydrate_single_message(record.clone(), mention_records.clone(), &member_map);

    if inserted {
        publish_message_appended(subscribers, &group.id, &view);
        notify_groups_changed(subscribers);
        if !group.paused {
            for mention in &mention_records {
                if mention.delivery_status == GroupDeliveryStatus::Queued.as_str()
                    && let Some(target) = member_map.get(mention.target_member_id.as_str())
                {
                    dispatch_mention_to_peer_bus(
                        store,
                        &group,
                        target,
                        &members,
                        &record,
                        &mention.delivery_id,
                        ApprovalPosture::FullAccess,
                        acp_sessions,
                        subscribers,
                    );
                }
            }
        }
    }

    Ok(view)
}

/// 成员发消息（来自 `group_post` 工具或 Auto-Bridge）：执行 cross-talk、熔断与权限水位线检查。
#[allow(clippy::too_many_arguments)]
pub(crate) fn post_member_message_with_store(
    store: &Store,
    group_id: &str,
    sender_member: &AgentGroupMemberRecord,
    content: String,
    kind: GroupMessageKind,
    reply_to_seq: Option<i64>,
    task_status: Option<GroupTaskStatus>,
    idempotency_key: Option<String>,
    auto_bridged: bool,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupMessageView, ControlError> {
    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        return Err(ControlError::new(
            ControlErrorCode::InvalidParams,
            "Message content cannot be empty",
        ));
    }
    let group = store
        .get_agent_group(group_id)
        .map_err(map_store_error)?
        .ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::NotFound,
                format!("Group not found: {group_id}"),
            )
        })?;
    if group.archived_at_ms.is_some() {
        return Err(ControlError::new(
            ControlErrorCode::FailedPrecondition,
            "Group is archived",
        ));
    }

    let members = store
        .list_agent_group_members(&group.id, true)
        .map_err(map_store_error)?;
    let active_members: Vec<&AgentGroupMemberRecord> = members
        .iter()
        .filter(|m| m.removed_at_ms.is_none())
        .collect();

    let parent_msg = match reply_to_seq {
        Some(seq) => store
            .get_agent_group_message_by_seq(&group.id, seq)
            .map_err(map_store_error)?,
        None => None,
    };
    let thread_root_seq = parent_msg
        .as_ref()
        .map(|p| p.thread_root_seq.unwrap_or(p.seq))
        .or(reply_to_seq);
    let chain_depth = parent_msg.as_ref().map(|p| p.chain_depth + 1).unwrap_or(1);
    let chain_id = parent_msg
        .as_ref()
        .and_then(|p| p.chain_id.clone())
        .unwrap_or_else(|| {
            lock_state()
                .chain_budgets
                .get(&group.id)
                .map(|b| b.chain_id.clone())
                .unwrap_or_else(|| format!("ch-{}", uuid::Uuid::new_v4().simple()))
        });

    let sender_posture =
        crate::peer_messages::session_approval_posture(&sender_member.session_id, acp_sessions);
    let msg_id = format!("gmsg-{}", uuid::Uuid::new_v4().simple());
    let now = now_ms();

    let targets = resolve_member_targets(&active_members, sender_member, &trimmed);
    let mut new_mentions = Vec::with_capacity(targets.len());
    {
        let mut state = lock_state();
        let budget = state
            .chain_budgets
            .entry(group.id.clone())
            .or_insert_with(|| ChainBudget::fresh(chain_id.clone()));
        if budget.chain_id != chain_id {
            *budget = ChainBudget::fresh(chain_id.clone());
        }

        for (target, mention_kind) in targets {
            let delivery_id = deterministic_group_delivery_id(&msg_id, &target.id);
            let target_posture =
                crate::peer_messages::session_approval_posture(&target.session_id, acp_sessions);
            let decision = evaluate_member_mention(
                &group,
                sender_member,
                target,
                sender_posture,
                target_posture,
                chain_depth,
                budget,
            );
            new_mentions.push(NewAgentGroupMention {
                target_member_id: target.id.clone(),
                mention_kind: mention_kind.as_str().to_string(),
                delivery_status: decision.status.as_str().to_string(),
                delivery_id,
                status_reason: decision.reason,
                updated_at_ms: now,
            });
        }
    }

    let task_status_str = if kind == GroupMessageKind::Task {
        Some(
            task_status
                .unwrap_or(GroupTaskStatus::Open)
                .as_str()
                .to_string(),
        )
    } else {
        None
    };

    let (record, mention_records, inserted) = store
        .append_agent_group_message_with_mentions(
            &NewAgentGroupMessage {
                id: msg_id,
                group_id: group.id.clone(),
                sender_type: GroupMessageSenderType::Member.as_str().to_string(),
                sender_member_id: Some(sender_member.id.clone()),
                kind: kind.as_str().to_string(),
                content: trimmed,
                reply_to_seq,
                thread_root_seq,
                task_status: task_status_str,
                chain_id: Some(chain_id),
                chain_depth,
                auto_bridged,
                pinned: false,
                idempotency_key,
                created_at_ms: now,
            },
            &new_mentions,
        )
        .map_err(map_store_error)?;

    // 发送方已读到自己发出的这一条，并标记本轮已通过显式工具发过言。
    let _ = store.advance_agent_group_member_read_seq(&sender_member.id, record.seq);
    if !auto_bridged {
        lock_state()
            .member_turn_acted
            .insert((group.id.clone(), sender_member.session_id.clone()));
    }

    let member_map: HashMap<&str, &AgentGroupMemberRecord> =
        members.iter().map(|m| (m.id.as_str(), m)).collect();
    let view = hydrate_single_message(record.clone(), mention_records.clone(), &member_map);

    if inserted {
        publish_message_appended(subscribers, &group.id, &view);
        notify_groups_changed(subscribers);
        if !group.paused {
            for mention in &mention_records {
                if mention.delivery_status == GroupDeliveryStatus::Queued.as_str()
                    && let Some(target) = member_map.get(mention.target_member_id.as_str())
                {
                    dispatch_mention_to_peer_bus(
                        store,
                        &group,
                        target,
                        &members,
                        &record,
                        &mention.delivery_id,
                        sender_posture,
                        acp_sessions,
                        subscribers,
                    );
                }
            }
        }
    }

    Ok(view)
}

/// 手动唤醒指定成员（等价于用户发一条带 `@alias` 的系统/用户提示并立刻投递）。
pub(crate) fn wake_member(
    params: GroupMemberWakeParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupMemberWakeResult, ControlError> {
    let store = default_store()?;
    let member = store
        .get_agent_group_member(&params.member_id)
        .map_err(map_store_error)?
        .filter(|m| m.group_id == params.group_id && m.removed_at_ms.is_none())
        .ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::NotFound,
                format!("Group member not found: {}", params.member_id),
            )
        })?;
    let note = params
        .note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("@{} {s}", member.alias))
        .unwrap_or_else(|| format!("@{} please review recent messages", member.alias));
    let posted = post_user_message_with_store(
        &store,
        GroupMessagePostParams {
            group_id: params.group_id,
            content: note,
            kind: GroupMessageKind::Chat,
            reply_to_seq: None,
            task_status: None,
            idempotency_key: None,
        },
        acp_sessions,
        subscribers,
    )?;
    let delivery_id = posted
        .mentions
        .iter()
        .find(|m| m.target_member_id == member.id)
        .map(|m| m.delivery_id.clone())
        .unwrap_or_else(|| deterministic_group_delivery_id(&posted.id, &member.id));
    Ok(GroupMemberWakeResult {
        message_id: posted.id,
        seq: posted.seq,
        delivery_id,
    })
}

/// 批准一条处于 `pending_approval` 的权限水位线 `@` 投递，复用原确定性 `delivery_id` 入队。
pub(crate) fn approve_mention(
    params: GroupMentionActionParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupMentionActionResult, ControlError> {
    let store = default_store()?;
    approve_mention_with_store(&store, params, acp_sessions, subscribers)
}

pub(crate) fn approve_mention_with_store(
    store: &Store,
    params: GroupMentionActionParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupMentionActionResult, ControlError> {
    let pending = store
        .get_agent_group_mention(&params.message_id, &params.target_member_id)
        .map_err(map_store_error)?
        .filter(|m| m.group_id == params.group_id)
        .ok_or_else(|| ControlError::new(ControlErrorCode::NotFound, "Group mention not found"))?;
    if pending.mention.delivery_status != GroupDeliveryStatus::PendingApproval.as_str() {
        return Ok(GroupMentionActionResult {
            updated: false,
            delivery_status: GroupDeliveryStatus::parse(&pending.mention.delivery_status)
                .unwrap_or_default(),
        });
    }

    let updated = store
        .update_agent_group_mention_status(
            &params.message_id,
            &params.target_member_id,
            GroupDeliveryStatus::Queued.as_str(),
            params.reason.as_deref(),
            now_ms(),
        )
        .map_err(map_store_error)?;

    if updated
        && let Ok(Some(group)) = store.get_agent_group(&params.group_id)
        && !group.paused
        && let Ok(Some(msg)) = store.get_agent_group_message_by_id(&params.message_id)
        && let Ok(members) = store.list_agent_group_members(&group.id, true)
        && let Some(target) = members
            .iter()
            .find(|m| m.id == params.target_member_id && m.removed_at_ms.is_none())
    {
        // 用户已明确批准提权唤醒，因此按用户授权姿态放行到 peer_messages。
        dispatch_mention_to_peer_bus(
            store,
            &group,
            target,
            &members,
            &msg,
            &pending.mention.delivery_id,
            ApprovalPosture::FullAccess,
            acp_sessions,
            subscribers,
        );
    }

    if updated {
        notify_groups_changed(subscribers);
    }
    Ok(GroupMentionActionResult {
        updated,
        delivery_status: GroupDeliveryStatus::Queued,
    })
}

pub(crate) fn reject_mention(
    params: GroupMentionActionParams,
    subscribers: &EventHubHandle,
) -> Result<GroupMentionActionResult, ControlError> {
    let store = default_store()?;
    reject_mention_with_store(&store, params, subscribers)
}

pub(crate) fn reject_mention_with_store(
    store: &Store,
    params: GroupMentionActionParams,
    subscribers: &EventHubHandle,
) -> Result<GroupMentionActionResult, ControlError> {
    let pending = store
        .get_agent_group_mention(&params.message_id, &params.target_member_id)
        .map_err(map_store_error)?
        .filter(|m| m.group_id == params.group_id)
        .ok_or_else(|| ControlError::new(ControlErrorCode::NotFound, "Group mention not found"))?;
    if pending.mention.delivery_status != GroupDeliveryStatus::PendingApproval.as_str() {
        return Ok(GroupMentionActionResult {
            updated: false,
            delivery_status: GroupDeliveryStatus::parse(&pending.mention.delivery_status)
                .unwrap_or_default(),
        });
    }

    let updated = store
        .update_agent_group_mention_status(
            &params.message_id,
            &params.target_member_id,
            GroupDeliveryStatus::Rejected.as_str(),
            params.reason.as_deref().or(Some("rejected_by_user")),
            now_ms(),
        )
        .map_err(map_store_error)?;
    if updated {
        notify_groups_changed(subscribers);
    }
    Ok(GroupMentionActionResult {
        updated,
        delivery_status: GroupDeliveryStatus::Rejected,
    })
}

/// 守护进程启动时恢复所有未暂停、未归档群组的 `queued` 投递。
pub(crate) fn resume_on_startup(acp_sessions: &AcpSessions, subscribers: &EventHubHandle) {
    let Ok(store) = smelt_core::sqlite_state::default_sqlite_store() else {
        return;
    };
    resume_mentions(&store, None, acp_sessions, subscribers);
}

/// 仅恢复指定群组的 `queued` 投递（`group.resume` 专用，不波及其他群）。
pub(crate) fn resume_group_queued_mentions(
    store: &Store,
    group_id: &str,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    resume_mentions(store, Some(group_id), acp_sessions, subscribers);
}

fn resume_mentions(
    store: &Store,
    group_id: Option<&str>,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let Ok(pending) =
        store.list_agent_group_mentions_by_status(group_id, GroupDeliveryStatus::Queued.as_str())
    else {
        return;
    };
    if pending.is_empty() {
        return;
    }
    // 每个 `(group_id, target_member_id)` 只投递最新一条，旧的在结算时自动合并为 `coalesced`。
    let mut latest_by_target: HashMap<
        (String, String),
        smelt_store::AgentGroupPendingMentionRecord,
    > = HashMap::new();
    for item in pending {
        let key = (item.group_id.clone(), item.mention.target_member_id.clone());
        let replace = latest_by_target
            .get(&key)
            .is_none_or(|prev| item.seq >= prev.seq);
        if replace {
            latest_by_target.insert(key, item);
        }
    }

    for ((gid, target_id), item) in latest_by_target {
        let Ok(Some(group)) = store.get_agent_group(&gid) else {
            continue;
        };
        if group.paused || group.archived_at_ms.is_some() {
            continue;
        }
        let Ok(members) = store.list_agent_group_members(&gid, true) else {
            continue;
        };
        let Some(target) = members
            .iter()
            .find(|m| m.id == target_id && m.removed_at_ms.is_none())
        else {
            continue;
        };
        let Ok(Some(msg)) = store.get_agent_group_message_by_id(&item.mention.message_id) else {
            continue;
        };
        // 重启恢复的 `queued` 记录已经通过入队时的权限水位线校验，按 `FullAccess` 放行。
        dispatch_mention_to_peer_bus(
            store,
            &group,
            target,
            &members,
            &msg,
            &item.mention.delivery_id,
            ApprovalPosture::FullAccess,
            acp_sessions,
            subscribers,
        );
    }
}

/// 当某个 ACP 会话结束一个回合时调用：仅当所属群开启 `auto_bridge_mentions` 且本轮未显式调用
/// `group_post` / `group_yield` 时，若助手末段回复含有效 `@alias`，自动桥接进群。
pub(crate) fn on_member_turn_ended(
    session_id: &str,
    reply_text: &str,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let Ok(store) = smelt_core::sqlite_state::default_sqlite_store() else {
        return;
    };
    on_member_turn_ended_with_store(&store, session_id, reply_text, acp_sessions, subscribers);
}

pub(crate) fn on_member_turn_ended_with_store(
    store: &Store,
    session_id: &str,
    reply_text: &str,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let trimmed = reply_text.trim();
    if trimmed.is_empty() {
        return;
    }
    let Ok(memberships) = store.find_active_agent_group_members_by_session(session_id) else {
        return;
    };
    if memberships.is_empty() {
        return;
    }
    let mentions = extract_mentions(trimmed);
    for member in memberships {
        let already_acted = lock_state()
            .member_turn_acted
            .remove(&(member.group_id.clone(), session_id.to_string()));
        if already_acted || mentions.is_empty() {
            continue;
        }
        let Ok(Some(group)) = store.get_agent_group(&member.group_id) else {
            continue;
        };
        if !group.auto_bridge_mentions || group.paused || group.archived_at_ms.is_some() {
            continue;
        }
        let Ok(all_members) = store.list_agent_group_members(&group.id, false) else {
            continue;
        };
        let has_valid_target = mentions.iter().any(|token| {
            token == "all"
                || all_members
                    .iter()
                    .any(|m| m.id != member.id && m.alias == *token)
        });
        if !has_valid_target {
            continue;
        }
        let _ = post_member_message_with_store(
            store,
            &group.id,
            &member,
            trimmed.to_string(),
            GroupMessageKind::Chat,
            None,
            None,
            None,
            true,
            acp_sessions,
            subscribers,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_mention_to_peer_bus(
    store: &Store,
    group: &AgentGroupRecord,
    target: &AgentGroupMemberRecord,
    all_members: &[AgentGroupMemberRecord],
    trigger_msg: &AgentGroupMessageRecord,
    delivery_id: &str,
    source_posture: ApprovalPosture,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let use_mcp = session_has_mcp_or_pi(&target.session_id, acp_sessions);
    let delta_records = store
        .list_agent_group_messages(&AgentGroupMessageQuery {
            group_id: group.id.clone(),
            since_seq: Some(target.last_delivered_seq),
            before_seq: None,
            limit: WAKE_DELTA_LIMIT,
            pinned_only: false,
        })
        .unwrap_or_else(|_| vec![trigger_msg.clone()]);
    let open_task_records = store
        .list_agent_group_open_tasks(&group.id)
        .unwrap_or_default();

    let member_map: HashMap<&str, &AgentGroupMemberRecord> =
        all_members.iter().map(|m| (m.id.as_str(), m)).collect();
    let roster: Vec<WakeRosterEntry> = all_members
        .iter()
        .filter(|m| m.removed_at_ms.is_none())
        .map(|m| WakeRosterEntry {
            alias: m.alias.clone(),
            display_name: m.display_name.clone(),
            role_kind: MemberRoleKind::parse(&m.role_kind).unwrap_or_default(),
            role_title: m.role_title.clone(),
            is_self: m.id == target.id,
        })
        .collect();
    let to_wake_entry = |msg: &AgentGroupMessageRecord| {
        let sender_label = match GroupMessageSenderType::parse(&msg.sender_type) {
            Some(GroupMessageSenderType::Member) => msg
                .sender_member_id
                .as_deref()
                .and_then(|id| member_map.get(id))
                .map(|m| format!("@{}", m.alias))
                .unwrap_or_else(|| "member".to_string()),
            Some(GroupMessageSenderType::System) => "system".to_string(),
            _ => "user".to_string(),
        };
        WakeMessageEntry {
            seq: msg.seq,
            sender_label,
            kind: GroupMessageKind::parse(&msg.kind).unwrap_or_default(),
            task_status: msg.task_status.as_deref().and_then(GroupTaskStatus::parse),
            content: msg.content.clone(),
        }
    };
    let delta_entries: Vec<WakeMessageEntry> = delta_records.iter().map(to_wake_entry).collect();
    let open_tasks: Vec<WakeMessageEntry> = open_task_records.iter().map(to_wake_entry).collect();

    let body = format_group_wake_body(
        &group.id,
        &group.title,
        &group.description,
        GroupMode::parse(&group.mode).unwrap_or_default(),
        &target.alias,
        MemberRoleKind::parse(&target.role_kind).unwrap_or_default(),
        &target.role_title,
        &target.role_prompt,
        !target.briefing_sent,
        use_mcp,
        &roster,
        &delta_entries,
        &open_tasks,
        trigger_msg.seq,
    );

    // 清除上次唤醒的 acted 标记，避免误跳过本轮 Auto-Bridge。
    lock_state()
        .member_turn_acted
        .remove(&(group.id.clone(), target.session_id.clone()));

    let delivery = PeerDelivery {
        id: delivery_id.to_string(),
        source_session_id: format!("group:{}", group.id),
        source_title: format!("Group · {}", group.title),
        kind: PeerMessageKind::Message,
        hops: 1,
        reply_to: None,
        text: body,
    };

    crate::peer_messages::deliver_group_message(
        &target.session_id,
        &target.display_name,
        delivery,
        source_posture,
        acp_sessions,
        subscribers,
    );
}

/// Fail-closed：找不到会话或会话尚未建好 MCP 连接时返回 `false`，引导其走 CLI 回退。
pub(crate) fn session_has_mcp_or_pi(session_id: &str, acp_sessions: &AcpSessions) -> bool {
    let Some(slot) = acp_sessions.get(session_id) else {
        return false;
    };
    crate::acp_host::session_uses_pi_admission(&slot.value)
        || slot
            .value
            .peer_mcp_connected
            .load(std::sync::atomic::Ordering::SeqCst)
}

fn resolve_thread_root(
    store: &Store,
    group_id: &str,
    reply_to_seq: Option<i64>,
) -> Result<Option<i64>, ControlError> {
    let Some(seq) = reply_to_seq else {
        return Ok(None);
    };
    let parent = store
        .get_agent_group_message_by_seq(group_id, seq)
        .map_err(map_store_error)?;
    Ok(Some(
        parent
            .as_ref()
            .map(|p| p.thread_root_seq.unwrap_or(p.seq))
            .unwrap_or(seq),
    ))
}

fn resolve_user_targets<'a>(
    group: &AgentGroupRecord,
    active_members: &[&'a AgentGroupMemberRecord],
    content: &str,
) -> Vec<(&'a AgentGroupMemberRecord, GroupMentionKind)> {
    let tokens = extract_mentions(content);
    let mut targets: Vec<(&'a AgentGroupMemberRecord, GroupMentionKind)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for token in &tokens {
        if token == "all" {
            for member in active_members {
                if seen.insert(member.id.as_str()) {
                    targets.push((*member, GroupMentionKind::All));
                }
            }
        } else if let Some(member) = active_members.iter().find(|m| m.alias == *token)
            && seen.insert(member.id.as_str())
        {
            targets.push((*member, GroupMentionKind::Direct));
        }
    }

    if targets.is_empty()
        && group.mode == GroupMode::Coordinator.as_str()
        && let Some(coord_id) = group.coordinator_member_id.as_deref()
        && let Some(coord) = active_members.iter().find(|m| m.id == coord_id)
    {
        targets.push((*coord, GroupMentionKind::CoordinatorDefault));
    }

    targets
}

fn resolve_member_targets<'a>(
    active_members: &[&'a AgentGroupMemberRecord],
    sender: &AgentGroupMemberRecord,
    content: &str,
) -> Vec<(&'a AgentGroupMemberRecord, GroupMentionKind)> {
    let tokens = extract_mentions(content);
    let mut targets = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for token in &tokens {
        if token == "all" {
            for member in active_members {
                if member.id != sender.id && seen.insert(member.id.as_str()) {
                    targets.push((*member, GroupMentionKind::All));
                }
            }
        } else if let Some(member) = active_members
            .iter()
            .find(|m| m.id != sender.id && m.alias == *token)
            && seen.insert(member.id.as_str())
        {
            targets.push((*member, GroupMentionKind::Direct));
        }
    }

    targets
}

pub(crate) struct MentionDecision {
    pub(crate) status: GroupDeliveryStatus,
    pub(crate) reason: Option<String>,
}

pub(crate) fn evaluate_member_mention(
    group: &AgentGroupRecord,
    sender: &AgentGroupMemberRecord,
    target: &AgentGroupMemberRecord,
    sender_posture: ApprovalPosture,
    target_posture: ApprovalPosture,
    chain_depth: u32,
    budget: &mut ChainBudget,
) -> MentionDecision {
    if target.muted {
        return MentionDecision {
            status: GroupDeliveryStatus::Skipped,
            reason: Some("member_muted".into()),
        };
    }
    if !group.allow_worker_cross_talk {
        let sender_is_coord = group.coordinator_member_id.as_deref() == Some(sender.id.as_str())
            || sender.role_kind == MemberRoleKind::Coordinator.as_str();
        let target_is_coord = group.coordinator_member_id.as_deref() == Some(target.id.as_str())
            || target.role_kind == MemberRoleKind::Coordinator.as_str();
        if !sender_is_coord && !target_is_coord {
            return MentionDecision {
                status: GroupDeliveryStatus::Skipped,
                reason: Some("worker_cross_talk_disabled".into()),
            };
        }
    }
    if group.max_chain_depth > 0 && chain_depth > group.max_chain_depth {
        return MentionDecision {
            status: GroupDeliveryStatus::Skipped,
            reason: Some(format!(
                "circuit_breaker:max_chain_depth({})",
                group.max_chain_depth
            )),
        };
    }
    if group.max_chain_hops > 0 && budget.hops_used >= group.max_chain_hops {
        return MentionDecision {
            status: GroupDeliveryStatus::Skipped,
            reason: Some(format!(
                "circuit_breaker:max_chain_hops({})",
                group.max_chain_hops
            )),
        };
    }
    let pair_key = canonical_pair(&sender.id, &target.id);
    let pair_used = budget.pair_counts.get(&pair_key).copied().unwrap_or(0);
    if group.max_pair_ping_pong > 0 && pair_used >= group.max_pair_ping_pong {
        return MentionDecision {
            status: GroupDeliveryStatus::Skipped,
            reason: Some(format!(
                "circuit_breaker:max_pair_ping_pong({})",
                group.max_pair_ping_pong
            )),
        };
    }

    if needs_approval_waterline(sender, target, sender_posture, target_posture) {
        return MentionDecision {
            status: GroupDeliveryStatus::PendingApproval,
            reason: Some("permission_waterline".into()),
        };
    }

    budget.hops_used += 1;
    *budget.pair_counts.entry(pair_key).or_insert(0) += 1;
    MentionDecision {
        status: GroupDeliveryStatus::Queued,
        reason: None,
    }
}

fn canonical_pair(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

fn is_read_only_member(member: &AgentGroupMemberRecord) -> bool {
    member.tool_restriction_profile == "read_only"
        || matches!(
            MemberRoleKind::parse(&member.role_kind),
            Some(MemberRoleKind::Observer | MemberRoleKind::Reviewer)
        )
}

/// 权限水位线：只读/审阅成员或较低审批姿态成员试图唤醒更高权限成员时，落为 `pending_approval`。
pub(crate) fn needs_approval_waterline(
    sender: &AgentGroupMemberRecord,
    target: &AgentGroupMemberRecord,
    sender_posture: ApprovalPosture,
    target_posture: ApprovalPosture,
) -> bool {
    if is_read_only_member(sender) && !is_read_only_member(target) {
        return true;
    }
    !sender_posture.allows_message_to(target_posture)
}

fn publish_message_appended(
    subscribers: &EventHubHandle,
    group_id: &str,
    message: &GroupMessageView,
) {
    let event = GroupMessageAppendedEvent {
        group_id: group_id.to_string(),
        message: message.clone(),
    };
    if let Err(error) = subscribers.publish_group_message_appended(&event) {
        crate::dlog(&format!(
            "[group] 推送 group.message_appended 失败：{error}"
        ));
    }
}
