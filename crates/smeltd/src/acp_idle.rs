//! 闲置会话挂起。
//!
//! 每场对话的驱动和它的 agent 子进程都在这个 smeltd 里。对话几个小时没人碰，
//! 这些进程也一直占着内存。这里把闲置够久的 runtime 停掉，也就是挂起，并丢掉
//! 内存里已经提交的历史前缀。回收线程睡到最早的闲置截止才醒；一个可能变闲置的
//! 会话都没有时就一直睡，直到下一次活动。
//!
//! 挂起只停进程、裁掉已提交前缀。会话本身留在注册表里。绑定、启动规格、
//! provider 会话占用都不动，客户端看到的仍是空闲。之后真要用 runtime 时，入口先调
//! [`wake_suspended_acp_session`]。会按记录的启动规格带 `--session` 重新拉起，
//! 等 Pi 重放完历史，再把动作交过去。往上翻历史走同一条唤醒。会用到 runtime 的
//! 情况包括：发消息、改配置、往上翻历史、跨智能体投递。客户端重新 `acp_open`
//! 时，`ensure_acp_session` 本来就会拉起没有 runtime 的会话。
//!
//! 只挂起确定安全的会话，条件是以下全部满足：
//! - 有活着的 runtime；
//! - 没有进行中的回合；
//! - 没有待审批或待回答的问题；
//! - 没有排队的输入；
//! - 没有在跑的后台任务；
//! - 不归自动化或插件驱动。
//!
//! 手机建的远程会话同样挂起。唤醒靠槽位里还在的启动规格；守护进程停掉之后，
//! 挂起的和没挂起的槽位都会消失，桌面端断线重连会 `acp_open` 重新拉起。
//!
//! 唤醒要把挂起那一刻的会话配置（模型、推理强度、权限模式等）写回新 runtime。
//! 新进程只会从 provider 自己的会话文件里找回一部分：Pi 的权限模式和脚本工具
//! 只活在驱动内存里，配了模型的智能体启动时还会被强制换回定义里的模型。挂起
//! 对用户应当不可见，所以由守护自己恢复，不指望客户端补发。

use super::*;

/// 没有任何活动这么久之后挂起。
pub(crate) const ACP_IDLE_SUSPEND_AFTER: Duration = Duration::from_secs(30 * 60);
/// 到点该挂起、却抢不到会话生命周期锁或 spawn 闸门时，隔多久再试。锁被占着说明
/// 有人正在操作这个会话（拉起、重启），这样的操作多半会记一次活动，把截止往后推。
/// 上限取改成事件驱动之前的扫描周期。
// 轮询例外：生命周期锁和 spawn 闸门放开时没有通知可等；周期 1 秒起翻倍，上限 1 分钟。
const ACP_IDLE_RETRY_FIRST: Duration = Duration::from_secs(1);
const ACP_IDLE_RETRY_MAX: Duration = Duration::from_secs(60);
/// 唤醒后等 Pi 恢复历史的上限。超时只让这一次动作失败，会话照常继续恢复。
const ACP_WAKE_READY_TIMEOUT: Duration = Duration::from_secs(120);
/// 唤醒后每一步配置恢复等 agent 确认的上限。超时只记日志，用户的动作照常送达。
const ACP_CONFIG_RESTORE_TIMEOUT: Duration = Duration::from_secs(10);
/// 驱动写配置失败时 `status_line` 的前缀（pi_rpc 与 acp_conn 同一句）。
const CONFIG_UPDATE_FAILURE_PREFIX: &str = "更新会话配置失败";

/// 会话的闲置账本。构造时记一次活动：刚建好的会话不能在第一轮扫描就被挂起。
pub(crate) struct AcpIdleState {
    last_activity_ms: AtomicU64,
    suspended: AtomicBool,
}

impl Default for AcpIdleState {
    fn default() -> Self {
        Self {
            last_activity_ms: AtomicU64::new(crate::event_hub::now_ms()),
            suspended: AtomicBool::new(false),
        }
    }
}

impl AcpIdleState {
    pub(crate) fn touch(&self) {
        self.last_activity_ms
            .store(crate::event_hub::now_ms(), Ordering::SeqCst);
        REAPER_BELL.ring();
    }

