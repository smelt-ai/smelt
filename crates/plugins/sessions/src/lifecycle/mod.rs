//! 会话的生命周期（S3）：跟着工作区目录维护选中，写新会话进目录，发布活动上下文。
//!
//! 这里只管数据，不碰窗口：建好的会话（分屏、视图实例）归会话视图，它在自己的窗口里跟着
//! 选中和目录建、扔、给焦点。人要去某段会话一律是打开会话视图（[`open_session`]）。
//!
//! 全靠事件走，不起定时器、不重试轮询：
//! - `HostEvent::SessionsChanged`（目录或会话状态变了）：那台机器的目录版本变了才动目录
//!   那部分——`Sessions::prune` 清库；选中的会话没了（或归档了、成了自动化运行）就清掉
//!   选中，选中的项目在目录推到打开它那一版之后还不在侧栏也清掉。然后 [`publish_context`]。
//! - `HostEvent::MachinesChanged`：机器表里真的消失了的（和上一份比，不是离线）调
//!   `Sessions::forget_machine`，清掉它上面的选中。
//! - `peers::conversation::AT_LATEST_CHANGED`：记下，[`publish_context`]。
//! - `HostEvent::ShownViewsChanged`：会话视图露出来、被挡住，[`publish_context`]。
//!
//! 选中一变就发 `contract::SELECTION_CHANGED`（[`announce_selection`]），会话列表跟着画。
//!
//! 宿主每台机器只留最新的一份目录，所以这里不用防旧推送：目录里没了就是真没了。

use rust_i18n::t;
use std::collections::{HashMap, HashSet};

use gpui::{App, Entity, Subscription, Task};
use smelt_plugin_api::model::control_api::{
    WorkspaceConversationCreate, WorkspaceConversationDelete, WorkspaceConversationUpdate,
};
use smelt_plugin_api::model::runtime_path::PathStyle;
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceConversation, WorkspaceConversationIdParams, WorkspaceDirectory, WorkspaceRevision,
};
use smelt_plugin_api::{self as api, ActiveContext, HostEvent, RuntimeKey, ViewedSession};

use crate::contract::{self, ConversationRef, SessionArgs, SessionAt};
use crate::model::{self, ConversationKey, Placement, Selection, Sessions};
use crate::peers::conversation::{AT_LATEST_CHANGED, AtLatest, ConversationStart};
use crate::session_view::{Pane, PaneKind};

/// 生命周期自己的状态，放在 `Sessions::lifecycle` 里。
#[derive(Default)]
pub(crate) struct State {
    /// 上一份机器表：机器表变了时和它比，认出真的移除了的。
    machines: HashSet<RuntimeKey>,
    /// 各台机器上次处理过的目录版本。会话状态变了也发 `SessionsChanged`，目录没变就不
    /// 重清库、不重同步。
    seen: HashMap<RuntimeKey, WorkspaceRevision>,
    /// 往上翻着、不在最新处的对话（机器 + 守护会话 id）。不在里面的算在最新处，跟对话
    /// 实例刚建好时一样。
    behind: HashSet<(RuntimeKey, String)>,
    _subscriptions: Vec<Subscription>,
}

/// 订阅宿主事件，接上上面那些处理。`register` 里建好 `Sessions` 之后调一次。
pub(crate) fn install(sessions: &Entity<Sessions>, cx: &mut App) {
    let machines = api::host(cx)
        .machines(cx)
        .into_iter()
        .map(|machine| machine.key)
        .collect();
    let events = api::events(cx);
    let subscription = cx.subscribe(&events, |_, event: &HostEvent, cx| on_event(event, cx));
    sessions.update(cx, |sessions, _| {
        sessions.lifecycle.machines = machines;
        sessions.lifecycle._subscriptions.push(subscription);
    });
    // 宿主可能在本插件登记之前就收到了目录：先按眼下的走一遍。
    on_sessions_changed(cx);
}

