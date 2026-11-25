//! 多插件共享的 ACP 活体订阅中心（纯非 UI、零 GPUI、事件驱动零空闲唤醒）。
//!
//! - 每个活跃订阅归属唯一 `owner`（插件 id），按 `sub_id` 索引，事件只定向推送给该 `owner`；
//! - 活跃回合吐字期间用事件驱动 12ms 节流合并增量帧（空闲时阻塞在 `event_rx.recv()`，0 周期唤醒），
//!   相位/审批/提问/配置/模型/记忆/结束态等关键变化零延迟直推；
//! - 支持同帧分屏重构复用（`reuse_key`）：`unsubscribe` 时将活体连接暂存 [`PARK_GRACE`]，
//!   同 `owner` 在窗口内带相同 `reuse_key` 重新 `subscribe` 时零断连接管并立即重放最新快照；
//! - 插件热重载/卸载/崩溃时调用 [`AcpHub::remove_owner`]，一次性释放该插件的全部连接。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use smelt_plugin_api::RuntimeKey;
use smelt_plugin_api::client::RuntimeClient;
use smelt_plugin_api::model::acp_session::{AcpUserAction, ConversationSnapshot};
use smelt_plugin_api::model::daemon_state::DaemonPhase;

use crate::client::{
    ConversationClientEvent, ConversationClientHandle, ConversationClientLaunch, spawn_acp_client,
};
use crate::images::ImageStore;

/// 流式文本增量的防抖合并窗口（12ms，对齐 `smeltd` 16ms 节流边界避免拍频，同时保护突发帧）。
const STREAM_COALESCE: Duration = Duration::from_millis(12);
/// 分屏重构同帧短暂 unmount→mount 的连接暂存宽限期。
const PARK_GRACE: Duration = Duration::from_millis(300);

/// 定向事件回调：`(owner_plugin_id, event_payload)`。
pub type AcpEventSink = Arc<dyn Fn(&str, Value) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct SubscribeRequest {
    pub sub_id: String,
    pub owner: String,
    pub machine: RuntimeKey,
    pub reuse_key: Option<String>,
    pub launch: ConversationClientLaunch,
}

#[derive(Debug, Clone, PartialEq)]
struct UrgentDigest {
    phase: DaemonPhase,
    replaying_history: bool,
    entries_offset: usize,
    pending_permissions_len: usize,
    has_elicitation: bool,
    has_memory_changes: bool,
    has_plan: bool,
    compacting: bool,
    pending_prompts_len: usize,
    composer_restore_revision: u64,
    model_current: Option<String>,
    config_count: usize,
    subagents_len: usize,
    background_tasks_len: usize,
    has_notice: bool,
    has_turn_outcome: bool,
    session_title: Option<String>,
    end_reason_nonempty: bool,
}

impl UrgentDigest {
    fn of(snap: &ConversationSnapshot) -> Self {
        Self {
            phase: snap.phase,
            replaying_history: snap.replaying_history,
            entries_offset: snap.entries_offset,
            pending_permissions_len: snap.pending_permissions.len(),
            has_elicitation: snap.pending_elicitation.is_some(),
            has_memory_changes: snap.memory_changes.is_some(),
            has_plan: snap.plan.is_some(),
            compacting: snap.compacting,
            pending_prompts_len: snap.pending_prompts.len(),
            composer_restore_revision: snap.composer_restore_revision,
            model_current: snap.model.as_ref().map(|m| m.current_value.clone()),
            config_count: snap.config_options.len(),
            subagents_len: snap.subagents.len(),
            background_tasks_len: snap.background_tasks.len(),
            has_notice: snap.notice.is_some(),
            has_turn_outcome: snap.turn_outcome.is_some(),
            session_title: snap.session_title.clone(),
            end_reason_nonempty: !snap.end_reason.is_empty(),
        }
    }
}

