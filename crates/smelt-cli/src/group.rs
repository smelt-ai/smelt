//! `smelt group …`：命令行管理本地智能体群组，以及智能体终端回退路径调用群组工具。
//!
//! - 人类管理命令：`list` / `create` / `pause` / `resume`（经 `group.*` Control API，守护校验调用方非智能体进程树）。
//! - 智能体工具回退：`post` / `yield` / `read` / `members` / `task-update`（经 `group.agent.tool`，来源会话由守护按进程树认定）。

use crate::{control, flag_value, respond, usage};
use smelt_model::agent_group::{GroupMode, GroupTaskStatus};
use smelt_model::control_api::{
    GroupAgentTool, GroupAgentToolParams, GroupCreate, GroupCreateParams, GroupList,
    GroupListParams, GroupPause, GroupPauseParams, GroupResume, GroupResumeParams,
};

pub(crate) fn run(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => match parse_list(rest) {
            Ok(params) => respond(control::<GroupList>(params)),
            Err(message) => usage(&message),
        },
        Some("create") => match parse_create(rest) {
            Ok(params) => respond(control::<GroupCreate>(params)),
            Err(message) => usage(&message),
        },
        Some("pause") => match parse_pause(rest) {
            Ok(params) => respond(control::<GroupPause>(params)),
            Err(message) => usage(&message),
        },
        Some("resume") => match parse_resume(rest) {
            Ok(params) => respond(control::<GroupResume>(params)),
            Err(message) => usage(&message),
        },
        Some("post") => match parse_agent_post(rest) {
            Ok(params) => respond(call_group_tool(params)),
            Err(message) => usage(&message),
        },
        Some("yield") => match parse_agent_yield(rest) {
            Ok(params) => respond(call_group_tool(params)),
            Err(message) => usage(&message),
        },
        Some("read") => match parse_agent_read(rest) {
            Ok(params) => respond(call_group_tool(params)),
            Err(message) => usage(&message),
        },
        Some("members") => match parse_agent_members(rest) {
            Ok(params) => respond(call_group_tool(params)),
            Err(message) => usage(&message),
        },
        Some("task-update") => match parse_agent_task_update(rest) {
            Ok(params) => respond(call_group_tool(params)),
            Err(message) => usage(&message),
        },
        Some(other) => usage(&format!("unknown group subcommand: {other}")),
        None => usage("smelt group list|create|pause|resume|post|yield|read|members|task-update"),
    }
}

fn call_group_tool(params: GroupAgentToolParams) -> Result<serde_json::Value, String> {
    let reply = control::<GroupAgentTool>(params)?;
    if reply.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
        let error = reply
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("group request rejected");
        return Err(error.to_string());
    }
    Ok(reply)
}

fn parse_list(args: &[String]) -> Result<GroupListParams, String> {
    let mut include_archived = false;
    for arg in args {
        match arg.as_str() {
            "--all" | "--include-archived" => include_archived = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(GroupListParams { include_archived })
}

fn parse_create(args: &[String]) -> Result<GroupCreateParams, String> {
    let mut title: Option<String> = None;
    let mut description = String::new();
    let mut project_root: Option<String> = None;
    let mut mode = GroupMode::Mention;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--title" => {
                title = Some(flag_value(args, index)?);
                index += 2;
            }
            "--description" => {
                description = flag_value(args, index)?;
                index += 2;
            }
            "--project" => {
                project_root = Some(flag_value(args, index)?);
                index += 2;
            }
            "--mode" => {
                let raw = flag_value(args, index)?;
                mode = GroupMode::parse(&raw)
                    .ok_or_else(|| format!("invalid --mode '{raw}' (use mention|coordinator)"))?;
                index += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let title = title.ok_or_else(|| {
        "smelt group create --title <title> [--description <text>] [--project <dir>] [--mode mention|coordinator]".to_string()
    })?;
    Ok(GroupCreateParams {
        title,
        description,
        project_root,
        workset_id: None,
        mode,
        allow_worker_cross_talk: true,
        max_chain_hops: 10,
        max_chain_depth: 6,
        max_pair_ping_pong: 3,
        auto_bridge_mentions: false,
    })
}

fn parse_pause(args: &[String]) -> Result<GroupPauseParams, String> {
    let mut group_id: Option<String> = None;
    let mut abort_running = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, index)?);
                index += 2;
            }
            "--abort-running" => {
                abort_running = true;
                index += 1;
            }
            other if !other.starts_with('-') && group_id.is_none() => {
                group_id = Some(other.to_string());
                index += 1;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let group_id =
        group_id.ok_or_else(|| "smelt group pause <group_id> [--abort-running]".to_string())?;
    Ok(GroupPauseParams {
        group_id,
        abort_running,
    })
}

