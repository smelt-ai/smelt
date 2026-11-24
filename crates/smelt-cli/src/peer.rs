//! `smelt peer …`：供未接入 ACP `mcpServers` 的第三方 / 自定义智能体经终端兜底跨对话协作。
//!
//! 来源会话由 `smeltd` 按本进程的内核 PID 进程树认定，命令行不接受伪造来源参数。

use crate::{control, flag_value, id_arg, respond, usage};
use smelt_model::control_api::{
    PeerSessionList, PeerSessionRead, PeerSessionSend, PeerSessionSpawn,
};

pub(crate) fn run(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => match parse_list(rest) {
            Ok(body) => respond(call_peer_reply::<PeerSessionList>(body)),
            Err(message) => usage(&message),
        },
        Some("read") => match parse_read(rest) {
            Ok(body) => respond(call_peer_reply::<PeerSessionRead>(body)),
            Err(message) => usage(&message),
        },
        Some("send") => match parse_send(rest) {
            Ok(body) => respond(call_peer_reply::<PeerSessionSend>(body)),
            Err(message) => usage(&message),
        },
        Some("spawn") => match parse_spawn(rest) {
            Ok(body) => respond(call_peer_reply::<PeerSessionSpawn>(body)),
            Err(message) => usage(&message),
        },
        Some(other) => usage(&format!("unknown peer subcommand: {other}")),
        None => usage("smelt peer list|read|send|spawn"),
    }
}

fn call_peer_reply<C>(params: serde_json::Value) -> Result<serde_json::Value, String>
where
    C: smelt_runtime_client::protocol::control::ControlCall<
            Params = serde_json::Value,
            Result = serde_json::Value,
        >,
{
    let reply = control::<C>(params)?;
    if reply.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
        let error = reply
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("peer request rejected");
        return Err(error.to_string());
    }
    Ok(reply)
}

fn parse_list(args: &[String]) -> Result<serde_json::Value, String> {
    let mut query: Option<String> = None;
    let mut include_agents = true;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--query" => {
                query = Some(flag_value(args, index)?);
                index += 2;
            }
            "--no-agents" => {
                include_agents = false;
                index += 1;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let mut map = serde_json::Map::new();
    map.insert("includeAgents".into(), include_agents.into());
    if let Some(query) = query {
        map.insert("query".into(), query.into());
    }
    Ok(serde_json::Value::Object(map))
}

fn parse_read(args: &[String]) -> Result<serde_json::Value, String> {
    let Some(session_id) = id_arg(args) else {
        return Err("smelt peer read <session_id> [--limit <1..5>]".to_string());
    };
    let mut limit: Option<u64> = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--limit" => {
                let raw = flag_value(args, index)?;
                let parsed: u64 = raw
                    .parse()
                    .map_err(|_| format!("invalid --limit value: {raw}"))?;
                limit = Some(parsed);
                index += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let mut map = serde_json::Map::new();
    map.insert("sessionId".into(), session_id.into());
    if let Some(limit) = limit {
        map.insert("limit".into(), limit.into());
    }
    Ok(serde_json::Value::Object(map))
}

fn parse_send(args: &[String]) -> Result<serde_json::Value, String> {
    let mut to: Option<String> = None;
    let mut text: Option<String> = None;
    let mut kind: Option<String> = None;
    let mut reply_to: Option<String> = None;
    let mut tool_call_id: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--to" => {
                to = Some(flag_value(args, index)?);
                index += 2;
            }
            "--message" | "--text" => {
                text = Some(flag_value(args, index)?);
                index += 2;
            }
            "--kind" => {
                kind = Some(flag_value(args, index)?);
                index += 2;
            }
            "--reply-to" => {
                reply_to = Some(flag_value(args, index)?);
                index += 2;
            }
            "--idempotency-key" => {
                tool_call_id = Some(flag_value(args, index)?);
                index += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let to = to.ok_or_else(|| {
        "smelt peer send --to <session_id> --message <text> [--kind message|task|completion] [--reply-to <pm-id>]".to_string()
    })?;
    let text = text.ok_or_else(|| "missing --message".to_string())?;
    let mut map = serde_json::Map::new();
    map.insert("to".into(), to.into());
    map.insert("text".into(), text.into());
    map.insert(
        "toolCallId".into(),
        tool_call_id
            .unwrap_or_else(|| format!("cli-{}", uuid::Uuid::new_v4().simple()))
            .into(),
    );
    if let Some(kind) = kind {
        map.insert("kind".into(), kind.into());
    }
    if let Some(reply_to) = reply_to {
        map.insert("replyTo".into(), reply_to.into());
    }
    Ok(serde_json::Value::Object(map))
}

fn parse_spawn(args: &[String]) -> Result<serde_json::Value, String> {
    let mut title: Option<String> = None;
    let mut task: Option<String> = None;
    let mut agent: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut tool_call_id: Option<String> = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--title" => {
                title = Some(flag_value(args, index)?);
                index += 2;
            }
            "--task" | "--message" => {
                task = Some(flag_value(args, index)?);
                index += 2;
            }
            "--agent" => {
                agent = Some(flag_value(args, index)?);
                index += 2;
            }
            "--cwd" => {
                cwd = Some(flag_value(args, index)?);
                index += 2;
            }
            "--idempotency-key" => {
                tool_call_id = Some(flag_value(args, index)?);
                index += 2;
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let title = title.ok_or_else(|| {
        "smelt peer spawn --title <title> --task <task> [--agent <id>] [--cwd <dir>]".to_string()
    })?;
    let task = task.ok_or_else(|| "missing --task".to_string())?;
    let mut map = serde_json::Map::new();
    map.insert("title".into(), title.into());
    map.insert("task".into(), task.into());
    map.insert(
        "toolCallId".into(),
        tool_call_id
            .unwrap_or_else(|| format!("cli-{}", uuid::Uuid::new_v4().simple()))
            .into(),
    );
    if let Some(agent) = agent {
        map.insert("agent".into(), agent.into());
    }
    if let Some(cwd) = cwd {
        map.insert("cwd".into(), cwd.into());
    }
    Ok(serde_json::Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;

    #[test]
    fn parse_peer_subcommands_builds_expected_payloads() {
        let listed = parse_list(&args(&["--query", "codex", "--no-agents"])).unwrap();
        assert_eq!(listed["query"], "codex");
        assert_eq!(listed["includeAgents"], false);

        let read = parse_read(&args(&["acp-1", "--limit", "3"])).unwrap();
        assert_eq!(read["sessionId"], "acp-1");
        assert_eq!(read["limit"], 3);

        let sent = parse_send(&args(&[
            "--to",
            "acp-2",
            "--message",
            "好了",
            "--kind",
            "completion",
            "--reply-to",
            "pm-1",
            "--idempotency-key",
            "k1",
        ]))
        .unwrap();
        assert_eq!(sent["to"], "acp-2");
        assert_eq!(sent["text"], "好了");
        assert_eq!(sent["kind"], "completion");
        assert_eq!(sent["replyTo"], "pm-1");
        assert_eq!(sent["toolCallId"], "k1");

        let spawned = parse_spawn(&args(&[
            "--title",
            "Codex 写单测",
            "--task",
            "补齐单测",
            "--agent",
            "codex",
            "--cwd",
            "/work/app",
        ]))
        .unwrap();
        assert_eq!(spawned["title"], "Codex 写单测");
        assert_eq!(spawned["agent"], "codex");
        assert_eq!(spawned["cwd"], "/work/app");
        assert!(spawned["toolCallId"].as_str().unwrap().starts_with("cli-"));
    }
}
