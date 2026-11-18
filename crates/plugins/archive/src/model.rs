//! 从运行时的工作区目录里挑出归档的对话，按机器、项目分好组。纯函数：不碰宿主，测试里
//! 直接喂目录。

use chrono::{Datelike, Local, TimeZone};
use rust_i18n::t;
use smelt_plugin_api::RuntimeKey;
use smelt_plugin_api::model::runtime_path::PathStyle;
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceConversation, WorkspaceDirectory, project_labels, project_root_of,
};

/// 一台机器上的归档。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineGroup {
    pub machine: RuntimeKey,
    pub name: String,
    /// 机器眼下用不了的原因（离线、版本不对）；能用是 `None`。
    pub unavailable: Option<String>,
    /// 最近归档的项目在前。
    pub projects: Vec<ProjectGroup>,
}

/// 一个项目下的归档。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectGroup {
    /// 项目根目录；认不出项目时是对话的工作目录。只当分组的身份和悬停提示用。
    pub root: String,
    pub label: String,
    /// 最近归档的在前。
    pub rows: Vec<ArchivedRow>,
}

/// 一段归档的对话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchivedRow {
    /// 工作区目录里的对话 id，恢复、删除都按它。
    pub id: String,
    pub title: String,
    /// ACP 对话；否则是一组终端。
    pub is_conversation: bool,
    /// 「归档于 …」，空的是不知道时间。
    pub when: String,
    /// 恢复以后跳过去用的守护会话 id：对话的 `sid`、终端组的第一格。终端还没起过的没有。
    pub session_id: Option<String>,
    /// 原工作树已经关掉：恢复前要另选一个工作树。
    pub closed_workset: Option<ClosedWorkset>,
}

/// 原工作树已关闭的归档：恢复时要选同一项目下的另一个工作树。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClosedWorkset {
    pub old_root: String,
    /// 同一项目下还在的工作树：`(id, 显示名)`。
    pub candidates: Vec<(String, String)>,
}

/// 一台机器的归档分组。`now` 是当前的 unix 秒，用来决定时间写不写年份。
pub(crate) fn machine_group(
    machine: RuntimeKey,
    name: String,
    unavailable: Option<String>,
    style: PathStyle,
    directory: &WorkspaceDirectory,
    now: i64,
) -> Option<MachineGroup> {
    let mut archived: Vec<&WorkspaceConversation> = directory
        .conversations
        .iter()
        .filter(|conversation| conversation.archived_at.is_some())
        .collect();
    if archived.is_empty() {
        return None;
    }
    archived.sort_by(|a, b| b.archived_at.cmp(&a.archived_at));

    // 按出现先后（也就是最近归档的项目在前）收进各组。
    let mut groups: Vec<(String, Vec<ArchivedRow>)> = Vec::new();
    for conversation in archived {
        let root = project_of(style, directory, conversation);
        let row = archived_row(style, directory, conversation, &root, now);
        match groups
            .iter_mut()
            .find(|(key, _)| style.clean(key) == style.clean(&root))
        {
            Some((_, rows)) => rows.push(row),
            None => groups.push((root, vec![row])),
        }
    }
    let roots: Vec<&str> = groups.iter().map(|(root, _)| root.as_str()).collect();
    let labels = project_labels(style, &roots);
    let projects = groups
        .into_iter()
        .zip(labels)
        .map(|((root, rows), label)| ProjectGroup { root, label, rows })
        .collect();
    Some(MachineGroup {
        machine,
        name,
        unavailable,
        projects,
    })
}

/// 对话归哪个项目：绑定的工作树所属的项目；原工作树已关的按它的路径推（工作树开在
/// `<项目>/worktrees/<任务>`）；否则按工作目录落在哪个打开的项目里；都认不出就按工作目录
/// 自成一组。
fn project_of(
    style: PathStyle,
    directory: &WorkspaceDirectory,
    conversation: &WorkspaceConversation,
) -> String {
    if let Some(workset) = conversation
        .workset_id
        .as_deref()
        .and_then(|id| directory.worksets.iter().find(|workset| workset.id == id))
    {
        return workset.project_root.clone();
    }
    if let Some(root) = conversation
        .closed_workset_root
        .as_deref()
        .and_then(|old| style.parent(old))
        .and_then(|worktrees| style.parent(&worktrees))
    {
        return root;
    }
    let cwd = cwd_of(conversation);
    project_root_of(style, &directory.projects, cwd)
        .unwrap_or(cwd)
        .to_string()
}

fn cwd_of(conversation: &WorkspaceConversation) -> &str {
    conversation
        .acp
        .as_ref()
        .and_then(|acp| acp.cwd.as_deref())
        .or_else(|| {
            conversation
                .terminals
                .first()
                .and_then(|terminal| terminal.cwd.as_deref())
        })
        .unwrap_or("")
}

fn archived_row(
    style: PathStyle,
    directory: &WorkspaceDirectory,
    conversation: &WorkspaceConversation,
    project_root: &str,
    now: i64,
) -> ArchivedRow {
    let closed_workset = conversation
        .closed_workset_root
        .as_ref()
        .filter(|_| conversation.workset_id.is_none())
        .map(|old_root| ClosedWorkset {
            old_root: old_root.clone(),
            candidates: directory
                .worksets
                .iter()
                .filter(|workset| style.clean(&workset.project_root) == style.clean(project_root))
                .map(|workset| {
                    (
                        workset.id.clone(),
                        format!("{} · {}", workset.task, workset.branch),
                    )
                })
                .collect(),
        });
    let session_id = match &conversation.acp {
        Some(acp) => Some(acp.sid.clone()),
        None => conversation
            .terminals
            .first()
            .and_then(|terminal| terminal.id.clone()),
    };
    ArchivedRow {
        id: conversation.id.clone(),
        title: title_of(conversation),
        is_conversation: conversation.acp.is_some(),
        when: conversation
            .archived_at
            .map(|at| archived_when(at, now))
            .unwrap_or_default(),
        session_id,
        closed_workset,
    }
}

