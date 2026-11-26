use super::*;
// ============================================================================
// 群组与成员 CRUD
// ============================================================================

pub(crate) fn create_group(
    params: GroupCreateParams,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    workspace_directory: &WorkspaceDirectoryReader,
    subscribers: &EventHubHandle,
) -> Result<GroupDetailResult, ControlError> {
    let store = group_store()?;
    let title = params.title.trim();
    if title.is_empty() {
        return Err(err(
            ControlErrorCode::InvalidParams,
            "Group title can't be empty",
        ));
    }
    let now = now_ms();
    let group_id = short_id("grp");
    let group = GroupRecord {
        id: group_id.clone(),
        title: title.to_string(),
        description: params.description.trim().to_string(),
        workset_id: params.workset_id.filter(|s| !s.trim().is_empty()),
        mode: params.mode.as_str().to_string(),
        coordinator_member_id: None,
        allow_worker_cross_talk: true,
        max_chain_hops: 10,
        max_chain_depth: 6,
        max_pair_ping_pong: 3,
        auto_bridge_mentions: false,
        paused: false,
        next_seq: 1,
        archived_at_ms: None,
        created_at_ms: now,
        updated_at_ms: now,
    };
    store
        .create_group(&group)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;

    let mut coordinator_id: Option<String> = None;
    for spec in params.members {
        let member = add_member_on(
            &store,
            &group_id,
            spec,
            sessions,
            acp_sessions,
            workspace_directory,
            subscribers,
        )?;
        if member.role_kind == MemberRoleKind::Coordinator && coordinator_id.is_none() {
            coordinator_id = Some(member.id);
        }
    }
    if let Some(coord_id) = coordinator_id {
        let _ = store.update_group(
            &group_id,
            &GroupUpdatePatch {
                coordinator_member_id: Some(Some(coord_id)),
                updated_at_ms: now_ms(),
                ..Default::default()
            },
        );
    }
    notify_groups_changed(subscribers, acp_sessions);
    get_group_on(&store, &group_id, acp_sessions)
}

pub(crate) fn list_groups(
    params: GroupListParams,
    acp_sessions: &AcpSessions,
) -> Result<GroupListResult, ControlError> {
    let store = group_store()?;
    let groups = list_groups_on(&store, params.include_archived, acp_sessions)?;
    let group_owned_session_ids = store.list_group_owned_session_ids().unwrap_or_default();
    Ok(GroupListResult {
        groups,
        group_owned_session_ids,
    })
}

pub(super) fn list_groups_on(
    store: &GroupStore,
    include_archived: bool,
    acp_sessions: &AcpSessions,
) -> Result<Vec<GroupSummaryDto>, ControlError> {
    let records = store
        .list_groups(include_archived)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        let members = store
            .list_members(&record.id, false)
            .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
        let mut running_count = 0usize;
        let mut waiting_approval_count = 0usize;
        for m in &members {
            match member_live_status(m, acp_sessions) {
                GroupMemberLiveStatus::Running => running_count += 1,
                GroupMemberLiveStatus::WaitingApproval => waiting_approval_count += 1,
                _ => {}
            }
        }
        let pending_mentions = store
            .list_pending_mentions(Some(&record.id))
            .unwrap_or_default();
        let pending_mention_approval_count = pending_mentions
            .iter()
            .filter(|m| m.delivery_status == "pending_approval")
            .count();
        let last_msg = store.latest_message(&record.id).unwrap_or(None);
        out.push(GroupSummaryDto {
            group: group_to_dto(&record),
            member_count: members.len(),
            running_count,
            waiting_approval_count,
            pending_mention_approval_count,
            last_message_preview: last_msg.as_ref().map(|m| truncate_preview(&m.content, 120)),
            last_message_at_ms: last_msg.as_ref().map(|m| m.created_at_ms),
        });
    }
    Ok(out)
}

pub(crate) fn get_group(
    params: GroupIdParams,
    acp_sessions: &AcpSessions,
) -> Result<GroupDetailResult, ControlError> {
    let store = group_store()?;
    get_group_on(&store, &params.group_id, acp_sessions)
}

