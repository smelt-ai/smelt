//! 列表跟着宿主和别的插件走：全靠事件，不起定时器、不轮询。
//!
//! - `HostEvent::SessionsChanged`（目录或会话状态变了）：那台机器的目录版本变了才清库
//!   （`SessionList::prune`）；侧栏重算。工作集顺便看一眼合没合进去。
//! - `HostEvent::WindowActivated`：人常常是去浏览器合完 PR 回来，工作集再看一眼。
//! - `HostEvent::MachinesChanged`：机器表里真的消失了的（和上一份比，不是离线）调
//!   `SessionList::forget_machine`；重算侧栏。
//! - `HostEvent::LocaleChanged`：侧栏缓存里有按语言算好的字，重算。
//! - `HostEvent::RuntimeTopic` 的 `attention.changed`：收进 `SessionList::unread`，有没看过
//!   的会话变了就重算侧栏。
//! - `HostEvent::OpenPaths`（从访达拖进来的）：本机上是目录的当项目打开。
//! - 会话插件的 `SELECTION_CHANGED`：换掉选中的缓存；选中的是会话就让侧栏露出它（从通知、
//!   命令跳过来时它可能折着）。起来时先经 `SELECTION` 问一次。
//! - 会话插件的 `CONVERSATION_CREATED`：记进 `SessionList::fresh`，列表等它出现时播动画。
//! - 智能体插件的 `AGENT_SETTINGS_CHANGED`：换掉那台机器的 `SessionList::agent_names`，重算
//!   侧栏。起来时先经 `AGENT_SETTINGS` 拉一份整的。
//!
//! 宿主每台机器只留最新的一份目录，所以这里不用防旧推送：目录里没了就是真没了。

use rust_i18n::t;
use std::collections::{HashMap, HashSet};

use gpui::{App, Entity, Subscription};
use smelt_plugin_api::model::daemon_state::CORE_TOPIC_ATTENTION_CHANGED;
use smelt_plugin_api::model::workspace_directory::WorkspaceRevision;
use smelt_plugin_api::{self as api, HostEvent, RuntimeKey};

use crate::core::{CONVERSATION_CREATED, ConversationRef, SELECTION, SELECTION_CHANGED, Selected};
use crate::model::{ConversationKey, SessionList};
use crate::peers::agents::{
    AGENT_SETTINGS, AGENT_SETTINGS_CHANGED, AgentSettingsState, MachineAgentSettings,
};
use crate::{notify_error, ops, worksets};

/// 自己的状态，放在 `SessionList::sync` 里。
#[derive(Default)]
pub(crate) struct State {
    /// 上一份机器表：机器表变了时和它比，认出真的移除了的。
    machines: HashSet<RuntimeKey>,
    /// 各台机器上次处理过的目录版本。会话状态变了也发 `SessionsChanged`，目录没变就不
    /// 重清库。
    seen: HashMap<RuntimeKey, WorkspaceRevision>,
    /// 已经收到过 `SELECTION_CHANGED`：起来时问的那次回来得晚，就不拿旧的盖掉新的。
    heard_selection: bool,
    _subscriptions: Vec<Subscription>,
}

/// 订阅宿主事件，接上上面那些处理。`register` 里建好 `SessionList` 之后调一次。
pub(crate) fn install(list: &Entity<SessionList>, cx: &mut App) {
    if let Err(error) = api::register_topics(crate::OWNER, [CORE_TOPIC_ATTENTION_CHANGED], cx) {
        eprintln!("[session-list] 话题登记失败：{error}");
    }
    let machines = api::host(cx)
        .machines(cx)
        .into_iter()
        .map(|machine| machine.key)
        .collect();
    let events = api::events(cx);
    let subscription = cx.subscribe(&events, |_, event: &HostEvent, cx| on_event(event, cx));
    list.update(cx, |list, _| {
        list.sync.machines = machines;
        list.sync._subscriptions.push(subscription);
    });
    // 宿主可能在本插件登记之前就收到了目录：先按眼下的走一遍。
    on_sessions_changed(cx);
    // 选中、智能体配置同理：问一份眼下的，之后跟着事件走。
    let selection = SELECTION.run(&(), cx);
    cx.spawn(async move |cx| match selection.await {
        Ok(selected) => cx.update(|cx| {
            if !SessionList::global(cx).read(cx).sync.heard_selection {
                on_selection(selected, cx);
            }
        }),
        Err(error) => eprintln!("[session-list] 问会话插件选中了什么失败：{error}"),
    })
    .detach();
    let settings = AGENT_SETTINGS.run(&(), cx);
    cx.spawn(async move |cx| match settings.await {
        Ok(all) => cx.update(|cx| {
            for machine in all {
                on_agent_settings(machine, cx);
            }
        }),
        Err(error) => cx.update(|cx| {
            notify_error(
                &t!(
                    "error.failed",
                    what = t!("action.read_agent_settings"),
                    error = error
                ),
                cx,
            )
        }),
    })
    .detach();
}