    pub(crate) fn is_suspended(&self) -> bool {
        self.suspended.load(Ordering::SeqCst)
    }

    /// 任何一次拉起 runtime 都结束挂起，并重新计时。
    pub(crate) fn mark_running(&self) {
        self.suspended.store(false, Ordering::SeqCst);
        self.touch();
    }

    fn mark_suspended(&self) {
        self.suspended.store(true, Ordering::SeqCst);
    }

    fn idle_for(&self, now_ms: u64) -> Duration {
        Duration::from_millis(now_ms.saturating_sub(self.last_activity_ms.load(Ordering::SeqCst)))
    }

    /// 从这时起算闲置够久（毫秒时间戳）。
    fn idle_deadline_ms(&self, idle_after: Duration) -> u64 {
        idle_deadline_ms(self.last_activity_ms.load(Ordering::SeqCst), idle_after)
    }
}

pub(crate) fn idle_deadline_ms(last_activity_ms: u64, idle_after: Duration) -> u64 {
    last_activity_ms.saturating_add(u64::try_from(idle_after.as_millis()).unwrap_or(u64::MAX))
}

/// 回收线程下一次该醒的时刻（毫秒时间戳）：最早的闲置截止，和抢锁失败后的重试，
/// 取早的那个。`None` 表示没有会话可能变闲置，一直睡到下一次活动。
pub(crate) fn next_reaper_wake_ms(
    idle_deadlines_ms: impl IntoIterator<Item = u64>,
    retry_at_ms: Option<u64>,
) -> Option<u64> {
    idle_deadlines_ms.into_iter().chain(retry_at_ms).min()
}

/// 连续第 `attempt` 次（从 0 起）抢锁失败后隔多久再试：从 1 秒起翻倍，封顶 1 分钟。
pub(crate) fn idle_retry_delay(attempt: u32) -> Duration {
    ACP_IDLE_RETRY_FIRST
        .saturating_mul(1u32.checked_shl(attempt).unwrap_or(u32::MAX))
        .min(ACP_IDLE_RETRY_MAX)
}

/// 闲置回收线程的门铃。
///
/// 回收线程睡到最早的「最后活动 + 闲置期」。`touch` 只会把某个会话的截止推到
/// 「现在 + 闲置期」，不会早于线程手里已有的截止，所以线程有截止可等时不用叫它。
/// 只有它一个截止都没有、无限期睡着时，下一次活动才得把它叫起来，重新排上这个
/// 会话。扫描期间也在听：扫过之后才被拉起的会话不能漏掉。
struct ReaperBell {
    state: Mutex<ReaperBellState>,
    rung: Condvar,
}

struct ReaperBellState {
    listening: bool,
    rings: u64,
}

static REAPER_BELL: ReaperBell = ReaperBell {
    state: Mutex::new(ReaperBellState {
        listening: false,
        rings: 0,
    }),
    rung: Condvar::new(),
};

impl ReaperBell {
    fn ring(&self) {
        let mut state = self.state.lock().unwrap();
        if state.listening {
            state.rings = state.rings.wrapping_add(1);
            self.rung.notify_one();
        }
    }

    /// 扫描前调用。从这时起的活动都会记下，返回当前的计数。
    fn listen(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.listening = true;
        state.rings
    }

    /// 有截止就睡到截止，期间的活动只会把截止往后推，不必理会；没有截止就等
    /// `listen` 之后的第一次活动。
    fn wait(&self, seen: u64, timeout: Option<Duration>) {
        let mut state = self.state.lock().unwrap();
        match timeout {
            Some(timeout) => {
                state.listening = false;
                drop(self.rung.wait_timeout(state, timeout).unwrap());
            }
            None => {
                drop(
                    self.rung
                        .wait_while(state, |state| state.rings == seen)
                        .unwrap(),
                );
            }
        }
    }
}

