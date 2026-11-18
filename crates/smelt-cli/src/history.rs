//! `smelt history …`：各家 agent 存在本机的历史会话，按项目目录查。
//!
//! 历史是 agent 自己的存档（Claude 的 jsonl 等），运行时只读、只按用户的意思删。`--agent`
//! 是历史来源的 id（`claude`、`codex`、`pi`、`antigravity` 等），`--cwd` 不给就是当前目录。
//! 想接着某段历史聊，用 `resume_id` 在桌面上续接。

use crate::{absolute, control, flag_value, id_arg, respond, usage};
use rust_i18n::t;
use smelt_model::agent_kind::HistorySourceKind;
use smelt_runtime_client::protocol::history::{
    HistoryDeleteCall, HistoryDeleteParams, HistoryDetailCall, HistoryDetailParams,
    HistoryListCall, HistoryListParams,
};

pub(crate) fn run(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    let verb = args.first().map(String::as_str);
    let parsed = match verb {
        Some("list" | "get" | "delete") => parse(rest),
        Some(command) => return usage(&t!("unknown_subcommand", command = command)),
        None => return usage(&t!("history.usage")),
    };
    let query = match parsed {
        Ok(query) => query,
        Err(message) => return usage(&message),
    };
    match (verb, query.target.as_deref()) {
        (Some("list"), None) => respond(list(&query)),
        (Some("get"), Some(path)) => respond(control::<HistoryDetailCall>(HistoryDetailParams {
            agent: query.agent,
            path: path.to_string(),
        })),
        (Some("delete"), Some(resume_id)) => respond(delete(&query, resume_id)),
        _ => usage(&t!("history.usage")),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Query {
    agent: String,
    profile_id: Option<String>,
    cwd: String,
    /// `get` 的存档路径、`delete` 的 resume_id。
    target: Option<String>,
}

fn parse(args: &[String]) -> Result<Query, String> {
    let mut agent = None;
    let mut profile_id = None;
    let mut cwd = None;
    let mut target = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--agent" => agent = Some(flag_value(args, index)?),
            "--profile" => profile_id = Some(flag_value(args, index)?),
            "--cwd" => cwd = Some(flag_value(args, index)?),
            _ if target.is_none() => {
                target = id_arg(&args[index..]).map(str::to_string);
                if target.is_none() {
                    return Err(t!("unknown_argument", arg = args[index]).into_owned());
                }
                index += 1;
                continue;
            }
            other => return Err(t!("extra_argument", arg = other).into_owned()),
        }
        index += 2;
    }
    let agent = agent.ok_or_else(|| t!("history.usage").into_owned())?;
    if HistorySourceKind::from_id(&agent).is_none() {
        let known: Vec<&str> = HistorySourceKind::ALL
            .iter()
            .map(|kind| kind.id())
            .collect();
        return Err(t!(
            "history.unknown_agent",
            agent = agent,
            known = known.join(", ")
        )
        .into_owned());
    }
    let cwd = match cwd {
        Some(cwd) => absolute(&cwd)?,
        None => std::env::current_dir()
            .map_err(|error| error.to_string())
            .and_then(|dir| absolute(&dir.to_string_lossy()))?,
    };
    Ok(Query {
        agent,
        profile_id,
        cwd,
        target,
    })
}

fn list_params(query: &Query) -> HistoryListParams {
    HistoryListParams {
        agent: query.agent.clone(),
        profile_id: query.profile_id.clone(),
        cwd: query.cwd.clone(),
    }
}

fn list(query: &Query) -> Result<serde_json::Value, String> {
    let result = control::<HistoryListCall>(list_params(query))?;
    Ok(
        serde_json::json!({ "cwd": query.cwd, "sessions": result.sessions, "agent_definition_ids": result.agent_definition_ids }),
    )
}

/// 运行时只删这个项目的历史列表里确实有的那一份，存档路径从列表里取。
fn delete(query: &Query, resume_id: &str) -> Result<serde_json::Value, String> {
    let sessions = control::<HistoryListCall>(list_params(query))?.sessions;
    let session = sessions
        .iter()
        .find(|session| session.resume_id == resume_id)
        .ok_or_else(|| t!("history.not_found", id = resume_id, cwd = query.cwd).into_owned())?;
    control::<HistoryDeleteCall>(HistoryDeleteParams {
        agent: query.agent.clone(),
        profile_id: query.profile_id.clone(),
        cwd: query.cwd.clone(),
        resume_id: session.resume_id.clone(),
        path: session.path.clone(),
    })?;
    Ok(serde_json::json!({ "resume_id": resume_id }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;

    #[test]
    fn parse_reads_flags_and_one_target() {
        let query = parse(&args(&["--agent", "claude", "--cwd", "/", "abc"])).unwrap();
        assert_eq!(
            query,
            Query {
                agent: "claude".into(),
                profile_id: None,
                // Windows 上是当前盘的根，如 `D:\`。
                cwd: crate::absolute("/").unwrap(),
                target: Some("abc".into()),
            }
        );
        assert!(
            !query.cwd.starts_with(r"\\?\"),
            "不能是 Windows 的 verbatim 路径：{}",
            query.cwd
        );
        // 位置参数在前也行。
        let query = parse(&args(&["abc", "--agent", "pi", "--cwd", "/"])).unwrap();
        assert_eq!(query.target.as_deref(), Some("abc"));
        assert!(parse(&args(&["--cwd", "/"])).is_err());
        assert!(parse(&args(&["--agent", "nope", "--cwd", "/"])).is_err());
        assert!(parse(&args(&["--agent", "claude", "--cwd", "/", "a", "b"])).is_err());
        assert!(parse(&args(&["--agent", "claude", "--bogus"])).is_err());
    }
}