/// 路由状态：在活体连接被暂存（`parked`）与重新认领（`reclaimed`）之间原位切换目标 `sub_id`。
struct RouteState {
    owner: String,
    active_sub_id: Option<String>,
    latest_snapshot: Option<Value>,
    /// 已经收到过完整信封的订阅：只有它收省略帧时才去掉信封键（它手里有上一份可沿用）。
    envelope_sub_id: Option<String>,
}

struct LiveSession {
    owner: String,
    machine: RuntimeKey,
    session_id: String,
    action_tx: smol::channel::Sender<AcpUserAction>,
    /// 持有底层连接句柄；Drop 时后台关闭 socket，读线程随之自然退出。
    _handle_guard: Arc<HandleGuard>,
    route: Arc<Mutex<RouteState>>,
}

struct HandleGuard {
    _conn: Mutex<Option<ConversationClientHandle>>,
}

struct ParkedEntry {
    token: u64,
    session: LiveSession,
}

#[derive(Default)]
struct HubState {
    active: HashMap<String, LiveSession>,
    parked: HashMap<String, ParkedEntry>,
}

/// 进程内 ACP 对话订阅中心。
#[derive(Clone)]
pub struct AcpHub {
    state: Arc<Mutex<HubState>>,
    images: ImageStore,
    sink: AcpEventSink,
    park_seq: Arc<AtomicU64>,
}

