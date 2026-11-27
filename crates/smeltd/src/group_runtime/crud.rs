//! 群组与成员的生命周期管理（创建、修改、归档、删除、暂停/恢复、消息查询与任务状态更新）。

use smelt_core::agent_group::{
    GroupDetail, GroupMemberView, MemberOwnership, MemberRoleKind, validate_member_alias,
};
use smelt_core::automation_store::AutomationStore;
use smelt_core::control_api::{
    GroupArchiveParams, GroupCreateParams, GroupDeleteParams, GroupDeleteResult, GroupGetParams,
    GroupListParams, GroupListResult, GroupMemberAddParams, GroupMemberRemoveParams,
    GroupMemberRemoveResult, GroupMemberUpdateParams, GroupMessageListParams,
    GroupMessageListResult, GroupMessagePinParams, GroupMessagePinResult, GroupPauseParams,
    GroupResumeParams, GroupTaskUpdateParams, GroupTaskUpdateResult, GroupUpdateParams,
    SessionCreateKind, SessionCreateParams,
};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};
use smelt_store::{AgentGroupMemberRecord, AgentGroupMessageQuery, AgentGroupRecord, Store};

use super::{
    build_group_detail, build_group_summaries, default_store, hydrate_messages, lock_state,
    map_store_error, member_to_dto, notify_groups_changed, now_ms, routing,
};
use crate::{AcpSessions, EventHubHandle, Sessions, WorkspaceDirectoryReader};

pub(crate) fn create_group(
    params: GroupCreateParams,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupDetail, ControlError> {
    let store = default_store()?;
    create_group_with_store(
        &store,
        params,
        workspace_directory,
        acp_sessions,
        subscribers,
    )
}

pub(crate) fn create_group_with_store(
    store: &Store,
    params: GroupCreateParams,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupDetail, ControlError> {
    let title = params.title.trim().to_string();
    if title.is_empty() {
        return Err(ControlError::new(
            ControlErrorCode::InvalidParams,
            "Group title cannot be empty",
        ));
    }
    let now = now_ms();
    let group = AgentGroupRecord {
        id: format!("grp-{}", uuid::Uuid::new_v4().simple()),
        title,
        description: params.description.trim().to_string(),
        project_root: params
            .project_root
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        workset_id: params
            .workset_id
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        mode: params.mode.as_str().to_string(),
        coordinator_member_id: None,
        allow_worker_cross_talk: params.allow_worker_cross_talk,
        max_chain_hops: params.max_chain_hops,
        max_chain_depth: params.max_chain_depth,
        max_pair_ping_pong: params.max_pair_ping_pong,
        auto_bridge_mentions: params.auto_bridge_mentions,
        paused: false,
        next_seq: 1,
        archived_at_ms: None,
        created_at_ms: now,
        updated_at_ms: now,
    };
    store.insert_agent_group(&group).map_err(map_store_error)?;
    notify_groups_changed(subscribers);
    build_group_detail(store, group, false, workspace_directory, acp_sessions)
}

pub(crate) fn list_groups(params: GroupListParams) -> Result<GroupListResult, ControlError> {
    let store = default_store()?;
    list_groups_with_store(&store, params)
}

pub(crate) fn list_groups_with_store(
    store: &Store,
    params: GroupListParams,
) -> Result<GroupListResult, ControlError> {
    let groups = build_group_summaries(store, params.include_archived).map_err(map_store_error)?;
    let group_owned_session_ids = store
        .list_group_owned_session_ids()
        .map_err(map_store_error)?;
    Ok(GroupListResult {
        groups,
        group_owned_session_ids,
    })
}

pub(crate) fn get_group(
    params: GroupGetParams,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
) -> Result<GroupDetail, ControlError> {
    let store = default_store()?;
    get_group_with_store(&store, params, workspace_directory, acp_sessions)
}

pub(crate) fn get_group_with_store(
    store: &Store,
    params: GroupGetParams,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
) -> Result<GroupDetail, ControlError> {
    let group = store
        .get_agent_group(&params.group_id)
        .map_err(map_store_error)?
        .ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::NotFound,
                format!("Group not found: {}", params.group_id),
            )
        })?;
    build_group_detail(
        store,
        group,
        params.include_removed_members,
        workspace_directory,
        acp_sessions,
    )
}