fn get_group_on(
    store: &GroupStore,
    group_id: &str,
    acp_sessions: &AcpSessions,
) -> Result<GroupDetailResult, ControlError> {
    let group = store
        .get_group(group_id)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| {
            err(
                ControlErrorCode::NotFound,
                format!("Group not found: {group_id}"),
            )
        })?;
    let members = store
        .list_members(group_id, false)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    let member_map: HashMap<String, &GroupMemberRecord> =
        members.iter().map(|m| (m.id.clone(), m)).collect();
    let member_dtos: Vec<AgentGroupMemberDto> = members
        .iter()
        .map(|m| member_to_dto(m, acp_sessions))
        .collect();
    let open_tasks = store
        .list_open_tasks(group_id)
        .unwrap_or_default()
        .into_iter()
        .map(|msg| message_to_dto(store, &msg, &member_map))
        .collect();
    let (all_msgs, _) = store
        .list_messages(
            group_id,
            &GroupMessageQuery {
                limit: 200,
                ..Default::default()
            },
        )
        .unwrap_or_default();
    let active_blind_prompts = all_msgs
        .into_iter()
        .filter(|m| m.kind == "blind_prompt" && m.blind_revealed_at_ms.is_none())
        .map(|m| message_to_dto(store, &m, &member_map))
        .collect();
    let leases = list_active_leases(group_id);
    Ok(GroupDetailResult {
        group: group_to_dto(&group),
        members: member_dtos,
        open_tasks,
        active_blind_prompts,
        leases,
    })
}

pub(crate) fn update_group(
    params: GroupUpdateParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupDetailResult, ControlError> {
    let store = group_store()?;
    let patch = GroupUpdatePatch {
        title: params.title.map(|s| s.trim().to_string()),
        description: params.description.map(|s| s.trim().to_string()),
        workset_id: params.workset_id,
        mode: params.mode.map(|m| m.as_str().to_string()),
        coordinator_member_id: params.coordinator_member_id,
        allow_worker_cross_talk: params.allow_worker_cross_talk,
        max_chain_hops: params.max_chain_hops,
        max_chain_depth: params.max_chain_depth,
        max_pair_ping_pong: params.max_pair_ping_pong,
        auto_bridge_mentions: params.auto_bridge_mentions,
        paused: None,
        updated_at_ms: now_ms(),
    };
    store
        .update_group(&params.group_id, &patch)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| {
            err(
                ControlErrorCode::NotFound,
                format!("Group not found: {}", params.group_id),
            )
        })?;
    notify_groups_changed(subscribers, acp_sessions);
    get_group_on(&store, &params.group_id, acp_sessions)
}

pub(crate) fn archive_group(
    params: GroupArchiveParams,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    automations: &crate::AutomationStore,
    subscribers: &EventHubHandle,
) -> Result<(), ControlError> {
    let store = group_store()?;
    let now = now_ms();
    let archived_at = params.archived.then_some(now);
    let updated = store
        .set_group_archived(&params.group_id, archived_at, now)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    if !updated {
        return Err(err(
            ControlErrorCode::NotFound,
            format!("Group not found: {}", params.group_id),
        ));
    }
    if params.archived {
        clear_group_runtime_state(&params.group_id);
        // 归属隔离联动：仅归档本机 `group_owned` 私有会话，`linked` 独立对话原样保留。
        if let Ok(members) = store.list_members(&params.group_id, false)
            && let Ok(main_store) = crate::workspace_directory::default_store()
            && let Ok(directory) = smelt_core::workspace_catalog::directory_on(&main_store)
        {
            for member in members {
                if member.runtime_id.is_none() && member.ownership == "group_owned" {
                    if let Some(conv) = directory
                        .conversations
                        .iter()
                        .find(|c| c.acp.as_ref().is_some_and(|a| a.sid == member.session_id))
                    {
                        let _ = crate::workspace_directory::archive_conversation(
                            &main_store,
                            &conv.id,
                            true,
                            sessions,
                            acp_sessions,
                            automations,
                            subscribers,
                        );
                    }
                }
            }
        }
    }
    notify_groups_changed(subscribers, acp_sessions);
    Ok(())
}

pub(crate) fn delete_group(
    params: GroupDeleteParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), ControlError> {
    let store = group_store()?;
    clear_group_runtime_state(&params.group_id);
    let deleted = store
        .delete_group(&params.group_id)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    if !deleted {
        return Err(err(
            ControlErrorCode::NotFound,
            format!("Group not found: {}", params.group_id),
        ));
    }
    notify_groups_changed(subscribers, acp_sessions);
    Ok(())
}

fn clear_group_runtime_state(group_id: &str) {
    let mut state = lock_state();
    state.chains.remove(group_id);
    state.pending_wakes.retain(|(gid, _), _| gid != group_id);
    state.leases.retain(|lease| lease.group_id != group_id);
    state
        .published_artifacts
        .retain(|_, entry| entry.group_id != group_id);
}

pub(crate) fn add_member(
    params: GroupMemberAddParams,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    workspace_directory: &WorkspaceDirectoryReader,
    subscribers: &EventHubHandle,
) -> Result<AgentGroupMemberDto, ControlError> {
    let store = group_store()?;
    let dto = add_member_on(
        &store,
        &params.group_id,
        params.member,
        sessions,
        acp_sessions,
        workspace_directory,
        subscribers,
    )?;
    notify_groups_changed(subscribers, acp_sessions);
    Ok(dto)
}