impl AcpHub {
    pub fn new(images: ImageStore, sink: AcpEventSink) -> Self {
        Self {
            state: Arc::new(Mutex::new(HubState::default())),
            images,
            sink,
            park_seq: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn images(&self) -> &ImageStore {
        &self.images
    }

    /// 建立或复接一段 ACP 订阅。
    pub fn subscribe(&self, runtime: RuntimeClient, req: SubscribeRequest) {
        let session_id = req.launch.id.clone();
        if let Some(reuse_key) = req.reuse_key.as_deref()
            && let Some(reclaimed) = self.try_reclaim(
                reuse_key,
                &req.owner,
                &req.machine,
                &session_id,
                &req.sub_id,
            )
        {
            if let Some(snapshot) = reclaimed {
                (self.sink)(
                    &req.owner,
                    json!({
                        "sub_id": req.sub_id,
                        "kind": "snapshot",
                        "snapshot": snapshot,
                    }),
                );
            }
            return;
        }

        let handle = spawn_acp_client(runtime, req.launch);
        self.attach_handle(req.sub_id, req.owner, req.machine, session_id, handle);
    }

    /// 将已创建的 `ConversationClientHandle` 挂入订阅表并启动事件驱动转发器（也供单测直接注入）。
    pub(crate) fn attach_handle(
        &self,
        sub_id: String,
        owner: String,
        machine: RuntimeKey,
        session_id: String,
        mut handle: ConversationClientHandle,
    ) {
        let action_tx = handle.action_tx.clone();
        let event_rx = handle.event_rx.clone();
        // 占位 dummy channel 仅保留 Drop 里的 conn_cell 托管能力。
        let (_, empty_rx) = smol::channel::bounded(1);
        handle.event_rx = empty_rx;

        let route = Arc::new(Mutex::new(RouteState {
            owner: owner.clone(),
            active_sub_id: Some(sub_id.clone()),
            latest_snapshot: None,
            envelope_sub_id: None,
        }));
        let guard = Arc::new(HandleGuard {
            _conn: Mutex::new(Some(handle)),
        });

        let session = LiveSession {
            owner: owner.clone(),
            machine,
            session_id,
            action_tx,
            _handle_guard: guard,
            route: Arc::clone(&route),
        };

        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(old) = state.active.insert(sub_id.clone(), session)
                && let Ok(mut r) = old.route.lock()
            {
                r.active_sub_id = None;
            }
        }

        let sink = Arc::clone(&self.sink);
        let images = self.images.clone();
        let thread_name = format!("smelt-acp-fwd-{}", &sub_id[..sub_id.len().min(12)]);
        let _ = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                smol::block_on(run_forwarder(
                    event_rx,
                    route,
                    images,
                    sink,
                    STREAM_COALESCE,
                ));
            });
    }

    fn try_reclaim(
        &self,
        reuse_key: &str,
        owner: &str,
        machine: &RuntimeKey,
        session_id: &str,
        new_sub_id: &str,
    ) -> Option<Option<Value>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let matches = state.parked.get(reuse_key).is_some_and(|p| {
            p.session.owner == owner
                && p.session.machine == *machine
                && p.session.session_id == session_id
        });
        if !matches {
            return None;
        }
        let parked = state.parked.remove(reuse_key)?;
        let latest = {
            let mut route = parked
                .session
                .route
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            route.active_sub_id = Some(new_sub_id.to_owned());
            route.envelope_sub_id = None;
            route.latest_snapshot.clone()
        };
        let replayable = latest.filter(|snap| {
            !snap
                .get("envelope_omitted")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && snap
                    .get("entries_offset")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    == 0
        });
        if replayable.is_none() {
            // 缓存帧是增量尾片或省略了信封时不能直接灌给新挂载组件，向守护请求完整权威快照。
            let _ = parked.session.action_tx.try_send(AcpUserAction::Refresh);
        } else if let Ok(mut route) = parked.session.route.lock() {
            route.envelope_sub_id = Some(new_sub_id.to_owned());
        }
        if let Some(old) = state.active.insert(new_sub_id.to_owned(), parked.session)
            && let Ok(mut r) = old.route.lock()
        {
            r.active_sub_id = None;
        }
        Some(replayable)
    }

    /// 取消订阅。带 `reuse_key` 时暂存 [`PARK_GRACE`] 供分屏重构同帧认领，超时自动断开。
    pub fn unsubscribe(&self, owner: &str, sub_id: &str, reuse_key: Option<String>) {
        let removed = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let owned = state.active.get(sub_id).is_some_and(|s| s.owner == owner);
            if !owned {
                return;
            }
            state.active.remove(sub_id)
        };
        let Some(session) = removed else {
            return;
        };
        if let Ok(mut route) = session.route.lock() {
            route.active_sub_id = None;
        }
        let Some(reuse_key) = reuse_key.filter(|k| !k.is_empty()) else {
            drop(session);
            return;
        };

        let token = self.park_seq.fetch_add(1, Ordering::Relaxed);
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .parked
                .insert(reuse_key.clone(), ParkedEntry { token, session });
        }
        let state_ref = Arc::clone(&self.state);
        let _ = std::thread::Builder::new()
            .name("smelt-acp-park-gc".into())
            .spawn(move || {
                std::thread::sleep(PARK_GRACE);
                let dropped = {
                    let mut state = state_ref.lock().unwrap_or_else(|e| e.into_inner());
                    if state
                        .parked
                        .get(&reuse_key)
                        .is_some_and(|p| p.token == token)
                    {
                        state.parked.remove(&reuse_key)
                    } else {
                        None
                    }
                };
                drop(dropped);
            });
    }

    /// 插件热重载 / 卸载 / 崩溃时，立即释放该插件拥有的全部活跃与暂存连接。
    pub fn remove_owner(&self, owner: &str) {
        let mut dropped = Vec::new();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let active_keys: Vec<String> = state
                .active
                .iter()
                .filter_map(|(k, s)| (s.owner == owner).then_some(k.clone()))
                .collect();
            for key in active_keys {
                if let Some(session) = state.active.remove(&key) {
                    if let Ok(mut r) = session.route.lock() {
                        r.active_sub_id = None;
                    }
                    dropped.push(session);
                }
            }
            let parked_keys: Vec<String> = state
                .parked
                .iter()
                .filter_map(|(k, p)| (p.session.owner == owner).then_some(k.clone()))
                .collect();
            for key in parked_keys {
                if let Some(entry) = state.parked.remove(&key) {
                    dropped.push(entry.session);
                }
            }
        }
        drop(dropped);
    }

    /// 发送一条 `AcpUserAction`；若附带 `image_refs`，在 Rust 侧从 [`ImageStore`] 还原为 `PromptImage`。
    pub fn send_action(
        &self,
        owner: &str,
        sub_id: &str,
        action_value: Value,
        image_refs: &[String],
    ) -> Result<(), String> {
        let tx = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let Some(session) = state.active.get(sub_id) else {
                return Err("subscription not found".into());
            };
            if session.owner != owner {
                return Err("subscription owned by another plugin".into());
            }
            session.action_tx.clone()
        };

        let mut action: AcpUserAction = serde_json::from_value(action_value)
            .map_err(|e| format!("invalid AcpUserAction: {e}"))?;

        if !image_refs.is_empty() {
            let mut resolved = Vec::with_capacity(image_refs.len());
            for ref_id in image_refs {
                let img = self
                    .images
                    .get_prompt_image(ref_id)
                    .ok_or_else(|| format!("image ref not found: {ref_id}"))?;
                resolved.push(img);
            }
            match &mut action {
                AcpUserAction::Prompt { images, .. } | AcpUserAction::FollowUp { images, .. } => {
                    images.extend(resolved);
                }
                _ => {}
            }
        }

        tx.try_send(action).map_err(|e| match e {
            smol::channel::TrySendError::Full(_) => "action queue is full".into(),
            smol::channel::TrySendError::Closed(_) => "session connection closed".into(),
        })
    }
}