fn on_event(event: &HostEvent, cx: &mut App) {
    match event {
        HostEvent::SessionsChanged => on_sessions_changed(cx),
        HostEvent::MachinesChanged => on_machines_changed(cx),
        HostEvent::ShownViewsChanged => publish_context(cx),
        HostEvent::Plugin { .. } => {
            if let Some(at) = AT_LATEST_CHANGED.parse(event) {
                on_at_latest(at, cx);
            }
        }
        _ => {}
    }
}

fn on_sessions_changed(cx: &mut App) {
    let host = api::host(cx);
    let sessions = Sessions::global(cx);
    let mut cleared = false;
    for machine in host.machines(cx) {
        let machine = machine.key;
        let Some(directory) = host.workspace_directory(&machine, cx) else {
            continue;
        };
        let style = host.path_style(&machine, cx);
        cleared |= sessions.update(cx, |sessions, cx| {
            let version = directory.version();
            if sessions.lifecycle.seen.get(&machine) == Some(&version) {
                return false;
            }
            sessions.lifecycle.seen.insert(machine.clone(), version);
            sessions.prune(&machine, &directory);
            let gone = match &sessions.selection {
                Selection::None => false,
                Selection::Session(key) => {
                    key.machine == machine
                        && model::shown_conversation(style, &directory, &key.id).is_none()
                }
                Selection::Project {
                    machine: on,
                    root,
                    since,
                } => {
                    let caught_up = since
                        .as_ref()
                        .is_none_or(|since| !directory.version().is_older_than(since));
                    *on == machine && caught_up && !project_listed(style, &directory, root)
                }
            };
            if gone {
                if sessions.selection.session().is_some() {
                    sessions.store.save_selection(None);
                }
                sessions.selection = Selection::None;
                cx.notify();
            }
            let session_ids: HashSet<&str> = directory
                .conversations
                .iter()
                .flat_map(WorkspaceConversation::session_ids)
                .collect();
            sessions
                .lifecycle
                .behind
                .retain(|(on, id)| *on != machine || session_ids.contains(id.as_str()));
            gone
        });
    }
    if cleared {
        announce_selection(cx);
    }
    publish_context(cx);
}

fn on_machines_changed(cx: &mut App) {
    let current: HashSet<RuntimeKey> = api::host(cx)
        .machines(cx)
        .into_iter()
        .map(|machine| machine.key)
        .collect();
    let cleared = Sessions::global(cx).update(cx, |sessions, cx| {
        let mut cleared = false;
        let gone: Vec<RuntimeKey> = sessions
            .lifecycle
            .machines
            .difference(&current)
            .cloned()
            .collect();
        for machine in &gone {
            // 库里它的选中由 `forget_machine` 一起删。
            sessions.forget_machine(machine);
            sessions.starts.retain(|key, _| key.machine != *machine);
            if sessions.selection.on(machine) {
                sessions.selection = Selection::None;
                cleared = true;
                cx.notify();
            }
            sessions.lifecycle.seen.remove(machine);
            sessions.lifecycle.behind.retain(|(on, _)| on != machine);
        }
        sessions.lifecycle.machines = current;
        cleared
    });
    if cleared {
        announce_selection(cx);
    }
    publish_context(cx);
}

fn on_at_latest(at: AtLatest, cx: &mut App) {
    let changed = Sessions::global(cx).update(cx, |sessions, _| {
        let key = (at.machine, at.session_id);
        if at.at_latest {
            sessions.lifecycle.behind.remove(&key)
        } else {
            sessions.lifecycle.behind.insert(key)
        }
    });
    if changed {
        publish_context(cx);
    }
}

/// 选中一段会话：存库、通知重画、[`announce_selection`]、[`publish_context`]。不建会话、
/// 不管会话视图露没露、不给焦点——那些归会话视图，它打开到这段会话时调这里（目录里找到
/// 了、在侧栏里才调）。已经选中的也发一次：选中后用户可能把它所在的项目折了，再从通知
/// 跳回来，列表要把它露出来。
pub(crate) fn select(key: ConversationKey, cx: &mut App) {
    Sessions::global(cx).update(cx, |sessions, cx| {
        if sessions.selection.session() == Some(&key) {
            return;
        }
        sessions.store.save_selection(Some(&key));
        sessions.selection = Selection::Session(key);
        cx.notify();
    });
    announce_selection(cx);
    publish_context(cx);
}

