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

use super::*;

/// 没有任何活动这么久之后挂起。
pub(crate) const ACP_IDLE_SUSPEND_AFTER: Duration = Duration::from_secs(30 * 60);
const ACP_IDLE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
/// 唤醒后等 Pi 恢复历史的上限。超时只让这一次动作失败，会话照常继续恢复。
const ACP_WAKE_READY_TIMEOUT: Duration = Duration::from_secs(120);
const ACP_WAKE_READY_POLL: Duration = Duration::from_millis(100);

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

/// 有 runtime，或者挂起了、用时能拉起。对外（跨智能体目标列表等）这两种都算可用。
pub(crate) fn acp_runtime_available(sess: &AcpSession) -> bool {
    acp_runtime_alive(sess) || sess.idle.is_suspended()
}

/// 这个动作要不要真正的 runtime。只读镜像的动作不必为它拉起进程。取消闲置会话
/// 本来就没有可取消的。刷新模型目录由 GUI 在登录新 provider 后广播给所有对话，
/// 挂起的会话醒来时本就会重新取一遍，不值得为它全部唤醒。有后台任务在跑的会话
/// 不会被挂起，所以挂起的会话也没有后台任务可停。
pub(crate) fn acp_action_needs_runtime(action: &smelt_core::acp_session::AcpUserAction) -> bool {
    use smelt_core::acp_session::AcpUserAction;
    !matches!(
        action,
        AcpUserAction::Refresh
            | AcpUserAction::HistoryPage { .. }
            | AcpUserAction::Cancel
            | AcpUserAction::RefreshModels
            | AcpUserAction::StopBackgroundTask { .. }
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
        if acp_suspend_blocker(sess, crate::event_hub::now_ms(), idle_after).is_some() {
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

/// 用到 runtime 之前调用。挂起的会话按记录的启动规格拉起，等 Pi 重放完历史；
/// 没挂起就只记一次活动。错误串说明这一次为什么用不了。
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
    {
        let _lifecycle = slot.lifecycle.lock().unwrap();
        if !acp_sessions.is_current(id, slot) {
            return Err("ACP session was replaced".to_string());
        }
        // 等锁期间别人可能已经拉起了。
        if sess.idle.is_suspended() && !acp_runtime_alive(sess) {
            crate::dlog(&format!("acp 唤醒闲置会话 id={id}"));
            relaunch_recorded_acp_runtime(id, slot, acp_sessions, subscribers)
                .map_err(str::to_string)?;
        }
    }
    wait_for_acp_ready(sess, ACP_WAKE_READY_TIMEOUT)
}

/// 等刚拉起的 runtime 恢复完：不在建连，也不在重放历史。拉起失败的终态原样报回。
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