/// 纯事件驱动转发循环：空闲阻塞在 `event_rx.recv()`（0 唤醒），高频流式增量在 `coalesce` 窗口内合并。
async fn run_forwarder(
    event_rx: smol::channel::Receiver<ConversationClientEvent>,
    route: Arc<Mutex<RouteState>>,
    images: ImageStore,
    sink: AcpEventSink,
    coalesce: Duration,
) {
    let mut last_emit: Option<Instant> = None;
    let mut last_urgent: Option<UrgentDigest> = None;
    let mut pending: Option<Box<ConversationSnapshot>> = None;

    loop {
        let next = if let Some(last) = last_emit
            && pending.is_some()
        {
            let deadline = last + coalesce;
            let now = Instant::now();
            if now >= deadline {
                if let Some(snap) = pending.take() {
                    emit_snapshot(&route, &images, &sink, &snap);
                    last_emit = Some(Instant::now());
                    last_urgent = Some(UrgentDigest::of(&snap));
                }
                continue;
            }
            let recv_fut = async { Some(event_rx.recv().await) };
            let timer_fut = async {
                smol::Timer::at(deadline).await;
                None
            };
            match smol::future::or(recv_fut, timer_fut).await {
                Some(res) => res,
                None => {
                    if let Some(snap) = pending.take() {
                        emit_snapshot(&route, &images, &sink, &snap);
                        last_emit = Some(Instant::now());
                        last_urgent = Some(UrgentDigest::of(&snap));
                    }
                    continue;
                }
            }
        } else {
            event_rx.recv().await
        };

        let Ok(event) = next else {
            if let Some(snap) = pending.take() {
                emit_snapshot(&route, &images, &sink, &snap);
            }
            break;
        };

        match event {
            ConversationClientEvent::InitialPromptUnsupported => {
                if let Some(snap) = pending.take() {
                    emit_snapshot(&route, &images, &sink, &snap);
                    last_emit = Some(Instant::now());
                    last_urgent = Some(UrgentDigest::of(&snap));
                }
                let target = {
                    let guard = route.lock().unwrap_or_else(|e| e.into_inner());
                    guard
                        .active_sub_id
                        .clone()
                        .map(|sub_id| (guard.owner.clone(), sub_id))
                };
                if let Some((owner, sub_id)) = target {
                    sink(
                        &owner,
                        json!({
                            "sub_id": sub_id,
                            "kind": "initial_prompt_unsupported",
                        }),
                    );
                }
            }
            ConversationClientEvent::Snapshot(mut snap) => {
                // 挂着的完整帧要被本帧顶掉：本帧省略了信封，信封的更新只在它身上，沿用它的。
                if snap.envelope_omitted
                    && let Some(replaced) = pending.take()
                    && !replaced.envelope_omitted
                {
                    snap.keep_envelope(*replaced);
                }
                let digest = UrgentDigest::of(&snap);
                let urgent = last_urgent.as_ref() != Some(&digest);
                let window_elapsed = last_emit.is_none_or(|t| t.elapsed() >= coalesce);
                if urgent || window_elapsed {
                    pending = None;
                    emit_snapshot(&route, &images, &sink, &snap);
                    last_emit = Some(Instant::now());
                    last_urgent = Some(digest);
                } else {
                    pending = Some(snap);
                }
            }
        }
    }
}

