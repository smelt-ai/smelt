//! 列表上各种行的动作：置顶、拖动换位置、关闭、归档、关工作树、打开项目（挑目录、从访达
//! 拖进来）。菜单和行上的按钮都调这里。要确认的（会话还在跑、项目里还有会话、关工作树）在
//! 这里先问。

use std::path::PathBuf;

use gpui::{App, SharedString, Window};
use smelt_plugin_api::model::agent_status::AgentStatus;
use smelt_plugin_api::model::control_api::WorkspaceWorksetFinish;
use smelt_plugin_api::model::workspace_directory::WorkspaceWorksetFinishParams;
use smelt_plugin_api::ui::dialog::Confirm;
use smelt_plugin_api::{self as api, RuntimeKey};

use super::{GroupKey, Sidebar, move_near, move_to_pin_boundary, splice_group};
use crate::lifecycle;
use crate::model::{ConversationKey, Pin, Sessions};

fn sidebar(cx: &App) -> std::rc::Rc<Sidebar> {
    Sessions::global(cx).read(cx).sidebar.clone()
}

// ---- 置顶 ----

/// 置顶或取消置顶一个项目，并把它挪到置顶区和其余的交界（见 [`move_to_pin_boundary`]）。
pub(super) fn pin_project(machine: &RuntimeKey, root: &str, pinned: bool, cx: &mut App) {
    let Some(roots) = sidebar(cx).machine(machine).map(|m| m.roots.clone()) else {
        return;
    };
    let sessions = Sessions::global(cx);
    let order = {
        let ui = &sessions.read(cx).ui;
        move_to_pin_boundary(&roots, root, |other| {
            if other == root {
                pinned
            } else {
                ui.is_pinned(&Pin::Project {
                    machine: machine.clone(),
                    root: other.to_string(),
                })
            }
        })
    };
    sessions.update(cx, |sessions, cx| {
        let pin = Pin::Project {
            machine: machine.clone(),
            root: root.to_string(),
        };
        sessions.set_pinned(pin, pinned, cx);
        sessions.set_project_order(machine, order, cx);
    });
}

/// 置顶或取消置顶一段会话。项目里的会话同样挪到交界；「对话」一段按最近动静排，不记手动
/// 顺序，只改置顶。
pub(super) fn pin_session(key: &ConversationKey, group: &GroupKey, pinned: bool, cx: &mut App) {
    let sessions = Sessions::global(cx);
    let order = match group {
        GroupKey::Conversations => None,
        GroupKey::Project { machine, .. } => {
            let sidebar = sidebar(cx);
            let group_ids: Vec<String> = sidebar
                .group_rows(group)
                .iter()
                .map(|row| row.key.id.clone())
                .collect();
            let ui = &sessions.read(cx).ui;
            let moved = move_to_pin_boundary(&group_ids, &key.id, |other| {
                if other == key.id {
                    pinned
                } else {
                    ui.is_pinned(&Pin::Conversation(ConversationKey {
                        machine: machine.clone(),
                        id: other.to_string(),
                    }))
                }
            });
            sidebar
                .machine(machine)
                .map(|section| splice_group(&section.conversation_ids, &moved))
        }
    };
    sessions.update(cx, |sessions, cx| {
        sessions.set_pinned(Pin::Conversation(key.clone()), pinned, cx);
        if let Some(order) = order {
            sessions.set_conversation_order(&key.machine, order, cx);
        }
    });
}

pub(super) fn pin_workset(machine: &RuntimeKey, id: &str, pinned: bool, cx: &mut App) {
    let pin = Pin::Workset {
        machine: machine.clone(),
        id: id.to_string(),
    };
    Sessions::global(cx).update(cx, |sessions, cx| sessions.set_pinned(pin, pinned, cx));
}

// ---- 拖动 ----

