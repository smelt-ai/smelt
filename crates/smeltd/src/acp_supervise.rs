//! 守护自己把 ACP runtime 拉回可用。客户端只发命令、只看快照，这些都不归它管。
//!
//! - **断线自动重连**：provider 的字节流意外断开（不是用户关闭，也不是 provider 自己报
//!   失败）时，按记录的启动规格重新拉起，带上自己的历史身份。连上就断的会话不能
//!   无限重来：连续 [`AUTO_RECONNECT_ATTEMPTS`] 次都没撑住就停下，相位留在结束，原因
//!   写清楚。用户再发消息就会重新拉起（见 `acp_idle::launch_for_user_input`）。连上之后
//!   撑过 [`RECONNECT_STABLE_AFTER`] 再断才算恢复过，次数回满。
//! - **就绪后补做**：每次拉起 runtime（新建、重启、重连、闲置唤醒、发消息拉起）都关上
//!   启动闸，等它就绪后先把会话配置写回去：新建时是客户端带来的初始配置，之后是拉起前
//!   那一刻生效的配置。再发新建时带来的首条消息，然后开闸，放拉起期间排进来的消息。
//!   首条消息发出之前，对外把就绪后的空闲报成思考中。

use super::*;
use crate::acp_idle::SuspendedConfig;
use smelt_core::acp_session::ConversationEndReason;

/// 连续自动重连的上限。
const AUTO_RECONNECT_ATTEMPTS: u32 = 6;
/// 连上后撑过这么久再断，才算真恢复过，次数回满。
const RECONNECT_STABLE_AFTER: Duration = Duration::from_secs(30);

/// runtime 就绪后守护要替这段对话做完的事。
#[derive(Default)]
pub(crate) struct AcpAfterReady {
    config: Option<SuspendedConfig>,
    prompt: Option<smelt_core::acp_session::AcpInitialPrompt>,
}

impl AcpAfterReady {
    fn is_empty(&self) -> bool {
        self.config.is_none() && self.prompt.is_none()
    }

    /// 首条消息还没发出。
    pub(crate) fn prompt_pending(&self) -> bool {
        self.prompt.is_some()
    }
}

/// 客户端这次打开要拉起 runtime，并带来了初始配置或首条消息。
///
/// 配置的权威在守护：还有没写进去的那份，或上一代 runtime 报过配置（拉起时
/// [`remember_relaunch_config`] 会抓下来，里面有别的端改过的），就用守护自己的。客户端
/// 带来的只是它那份缓存，守护什么都不知道时（新建、守护重启过）才用。首条消息只有
/// 客户端知道，但它也只是「还没看到发出去」：守护还欠着一条、或这段对话已经有正文
/// （发过了，客户端没赶上看到），就不再收，免得同一条发两遍。
pub(crate) fn remember_requested_after_ready(sess: &AcpSession, req: &AcpOpenRequest) {
    // 先读归约状态再锁待办：两把锁不同时持（见 `update_acp_daemon_state`）。
    let (knows_config, has_entries) = {
        let reduced = sess.reduced.lock();
        (
            !SuspendedConfig::capture(&reduced).is_empty(),
            reduced.entries_total() > 0,
        )
    };
    let mut pending = sess.after_ready.lock();
    if !req.initial_config.is_empty() && !knows_config {
        pending
            .config
            .get_or_insert_with(|| SuspendedConfig::from_values(&req.initial_config));
    }
    if let Some(prompt) = &req.initial_prompt
        && pending.prompt.is_none()
        && !has_entries
    {
        pending.prompt = Some(prompt.clone());
    }
}

/// 拉起前那一刻生效的配置，就绪后写回。还有没写进去的那份（新建时带来的初始配置，
/// 或上一次没起来留下的）就不覆盖。
pub(crate) fn remember_relaunch_config(sess: &AcpSession, captured: SuspendedConfig) {
    if captured.is_empty() {
        return;
    }
    sess.after_ready.lock().config.get_or_insert(captured);
}

/// 还有就绪后要补做的事。
pub(crate) fn after_ready_pending(sess: &AcpSession) -> bool {
    !sess.after_ready.lock().is_empty()
}