/// 告诉会话列表眼下选中的是什么（`contract::SELECTION_CHANGED`）。选中改了之后调。
fn announce_selection(cx: &mut App) {
    let selected = Sessions::global(cx).read(cx).selection.to_selected();
    contract::SELECTION_CHANGED.emit(&selected, cx);
}

/// 选中一个项目（`contract::SELECT_PROJECT`）：里面还没有会话时，文件、Git 这些也按它看。
/// 选中的会话跟着清掉（库里那行一起），项目只在内存里，不存库。`since` 是刚打开它时写进
/// 目录的版本，见 `Selection::Project`。目录推到那一版之后还不在侧栏是失败。
pub(crate) fn select_project(
    machine: RuntimeKey,
    root: String,
    since: Option<WorkspaceRevision>,
    cx: &mut App,
) -> Result<(), String> {
    let host = api::host(cx);
    let style = host.path_style(&machine, cx);
    let root = style.clean(root.trim());
    if root.is_empty() {
        return Err(t!("error.no_project_dir").into_owned());
    }
    let directory = host.workspace_directory(&machine, cx);
    let caught_up = match (&directory, &since) {
        (None, _) => since.is_none(),
        (Some(directory), Some(since)) => !directory.version().is_older_than(since),
        (Some(_), None) => true,
    };
    let listed = directory
        .as_ref()
        .is_some_and(|directory| project_listed(style, directory, &root));
    if caught_up && !listed {
        return Err(t!("error.project_not_listed").into_owned());
    }
    Sessions::global(cx).update(cx, |sessions, cx| {
        if sessions.selection.session().is_some() {
            sessions.store.save_selection(None);
        }
        sessions.selection = Selection::Project {
            machine,
            root,
            since,
        };
        cx.notify();
    });
    announce_selection(cx);
    publish_context(cx);
    Ok(())
}

/// 项目 `root`（已经按 `style` 规整过）在侧栏里还有行：打开着，或有会话按工作目录撑着它。
fn project_listed(style: PathStyle, directory: &WorkspaceDirectory, root: &str) -> bool {
    model::listed_roots(style, directory)
        .iter()
        .any(|listed| style.clean(listed) == root)
}

