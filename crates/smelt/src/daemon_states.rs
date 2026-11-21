//! 各运行时上报的会话状态镜像（GPUI 全局单例）。唯一的守护订阅循环写（见 `main`），插件
//! 经宿主接口读（见 `plugin_host`），变了发 `HostEvent::SessionsChanged`。

use std::collections::HashMap;

use gpui::{App, Global};

use smelt_client::daemon_state::{DaemonSessionState, RuntimePresence};
use smelt_client::runtime::RuntimeKey;

/// 各运行时上报的会话状态镜像（全局单例，跨窗口共享）。按运行时分开：每个运行时一条
/// 常驻 subscribe，各自的快照只替换自己那份，断线也只影响自己那份。会话 key =
/// smeltd session id（每个 pane 一个）/ ACP 会话的 `acp-` 前缀 sid。终端 hook 与 ACP
/// 协议状态都先在运行时归约，再共用同一套状态着色链路。
///
/// 写入只走这里的几个方法：先只读比较，真变了才可写地取全局。GPUI 里可写地取一下就算变了，
/// 观察者（转成插件的 `SessionsChanged`、守护换代等会话停下来）都会被叫醒，读过它的视图
/// 都要重建；运行时重发一份一样的状态不能惊动它们。
#[derive(Default)]
pub struct DaemonStates {
    runtimes: HashMap<RuntimeKey, RuntimeMirror>,
}

#[derive(Default, PartialEq)]
struct RuntimeMirror {
    sessions: HashMap<String, DaemonSessionState>,
    /// 收到过至少一帧快照（空目录也算）。
    primed: bool,
    /// 当前这条 subscribe 连接是否仍活着。守护 exec 后旧连接断开，必须先置
    /// false，否则上一 epoch 的快照会把「正在交接」误判成会话已死。
    subscription_live: bool,
}

impl DaemonStates {
    fn mirror<'a>(runtime: &RuntimeKey, cx: &'a App) -> Option<&'a RuntimeMirror> {
        cx.try_global::<Self>()?.runtimes.get(runtime)
    }

    fn mirror_mut<'a>(runtime: &RuntimeKey, cx: &'a mut App) -> &'a mut RuntimeMirror {
        cx.default_global::<Self>()
            .runtimes
            .entry(runtime.clone())
            .or_default()
    }

    /// 一个运行时的完整快照：换掉它那份镜像。
    pub fn replace_all(runtime: &RuntimeKey, list: Vec<DaemonSessionState>, cx: &mut App) {
        let mirror = RuntimeMirror {
            sessions: list
                .into_iter()
                .map(|state| (state.id.clone(), state))
                .collect(),
            primed: true,
            subscription_live: true,
        };
        if Self::mirror(runtime, cx) == Some(&mirror) {
            return;
        }
        *Self::mirror_mut(runtime, cx) = mirror;
    }

    pub fn remove(runtime: &RuntimeKey, id: &str, cx: &mut App) {
        if Self::mirror(runtime, cx).is_some_and(|mirror| mirror.sessions.contains_key(id)) {
            Self::mirror_mut(runtime, cx).sessions.remove(id);
        }
    }

    pub fn upsert(runtime: &RuntimeKey, state: DaemonSessionState, cx: &mut App) {
        let current = Self::mirror(runtime, cx).and_then(|mirror| mirror.sessions.get(&state.id));
        if current == Some(&state) {
            return;
        }
        Self::mirror_mut(runtime, cx)
            .sessions
            .insert(state.id.clone(), state);
    }

    /// 运行时被移除：它那份镜像整个丢掉。
    pub fn forget_runtime(runtime: &RuntimeKey, cx: &mut App) {
        if Self::mirror(runtime, cx).is_some() {
            cx.default_global::<Self>().runtimes.remove(runtime);
        }
    }

    /// 这个运行时的 subscribe 断开：它那份镜像不再代表活着的 daemon epoch。
    pub fn mark_subscription_disconnected(runtime: &RuntimeKey, cx: &mut App) {
        if Self::mirror(runtime, cx).is_some_and(|mirror| mirror.subscription_live) {
            Self::mirror_mut(runtime, cx).subscription_live = false;
        }
    }

    pub fn snapshot(runtime: &RuntimeKey, cx: &App) -> Vec<DaemonSessionState> {
        Self::mirror(runtime, cx)
            .map(|mirror| mirror.sessions.values().cloned().collect())
            .unwrap_or_default()
    }

    /// `None`：这个运行时的订阅未就绪，不能判死。其余见 [`RuntimePresence`]。
    pub fn runtime_presence(
        runtime: &RuntimeKey,
        session_id: &str,
        cx: &App,
    ) -> Option<RuntimePresence> {
        let mirror = Self::mirror(runtime, cx);
        DaemonSessionState::runtime_presence_from_mirror(
            mirror.is_some_and(|m| m.subscription_live),
            mirror.is_some_and(|m| m.primed),
            mirror.and_then(|m| m.sessions.get(session_id)),
        )
    }
}