/// 用户发消息时带的配置（输入框里刚改的模型、权限模式等）。runtime 还在启动、启动闸
/// 关着时直接下发，会被随后写回的旧配置盖掉，所以并进就绪后补做的那份，排在旧配置
/// 后面生效。返回 false 表示启动闸已开，调用方照常下发。要持着回合收尾锁做：开闸前
/// [`run_after_ready`] 在同一把锁里复核这份为空。
pub(crate) fn hold_submit_config(
    _turn_completion: &std::sync::MutexGuard<'_, ()>,
    sess: &AcpSession,
    values: &[(String, String)],
) -> bool {
    if !sess.startup_hold.load(Ordering::SeqCst) {
        return false;
    }
    if values.is_empty() {
        return true;
    }
    let submitted = SuspendedConfig::from_values(values);
    let mut pending = sess.after_ready.lock();
    match pending.config.as_mut() {
        Some(config) => config.override_with(submitted),
        None => pending.config = Some(submitted),
    }
    true
}

/// 这一代就绪了：起一条线程做 [`run_after_ready`]。由 drain 在第一次就绪时叫，调用方
/// 持着回合收尾锁，补做要等 agent 确认，不能在 drain 上做。换了代就不做，留给下一代。
/// 用到 runtime 的动作也会先把它做完（见 `wake_suspended_acp_session`）。
pub(crate) fn spawn_after_ready(
    slot: Arc<AcpSlot<AcpSession>>,
    id: String,
    generation: u64,
    subscribers: EventHubHandle,
) {
    let label = id.clone();
    let spawned = thread::Builder::new()
        .name("smelt-acp-after-ready".into())
        .spawn(move || {
            let sess = &slot.value;
            run_after_ready_for_generation(&id, sess, Some(generation), &subscribers);
        });
    if let Err(error) = spawned {
        eprintln!("[acp] 会话 {label} 起就绪后补做线程失败：{error}");
    }
}

fn after_ready_still_valid(sess: &AcpSession, expected_generation: Option<u64>) -> bool {
    expected_generation
        .is_none_or(|generation| sess.connection_generation.load(Ordering::SeqCst) == generation)
        && matches!(
            crate::acp_idle::acp_ready_outcome(&sess.reduced.lock()),
            Some(Ok(()))
        )
}

/// 把就绪后要补的做掉：先写回配置、等 agent 确认，再发首条消息，最后开启动闸，放拉起
/// 期间排进来的消息。调用方确认 runtime 已就绪。后台那条正在做时在门上等它做完；做完
/// 的取走了就没有了，不会做两遍。
pub(crate) fn run_after_ready(id: &str, sess: &AcpSession, subscribers: &EventHubHandle) {
    run_after_ready_for_generation(id, sess, None, subscribers);
}

fn run_after_ready_for_generation(
    id: &str,
    sess: &AcpSession,
    expected_generation: Option<u64>,
    subscribers: &EventHubHandle,
) {
    let _gate = sess.after_ready_gate.lock();
    // 写回期间用户又在输入框里改了配置（见 [`hold_submit_config`]），接着写，最后一份为准。
    // 那边在回合收尾锁里、启动闸关着时才往这里放，所以拿着锁复核为空再开闸，不会漏。
    let turn_completion = loop {
        // 取出来再写：写回要等 agent 确认，事件线程归并确认时也要拿 after_ready。
        // `while let` 的锁守卫活到循环体结束，在条件里取会拿着锁等满超时。
        loop {
            if !after_ready_still_valid(sess, expected_generation) {
                return;
            }
            let Some(config) = sess.after_ready.lock().config.take() else {
                break;
            };
            crate::acp_idle::restore_suspended_config(id, sess, &config, subscribers);
        }
        let turn_completion = sess.turn_completion.lock();
        if !after_ready_still_valid(sess, expected_generation) {
            return;
        }
        if sess.after_ready.lock().config.is_none() {
            break turn_completion;
        }
    };
    let prompt = sess.after_ready.lock().prompt.clone();
    let was_held = sess.startup_hold.swap(false, Ordering::SeqCst);
    let Some(prompt) = prompt else {
        // 没有首条消息：当作刚就绪放闸，排着的第一条接着走。别人已经开过闸就没事可做。
        if was_held {
            settle_acp_turn_locked(sess, false, true, false, subscribers);
        }
        return;
    };
    crate::dlog(&format!("acp 发新建时带来的首条消息 id={id}"));
    let mut input = smelt_core::conversation::ConversationInput::new(prompt.text, prompt.images);
    let preset_used = prepare_conversation_input(sess, &mut input);
    let result = send_first_prompt_locked(sess, input.text, input.images, subscribers);
    // 发出之后才清：在这之前对外一直报思考中，中间不会闪一下空闲。
    sess.after_ready.lock().prompt = None;
    drop(turn_completion);
    match result {
        Ok(()) if preset_used => consume_agent_preset(sess, subscribers),
        Ok(()) => {}
        Err(error) => eprintln!("[acp] 会话 {id} 发首条消息失败：{error}"),
    }
    update_acp_daemon_state(sess, subscribers);
}