fn add_member_on(
    store: &GroupStore,
    group_id: &str,
    spec: GroupCreateMemberSpec,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    workspace_directory: &WorkspaceDirectoryReader,
    subscribers: &EventHubHandle,
) -> Result<AgentGroupMemberDto, ControlError> {
    let group = store
        .get_group(group_id)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| {
            err(
                ControlErrorCode::NotFound,
                format!("Group not found: {group_id}"),
            )
        })?;
    let alias =
        validate_member_alias(&spec.alias).map_err(|e| err(ControlErrorCode::InvalidParams, e))?;
    let runtime_id = spec.runtime_id.filter(|s| !s.trim().is_empty());

    // 解析或创建底层 session_id（本机新成员直接复用 session_control::create 创建会话）
    let (session_id, ownership) =
        if let Some(existing_sid) = spec.session_id.filter(|s| !s.trim().is_empty()) {
            (existing_sid, spec.ownership)
        } else if runtime_id.is_none() {
            let agent_option_id = spec.agent_option_id.filter(|s| !s.trim().is_empty());
            let create_params = SessionCreateParams {
                kind: Some(SessionCreateKind::Acp),
                agent_option_id,
                launch_key: None,
                project_root: String::new(),
                workset_id: spec.workset_id.or(group.workset_id),
                cwd: spec.cwd,
                resume_id: None,
                title: Some(format!("{} · @{}", group.title, alias)),
                disallowed_tools: if spec.tool_restriction_profile == "readonly" {
                    vec![
                        "Edit".into(),
                        "Write".into(),
                        "MultiEdit".into(),
                        "Bash(git push:*)".into(),
                    ]
                } else {
                    Vec::new()
                },
                request_id: Some(format!("grp-member-{group_id}-{alias}")),
            };
            let created = crate::session_control::create_session(
                create_params,
                sessions,
                acp_sessions,
                workspace_directory,
                subscribers,
            )?;
            (created.session_id, MemberOwnership::GroupOwned)
        } else {
            return Err(err(
                ControlErrorCode::InvalidParams,
                "Remote member must supply session_id (created on the remote node first)",
            ));
        };

    let now = now_ms();
    let display_name = spec
        .display_name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("@{alias}"));
    let record = GroupMemberRecord {
        id: short_id("gm"),
        group_id: group_id.to_string(),
        alias,
        display_name,
        ownership: ownership.as_str().to_string(),
        runtime_id,
        session_id,
        role_kind: spec.role_kind.as_str().to_string(),
        role_title: spec.role_title.trim().to_string(),
        role_prompt: spec.role_prompt.trim().to_string(),
        workspace_mode: spec.workspace_mode.as_str().to_string(),
        tool_restriction_profile: if spec.tool_restriction_profile.trim().is_empty() {
            "default".to_string()
        } else {
            spec.tool_restriction_profile.trim().to_string()
        },
        briefing_sent: false,
        last_delivered_seq: group.next_seq - 1,
        last_read_seq: group.next_seq - 1,
        muted: false,
        removed_at_ms: None,
        created_at_ms: now,
    };
    store
        .add_member(&record)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    Ok(member_to_dto(&record, acp_sessions))
}

pub(crate) fn update_member(
    params: GroupMemberUpdateParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<AgentGroupMemberDto, ControlError> {
    let store = group_store()?;
    let alias = match params.alias {
        Some(raw) => {
            Some(validate_member_alias(&raw).map_err(|e| err(ControlErrorCode::InvalidParams, e))?)
        }
        None => None,
    };
    let patch = GroupMemberUpdatePatch {
        alias,
        display_name: params.display_name,
        role_kind: params.role_kind.map(|r| r.as_str().to_string()),
        role_title: params.role_title,
        role_prompt: params.role_prompt,
        workspace_mode: params.workspace_mode.map(|w| w.as_str().to_string()),
        tool_restriction_profile: params.tool_restriction_profile,
        muted: params.muted,
    };
    let updated = store
        .update_member(&params.member_id, &patch)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| {
            err(
                ControlErrorCode::NotFound,
                format!("Group member not found: {}", params.member_id),
            )
        })?;
    notify_groups_changed(subscribers, acp_sessions);
    Ok(member_to_dto(&updated, acp_sessions))
}

pub(crate) fn remove_member(
    params: GroupMemberIdParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), ControlError> {
    let store = group_store()?;
    let removed = store
        .remove_member(&params.member_id, now_ms())
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?;
    if !removed {
        return Err(err(
            ControlErrorCode::NotFound,
            format!("Group member not found: {}", params.member_id),
        ));
    }
    lock_state()
        .pending_wakes
        .remove(&(params.group_id, params.member_id));
    notify_groups_changed(subscribers, acp_sessions);
    Ok(())
}

