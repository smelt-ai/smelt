//! 异构多智能体群组会话运行时引擎（`GroupRuntime`）。
//!
//! 设计见 `docs/agent-groups.md`：
//! - 独立运行时库 `~/.smelt/groups.sqlite3`（`smelt_store::GroupStore`，5 张极简表）；
//! - Commit-before-Publish：写消息与 mention 先原子落库，再推事件和唤醒成员；
//! - Delta Cursor + Delta Coalescing：成员忙时多次 `@` 合并为唯一一次增量投递；
//! - 盲评栅栏（`blind_prompt` + `blind_submission` 揭晓前硬隔离，全员交卷自动原子揭晓）；
//! - 三态 `ApprovalPosture` 权限水位线放行卡 + Chain Hop / 深度 / 乒乓熔断 + 人类插话抢占；
//! - 回合结束自动桥接（`ActiveGroupTurn` 防第三方引擎忘调 `group_post` / `group_blind_submit`）；
//! - 内存态顾问式文件锁（按 `(group_id, runtime_id, repo_root)` 隔离命名空间，TTL 过期）；
//! - 跨机器零信任白名单防线（`remote_group_member_grant`、远端 `User` 降级、`local_user_origin_nonce`、
//!   `published_artifact_id` 路径沙箱、每分钟速率限流）。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use smelt_core::acp_session::{AcpTurnOutcome, AcpUserAction};
use smelt_core::agent_group::{
    AgentGroupDto, AgentGroupMemberDto, AgentGroupMentionDto, AgentGroupMessageDto,
    GroupAttachment, GroupDeliveryStatus, GroupEnvelopeInput, GroupLeaseDto, GroupMemberLiveStatus,
    GroupMentionKind, GroupMessageKind, GroupMode, GroupSenderType, GroupSummaryDto,
    GroupTaskStatus, MemberOwnership, MemberRoleKind, RemoteGroupGrantDto, WorkspaceMode,
    extract_mentions, format_group_message_envelope, validate_member_alias,
};
use smelt_core::agent_kind::ApprovalPosture;
use smelt_core::control_api::{
    GroupArchiveParams, GroupBlindRevealParams, GroupBlindRevealResult, GroupBlindStartParams,
    GroupCreateMemberSpec, GroupCreateParams, GroupDeleteParams, GroupDetailResult, GroupIdParams,
    GroupLeaseReleaseParams, GroupListParams, GroupListResult, GroupMemberAddParams,
    GroupMemberIdParams, GroupMemberUpdateParams, GroupMemberWakeResult,
    GroupMentionDecisionParams, GroupMergeStrategy, GroupMergeTarget, GroupMessageListParams,
    GroupMessageListResult, GroupMessagePinParams, GroupMessagePostParams, GroupPauseParams,
    GroupRemoteGrantRevokeParams, GroupRemoteGrantUpsertParams, GroupResumeParams,
    GroupTaskUpdateParams, GroupUpdateParams, GroupWorktreeApplyPatchParams,
    GroupWorktreeApplyPatchResult, GroupWorktreeDiffResult, GroupWorktreeMergeParams,
    GroupWorktreeMergeResult, SessionCreateKind, SessionCreateParams,
};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};
use smelt_store::{
    GroupMemberRecord, GroupMemberUpdatePatch, GroupMentionRecord, GroupMessageQuery,
    GroupMessageRecord, GroupRecord, GroupStore, GroupUpdatePatch, NewGroupMention,
    NewGroupMessage, RemoteGroupMemberGrantRecord,
};

use crate::{AcpSessions, EventHubHandle, Sessions, WorkspaceDirectoryReader};