/// 首条消息要排在拉起期间进来的消息前面：闸门空着就直接占住发出，排着的等它的回合
/// 结束再走；被占着就排到队首。会话命令（`/reload` 这类）不是模型输入，交给 Pi 处理
/// 后照常放闸。调用方持有回合收尾锁，启动闸已开。
fn send_first_prompt_locked(
    sess: &AcpSession,
    text: String,
    images: Vec<smelt_core::acp_conn::PromptImage>,
    subscribers: &EventHubHandle,
) -> Result<(), &'static str> {
    if dispatch_pi_session_command(sess, &text, images.clone())? {
        settle_acp_turn_locked(sess, false, true, false, subscribers);
        return Ok(());
    }
    if sess.prompt_in_flight.swap(true, Ordering::SeqCst) {
        let id = next_pending_prompt_id(sess);
        edit_pending_prompts(sess, |queue| {
            queue.push_front(QueuedAcpPrompt {
                id,
                text,
                images,
                delivery_id: None,
                origin: AcpPromptOrigin::User,
                echoed: false,
            })
        });
        push_acp_snapshot_since(sess, false, None);
        return Ok(());
    }
    send_acp_prompt_reserved(
        sess,
        text,
        images,
        None,
        AcpPromptOrigin::User,
        false,
        subscribers,
    )
}

/// 自动重连的账本。
#[derive(Default)]
pub(crate) struct AcpReconnectBudget {
    /// 自上次真恢复以来连续失败的次数。
    failures: u32,
    /// 当前这一代第一次可用的时刻；断开时清掉。
    usable_since: Option<Instant>,
}

impl AcpReconnectBudget {
    /// 这一代可用了。只记第一次。
    pub(crate) fn usable(&mut self, now: Instant) {
        self.usable_since.get_or_insert(now);
    }

    /// 断开时结算：这一代撑够了就回满。返回这次之前已经连续失败了几次，用完返回 `None`。
    fn disconnected(&mut self, now: Instant) -> Option<u32> {
        if let Some(since) = self.usable_since.take()
            && now.saturating_duration_since(since) >= RECONNECT_STABLE_AFTER
        {
            self.failures = 0;
        }
        if self.failures >= AUTO_RECONNECT_ATTEMPTS {
            return None;
        }
        let previous = self.failures;
        self.failures += 1;
        Some(previous)
    }

