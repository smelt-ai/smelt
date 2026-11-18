//! 列表上各种行的动作：置顶、拖动换位置、关闭、归档、关工作树、打开项目（挑目录、从访达
//! 拖进来）。菜单和行上的按钮都调这里。要确认的（会话还在跑、项目里还有会话、关工作树）在
//! 这里先问。

use rust_i18n::t;
use std::path::PathBuf;

use gpui::{App, SharedString, Window};
use smelt_plugin_api::model::agent_status::AgentStatus;
use smelt_plugin_api::model::control_api::WorkspaceWorksetFinish;
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceWorksetFinishParams, WorkspaceWorksetInspectResult,
};
use smelt_plugin_api::ui::dialog::Confirm;
use smelt_plugin_api::{self as api, RuntimeKey};

use super::{
    GroupKey, Sidebar, merge_project_order, move_near, move_to_pin_boundary, splice_group,
};
use crate::model::{ConversationKey, Pin, ProjectKey, Sessions};
use crate::worksets::{self, MergeSummary, WorksetKey};
use crate::{contract, lifecycle, peers};

fn sidebar(cx: &App) -> std::rc::Rc<Sidebar> {
    Sessions::global(cx).read(cx).sidebar.clone()
}

// ---- 置顶 ----

fn project_pin(key: &ProjectKey) -> Pin {
    Pin::Project {
        machine: key.machine.clone(),
        root: key.root.clone(),
    }
}

/// 置顶或取消置顶一个项目，并把它挪到置顶区和其余的交界（见 [`move_to_pin_boundary`]）。
/// 置顶区不分机器：所有机器的项目排在一份列表里。
pub(super) fn pin_project(machine: &RuntimeKey, root: &str, pinned: bool, cx: &mut App) {
    let item = ProjectKey {
        machine: machine.clone(),
        root: root.to_string(),
    };
    let shown = sidebar(cx).project_keys();
    let sessions = Sessions::global(cx);
    let order = {
        let ui = &sessions.read(cx).ui;
        let shown = move_to_pin_boundary(&shown, &item, |other| {
            if *other == item {
                pinned
            } else {
                ui.is_pinned(&project_pin(other))
            }
        });
        merge_project_order(&ui.project_order, shown)
    };
    sessions.update(cx, |sessions, cx| {
        sessions.set_pinned(project_pin(&item), pinned, cx);
        sessions.set_project_order(order, cx);
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
                if *other == key.id {
                    pinned
                } else {
                    ui.is_pinned(&Pin::Conversation(ConversationKey {
                        machine: machine.clone(),
                        id: other.clone(),
                    }))
                }
            });
            Some(splice_group(sidebar.conversation_ids(machine), &moved))
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

/// 把项目 `from` 放到 `to` 前面或后面，哪台机器上的都行（所有机器的项目排在一份列表里）。
/// 跨过置顶区和其余的交界就跟着 `to` 置顶或取消置顶。
pub(super) fn drop_project(from: &ProjectKey, to: &ProjectKey, before: bool, cx: &mut App) {
    let mut shown = sidebar(cx).project_keys();
    if !move_near(&mut shown, from, to, before) {
        return;
    }
    let sessions = Sessions::global(cx);
    let (pinned, order) = {
        let ui = &sessions.read(cx).ui;
        (
            ui.is_pinned(&project_pin(to)),
            merge_project_order(&ui.project_order, shown),
        )
    };
    sessions.update(cx, |sessions, cx| {
        sessions.set_pinned(project_pin(from), pinned, cx);
        sessions.set_project_order(order, cx);
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
    let order = splice_group(sidebar.conversation_ids(machine), &ids);
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
        crate::report_failure(t!("action.close_session"), closed, cx);
    };
    if status == AgentStatus::Idle {
        close(cx);
        return;
    }
    Confirm::new(
        t!("close.title", title = title),
        t!("close.confirm"),
        move |_, cx| {
            close(cx);
            true
        },
    )
    .danger()
    .description(t!("close.running_hint"))
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
        crate::report_failure(t!("action.archive_session"), archived, cx);
    };
    if status == AgentStatus::Idle {
        archive(cx);
        return;
    }
    Confirm::new(
        t!("archive.title", title = title),
        t!("archive.confirm"),
        move |_, cx| {
            archive(cx);
            true
        },
    )
    .description(t!("archive.running_hint"))
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
        crate::report_failure(t!("action.close_project"), closed, cx);
    };
    if sessions == 0 {
        close(cx);
        return;
    }
    Confirm::new(
        t!("close_project.title", name = name),
        t!("close.confirm"),
        move |_, cx| {
            close(cx);
            true
        },
    )
    .danger()
    .description(if sessions == 1 {
        t!("close_project.sessions.one")
    } else {
        t!("close_project.sessions.other", count = sessions)
    })
    .open(window, cx);
}