/// 把项目 `from` 放到同一台机器上的 `to` 前面或后面。跨过置顶区和其余的交界就跟着
/// `to` 置顶或取消置顶。
pub(super) fn drop_project(machine: &RuntimeKey, from: &str, to: &str, before: bool, cx: &mut App) {
    let Some(mut roots) = sidebar(cx).machine(machine).map(|m| m.roots.clone()) else {
        return;
    };
    if !move_near(&mut roots, from, to, before) {
        return;
    }
    let sessions = Sessions::global(cx);
    let pin_of = |root: &str| Pin::Project {
        machine: machine.clone(),
        root: root.to_string(),
    };
    let pinned = sessions.read(cx).ui.is_pinned(&pin_of(to));
    sessions.update(cx, |sessions, cx| {
        sessions.set_pinned(pin_of(from), pinned, cx);
        sessions.set_project_order(machine, roots, cx);
    });
}

/// 把会话 `from` 放到同一组里 `to` 的前面或后面，规则同 [`drop_project`]。
pub(super) fn drop_session(
    from: &ConversationKey,
    to: &ConversationKey,
    group: &GroupKey,
    before: bool,
    cx: &mut App,
) {
    let GroupKey::Project { machine, .. } = group else {
        return;
    };
    let sidebar = sidebar(cx);
    let mut ids: Vec<String> = sidebar
        .group_rows(group)
        .iter()
        .map(|row| row.key.id.clone())
        .collect();
    if !move_near(&mut ids, &from.id, &to.id, before) {
        return;
    }
    let Some(order) = sidebar
        .machine(machine)
        .map(|section| splice_group(&section.conversation_ids, &ids))
    else {
        return;
    };
    let sessions = Sessions::global(cx);
    let pinned = sessions
        .read(cx)
        .ui
        .is_pinned(&Pin::Conversation(to.clone()));
    sessions.update(cx, |sessions, cx| {
        sessions.set_pinned(Pin::Conversation(from.clone()), pinned, cx);
        sessions.set_conversation_order(machine, order, cx);
    });
}

// ---- 关闭、归档 ----

/// 关掉一段会话。还在跑、在等人的先问一句。
pub(super) fn close_session(
    key: ConversationKey,
    title: SharedString,
    status: AgentStatus,
    window: &mut Window,
    cx: &mut App,
) {
    let close = move |cx: &mut App| {
        let closed = lifecycle::close_conversation(key.clone(), cx);
        crate::report_failure("关闭会话", closed, cx);
    };
    if status == AgentStatus::Idle {
        close(cx);
        return;
    }
    Confirm::new(format!("关闭「{title}」？"), "关闭", move |_, cx| {
        close(cx);
        true
    })
    .danger()
    .description("它还没停下，关掉会结束里面在跑的程序。")
    .open(window, cx);
}

/// 归档一段会话（去归档里还找得回来）。还在跑、在等人的先问一句。
pub(super) fn archive_session(
    key: ConversationKey,
    title: SharedString,
    status: AgentStatus,
    window: &mut Window,
    cx: &mut App,
) {
    let archive = move |cx: &mut App| {
        let archived = lifecycle::archive_conversation(key.clone(), cx);
        crate::report_failure("归档会话", archived, cx);
    };
    if status == AgentStatus::Idle {
        archive(cx);
        return;
    }
    Confirm::new(format!("归档「{title}」？"), "归档", move |_, cx| {
        archive(cx);
        true
    })
    .description("它还没停下，归档会结束里面在跑的程序。")
    .open(window, cx);
}

/// 关掉项目，连同下面的会话。里面有会话的先问一句。
pub(super) fn close_project(
    machine: RuntimeKey,
    root: String,
    name: SharedString,
    sessions: usize,
    window: &mut Window,
    cx: &mut App,
) {
    let close = move |cx: &mut App| {
        let closed = lifecycle::close_project(machine.clone(), root.clone(), cx);
        crate::report_failure("关闭项目", closed, cx);
    };
    if sessions == 0 {
        close(cx);
        return;
    }
    Confirm::new(
        format!("关闭项目「{name}」？"),
        "关闭",
        move |_, cx| {
            close(cx);
            true
        },
    )
    .danger()
    .description(format!("会一起关掉里面的 {sessions} 个会话。"))
    .open(window, cx);
}