    /// 客户端明确要求拉起：之前的失败不再算数。
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

/// provider 字节流意外断开、相位已经收成 `TransportDisconnected` 之后调。调用方持有
/// 生命周期锁，已经收掉旧 runtime。退避后在锁里复核：槽位还是这个、没换代、仍是这次
/// 断开，才按记录的启动规格重新拉起；拉起后的配置由 [`run_after_ready`] 写回。
pub(crate) fn schedule_acp_auto_reconnect(
    slot: &Arc<AcpSlot<AcpSession>>,
    id: &str,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) {
    let sess = &slot.value;
    let generation = sess.connection_generation.load(Ordering::SeqCst);
    // 先放下账本锁：放弃时要更新状态，那里会在归约锁里再拿它。
    let budget = sess.reconnect.lock().disconnected(Instant::now());
    let Some(previous_failures) = budget else {
        give_up_reconnect(
            sess,
            subscribers,
            ConversationEndReason::ReconnectExhausted {
                attempts: AUTO_RECONNECT_ATTEMPTS,
            },
            String::new(),
        );
        return;
    };
    let task_slot = Arc::clone(slot);
    let task_id = id.to_string();
    let task_sessions = Arc::clone(acp_sessions);
    let task_subscribers = subscribers.clone();
    let spawned = thread::Builder::new()
        .name("smelt-acp-reconnect".into())
        .spawn(move || {
            // 轮询例外：provider 子进程断了，没有「可以再拉起了」的事件可等；周期 250ms
            // 起翻倍，上限 8s，最多 6 次。
            thread::sleep(smelt_core::daemon_state::daemon_reconnect_backoff(
                previous_failures,
            ));
            let sess = &task_slot.value;
            let _lifecycle = task_slot.lifecycle.lock();
            if !task_sessions.is_current(&task_id, &task_slot)
                || sess.connection_generation.load(Ordering::SeqCst) != generation
            {
                return;
            }
            {
                let reduced = sess.reduced.lock();
                if reduced.phase != smelt_core::daemon_state::DaemonPhase::Dead
                    || reduced.end_kind
                        != smelt_core::acp_session::AcpEndKind::TransportDisconnected
                {
                    return;
                }
            }
            crate::dlog(&format!(
                "acp 自动重连 id={task_id} 第 {} 次",
                previous_failures + 1
            ));
            if let Err(error) = relaunch_recorded_acp_runtime(
                &task_id,
                &task_slot,
                &task_sessions,
                &task_subscribers,
            ) {
                give_up_reconnect(
                    sess,
                    &task_subscribers,
                    ConversationEndReason::ReconnectFailed,
                    error.to_string(),
                );
            }
        });
    if let Err(error) = spawned {
        give_up_reconnect(
            sess,
            subscribers,
            ConversationEndReason::ReconnectFailed,
            error.to_string(),
        );
    }
}

/// 不再自动重连：相位留在结束，原因换成说得清的那句。不摘槽位，用户可以手动重新开始。
fn give_up_reconnect(
    sess: &AcpSession,
    subscribers: &EventHubHandle,
    reason: ConversationEndReason,
    detail: String,
) {
    let mut reduced = sess.reduced.lock();
    smelt_core::acp_session::force_end_because(
        &mut reduced,
        smelt_core::acp_session::AcpEndKind::ProviderFailed,
        reason,
        detail,
    );
    drop(reduced);
    return_queued_user_prompts(sess);
    push_acp_snapshot_since(sess, true, None);
    update_acp_daemon_state(sess, subscribers);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(value: &str) -> SuspendedConfig {
        SuspendedConfig::from_values(&[("model".into(), value.into())])
    }

    fn open_with_initial(id: &str, value: &str) -> AcpOpenRequest {
        parse_acp_open_request(&serde_json::json!({
            "id": id,
            "launch": {"command": "pi"},
            "initial_config": [["model", value]],
            "initial_prompt": {"text": "hi"}
        }))
        .unwrap()
    }

    /// 守护什么都不知道（新建、守护重启过）：用客户端带来的。拉起前抓到的空配置不顶掉它。
    #[test]
    fn a_session_the_daemon_knows_nothing_about_takes_the_requested_config() {
        let sess = make_acp_session("acp-after-ready", None, None, None);
        assert!(!after_ready_pending(&sess));
        remember_requested_after_ready(
            &sess,
            &open_with_initial("acp-after-ready", "deepseek/flash"),
        );
        remember_relaunch_config(&sess, SuspendedConfig::capture(&sess.reduced.lock()));
        let pending = sess.after_ready.lock();
        assert_eq!(pending.config, Some(model("deepseek/flash")));
        assert!(pending.prompt_pending());
    }

    /// 上一代 runtime 报过配置（可能是手机上改的）：守护自己的为准，客户端那份缓存不顶掉它。
    #[test]
    fn the_daemons_own_config_wins_over_the_clients_cached_copy() {
        let sess = make_acp_session("acp-known-config", None, None, None);
        sess.reduced.lock().model = Some(smelt_core::acp_conn::ModelState {
            config_id: "model".into(),
            current_value: "openai/gpt-5".into(),
            current_name: "openai/gpt-5".into(),
            options: Vec::new(),
            provider_groups: Vec::new(),
        });
        remember_requested_after_ready(
            &sess,
            &open_with_initial("acp-known-config", "deepseek/flash"),
        );
        remember_relaunch_config(&sess, SuspendedConfig::capture(&sess.reduced.lock()));
        let pending = sess.after_ready.lock();
        assert_eq!(pending.config, Some(model("openai/gpt-5")));
        assert!(pending.prompt_pending(), "首条消息只有客户端知道");
    }

    #[test]
    fn relaunch_config_waits_until_it_is_written() {
        let sess = make_acp_session("acp-relaunch-config", None, None, None);
        remember_relaunch_config(&sess, SuspendedConfig::default());
        assert!(!after_ready_pending(&sess), "没报过配置就没什么要写回");
        remember_relaunch_config(&sess, model("deepseek/flash"));
        // 上一次没起来，这次拉起前镜像已经清空；还没写回的那份留着。
        remember_relaunch_config(&sess, SuspendedConfig::default());
        assert_eq!(
            sess.after_ready.lock().config,
            Some(model("deepseek/flash"))
        );
    }

    #[test]
    fn a_hot_attach_request_without_initial_items_keeps_what_is_pending() {
        let sess = make_acp_session("acp-keep-pending", None, None, None);
        remember_relaunch_config(&sess, model("deepseek/flash"));
        let req = parse_acp_open_request(&serde_json::json!({
            "id": "acp-keep-pending",
            "launch": {"command": "pi"}
        }))
        .unwrap();
        remember_requested_after_ready(&sess, &req);
        let pending = sess.after_ready.lock();
        assert_eq!(pending.config, Some(model("deepseek/flash")));
        assert!(!pending.prompt_pending());
    }

    /// 首条消息已经发出去（对话有正文）客户端还没看到，重开时又带来：不再发第二遍。
    #[test]
    fn a_requested_prompt_is_ignored_once_the_conversation_has_entries() {
        let sess = make_acp_session("acp-prompt-sent", None, None, None);
        sess.reduced
            .lock()
            .entries_mut()
            .push(smelt_core::acp_chat::AcpEntry::User("hi".into()));
        remember_requested_after_ready(&sess, &open_with_initial("acp-prompt-sent", "m"));
        assert!(!sess.after_ready.lock().prompt_pending());
    }

    /// 守护还欠着一条：后来的请求不顶掉它，也不排第二条。
    #[test]
    fn a_pending_prompt_is_not_replaced_by_a_later_request() {
        let sess = make_acp_session("acp-prompt-pending", None, None, None);
        remember_requested_after_ready(&sess, &open_with_initial("acp-prompt-pending", "m"));
        let later = parse_acp_open_request(&serde_json::json!({
            "id": "acp-prompt-pending",
            "launch": {"command": "pi"},
            "initial_prompt": {"text": "again"}
        }))
        .unwrap();
        remember_requested_after_ready(&sess, &later);
        let pending = sess.after_ready.lock();
        assert_eq!(
            pending.prompt.as_ref().map(|prompt| prompt.text.as_str()),
            Some("hi")
        );
    }

    /// 有 runtime、刚就绪的会话。驱动收到的命令从返回的通道里读。
    fn running_session(
        id: &str,
    ) -> (
        AcpSession,
        smol::channel::Receiver<smelt_core::acp_conn::ConversationCommand>,
    ) {
        let sess = make_acp_session(id, None, None, None);
        sess.reduced.lock().phase = smelt_core::daemon_state::DaemonPhase::Idle;
        let (cmd_tx, cmd_rx) = smol::channel::unbounded();
        let (_event_tx, event_rx) = smol::channel::unbounded();
        *sess.handle.lock() = Some(smelt_core::acp_conn::ConversationHandle {
            cmd_tx,
            event_rx,
            stdio: Arc::new(std::sync::Mutex::new(None)),
            in_flight_rpc: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            supports_mid_turn_input: false,
            supports_compaction: false,
            supports_native_queue: false,
            supports_rewind: false,
        });
        (sess, cmd_rx)
    }

    fn sent_prompts(
        cmd_rx: &smol::channel::Receiver<smelt_core::acp_conn::ConversationCommand>,
    ) -> Vec<String> {
        std::iter::from_fn(|| cmd_rx.try_recv().ok())
            .filter_map(|command| match command {
                smelt_core::acp_conn::ConversationCommand::Prompt { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    fn user_prompt(text: &str) -> smelt_core::acp_session::AcpUserAction {
        smelt_core::acp_session::AcpUserAction::Prompt {
            text: text.into(),
            images: Vec::new(),
            delivery_id: None,
        }
    }

    /// 拉起期间发的消息不抢在首条消息前面：先排队，补做发出首条后才轮到它。
    #[test]
    fn prompts_sent_while_starting_wait_behind_the_first_prompt() {
        let subscribers = crate::new_event_hub();
        let (sess, cmd_rx) = running_session("acp-starting");
        sess.startup_hold.store(true, Ordering::SeqCst);
        sess.after_ready.lock().prompt = Some(smelt_core::acp_session::AcpInitialPrompt {
            text: "first".into(),
            images: Vec::new(),
        });

        apply_acp_user_action(&sess, user_prompt("second"), &subscribers).unwrap();
        assert!(sent_prompts(&cmd_rx).is_empty(), "启动闸关着不该发");
        assert_eq!(sess.reduced.lock().pending_prompts.len(), 1);
        assert!(!sess.prompt_in_flight.load(Ordering::SeqCst));

        run_after_ready("acp-starting", &sess, &subscribers);
        assert_eq!(sent_prompts(&cmd_rx), vec!["first".to_string()]);
        assert!(!sess.startup_hold.load(Ordering::SeqCst));
        assert!(!after_ready_pending(&sess));
        let queued = sess.reduced.lock().pending_prompts.clone();
        assert_eq!(queued.len(), 1, "第二条等首条的回合结束");
        assert_eq!(queued[0].text, "second");
    }

    /// 没有首条消息：补做完开闸，排着的第一条马上发。
    #[test]
    fn after_ready_without_a_first_prompt_releases_the_queue() {
        let subscribers = crate::new_event_hub();
        let (sess, cmd_rx) = running_session("acp-woken");
        sess.startup_hold.store(true, Ordering::SeqCst);
        apply_acp_user_action(&sess, user_prompt("hello again"), &subscribers).unwrap();
        assert!(sent_prompts(&cmd_rx).is_empty());

        run_after_ready("acp-woken", &sess, &subscribers);
        assert_eq!(sent_prompts(&cmd_rx), vec!["hello again".to_string()]);
        assert!(sess.reduced.lock().pending_prompts.is_empty());
    }

    /// 重放尚未结束或刚换到仍在连接的新代时，陈旧的补做线程不能提前把启动闸放开。
    #[test]
    fn stale_after_ready_does_not_release_startup_hold_while_replaying() {
        let subscribers = crate::new_event_hub();
        let (sess, cmd_rx) = running_session("acp-replaying");
        sess.startup_hold.store(true, Ordering::SeqCst);
        sess.reduced.lock().replaying_history = true;
        apply_acp_user_action(&sess, user_prompt("queued during replay"), &subscribers).unwrap();

        run_after_ready("acp-replaying", &sess, &subscribers);
        assert!(
            sess.startup_hold.load(Ordering::SeqCst),
            "重放未结束不得放开启动闸"
        );
        assert!(
            sent_prompts(&cmd_rx).is_empty(),
            "重放未结束不得抢先发出排队消息"
        );
        assert_eq!(sess.reduced.lock().pending_prompts.len(), 1);
    }

    /// 即使启动闸未关，历史重放进行中收到用户消息也必须排队，不能插进草稿中途直接发送。
    #[test]
    fn prompts_wait_while_replaying_history_even_if_startup_hold_is_clear() {
        let subscribers = crate::new_event_hub();
        let (sess, cmd_rx) = running_session("acp-replay-guard");
        sess.startup_hold.store(false, Ordering::SeqCst);
        sess.reduced.lock().replaying_history = true;

        apply_acp_user_action(&sess, user_prompt("wait for replay"), &subscribers).unwrap();
        assert!(
            sent_prompts(&cmd_rx).is_empty(),
            "重放进行中不能直接发 prompt"
        );
        assert_eq!(sess.reduced.lock().pending_prompts.len(), 1);
        assert!(!sess.prompt_in_flight.load(Ordering::SeqCst));
    }

    /// 启动期间用户在输入框里改的配置排在拉起前那份后面写，不被旧的盖掉；开闸后照常下发。
    #[test]
    fn submitted_config_while_starting_overrides_the_relaunch_config() {
        let sess = make_acp_session("acp-config-hold", None, None, None);
        remember_relaunch_config(
            &sess,
            SuspendedConfig::from_values(&[
                ("model".into(), "old".into()),
                ("mode".into(), "plan".into()),
            ]),
        );
        sess.startup_hold.store(true, Ordering::SeqCst);
        assert!(hold_submit_config(
            &sess.turn_completion.lock(),
            &sess,
            &[("model".into(), "new".into())],
        ));
        assert_eq!(
            sess.after_ready.lock().config,
            Some(SuspendedConfig::from_values(&[
                ("model".into(), "new".into()),
                ("mode".into(), "plan".into()),
            ]))
        );

        sess.startup_hold.store(false, Ordering::SeqCst);
        assert!(!hold_submit_config(
            &sess.turn_completion.lock(),
            &sess,
            &[("model".into(), "newer".into())],
        ));
    }

    /// 起不来：用户排着的消息回输入框，任务投来的留在队里等下次拉起。
    #[test]
    fn a_stopped_runtime_returns_queued_user_prompts_to_the_composer() {
        let subscribers = crate::new_event_hub();
        let (sess, _cmd_rx) = running_session("acp-stopped");
        sess.startup_hold.store(true, Ordering::SeqCst);
        apply_acp_user_action(&sess, user_prompt("one"), &subscribers).unwrap();
        apply_acp_user_action(
            &sess,
            smelt_core::acp_session::AcpUserAction::Prompt {
                text: "task".into(),
                images: Vec::new(),
                delivery_id: Some("delivery-1".into()),
            },
            &subscribers,
        )
        .unwrap();
        apply_acp_user_action(&sess, user_prompt("two"), &subscribers).unwrap();

        give_up_reconnect(
            &sess,
            &subscribers,
            ConversationEndReason::ReconnectFailed,
            String::new(),
        );
        let reduced = sess.reduced.lock();
        assert_eq!(reduced.composer_restore_texts, vec!["one", "two"]);
        assert_eq!(reduced.pending_prompts.len(), 1);
        assert_eq!(reduced.pending_prompts[0].text, "task");
    }

    #[test]
    fn reconnect_budget_runs_out_when_every_connection_drops_quickly() {
        let mut budget = AcpReconnectBudget::default();
        let start = Instant::now();
        for attempt in 0..AUTO_RECONNECT_ATTEMPTS {
            budget.usable(start);
            assert_eq!(budget.disconnected(start), Some(attempt));
        }
        budget.usable(start);
        assert_eq!(budget.disconnected(start), None);
    }

    #[test]
    fn reconnect_budget_refills_after_a_stable_connection() {
        let mut budget = AcpReconnectBudget::default();
        let start = Instant::now();
        for _ in 0..AUTO_RECONNECT_ATTEMPTS {
            assert!(budget.disconnected(start).is_some());
        }
        budget.usable(start);
        assert_eq!(
            budget.disconnected(start + RECONNECT_STABLE_AFTER),
            Some(0),
            "撑过稳定期再断，次数回满"
        );
    }

    #[test]
    fn reconnect_budget_does_not_refill_without_ever_becoming_usable() {
        let mut budget = AcpReconnectBudget::default();
        let start = Instant::now();
        assert_eq!(budget.disconnected(start), Some(0));
        assert_eq!(
            budget.disconnected(start + RECONNECT_STABLE_AFTER * 2),
            Some(1),
            "没连上过就不算恢复"
        );
    }

    #[test]
    fn reconnect_budget_reset_forgets_earlier_failures() {
        let mut budget = AcpReconnectBudget::default();
        let start = Instant::now();
        for _ in 0..AUTO_RECONNECT_ATTEMPTS {
            assert!(budget.disconnected(start).is_some());
        }
        assert_eq!(budget.disconnected(start), None);
        budget.reset();
        assert_eq!(budget.disconnected(start), Some(0));
    }
}
