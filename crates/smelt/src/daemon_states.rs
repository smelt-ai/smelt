//! 各运行时上报的会话状态镜像（GPUI 全局单例）。唯一的守护订阅循环写（见 `main`），插件
//! 经宿主接口读（见 `plugin_host`），变了发 `HostEvent::SessionsChanged`。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gpui::{App, BorrowAppContext, Global};

use smelt_client::daemon_state::{DaemonSessionState, RuntimePresence};
use smelt_client::runtime::RuntimeKey;

/// 各运行时上报的会话状态镜像（全局单例，跨窗口共享）。按运行时分开：每个运行时一条
/// 常驻 subscribe，各自的快照只替换自己那份，断线也只影响自己那份。会话 key =
/// smeltd session id（每个 pane 一个）/ ACP 会话的 `acp-` 前缀 sid。终端 hook 与 ACP
/// 协议状态都先在运行时归约，再共用同一套状态着色链路。
///
/// 内层存数据，`revision` 只为让 GPUI 觉得 Global 变了。写入必须走 [`Self::replace_all`] /
/// [`Self::upsert`]，成功后唤醒应用级观察器（转成插件事件、守护换代等会话停下来）。
#[derive(Clone, Default)]
pub struct DaemonStates {
    runtimes: Arc<Mutex<HashMap<RuntimeKey, RuntimeMirror>>>,
    revision: u64,
}

#[derive(Default)]
struct RuntimeMirror {
    sessions: HashMap<String, DaemonSessionState>,
    /// 收到过至少一帧快照（空目录也算）。
    primed: bool,
    /// 当前这条 subscribe 连接是否仍活着。守护 exec 后旧连接断开，必须先置
    /// false，否则上一 epoch 的快照会把「正在交接」误判成会话已死。
    subscription_live: bool,
}

impl DaemonStates {
    fn signal(cx: &mut App) {
        cx.update_global::<Self, _>(|global, _| {
            global.revision = global.revision.wrapping_add(1);
        });
    }

    fn read<T>(cx: &App, read: impl FnOnce(&HashMap<RuntimeKey, RuntimeMirror>) -> T) -> Option<T> {
        let global = cx.try_global::<Self>()?;
        let runtimes = global.runtimes.lock().ok()?;
        Some(read(&runtimes))
    }

    /// 一个运行时的完整快照：换掉它那份镜像。
    pub fn replace_all(runtime: &RuntimeKey, list: Vec<DaemonSessionState>, cx: &mut App) {
        let runtimes = cx.global::<Self>().runtimes.clone();
        {
            let mut runtimes = runtimes.lock().unwrap();
            let mirror = runtimes.entry(runtime.clone()).or_default();
            mirror.sessions = list
                .into_iter()
                .map(|state| (state.id.clone(), state))
                .collect();
            mirror.primed = true;
            mirror.subscription_live = true;
        }
        Self::signal(cx);
    }

    pub fn remove(runtime: &RuntimeKey, id: &str, cx: &mut App) {
        let runtimes = cx.global::<Self>().runtimes.clone();
        let removed = runtimes
            .lock()
            .unwrap()
            .get_mut(runtime)
            .is_some_and(|mirror| mirror.sessions.remove(id).is_some());
        if removed {
            Self::signal(cx);
        }
    }

    pub fn upsert(runtime: &RuntimeKey, state: DaemonSessionState, cx: &mut App) {
        let runtimes = cx.global::<Self>().runtimes.clone();
        runtimes
            .lock()
            .unwrap()
            .entry(runtime.clone())
            .or_default()
            .sessions
            .insert(state.id.clone(), state);
        Self::signal(cx);
    }

    /// 运行时被移除：它那份镜像整个丢掉。
    pub fn forget_runtime(runtime: &RuntimeKey, cx: &mut App) {
        let Some(global) = cx.try_global::<Self>() else {
            return;
        };
        let runtimes = global.runtimes.clone();
        runtimes.lock().unwrap().remove(runtime);
        Self::signal(cx);
    }

    /// 这个运行时的 subscribe 断开：它那份镜像不再代表活着的 daemon epoch。
    pub fn mark_subscription_disconnected(runtime: &RuntimeKey, cx: &mut App) {
        let Some(global) = cx.try_global::<Self>() else {
            return;
        };
        let changed = global
            .runtimes
            .lock()
            .unwrap()
            .get_mut(runtime)
            .is_some_and(|mirror| std::mem::replace(&mut mirror.subscription_live, false));
        if changed {
            Self::signal(cx);
        }
    }

    pub fn snapshot(runtime: &RuntimeKey, cx: &App) -> Vec<DaemonSessionState> {
        Self::read(cx, |runtimes| {
            runtimes
                .get(runtime)
                .map(|mirror| mirror.sessions.values().cloned().collect())
                .unwrap_or_default()
        })
        .unwrap_or_default()
    }

    /// `None`：这个运行时的订阅未就绪，不能判死。其余见 [`RuntimePresence`]。
    pub fn runtime_presence(
        runtime: &RuntimeKey,
        session_id: &str,
        cx: &App,
    ) -> Option<RuntimePresence> {
        Self::read(cx, |runtimes| {
            let mirror = runtimes.get(runtime);
            DaemonSessionState::runtime_presence_from_mirror(
                mirror.is_some_and(|m| m.subscription_live),
                mirror.is_some_and(|m| m.primed),
                mirror.and_then(|m| m.sessions.get(session_id)),
            )
        })
        .flatten()
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
}
