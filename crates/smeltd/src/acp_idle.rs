//! 闲置会话挂起。
//!
//! 每场打开过的对话都有一个独立会话宿主，外加它的 Pi 进程。对话几个小时没人碰，
//! 这些进程也一直占着内存。这里定期把闲置够久的会话宿主停掉，也就是挂起。
//!
//! 挂起只停进程，会话本身留在注册表里。镜像、绑定、启动规格、provider 会话占用
//! 都不动，客户端看到的仍是空闲。之后真要用 runtime 时，入口先调
//! [`wake_suspended_acp_session`]。会按记录的启动规格带 `--session` 重新拉起，
//! 等 Pi 重放完历史，再把动作交过去。会用到 runtime 的情况包括：发消息、改配置、
//! 手机往上翻历史、跨智能体投递。客户端重新 `acp_open` 时，`ensure_acp_session`
//! 本来就会拉起没有 runtime 的会话。
//!
//! 只挂起确定安全的会话，条件是以下全部满足：
//! - 跑在独立宿主里；
//! - 没有进行中的回合；
//! - 没有待审批或待回答的问题；
//! - 没有排队的输入；
//! - 没有在跑的后台任务；
//! - 不归自动化或插件驱动；
//! - 不是手机建的远程会话。远程会话只靠槽位里的启动规格才能恢复，升级交接不会
//!   带走已挂起的槽位。
//!
//! 已挂起的槽位没有可交接的 fd，daemon 升级后就不在了。桌面端断线重连会
//! `acp_open`，把它们重新拉起来，再过一轮闲置期后再次挂起。
//!
//! 唤醒要把挂起那一刻的会话配置（模型、推理强度、权限模式等）写回新 runtime。
//! 新进程只会从 provider 自己的会话文件里找回一部分：Pi 的权限模式和脚本工具
//! 只活在驱动内存里，配了模型的智能体启动时还会被强制换回定义里的模型。挂起
//! 对用户应当不可见，所以由守护自己恢复，不指望客户端补发。

use super::*;

/// 没有任何活动这么久之后挂起。
pub(crate) const ACP_IDLE_SUSPEND_AFTER: Duration = Duration::from_secs(30 * 60);
const ACP_IDLE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
/// 唤醒后等 Pi 恢复历史的上限。超时只让这一次动作失败，会话照常继续恢复。
const ACP_WAKE_READY_TIMEOUT: Duration = Duration::from_secs(120);
const ACP_WAKE_READY_POLL: Duration = Duration::from_millis(100);
/// 唤醒后每一步配置恢复等 agent 确认的上限。超时只记日志，用户的动作照常送达。
const ACP_CONFIG_RESTORE_TIMEOUT: Duration = Duration::from_secs(10);
/// 驱动写配置失败时 `status_line` 的前缀（pi_rpc 与 acp_conn 同一句）。
const CONFIG_UPDATE_FAILURE_PREFIX: &str = "更新会话配置失败";

/// 会话的闲置账本。构造时记一次活动：刚建好或刚交接过来的会话不能在第一轮扫描就被挂起。
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
        // 子智能体的进程是会话里的 Pi 起的，停宿主会把它一起杀掉。
        return Some("subagent running");
    }
    if reduced
        .peer_inbox
        .iter()
        .any(|message| message.status == smelt_core::acp_session::PeerMessageStatus::Pending)
    {
        // 别的对话发来、还没送进会话的消息只在宿主内存里，停宿主就丢了。
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
    if sess.hosted_handle.lock().unwrap().is_none() {
        return Some("no hosted runtime");
    }
    if sess.unreaped_pid.lock().unwrap().is_some() {
        return Some("previous runtime not reaped");
    }
    if matches!(
        &*sess.conversation_binding.lock().unwrap(),
        Some(smelt_core::conversation::ConversationBinding::Automation { .. })
    ) {
        return Some("daemon-owned binding");
    }
    if sess.idle.idle_for(now_ms) < idle_after {
        return Some("recently active");
    }
    if sess.prompt_in_flight.load(Ordering::SeqCst)
        || !sess.pending_prompts.lock().unwrap().is_empty()
        || sess.unaccepted_prompt.lock().unwrap().is_some()
        || sess.immediate_delivery.lock().unwrap().is_some()
    {
        return Some("prompt in flight");
    }
    reduced_suspend_blocker(&sess.reduced.lock().unwrap())
}

pub(crate) fn spawn_acp_idle_reaper(acp_sessions: AcpSessions, remote_sessions: RemoteSessions) {
    let spawned = thread::Builder::new()
        .name("smeltd-acp-idle".into())
        .spawn(move || {
            loop {
                thread::sleep(ACP_IDLE_SWEEP_INTERVAL);
                suspend_idle_acp_sessions(&acp_sessions, &remote_sessions, ACP_IDLE_SUSPEND_AFTER);
            }
        });
    if let Err(error) = spawned {
        eprintln!("[acp] 闲置回收线程启动失败：{error}");
    }
}

