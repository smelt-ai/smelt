//! 两个跨视图通信用的 GPUI 全局单例：ACP 视图（`smelt-acp-view`）和主 GUI 都要
//! 读写，放共享层而不是随便哪一边，免得循环依赖。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gpui::{App, BorrowAppContext, Global};

use smelt_client::daemon_state::{DaemonSessionState, RuntimePresence, TerminalProgress};
use smelt_client::runtime::RuntimeKey;
pub use smelt_model::attention::{AttentionItem, AttentionKind, AttentionStore};

/// 各运行时上报的会话状态镜像（全局单例，跨窗口共享）。按运行时分开：每个运行时一条
/// 常驻 subscribe，各自的快照只替换自己那份，断线也只影响自己那份。会话 key =
/// smeltd session id（每个 pane 一个）/ ACP 会话的 `acp-` 前缀 sid。终端 hook 与 ACP
/// 协议状态都先在运行时归约，再共用同一套「状态着色 / Dock 与菜单栏角标 / 通知」链路。
///
/// 与 [`AttentionGlobal`] 相同：内层存数据，`revision` 只为让 GPUI 觉得
/// Global 变了。写入必须走 [`Self::replace_all`] / [`Self::upsert`]，成功后
/// 唤醒应用级观察器。观察器负责 pane 副作用和侧栏刷新，不要在各面板手写 notify。
#[derive(Clone, Default)]
pub struct DaemonStates {
    runtimes: Arc<Mutex<HashMap<RuntimeKey, RuntimeMirror>>>,
    pending: Arc<Mutex<Vec<(RuntimeKey, DaemonSessionState)>>>,
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
    /// 终端程序用 OSC 9;4 报的进度，只在内存里。断线就清：运行时会定期重发还在的。
    progress: HashMap<String, TerminalProgress>,
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

    /// 一个运行时的完整快照：换掉它那份镜像，返回不再存在的会话。
    pub fn replace_all(
        runtime: &RuntimeKey,
        list: Vec<DaemonSessionState>,
        cx: &mut App,
    ) -> Vec<String> {
        let runtimes = cx.global::<Self>().runtimes.clone();
        let pending = cx.global::<Self>().pending.clone();
        let stale_ids = {
            let mut runtimes = runtimes.lock().unwrap();
            let mirror = runtimes.entry(runtime.clone()).or_default();
            let live_ids: std::collections::HashSet<&str> =
                list.iter().map(|state| state.id.as_str()).collect();
            let stale_ids: Vec<String> = mirror
                .sessions
                .keys()
                .filter(|id| !live_ids.contains(id.as_str()))
                .cloned()
                .collect();
            mirror.sessions = list
                .iter()
                .map(|state| (state.id.clone(), state.clone()))
                .collect();
            mirror
                .progress
                .retain(|id, _| live_ids.contains(id.as_str()));
            mirror.primed = true;
            mirror.subscription_live = true;
            stale_ids
        };
        {
            let mut pending = pending.lock().unwrap();
            pending.retain(|(key, _)| key != runtime);
            pending.extend(list.into_iter().map(|state| (runtime.clone(), state)));
        }
        Self::signal(cx);
        stale_ids
    }

    pub fn remove(runtime: &RuntimeKey, id: &str, cx: &mut App) -> bool {
        let runtimes = cx.global::<Self>().runtimes.clone();
        let pending = cx.global::<Self>().pending.clone();
        let removed = runtimes
            .lock()
            .unwrap()
            .get_mut(runtime)
            .is_some_and(|mirror| {
                mirror.progress.remove(id);
                mirror.sessions.remove(id).is_some()
            });
        pending
            .lock()
            .unwrap()
            .retain(|(key, state)| key != runtime || state.id != id);
        if removed {
            Self::signal(cx);
        }
        removed
    }

