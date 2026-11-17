//! 会话的生命周期（S3）：跟着工作区目录维护选中，发目录操作，发布活动上下文。旧宿主
//! `workspace_sessions.rs`、`runtime_space.rs`、`workspace_persist.rs` 里会话那部分、
//! `workspace_attention.rs` 搬到这里（本目录下的文件都归 S3）。
//!
//! 这里只管数据，不碰窗口：建好的会话（分屏、视图实例）归会话视图，它在自己的窗口里跟着
//! 选中和目录建、扔、给焦点。人要去某段会话一律是打开会话视图（[`open_session`]）。
//!
//! 全靠事件走，不起定时器、不重试轮询：
//! - `HostEvent::SessionsChanged`（目录或会话状态变了）：那台机器的目录版本变了才动目录
//!   那部分——`Sessions::prune` 清库；选中的会话没了（或归档了、成了自动化运行）就清掉
//!   选中，选中的项目在目录推到打开它那一版之后还不在侧栏也清掉。然后
//!   `Sessions::rebuild_sidebar`、[`publish_context`]。
//! - `HostEvent::MachinesChanged`：机器表里真的消失了的（和上一份比，不是离线）调
//!   `Sessions::forget_machine`，清掉它上面的选中；重算侧栏。
//! - `peers::conversation::AT_LATEST_CHANGED`：记下，[`publish_context`]。
//! - `peers::agents::AGENT_SETTINGS_CHANGED`：换掉那台机器的 `Sessions::agent_names`，重算
//!   侧栏。起来时先经 `AGENT_SETTINGS` 拉一份整的（智能体插件要先登记）。
//! - `HostEvent::ShownViewsChanged`：会话视图露出来、被挡住，[`publish_context`]。
//! - `HostEvent::OpenPaths`（从访达拖进来的）：本机上是目录的 [`open_project`]。
//!
//! 宿主每台机器只留最新的一份目录，所以这里不用防旧推送：目录里没了就是真没了。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use gpui::{App, Entity, Subscription, Task};
use smelt_plugin_api::model::control_api::{
    WorkspaceConversationArchive, WorkspaceConversationCreate, WorkspaceConversationDelete,
    WorkspaceConversationUpdate, WorkspaceProjectClose, WorkspaceProjectOpen,
};
use smelt_plugin_api::model::runtime_path::PathStyle;
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceConversation, WorkspaceConversationArchiveParams, WorkspaceConversationIdParams,
    WorkspaceDirectory, WorkspaceProjectParams, WorkspaceRevision,
};
use smelt_plugin_api::{self as api, ActiveContext, HostEvent, RuntimeKey, ViewedSession};

use crate::contract::{self, SessionArgs, SessionAt};
use crate::model::{self, ConversationKey, Placement, Selection, Sessions};
use crate::peers::agents::{
    AGENT_SETTINGS, AGENT_SETTINGS_CHANGED, AgentSettingsState, MachineAgentSettings,
};
use crate::peers::conversation::{AT_LATEST_CHANGED, AtLatest, ConversationStart};
use crate::session_view::{Pane, PaneKind};
use crate::{notify_error, report_failure};

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
    // 智能体配置同理：拉一份眼下的，之后跟着事件走。
    let settings = AGENT_SETTINGS.run(&(), cx);
    cx.spawn(async move |cx| match settings.await {
        Ok(all) => cx.update(|cx| {
            for machine in all {
                on_agent_settings(machine, cx);
            }
        }),
        Err(error) => cx.update(|cx| notify_error(&format!("读智能体配置：{error}"), cx)),
    })
    .detach();
}

fn on_event(event: &HostEvent, cx: &mut App) {
    match event {
        HostEvent::SessionsChanged => on_sessions_changed(cx),
        HostEvent::MachinesChanged => on_machines_changed(cx),
        HostEvent::ShownViewsChanged => publish_context(cx),
        HostEvent::OpenPaths { machine, paths } => open_paths(machine.clone(), paths.clone(), cx),
        HostEvent::Plugin { .. } => {
            if let Some(at) = AT_LATEST_CHANGED.parse(event) {
                on_at_latest(at, cx);
            } else if let Some(changed) = AGENT_SETTINGS_CHANGED.parse(event) {
                on_agent_settings(changed, cx);
            }
        }
        _ => {}
    }
}