/// 增量打包硬上限：超过条数或字节数时自动折叠中间非点名消息。
const DELTA_MAX_MESSAGES: usize = 20;
const DELTA_MAX_BYTES: usize = 12 * 1024;
/// 跨机产物读取单次最大字节数（256 KB）。
const MAX_ARTIFACT_BYTES: usize = 256 * 1024;
/// 顾问式文件锁默认与最大 TTL。
const DEFAULT_LEASE_TTL_MS: i64 = 10 * 60 * 1000;
const MAX_LEASE_TTL_MS: i64 = 60 * 60 * 1000;
/// 跨机器每成员每分钟最大唤醒配额（防被攻破远端刷爆本机额度）。
const REMOTE_WAKE_RATE_LIMIT_PER_MIN: usize = 12;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn short_id(prefix: &str) -> String {
    let raw = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}-{}", &raw[..12])
}

fn err(code: ControlErrorCode, msg: impl Into<String>) -> ControlError {
    ControlError::new(code, msg)
}

fn group_store() -> Result<GroupStore, ControlError> {
    smelt_core::sqlite_state::default_group_store()
        .map_err(|e| err(ControlErrorCode::OperationFailed, e))
}

// ============================================================================
// 运行时内存状态（瞬态：跳数链、乒乓计数、Delta Coalescing、活跃群回合、文件锁、跨机零信任防线）
// ============================================================================

#[derive(Clone, Debug)]
struct ChainBudget {
    chain_id: String,
    hops_left: u32,
    pair_counts: HashMap<(String, String), u32>,
    /// 本条链内用户已批准免问的目标成员集合。
    approved_targets: HashSet<String>,
}

#[derive(Clone, Debug)]
struct PendingGroupWake {
    group_id: String,
    #[allow(dead_code)]
    member_id: String,
    trigger_message_ids: Vec<String>,
    trigger_seqs: Vec<i64>,
    trigger_label: String,
    blind_prompt_seq: Option<i64>,
    chain_id: Option<String>,
    chain_depth: u32,
}

/// 成员正在执行的群唤醒回合上下文（供回合结束自动桥接兜底使用）。
#[derive(Clone, Debug)]
pub(crate) struct ActiveGroupTurn {
    pub(crate) group_id: String,
    pub(crate) member_id: String,
    pub(crate) trigger_seqs: Vec<i64>,
    pub(crate) blind_prompt_seq: Option<i64>,
    #[allow(dead_code)]
    pub(crate) chain_id: Option<String>,
    pub(crate) chain_depth: u32,
    pub(crate) posted_or_yielded: bool,
}

#[derive(Clone, Debug)]
struct MemoryFileLease {
    id: String,
    group_id: String,
    runtime_id: Option<String>,
    repo_root: String,
    holder_member_id: String,
    holder_alias: String,
    path_globs: Vec<String>,
    reason: String,
    expires_at_ms: i64,
}

/// 本机显式发布过的产物白名单条目（Push-Only，远端绝不能传任意路径读本机磁盘）。
#[derive(Clone, Debug)]
struct PublishedArtifactEntry {
    canonical_path: PathBuf,
    start_line: Option<u32>,
    end_line: Option<u32>,
    group_id: String,
}

/// 跨机器远端成员附着记录（远端节点纯内存保存，断线重连后由群主重推恢复）。
#[derive(Clone, Debug)]
pub(crate) struct RemoteGroupAttachment {
    pub(crate) host_runtime_id: String,
    pub(crate) group_id: String,
    pub(crate) member_id: String,
    #[allow(dead_code)]
    pub(crate) alias: String,
    pub(crate) session_id: String,
}