pub(crate) fn update_group(
    params: GroupUpdateParams,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupDetail, ControlError> {
    let store = default_store()?;
    update_group_with_store(
        &store,
        params,
        workspace_directory,
        acp_sessions,
        subscribers,
    )
}

pub(crate) fn update_group_with_store(
    store: &Store,
    params: GroupUpdateParams,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupDetail, ControlError> {
    let mut group = store
        .get_agent_group(&params.group_id)
        .map_err(map_store_error)?
        .ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::NotFound,
                format!("Group not found: {}", params.group_id),
            )
        })?;

    if let Some(title) = params.title {
        let trimmed = title.trim();
        if trimmed.is_empty() {
            return Err(ControlError::new(
                ControlErrorCode::InvalidParams,
                "Group title cannot be empty",
            ));
        }
        group.title = trimmed.to_string();
    }
    if let Some(description) = params.description {
        group.description = description.trim().to_string();
    }
    if let Some(mode) = params.mode {
        group.mode = mode.as_str().to_string();
    }
    if let Some(coord) = params.coordinator_member_id {
        if let Some(ref member_id) = coord {
            let member = store
                .get_agent_group_member(member_id)
                .map_err(map_store_error)?
                .filter(|m| m.group_id == group.id && m.removed_at_ms.is_none());
            if member.is_none() {
                return Err(ControlError::new(
                    ControlErrorCode::InvalidParams,
                    format!("Coordinator member not found in group: {member_id}"),
                ));
            }
        }
        group.coordinator_member_id = coord;
    }
    if let Some(v) = params.allow_worker_cross_talk {
        group.allow_worker_cross_talk = v;
    }
    if let Some(v) = params.max_chain_hops {
        group.max_chain_hops = v;
    }
    if let Some(v) = params.max_chain_depth {
        group.max_chain_depth = v;
    }
    if let Some(v) = params.max_pair_ping_pong {
        group.max_pair_ping_pong = v;
    }
    if let Some(v) = params.auto_bridge_mentions {
        group.auto_bridge_mentions = v;
    }
    group.updated_at_ms = now_ms();

    store.update_agent_group(&group).map_err(map_store_error)?;
    notify_groups_changed(subscribers);
    build_group_detail(store, group, false, workspace_directory, acp_sessions)
}

pub(crate) fn archive_group(
    params: GroupArchiveParams,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupDetail, ControlError> {
    let store = default_store()?;
    let mut group = store
        .get_agent_group(&params.group_id)
        .map_err(map_store_error)?
        .ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::NotFound,
                format!("Group not found: {}", params.group_id),
            )
        })?;
    let now = now_ms();
    group.archived_at_ms = if params.archived { Some(now) } else { None };
    if params.archived {
        group.paused = true;
    }
    group.updated_at_ms = now;
    store.update_agent_group(&group).map_err(map_store_error)?;
    notify_groups_changed(subscribers);
    build_group_detail(&store, group, false, workspace_directory, acp_sessions)
}