/// 这个动作要不要真正的 runtime。只读镜像的动作不必为它拉起进程。取消闲置会话
/// 本来就没有可取消的。刷新模型目录由 GUI 在登录新 provider 后广播给所有对话，
/// 挂起的会话醒来时本就会重新取一遍，不值得为它全部唤醒。有后台任务在跑的会话
/// 不会被挂起，所以挂起的会话也没有后台任务或子智能体可停。
pub(crate) fn acp_action_needs_runtime(action: &smelt_core::acp_session::AcpUserAction) -> bool {
    use smelt_core::acp_session::AcpUserAction;
    !matches!(
        action,
        AcpUserAction::Refresh
            | AcpUserAction::HistoryPage { .. }
            | AcpUserAction::Cancel
            | AcpUserAction::RefreshModels
            | AcpUserAction::StopBackgroundTask { .. }
            | AcpUserAction::StopSubagent { .. }
            | AcpUserAction::PeerReply { .. }
    )
}

/// 会话投影里拦住挂起的第一条原因。`None` 表示对话安静，停掉进程不丢任何东西。
pub(crate) fn reduced_suspend_blocker(
    reduced: &smelt_core::acp_session::AcpSessionState,
) -> Option<&'static str> {
    use smelt_core::acp_session::BackgroundTaskStatus;
    use smelt_core::daemon_state::DaemonPhase;
    if reduced.phase != DaemonPhase::Idle {
        return Some("not idle");
    }
    if reduced.turn_started_at_ms.is_some() {
        return Some("turn running");
    }
    if !reduced.permissions.is_empty() || reduced.elicitation.is_some() {
        return Some("waiting for the user");
    }
    if reduced.compacting || reduced.replaying_history || reduced.awaiting_user_echo {
        return Some("busy");
    }
    if !reduced.queued_steering.is_empty() || !reduced.queued_follow_up.is_empty() {
        return Some("queued input");
    }
    if reduced
        .background_tasks
        .iter()
        .any(|task| task.status == BackgroundTaskStatus::Running)
    {
        // 后台任务的 supervisor 在宿主里，停宿主会把任务一起杀掉。
        return Some("background task running");
    }
    if reduced
        .subagents
        .iter()
        .any(|subagent| subagent.status.is_running())
    {
        // 子智能体的进程是会话里的 Pi 起的，停掉 runtime 会把它一起杀掉。
        return Some("subagent running");
    }
    if reduced
        .peer_inbox
        .iter()
        .any(|message| message.status == smelt_core::acp_session::PeerMessageStatus::Pending)
    {
        // 别的对话发来、还没送进会话的消息只在驱动内存里，停掉就丢了。
        return Some("peer message pending");
    }
    None
}

/// 挂起前必须全部成立的条件。返回第一条不满足的原因。
pub(crate) fn acp_suspend_blocker(
    sess: &AcpSession,
    now_ms: u64,
    idle_after: Duration,
) -> Option<&'static str> {
    if sess.handle.lock().is_none() {
        return Some("no runtime");
    }
    if sess.unreaped_pid.lock().is_some() {
        return Some("previous runtime not reaped");
    }
    if matches!(
        &*sess.conversation_binding.lock(),
        Some(smelt_core::conversation::ConversationBinding::Automation { .. })
    ) {
        return Some("daemon-owned binding");
    }
    if sess.idle.idle_for(now_ms) < idle_after {
        return Some("recently active");
    }
    if sess.prompt_in_flight.load(Ordering::SeqCst)
        || !sess.pending_prompts.lock().is_empty()
        || sess.unaccepted_prompt.lock().is_some()
        || sess.immediate_delivery.lock().is_some()
    {
        return Some("prompt in flight");
    }
    reduced_suspend_blocker(&sess.reduced.lock())
}

pub(crate) fn spawn_acp_idle_reaper(acp_sessions: AcpSessions) {
    let spawned = thread::Builder::new()
        .name("smeltd-acp-idle".into())
        .spawn(move || {
            let mut contended_rounds = 0u32;
            loop {
                let seen = REAPER_BELL.listen();
                let sweep = suspend_idle_acp_sessions(&acp_sessions, ACP_IDLE_SUSPEND_AFTER);
                let now = crate::event_hub::now_ms();
                let retry_at = if sweep.contended {
                    let delay = idle_retry_delay(contended_rounds);
                    contended_rounds = contended_rounds.saturating_add(1);
                    Some(now.saturating_add(delay.as_millis() as u64))
                } else {
                    contended_rounds = 0;
                    None
                };
                let wake_at = next_reaper_wake_ms(sweep.next_deadline_ms, retry_at);
                REAPER_BELL.wait(
                    seen,
                    wake_at.map(|at| Duration::from_millis(at.saturating_sub(now))),
                );
            }
        });
    if let Err(error) = spawned {
        eprintln!("[acp] 闲置回收线程启动失败：{error}");
    }
}