#[derive(Default)]
struct RuntimeState {
    chains: HashMap<String, ChainBudget>,
    /// key: (group_id, member_id)
    pending_wakes: HashMap<(String, String), PendingGroupWake>,
    /// key: session_id -> 当前正在跑的群唤醒回合
    active_turns: HashMap<String, ActiveGroupTurn>,
    leases: Vec<MemoryFileLease>,
    /// 本机用户发往远端群主的消息一次性 nonce（nonce -> 过期时间戳 ms，带 TTL 防重放与内存增长）。
    local_user_nonces: HashMap<String, i64>,
    /// 仓库级合并串行锁：防止多个成员同时对同一 `target_cwd` 并发执行 `git checkout/merge`。
    repo_merge_locks: HashMap<String, std::sync::Arc<Mutex<()>>>,
    /// Push-Only 产物白名单：artifact_id -> 已通过 realpath 校验的路径。
    published_artifacts: HashMap<String, PublishedArtifactEntry>,
    /// 远端唤醒每分钟滑动窗口限流：key = (host_runtime_id, local_session_id) -> 时间戳队列。
    remote_wake_window: HashMap<(String, String), VecDeque<i64>>,
    /// 当本机作为远端群主的成员节点时的纯内存附着表。
    remote_attachments: Vec<RemoteGroupAttachment>,
    /// 远端成员实时在线状态缓存：key = (runtime_id, session_id) -> status。
    remote_live_status: HashMap<(String, String), GroupMemberLiveStatus>,
    /// 群主侧：已反向附着（`op=node_group_attach`）的远端成员节点活跃下行通道（key = peer runtime_id）。
    attached_peer_streams: HashMap<String, std::sync::mpsc::Sender<Value>>,
    /// 成员侧：已启动外拨 `op=node_group_attach` 守护线程的远端群主集合（去重）。
    active_outbound_attaches: HashSet<String>,
}

static STATE: LazyLock<Mutex<RuntimeState>> = LazyLock::new(Default::default);

fn lock_state() -> std::sync::MutexGuard<'static, RuntimeState> {
    STATE.lock().unwrap_or_else(|p| p.into_inner())
}