/// 挂起所有闲置够久的会话，返回挂起了哪些。
///
/// 每个会话在自己的生命周期锁里复核一遍再动手，锁拿不到就留到下一轮。随后拿
/// spawn 闸门的读锁，挡住升级收集交接。锁序与 `ensure_acp_session` 一致：先
/// 生命周期锁，后闸门。
pub(crate) fn suspend_idle_acp_sessions(
    acp_sessions: &AcpSessions,
    remote_sessions: &RemoteSessions,
    idle_after: Duration,
) -> Vec<String> {
    let mut suspended = Vec::new();
    for (id, slot) in acp_sessions.snapshot() {
        let sess = &slot.value;
        // 发件队列里还有发给它、没送到的信：挂起会让交出去的那几封跟着宿主一起没了。
        if acp_suspend_blocker(sess, crate::event_hub::now_ms(), idle_after).is_some()
            || crate::peer_messages::has_undelivered(&id)
        {
            continue;
        }
        if is_known_remote_session(remote_sessions, RemoteSessionKind::Acp, &id) != Some(false) {
            continue;
        }
        let Ok(_lifecycle) = slot.lifecycle.try_lock() else {
            continue;
        };
        if !acp_sessions.is_current(&id, &slot)
            || acp_suspend_blocker(sess, crate::event_hub::now_ms(), idle_after).is_some()
            || crate::peer_messages::has_undelivered(&id)
        {
            continue;
        }
        let gate = acp_sessions.spawn_gate();
        let Ok(_spawn) = gate.try_read() else {
            continue;
        };
        if retire_acp_runtime(sess) {
            sess.idle.mark_suspended();
            crate::dlog(&format!("acp 闲置挂起 id={id}"));
            suspended.push(id);
        } else {
            eprintln!("[acp] 闲置挂起会话 {id} 时未能证明宿主已退出");
        }
    }
    suspended
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
        let _lifecycle = slot.lifecycle.lock().unwrap();
        if !acp_sessions.is_current(id, slot) {
            return Err("ACP session was replaced".to_string());
        }
        // 等锁期间别人可能已经拉起了。
        if sess.idle.is_suspended() && !acp_runtime_alive(sess) {
            crate::dlog(&format!("acp 唤醒闲置会话 id={id}"));
            // 拉起会清掉镜像里的模型，必须在那之前记下。挂起期间改不了配置
            // （改配置要 runtime，会先走到这里），所以镜像就是挂起时的值。
            let saved = SuspendedConfig::capture(&sess.reduced.lock().unwrap());
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
    let model = saved.model_action(&sess.reduced.lock().unwrap());
    apply_config_restore_step(
        id,
        sess,
        model.into_iter().collect(),
        subscribers,
        |reduced| saved.model_action(reduced).is_none(),
    );
    let options = saved.option_actions(&sess.reduced.lock().unwrap());
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
    let status_before = sess.reduced.lock().unwrap().status_line.clone();
    for action in actions {
        if let Err(error) = apply_acp_user_action(sess, action, subscribers) {
            eprintln!("[acp] 会话 {id} 恢复闲置前的配置失败：{error}");
            return;
        }
    }
    let deadline = Instant::now() + ACP_CONFIG_RESTORE_TIMEOUT;
    loop {
        {
            let reduced = sess.reduced.lock().unwrap();
            if restored(&reduced) || reduced.phase == DaemonPhase::Dead {
                return;
            }
            if reduced.status_line != status_before
                && let Some(status) = reduced.status_line.as_deref()
                && status.starts_with(CONFIG_UPDATE_FAILURE_PREFIX)
            {
                eprintln!("[acp] 会话 {id} 恢复闲置前的配置被拒绝：{status}");
                return;
            }
        }
        if Instant::now() >= deadline {
            eprintln!("[acp] 会话 {id} 恢复闲置前的配置超时，未等到 agent 确认");
            return;
        }
        thread::sleep(ACP_WAKE_READY_POLL);
    }
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
    let saved = SuspendedConfig::capture(&source.reduced.lock().unwrap());
    wait_for_acp_ready(target, ACP_WAKE_READY_TIMEOUT)?;
    restore_suspended_config(id, target, &saved, subscribers);
    Ok(())
}

fn wait_for_acp_ready(sess: &AcpSession, timeout: Duration) -> Result<(), String> {
    use smelt_core::daemon_state::DaemonPhase;
    let deadline = Instant::now() + timeout;
    loop {
        {
            let reduced = sess.reduced.lock().unwrap();
            match reduced.phase {
                DaemonPhase::Dead => {
                    let reason = reduced.end_reason.trim();
                    return Err(if reason.is_empty() {
                        "会话恢复失败".to_string()
                    } else {
                        reason.to_string()
                    });
                }
                DaemonPhase::Connecting => {}
                _ if reduced.replaying_history => {}
                _ => return Ok(()),
            }
        }
        if Instant::now() >= deadline {
            return Err("会话恢复超时".to_string());
        }
        thread::sleep(ACP_WAKE_READY_POLL);
    }
}
