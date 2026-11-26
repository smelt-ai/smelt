//! `smelt group …`：同时服务两类调用方：
//! 1. 未接入 ACP `mcpServers` 的第三方 / 自定义智能体经终端兜底调用群工具（经 `group.agent_tool`
//!    并由 `smeltd` 按内核 PID 进程树认定来源会话）；
//! 2. 人类用户在终端/脚本里管理群组（`list` / `create` / `pause` / `resume` / `reveal`）。

use serde_json::{Value, json};
use smelt_model::agent_group::GroupMode;
use smelt_model::control_api::{
    GroupAgentTool, GroupBlindReveal, GroupBlindRevealParams, GroupCreate, GroupCreateParams,
    GroupList, GroupListParams, GroupPause, GroupPauseParams, GroupResume, GroupResumeParams,
};

use crate::{control, flag_value, respond, usage};

pub(crate) fn run(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => respond(control::<GroupList>(GroupListParams {
            include_archived: rest.iter().any(|a| a == "--all" || a == "--archived"),
        })),
        Some("create") => match parse_create(rest) {
            Ok(params) => respond(control::<GroupCreate>(params)),
            Err(message) => usage(&message),
        },
        Some("post") => match parse_agent_post(rest) {
            Ok(body) => respond(call_agent_tool(body)),
            Err(message) => usage(&message),
        },
        Some("yield") => match parse_agent_yield(rest) {
            Ok(body) => respond(call_agent_tool(body)),
            Err(message) => usage(&message),
        },
        Some("read") => match parse_agent_read(rest) {
            Ok(body) => respond(call_agent_tool(body)),
            Err(message) => usage(&message),
        },
        Some("members") => match parse_group_only("group_members", rest) {
            Ok(body) => respond(call_agent_tool(body)),
            Err(message) => usage(&message),
        },
        Some("blind-submit") => match parse_blind_submit(rest) {
            Ok(body) => respond(call_agent_tool(body)),
            Err(message) => usage(&message),
        },
        Some("task-update") => match parse_task_update(rest) {
            Ok(body) => respond(call_agent_tool(body)),
            Err(message) => usage(&message),
        },
        Some("reveal") => match parse_reveal(rest) {
            Ok(params) => respond(control::<GroupBlindReveal>(params)),
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
        Some(other) => usage(&format!("unknown group subcommand: {other}")),
        None => usage(
            "smelt group list|create|post|yield|read|members|blind-submit|task-update|reveal|pause|resume",
        ),
    }
}

fn call_agent_tool(params: Value) -> Result<Value, String> {
    let reply = control::<GroupAgentTool>(params)?;
    if reply.get("ok").and_then(Value::as_bool) == Some(false) {
        let error = reply
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("group tool request rejected");
        return Err(error.to_string());
    }
    Ok(reply)
}

fn parse_create(args: &[String]) -> Result<GroupCreateParams, String> {
    let mut title: Option<String> = None;
    let mut description = String::new();
    let mut mode = GroupMode::Mention;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--title" => {
                title = Some(flag_value(args, i)?);
                i += 2;
            }
            "--description" => {
                description = flag_value(args, i)?;
                i += 2;
            }
            "--mode" => {
                let raw = flag_value(args, i)?;
                mode = GroupMode::parse(&raw).ok_or_else(|| {
                    format!("invalid --mode `{raw}` (expected mention|coordinator)")
                })?;
                i += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let title = title.ok_or_else(|| {
        "smelt group create --title <title> [--mode mention|coordinator]".to_string()
    })?;
    Ok(GroupCreateParams {
        title,
        description,
        workset_id: None,
        mode,
        members: Vec::new(),
    })
}