pub(crate) fn wake_member(
    params: GroupMemberIdParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<GroupMemberWakeResult, ControlError> {
    let store = group_store()?;
    let member = store
        .get_member(&params.member_id)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| {
            err(
                ControlErrorCode::NotFound,
                format!("Group member not found: {}", params.member_id),
            )
        })?;
    if member.runtime_id.is_none() {
        ensure_local_session_awake(&member.session_id, acp_sessions, subscribers)?;
    }
    Ok(GroupMemberWakeResult {
        session_id: member.session_id,
    })
}

// ============================================================================
// 暂停、恢复与任务状态更新
// ============================================================================

pub(crate) fn pause_group(
    params: GroupPauseParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), ControlError> {
    let store = group_store()?;
    store
        .update_group(
            &params.group_id,
            &GroupUpdatePatch {
                paused: Some(true),
                updated_at_ms: now_ms(),
                ..Default::default()
            },
        )
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| {
            err(
                ControlErrorCode::NotFound,
                format!("Group not found: {}", params.group_id),
            )
        })?;
    lock_state()
        .pending_wakes
        .retain(|(gid, _), _| gid != &params.group_id);
    if params.abort_running
        && let Ok(members) = store.list_members(&params.group_id, false)
    {
        for member in members {
            if member.runtime_id.is_none()
                && let Some(slot) = acp_sessions.get(&member.session_id)
            {
                let _ = acp_sessions.with_current(&member.session_id, &slot, |sess| {
                    crate::acp_host::apply_acp_user_action(sess, AcpUserAction::Cancel, subscribers)
                });
            }
        }
    }
    notify_groups_changed(subscribers, acp_sessions);
    Ok(())
}

pub(crate) fn resume_group(
    params: GroupResumeParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), ControlError> {
    let store = group_store()?;
    let group = store
        .update_group(
            &params.group_id,
            &GroupUpdatePatch {
                paused: Some(false),
                updated_at_ms: now_ms(),
                ..Default::default()
            },
        )
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| {
            err(
                ControlErrorCode::NotFound,
                format!("Group not found: {}", params.group_id),
            )
        })?;
    if let Some(extra) = params.extra_hops {
        let mut state = lock_state();
        let budget = state
            .chains
            .entry(group.id.clone())
            .or_insert_with(|| ChainBudget {
                chain_id: short_id("chn"),
                hops_left: 0,
                pair_counts: HashMap::new(),
                approved_targets: HashSet::new(),
            });
        budget.hops_left = budget.hops_left.saturating_add(extra);
        budget.pair_counts.clear();
    }
    resume_on_startup(acp_sessions, subscribers);
    notify_groups_changed(subscribers, acp_sessions);
    Ok(())
}

pub(crate) fn update_task_status(
    params: GroupTaskUpdateParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<AgentGroupMessageDto, ControlError> {
    let store = group_store()?;
    let updated = store
        .update_task_status(&params.group_id, params.seq, params.task_status.as_str())
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| {
            err(
                ControlErrorCode::NotFound,
                format!(
                    "Task message seq={} not found in group {}",
                    params.seq, params.group_id
                ),
            )
        })?;
    let members = store
        .list_members(&params.group_id, true)
        .unwrap_or_default();
    let member_map: HashMap<String, &GroupMemberRecord> =
        members.iter().map(|m| (m.id.clone(), m)).collect();
    let dto = message_to_dto(&store, &updated, &member_map);
    notify_groups_changed(subscribers, acp_sessions);
    Ok(dto)
}

pub(crate) fn pin_message(
    params: GroupMessagePinParams,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<AgentGroupMessageDto, ControlError> {
    let store = group_store()?;
    let updated = store
        .set_message_pinned(&params.group_id, params.seq, params.pinned)
        .map_err(|e| err(ControlErrorCode::OperationFailed, e.to_string()))?
        .ok_or_else(|| {
            err(
                ControlErrorCode::NotFound,
                format!(
                    "Message seq={} not found in group {}",
                    params.seq, params.group_id
                ),
            )
        })?;
    let members = store
        .list_members(&params.group_id, true)
        .unwrap_or_default();
    let member_map: HashMap<String, &GroupMemberRecord> =
        members.iter().map(|m| (m.id.clone(), m)).collect();
    let dto = message_to_dto(&store, &updated, &member_map);
    notify_message_appended(
        subscribers,
        json!({
            "event": "message_pinned",
            "group_id": params.group_id,
            "message": dto,
        }),
    );
    notify_groups_changed(subscribers, acp_sessions);
    Ok(dto)
}
