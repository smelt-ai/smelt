//! 列表上发起的事：去某段会话（打开会话插件的会话视图）、选中项目（会话插件的
//! `SELECT_PROJECT`），以及关会话、归档、打开 / 关掉项目这些目录操作（直接调那台机器的
//! 运行时）。都回一个任务，失败由调用方挂到发起的那一行上。

use rust_i18n::t;
use std::path::Path;

use gpui::{App, ClipboardItem, Task};
use smelt_plugin_api::model::control_api::{
    WorkspaceConversationArchive, WorkspaceConversationDelete, WorkspaceProjectClose,
    WorkspaceProjectOpen,
};
use smelt_plugin_api::model::daemon_state::DaemonSessionState;
use smelt_plugin_api::model::runtime_path::PathStyle;
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceConversationArchiveParams, WorkspaceConversationIdParams, WorkspaceDirectory,
    WorkspaceProjectParams,
};
use smelt_plugin_api::{self as api, RuntimeKey};

use crate::core::{SELECT_PROJECT, SESSION, SelectProject, SessionArgs, SessionAt};
use crate::model::{self, ConversationKey, Placement};

/// 在主窗口里露出这段会话、焦点给它：主窗口拿到前面（关着就先开），再打开会话视图。
/// 找不到、要等目录推到都归会话视图。
pub(crate) fn open_conversation(key: ConversationKey, cx: &mut App) -> Task<Result<(), String>> {
    let args = SessionArgs {
        machine: key.machine,
        at: SessionAt::Conversation {
            conversation_id: key.id,
        },
        after: None,
    };
    let args = match serde_json::to_value(&args) {
        Ok(args) => args,
        Err(error) => return Task::ready(Err(error.to_string())),
    };
    let shown = api::SHOW_MAIN_WINDOW.run(&(), cx);
    cx.spawn(async move |cx| {
        shown.await?;
        let opened = cx.update(|cx| {
            api::OPEN_VIEW.run(
                &api::OpenView {
                    view: SESSION.to_string(),
                    args,
                },
                cx,
            )
        });
        if opened.await? {
            Ok(())
        } else {
            Err(t!("error.no_room").into_owned())
        }
    })
}

/// 选中一个已经在列表里的项目。
pub(crate) fn select_project(
    machine: RuntimeKey,
    root: String,
    cx: &mut App,
) -> Task<Result<(), String>> {
    SELECT_PROJECT.run(
        &SelectProject {
            machine,
            root,
            after: None,
        },
        cx,
    )
}

/// 关会话：从目录里删掉。运行时删之前先收掉它的终端和对话（收不掉就报错、目录留着），
/// 这边不另外杀。要不要先确认归调用方。
pub(crate) fn close_conversation(key: ConversationKey, cx: &mut App) -> Task<Result<(), String>> {
    let params = WorkspaceConversationIdParams {
        conversation_id: key.id,
    };
    let deleted =
        api::call::<WorkspaceConversationDelete>(&key.machine, params, api::CALL_TIMEOUT, cx);
    cx.spawn(async move |_| deleted.await.map(|_| ()))
}

/// 归档一段会话。自动化运行的运行时会拒绝。
pub(crate) fn archive_conversation(key: ConversationKey, cx: &mut App) -> Task<Result<(), String>> {
    let params = WorkspaceConversationArchiveParams {
        conversation_id: key.id,
        archived: true,
        target_workset_id: None,
    };
    let archived =
        api::call::<WorkspaceConversationArchive>(&key.machine, params, api::CALL_TIMEOUT, cx);
    cx.spawn(async move |_| archived.await.map(|_| ()))
}

