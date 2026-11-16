//! 守护自己把 ACP runtime 拉回可用。客户端只发命令、只看快照，这些都不归它管。
//!
//! - **断线自动重连**：provider 的字节流意外断开（不是用户关闭，也不是 provider 自己报
//!   失败）时，按记录的启动规格重新拉起，带上自己的历史身份。连上就断的会话不能
//!   无限重来：连续 [`AUTO_RECONNECT_ATTEMPTS`] 次都没撑住就停下，相位留在结束，原因
//!   写清楚，用户可以手动重新开始。连上之后撑过 [`RECONNECT_STABLE_AFTER`] 再断才算
//!   恢复过，次数回满。
//! - **就绪后补做**：每次拉起 runtime（新建、重启、重连、闲置唤醒），等它就绪后先把
//!   会话配置写回去：新建时是客户端带来的初始配置，之后是拉起前那一刻生效的配置。
//!   再发新建时带来的首条消息。首条消息发出之前，对外把就绪后的空闲报成思考中。

use super::*;
use crate::acp_idle::SuspendedConfig;

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

/// 每次拉起各起一条：等这一代就绪后做 [`run_after_ready`]。没起来、换了代或超时就
/// 原样留给下一次拉起；用到 runtime 的动作也会先把它做完（见 `wake_suspended_acp_session`）。
pub(crate) fn spawn_after_ready(
    slot: Arc<AcpSlot<AcpSession>>,
    id: String,
    generation: u64,
    subscribers: EventHubHandle,
) {
    if !after_ready_pending(&slot.value) {
        return;
    }
    let label = id.clone();
    let spawned = thread::Builder::new()
        .name("smelt-acp-after-ready".into())
        .spawn(move || {
            let sess = &slot.value;
            let current = || sess.connection_generation.load(Ordering::SeqCst) == generation;
            // `reduced` 每改一次醒一次，超时只是上限。换代时拉起会重置归约状态，也会叫醒。
            let (reduced, _) = sess
                .reduced
                .wait_timeout_while(crate::acp_idle::ACP_WAKE_READY_TIMEOUT, |reduced| {
                    current() && crate::acp_idle::acp_ready_outcome(reduced).is_none()
                });
            let ready = matches!(crate::acp_idle::acp_ready_outcome(&reduced), Some(Ok(())));
            drop(reduced);
            if ready && current() {
                run_after_ready(&id, sess, &subscribers);
            }
        });
    if let Err(error) = spawned {
        eprintln!("[acp] 会话 {label} 起就绪后补做线程失败：{error}");
    }
}

/// 把就绪后要补的做掉：先写回配置、等 agent 确认，再发首条消息。调用方确认 runtime
/// 已就绪。后台那条正在做时在门上等它做完；做完的取走了就没有了，不会做两遍。
pub(crate) fn run_after_ready(id: &str, sess: &AcpSession, subscribers: &EventHubHandle) {
    let _gate = sess.after_ready_gate.lock();
    let config = sess.after_ready.lock().config.take();
    if let Some(config) = config {
        crate::acp_idle::restore_suspended_config(id, sess, &config, subscribers);
    }
    // 发出之后才清：在这之前对外一直报思考中，中间不会闪一下空闲。
    let Some(prompt) = sess.after_ready.lock().prompt.clone() else {
        return;
    };
    crate::dlog(&format!("acp 发新建时带来的首条消息 id={id}"));
    let result = dispatch_conversation_input(
        sess,
        smelt_core::conversation::ConversationInput::new(prompt.text, prompt.images),
        subscribers,
    );
    sess.after_ready.lock().prompt = None;
    if let Err(error) = result {
        eprintln!("[acp] 会话 {id} 发首条消息失败：{}", error.message);
    }
    update_acp_daemon_state(sess, subscribers);
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
            &format!("连接接连中断，自动重连了 {AUTO_RECONNECT_ATTEMPTS} 次都没撑住"),
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
                give_up_reconnect(sess, &task_subscribers, &format!("自动重连失败：{error}"));
            }
        });
    if let Err(error) = spawned {
        give_up_reconnect(sess, subscribers, &format!("自动重连失败：{error}"));
    }
}

/// 不再自动重连：相位留在结束，原因换成说得清的那句。不摘槽位，用户可以手动重新开始。
fn give_up_reconnect(sess: &AcpSession, subscribers: &EventHubHandle, reason: &str) {
    let mut reduced = sess.reduced.lock();
    smelt_core::acp_session::force_end(
        &mut reduced,
        smelt_core::acp_session::AcpEndKind::ProviderFailed,
        reason,
    );
    drop(reduced);
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