/// 删除群组，并仅清理 `ownership = group_owned` 的底层会话；绝不触碰 `linked` 会话。
pub(crate) fn delete_group(
    params: GroupDeleteParams,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    workspace_directory: &WorkspaceDirectoryReader,
    automations: &AutomationStore,
    subscribers: &EventHubHandle,
) -> Result<GroupDeleteResult, ControlError> {
    let store = default_store()?;
    let members = store
        .list_agent_group_members(&params.group_id, false)
        .map_err(map_store_error)?;
    let owned_session_ids: Vec<String> = members
        .into_iter()
        .filter(|m| m.ownership == MemberOwnership::GroupOwned.as_str())
        .map(|m| m.session_id)
        .collect();

    let deleted = store
        .delete_agent_group(&params.group_id)
        .map_err(map_store_error)?;
    if deleted {
        lock_state().chain_budgets.remove(&params.group_id);
        for session_id in owned_session_ids {
            if let Err(error) = crate::session_control::delete_session(
                &session_id,
                sessions,
                acp_sessions,
                workspace_directory,
                automations,
                subscribers,
            ) {
                crate::dlog(&format!(
                    "[group] 删群 {} 后清理 group_owned 会话 {session_id} 失败：{}",
                    params.group_id, error.message
                ));
            }
        }
        notify_groups_changed(subscribers);
    }
    Ok(GroupDeleteResult { deleted })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_member(
    params: GroupMemberAddParams,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    workspace_directory: &WorkspaceDirectoryReader,
    automations: &AutomationStore,
    subscribers: &EventHubHandle,
) -> Result<GroupMemberView, ControlError> {
    let store = default_store()?;
    let mut group = store
        .get_agent_group(&params.group_id)
        .map_err(map_store_error)?
        .ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::NotFound,
                format!("Group not found: {}", params.group_id),
            )
        })?;

    let alias = validate_member_alias(&params.alias)
        .map_err(|message| ControlError::new(ControlErrorCode::InvalidParams, message))?;
    let active_members = store
        .list_agent_group_members(&group.id, false)
        .map_err(map_store_error)?;
    if active_members.iter().any(|m| m.alias == alias) {
        return Err(ControlError::new(
            ControlErrorCode::Conflict,
            format!("Alias @{alias} is already used in this group"),
        ));
    }

    let member_uuid = uuid::Uuid::new_v4();
    let member_id = format!("mem-{}", member_uuid.simple());
    let display_name = {
        let trimmed = params.display_name.trim();
        if trimmed.is_empty() {
            alias.clone()
        } else {
            trimmed.to_string()
        }
    };

    let (session_id, created_owned_session) = match params.ownership {
        MemberOwnership::Linked => {
            let sid = params
                .session_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    ControlError::new(
                        ControlErrorCode::InvalidParams,
                        "Linked member requires session_id",
                    )
                })?
                .to_string();
            if active_members.iter().any(|m| m.session_id == sid) {
                return Err(ControlError::new(
                    ControlErrorCode::Conflict,
                    format!("Conversation {sid} is already in this group"),
                ));
            }
            let exists_in_dir = workspace_directory.read().ok().is_some_and(|dir| {
                dir.conversations
                    .iter()
                    .any(|c| c.acp.as_ref().is_some_and(|acp| acp.sid == sid))
            });
            if acp_sessions.get(&sid).is_none() && !exists_in_dir {
                return Err(ControlError::new(
                    ControlErrorCode::NotFound,
                    format!("Conversation not found: {sid}"),
                ));
            }
            (sid, false)
        }
        MemberOwnership::GroupOwned => {
            let agent_option_id = params
                .agent
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("pi")
                .to_string();
            let cwd = params
                .cwd
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| group.project_root.clone());
            let disallowed_tools = if params.tool_restriction_profile.trim() == "read_only" {
                vec!["Write".to_string(), "Edit".to_string(), "Bash".to_string()]
            } else {
                Vec::new()
            };
            // 使用唯一成员 UUID 作为 request_id，删除成员后按同 alias 重建绝不撞 `session_control` 去重。
            let created = crate::session_control::create_session(
                SessionCreateParams {
                    project_root: group.project_root.clone().unwrap_or_default(),
                    cwd,
                    workset_id: group.workset_id.clone(),
                    kind: Some(SessionCreateKind::Acp),
                    launch_key: None,
                    agent_option_id: Some(agent_option_id),
                    resume_id: None,
                    request_id: Some(member_uuid.to_string()),
                    title: Some(format!("{} · @{alias}", group.title)),
                    disallowed_tools,
                },
                sessions,
                acp_sessions,
                workspace_directory,
                subscribers,
            )?;
            (created.session_id, true)
        }
    };

    let now = now_ms();
    let member = AgentGroupMemberRecord {
        id: member_id,
        group_id: group.id.clone(),
        alias,
        display_name,
        ownership: params.ownership.as_str().to_string(),
        session_id: session_id.clone(),
        role_kind: params.role_kind.as_str().to_string(),
        role_title: params.role_title.trim().to_string(),
        role_prompt: params.role_prompt.trim().to_string(),
        tool_restriction_profile: normalize_tool_profile(&params.tool_restriction_profile),
        briefing_sent: false,
        last_delivered_seq: (group.next_seq - 1).max(0),
        last_read_seq: (group.next_seq - 1).max(0),
        muted: false,
        removed_at_ms: None,
        created_at_ms: now,
    };

    if let Err(error) = store.insert_agent_group_member(&member, now) {
        if created_owned_session {
            let _ = crate::session_control::delete_session(
                &session_id,
                sessions,
                acp_sessions,
                workspace_directory,
                automations,
                subscribers,
            );
        }
        return Err(map_store_error(error));
    }

    if params.role_kind == MemberRoleKind::Coordinator && group.coordinator_member_id.is_none() {
        group.coordinator_member_id = Some(member.id.clone());
        group.updated_at_ms = now;
        let _ = store.update_agent_group(&group);
    }

    notify_groups_changed(subscribers);
    let directory = workspace_directory.read().ok();
    Ok(member_to_dto(&member, directory.as_ref(), acp_sessions))
}