/// 打开一个项目并选中它（已经开着就只选中）。
pub(crate) fn open_project(
    machine: RuntimeKey,
    root: String,
    cx: &mut App,
) -> Task<Result<(), String>> {
    let host = api::host(cx);
    let style = host.path_style(&machine, cx);
    let root = style.clean(root.trim());
    if root.is_empty() {
        return Task::ready(Err(t!("error.no_project_dir").into_owned()));
    }
    let open = host
        .workspace_directory(&machine, cx)
        .is_some_and(|directory| {
            directory
                .projects
                .iter()
                .any(|project| style.clean(project) == root)
        });
    if open {
        return select_project(machine, root, cx);
    }
    let opened = api::call::<WorkspaceProjectOpen>(
        &machine,
        WorkspaceProjectParams { root: root.clone() },
        api::CALL_TIMEOUT,
        cx,
    );
    cx.spawn(async move |cx| {
        let revision = opened.await?;
        // 刚打开的项目要等目录推到这一版才在列表里：带上版本，会话插件等到了再认。
        let selected = cx.update(|cx| {
            SELECT_PROJECT.run(
                &SelectProject {
                    machine,
                    root,
                    after: Some(revision),
                },
                cx,
            )
        });
        selected.await
    })
}

/// 关掉一个项目：先一段段关掉侧栏里摆在它下面的会话（含它的工作集里的），都关掉了再从
/// 目录里关项目；中途有关不掉的就停下报错，项目留着。只靠会话工作目录撑起来的组（不在
/// 打开的项目里）关完会话就没了。正选着它的话，目录推到后会话插件自己清掉选中。要不要先
/// 确认归调用方。
pub(crate) fn close_project(
    machine: RuntimeKey,
    root: String,
    cx: &mut App,
) -> Task<Result<(), String>> {
    let host = api::host(cx);
    let style = host.path_style(&machine, cx);
    let root = style.clean(root.trim());
    if root.is_empty() {
        return Task::ready(Err(t!("error.no_project_dir").into_owned()));
    }
    let Some(directory) = host.workspace_directory(&machine, cx) else {
        return Task::ready(Err(t!("error.directory_pending").into_owned()));
    };
    let closing = conversations_under(style, &directory, &root);
    let opened = directory
        .projects
        .iter()
        .find(|project| style.clean(project) == root)
        .cloned();
    cx.spawn(async move |cx| {
        for conversation_id in closing {
            let deleted = cx.update(|cx| {
                api::call::<WorkspaceConversationDelete>(
                    &machine,
                    WorkspaceConversationIdParams { conversation_id },
                    api::CALL_TIMEOUT,
                    cx,
                )
            });
            deleted.await?;
        }
        if let Some(opened) = opened {
            let closed = cx.update(|cx| {
                api::call::<WorkspaceProjectClose>(
                    &machine,
                    WorkspaceProjectParams { root: opened },
                    api::CALL_TIMEOUT,
                    cx,
                )
            });
            closed.await?;
        }
        Ok(())
    })
}

/// 侧栏里摆在项目 `root`（已经按 `style` 规整过）下面的会话。
fn conversations_under(
    style: PathStyle,
    directory: &WorkspaceDirectory,
    root: &str,
) -> Vec<String> {
    directory
        .conversations
        .iter()
        .filter(|conversation| {
            matches!(
                model::placement(style, directory, conversation),
                Placement::Project { root: placed, .. } if style.clean(&placed) == root
            )
        })
        .map(|conversation| conversation.id.clone())
        .collect()
}

/// 把这一格的会话 id 放进剪贴板，见 [`id_to_copy`]。
pub(crate) fn copy_session_id(machine: &RuntimeKey, session_id: &str, cx: &mut App) {
    let host = api::host(cx);
    let directory = host.workspace_directory(machine, cx);
    let states = host.session_states(machine, cx);
    let id = id_to_copy(directory.as_ref(), &states, session_id);
    cx.write_to_clipboard(ClipboardItem::new_string(id));
}