fn parse_resume(args: &[String]) -> Result<GroupResumeParams, String> {
    let mut group_id: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, index)?);
                index += 2;
            }
            other if !other.starts_with('-') && group_id.is_none() => {
                group_id = Some(other.to_string());
                index += 1;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let group_id = group_id.ok_or_else(|| "smelt group resume <group_id>".to_string())?;
    Ok(GroupResumeParams { group_id })
}

fn parse_agent_post(args: &[String]) -> Result<GroupAgentToolParams, String> {
    let mut group_id: Option<String> = None;
    let mut content: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut reply_to: Option<i64> = None;
    let mut task_status: Option<String> = None;
    let mut idempotency_key: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, index)?);
                index += 2;
            }
            "--message" | "--text" | "--content" => {
                content = Some(flag_value(args, index)?);
                index += 2;
            }
            "--kind" => {
                kind = Some(flag_value(args, index)?);
                index += 2;
            }
            "--reply-to" => {
                let raw = flag_value(args, index)?;
                reply_to = Some(
                    raw.parse::<i64>()
                        .map_err(|_| format!("invalid --reply-to value: {raw}"))?,
                );
                index += 2;
            }
            "--task-status" => {
                task_status = Some(flag_value(args, index)?);
                index += 2;
            }
            "--idempotency-key" => {
                idempotency_key = Some(flag_value(args, index)?);
                index += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let group_id = group_id.ok_or_else(|| {
        "smelt group post --group <group_id> --message <text> [--kind chat|task|decision] [--reply-to <seq>]".to_string()
    })?;
    let content = content.ok_or_else(|| "missing --message".to_string())?;
    let mut map = serde_json::Map::new();
    map.insert("group_id".into(), group_id.into());
    map.insert("content".into(), content.into());
    if let Some(kind) = kind {
        map.insert("kind".into(), kind.into());
    }
    if let Some(seq) = reply_to {
        map.insert("reply_to_seq".into(), seq.into());
    }
    if let Some(status) = task_status {
        map.insert("task_status".into(), status.into());
    }
    if let Some(key) = idempotency_key {
        map.insert("idempotency_key".into(), key.into());
    }
    Ok(GroupAgentToolParams {
        op: "group_post".into(),
        args: serde_json::Value::Object(map),
    })
}

fn parse_agent_yield(args: &[String]) -> Result<GroupAgentToolParams, String> {
    let mut group_id: Option<String> = None;
    let mut up_to_seq: Option<i64> = None;
    let mut reason: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, index)?);
                index += 2;
            }
            "--up-to" => {
                let raw = flag_value(args, index)?;
                up_to_seq = Some(
                    raw.parse::<i64>()
                        .map_err(|_| format!("invalid --up-to value: {raw}"))?,
                );
                index += 2;
            }
            "--reason" => {
                reason = Some(flag_value(args, index)?);
                index += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let group_id = group_id.ok_or_else(|| {
        "smelt group yield --group <group_id> [--up-to <seq>] [--reason <text>]".to_string()
    })?;
    let mut map = serde_json::Map::new();
    map.insert("group_id".into(), group_id.into());
    if let Some(seq) = up_to_seq {
        map.insert("up_to_seq".into(), seq.into());
    }
    if let Some(reason) = reason {
        map.insert("reason".into(), reason.into());
    }
    Ok(GroupAgentToolParams {
        op: "group_yield".into(),
        args: serde_json::Value::Object(map),
    })
}