pub(crate) fn update_member(
    params: GroupMemberUpdateParams,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupMemberView, ControlError> {
    let store = default_store()?;
    let mut member = store
        .get_agent_group_member(&params.member_id)
        .map_err(map_store_error)?
        .filter(|m| m.group_id == params.group_id && m.removed_at_ms.is_none())
        .ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::NotFound,
                format!("Group member not found: {}", params.member_id),
            )
        })?;

    if let Some(alias) = params.alias {
        let normalized = validate_member_alias(&alias)
            .map_err(|message| ControlError::new(ControlErrorCode::InvalidParams, message))?;
        if normalized != member.alias {
            let active = store
                .list_agent_group_members(&params.group_id, false)
                .map_err(map_store_error)?;
            if active
                .iter()
                .any(|m| m.id != member.id && m.alias == normalized)
            {
                return Err(ControlError::new(
                    ControlErrorCode::Conflict,
                    format!("Alias @{normalized} is already used in this group"),
                ));
            }
            member.alias = normalized;
        }
    }
    if let Some(name) = params.display_name {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            member.display_name = trimmed.to_string();
        }
    }
    if let Some(role_kind) = params.role_kind {
        member.role_kind = role_kind.as_str().to_string();
    }
    if let Some(role_title) = params.role_title {
        member.role_title = role_title.trim().to_string();
    }
    if let Some(role_prompt) = params.role_prompt {
        member.role_prompt = role_prompt.trim().to_string();
    }
    if let Some(profile) = params.tool_restriction_profile {
        member.tool_restriction_profile = normalize_tool_profile(&profile);
    }
    if let Some(muted) = params.muted {
        member.muted = muted;
    }

    let now = now_ms();
    store
        .update_agent_group_member(&member, now)
        .map_err(map_store_error)?;
    notify_groups_changed(subscribers);
    let directory = workspace_directory.read().ok();
    Ok(member_to_dto(&member, directory.as_ref(), acp_sessions))
}

pub(crate) fn remove_member(
    params: GroupMemberRemoveParams,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    workspace_directory: &WorkspaceDirectoryReader,
    automations: &AutomationStore,
    subscribers: &EventHubHandle,
) -> Result<GroupMemberRemoveResult, ControlError> {
    let store = default_store()?;
    let member = store
        .get_agent_group_member(&params.member_id)
        .map_err(map_store_error)?
        .filter(|m| m.group_id == params.group_id && m.removed_at_ms.is_none());
    let Some(member) = member else {
        return Ok(GroupMemberRemoveResult { removed: false });
    };

    let removed = store
        .mark_agent_group_member_removed(&params.group_id, &params.member_id, now_ms())
        .map_err(map_store_error)?;
    if removed {
        // 仅清理 group_owned 且显式请求删除的底层会话；linked 会话绝不删除。
        if params.delete_owned_session && member.ownership == MemberOwnership::GroupOwned.as_str() {
            let _ = crate::session_control::delete_session(
                &member.session_id,
                sessions,
                acp_sessions,
                workspace_directory,
                automations,
                subscribers,
            );
        }
        notify_groups_changed(subscribers);
    }
    Ok(GroupMemberRemoveResult { removed })
}

