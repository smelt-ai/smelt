//! `smelt conversation …`：侧栏里的对话。
//!
//! 对话有两个 id：工作区目录里的对话 id，和智能体对话的会话 id（`agent chat` 返回的
//! `session_id`）。这里两个都认，按需要换成对应的那个。
//!
//! `send` 把一句话作为一次外部投递发进对话（正在跑就排在这一轮后面），立刻返回投递 id；
//! 这一轮的结果之后用 `result` 按 id 取，命令行不替调用方守着等。

use crate::{control, control_within, flag_value, id_arg, respond, usage};
use rust_i18n::t;
use smelt_model::control_api::{
    ConversationPrompt, ConversationPromptParams, ConversationTurnResult,
    ConversationTurnResultParams, WorkspaceConversationArchive, WorkspaceConversationDelete,
    WorkspaceDirectoryGet,
};
use smelt_model::workspace_directory::{
    WorkspaceConversation, WorkspaceConversationArchiveParams, WorkspaceConversationIdParams,
};
use smelt_runtime_client::protocol::control::EmptyParams;

pub(crate) fn run(args: &[String]) -> i32 {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("list") => match parse_list_args(rest) {
            Ok(filter) => respond(list(&filter)),
            Err(message) => usage(&message),
        },
        Some("rename") => {
            let Some(id) = id_arg(rest) else {
                return usage(&t!("conversation.rename_usage"));
            };
            match title_flag(&rest[1..]) {
                Ok(title) => respond(rename(id, &title)),
                Err(message) => usage(&message),
            }
        }
        Some("send") => {
            let Some(id) = id_arg(rest) else {
                return usage(&t!("conversation.send_usage"));
            };
            match message_flag(&rest[1..]) {
                Ok(text) => respond(send(id, text)),
                Err(message) => usage(&message),
            }
        }
        Some("result") => match rest {
            [id, delivery_id] => respond(turn_result(id, delivery_id)),
            _ => usage("smelt conversation result <id> <delivery_id>"),
        },
        Some(verb @ ("archive" | "unarchive" | "delete")) => {
            let Some(id) = id_arg(rest) else {
                return usage(&format!("smelt conversation {verb} <id>"));
            };
            respond(change(verb, id))
        }
        Some(command) => usage(&t!("unknown_subcommand", command = command)),
        None => usage("smelt conversation list|send|result|rename|archive|unarchive|delete"),
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ListFilter {
    agent: Option<String>,
    archived: bool,
}

fn parse_list_args(args: &[String]) -> Result<ListFilter, String> {
    let mut filter = ListFilter::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--archived" => {
                filter.archived = true;
                index += 1;
            }
            "--agent" => {
                filter.agent = Some(flag_value(args, index)?);
                index += 2;
            }
            other => return Err(t!("unknown_argument", arg = other).into_owned()),
        }
    }
    Ok(filter)
}

fn directory() -> Result<Vec<WorkspaceConversation>, String> {
    control::<WorkspaceDirectoryGet>(EmptyParams::default())
        .map(|directory| directory.conversations)
}

fn list(filter: &ListFilter) -> Result<serde_json::Value, String> {
    let conversations = directory()?;
    Ok(serde_json::json!({ "conversations": summarize(&conversations, filter) }))
}

/// 新的在前；默认不列已归档的。
fn summarize(
    conversations: &[WorkspaceConversation],
    filter: &ListFilter,
) -> Vec<serde_json::Value> {
    let mut picked: Vec<&WorkspaceConversation> = conversations
        .iter()
        .filter(|conversation| filter.archived || conversation.archived_at.is_none())
        .filter(|conversation| {
            filter.agent.as_deref().is_none_or(|agent| {
                conversation
                    .acp
                    .as_ref()
                    .and_then(|acp| acp.agent_definition_id.as_deref())
                    == Some(agent)
            })
        })
        .collect();
    picked.sort_by_key(|conversation| std::cmp::Reverse(conversation.last_updated_at));
    picked.into_iter().map(summary).collect()
}