fn on_sessions_changed(cx: &mut App) {
    let host = api::host(cx);
    let sessions = Sessions::global(cx);
    for machine in host.machines(cx) {
        let machine = machine.key;
        let Some(directory) = host.workspace_directory(&machine, cx) else {
            continue;
        };
        let style = host.path_style(&machine, cx);
        sessions.update(cx, |sessions, _| {
            let version = directory.version();
            if sessions.lifecycle.seen.get(&machine) == Some(&version) {
                return;
            }
            sessions.prune(&machine, style, &directory);
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
            }
            let session_ids: HashSet<&str> = directory
                .conversations
                .iter()
                .flat_map(model::session_ids)
                .collect();
            sessions
                .lifecycle
                .behind
                .retain(|(on, id)| *on != machine || session_ids.contains(id.as_str()));
        });
    }
    sessions.update(cx, |sessions, cx| sessions.rebuild_sidebar(cx));
    publish_context(cx);
}

fn on_machines_changed(cx: &mut App) {
    let current: HashSet<RuntimeKey> = api::host(cx)
        .machines(cx)
        .into_iter()
        .map(|machine| machine.key)
        .collect();
    Sessions::global(cx).update(cx, |sessions, cx| {
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
            }
            sessions.lifecycle.seen.remove(machine);
            sessions.lifecycle.behind.retain(|(on, _)| on != machine);
        }
        sessions.lifecycle.machines = current;
        // 离线、改名也会走到这里：机器行要跟着变。
        sessions.rebuild_sidebar(cx);
    });
    publish_context(cx);
}

/// 换掉一台机器的智能体名字。给不了、移除了的那台就没有名字，会话行不标。
fn on_agent_settings(changed: MachineAgentSettings, cx: &mut App) {
    let names = match changed.state {
        AgentSettingsState::Ready { settings } => Some(
            settings
                .agents
                .into_iter()
                .map(|agent| (agent.id, agent.name.trim().to_string()))
                .collect::<HashMap<_, _>>(),
        ),
        AgentSettingsState::Unavailable { .. } | AgentSettingsState::Absent => None,
    };
    Sessions::global(cx).update(cx, |sessions, cx| {
        let unchanged = match &names {
            Some(names) => sessions.agent_names.get(&changed.machine) == Some(names),
            None => !sessions.agent_names.contains_key(&changed.machine),
        };
        if unchanged {
            return;
        }
        match names {
            Some(names) => sessions.agent_names.insert(changed.machine, names),
            None => sessions.agent_names.remove(&changed.machine),
        };
        sessions.rebuild_sidebar(cx);
    });
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

/// 选中一段会话：存库、通知重画、[`publish_context`]。只改选中，不建会话、不管会话视图
/// 露没露、不给焦点——那些归会话视图，它打开到这段会话时调这里（目录里找到了、在侧栏里
/// 才调）。
pub(crate) fn select(key: ConversationKey, cx: &mut App) {
    Sessions::global(cx).update(cx, |sessions, cx| {
        if sessions.selection.session() == Some(&key) {
            return;
        }
        sessions.store.save_selection(Some(&key));
        sessions.selection = Selection::Session(key);
        cx.notify();
    });
    publish_context(cx);
}

/// 选中一个项目：里面还没有会话时，文件、Git 这些也按它看。选中的会话跟着清掉（库里那行
/// 一起），项目只在内存里，不存库。S1 点项目行时调。
pub(crate) fn select_project(machine: RuntimeKey, root: String, cx: &mut App) {
    select_project_since(machine, root, None, cx);
}

/// 同 [`select_project`]；`since` 是刚打开它时写进目录的版本，见 `Selection::Project`。
fn select_project_since(
    machine: RuntimeKey,
    root: String,
    since: Option<WorkspaceRevision>,
    cx: &mut App,
) {
    let root = api::host(cx).path_style(&machine, cx).clean(root.trim());
    if root.is_empty() {
        return;
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
        sessions.rebuild_sidebar(cx);
    });
    publish_context(cx);
}