/// 一轮扫描之后回收线程要知道的事。
#[derive(Debug, Default)]
pub(crate) struct IdleSweep {
    /// 还没闲置够的会话里，最早到点的那个（毫秒时间戳）。被别的条件挡住、已经
    /// 过了闲置期的会话不排进来：挡住它的条件解除时总伴随一次活动，`touch` 会
    /// 重新排上它。
    pub(crate) next_deadline_ms: Option<u64>,
    /// 有会话到点了，却抢不到它的生命周期锁或 spawn 闸门，要过一会儿再试。
    pub(crate) contended: bool,
}

/// 挂起所有闲置够久的会话。
///
/// 每个会话在自己的生命周期锁里复核一遍再动手，锁拿不到就记下，稍后重试。随后拿
/// spawn 闸门的读锁，挡住停机收割。锁序与 `ensure_acp_session` 一致：先
/// 生命周期锁，后闸门。
pub(crate) fn suspend_idle_acp_sessions(
    acp_sessions: &AcpSessions,
    idle_after: Duration,
) -> IdleSweep {
    let mut sweep = IdleSweep::default();
    let not_yet = |sess: &AcpSession, sweep: &mut IdleSweep| {
        if sess.idle.idle_for(crate::event_hub::now_ms()) >= idle_after {
            return false;
        }
        let deadline = sess.idle.idle_deadline_ms(idle_after);
        sweep.next_deadline_ms = next_reaper_wake_ms(sweep.next_deadline_ms, Some(deadline));
        true
    };
    for (id, slot) in acp_sessions.snapshot() {
        let sess = &slot.value;
        // 挂起的会话下次拉起时会记活动，到时再排。
        if sess.idle.is_suspended() || not_yet(sess, &mut sweep) {
            continue;
        }
        // 发件队列里还有发给它、没送到的信：挂起会让交出去的那几封跟着进程一起没了。
        if acp_suspend_blocker(sess, crate::event_hub::now_ms(), idle_after).is_some()
            || crate::peer_messages::has_undelivered(&id)
        {
            continue;
        }
        let Some(_lifecycle) = slot.lifecycle.try_lock() else {
            sweep.contended = true;
            continue;
        };
        if !acp_sessions.is_current(&id, &slot) || not_yet(sess, &mut sweep) {
            continue;
        }
        if acp_suspend_blocker(sess, crate::event_hub::now_ms(), idle_after).is_some()
            || crate::peer_messages::has_undelivered(&id)
        {
            continue;
        }
        let gate = acp_sessions.spawn_gate();
        let Ok(_spawn) = gate.try_read() else {
            sweep.contended = true;
            continue;
        };
        if retire_acp_runtime(sess) {
            sess.reduced.lock().trim_to_current_turn();
            sess.idle.mark_suspended();
            crate::dlog(&format!("acp 闲置挂起 id={id}"));
        } else {
            // 留下的 unreaped_pid 挡住后面的挂起和拉起，下次拉起前会先复核它。
            eprintln!("[acp] 闲置挂起会话 {id} 时未能证明 runtime 已退出");
        }
    }
    sweep
}

/// 挂起那一刻生效的会话配置，唤醒后照它写回。
///
/// 模型单独放：换模型会把 Pi 的推理强度重置成模型默认值，必须先换模型、
/// 等确认后再写其它项。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SuspendedConfig {
    model: Option<String>,
    options: Vec<(String, String)>,
}

impl SuspendedConfig {
    pub(crate) fn capture(reduced: &smelt_core::acp_session::AcpSessionState) -> Self {
        let model_config_id = reduced.model.as_ref().map(|model| model.config_id.as_str());
        Self {
            model: reduced
                .model
                .as_ref()
                .map(|model| model.current_value.clone())
                .filter(|value| !value.is_empty()),
            options: reduced
                .config_options
                .iter()
                // 有的 adapter 把模型同时放在配置项里，那份已经由 `model` 管。
                .filter(|config| Some(config.config_id.as_str()) != model_config_id)
                .filter_map(|config| Some((config.config_id.clone(), config_value(config)?)))
                .collect(),
        }
    }