/// 关掉工作树：运行时移除它的目录，下面的会话回到项目里。有没合进去的分支、删不掉的
/// 文件时说一声。
pub(super) fn finish_workset(
    machine: RuntimeKey,
    id: String,
    task: SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    Confirm::new(
        format!("关闭工作树「{task}」？"),
        "关闭",
        move |_, cx| {
            let params = WorkspaceWorksetFinishParams {
                workset_id: id.clone(),
            };
            let finished =
                api::call::<WorkspaceWorksetFinish>(&machine, params, api::CALL_TIMEOUT, cx);
            cx.spawn(async move |cx| match finished.await {
                Ok(result) => {
                    let mut notes = Vec::new();
                    if !result.retained_branches.is_empty() {
                        notes.push(format!(
                            "留下了分支：{}",
                            result.retained_branches.join("、")
                        ));
                    }
                    if !result.leftover_entries.is_empty() {
                        notes.push(format!(
                            "这些没删掉：{}",
                            result.leftover_entries.join("、")
                        ));
                    }
                    if !notes.is_empty() {
                        let notify = api::Notify {
                            subtitle: "工作树已关闭".to_string(),
                            body: notes.join("\n"),
                            ..Default::default()
                        };
                        cx.update(|cx| api::NOTIFY.run(&notify, cx).detach());
                    }
                }
                Err(error) => {
                    cx.update(|cx| crate::notify_error(&format!("关闭工作树失败：{error}"), cx))
                }
            })
            .detach();
            true
        },
    )
    .danger()
    .description("会移除工作树的目录；里面的会话回到项目里。")
    .open(window, cx);
}

// ---- 打开项目 ----

/// 在那台机器上挑一个目录打开成项目。本机是系统选择框，远程机器由宿主画浏览那台机器的框。
pub(super) fn pick_project(machine: RuntimeKey, cx: &mut App) {
    let picked = api::PICK_PATHS.run(
        &api::PathRequest {
            machine: machine.clone(),
            title: "打开项目".to_string(),
            confirm: "打开".to_string(),
            files: false,
            multiple: false,
        },
        cx,
    );
    cx.spawn(async move |cx| {
        let root = match picked.await {
            Ok(Some(paths)) => paths.into_iter().next(),
            Ok(None) => None,
            Err(error) => {
                cx.update(|cx| crate::notify_error(&format!("打开项目：{error}"), cx));
                None
            }
        };
        let Some(root) = root else {
            return;
        };
        cx.update(|cx| {
            let opened = lifecycle::open_project(machine, root, cx);
            crate::report_failure("打开项目", opened, cx);
        });
    })
    .detach();
}

/// 从访达拖进列表的路径：目录就是项目，文件取它所在的目录，去重后在本机逐个打开。看路径
/// 是什么要读文件系统，放后台做。一个能用的都没有时报给人看。
pub(super) fn open_dropped_paths(paths: Vec<PathBuf>, cx: &mut App) {
    let roots = cx.background_executor().spawn(async move {
        let mut roots: Vec<String> = Vec::new();
        for path in paths {
            let dir = match std::fs::metadata(&path) {
                Ok(meta) if meta.is_dir() => path,
                Ok(_) => match path.parent() {
                    Some(parent) => parent.to_path_buf(),
                    None => continue,
                },
                Err(_) => continue,
            };
            let Some(dir) = dir.to_str().map(str::to_string) else {
                continue;
            };
            if !roots.contains(&dir) {
                roots.push(dir);
            }
        }
        roots
    });
    cx.spawn(async move |cx| {
        let roots = roots.await;
        cx.update(|cx| {
            if roots.is_empty() {
                crate::notify_error("拖入的路径无法作为项目目录打开", cx);
                return;
            }
            for root in roots {
                let opened = lifecycle::open_project(RuntimeKey::Local, root, cx);
                crate::report_failure("打开项目", opened, cx);
            }
        });
    })
    .detach();
}