/// 关掉工作树：先问运行时各仓库的情况（会 fetch 基线），再按结果确认。都合进去了是普通的
/// 确认；有判断不了是否已合并的，说清楚哪个仓库、让人决定要不要仍然关闭；有未提交改动、
/// 确实没合并的直接说为什么关不了。运行时移除目录，下面的会话回到项目里。
pub(super) fn finish_workset(
    machine: RuntimeKey,
    id: String,
    task: SharedString,
    window: &mut Window,
    cx: &mut App,
) {
    let handle = window.window_handle();
    let key = WorksetKey {
        machine: machine.clone(),
        id: id.clone(),
    };
    let inspected = worksets::inspect(key, cx);
    cx.spawn(async move |cx| {
        let inspected = inspected.await;
        let shown = handle.update(cx, |_, window, cx| {
            confirm_finish(machine, id, task, inspected, window, cx)
        });
        if shown.is_err() {
            cx.update(|cx| crate::notify_error(&t!("workset.window_closed"), cx));
        }
    })
    .detach();
}

fn confirm_finish(
    machine: RuntimeKey,
    id: String,
    task: SharedString,
    inspected: Result<WorkspaceWorksetInspectResult, String>,
    window: &mut Window,
    cx: &mut App,
) {
    let moves = t!("workset.finish_moves");
    // 查不了（运行时还没有这条命令、Git 登记对不上……）：照旧问一句，关的时候运行时会再查。
    let (summary, lines) = match &inspected {
        Ok(result) => (
            MergeSummary::of_result(result),
            worksets::describe(result).join("\n"),
        ),
        Err(_) => (MergeSummary::Unknown, String::new()),
    };
    let finish = move |allow_unconfirmed_merge: bool| {
        move |_: &mut Window, cx: &mut App| {
            finish_now(machine.clone(), id.clone(), allow_unconfirmed_merge, cx);
            true
        }
    };
    match summary {
        MergeSummary::Pending => {
            crate::notify_error(&t!("workset.cannot_finish", task = task, lines = lines), cx);
        }
        MergeSummary::Unconfirmed => {
            Confirm::new(
                t!("workset.finish_anyway_title", task = task),
                t!("workset.finish_anyway"),
                finish(true),
            )
            .danger()
            .description(format!(
                "{lines}\n\n{} {moves}",
                t!("workset.unconfirmed_hint")
            ))
            .open(window, cx);
        }
        MergeSummary::Merged | MergeSummary::Unknown => {
            let description = if lines.is_empty() {
                moves.into_owned()
            } else {
                format!("{lines}\n\n{moves}")
            };
            Confirm::new(
                t!("workset.finish_title", task = task),
                t!("close.confirm"),
                finish(false),
            )
            .danger()
            .description(description)
            .open(window, cx);
        }
    }
}