/// 在主窗口里露出一段会话、焦点给它：主窗口拿到前面（关着就先开），再打开会话视图
/// （`OPEN_VIEW`，参数见 [`SessionArgs`]）。人要去某段会话——点列表、上一段 / 下一段、
/// `ACTIVATE`、新建之后——一律走这里；找不到、要等目录推到都归会话视图。
pub(crate) fn open_session(args: SessionArgs, cx: &mut App) -> Task<Result<(), String>> {
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
                    view: contract::SESSION.to_string(),
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

/// 打开会话视图露出目录里的这一条。
pub(crate) fn open_conversation(key: ConversationKey, cx: &mut App) -> Task<Result<(), String>> {
    open_session(
        SessionArgs {
            machine: key.machine,
            at: SessionAt::Conversation {
                conversation_id: key.id,
            },
            after: None,
        },
        cx,
    )
}

/// 按眼下的选中、它那一格（`Sessions::active_pane`）、翻没翻在最新处、会话视图露没
/// 露着（`Host::view_shown(contract::SESSION)`），算出活动上下文发布
/// （`smelt_plugin_api::set_active_context`，没变它自己不发）。这几样任何一样变了都调；
/// 只在事件里调，不在 `Sessions` 的更新里调（要读它）。
pub(crate) fn publish_context(cx: &mut App) {
    let context = current_context(cx);
    api::set_active_context(context, cx);
}

fn current_context(cx: &App) -> ActiveContext {
    let host = api::host(cx);
    let sessions = Sessions::global(cx).read(cx);
    let key = match &sessions.selection {
        Selection::None => return ActiveContext::default(),
        Selection::Project { machine, root, .. } => {
            return ActiveContext {
                project: Some(api::Project {
                    machine: machine.clone(),
                    root: root.clone(),
                }),
                session: None,
            };
        }
        Selection::Session(key) => key,
    };
    let Some(directory) = host.workspace_directory(&key.machine, cx) else {
        return ActiveContext::default();
    };
    let Some(conversation) = directory
        .conversations
        .iter()
        .find(|conversation| conversation.id == key.id)
    else {
        return ActiveContext::default();
    };
    let style = host.path_style(&key.machine, cx);
    let project = context_root(style, &directory, conversation).map(|root| api::Project {
        machine: key.machine.clone(),
        root,
    });
    let session = host
        .view_shown(contract::SESSION, cx)
        .then(|| {
            sessions
                .active_pane
                .as_ref()
                .filter(|(on, _)| on == key)
                .map(|(_, pane)| pane.clone())
                .or_else(|| lead_pane(conversation))
        })
        .flatten()
        .map(|pane| ViewedSession {
            machine: key.machine.clone(),
            at_latest: pane.kind == PaneKind::Terminal
                || !sessions
                    .lifecycle
                    .behind
                    .contains(&(key.machine.clone(), pane.session_id.clone())),
            id: pane.session_id,
        });
    ActiveContext { project, session }
}

/// 选中的会话算在哪个项目里（文件、Git 这些看它）：绑了工作集的是工作集的目录，在项目
/// 里的是项目根，「对话」那段的是对话自己的工作目录；没有工作目录的不算项目。
fn context_root(
    style: PathStyle,
    directory: &WorkspaceDirectory,
    conversation: &WorkspaceConversation,
) -> Option<String> {
    match model::placement(style, directory, conversation) {
        Placement::Project {
            workset: Some(id), ..
        } => directory
            .worksets
            .iter()
            .find(|workset| workset.id == id)
            .map(|workset| workset.task_root(style)),
        Placement::Project {
            root,
            workset: None,
        } => (!root.is_empty()).then_some(root),
        Placement::Conversations => model::cwd_of(conversation).map(str::to_string),
        Placement::Hidden => None,
    }
}

/// 会话还没建（或还没摆上格子）时当它露着的那一格：有对话是对话，否则第一格终端。
fn lead_pane(conversation: &WorkspaceConversation) -> Option<Pane> {
    if let Some(acp) = &conversation.acp {
        return Some(Pane {
            session_id: acp.sid.clone(),
            kind: PaneKind::Conversation,
        });
    }
    conversation
        .terminals
        .iter()
        .find_map(|terminal| terminal.id.clone())
        .map(|session_id| Pane {
            session_id,
            kind: PaneKind::Terminal,
        })
}

// ---- 目录操作。结果由目录推回来，这里不改本地状态（`Sessions::starts` 除外） ----

/// 把一段新会话写进那台机器的目录，然后 [`open_session`] 过去（带上写进去的目录版本，
/// 目录推到之前会话视图等着）。对话的 `start` 记进 `Sessions::starts`，建视图时带上；写
/// 失败了拿掉。写之前发 `contract::CONVERSATION_CREATED`，列表认得出它是刚建的。写不进去、
/// 打不开都是失败。
pub(crate) fn create_conversation(
    machine: RuntimeKey,
    conversation: WorkspaceConversation,
    start: Option<ConversationStart>,
    cx: &mut App,
) -> Task<Result<ConversationKey, String>> {
    let key = ConversationKey {
        machine: machine.clone(),
        id: conversation.id.clone(),
    };
    // 先记再写：目录可能在调用回来之前就推到，那时有人选它也要带上；列表也要认得出它是
    // 刚建的。
    Sessions::global(cx).update(cx, |sessions, _| {
        if let Some(start) = start {
            sessions.starts.insert(key.clone(), start);
        }
    });
    contract::CONVERSATION_CREATED.emit(
        &ConversationRef {
            machine: key.machine.clone(),
            conversation_id: key.id.clone(),
        },
        cx,
    );
    let created =
        api::call::<WorkspaceConversationCreate>(&machine, conversation, api::CALL_TIMEOUT, cx);
    cx.spawn(async move |cx| {
        let revision = match created.await {
            Ok(revision) => revision,
            Err(error) => {
                cx.update(|cx| {
                    Sessions::global(cx).update(cx, |sessions, _| sessions.starts.remove(&key))
                });
                return Err(error);
            }
        };
        let opened = cx.update(|cx| {
            open_session(
                SessionArgs {
                    machine: key.machine.clone(),
                    at: SessionAt::Conversation {
                        conversation_id: key.id.clone(),
                    },
                    after: Some(revision),
                },
                cx,
            )
        });
        opened.await?;
        Ok(key)
    })
}

/// 整条替换一段会话（改名、加减终端）。
pub(crate) fn update_conversation(
    machine: RuntimeKey,
    conversation: WorkspaceConversation,
    cx: &mut App,
) -> Task<Result<(), String>> {
    let updated =
        api::call::<WorkspaceConversationUpdate>(&machine, conversation, api::CALL_TIMEOUT, cx);
    cx.spawn(async move |_| updated.await.map(|_| ()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_plugin_api::model::workspace_directory::{
        WorkspaceAcp, WorkspaceTerminal, WorkspaceWorkset,
    };

    fn terminals(id: &str, cwd: &str, ids: &[&str]) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            terminals: ids
                .iter()
                .map(|terminal| WorkspaceTerminal {
                    id: Some((*terminal).into()),
                    cwd: Some(cwd.into()),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn agent(id: &str, sid: &str, cwd: &str) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            acp: Some(WorkspaceAcp {
                sid: sid.into(),
                cwd: Some(cwd.into()),
                agent_definition_id: Some("writer".into()),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn directory() -> WorkspaceDirectory {
        let mut in_workset = terminals("w", "/elsewhere", &["t0"]);
        in_workset.workset_id = Some("ws".into());
        let mut archived = terminals("old", "/repo", &["t9"]);
        archived.archived_at = Some(1);
        WorkspaceDirectory {
            projects: vec!["/repo".into()],
            worksets: vec![WorkspaceWorkset {
                id: "ws".into(),
                project_root: "/repo".into(),
                task: "fix".into(),
                ..Default::default()
            }],
            conversations: vec![
                in_workset,
                terminals("p", "/repo/sub", &["t1", "t2"]),
                agent("a", "a1", "/home/u/.smelt/agents/writer"),
                terminals("l", "/tmp/x", &["t3"]),
                terminals("n", "", &[]),
                archived,
            ],
            ..Default::default()
        }
    }

    #[test]
    fn context_root_follows_where_the_session_sits() {
        let style = PathStyle::Posix;
        let directory = directory();
        let root = |id: &str| {
            let conversation = directory
                .conversations
                .iter()
                .find(|conversation| conversation.id == id)
                .unwrap();
            context_root(style, &directory, conversation)
        };
        assert_eq!(root("w").as_deref(), Some("/repo/worktrees/fix"));
        assert_eq!(root("p").as_deref(), Some("/repo"));
        assert_eq!(root("a").as_deref(), Some("/home/u/.smelt/agents/writer"));
        assert_eq!(root("l").as_deref(), Some("/tmp/x"));
        assert_eq!(root("n"), None);
        assert_eq!(root("old"), None);
    }

    #[test]
    fn lead_pane_prefers_the_conversation_then_the_first_terminal() {
        let directory = directory();
        assert_eq!(
            lead_pane(&directory.conversations[2]),
            Some(Pane {
                session_id: "a1".into(),
                kind: PaneKind::Conversation,
            })
        );
        assert_eq!(
            lead_pane(&directory.conversations[1]),
            Some(Pane {
                session_id: "t1".into(),
                kind: PaneKind::Terminal,
            })
        );
        assert_eq!(lead_pane(&directory.conversations[4]), None);
    }
}