    /// 模型和挂起前不一样、且新 runtime 还能选它时，返回换回去的动作。
    pub(crate) fn model_action(
        &self,
        reduced: &smelt_core::acp_session::AcpSessionState,
    ) -> Option<smelt_core::acp_session::AcpUserAction> {
        let wanted = self.model.as_ref()?;
        let model = reduced.model.as_ref()?;
        if &model.current_value == wanted
            || (!model.options.is_empty()
                && !model.options.iter().any(|(value, _)| value == wanted))
        {
            return None;
        }
        Some(smelt_core::acp_session::AcpUserAction::SetConfigOption {
            config_id: model.config_id.clone(),
            value_id: wanted.clone(),
            boolean: None,
        })
    }

    /// 模型以外和挂起前不一样、且新 runtime 仍提供的配置项。
    pub(crate) fn option_actions(
        &self,
        reduced: &smelt_core::acp_session::AcpSessionState,
    ) -> Vec<smelt_core::acp_session::AcpUserAction> {
        self.options
            .iter()
            .filter_map(|(config_id, wanted)| {
                let config = reduced
                    .config_options
                    .iter()
                    .find(|config| &config.config_id == config_id)?;
                if config_value(config).as_ref() == Some(wanted) {
                    return None;
                }
                let boolean = match config.boolean {
                    Some(_) => Some(wanted == "true"),
                    None if config.options.iter().any(|(value, _)| value == wanted) => None,
                    None => return None,
                };
                Some(smelt_core::acp_session::AcpUserAction::SetConfigOption {
                    config_id: config_id.clone(),
                    value_id: wanted.clone(),
                    boolean,
                })
            })
            .collect()
    }
}

/// 配置项当前的值 id。开关是 `true` / `false`，选择项按当前显示名反查。
pub(crate) fn config_value(config: &smelt_core::acp_conn::SessionConfigState) -> Option<String> {
    if let Some(flag) = config.boolean {
        return Some(flag.to_string());
    }
    config
        .options
        .iter()
        .find(|(_, name)| name == &config.current_name)
        .map(|(value, _)| value.clone())
}

/// 用到 runtime 之前调用。挂起的会话按记录的启动规格拉起，等 Pi 重放完历史，
/// 再把挂起前的会话配置写回去；没挂起就只记一次活动。错误串说明这一次为什么
/// 用不了。
pub(crate) fn wake_suspended_acp_session(
    id: &str,
    slot: &Arc<AcpSlot<AcpSession>>,
    acp_sessions: &AcpSessions,
    subscribers: &EventHubHandle,
) -> Result<(), String> {
    let sess = &slot.value;
    sess.idle.touch();
    if !sess.idle.is_suspended() {
        return Ok(());
    }
    let saved = {
        let _lifecycle = slot.lifecycle.lock();
        if !acp_sessions.is_current(id, slot) {
            return Err("ACP session was replaced".to_string());
        }
        // 等锁期间别人可能已经拉起了。
        if sess.idle.is_suspended() && !acp_runtime_alive(sess) {
            crate::dlog(&format!("acp 唤醒闲置会话 id={id}"));
            // 拉起会清掉镜像里的模型，必须在那之前记下。挂起期间改不了配置
            // （改配置要 runtime，会先走到这里），所以镜像就是挂起时的值。
            let saved = SuspendedConfig::capture(&sess.reduced.lock());
            relaunch_recorded_acp_runtime(id, slot, acp_sessions, subscribers)
                .map_err(str::to_string)?;
            Some(saved)
        } else {
            None
        }
    };
    wait_for_acp_ready(sess, ACP_WAKE_READY_TIMEOUT)?;
    if let Some(saved) = saved {
        restore_suspended_config(id, sess, &saved, subscribers);
    }
    Ok(())
}

