//! 小人插件的全局状态 [`Cast`]：每段会话在什么状态，以及演什么（[`Stage`]）。头像只读它。
//!
//! 只看宿主，全靠事件更新，不起定时器：`HostEvent::SessionsChanged`、`MachinesChanged`、
//! `ActiveContextChanged` 来了，按宿主的工作区目录、会话状态和露着的会话重算每段会话，交给
//! [`Stage`] 比前后。已经收到过目录的机器上新冒出来的会话是刚建的，打个招呼。会话在什么
//! 状态，用目录类型自己的方法算，和会话列表是同一套。

use std::collections::{HashMap, HashSet};

use gpui::{App, AppContext as _, Context, Entity, Global, Subscription};
use smelt_plugin_api::model::agent_status::AgentStatus;
use smelt_plugin_api::{self as api, HostEvent, RuntimeKey};

use crate::buddy::{Buddy, Stage};
use crate::frames::Clock;

/// 哪台机器上的哪段会话（工作区目录里的 id）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ConversationKey {
    pub(crate) machine: RuntimeKey,
    pub(crate) id: String,
}

pub(crate) struct Cast {
    /// 各段会话眼下的状态。
    seats: HashMap<ConversationKey, AgentStatus>,
    /// 露着的那段会话。
    viewed: Option<ConversationKey>,
    stage: Stage,
    /// 所有小人共用的出帧格。
    clock: Clock,
    /// 收到过目录的机器。之后它的目录里新冒出来的会话是刚建的。
    known: HashSet<RuntimeKey>,
    _subscriptions: Vec<Subscription>,
}

struct CastGlobal(Entity<Cast>);

impl Global for CastGlobal {}

impl Cast {
    pub(crate) fn global(cx: &App) -> Entity<Self> {
        cx.global::<CastGlobal>().0.clone()
    }

    /// 会话 `key` 的小人：按会话 id 落，每段会话一个。
    pub(crate) fn buddy_of(&self, key: &ConversationKey) -> Buddy {
        Buddy::of(&key.id)
    }

    pub(crate) fn stage(&self) -> &Stage {
        &self.stage
    }

    pub(crate) fn clock(&self) -> Clock {
        self.clock
    }

    /// 按宿主眼下的目录、会话状态和露着的会话重算。没变就不通知，头像不重画。
    fn sync_sessions(&mut self, cx: &mut Context<Self>) {
        let host = api::host(cx);
        let viewed_session = api::active_context(cx).session;
        let mut seats = HashMap::new();
        let mut viewed = None;
        let mut fresh = Vec::new();
        let mut machines = HashSet::new();
        for machine in host.machines(cx) {
            let machine = machine.key;
            let Some(directory) = host.workspace_directory(&machine, cx) else {
                continue;
            };
            let states = host.session_states(&machine, cx);
            let known = self.known.contains(&machine);
            for conversation in &directory.conversations {
                let key = ConversationKey {
                    machine: machine.clone(),
                    id: conversation.id.clone(),
                };
                if known && !self.seats.contains_key(&key) && !self.stage.knows(&key) {
                    fresh.push(key.clone());
                }
                seats.insert(key, conversation.status(&states));
            }
            if let Some(session) = viewed_session
                .as_ref()
                .filter(|session| session.machine == machine)
            {
                viewed = directory
                    .conversation_of_session(&session.id)
                    .map(|conversation| ConversationKey {
                        machine: machine.clone(),
                        id: conversation.id.clone(),
                    });
            }
            machines.insert(machine);
        }
        self.known = machines;
        if seats == self.seats && viewed == self.viewed {
            return;
        }
        let now = cx.background_executor().now();
        self.stage.track(
            seats.iter().map(|(key, status)| (key, *status)),
            viewed.as_ref(),
            now,
        );
        for key in &fresh {
            self.stage.greet(key, now);
        }
        self.seats = seats;
        self.viewed = viewed;
        self.prepare();
        cx.notify();
    }

    /// 把眼下各会话的小人要的零件解好（解过的跳过）。在事件里调，render 只取。
    fn prepare(&self) {
        let buddies: HashSet<Buddy> = self.seats.keys().map(|key| self.buddy_of(key)).collect();
        for buddy in buddies {
            buddy.prepare();
        }
    }
}

/// 建好 [`Cast`]、订上事件。`register` 里调一次，在登记视图之前。
pub(crate) fn install(cx: &mut App) {
    let cast = cx.new(|cx| {
        let events = api::events(cx);
        let subscription = cx.subscribe(&events, |cast: &mut Cast, _, event: &HostEvent, cx| {
            if matches!(
                event,
                HostEvent::SessionsChanged
                    | HostEvent::MachinesChanged
                    | HostEvent::ActiveContextChanged
            ) {
                cast.sync_sessions(cx);
            }
        });
        let now = cx.background_executor().now();
        Cast {
            seats: HashMap::new(),
            viewed: None,
            stage: Stage::new(std::iter::empty(), None, now),
            clock: Clock::new(now),
            known: HashSet::new(),
            _subscriptions: vec![subscription],
        }
    });
    cx.set_global(CastGlobal(cast.clone()));
    // 宿主可能在本插件登记之前就收到了目录：先按眼下的走一遍。这一遍看到的会话都只按状态摆。
    cast.update(cx, |cast, cx| cast.sync_sessions(cx));
}