    pub fn upsert(runtime: &RuntimeKey, state: DaemonSessionState, cx: &mut App) {
        let runtimes = cx.global::<Self>().runtimes.clone();
        let pending = cx.global::<Self>().pending.clone();
        runtimes
            .lock()
            .unwrap()
            .entry(runtime.clone())
            .or_default()
            .sessions
            .insert(state.id.clone(), state.clone());
        pending.lock().unwrap().push((runtime.clone(), state));
        Self::signal(cx);
    }

    /// 运行时被移除：它那份镜像整个丢掉，返回原来在上面的会话。
    pub fn forget_runtime(runtime: &RuntimeKey, cx: &mut App) -> Vec<String> {
        let Some(global) = cx.try_global::<Self>() else {
            return Vec::new();
        };
        let runtimes = global.runtimes.clone();
        let pending = global.pending.clone();
        let ids: Vec<String> = runtimes
            .lock()
            .unwrap()
            .remove(runtime)
            .map(|mirror| mirror.sessions.into_keys().collect())
            .unwrap_or_default();
        pending.lock().unwrap().retain(|(key, _)| key != runtime);
        Self::signal(cx);
        ids
    }

    pub fn drain_pending(cx: &mut App) -> Vec<(RuntimeKey, DaemonSessionState)> {
        std::mem::take(&mut *cx.global::<Self>().pending.lock().unwrap())
    }

    pub fn get(runtime: &RuntimeKey, session_id: &str, cx: &App) -> Option<DaemonSessionState> {
        Self::read(cx, |runtimes| {
            runtimes
                .get(runtime)
                .and_then(|mirror| mirror.sessions.get(session_id))
                .cloned()
        })
        .flatten()
    }

    /// 这个运行时是否已收到过至少一帧 subscribe（空目录的快照也算）。
    pub fn is_primed(runtime: &RuntimeKey, cx: &App) -> bool {
        Self::read(cx, |runtimes| {
            runtimes.get(runtime).is_some_and(|m| m.primed)
        })
        .unwrap_or(false)
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
            .is_some_and(|mirror| {
                let had_progress = !mirror.progress.is_empty();
                mirror.progress.clear();
                std::mem::replace(&mut mirror.subscription_live, false) || had_progress
            });
        if changed {
            Self::signal(cx);
        }
    }

    /// 运行时推来的终端进度（`None` = 清掉）。没变就不唤醒观察器。
    pub fn set_progress(
        runtime: &RuntimeKey,
        session_id: &str,
        progress: Option<TerminalProgress>,
        cx: &mut App,
    ) {
        let Some(global) = cx.try_global::<Self>() else {
            return;
        };
        let changed = {
            let mut runtimes = global.runtimes.lock().unwrap();
            let mirror = runtimes.entry(runtime.clone()).or_default();
            match progress {
                Some(progress) => {
                    mirror.progress.insert(session_id.to_string(), progress) != Some(progress)
                }
                None => mirror.progress.remove(session_id).is_some(),
            }
        };
        if changed {
            Self::signal(cx);
        }
    }

