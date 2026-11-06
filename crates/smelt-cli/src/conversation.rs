//! `smelt conversation …`：侧栏里的对话。
//!
//! 对话有两个 id：工作区目录里的对话 id，和智能体对话的会话 id（`agent chat` 返回的
//! `session_id`）。这里两个都认，按需要换成对应的那个。往对话里发消息、读对话内容用
//! Pi 自带的 `session_send` / `session_read`，不在命令行重复一份。

use crate::{control, flag_value, id_arg, respond, usage};
use smelt_model::control_api::{
    WorkspaceConversationArchive, WorkspaceConversationDelete, WorkspaceConversationPin,
    WorkspaceDirectoryGet,
};
use smelt_model::workspace_directory::{
    WorkspaceConversation, WorkspaceConversationArchiveParams, WorkspaceConversationIdParams,
    WorkspaceConversationPinParams,
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
                return usage("smelt conversation rename <id> --title <标题>");
            };
            match title_flag(&rest[1..]) {
                Ok(title) => respond(rename(id, &title)),
                Err(message) => usage(&message),
            }
        }
        Some(verb @ ("archive" | "unarchive" | "pin" | "unpin" | "delete")) => {
            let Some(id) = id_arg(rest) else {
                return usage(&format!("smelt conversation {verb} <id>"));
            };
            respond(change(verb, id))
        }
        Some(command) => usage(&format!("未知子命令: {command}")),
        None => usage("smelt conversation list|rename|archive|unarchive|pin|unpin|delete"),
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
            other => return Err(format!("未知参数: {other}")),
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
        "title": title,
        "agent": acp.map(|acp| &acp.agent),
        "agent_definition_id": acp.and_then(|acp| acp.agent_definition_id.as_ref()),
        "automation_id": acp.and_then(|acp| acp.automation_id.as_ref()),
        "cwd": acp.and_then(|acp| acp.cwd.as_ref()),
        "archived": conversation.archived_at.is_some(),
        "pinned": conversation.pinned_at.is_some(),
        "last_updated_at": conversation.last_updated_at,
        "terminal": acp.is_none(),
    })
}

/// 对话 id 或会话 id 都行。
fn find<'a>(
    conversations: &'a [WorkspaceConversation],
    id: &str,
) -> Result<&'a WorkspaceConversation, String> {
    conversations
        .iter()
        .find(|conversation| {
            conversation.id == id || conversation.acp.as_ref().is_some_and(|acp| acp.sid == id)
        })
        .ok_or_else(|| format!("没有这段对话：{id}（smelt conversation list 查看）"))
}

fn rename(id: &str, title: &str) -> Result<serde_json::Value, String> {
    let conversations = directory()?;
    let conversation = find(&conversations, id)?;
    let Some(acp) = &conversation.acp else {
        return Err("终端对话不能用命令行改名".into());
    };
    let renamed = smelt_client::session_ops::rename_agent_conversation(
        &smelt_runtime_client::local::local_runtime(),
        &acp.sid,
        title,
    )?;
    serde_json::to_value(renamed).map_err(|error| error.to_string())
}

fn change(verb: &str, id: &str) -> Result<serde_json::Value, String> {
    let conversations = directory()?;
    let conversation_id = find(&conversations, id)?.id.clone();
    match verb {
        "archive" | "unarchive" => {
            control::<WorkspaceConversationArchive>(WorkspaceConversationArchiveParams {
                conversation_id: conversation_id.clone(),
                archived: verb == "archive",
            })
        }
        "pin" | "unpin" => control::<WorkspaceConversationPin>(WorkspaceConversationPinParams {
            conversation_id: conversation_id.clone(),
            pinned: verb == "pin",
        }),
        _ => control::<WorkspaceConversationDelete>(WorkspaceConversationIdParams {
            conversation_id: conversation_id.clone(),
        }),
    }?;
    Ok(serde_json::json!({ "id": conversation_id }))
}

fn title_flag(args: &[String]) -> Result<String, String> {
    match args {
        [flag, title] if flag == "--title" && !title.trim().is_empty() => Ok(title.clone()),
        [flag, ..] if flag == "--title" => Err("--title 需要标题".into()),
        [other, ..] => Err(format!("未知参数: {other}")),
        [] => Err("smelt conversation rename <id> --title <标题>".into()),
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
}