/// 项目 `root`（已经按 `style` 规整过）在侧栏里还有行：打开着，或有会话按工作目录撑着它。
fn project_listed(style: PathStyle, directory: &WorkspaceDirectory, root: &str) -> bool {
    model::known_ids(style, directory)
        .roots
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
            Err("会话视图放不下".to_string())
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
/// 失败了拿掉。
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
    // 先记再写：目录可能在调用回来之前就推到，那时有人选它也要带上。
    if let Some(start) = start {
        Sessions::global(cx).update(cx, |sessions, _| {
            sessions.starts.insert(key.clone(), start);
        });
    }
    let created =
        api::call::<WorkspaceConversationCreate>(&machine, conversation, api::CALL_TIMEOUT, cx);
    cx.spawn(async move |cx| {
        let created = created.await;
        cx.update(|cx| match created {
            Ok(revision) => {
                let opened = open_session(
                    SessionArgs {
                        machine: key.machine.clone(),
                        at: SessionAt::Conversation {
                            conversation_id: key.id.clone(),
                        },
                        after: Some(revision),
                    },
                    cx,
                );
                report_failure("打开会话", opened, cx);
                Ok(key)
            }
            Err(error) => {
                Sessions::global(cx).update(cx, |sessions, _| {
                    sessions.starts.remove(&key);
                });
                Err(error)
            }
        })
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

/// 打开一个项目（已经开着什么都不做）。
pub(crate) fn open_project(
    machine: RuntimeKey,
    root: String,
    cx: &mut App,
) -> Task<Result<(), String>> {
    let host = api::host(cx);
    let style = host.path_style(&machine, cx);
    let root = style.clean(root.trim());
    if root.is_empty() {
        return Task::ready(Err("没有给项目目录".into()));
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
        select_project(machine, root, cx);
        return Task::ready(Ok(()));
    }
    let opened = api::call::<WorkspaceProjectOpen>(
        &machine,
        WorkspaceProjectParams { root: root.clone() },
        api::CALL_TIMEOUT,
        cx,
    );
    cx.spawn(async move |cx| {
        let revision = opened.await?;
        cx.update(|cx| select_project_since(machine, root, Some(revision), cx));
        Ok(())
    })
}

/// 关掉一个项目：先一段段关掉侧栏里摆在它下面的会话（含它的工作集里的），都关掉了再从
/// 目录里关项目；中途有关不掉的就停下报错，项目留着。只靠会话工作目录撑起来的组（不在
/// 打开的项目里）关完会话就没了。要不要先确认归调用方。
pub(crate) fn close_project(
    machine: RuntimeKey,
    root: String,
    cx: &mut App,
) -> Task<Result<(), String>> {
    let host = api::host(cx);
    let style = host.path_style(&machine, cx);
    let root = style.clean(root.trim());
    if root.is_empty() {
        return Task::ready(Err("没有给项目目录".into()));
    }
    let Some(directory) = host.workspace_directory(&machine, cx) else {
        return Task::ready(Err("这台机器的工作区目录还没收到".into()));
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
        cx.update(|cx| forget_selected_project(&machine, &root, cx));
        Ok(())
    })
}

/// 关掉的项目正选着：清掉。
fn forget_selected_project(machine: &RuntimeKey, root: &str, cx: &mut App) {
    let cleared = Sessions::global(cx).update(cx, |sessions, cx| {
        let selected = sessions
            .selection
            .project()
            .is_some_and(|(on, selected)| on == machine && selected == root);
        if selected {
            sessions.selection = Selection::None;
            sessions.rebuild_sidebar(cx);
        }
        selected
    });
    if cleared {
        publish_context(cx);
    }
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

/// 访达拖进来、「用 Smelt 打开」的路径：是目录的当项目打开，文件留给文件打开者。只认
/// 本机：别的机器上的路径这边判断不了是不是目录。
fn open_paths(machine: RuntimeKey, paths: Vec<String>, cx: &mut App) {
    if machine != RuntimeKey::Local {
        eprintln!(
            "[sessions] 只认本机上拖进来的目录，忽略 {} 条路径",
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
                report_failure("打开项目", opened, cx);
            }
        });
    })
    .detach();
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

    #[test]
    fn closing_a_project_takes_its_worksets_sessions_too() {
        let style = PathStyle::Posix;
        let directory = directory();
        assert_eq!(conversations_under(style, &directory, "/repo"), ["w", "p"]);
        assert_eq!(conversations_under(style, &directory, "/tmp/x"), ["l"]);
        assert!(conversations_under(style, &directory, "/nowhere").is_empty());
    }
}