impl Global for DaemonStates {}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    fn state(id: &str) -> DaemonSessionState {
        DaemonSessionState {
            id: id.to_string(),
            generation: 1,
            revision: 1,
            ..Default::default()
        }
    }

    fn ids(runtime: &RuntimeKey, cx: &App) -> Vec<String> {
        DaemonStates::snapshot(runtime, cx)
            .into_iter()
            .map(|state| state.id)
            .collect()
    }

    /// 一个运行时的快照、断线只动它自己那份：远程运行时连上时不能把本机会话冲掉，
    /// 远程断线也不能让本机会话被判死。
    #[gpui::test]
    fn each_runtime_keeps_its_own_mirror(cx: &mut TestAppContext) {
        let remote = RuntimeKey::Remote("rt-linux".to_string());
        cx.update(|cx| {
            cx.set_global(DaemonStates::default());
            DaemonStates::replace_all(&RuntimeKey::Local, vec![state("t-local")], cx);
            assert_eq!(
                DaemonStates::runtime_presence(&remote, "t-remote", cx),
                None
            );

            DaemonStates::replace_all(&remote, vec![state("t-remote")], cx);
            assert_eq!(ids(&RuntimeKey::Local, cx), ["t-local"]);
            assert_eq!(ids(&remote, cx), ["t-remote"]);

            DaemonStates::mark_subscription_disconnected(&remote, cx);
            assert_eq!(
                DaemonStates::runtime_presence(&remote, "t-remote", cx),
                None
            );
            assert_ne!(
                DaemonStates::runtime_presence(&RuntimeKey::Local, "t-local", cx),
                None
            );

            DaemonStates::forget_runtime(&remote, cx);
            assert!(DaemonStates::snapshot(&remote, cx).is_empty());
            assert_eq!(DaemonStates::snapshot(&RuntimeKey::Local, cx).len(), 1);
        });
    }

    /// 运行时重发一份一样的状态、快照，或断开已经断开的订阅，都不惊动观察者：它们会把整个
    /// 会话列表重算一遍。真变了才叫醒。
    #[gpui::test]
    fn unchanged_reports_leave_observers_alone(cx: &mut TestAppContext) {
        let signals = std::rc::Rc::new(std::cell::Cell::new(0));
        cx.update(|cx| {
            cx.set_global(DaemonStates::default());
            let signals = signals.clone();
            cx.observe_global::<DaemonStates>(move |_| signals.set(signals.get() + 1))
                .detach();
        });
        let local = RuntimeKey::Local;
        let report = |run: &dyn Fn(&mut App)| cx.update(|cx| run(cx));
        report(&|cx| DaemonStates::replace_all(&local, vec![state("a")], cx));
        assert_eq!(signals.get(), 1);
        report(&|cx| DaemonStates::replace_all(&local, vec![state("a")], cx));
        report(&|cx| DaemonStates::upsert(&local, state("a"), cx));
        report(&|cx| DaemonStates::remove(&local, "missing", cx));
        report(&|cx| DaemonStates::forget_runtime(&RuntimeKey::Remote("x".into()), cx));
        assert_eq!(signals.get(), 1, "什么都没变");

        let mut busy = state("a");
        busy.phase = smelt_client::daemon_state::DaemonPhase::Thinking;
        report(&|cx| DaemonStates::upsert(&local, busy.clone(), cx));
        assert_eq!(signals.get(), 2);

        report(&|cx| DaemonStates::mark_subscription_disconnected(&local, cx));
        report(&|cx| DaemonStates::mark_subscription_disconnected(&local, cx));
        assert_eq!(signals.get(), 3, "断开第二次不算变");
    }
}