fn emit_snapshot(
    route: &Arc<Mutex<RouteState>>,
    images: &ImageStore,
    sink: &AcpEventSink,
    snap: &ConversationSnapshot,
) {
    let Ok(mut value) = serde_json::to_value(snap) else {
        return;
    };
    images.compact_snapshot_json(&mut value);
    let (target, strip_envelope) = {
        let mut guard = route.lock().unwrap_or_else(|e| e.into_inner());
        guard.latest_snapshot = Some(value.clone());
        let sub_id = guard.active_sub_id.clone();
        let strip = if snap.envelope_omitted {
            sub_id.is_some() && guard.envelope_sub_id == sub_id
        } else {
            guard.envelope_sub_id.clone_from(&sub_id);
            false
        };
        (sub_id.map(|sub_id| (guard.owner.clone(), sub_id)), strip)
    };
    // 省略帧里的信封字段是空值不是清空：去掉这些键，插件拿本帧盖上一份就是对的，
    // 不必自己再列一遍哪些是信封。
    if strip_envelope && let Some(fields) = value.as_object_mut() {
        for key in ConversationSnapshot::ENVELOPE_KEYS {
            fields.remove(*key);
        }
    }
    if let Some((owner, sub_id)) = target {
        sink(
            &owner,
            json!({
                "sub_id": sub_id,
                "kind": "snapshot",
                "snapshot": value,
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::fallback_snapshot;
    use image::{ImageFormat, Rgba, RgbaImage};
    use smelt_plugin_api::model::acp_chat::AcpEntry;
    use std::io::Cursor;

    fn make_png_bytes() -> Vec<u8> {
        let img = RgbaImage::from_pixel(16, 16, Rgba([1, 2, 3, 255]));
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn coalesces_rapid_streaming_deltas_and_emits_urgent_immediately() {
        let received = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let recv_clone = Arc::clone(&received);
        let sink: AcpEventSink = Arc::new(move |owner, val| {
            recv_clone.lock().unwrap().push((owner.to_owned(), val));
        });
        let hub = AcpHub::new(ImageStore::new(), sink);
        let (handle, _action_rx, event_tx) = ConversationClientHandle::detached();

        hub.attach_handle(
            "sub-1".into(),
            "smelt.sessions".into(),
            RuntimeKey::Local,
            "s-1".into(),
            handle,
        );

        let mut s1 = fallback_snapshot("", 0);
        s1.phase = DaemonPhase::Thinking;
        s1.entries = vec![AcpEntry::Assistant {
            text: "a".into(),
            thought: false,
        }];
        s1.entries_total = 1;
        event_tx
            .try_send(ConversationClientEvent::Snapshot(Box::new(s1.clone())))
            .unwrap();

        // 短时间内连发 20 帧增量文本（phase 未变），应被节流合并。
        for i in 2..=21 {
            let mut s = s1.clone();
            s.entries = vec![AcpEntry::Assistant {
                text: "a".repeat(i),
                thought: false,
            }];
            s.snapshot_revision = i as u64;
            event_tx
                .try_send(ConversationClientEvent::Snapshot(Box::new(s)))
                .unwrap();
        }

        // 等节流窗落定
        std::thread::sleep(Duration::from_millis(80));
        let events = received.lock().unwrap().clone();
        assert!(!events.is_empty());
        assert!(
            events.len() <= 4,
            "21 rapid frames should coalesce, got {}",
            events.len()
        );
        let last_text = events.last().unwrap().1["snapshot"]["entries"][0]["Assistant"]["text"]
            .as_str()
            .unwrap();
        assert_eq!(last_text.len(), 21);

        // 回合结束（phase 跳变）立即发出
        let mut ended = s1;
        ended.phase = DaemonPhase::Idle;
        ended.snapshot_revision = 99;
        event_tx
            .try_send(ConversationClientEvent::Snapshot(Box::new(ended)))
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let final_phase = received.lock().unwrap().last().unwrap().1["snapshot"]["phase"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(final_phase, "idle");
    }

    /// 节流窗里先到的完整帧被后到的省略信封帧顶掉时，信封的更新不能丢；之后单独到的
    /// 省略帧发给插件时去掉信封键，插件照旧沿用手里的那份。
    #[test]
    fn coalescing_keeps_envelope_of_replaced_full_frame_and_strips_omitted_envelope() {
        let received = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let recv_clone = Arc::clone(&received);
        let sink: AcpEventSink = Arc::new(move |owner, val| {
            recv_clone.lock().unwrap().push((owner.to_owned(), val));
        });
        let hub = AcpHub::new(ImageStore::new(), sink);
        let (handle, _action_rx, event_tx) = ConversationClientHandle::detached();
        hub.attach_handle(
            "sub-1".into(),
            "smelt.sessions".into(),
            RuntimeKey::Local,
            "s-1".into(),
            handle,
        );
        let send = |snap: ConversationSnapshot| {
            event_tx
                .try_send(ConversationClientEvent::Snapshot(Box::new(snap)))
                .unwrap();
        };

        let mut full = fallback_snapshot("", 0);
        full.phase = DaemonPhase::Thinking;
        full.available_commands = vec![("a".into(), "A".into())];
        full.snapshot_revision = 1;
        send(full.clone());

        // 同一节流窗里：命令变了的完整帧，紧跟一个省略信封的正文帧。
        full.available_commands.push(("b".into(), "B".into()));
        full.snapshot_revision = 2;
        send(full.clone());
        let mut omitted = fallback_snapshot("", 0);
        omitted.phase = DaemonPhase::Thinking;
        omitted.envelope_omitted = true;
        omitted.snapshot_revision = 3;
        send(omitted.clone());
        std::thread::sleep(Duration::from_millis(60));

        let last = received.lock().unwrap().last().unwrap().1["snapshot"].clone();
        assert_eq!(last["snapshot_revision"], 3);
        assert_eq!(
            last["available_commands"].as_array().map(Vec::len),
            Some(2),
            "被顶掉的完整帧里的命令更新丢了：{last}"
        );

        // 单独到的省略帧：去掉信封键，留着正文和相位。
        omitted.snapshot_revision = 4;
        send(omitted);
        std::thread::sleep(Duration::from_millis(60));
        let last = received.lock().unwrap().last().unwrap().1["snapshot"].clone();
        assert_eq!(last["snapshot_revision"], 4);
        assert_eq!(last["envelope_omitted"], true);
        for key in ConversationSnapshot::ENVELOPE_KEYS {
            assert!(last.get(*key).is_none(), "省略帧不该带信封键 {key}");
        }
        assert_eq!(last["phase"], "thinking");
    }

    #[test]
    fn park_and_reclaim_reuses_live_session_without_disconnect() {
        let received = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let recv_clone = Arc::clone(&received);
        let sink: AcpEventSink = Arc::new(move |owner, val| {
            recv_clone.lock().unwrap().push((owner.to_owned(), val));
        });
        let hub = AcpHub::new(ImageStore::new(), sink);
        let (handle, action_rx, event_tx) = ConversationClientHandle::detached();

        hub.attach_handle(
            "sub-old".into(),
            "smelt.sessions".into(),
            RuntimeKey::Local,
            "s-1".into(),
            handle,
        );
        let mut snap = fallback_snapshot("", 0);
        snap.phase = DaemonPhase::Idle;
        snap.session_title = Some("demo".into());
        event_tx
            .try_send(ConversationClientEvent::Snapshot(Box::new(snap)))
            .unwrap();
        std::thread::sleep(Duration::from_millis(30));

        hub.unsubscribe("smelt.sessions", "sub-old", Some("pane-1".into()));
        let reclaimed = hub.try_reclaim(
            "pane-1",
            "smelt.sessions",
            &RuntimeKey::Local,
            "s-1",
            "sub-new",
        );
        let cached = reclaimed
            .expect("should reclaim parked session")
            .expect("should have cached snapshot");
        assert_eq!(cached["session_title"], "demo");

        // 用新 sub_id 仍能向同一条 action_rx 发动作
        hub.send_action("smelt.sessions", "sub-new", json!("Cancel"), &[])
            .unwrap();
        assert!(matches!(action_rx.try_recv(), Ok(AcpUserAction::Cancel)));
    }

    #[test]
    fn reclaim_requests_full_refresh_instead_of_replaying_envelope_omitted_delta() {
        let sink: AcpEventSink = Arc::new(|_, _| {});
        let hub = AcpHub::new(ImageStore::new(), sink);
        let (handle, action_rx, event_tx) = ConversationClientHandle::detached();

        hub.attach_handle(
            "sub-old".into(),
            "smelt.sessions".into(),
            RuntimeKey::Local,
            "s-delta".into(),
            handle,
        );
        let mut snap = fallback_snapshot("", 5);
        snap.phase = DaemonPhase::Thinking;
        snap.envelope_omitted = true;
        event_tx
            .try_send(ConversationClientEvent::Snapshot(Box::new(snap)))
            .unwrap();
        std::thread::sleep(Duration::from_millis(30));

        hub.unsubscribe("smelt.sessions", "sub-old", Some("pane-delta".into()));
        let reclaimed = hub
            .try_reclaim(
                "pane-delta",
                "smelt.sessions",
                &RuntimeKey::Local,
                "s-delta",
                "sub-new",
            )
            .expect("should reclaim parked session");
        assert!(
            reclaimed.is_none(),
            "不应把 envelope_omitted/offset>0 的增量帧直接回放给新挂载组件"
        );
        assert!(matches!(action_rx.try_recv(), Ok(AcpUserAction::Refresh)));
    }

    #[test]
    fn send_action_resolves_image_refs_and_enforces_owner() {
        let sink: AcpEventSink = Arc::new(|_, _| {});
        let images = ImageStore::new();
        let meta = images.store_bytes(make_png_bytes(), None).unwrap();
        let hub = AcpHub::new(images, sink);
        let (handle, action_rx, _event_tx) = ConversationClientHandle::detached();

        hub.attach_handle(
            "sub-1".into(),
            "smelt.sessions".into(),
            RuntimeKey::Local,
            "s-1".into(),
            handle,
        );

        // 别的插件无权向该订阅发动作
        assert!(
            hub.send_action("smelt.other", "sub-1", json!("Cancel"), &[])
                .is_err()
        );

        hub.send_action(
            "smelt.sessions",
            "sub-1",
            json!({ "Prompt": { "text": "check", "images": [] } }),
            &[meta.ref_id],
        )
        .unwrap();
        let Ok(AcpUserAction::Prompt { text, images, .. }) = action_rx.try_recv() else {
            panic!("expected Prompt action");
        };
        assert_eq!(text, "check");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].mime, "image/png");
        assert!(!images[0].data_b64.is_empty());

        hub.remove_owner("smelt.sessions");
        assert!(
            hub.send_action("smelt.sessions", "sub-1", json!("Cancel"), &[])
                .is_err()
        );
    }
}