fn parse_agent_post(args: &[String]) -> Result<Value, String> {
    let mut group_id: Option<String> = None;
    let mut content: Option<String> = None;
    let mut mentions: Vec<String> = Vec::new();
    let mut kind: Option<String> = None;
    let mut reply_to_seq: Option<i64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, i)?);
                i += 2;
            }
            "--message" | "--content" => {
                content = Some(flag_value(args, i)?);
                i += 2;
            }
            "--mentions" => {
                let raw = flag_value(args, i)?;
                mentions.extend(
                    raw.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(ToOwned::to_owned),
                );
                i += 2;
            }
            "--kind" => {
                kind = Some(flag_value(args, i)?);
                i += 2;
            }
            "--reply-to" => {
                let raw = flag_value(args, i)?;
                reply_to_seq = Some(
                    raw.parse()
                        .map_err(|_| format!("invalid --reply-to seq: {raw}"))?,
                );
                i += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let content = content.ok_or_else(|| {
        "smelt group post [--group <id>] --message <text> [--mentions a,b] [--kind chat|task|decision]".to_string()
    })?;
    let mut map = serde_json::Map::new();
    map.insert("op".into(), "group_post".into());
    map.insert("content".into(), content.into());
    map.insert("mentions".into(), json!(mentions));
    map.insert(
        "toolCallId".into(),
        format!("cli-{}", uuid::Uuid::new_v4().simple()).into(),
    );
    if let Some(gid) = group_id {
        map.insert("group_id".into(), gid.into());
    }
    if let Some(k) = kind {
        map.insert("kind".into(), k.into());
    }
    if let Some(r) = reply_to_seq {
        map.insert("reply_to_seq".into(), r.into());
    }
    Ok(Value::Object(map))
}

fn parse_agent_yield(args: &[String]) -> Result<Value, String> {
    let mut group_id: Option<String> = None;
    let mut reason: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, i)?);
                i += 2;
            }
            "--reason" => {
                reason = Some(flag_value(args, i)?);
                i += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let mut map = serde_json::Map::new();
    map.insert("op".into(), "group_yield".into());
    if let Some(gid) = group_id {
        map.insert("group_id".into(), gid.into());
    }
    if let Some(r) = reason {
        map.insert("reason".into(), r.into());
    }
    Ok(Value::Object(map))
}

fn parse_agent_read(args: &[String]) -> Result<Value, String> {
    let mut group_id: Option<String> = None;
    let mut after_seq: Option<i64> = None;
    let mut limit: Option<u64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, i)?);
                i += 2;
            }
            "--after" => {
                let raw = flag_value(args, i)?;
                after_seq = Some(raw.parse().map_err(|_| format!("invalid --after: {raw}"))?);
                i += 2;
            }
            "--limit" => {
                let raw = flag_value(args, i)?;
                limit = Some(raw.parse().map_err(|_| format!("invalid --limit: {raw}"))?);
                i += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let mut map = serde_json::Map::new();
    map.insert("op".into(), "group_read".into());
    if let Some(gid) = group_id {
        map.insert("group_id".into(), gid.into());
    }
    if let Some(a) = after_seq {
        map.insert("after_seq".into(), a.into());
    }
    if let Some(l) = limit {
        map.insert("limit".into(), l.into());
    }
    Ok(Value::Object(map))
}

fn parse_group_only(op: &str, args: &[String]) -> Result<Value, String> {
    let mut group_id: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, i)?);
                i += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let mut map = serde_json::Map::new();
    map.insert("op".into(), op.into());
    if let Some(gid) = group_id {
        map.insert("group_id".into(), gid.into());
    }
    Ok(Value::Object(map))
}

fn parse_blind_submit(args: &[String]) -> Result<Value, String> {
    let mut group_id: Option<String> = None;
    let mut prompt_seq: Option<i64> = None;
    let mut content: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, i)?);
                i += 2;
            }
            "--prompt-seq" => {
                let raw = flag_value(args, i)?;
                prompt_seq = Some(
                    raw.parse()
                        .map_err(|_| format!("invalid --prompt-seq: {raw}"))?,
                );
                i += 2;
            }
            "--message" | "--content" => {
                content = Some(flag_value(args, i)?);
                i += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let prompt_seq = prompt_seq.ok_or_else(|| "missing --prompt-seq".to_string())?;
    let content = content.ok_or_else(|| "missing --message".to_string())?;
    let mut map = serde_json::Map::new();
    map.insert("op".into(), "group_blind_submit".into());
    map.insert("prompt_seq".into(), prompt_seq.into());
    map.insert("content".into(), content.into());
    if let Some(gid) = group_id {
        map.insert("group_id".into(), gid.into());
    }
    Ok(Value::Object(map))
}

