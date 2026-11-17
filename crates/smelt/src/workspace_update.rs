//! 本机守护落后于磁盘上的 smeltd 时自动重启换代。应用更新在内置插件「更新与帮助」
//! （`smelt-updater-ui`）。

use smelt_model::daemon_state::{DaemonPhase, DaemonSessionState};

use super::*;

/// 正在流式输出或执行的会话：这时重启守护会打断它。空闲 shell、等用户输入的会话不算。
fn is_busy(state: &DaemonSessionState) -> bool {
    state.has_runtime()
        && matches!(
            state.effective_phase(),
            DaemonPhase::Thinking | DaemonPhase::ExecutingTool | DaemonPhase::Connecting
        )
}

impl Workspace {
    /// 本机守护里有没有会被重启打断的会话。只看内存里的会话状态。
    pub(crate) fn local_has_busy_work(&self, cx: &App) -> bool {
        DaemonStates::snapshot(&crate::RuntimeKey::Local, cx)
            .iter()
            .any(is_busy)
    }

    /// 后台查一次守护是否落后于磁盘上的 smeltd 二进制；落后又没有进行中的会话就直接重启。
    pub(crate) fn check_daemon_outdated(&mut self, cx: &mut Context<Self>) {
        self.probe_daemon_outdated(true, cx);
    }

    /// 见 [`Self::check_daemon_outdated`]。`restart_if_idle` 为假时只看不重启：本机运行时
    /// 刚重连多半就是一次重启换代的结果，换完还落后时再接着重启，会和重连互相触发、停不
    /// 下来。本地 Unix socket 往返很快，但仍走后台线程，
    /// 别在 UI 线程里做阻塞 IO。
    pub(crate) fn probe_daemon_outdated(&mut self, restart_if_idle: bool, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            // 只探测状态，不再在此 ensure（冷启动 ensure 由 restore 线程串行做完）。
            let outdated = cx
                .background_executor()
                .spawn(async { crate::local_daemon::daemon_outdated() })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !outdated {
                    this.daemon_restart_waiting_idle = false;
                }
                // 磁盘上的映像已经不是正在跑的这份。没有进行中的会话就直接重启。
                // 有的话不在后台打断它们，也不弹窗（每次检查都会弹）：会话都停下来时
                // 再查一次，还落后就重启（见 `retry_daemon_restart_when_idle`）。
                if outdated
                    && restart_if_idle
                    && !crate::runtimes::local_upgrade_in_hand(cx)
                    && !this.daemon_upgrading
                {
                    if this.local_has_busy_work(cx) {
                        this.daemon_restart_waiting_idle = true;
                    } else {
                        this.restart_local_daemon(cx);
                    }
                }
            });
        })
        .detach();
    }

    /// 让守护退出（断开本机所有会话），拉起磁盘上最新的 smeltd。各个终端、对话自己发现断线、
    /// 等本机重新上线再连回去，这里不管。杀和拉起都阻塞，放后台。
    pub(crate) fn restart_local_daemon(&mut self, cx: &mut Context<Self>) {
        if self.daemon_upgrading {
            return;
        }
        self.daemon_upgrading = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn(async {
                    crate::local_daemon::restart_daemon();
                    crate::local_daemon::ensure_daemon_running();
                })
                .await;
            let _ = this.update(cx, |this, _| this.daemon_upgrading = false);
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(phase: DaemonPhase, runtime: bool) -> DaemonSessionState {
        DaemonSessionState {
            id: "s".to_string(),
            phase,
            runtime,
            turn_events: true,
            ..DaemonSessionState::default()
        }
    }

    #[test]
    fn only_running_turns_block_a_restart() {
        assert!(!is_busy(&state(DaemonPhase::Idle, true)));
        assert!(!is_busy(&state(DaemonPhase::WaitingForUser, true)));
        assert!(!is_busy(&state(DaemonPhase::Thinking, false)));
        assert!(is_busy(&state(DaemonPhase::Thinking, true)));
        assert!(is_busy(&state(DaemonPhase::ExecutingTool, true)));
        assert!(is_busy(&state(DaemonPhase::Connecting, true)));
    }
}