fn parse_agent_read(args: &[String]) -> Result<GroupAgentToolParams, String> {
    let mut group_id: Option<String> = None;
    let mut since_seq: Option<i64> = None;
    let mut before_seq: Option<i64> = None;
    let mut limit: Option<u64> = None;
    let mut pinned_only = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, index)?);
                index += 2;
            }
            "--since" => {
                let raw = flag_value(args, index)?;
                since_seq = Some(
                    raw.parse::<i64>()
                        .map_err(|_| format!("invalid --since value: {raw}"))?,
                );
                index += 2;
            }
            "--before" => {
                let raw = flag_value(args, index)?;
                before_seq = Some(
                    raw.parse::<i64>()
                        .map_err(|_| format!("invalid --before value: {raw}"))?,
                );
                index += 2;
            }
            "--limit" => {
                let raw = flag_value(args, index)?;
                limit = Some(
                    raw.parse::<u64>()
                        .map_err(|_| format!("invalid --limit value: {raw}"))?,
                );
                index += 2;
            }
            "--pinned" => {
                pinned_only = true;
                index += 1;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let group_id = group_id.ok_or_else(|| {
        "smelt group read --group <group_id> [--since <seq>] [--before <seq>] [--limit <n>] [--pinned]"
            .to_string()
    })?;
    let mut map = serde_json::Map::new();
    map.insert("group_id".into(), group_id.into());
    if let Some(s) = since_seq {
        map.insert("since_seq".into(), s.into());
    }
    if let Some(b) = before_seq {
        map.insert("before_seq".into(), b.into());
    }
    if let Some(l) = limit {
        map.insert("limit".into(), l.into());
    }
    if pinned_only {
        map.insert("pinned_only".into(), true.into());
    }
    Ok(GroupAgentToolParams {
        op: "group_read".into(),
        args: serde_json::Value::Object(map),
    })
}

fn parse_agent_members(args: &[String]) -> Result<GroupAgentToolParams, String> {
    let mut group_id: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, index)?);
                index += 2;
            }
            other if !other.starts_with('-') && group_id.is_none() => {
                group_id = Some(other.to_string());
                index += 1;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let group_id = group_id.ok_or_else(|| "smelt group members --group <group_id>".to_string())?;
    Ok(GroupAgentToolParams {
        op: "group_members".into(),
        args: serde_json::json!({ "group_id": group_id }),
    })
}

fn parse_agent_task_update(args: &[String]) -> Result<GroupAgentToolParams, String> {
    let mut group_id: Option<String> = None;
    let mut seq: Option<i64> = None;
    let mut status: Option<GroupTaskStatus> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, index)?);
                index += 2;
            }
            "--seq" => {
                let raw = flag_value(args, index)?;
                seq = Some(
                    raw.parse::<i64>()
                        .map_err(|_| format!("invalid --seq value: {raw}"))?,
                );
                index += 2;
            }
            "--status" => {
                let raw = flag_value(args, index)?;
                status = Some(GroupTaskStatus::parse(&raw).ok_or_else(|| {
                    format!("invalid --status '{raw}' (use open|in_progress|done|blocked)")
                })?);
                index += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let group_id = group_id.ok_or_else(|| {
        "smelt group task-update --group <group_id> --seq <n> --status open|in_progress|done|blocked"
            .to_string()
    })?;
    let seq = seq.ok_or_else(|| "missing --seq".to_string())?;
    let status = status.ok_or_else(|| "missing --status".to_string())?;
    Ok(GroupAgentToolParams {
        op: "group_task_update".into(),
        args: serde_json::json!({
            "group_id": group_id,
            "seq": seq,
            "task_status": status.as_str(),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;

    #[test]
    fn parses_group_cli_commands() {
        let created = parse_create(&args(&[
            "--title",
            "API Review",
            "--mode",
            "coordinator",
            "--project",
            "/repo",
        ]))
        .unwrap();
        assert_eq!(created.title, "API Review");
        assert_eq!(created.mode, GroupMode::Coordinator);
        assert_eq!(created.project_root.as_deref(), Some("/repo"));

        let posted = parse_agent_post(&args(&[
            "--group",
            "grp-1",
            "--message",
            "@coder ready",
            "--reply-to",
            "4",
            "--kind",
            "decision",
        ]))
        .unwrap();
        assert_eq!(posted.op, "group_post");
        assert_eq!(posted.args["group_id"], "grp-1");
        assert_eq!(posted.args["reply_to_seq"], 4);
        assert_eq!(posted.args["kind"], "decision");

        let yielded = parse_agent_yield(&args(&["--group", "grp-1", "--up-to", "5"])).unwrap();
        assert_eq!(yielded.op, "group_yield");
        assert_eq!(yielded.args["up_to_seq"], 5);

        let updated = parse_agent_task_update(&args(&[
            "--group", "grp-1", "--seq", "2", "--status", "done",
        ]))
        .unwrap();
        assert_eq!(updated.op, "group_task_update");
        assert_eq!(updated.args["task_status"], "done");
    }
}