fn summary(conversation: &WorkspaceConversation) -> serde_json::Value {
    let acp = conversation.acp.as_ref();
    let title = conversation
        .custom_title
        .clone()
        .or_else(|| acp.and_then(|acp| acp.session_title.clone()))
        .unwrap_or_default();
    serde_json::json!({
        "id": conversation.id,
        "session_id": acp.map(|acp| &acp.sid),
        "history_session_id": acp.and_then(|acp| acp.history_session_id.as_ref()),
        "title": title,
        "agent": acp.map(|acp| &acp.agent),
        "agent_definition_id": acp.and_then(|acp| acp.agent_definition_id.as_ref()),
        "automation_id": acp.and_then(|acp| acp.automation_id.as_ref()),
        "cwd": acp.and_then(|acp| acp.cwd.as_ref()),
        "archived": conversation.archived_at.is_some(),
        "last_updated_at": conversation.last_updated_at,
        "terminal": acp.is_none(),
    })
}

/// 对话 id、守护会话 id 或 agent 原生会话 id（`history_session_id`）都行。
/// 同一段原生历史若存在已归档和当前打开的多条记录，优先命中未归档且最近更新的。
fn find<'a>(
    conversations: &'a [WorkspaceConversation],
    id: &str,
) -> Result<&'a WorkspaceConversation, String> {
    conversations
        .iter()
        .find(|conversation| {
            conversation.id == id || conversation.acp.as_ref().is_some_and(|acp| acp.sid == id)
        })
        .or_else(|| {
            conversations
                .iter()
                .filter(|conversation| {
                    conversation
                        .acp
                        .as_ref()
                        .and_then(|acp| acp.history_session_id.as_deref())
                        == Some(id)
                })
                .max_by_key(|conversation| {
                    (
                        conversation.archived_at.is_none(),
                        conversation.last_updated_at,
                    )
                })
        })
        .ok_or_else(|| t!("conversation.not_found", id = id).into_owned())
}

fn rename(id: &str, title: &str) -> Result<serde_json::Value, String> {
    let conversations = directory()?;
    let conversation = find(&conversations, id)?;
    let Some(acp) = &conversation.acp else {
        return Err(t!("conversation.terminal_rename").into_owned());
    };
    let renamed = smelt_client::session_ops::rename_agent_conversation(
        &crate::runtime(),
        &acp.sid,
        title,
    )?;
    serde_json::to_value(renamed).map_err(|error| error.to_string())
}

/// 智能体对话的会话 id。终端没有，发不了话。
fn session_id(id: &str) -> Result<String, String> {
    let conversations = directory()?;
    let conversation = find(&conversations, id)?;
    conversation
        .acp
        .as_ref()
        .map(|acp| acp.sid.clone())
        .ok_or_else(|| t!("conversation.terminal_send").into_owned())
}

/// 挂起的对话要先唤醒、等它续上历史，运行时最多等两分钟；多留一点给往返。
const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(150);

fn send(id: &str, text: String) -> Result<serde_json::Value, String> {
    let session_id = session_id(id)?;
    let delivery_id = uuid::Uuid::new_v4().to_string();
    control_within::<ConversationPrompt>(
        ConversationPromptParams {
            session_id: session_id.clone(),
            delivery_id: delivery_id.clone(),
            text,
            disallowed_tools: Vec::new(),
        },
        SEND_TIMEOUT,
    )?;
    Ok(serde_json::json!({ "session_id": session_id, "delivery_id": delivery_id }))
}

fn turn_result(id: &str, delivery_id: &str) -> Result<serde_json::Value, String> {
    let status = control::<ConversationTurnResult>(ConversationTurnResultParams {
        session_id: session_id(id)?,
        delivery_id: delivery_id.to_string(),
    })?;
    serde_json::to_value(status).map_err(|error| error.to_string())
}

fn message_flag(args: &[String]) -> Result<String, String> {
    match args {
        [flag, text] if flag == "--message" && !text.trim().is_empty() => Ok(text.clone()),
        [flag, ..] if flag == "--message" => Err(t!("agent.message_missing").into_owned()),
        [other, ..] => Err(t!("unknown_argument", arg = other).into_owned()),
        [] => Err(t!("conversation.send_usage").into_owned()),
    }
}

