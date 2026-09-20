//! 两个跨视图通信用的 GPUI 全局单例：ACP 视图（`smelt-acp-view`）和主 GUI 都要
//! 读写，放共享层而不是随便哪一边，免得循环依赖。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gpui::{App, BorrowAppContext, Global};

pub use smelt_core::attention::{AttentionItem, AttentionKind, AttentionStore};
use smelt_core::daemon_state::DaemonSessionState;

/// 守护上报的会话状态镜像（全局单例，跨窗口共享）。key = smeltd session id
/// （每个 pane 一个）/ ACP 会话的 `acp-` 前缀 sid。由主 GUI 启动时那条常驻
/// subscribe 转发任务维护；终端 hook 与 ACP 协议状态都先在 smeltd 归约，再共用
/// 同一套「状态着色 / Dock 与菜单栏角标 / 通知」链路。
///
/// 与 [`AttentionGlobal`] 相同：内层存数据，`revision` 只为让 GPUI 觉得
/// Global 变了。写入必须走 [`Self::replace_all`] / [`Self::upsert`]，成功后
/// 唤醒应用级观察器。观察器负责 pane 副作用和合并刷新，不要在各面板手写 notify。
#[derive(Clone, Default)]
pub struct DaemonStates {
    pub map: Arc<Mutex<HashMap<String, DaemonSessionState>>>,
    pending: Arc<Mutex<Vec<DaemonSessionState>>>,
    revision: u64,
    /// 当前这条 subscribe 连接是否仍活着。守护 exec 后旧连接断开，必须先置
    /// false，否则上一 epoch 的 primed 快照会把「正在交接」误判成会话已死。
    subscription_live: bool,
}

impl DaemonStates {
    fn signal(cx: &mut App) {
        cx.update_global::<Self, _>(|global, _| {
            global.revision = global.revision.wrapping_add(1);
        });
    }

    pub fn replace_all(list: Vec<DaemonSessionState>, cx: &mut App) -> Vec<String> {
        let map = cx.global::<Self>().map.clone();
        let pending = cx.global::<Self>().pending.clone();
        let stale_ids = {
            let mut map = map.lock().unwrap();
            let live_ids: std::collections::HashSet<&str> =
                list.iter().map(|state| state.id.as_str()).collect();
            let stale_ids: Vec<String> = map
                .keys()
                .filter(|id| !live_ids.contains(id.as_str()))
                .cloned()
                .collect();
            map.clear();
            for state in &list {
                map.insert(state.id.clone(), state.clone());
            }
            stale_ids
        };
        {
            let mut pending = pending.lock().unwrap();
            pending.clear();
            pending.extend(list);
        }
        cx.update_global::<Self, _>(|global, _| {
            global.subscription_live = true;
            global.revision = global.revision.wrapping_add(1);
        });
        stale_ids
    }

    pub fn remove(id: &str, cx: &mut App) -> bool {
        let map = cx.global::<Self>().map.clone();
        let pending = cx.global::<Self>().pending.clone();
        let removed = map.lock().unwrap().remove(id).is_some();
        pending.lock().unwrap().retain(|state| state.id != id);
        if removed {
            Self::signal(cx);
        }
        removed
    }

    pub fn upsert(state: DaemonSessionState, cx: &mut App) {
        let map = cx.global::<Self>().map.clone();
        let pending = cx.global::<Self>().pending.clone();
        map.lock().unwrap().insert(state.id.clone(), state.clone());
        pending.lock().unwrap().push(state);
        Self::signal(cx);
    }

    pub fn drain_pending(cx: &mut App) -> Vec<DaemonSessionState> {
        std::mem::take(&mut *cx.global::<Self>().pending.lock().unwrap())
    }

    pub fn get(session_id: &str, cx: &App) -> Option<DaemonSessionState> {
        cx.try_global::<Self>()?
            .map
            .lock()
            .ok()?
            .get(session_id)
            .cloned()
    }

    /// 是否已收到过至少一帧 subscribe（空目录的 replace_all 也算）。
    pub fn is_primed(cx: &App) -> bool {
        cx.try_global::<Self>()
            .is_some_and(|global| global.revision != 0)
    }

    /// subscribe socket 断开：当前镜像不再代表活着的 daemon epoch。
    pub fn mark_subscription_disconnected(cx: &mut App) {
        if cx.try_global::<Self>().is_none() {
            return;
        }
        cx.update_global::<Self, _>(|global, _| {
            if !global.subscription_live {
                return;
            }
            global.subscription_live = false;
            global.revision = global.revision.wrapping_add(1);
        });
    }

    pub fn snapshot(cx: &App) -> Vec<DaemonSessionState> {
        cx.try_global::<Self>()
            .and_then(|global| global.map.lock().ok())
            .map(|map| map.values().cloned().collect())
            .unwrap_or_default()
    }

    /// `None`：当前订阅未就绪，不能判死。`Some(false)`：当前 epoch 快照里没有可 attach 的运行时。
    pub fn runtime_alive(session_id: &str, cx: &App) -> Option<bool> {
        let global = cx.try_global::<Self>()?;
        let subscription_live = global.subscription_live;
        let primed = global.revision != 0;
        let state = global.map.lock().ok()?.get(session_id).cloned();
        DaemonSessionState::runtime_alive_from_mirror(subscription_live, primed, state.as_ref())
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
                smelt_core::attention::apply_daemon_transition(&mut store, previous, state, now);
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
            let item = smelt_core::attention::apply_daemon_baseline(&mut store, state);
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