/// 把挂起前的配置写回刚拉起的 runtime。先模型、再其它项，每一步等 agent 确认：
/// 动作之后紧跟的就是用户的消息，配置没落定就开回合会用错模型或权限。
/// 写不回去（模型下架、agent 不再提供某项）只记日志，不挡用户的动作。
pub(crate) fn restore_suspended_config(
    id: &str,
    sess: &AcpSession,
    saved: &SuspendedConfig,
    subscribers: &EventHubHandle,
) {
    let model = saved.model_action(&sess.reduced.lock());
    apply_config_restore_step(
        id,
        sess,
        model.into_iter().collect(),
        subscribers,
        |reduced| saved.model_action(reduced).is_none(),
    );
    let options = saved.option_actions(&sess.reduced.lock());
    apply_config_restore_step(id, sess, options, subscribers, |reduced| {
        saved.option_actions(reduced).is_empty()
    });
}

fn apply_config_restore_step(
    id: &str,
    sess: &AcpSession,
    actions: Vec<smelt_core::acp_session::AcpUserAction>,
    subscribers: &EventHubHandle,
    restored: impl Fn(&smelt_core::acp_session::AcpSessionState) -> bool,
) {
    use smelt_core::daemon_state::DaemonPhase;
    if actions.is_empty() {
        return;
    }
    crate::dlog(&format!("acp 恢复闲置前的会话配置 id={id} {actions:?}"));
    let status_before = sess.reduced.lock().status_line.clone();
    for action in actions {
        if let Err(error) = apply_acp_user_action(sess, action, subscribers) {
            eprintln!("[acp] 会话 {id} 恢复闲置前的配置失败：{error}");
            return;
        }
    }
    let rejected = |reduced: &smelt_core::acp_session::AcpSessionState| {
        reduced.status_line != status_before
            && reduced
                .status_line
                .as_deref()
                .is_some_and(|status| status.starts_with(CONFIG_UPDATE_FAILURE_PREFIX))
    };
    // `reduced` 每改一次醒一次，超时只是上限。
    let (reduced, _) = sess
        .reduced
        .wait_timeout_while(ACP_CONFIG_RESTORE_TIMEOUT, |reduced| {
            !restored(reduced) && reduced.phase != DaemonPhase::Dead && !rejected(reduced)
        });
    if restored(&reduced) || reduced.phase == DaemonPhase::Dead {
        return;
    }
    if rejected(&reduced) {
        let status = reduced.status_line.as_deref().unwrap_or_default();
        eprintln!("[acp] 会话 {id} 恢复闲置前的配置被拒绝：{status}");
        return;
    }
    eprintln!("[acp] 会话 {id} 恢复闲置前的配置超时，未等到 agent 确认");
}

/// 等刚拉起的 runtime 恢复完：不在建连，也不在重放历史。拉起失败的终态原样报回。
/// 别的对话起的新对话和发起方用同一套配置：模型、推理强度、权限模式。等新 runtime
/// 就绪后照唤醒恢复的办法写进去，每一步等 agent 确认，之后再发任务。
pub(crate) fn copy_session_config(
    source: &AcpSession,
    id: &str,
    target: &AcpSession,
    subscribers: &EventHubHandle,
) -> Result<(), String> {
    let saved = SuspendedConfig::capture(&source.reduced.lock());
    wait_for_acp_ready(target, ACP_WAKE_READY_TIMEOUT)?;
    restore_suspended_config(id, target, &saved, subscribers);
    Ok(())
}

fn wait_for_acp_ready(sess: &AcpSession, timeout: Duration) -> Result<(), String> {
    // `reduced` 每改一次醒一次，超时只是上限。
    let (reduced, _) = sess
        .reduced
        .wait_timeout_while(timeout, |reduced| acp_ready_outcome(reduced).is_none());
    acp_ready_outcome(&reduced).unwrap_or_else(|| Err("会话恢复超时".to_string()))
}

/// 恢复有没有结果。`None` 表示还在建连或重放历史。
fn acp_ready_outcome(
    reduced: &smelt_core::acp_session::AcpSessionState,
) -> Option<Result<(), String>> {
    use smelt_core::daemon_state::DaemonPhase;
    match reduced.phase {
        DaemonPhase::Dead => {
            let reason = reduced.end_reason.trim();
            Some(Err(if reason.is_empty() {
                "会话恢复失败".to_string()
            } else {
                reason.to_string()
            }))
        }
        DaemonPhase::Connecting => None,
        _ if reduced.replaying_history => None,
        _ => Some(Ok(())),
    }
}
