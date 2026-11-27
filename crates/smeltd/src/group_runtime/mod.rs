//! 本地智能体群组运行时（PR 1）。
//!
//! 职责边界：
//! - 持久化走运行时主库 `smelt.sqlite3`（`smelt_store::Store` v43 四张规范化表）；
//! - 成员唤醒完全复用 `crate::peer_messages::deliver_group_message` +
//!   `crate::acp_host::deliver_peer_message`，零并行投递通道、绝不伪造用户输入；
//! - 成员 DTO 从单次 `WorkspaceDirectoryReader::read()` 与内存 `AcpSessions` 组装，零每成员查库；
//! - 批量消息列表经 `list_agent_group_mentions_for_messages` 单次联查，零 N+1。

pub(crate) mod crud;
pub(crate) mod routing;
pub(crate) mod tools;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use smelt_core::agent_group::{
    GroupDeliveryStatus, GroupDetail, GroupMemberView, GroupMentionKind, GroupMentionView,
    GroupMessageKind, GroupMessageSenderType, GroupMessageView, GroupMode, GroupSummary,
    GroupTaskStatus, GroupsSnapshot, MemberOwnership, MemberRoleKind,
};
use smelt_core::workspace_directory::WorkspaceDirectory;
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};
use smelt_store::{
    AgentGroupMemberRecord, AgentGroupMentionRecord, AgentGroupMessageRecord, AgentGroupRecord,
    Store, StoreError,
};

use crate::{AcpSessions, EventHubHandle, WorkspaceDirectoryReader};

pub(crate) use crud::{
    add_member, archive_group, create_group, delete_group, get_group, list_groups, list_messages,
    pause_group, pin_message, remove_member, resume_group, update_group, update_member,
    update_task_status,
};
pub(crate) use routing::{
    approve_mention, on_member_turn_ended, post_user_message, reject_mention, resume_on_startup,
    wake_member,
};
pub(crate) use tools::dispatch_agent_group_tool;

#[derive(Default)]
pub(crate) struct GroupRuntimeState {
    /// 每个群组当前的链预算（用户发消息时重置，绝不清空其他成员待投递队列）。
    pub(crate) chain_budgets: HashMap<String, ChainBudget>,
    /// 记录本轮是否已经主动调用过 `group_post` / `group_yield`，防止 Auto-Bridge 重复桥接。
    pub(crate) member_turn_acted: HashSet<(String, String)>,
}

#[derive(Clone, Debug)]
pub(crate) struct ChainBudget {
    pub(crate) chain_id: String,
    pub(crate) hops_used: u32,
    pub(crate) pair_counts: HashMap<(String, String), u32>,
}

impl ChainBudget {
    pub(crate) fn fresh(chain_id: String) -> Self {
        Self {
            chain_id,
            hops_used: 0,
            pair_counts: HashMap::new(),
        }
    }
}

static GROUP_RUNTIME: LazyLock<Mutex<GroupRuntimeState>> = LazyLock::new(Default::default);
static GROUPS_REVISION: AtomicU64 = AtomicU64::new(0);

pub(crate) fn lock_state() -> std::sync::MutexGuard<'static, GroupRuntimeState> {
    GROUP_RUNTIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub(crate) fn default_store() -> Result<Store, ControlError> {
    smelt_core::sqlite_state::default_sqlite_store()
        .map_err(|message| ControlError::new(ControlErrorCode::TemporarilyUnavailable, message))
}

pub(crate) fn map_store_error(error: StoreError) -> ControlError {
    match error {
        StoreError::AgentGroupNotFound { id } => {
            ControlError::new(ControlErrorCode::NotFound, format!("Group not found: {id}"))
        }
        StoreError::AgentGroupMemberNotFound { id } => ControlError::new(
            ControlErrorCode::NotFound,
            format!("Group member not found: {id}"),
        ),
        other => ControlError::new(ControlErrorCode::OperationFailed, other.to_string()),
    }
}

/// 当前会话是否属于任意一个活跃群组（用于动态暴露群组 MCP / Pi 工具）。
pub(crate) fn session_in_any_group(session_id: &str) -> bool {
    let Ok(store) = smelt_core::sqlite_state::default_sqlite_store() else {
        return false;
    };
    store
        .find_active_agent_group_members_by_session(session_id)
        .map(|members| !members.is_empty())
        .unwrap_or(false)
}

/// 生成供 `group.changed` 与 `core.groups.v1` 快照使用的完整视图。
pub(crate) fn groups_snapshot_or_empty() -> GroupsSnapshot {
    let Ok(store) = smelt_core::sqlite_state::default_sqlite_store() else {
        return GroupsSnapshot::default();
    };
    build_groups_snapshot(&store).unwrap_or_default()
}

pub(crate) fn build_groups_snapshot(store: &Store) -> Result<GroupsSnapshot, StoreError> {
    let groups = build_group_summaries(store, false)?;
    let group_owned_session_ids = store.list_group_owned_session_ids()?;
    let revision = GROUPS_REVISION.fetch_add(1, Ordering::SeqCst) + 1;
    Ok(GroupsSnapshot {
        revision,
        groups,
        group_owned_session_ids,
    })
}