/// 对话的名字：用户改的、agent 起的、终端的，都没有就按种类叫。
pub(crate) fn title_of(conversation: &WorkspaceConversation) -> String {
    conversation
        .custom_title
        .as_deref()
        .or_else(|| {
            conversation
                .acp
                .as_ref()
                .and_then(|acp| acp.session_title.as_deref())
        })
        .or_else(|| {
            conversation.terminals.first().and_then(|terminal| {
                terminal
                    .custom_title
                    .as_deref()
                    .or(terminal.launch_label.as_deref())
            })
        })
        .filter(|title| !title.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if conversation.acp.is_some() {
                t!("row.untitled").into_owned()
            } else {
                t!("row.terminal").into_owned()
            }
        })
}

/// 「归档于 11-14 10:32」，不是今年的写年份。
fn archived_when(at: i64, now: i64) -> String {
    let (Some(at), Some(now)) = (
        Local.timestamp_opt(at, 0).single(),
        Local.timestamp_opt(now, 0).single(),
    ) else {
        return String::new();
    };
    if at.year() == now.year() {
        t!("row.archived_at", time = at.format("%m-%d %H:%M")).into_owned()
    } else {
        t!("row.archived_at", time = at.format("%Y-%m-%d")).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use smelt_plugin_api::model::workspace_directory::{
        WorkspaceAcp, WorkspaceTerminal, WorkspaceWorkset,
    };

    use super::*;

    fn acp(id: &str, cwd: &str, archived_at: Option<i64>) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            archived_at,
            acp: Some(WorkspaceAcp {
                cwd: Some(cwd.into()),
                sid: format!("sid-{id}"),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn group(directory: &WorkspaceDirectory) -> Option<MachineGroup> {
        machine_group(
            RuntimeKey::Local,
            "本机".into(),
            None,
            PathStyle::Posix,
            directory,
            1_700_000_000,
        )
    }

    #[test]
    fn no_archived_means_no_group() {
        let directory = WorkspaceDirectory {
            projects: vec!["/p/a".into()],
            conversations: vec![acp("1", "/p/a", None)],
            ..Default::default()
        };
        assert_eq!(group(&directory), None);
    }

    #[test]
    fn groups_by_project_latest_first() {
        let directory = WorkspaceDirectory {
            projects: vec!["/p/a".into(), "/p/b".into()],
            conversations: vec![
                acp("1", "/p/a/sub", Some(10)),
                acp("2", "/p/b", Some(30)),
                acp("3", "/p/a", Some(20)),
                acp("4", "/p/a", None),
            ],
            ..Default::default()
        };
        let group = group(&directory).unwrap();
        let shape: Vec<(&str, Vec<&str>)> = group
            .projects
            .iter()
            .map(|p| {
                (
                    p.root.as_str(),
                    p.rows.iter().map(|r| r.id.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(shape, vec![("/p/b", vec!["2"]), ("/p/a", vec!["3", "1"])]);
        assert_eq!(
            group.projects[1].rows[0].session_id.as_deref(),
            Some("sid-3")
        );
    }

    #[test]
    fn closed_project_falls_back_to_cwd() {
        let directory = WorkspaceDirectory {
            conversations: vec![acp("1", "/gone/x", Some(1))],
            ..Default::default()
        };
        let group = group(&directory).unwrap();
        assert_eq!(group.projects[0].root, "/gone/x");
        assert_eq!(group.projects[0].label, "x");
    }

    #[test]
    fn closed_workset_offers_same_project_worksets() {
        let mut conversation = acp("1", "/p/a/worktrees/old", Some(1));
        conversation.closed_workset_root = Some("/p/a/worktrees/old".into());
        let directory = WorkspaceDirectory {
            projects: vec!["/p/a".into()],
            worksets: vec![
                WorkspaceWorkset {
                    id: "w1".into(),
                    project_root: "/p/a".into(),
                    task: "new".into(),
                    branch: "feat/new".into(),
                    ..Default::default()
                },
                WorkspaceWorkset {
                    id: "w2".into(),
                    project_root: "/p/b".into(),
                    task: "other".into(),
                    branch: "other".into(),
                    ..Default::default()
                },
            ],
            conversations: vec![conversation],
            ..Default::default()
        };
        let group = group(&directory).unwrap();
        assert_eq!(group.projects[0].root, "/p/a");
        let closed = group.projects[0].rows[0].closed_workset.clone().unwrap();
        assert_eq!(closed.old_root, "/p/a/worktrees/old");
        assert_eq!(
            closed.candidates,
            vec![("w1".to_string(), "new · feat/new".to_string())]
        );
    }

    #[test]
    fn terminal_rows_use_first_pane() {
        let directory = WorkspaceDirectory {
            conversations: vec![WorkspaceConversation {
                id: "t".into(),
                archived_at: Some(1),
                terminals: vec![WorkspaceTerminal {
                    id: Some("pane-1".into()),
                    cwd: Some("/p".into()),
                    launch_label: Some("zsh".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let row = &group(&directory).unwrap().projects[0].rows[0];
        assert!(!row.is_conversation);
        assert_eq!(row.title, "zsh");
        assert_eq!(row.session_id.as_deref(), Some("pane-1"));
    }
}