/// 暂停群组。当 `abort_running = true` 时，仅中止 `ownership == group_owned` 的活跃成员回合，
/// 绝不中止用户的 `linked` 会话。
pub(crate) fn pause_group(
    params: GroupPauseParams,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupDetail, ControlError> {
    let store = default_store()?;
    pause_group_with_store(
        &store,
        params,
        workspace_directory,
        acp_sessions,
        subscribers,
    )
}

pub(crate) fn pause_group_with_store(
    store: &Store,
    params: GroupPauseParams,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupDetail, ControlError> {
    let mut group = store
        .get_agent_group(&params.group_id)
        .map_err(map_store_error)?
        .ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::NotFound,
                format!("Group not found: {}", params.group_id),
            )
        })?;
    group.paused = true;
    group.updated_at_ms = now_ms();
    store.update_agent_group(&group).map_err(map_store_error)?;

    if params.abort_running {
        let members = store
            .list_agent_group_members(&group.id, false)
            .map_err(map_store_error)?;
        for member in members {
            if member.ownership == MemberOwnership::GroupOwned.as_str() {
                crate::acp_host::cancel_acp_session_if_running(
                    &member.session_id,
                    acp_sessions,
                    subscribers,
                );
            }
        }
    }

    notify_groups_changed(subscribers);
    build_group_detail(store, group, false, workspace_directory, acp_sessions)
}

/// 恢复群组：仅重新派发本群的 `queued` 投递（绝不波及其他群组）。
pub(crate) fn resume_group(
    params: GroupResumeParams,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupDetail, ControlError> {
    let store = default_store()?;
    resume_group_with_store(
        &store,
        params,
        workspace_directory,
        acp_sessions,
        subscribers,
    )
}

pub(crate) fn resume_group_with_store(
    store: &Store,
    params: GroupResumeParams,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupDetail, ControlError> {
    let mut group = store
        .get_agent_group(&params.group_id)
        .map_err(map_store_error)?
        .ok_or_else(|| {
            ControlError::new(
                ControlErrorCode::NotFound,
                format!("Group not found: {}", params.group_id),
            )
        })?;
    group.paused = false;
    group.updated_at_ms = now_ms();
    store.update_agent_group(&group).map_err(map_store_error)?;

    routing::resume_group_queued_mentions(store, &group.id, acp_sessions, subscribers);
    notify_groups_changed(subscribers);
    build_group_detail(store, group, false, workspace_directory, acp_sessions)
}

pub(crate) fn list_messages(
    params: GroupMessageListParams,
) -> Result<GroupMessageListResult, ControlError> {
    let store = default_store()?;
    list_messages_with_store(&store, params)
}

pub(crate) fn list_messages_with_store(
    store: &Store,
    params: GroupMessageListParams,
) -> Result<GroupMessageListResult, ControlError> {
    if store
        .get_agent_group(&params.group_id)
        .map_err(map_store_error)?
        .is_none()
    {
        return Err(ControlError::new(
            ControlErrorCode::NotFound,
            format!("Group not found: {}", params.group_id),
        ));
    }
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let mut records = store
        .list_agent_group_messages(&AgentGroupMessageQuery {
            group_id: params.group_id.clone(),
            since_seq: params.since_seq,
            before_seq: params.before_seq,
            limit: limit + 1,
            pinned_only: params.pinned_only,
        })
        .map_err(map_store_error)?;
    let has_more = records.len() > limit;
    if has_more {
        if params.since_seq.is_some() {
            records.truncate(limit);
        } else {
            records.remove(0);
        }
    }
    let members = store
        .list_agent_group_members(&params.group_id, true)
        .map_err(map_store_error)?;
    let messages = hydrate_messages(store, records, &members).map_err(map_store_error)?;
    Ok(GroupMessageListResult { messages, has_more })
}

pub(crate) fn pin_message(
    params: GroupMessagePinParams,
    subscribers: &EventHubHandle,
) -> Result<GroupMessagePinResult, ControlError> {
    let store = default_store()?;
    let updated = store
        .set_agent_group_message_pinned(&params.group_id, params.seq, params.pinned, now_ms())
        .map_err(map_store_error)?;
    if updated {
        notify_groups_changed(subscribers);
    }
    Ok(GroupMessagePinResult { updated })
}

pub(crate) fn update_task_status(
    params: GroupTaskUpdateParams,
    subscribers: &EventHubHandle,
) -> Result<GroupTaskUpdateResult, ControlError> {
    let store = default_store()?;
    let updated = store
        .update_agent_group_task_status(
            &params.group_id,
            params.seq,
            params.task_status.as_str(),
            now_ms(),
        )
        .map_err(map_store_error)?;
    if updated {
        notify_groups_changed(subscribers);
    }
    Ok(GroupTaskUpdateResult { updated })
}

fn normalize_tool_profile(raw: &str) -> String {
    match raw.trim() {
        "read_only" => "read_only".to_string(),
        _ => "default".to_string(),
    }
}