pub(crate) fn build_group_summaries(
    store: &Store,
    include_archived: bool,
) -> Result<Vec<GroupSummary>, StoreError> {
    let records = store.list_agent_groups(include_archived)?;
    let ids: Vec<String> = records.iter().map(|g| g.id.clone()).collect();
    let stats_map = store.list_agent_group_stats(&ids)?;
    Ok(records
        .into_iter()
        .map(|group| {
            let stats = stats_map.get(&group.id).cloned().unwrap_or_default();
            GroupSummary {
                id: group.id,
                title: group.title,
                description: group.description,
                project_root: group.project_root,
                workset_id: group.workset_id,
                mode: GroupMode::parse(&group.mode).unwrap_or_default(),
                coordinator_member_id: group.coordinator_member_id,
                paused: group.paused,
                archived_at_ms: group.archived_at_ms,
                member_count: stats.member_count,
                active_member_count: stats.active_member_count,
                last_seq: stats.last_seq,
                last_message_preview: stats.last_message_preview,
                last_message_at_ms: stats.last_message_at_ms,
                pending_approval_count: stats.pending_approval_count,
                open_task_count: stats.open_task_count,
                created_at_ms: group.created_at_ms,
                updated_at_ms: group.updated_at_ms,
            }
        })
        .collect())
}

/// `peer_messages` 结算一封 `pm-grp-...` 信件后回调：原子落库并推送 `group.changed`。
pub(crate) fn on_delivery_settled(
    delivery_id: &str,
    result: Result<(), String>,
    subscribers: &EventHubHandle,
) {
    let Ok(store) = smelt_core::sqlite_state::default_sqlite_store() else {
        return;
    };
    on_delivery_settled_with_store(&store, delivery_id, result, subscribers);
}

pub(crate) fn on_delivery_settled_with_store(
    store: &Store,
    delivery_id: &str,
    result: Result<(), String>,
    subscribers: &EventHubHandle,
) {
    let (status, reason) = match &result {
        Ok(()) => (GroupDeliveryStatus::Delivered.as_str(), None),
        Err(reason) => (GroupDeliveryStatus::Failed.as_str(), Some(reason.as_str())),
    };
    match store.settle_agent_group_mention_by_delivery_id(delivery_id, status, reason, now_ms()) {
        Ok(Some(_)) => {
            notify_groups_changed(subscribers);
        }
        Ok(None) => {}
        Err(error) => {
            crate::dlog(&format!("[group] 结算群组投递 {delivery_id} 失败：{error}"));
        }
    }
}

pub(crate) fn notify_groups_changed(subscribers: &EventHubHandle) {
    if let Err(error) = subscribers.publish_groups_changed() {
        crate::dlog(&format!("[group] 推送 group.changed 失败：{error}"));
    }
}

/// 单次读入的内存 `WorkspaceDirectory` + `AcpSessions` 拼出成员 DTO（零每成员打开 SQLite）。
pub(crate) fn member_to_dto(
    member: &AgentGroupMemberRecord,
    directory: Option<&WorkspaceDirectory>,
    acp_sessions: &AcpSessions,
) -> GroupMemberView {
    let dir_conv = directory.and_then(|dir| {
        dir.conversations.iter().find(|conv| {
            conv.acp
                .as_ref()
                .is_some_and(|acp| acp.sid == member.session_id)
        })
    });
    let dir_acp = dir_conv.and_then(|conv| conv.acp.as_ref());
    let agent_label = dir_acp.map(|acp| acp.agent.clone());
    let cwd = dir_acp
        .and_then(|acp| acp.cwd.clone())
        .filter(|s| !s.trim().is_empty());

    let (phase, session_alive) = match acp_sessions.get(&member.session_id) {
        Some(slot) => {
            let alive = crate::acp_runtime_alive(&slot.value);
            let phase = format!("{:?}", slot.value.state.lock().phase).to_ascii_lowercase();
            (Some(phase), alive)
        }
        None => (None, false),
    };

    GroupMemberView {
        id: member.id.clone(),
        group_id: member.group_id.clone(),
        alias: member.alias.clone(),
        display_name: member.display_name.clone(),
        ownership: MemberOwnership::parse(&member.ownership).unwrap_or_default(),
        session_id: member.session_id.clone(),
        role_kind: MemberRoleKind::parse(&member.role_kind).unwrap_or_default(),
        role_title: member.role_title.clone(),
        role_prompt: member.role_prompt.clone(),
        tool_restriction_profile: member.tool_restriction_profile.clone(),
        briefing_sent: member.briefing_sent,
        last_delivered_seq: member.last_delivered_seq,
        last_read_seq: member.last_read_seq,
        muted: member.muted,
        removed_at_ms: member.removed_at_ms,
        created_at_ms: member.created_at_ms,
        agent_label,
        cwd,
        phase,
        session_alive,
    }
}