    pub fn progress(runtime: &RuntimeKey, session_id: &str, cx: &App) -> Option<TerminalProgress> {
        Self::read(cx, |runtimes| {
            runtimes
                .get(runtime)
                .and_then(|mirror| mirror.progress.get(session_id))
                .copied()
        })
        .flatten()
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

/// 所有 agent 关注事件的唯一 UI store。生产者必须通过
/// `publish_terminal_notification` / `apply_daemon_transition` 写入；成功入队后会更新
/// revision，唤醒应用级投递观察器。
/// 这样后台系统通知不依赖某扇窗口继续 render（macOS 原生全屏窗口在其它 Space
/// 时会暂停帧回调）。铃铛、Dock 与菜单栏仍只读未读集合。
#[derive(Clone, Default)]
pub struct AttentionGlobal(pub Arc<Mutex<AttentionStore>>, u64);

impl AttentionGlobal {
    fn signal(cx: &mut App) {
        cx.update_global::<Self, _>(|global, _| {
            global.1 = global.1.wrapping_add(1);
        });
    }

    pub fn publish_terminal_notification(item: AttentionItem, now: Instant, cx: &mut App) -> bool {
        let store = cx.global::<Self>().0.clone();
        let (published, changed) = {
            let mut store = store.lock().unwrap();
            let badge_before = store.badge_count();
            let published = store.publish_terminal_notification(item, now);
            (published, published || store.badge_count() != badge_before)
        };
        if changed {
            Self::signal(cx);
        }
        published
    }

    pub fn apply_daemon_transition(
        previous: Option<&DaemonSessionState>,
        state: &DaemonSessionState,
        now: Instant,
        cx: &mut App,
    ) -> Option<AttentionItem> {
        let store = cx.global::<Self>().0.clone();
        let (item, changed) = {
            let mut store = store.lock().unwrap();
            let badge_before = store.badge_count();
            let item =
                smelt_model::attention::apply_daemon_transition(&mut store, previous, state, now);
            let changed = item.is_some() || store.badge_count() != badge_before;
            (item, changed)
        };
        if changed {
            Self::signal(cx);
        }
        item
    }

    pub fn apply_daemon_baseline(
        state: &DaemonSessionState,
        cx: &mut App,
    ) -> Option<AttentionItem> {
        let store = cx.global::<Self>().0.clone();
        let (item, changed) = {
            let mut store = store.lock().unwrap();
            let badge_before = store.badge_count();
            let item = smelt_model::attention::apply_daemon_baseline(&mut store, state);
            (item, store.badge_count() != badge_before)
        };
        if changed {
            Self::signal(cx);
        }
        item
    }

    pub fn mark_read(session_id: &str, cx: &mut App) -> Option<AttentionItem> {
        let store = cx.global::<Self>().0.clone();
        let item = store.lock().unwrap().mark_read(session_id);
        if item.is_some() {
            Self::signal(cx);
        }
        item
    }

    pub fn remove_session(session_id: &str, cx: &mut App) -> bool {
        let store = cx.global::<Self>().0.clone();
        let removed = store.lock().unwrap().remove_session(session_id);
        if removed {
            Self::signal(cx);
        }
        removed
    }
}

impl Global for AttentionGlobal {}

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

    /// 一个运行时的快照、断线只动它自己那份：远程运行时连上时不能把本机会话冲掉，
    /// 远程断线也不能让本机会话被判死。
    #[gpui::test]
    fn each_runtime_keeps_its_own_mirror(cx: &mut TestAppContext) {
        let remote = RuntimeKey::Remote("rt-linux".to_string());
        cx.update(|cx| {
            cx.set_global(DaemonStates::default());
            DaemonStates::replace_all(&RuntimeKey::Local, vec![state("t-local")], cx);
            assert!(!DaemonStates::is_primed(&remote, cx));

            let stale = DaemonStates::replace_all(&remote, vec![state("t-remote")], cx);
            assert!(stale.is_empty());
            assert!(DaemonStates::get(&RuntimeKey::Local, "t-local", cx).is_some());
            assert!(DaemonStates::get(&remote, "t-local", cx).is_none());
            assert!(DaemonStates::get(&remote, "t-remote", cx).is_some());

            DaemonStates::mark_subscription_disconnected(&remote, cx);
            assert_eq!(DaemonStates::runtime_presence(&remote, "t-remote", cx), None);
            assert_ne!(
                DaemonStates::runtime_presence(&RuntimeKey::Local, "t-local", cx),
                None
            );

            let drained = DaemonStates::drain_pending(cx);
            assert_eq!(drained.len(), 2);
            assert_eq!(
                DaemonStates::forget_runtime(&remote, cx),
                vec!["t-remote".to_string()]
            );
            assert!(DaemonStates::snapshot(&remote, cx).is_empty());
            assert_eq!(DaemonStates::snapshot(&RuntimeKey::Local, cx).len(), 1);
        });
    }
}
