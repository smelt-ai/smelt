//! 智能体侧群组工具分发（`group_post` / `group_yield` / `group_read` / `group_members` /
//! `group_task_update`）。
//!
//! 调用方身份 `caller_session_id` 永远由 `smeltd` 按套接字绑定或内核 `peer_pid` 进程树认定，
//! 工具入参不能指定或伪造 `sender_member_id`；且调用方只能访问自己作为活跃成员所属的群组。

use serde_json::{Value, json};
use smelt_core::agent_group::{GroupMessageKind, GroupTaskStatus};
use smelt_store::{AgentGroupMemberRecord, AgentGroupMessageQuery, Store};

use super::{
    default_store, hydrate_messages, lock_state, member_to_dto, notify_groups_changed, now_ms,
    routing,
};
use crate::{AcpSessions, EventHubHandle, WorkspaceDirectoryReader};

pub(crate) fn dispatch_agent_group_tool(
    op: &str,
    caller_session_id: &str,
    body: &Value,
    acp_sessions: &AcpSessions,
    workspace_directory: &WorkspaceDirectoryReader,
    subscribers: &EventHubHandle,
) -> Value {
    let store = match default_store() {
        Ok(store) => store,
        Err(error) => return err_reply("unavailable", error.message),
    };
    dispatch_agent_group_tool_with_store(
        &store,
        op,
        caller_session_id,
        body,
        acp_sessions,
        workspace_directory,
        subscribers,
    )
}

pub(crate) fn dispatch_agent_group_tool_with_store(
    store: &Store,
    op: &str,
    caller_session_id: &str,
    body: &Value,
    acp_sessions: &AcpSessions,
    workspace_directory: &WorkspaceDirectoryReader,
    subscribers: &EventHubHandle,
) -> Value {
    let Some(group_id) = body
        .get("group_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return err_reply("invalid_params", "group_id is required");
    };

    let member = match resolve_caller_membership(store, group_id, caller_session_id) {
        Ok(member) => member,
        Err(reply) => return reply,
    };

    match op {
        "group_post" => handle_post(store, &member, body, acp_sessions, subscribers),
        "group_yield" => handle_yield(store, &member, body),
        "group_read" => handle_read(store, &member, body),
        "group_members" => handle_members(store, &member, workspace_directory, acp_sessions),
        "group_task_update" => handle_task_update(store, &member, body, subscribers),
        _ => err_reply("invalid", format!("Unknown group tool: {op}")),
    }
}

fn resolve_caller_membership(
    store: &Store,
    group_id: &str,
    caller_session_id: &str,
) -> Result<AgentGroupMemberRecord, Value> {
    let memberships = store
        .find_active_agent_group_members_by_session(caller_session_id)
        .map_err(|error| err_reply("internal", error.to_string()))?;
    memberships
        .into_iter()
        .find(|m| m.group_id == group_id)
        .ok_or_else(|| {
            err_reply(
                "permission_denied",
                format!("This conversation is not an active member of group {group_id}"),
            )
        })
}