/// 批量水合群组消息：用单次 SQL `list_agent_group_mentions_for_messages` 拉齐所有 `@` 记录（零 N+1）。
pub(crate) fn hydrate_messages(
    store: &Store,
    records: Vec<AgentGroupMessageRecord>,
    members: &[AgentGroupMemberRecord],
) -> Result<Vec<GroupMessageView>, StoreError> {
    if records.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<String> = records.iter().map(|m| m.id.clone()).collect();
    let mut mentions_by_msg = store.list_agent_group_mentions_for_messages(&ids)?;
    let member_map: HashMap<&str, &AgentGroupMemberRecord> =
        members.iter().map(|m| (m.id.as_str(), m)).collect();

    Ok(records
        .into_iter()
        .map(|msg| {
            let mentions = mentions_by_msg.remove(&msg.id).unwrap_or_default();
            hydrate_single_message(msg, mentions, &member_map)
        })
        .collect())
}

pub(crate) fn hydrate_single_message(
    msg: AgentGroupMessageRecord,
    mentions: Vec<AgentGroupMentionRecord>,
    member_map: &HashMap<&str, &AgentGroupMemberRecord>,
) -> GroupMessageView {
    let sender_type = GroupMessageSenderType::parse(&msg.sender_type).unwrap_or_default();
    let (sender_alias, sender_display_name) = match sender_type {
        GroupMessageSenderType::User => (None, "User".to_string()),
        GroupMessageSenderType::System => (None, "System".to_string()),
        GroupMessageSenderType::Member => msg
            .sender_member_id
            .as_deref()
            .and_then(|id| member_map.get(id).copied())
            .map(|m| (Some(m.alias.clone()), m.display_name.clone()))
            .unwrap_or_else(|| (None, "Member".to_string())),
    };

    let mention_views = mentions
        .into_iter()
        .map(|mn| {
            let target_alias = member_map
                .get(mn.target_member_id.as_str())
                .map(|m| m.alias.clone())
                .unwrap_or_else(|| mn.target_member_id.clone());
            GroupMentionView {
                message_id: mn.message_id,
                target_member_id: mn.target_member_id,
                target_alias,
                mention_kind: GroupMentionKind::parse(&mn.mention_kind).unwrap_or_default(),
                delivery_status: GroupDeliveryStatus::parse(&mn.delivery_status)
                    .unwrap_or_default(),
                delivery_id: mn.delivery_id,
                status_reason: mn.status_reason,
                updated_at_ms: mn.updated_at_ms,
            }
        })
        .collect();

    GroupMessageView {
        id: msg.id,
        group_id: msg.group_id,
        seq: msg.seq,
        sender_type,
        sender_member_id: msg.sender_member_id,
        sender_alias,
        sender_display_name,
        kind: GroupMessageKind::parse(&msg.kind).unwrap_or_default(),
        content: msg.content,
        reply_to_seq: msg.reply_to_seq,
        thread_root_seq: msg.thread_root_seq,
        task_status: msg.task_status.as_deref().and_then(GroupTaskStatus::parse),
        chain_id: msg.chain_id,
        chain_depth: msg.chain_depth,
        auto_bridged: msg.auto_bridged,
        pinned: msg.pinned,
        created_at_ms: msg.created_at_ms,
        mentions: mention_views,
    }
}

pub(crate) fn build_group_detail(
    store: &Store,
    group: AgentGroupRecord,
    include_removed_members: bool,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
) -> Result<GroupDetail, ControlError> {
    let all_members = store
        .list_agent_group_members(&group.id, true)
        .map_err(map_store_error)?;
    // 单次读内存目录，避免为每个成员重复读库。
    let directory = workspace_directory.read().ok();
    let members: Vec<GroupMemberView> = all_members
        .iter()
        .filter(|m| include_removed_members || m.removed_at_ms.is_none())
        .map(|m| member_to_dto(m, directory.as_ref(), acp_sessions))
        .collect();

    let open_task_records = store
        .list_agent_group_open_tasks(&group.id)
        .map_err(map_store_error)?;
    let open_tasks =
        hydrate_messages(store, open_task_records, &all_members).map_err(map_store_error)?;

    Ok(GroupDetail {
        id: group.id,
        title: group.title,
        description: group.description,
        project_root: group.project_root,
        workset_id: group.workset_id,
        mode: GroupMode::parse(&group.mode).unwrap_or_default(),
        coordinator_member_id: group.coordinator_member_id,
        allow_worker_cross_talk: group.allow_worker_cross_talk,
        max_chain_hops: group.max_chain_hops,
        max_chain_depth: group.max_chain_depth,
        max_pair_ping_pong: group.max_pair_ping_pong,
        auto_bridge_mentions: group.auto_bridge_mentions,
        paused: group.paused,
        next_seq: group.next_seq,
        archived_at_ms: group.archived_at_ms,
        created_at_ms: group.created_at_ms,
        updated_at_ms: group.updated_at_ms,
        members,
        open_tasks,
    })
}