fn normalized_pair(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

// ============================================================================
// 启动自愈 `resume()` 与快照构建
// ============================================================================

/// 守护启动时调用：扫描 `groups.sqlite3` 中未终结的 `queued` / `offline_pending` 投递，
/// 重建内存待唤醒槽位并按需触发投递（幂等 `delivery_id` 防重复执行）。
pub(crate) fn resume_on_startup(acp_sessions: &AcpSessions, subscribers: &EventHubHandle) {
    let Ok(store) = group_store() else {
        return;
    };
    let Ok(pending) = store.list_pending_mentions(None) else {
        return;
    };
    let mut to_kick = Vec::new();
    for mention in pending {
        if mention.delivery_status != "queued" {
            continue;
        }
        let Ok(Some(group)) = store.get_group(&mention.group_id) else {
            continue;
        };
        if group.paused || group.archived_at_ms.is_some() {
            continue;
        }
        let Ok(Some(member)) = store.get_member(&mention.target_member_id) else {
            continue;
        };
        if member.removed_at_ms.is_some() || member.muted {
            continue;
        }
        let trigger_label = if mention.mention_kind == "blind_invite" {
            "blind_round"
        } else {
            "resumed"
        };
        enqueue_wake_slot(
            &group.id,
            &member.id,
            mention.message_id.clone(),
            0,
            trigger_label,
            None,
            None,
            0,
        );
        to_kick.push((group.id, member));
    }
    for (group_id, member) in to_kick {
        if member.runtime_id.is_none() {
            let _ = try_dispatch_local_wake(&store, &group_id, &member, acp_sessions, subscribers);
        }
    }

    // 成员节点启动自愈：按本地白名单 `remote_group_member_grant` 主动向远端群主重建反向附着流
    if let Ok(grants) = store.list_remote_grants(None) {
        let mut hosts = HashSet::new();
        {
            let mut state = lock_state();
            for grant in &grants {
                hosts.insert(grant.host_runtime_id.clone());
                if !state.remote_attachments.iter().any(|a| {
                    a.host_runtime_id == grant.host_runtime_id
                        && a.group_id == grant.group_id
                        && a.member_id == grant.member_id
                }) {
                    state.remote_attachments.push(RemoteGroupAttachment {
                        host_runtime_id: grant.host_runtime_id.clone(),
                        group_id: grant.group_id.clone(),
                        member_id: grant.member_id.clone(),
                        alias: grant.member_id.clone(),
                        session_id: grant.local_session_id.clone(),
                    });
                }
            }
        }
        for host_runtime_id in hosts {
            spawn_outbound_group_attach(
                host_runtime_id,
                std::sync::Arc::clone(acp_sessions),
                std::sync::Arc::clone(subscribers),
            );
        }
    }
}

/// 构造 `group.changed` 快照载荷（供 `EventHub` 的 `GroupsSnapshotProvider` 调用）。
pub(crate) fn snapshot_groups_json(acp_sessions: &AcpSessions) -> Value {
    let Ok(store) = group_store() else {
        return json!({ "groups": [], "group_owned_session_ids": [] });
    };
    let groups = list_groups_on(&store, false, acp_sessions).unwrap_or_default();
    let group_owned_session_ids = store.list_group_owned_session_ids().unwrap_or_default();
    serde_json::to_value(GroupListResult {
        groups,
        group_owned_session_ids,
    })
    .unwrap_or_else(|_| json!({ "groups": [], "group_owned_session_ids": [] }))
}

fn notify_groups_changed(subscribers: &EventHubHandle, acp_sessions: &AcpSessions) {
    let payload = snapshot_groups_json(acp_sessions);
    let _ = subscribers.publish_groups_changed(payload);
}

fn notify_message_appended(subscribers: &EventHubHandle, event: Value) {
    let _ = subscribers.publish_group_message_appended(event);
}

mod blind;
mod crud;
mod remote;
mod routing;
mod tools;
mod worktree;

#[cfg(test)]
mod tests;

pub(crate) use blind::{reveal_blind_round, start_blind_round};
pub(crate) use crud::{
    add_member, archive_group, create_group, delete_group, get_group, list_groups, pause_group,
    pin_message, remove_member, resume_group, update_group, update_member, update_task_status,
    wake_member,
};
pub(crate) use remote::{
    handle_inbound_node_group_call, handle_inbound_remote_deliver, handle_node_group_attach,
    handle_node_group_link, revoke_remote_grant, upsert_remote_grant,
};
pub(crate) use routing::{
    approve_mention, list_messages, on_member_turn_ended, post_user_message, reject_mention,
};
pub(crate) use tools::{
    dispatch_agent_group_tool, dispatch_agent_group_tool_for_runtime, release_file_lease,
    session_in_any_group,
};
pub(crate) use worktree::{apply_member_patch, diff_member_worktree, merge_member_worktree};

use blind::submit_blind_entry;
use crud::list_groups_on;
use remote::{
    dispatch_remote_wake_or_mark_offline, find_remote_attachment, forward_remote_member_tool_call,
    parse_and_register_published_artifacts, parse_remote_published_attachments,
    read_published_artifact, spawn_outbound_group_attach,
};
use routing::{
    PostMessageRequest, build_delta_envelope, enqueue_wake_slot, ensure_local_session_awake,
    format_delta_messages_with_folding, post_message_inner, try_dispatch_local_wake,
};
use tools::{acquire_file_lease, list_active_leases};
// ============================================================================
// 辅助映射与状态推导（严格读现有单一权威表）
// ============================================================================

fn session_approval_posture(session_id: &str, acp_sessions: &AcpSessions) -> ApprovalPosture {
    crate::peer_messages::session_approval_posture(session_id, acp_sessions)
}

fn member_approval_posture(
    member: &GroupMemberRecord,
    acp_sessions: &AcpSessions,
) -> ApprovalPosture {
    if member.runtime_id.is_some() {
        ApprovalPosture::Unknown
    } else {
        session_approval_posture(&member.session_id, acp_sessions)
    }
}

fn session_has_mcp_or_pi(session_id: &str, acp_sessions: &AcpSessions) -> bool {
    let Some(slot) = acp_sessions.get(session_id) else {
        return true;
    };
    crate::acp_host::session_supports_group_tools(&slot.value)
}

fn member_live_status(
    member: &GroupMemberRecord,
    acp_sessions: &AcpSessions,
) -> GroupMemberLiveStatus {
    if let Some(remote_id) = member.runtime_id.as_deref() {
        return lock_state()
            .remote_live_status
            .get(&(remote_id.to_string(), member.session_id.clone()))
            .copied()
            .unwrap_or(GroupMemberLiveStatus::Idle);
    }
    let Some(slot) = acp_sessions.get(&member.session_id) else {
        return GroupMemberLiveStatus::Dormant;
    };
    let reduced = slot.value.reduced.lock();
    match reduced.phase {
        smelt_core::daemon_state::DaemonPhase::Idle => GroupMemberLiveStatus::Idle,
        smelt_core::daemon_state::DaemonPhase::AwaitingApproval
        | smelt_core::daemon_state::DaemonPhase::WaitingForUser => {
            GroupMemberLiveStatus::WaitingApproval
        }
        smelt_core::daemon_state::DaemonPhase::Failed => GroupMemberLiveStatus::Error,
        _ => GroupMemberLiveStatus::Running,
    }
}

fn resolve_session_cwd_and_branch(session_id: &str) -> (Option<String>, Option<String>) {
    let Ok(main_store) = smelt_core::sqlite_state::default_sqlite_store() else {
        return (None, None);
    };
    let Ok((_, snapshot)) = main_store.workspace_directory() else {
        return (None, None);
    };
    for conv in &snapshot.sessions {
        if let Some(acp) = &conv.acp
            && acp.sid.as_deref() == Some(session_id)
        {
            let branch = conv
                .workset_id
                .as_deref()
                .and_then(|wid| snapshot.worksets.iter().find(|w| w.id == wid))
                .map(|w| w.branch.clone());
            return (acp.cwd.clone(), branch);
        }
    }
    (None, None)
}

fn resolve_session_agent_info(session_id: &str) -> (Option<String>, Option<String>) {
    let Ok(main_store) = smelt_core::sqlite_state::default_sqlite_store() else {
        return (None, None);
    };
    let Ok((_, snapshot)) = main_store.workspace_directory() else {
        return (None, None);
    };
    for conv in &snapshot.sessions {
        if let Some(acp) = &conv.acp
            && acp.sid.as_deref() == Some(session_id)
        {
            return (Some(acp.agent.clone()), Some(acp.agent.clone()));
        }
    }
    (None, None)
}

fn group_to_dto(record: &GroupRecord) -> AgentGroupDto {
    let hops_left = lock_state().chains.get(&record.id).map(|b| b.hops_left);
    AgentGroupDto {
        id: record.id.clone(),
        title: record.title.clone(),
        description: record.description.clone(),
        workset_id: record.workset_id.clone(),
        mode: GroupMode::parse(&record.mode).unwrap_or(GroupMode::Mention),
        coordinator_member_id: record.coordinator_member_id.clone(),
        allow_worker_cross_talk: record.allow_worker_cross_talk,
        max_chain_hops: record.max_chain_hops,
        max_chain_depth: record.max_chain_depth,
        max_pair_ping_pong: record.max_pair_ping_pong,
        auto_bridge_mentions: record.auto_bridge_mentions,
        paused: record.paused,
        next_seq: record.next_seq,
        active_chain_hops_left: hops_left,
        archived_at_ms: record.archived_at_ms,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

fn member_to_dto(record: &GroupMemberRecord, acp_sessions: &AcpSessions) -> AgentGroupMemberDto {
    let (agent, agent_label) = if record.runtime_id.is_none() {
        resolve_session_agent_info(&record.session_id)
    } else {
        (None, None)
    };
    let (cwd, branch) = if record.runtime_id.is_none() {
        resolve_session_cwd_and_branch(&record.session_id)
    } else {
        (None, None)
    };
    AgentGroupMemberDto {
        id: record.id.clone(),
        group_id: record.group_id.clone(),
        alias: record.alias.clone(),
        display_name: record.display_name.clone(),
        ownership: MemberOwnership::parse(&record.ownership).unwrap_or(MemberOwnership::GroupOwned),
        runtime_id: record.runtime_id.clone(),
        session_id: record.session_id.clone(),
        role_kind: MemberRoleKind::parse(&record.role_kind).unwrap_or(MemberRoleKind::Worker),
        role_title: record.role_title.clone(),
        role_prompt: record.role_prompt.clone(),
        workspace_mode: WorkspaceMode::parse(&record.workspace_mode)
            .unwrap_or(WorkspaceMode::SharedReadonly),
        tool_restriction_profile: record.tool_restriction_profile.clone(),
        briefing_sent: record.briefing_sent,
        last_delivered_seq: record.last_delivered_seq,
        last_read_seq: record.last_read_seq,
        muted: record.muted,
        agent,
        agent_label,
        cwd,
        branch,
        live_status: member_live_status(record, acp_sessions),
        created_at_ms: record.created_at_ms,
    }
}

fn message_to_dto(
    store: &GroupStore,
    record: &GroupMessageRecord,
    member_map: &HashMap<String, &GroupMemberRecord>,
) -> AgentGroupMessageDto {
    let mentions = store
        .list_mentions_for_message(&record.id)
        .unwrap_or_default();
    message_to_dto_with_mentions(record, &mentions, member_map)
}

fn message_to_dto_with_mentions(
    record: &GroupMessageRecord,
    mentions: &[GroupMentionRecord],
    member_map: &HashMap<String, &GroupMemberRecord>,
) -> AgentGroupMessageDto {
    let sender_member = record
        .sender_member_id
        .as_deref()
        .and_then(|id| member_map.get(id));
    let attachments: Vec<GroupAttachment> =
        serde_json::from_slice(&record.attachments_json).unwrap_or_default();
    let mention_dtos = mentions
        .iter()
        .map(|m| AgentGroupMentionDto {
            message_id: m.message_id.clone(),
            target_member_id: m.target_member_id.clone(),
            target_alias: member_map
                .get(&m.target_member_id)
                .map(|rec| rec.alias.clone()),
            mention_kind: GroupMentionKind::parse(&m.mention_kind)
                .unwrap_or(GroupMentionKind::Direct),
            delivery_status: GroupDeliveryStatus::parse(&m.delivery_status)
                .unwrap_or(GroupDeliveryStatus::Queued),
            delivery_id: m.delivery_id.clone(),
            status_reason: m.status_reason.clone(),
            updated_at_ms: m.updated_at_ms,
        })
        .collect();
    AgentGroupMessageDto {
        id: record.id.clone(),
        group_id: record.group_id.clone(),
        seq: record.seq,
        sender_type: GroupSenderType::parse(&record.sender_type).unwrap_or(GroupSenderType::User),
        sender_member_id: record.sender_member_id.clone(),
        sender_alias: sender_member.map(|m| m.alias.clone()),
        sender_label: sender_member.map(|m| m.display_name.clone()),
        kind: GroupMessageKind::parse(&record.kind).unwrap_or(GroupMessageKind::Chat),
        content: record.content.clone(),
        attachments,
        reply_to_seq: record.reply_to_seq,
        thread_root_seq: record.thread_root_seq,
        blind_deadline_at_ms: record.blind_deadline_at_ms,
        blind_revealed_at_ms: record.blind_revealed_at_ms,
        task_status: record
            .task_status
            .as_deref()
            .and_then(GroupTaskStatus::parse),
        chain_id: record.chain_id.clone(),
        chain_depth: record.chain_depth,
        auto_bridged: record.auto_bridged,
        pinned: record.pinned,
        mentions: mention_dtos,
        created_at_ms: record.created_at_ms,
    }
}

fn lease_to_dto(lease: &MemoryFileLease) -> GroupLeaseDto {
    GroupLeaseDto {
        id: lease.id.clone(),
        group_id: lease.group_id.clone(),
        runtime_id: lease.runtime_id.clone(),
        repo_root: lease.repo_root.clone(),
        holder_member_id: lease.holder_member_id.clone(),
        holder_alias: lease.holder_alias.clone(),
        path_globs: lease.path_globs.clone(),
        reason: lease.reason.clone(),
        expires_at_ms: lease.expires_at_ms,
    }
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    let first_line = text.lines().next().unwrap_or_default().trim();
    if first_line.chars().count() <= max_chars {
        first_line.to_string()
    } else {
        let prefix: String = first_line.chars().take(max_chars).collect();
        format!("{prefix}…")
    }
}
