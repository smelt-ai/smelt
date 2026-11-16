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
                        this.restart_daemon_and_rebuild_terminals(cx);
                    }
                }
            });
        })
        .detach();
    }

    /// 让守护退出（断开本机所有会话）、拉起磁盘上最新的 smeltd，再把各 pane 的终端重建起来。
    ///
    /// **禁止**在 `update` 里同步 `Terminal::spawn`：连守护、握手、等守护换代都会阻塞，
    /// 多 pane 会把 UI 卡死。流程：后台杀+拉起守护 → 后台按 cwd/sid 建 Terminal → 主线程
    /// `adopt_terminal` 挂回各 pane。
    pub(crate) fn restart_daemon_and_rebuild_terminals(&mut self, cx: &mut Context<Self>) {
        if self.daemon_upgrading {
            return;
        }
        self.daemon_upgrading = true;
        // 收集重建参数（Entity 可 Clone；真正 spawn 扔后台）。
        // 硬重启会清掉守护里的会话。终端 agent 不做语义恢复，缺失会话统一重开 shell。
        let mut jobs: Vec<(Entity<TerminalView>, Option<String>, String)> = Vec::new();
        for sess in &self.sessions {
            // 硬重启只清本机守护里的会话。远程机器上的终端不动。
            if !sess.runtime(cx).is_local() {
                continue;
            }
            let leaves = sess.term_leaves();
            for leaf in leaves {
                let view = leaf.read(cx);
                let cwd = view.cwd();
                let sid = view.session_id().to_string();
                jobs.push((leaf, cwd, sid));
            }
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn(async {
                    crate::local_daemon::restart_daemon();
                    crate::local_daemon::ensure_daemon_running();
                })
                .await;

            // 握手和等守护换代全在后台；主线程只接结果
            let connector = crate::runtimes::connector(&crate::runtimes::Runtime::local());
            let built = cx
                .background_executor()
                .spawn(async move {
                    let mut out = Vec::with_capacity(jobs.len());
                    for (entity, cwd, sid) in jobs {
                        let term = terminal::Terminal::spawn(
                            connector.as_ref(),
                            24,
                            80,
                            cwd.as_deref(),
                            &sid,
                            None,
                        );
                        out.push((entity, term));
                    }
                    out
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.daemon_upgrading = false;
                let mut failed = 0usize;
                for (entity, term) in built {
                    match term {
                        Ok(t) => {
                            entity.update(cx, |view, cx| view.adopt_terminal(t, cx));
                        }
                        Err(e) => {
                            failed += 1;
                            eprintln!("[workspace] 硬重启后重开终端失败：{e:#}");
                        }
                    }
                }
                if failed > 0 {
                    this.report_background_error(format!(
                        "守护已重启，但有 {failed} 个终端没能重开（侧栏会话仍在，可关了再开）"
                    ));
                }
                // 布局没变，写盘刷新 launch_cmd 等字段即可。
                this.save_state(cx);
                cx.notify();
            });
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