fn on_event(event: &HostEvent, cx: &mut App) {
    match event {
        HostEvent::SessionsChanged => {
            on_sessions_changed(cx);
            worksets::refresh(false, cx);
        }
        HostEvent::WindowActivated => worksets::refresh(true, cx),
        HostEvent::MachinesChanged => on_machines_changed(cx),
        HostEvent::LocaleChanged => {
            SessionList::global(cx).update(cx, |list, cx| list.rebuild_sidebar(cx));
        }
        HostEvent::RuntimeTopic {
            machine,
            topic,
            message,
        } if topic == CORE_TOPIC_ATTENTION_CHANGED => {
            SessionList::global(cx).update(cx, |list, cx| {
                if list.unread.apply(machine, message) {
                    list.rebuild_sidebar(cx);
                }
            });
        }
        HostEvent::OpenPaths { machine, paths } => {
            ops::open_paths(machine.clone(), paths.clone(), cx)
        }
        HostEvent::Plugin { .. } => {
            if let Some(selected) = SELECTION_CHANGED.parse(event) {
                SessionList::global(cx).update(cx, |list, _| list.sync.heard_selection = true);
                on_selection(selected, cx);
            } else if let Some(created) = CONVERSATION_CREATED.parse(event) {
                on_created(created, cx);
            } else if let Some(changed) = AGENT_SETTINGS_CHANGED.parse(event) {
                on_agent_settings(changed, cx);
            }
        }
        _ => {}
    }
}

fn on_sessions_changed(cx: &mut App) {
    let host = api::host(cx);
    let list = SessionList::global(cx);
    for machine in host.machines(cx) {
        let machine = machine.key;
        let Some(directory) = host.workspace_directory(&machine, cx) else {
            continue;
        };
        let style = host.path_style(&machine, cx);
        list.update(cx, |list, _| {
            let version = directory.version();
            if list.sync.seen.get(&machine) == Some(&version) {
                return;
            }
            list.prune(&machine, style, &directory);
            list.sync.seen.insert(machine, version);
        });
    }
    list.update(cx, |list, cx| list.rebuild_sidebar(cx));
}

fn on_machines_changed(cx: &mut App) {
    let current: HashSet<RuntimeKey> = api::host(cx)
        .machines(cx)
        .into_iter()
        .map(|machine| machine.key)
        .collect();
    SessionList::global(cx).update(cx, |list, cx| {
        let gone: Vec<RuntimeKey> = list.sync.machines.difference(&current).cloned().collect();
        for machine in &gone {
            list.forget_machine(machine);
            list.sync.seen.remove(machine);
            list.unread.forget(machine);
        }
        list.sync.machines = current;
        // 离线、改名也会走到这里：机器行要跟着变。
        list.rebuild_sidebar(cx);
    });
}

/// 会话插件那边的选中换了（或又选了一次同一段）：换掉缓存，选中的会话在侧栏里露出来。
fn on_selection(selected: Selected, cx: &mut App) {
    SessionList::global(cx).update(cx, |list, cx| {
        list.selection = selected.into();
        if let Some(key) = list.selection.session().cloned() {
            list.reveal(&key, cx);
        }
        cx.notify();
    });
}

fn on_created(created: ConversationRef, cx: &mut App) {
    SessionList::global(cx).update(cx, |list, _| {
        list.fresh = Some(ConversationKey {
            machine: created.machine,
            id: created.conversation_id,
        });
    });
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
    SessionList::global(cx).update(cx, |list, cx| {
        let unchanged = match &names {
            Some(names) => list.agent_names.get(&changed.machine) == Some(names),
            None => !list.agent_names.contains_key(&changed.machine),
        };
        if unchanged {
            return;
        }
        match names {
            Some(names) => list.agent_names.insert(changed.machine, names),
            None => list.agent_names.remove(&changed.machine),
        };
        list.rebuild_sidebar(cx);
    });
}