/// 复制会话 id 时给的：agent 自己的对话 id（续接身份、或 hook 报来的那段对话），拿去
/// `--resume`、找存档才用得上；agent 还没报上来时是守护会话 id。握手、TUI 里 `/resume`
/// 都会换 id，所以用时现取。和会话插件的「复制会话 id」同一个规则，两边各算一份。
fn id_to_copy(
    directory: Option<&WorkspaceDirectory>,
    states: &[DaemonSessionState],
    session_id: &str,
) -> String {
    let history = directory
        .and_then(|directory| directory.conversation_of_session(session_id))
        .and_then(|conversation| conversation.acp.as_ref())
        .filter(|acp| acp.sid == session_id)
        .and_then(|acp| acp.history_session_id.as_deref());
    let reported = || {
        states
            .iter()
            .find(|state| state.id == session_id)
            .and_then(|state| state.conversation_id.as_deref())
    };
    history
        .or_else(reported)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(session_id)
        .to_string()
}

/// 访达拖进来、「用 Smelt 打开」的路径：是目录的当项目打开，文件留给文件打开者。只认
/// 本机：别的机器上的路径这边判断不了是不是目录。
pub(crate) fn open_paths(machine: RuntimeKey, paths: Vec<String>, cx: &mut App) {
    if machine != RuntimeKey::Local {
        eprintln!(
            "[session-list] 只认本机上拖进来的目录，忽略 {} 条路径",
            paths.len()
        );
        return;
    }
    // 判断是不是目录要读盘，放到后台。
    let dirs = cx.background_executor().spawn(async move {
        let mut dirs: Vec<String> = Vec::new();
        for path in paths {
            if Path::new(&path).is_dir() && !dirs.contains(&path) {
                dirs.push(path);
            }
        }
        dirs
    });
    cx.spawn(async move |cx| {
        let dirs = dirs.await;
        cx.update(|cx| {
            for dir in dirs {
                let opened = open_project(machine.clone(), dir, cx);
                crate::report_failure(t!("action.open_project"), opened, cx);
            }
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_plugin_api::model::workspace_directory::{
        WorkspaceAcp, WorkspaceConversation, WorkspaceTerminal, WorkspaceWorkset,
    };

    fn terminals(id: &str, cwd: &str) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            terminals: vec![WorkspaceTerminal {
                id: Some(format!("{id}-t")),
                cwd: Some(cwd.into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn closing_a_project_takes_its_worksets_sessions_too() {
        let style = PathStyle::Posix;
        let mut in_workset = terminals("w", "/elsewhere");
        in_workset.workset_id = Some("ws".into());
        let mut archived = terminals("old", "/repo");
        archived.archived_at = Some(1);
        let directory = WorkspaceDirectory {
            projects: vec!["/repo".into()],
            worksets: vec![WorkspaceWorkset {
                id: "ws".into(),
                project_root: "/repo".into(),
                task: "fix".into(),
                ..Default::default()
            }],
            conversations: vec![
                in_workset,
                terminals("p", "/repo/sub"),
                terminals("l", "/tmp/x"),
                archived,
            ],
            ..Default::default()
        };
        assert_eq!(conversations_under(style, &directory, "/repo"), ["w", "p"]);
        assert_eq!(conversations_under(style, &directory, "/tmp/x"), ["l"]);
        assert!(conversations_under(style, &directory, "/nowhere").is_empty());
    }

    #[test]
    fn the_copied_id_is_the_agents_own_when_known() {
        let directory = WorkspaceDirectory {
            conversations: vec![
                WorkspaceConversation {
                    id: "c".into(),
                    acp: Some(WorkspaceAcp {
                        sid: "acp-1".into(),
                        history_session_id: Some("h1".into()),
                        ..WorkspaceAcp::default()
                    }),
                    ..WorkspaceConversation::default()
                },
                terminals("t", "/repo"),
            ],
            ..WorkspaceDirectory::default()
        };
        let reported: DaemonSessionState = serde_json::from_value(serde_json::json!({
            "id": "t-t",
            "conversation_id": " tui-9 ",
        }))
        .unwrap();
        assert_eq!(id_to_copy(Some(&directory), &[], "acp-1"), "h1");
        assert_eq!(id_to_copy(Some(&directory), &[reported], "t-t"), "tui-9");
        assert_eq!(id_to_copy(Some(&directory), &[], "t-t"), "t-t");
        assert_eq!(id_to_copy(None, &[], "acp-1"), "acp-1");
    }
}