fn parse_task_update(args: &[String]) -> Result<Value, String> {
    let mut group_id: Option<String> = None;
    let mut seq: Option<i64> = None;
    let mut task_status: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, i)?);
                i += 2;
            }
            "--seq" => {
                let raw = flag_value(args, i)?;
                seq = Some(raw.parse().map_err(|_| format!("invalid --seq: {raw}"))?);
                i += 2;
            }
            "--status" => {
                task_status = Some(flag_value(args, i)?);
                i += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let seq = seq.ok_or_else(|| "missing --seq".to_string())?;
    let task_status = task_status.ok_or_else(|| "missing --status".to_string())?;
    let mut map = serde_json::Map::new();
    map.insert("op".into(), "group_task_update".into());
    map.insert("seq".into(), seq.into());
    map.insert("task_status".into(), task_status.into());
    if let Some(gid) = group_id {
        map.insert("group_id".into(), gid.into());
    }
    Ok(Value::Object(map))
}

fn parse_reveal(args: &[String]) -> Result<GroupBlindRevealParams, String> {
    let mut group_id: Option<String> = None;
    let mut prompt_seq: Option<i64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, i)?);
                i += 2;
            }
            "--prompt-seq" => {
                let raw = flag_value(args, i)?;
                prompt_seq = Some(
                    raw.parse()
                        .map_err(|_| format!("invalid --prompt-seq: {raw}"))?,
                );
                i += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(GroupBlindRevealParams {
        group_id: group_id.ok_or_else(|| "missing --group".to_string())?,
        prompt_seq: prompt_seq.ok_or_else(|| "missing --prompt-seq".to_string())?,
    })
}

fn parse_pause(args: &[String]) -> Result<GroupPauseParams, String> {
    let mut group_id: Option<String> = None;
    let mut abort_running = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, i)?);
                i += 2;
            }
            "--abort" => {
                abort_running = true;
                i += 1;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(GroupPauseParams {
        group_id: group_id.ok_or_else(|| "missing --group".to_string())?,
        abort_running,
    })
}

fn parse_resume(args: &[String]) -> Result<GroupResumeParams, String> {
    let mut group_id: Option<String> = None;
    let mut extra_hops: Option<u32> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--group" => {
                group_id = Some(flag_value(args, i)?);
                i += 2;
            }
            "--extra-hops" => {
                let raw = flag_value(args, i)?;
                extra_hops = Some(
                    raw.parse()
                        .map_err(|_| format!("invalid --extra-hops: {raw}"))?,
                );
                i += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(GroupResumeParams {
        group_id: group_id.ok_or_else(|| "missing --group".to_string())?,
        extra_hops,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|v| (*v).to_string()).collect()
    }

    #[test]
    fn parses_create_post_and_blind_submit_flags() {
        let created =
            parse_create(&s(&["--title", "OAuth 评审", "--mode", "coordinator"])).unwrap();
        assert_eq!(created.title, "OAuth 评审");
        assert_eq!(created.mode, GroupMode::Coordinator);

        let posted = parse_agent_post(&s(&[
            "--group",
            "grp-1",
            "--message",
            "@codex 检查单测",
            "--mentions",
            "codex,claude",
            "--kind",
            "task",
            "--reply-to",
            "12",
        ]))
        .unwrap();
        assert_eq!(posted["op"], "group_post");
        assert_eq!(posted["group_id"], "grp-1");
        assert_eq!(posted["mentions"], json!(["codex", "claude"]));
        assert_eq!(posted["kind"], "task");
        assert_eq!(posted["reply_to_seq"], 12);

        let blind = parse_blind_submit(&s(&[
            "--group",
            "grp-1",
            "--prompt-seq",
            "7",
            "--message",
            "独立结论",
        ]))
        .unwrap();
        assert_eq!(blind["op"], "group_blind_submit");
        assert_eq!(blind["prompt_seq"], 7);
        assert_eq!(blind["content"], "独立结论");
    }
}