fn finish_now(machine: RuntimeKey, id: String, allow_unconfirmed_merge: bool, cx: &mut App) {
    let params = WorkspaceWorksetFinishParams {
        workset_id: id,
        allow_unconfirmed_merge,
    };
    let finished =
        api::call::<WorkspaceWorksetFinish>(&machine, params, worksets::WORKSET_GIT_TIMEOUT, cx);
    cx.spawn(async move |cx| match finished.await {
        Ok(result) => {
            let mut notes = Vec::new();
            if !result.retained_branches.is_empty() {
                notes.push(
                    t!(
                        "workset.retained_branches",
                        branches = result.retained_branches.join(&*t!("list.separator"))
                    )
                    .into_owned(),
                );
            }
            if !result.leftover_entries.is_empty() {
                notes.push(
                    t!(
                        "workset.leftover_entries",
                        entries = result.leftover_entries.join(&*t!("list.separator"))
                    )
                    .into_owned(),
                );
            }
            if !notes.is_empty() {
                let notify = api::Notify {
                    subtitle: t!("workset.finished").into_owned(),
                    body: notes.join("\n"),
                    ..Default::default()
                };
                cx.update(|cx| api::NOTIFY.run(&notify, cx).detach());
            }
        }
        Err(error) => cx.update(|cx| {
            crate::notify_error(
                &t!(
                    "error.failed",
                    what = t!("action.finish_workset"),
                    error = error
                ),
                cx,
            )
        }),
    })
    .detach();
}

/// 菜单里的「检查合并状态」：马上问一次，结果画在工作树行上；查失败了说一声。
pub(super) fn check_workset(key: WorksetKey, cx: &mut App) {
    let checked = worksets::inspect(key, cx);
    cx.spawn(async move |cx| {
        if let Err(error) = checked.await {
            cx.update(|cx| {
                crate::notify_error(
                    &t!(
                        "error.failed",
                        what = t!("action.check_merge"),
                        error = error
                    ),
                    cx,
                )
            });
        }
    })
    .detach();
}

/// 工作树里某个仓库的新建 PR 页，交给变更插件开（它认得远端是哪家托管）。
pub(super) fn open_pull_request(machine: RuntimeKey, root: String, cx: &mut App) {
    let opened =
        peers::git::OPEN_PULL_REQUEST.run(&peers::git::OpenPullRequest { machine, root }, cx);
    cx.spawn(async move |cx| {
        if let Err(error) = opened.await {
            cx.update(|cx| {
                crate::notify_error(
                    &t!("error.failed", what = t!("action.create_pr"), error = error),
                    cx,
                )
            });
        }
    })
    .detach();
}

// ---- 打开项目 ----

/// 在那台机器上打开项目：本机走「打开项目…」命令（系统选择框，能多选），远程机器在宿主画的
/// 浏览框里挑一个目录。
pub(super) fn open_project(machine: RuntimeKey, cx: &mut App) {
    if matches!(machine, RuntimeKey::Local) {
        let opened = contract::OPEN_PROJECT.run(&(), cx);
        crate::report_failure(t!("action.add_project"), opened, cx);
    } else {
        pick_project(machine, cx);
    }
}

/// 在远程机器上挑一个目录打开成项目，浏览框由宿主画。
fn pick_project(machine: RuntimeKey, cx: &mut App) {
    let picked = api::PICK_PATHS.run(
        &api::PathRequest {
            machine: machine.clone(),
            title: t!("pick.title").into_owned(),
            confirm: t!("pick.confirm").into_owned(),
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
                cx.update(|cx| {
                    crate::notify_error(
                        &t!(
                            "error.failed",
                            what = t!("action.open_project"),
                            error = error
                        ),
                        cx,
                    )
                });
                None
            }
        };
        let Some(root) = root else {
            return;
        };
        cx.update(|cx| {
            let opened = lifecycle::open_project(machine, root, cx);
            crate::report_failure(t!("action.add_project"), opened, cx);
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
                crate::notify_error(&t!("error.drop_not_folder"), cx);
                return;
            }
            for root in roots {
                let opened = lifecycle::open_project(RuntimeKey::Local, root, cx);
                crate::report_failure(t!("action.add_project"), opened, cx);
            }
        });
    })
    .detach();
}