fn change(verb: &str, id: &str) -> Result<serde_json::Value, String> {
    let conversations = directory()?;
    let conversation_id = find(&conversations, id)?.id.clone();
    match verb {
        "archive" | "unarchive" => {
            control::<WorkspaceConversationArchive>(WorkspaceConversationArchiveParams {
                conversation_id: conversation_id.clone(),
                archived: verb == "archive",
                target_workset_id: None,
            })
        }
        _ => control::<WorkspaceConversationDelete>(WorkspaceConversationIdParams {
            conversation_id: conversation_id.clone(),
        }),
    }?;
    Ok(serde_json::json!({ "id": conversation_id }))
}

fn title_flag(args: &[String]) -> Result<String, String> {
    match args {
        [flag, title] if flag == "--title" && !title.trim().is_empty() => Ok(title.clone()),
        [flag, ..] if flag == "--title" => Err(t!("conversation.title_missing").into_owned()),
        [other, ..] => Err(t!("unknown_argument", arg = other).into_owned()),
        [] => Err(t!("conversation.rename_usage").into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args;
    use smelt_model::workspace_directory::WorkspaceAcp;

    fn conversation(
        id: &str,
        sid: Option<&str>,
        agent: Option<&str>,
        updated: i64,
    ) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            last_updated_at: updated,
            acp: sid.map(|sid| WorkspaceAcp {
                sid: sid.into(),
                agent_definition_id: agent.map(str::to_string),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn find_accepts_either_id() {
        let conversations = [conversation("c1", Some("s1"), None, 0)];
        assert_eq!(find(&conversations, "c1").unwrap().id, "c1");
        assert_eq!(find(&conversations, "s1").unwrap().id, "c1");
        assert!(find(&conversations, "s2").is_err());
    }

    #[test]
    fn find_accepts_history_session_id_and_prefers_active() {
        let mut archived = conversation("old", Some("acp-old"), None, 10);
        archived.archived_at = Some(1);
        archived.acp.as_mut().unwrap().history_session_id = Some("native-1".into());

        let mut active = conversation("cur", Some("acp-cur"), None, 5);
        active.acp.as_mut().unwrap().history_session_id = Some("native-1".into());

        let conversations = [archived, active];
        assert_eq!(find(&conversations, "native-1").unwrap().id, "cur");
        assert_eq!(find(&conversations, "acp-old").unwrap().id, "old");
    }

    #[test]
    fn summary_exposes_history_session_id() {
        let mut item = conversation("c1", Some("acp-1"), None, 1);
        item.acp.as_mut().unwrap().history_session_id = Some("native-1".into());
        let json = summary(&item);
        assert_eq!(json["session_id"], "acp-1");
        assert_eq!(json["history_session_id"], "native-1");
    }

    #[test]
    fn list_hides_archived_and_filters_by_agent() {
        let mut archived = conversation("old", Some("s0"), Some("writer"), 5);
        archived.archived_at = Some(1);
        let conversations = [
            conversation("a", Some("s1"), Some("writer"), 1),
            conversation("b", None, None, 3),
            conversation("c", Some("s2"), Some("writer"), 2),
            archived,
        ];
        let ids = |filter: &ListFilter| {
            summarize(&conversations, filter)
                .into_iter()
                .map(|summary| summary["id"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&ListFilter::default()), ["b", "c", "a"]);
        assert_eq!(
            ids(&parse_list_args(&args(&["--agent", "writer", "--archived"])).unwrap()),
            ["old", "c", "a"]
        );
        assert!(parse_list_args(&args(&["--agent"])).is_err());
    }

    #[test]
    fn send_takes_exactly_one_message() {
        assert_eq!(message_flag(&args(&["--message", "hi"])).unwrap(), "hi");
        assert!(message_flag(&args(&["--message", " "])).is_err());
        assert!(message_flag(&args(&["--message"])).is_err());
        assert!(message_flag(&args(&["hi"])).is_err());
        assert!(message_flag(&args(&[])).is_err());
    }
}