fn handle_post(
    store: &Store,
    member: &AgentGroupMemberRecord,
    body: &Value,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Value {
    let Some(content) = body
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return err_reply("invalid_params", "content is required");
    };
    let kind = body
        .get("kind")
        .and_then(Value::as_str)
        .and_then(GroupMessageKind::parse)
        .unwrap_or(GroupMessageKind::Chat);
    if kind == GroupMessageKind::System {
        return err_reply("permission_denied", "Members cannot post system messages");
    }
    let reply_to_seq = body.get("reply_to_seq").and_then(Value::as_i64);
    let task_status = body
        .get("task_status")
        .and_then(Value::as_str)
        .and_then(GroupTaskStatus::parse);
    let idempotency_key = body
        .get("idempotency_key")
        .or_else(|| body.get("toolCallId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    match routing::post_member_message_with_store(
        store,
        &member.group_id,
        member,
        content.to_string(),
        kind,
        reply_to_seq,
        task_status,
        idempotency_key,
        false,
        acp_sessions,
        subscribers,
    ) {
        Ok(view) => json!({
            "ok": true,
            "group_id": view.group_id,
            "message_id": view.id,
            "seq": view.seq,
            "mentions": view.mentions,
        }),
        Err(error) => err_reply("rejected", error.message),
    }
}

fn handle_yield(store: &Store, member: &AgentGroupMemberRecord, body: &Value) -> Value {
    let target_seq = body
        .get("up_to_seq")
        .and_then(Value::as_i64)
        .or_else(|| {
            store
                .get_agent_group(&member.group_id)
                .ok()
                .flatten()
                .map(|g| (g.next_seq - 1).max(0))
        })
        .unwrap_or(member.last_read_seq);
    if let Err(error) = store.advance_agent_group_member_read_seq(&member.id, target_seq) {
        return err_reply("internal", error.to_string());
    }
    lock_state()
        .member_turn_acted
        .insert((member.group_id.clone(), member.session_id.clone()));
    json!({
        "ok": true,
        "group_id": member.group_id,
        "last_read_seq": target_seq.max(member.last_read_seq),
    })
}

fn handle_read(store: &Store, member: &AgentGroupMemberRecord, body: &Value) -> Value {
    let since_seq = body.get("since_seq").and_then(Value::as_i64);
    let before_seq = body.get("before_seq").and_then(Value::as_i64);
    let limit = body
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .clamp(1, 100) as usize;
    let pinned_only = body
        .get("pinned_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let records = match store.list_agent_group_messages(&AgentGroupMessageQuery {
        group_id: member.group_id.clone(),
        since_seq,
        before_seq,
        limit,
        pinned_only,
    }) {
        Ok(records) => records,
        Err(error) => return err_reply("internal", error.to_string()),
    };
    let members = match store.list_agent_group_members(&member.group_id, true) {
        Ok(members) => members,
        Err(error) => return err_reply("internal", error.to_string()),
    };
    if let Some(last) = records.last() {
        let _ = store.advance_agent_group_member_read_seq(&member.id, last.seq);
    }
    let messages = match hydrate_messages(store, records, &members) {
        Ok(messages) => messages,
        Err(error) => return err_reply("internal", error.to_string()),
    };
    json!({
        "ok": true,
        "group_id": member.group_id,
        "messages": messages,
    })
}

fn handle_members(
    store: &Store,
    member: &AgentGroupMemberRecord,
    workspace_directory: &WorkspaceDirectoryReader,
    acp_sessions: &AcpSessions,
) -> Value {
    let members = match store.list_agent_group_members(&member.group_id, false) {
        Ok(members) => members,
        Err(error) => return err_reply("internal", error.to_string()),
    };
    let directory = workspace_directory.read().ok();
    let views: Vec<_> = members
        .iter()
        .map(|m| member_to_dto(m, directory.as_ref(), acp_sessions))
        .collect();
    json!({
        "ok": true,
        "group_id": member.group_id,
        "members": views,
    })
}

fn handle_task_update(
    store: &Store,
    member: &AgentGroupMemberRecord,
    body: &Value,
    subscribers: &EventHubHandle,
) -> Value {
    let Some(seq) = body.get("seq").and_then(Value::as_i64) else {
        return err_reply("invalid_params", "seq is required");
    };
    let Some(status) = body
        .get("task_status")
        .and_then(Value::as_str)
        .and_then(GroupTaskStatus::parse)
    else {
        return err_reply(
            "invalid_params",
            "task_status must be one of open, in_progress, done, blocked",
        );
    };
    match store.update_agent_group_task_status(&member.group_id, seq, status.as_str(), now_ms()) {
        Ok(true) => {
            notify_groups_changed(subscribers);
            json!({
                "ok": true,
                "group_id": member.group_id,
                "seq": seq,
                "task_status": status.as_str(),
            })
        }
        Ok(false) => err_reply("not_found", format!("Task message #{seq} not found")),
        Err(error) => err_reply("internal", error.to_string()),
    }
}

fn err_reply(reason: &str, error: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "reason": reason,
        "error": error.into(),
    })
}
