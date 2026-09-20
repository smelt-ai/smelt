//! 应用更新、守护无缝升级、会话管理器。

use super::*;

impl Workspace {
    /// 设置入口与设置页共用它决定要不要显示更新提示。
    pub(crate) fn update_available(&self) -> bool {
        self.update_status.staged_update().is_some()
            || matches!(
                self.update_status,
                updater::UpdateStatus::Available { .. }
                    | updater::UpdateStatus::Downloading { .. }
                    | updater::UpdateStatus::Installing { .. }
                    | updater::UpdateStatus::Applying { .. }
                    | updater::UpdateStatus::RestartRequired { .. }
            )
    }

    /// 检查是否有新版本。`silent` 区分启动时的后台静默检查（离线/失败时不打扰用户，
    /// 悄悄退回 Idle）和设置页手动点「检查更新」（失败要如实展示原因）。
    ///
    /// 开着「自动下载安装」时发现新版本会直接接上后台静默下载，不需要用户二次确认；
    /// 关掉则停在 `Available`，只亮出版本号等用户自己决定何时下载。
    /// 检查本身两种情况下都照做——不然关掉之后连"有新版"都无从得知。
    pub(crate) fn check_for_update(&mut self, silent: bool, cx: &mut Context<Self>) {
        if !self.update_status.can_check() {
            return;
        }
        let failed_update = match &self.update_status {
            updater::UpdateStatus::InstallFailed(update) => Some(update.clone()),
            _ => None,
        };
        self.update_status = updater::UpdateStatus::Checking;
        let channel = cx.global::<settings::UpdateSettings>().channel;
        let current_release_url = match updater::current_release_url() {
            Ok(url) => url,
            Err(error) => {
                smelt_core::app_log::error(
                    "updater",
                    &format!("读取本机更新状态失败，暂停检查：{error:#}"),
                );
                self.update_status = failed_update.map_or_else(
                    || updater::UpdateStatus::Failed(updater::UpdateFailure::Recovery),
                    updater::UpdateStatus::InstallFailed,
                );
                cx.notify();
                return;
            }
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    smelt_core::block_on::block_on_tokio(updater::fetch_latest(channel))
                        .and_then(|r| r)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(candidate) => {
                        // 开关读的是"此刻"的值而不是发起检查时的快照：检查期间用户
                        // 若刚把自动更新关掉，就不该再被一个后台下载打脸。
                        let auto_install = cx.global::<settings::UpdateSettings>().auto_install;
                        if let Some(failed) = failed_update {
                            match updater::decide_failed_update_check(
                                candidate,
                                current_release_url.as_deref(),
                                &failed,
                                auto_install,
                            ) {
                                Some(outcome) => {
                                    this.replace_failed_update_after_check(failed, outcome, cx);
                                }
                                None => {
                                    this.update_status =
                                        updater::UpdateStatus::InstallFailed(failed);
                                    cx.notify();
                                }
                            }
                            return;
                        }
                        let outcome = updater::decide_check_outcome(
                            candidate,
                            current_release_url.as_deref(),
                            auto_install,
                        );
                        this.apply_update_check_outcome(outcome, cx);
                        return;
                    }
                    Err(e) => {
                        smelt_core::app_log::error(
                            "updater",
                            &format!("检查{}更新失败：{e:#}", channel.label()),
                        );
                        this.update_status = if let Some(failed) = failed_update {
                            updater::UpdateStatus::InstallFailed(failed)
                        } else if silent {
                            updater::UpdateStatus::Idle
                        } else {
                            updater::UpdateStatus::Failed(updater::UpdateFailure::Check)
                        };
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_update_check_outcome(
        &mut self,
        outcome: updater::CheckOutcome,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            updater::CheckOutcome::Download(candidate) => {
                self.start_update_download(candidate, cx);
            }
            updater::CheckOutcome::Notify(candidate) => {
                self.update_status = updater::UpdateStatus::Available {
                    version: candidate.version,
                    url: candidate.url,
                };
                cx.notify();
            }
            updater::CheckOutcome::NoUpdate => {
                self.update_status = updater::UpdateStatus::UpToDate;
                cx.notify();
            }
        }
    }

    /// 只在 manifest 已经指向另一份发布包后进入。持久化旧作业的清理放到后台线程，
    /// 成功后才能开始新下载，保证 begin_staged_update 永远不会撞上旧事务。
    fn replace_failed_update_after_check(
        &mut self,
        failed: updater::StagedUpdate,
        outcome: updater::CheckOutcome,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let failed_for_work = failed.clone();
            let result = cx
                .background_executor()
                .spawn(async move { updater::discard_failed_update(&failed_for_work) })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    smelt_core::app_log::info(
                        "updater",
                        &format!(
                            "更新源已替换安装失败的包 {}，作废旧作业并继续检查结果",
                            failed.version
                        ),
                    );
                    this.apply_update_check_outcome(outcome, cx);
                }
                Err(error) => {
                    smelt_core::app_log::error(
                        "updater",
                        &format!("作废安装失败的更新 {} 失败：{error:#}", failed.version),
                    );
                    this.update_status = updater::UpdateStatus::InstallFailed(failed);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 恢复失败时先收敛原事务；安装失败则由 check_for_update 拉取 manifest，只有
    /// 确认发布 URL 已变化后才作废旧作业，不能绕过持久化事务直接另起下载。
    pub(crate) fn check_or_recover_update(&mut self, silent: bool, cx: &mut Context<Self>) {
        if self.update_status.can_retry_recovery() {
            self.recover_update_at_launch(silent, cx);
        } else {
            self.check_for_update(silent, cx);
        }
    }

    /// 后台静默下载新版 ZIP 并暂存好 `.app`，完成后置 `ReadyToInstall`（不重启、不打断）。
    /// 下载线程通过 channel 往回推字节进度，UI 线程照单刷新状态；发送端随下载任务结束而
    /// drop，`recv` 收到 Err 即代表下载收尾，此时再 `await` 任务拿最终结果。
    pub(crate) fn start_update_download(
        &mut self,
        candidate: updater::UpdateCandidate,
        cx: &mut Context<Self>,
    ) {
        let version = candidate.version;
        let url = candidate.url;
        self.update_status = updater::UpdateStatus::Downloading {
            version: version.clone(),
            received: 0,
            total: None,
        };
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (tx, rx) = smol::channel::unbounded::<updater::DownloadProgress>();
            let v = version.clone();
            let download_url = url.clone();
            let task = cx.background_executor().spawn(async move {
                smelt_core::block_on::block_on_tokio(updater::download_and_stage(
                    &download_url,
                    &v,
                    |p| {
                        let _ = tx.try_send(p);
                    },
                ))
                .and_then(|r| r)
            });

            while let Ok(progress) = rx.recv().await {
                let version = version.clone();
                let _ = this.update(cx, |this, cx| {
                    this.update_status = match progress {
                        updater::DownloadProgress::Bytes { received, total } => {
                            updater::UpdateStatus::Downloading {
                                version,
                                received,
                                total,
                            }
                        }
                        updater::DownloadProgress::Installing => {
                            updater::UpdateStatus::Installing { version }
                        }
                    };
                    cx.notify();
                });
            }

            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.update_status = match result {
                    Ok(update) => updater::UpdateStatus::ReadyToInstall(update),
                    Err(e) => {
                        smelt_core::app_log::error(
                            "updater",
                            &format!("下载并准备更新包 {version} 失败：{e:#}"),
                        );
                        updater::UpdateStatus::Failed(updater::UpdateFailure::Download)
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// 用户点击、启动恢复、退出安装都只从这里进入同一条安装状态机。
    pub(crate) fn begin_ready_update_install(&mut self, cx: &mut Context<Self>) {
        let Some(update) = self.update_status.ready_update().cloned() else {
            return;
        };
        self.start_update_install(update, UpdateInstallTrigger::User, cx);
    }

    pub(crate) fn recover_update_at_launch(&mut self, silent_check: bool, cx: &mut Context<Self>) {
        self.update_status = updater::UpdateStatus::Checking;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async { updater::recover_pending_update() })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(Some(update)) => {
                    smelt_core::app_log::info(
                        "updater",
                        &format!("恢复待安装更新 {}，接入统一安装流程", update.version),
                    );
                    this.update_status = updater::UpdateStatus::ReadyToInstall(update.clone());
                    this.start_update_install(update, UpdateInstallTrigger::Launch, cx);
                }
                Ok(None) => {
                    this.update_status = updater::UpdateStatus::Idle;
                    this.check_for_update(silent_check, cx);
                }
                Err(error) => {
                    smelt_core::app_log::error(
                        "updater",
                        &format!("恢复更新事务失败，暂停新更新作业：{error:#}"),
                    );
                    this.update_status =
                        updater::UpdateStatus::Failed(updater::UpdateFailure::Recovery);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn start_update_install(
        &mut self,
        update: updater::StagedUpdate,
        trigger: UpdateInstallTrigger,
        cx: &mut Context<Self>,
    ) {
        let version = update.version.clone();

        self.update_install_generation = self.update_install_generation.wrapping_add(1);
        let generation = self.update_install_generation;
        self.update_status = updater::UpdateStatus::Applying {
            version: version.clone(),
        };
        cx.notify();

        cx.spawn(async move |this, cx| {
            let mut wait_started_at: Option<std::time::Instant> = None;
            loop {
                let update_for_attempt = update.clone();
                let result =
                    cx.background_executor()
                        .spawn(async move {
                            terminal::install_app_preserving_sessions(&update_for_attempt)
                        })
                        .await;

                match result {
                    Ok(terminal::AppInstallOutcome::Installed) => {
                        let current = this.update(cx, |this, _cx| {
                            this.update_install_generation == generation
                                && matches!(
                                    &this.update_status,
                                    updater::UpdateStatus::Applying {
                                        version: current_version
                                    } if current_version == &version
                                )
                        });
                        if !matches!(current, Ok(true)) {
                            return;
                        }
                        if trigger.relaunches() {
                            match updater::relaunch() {
                                Ok(()) => {
                                    cx.update(request_app_quit);
                                }
                                Err(error) => {
                                    smelt_core::app_log::error(
                                        "updater",
                                        &format!("更新已安装，但自动重启失败：{error:#}"),
                                    );
                                    let _ = this.update(cx, |this, cx| {
                                        this.update_status =
                                            updater::UpdateStatus::RestartRequired {
                                                version: version.clone(),
                                            };
                                        cx.notify();
                                    });
                                }
                            }
                        } else {
                            cx.update(request_app_quit);
                        }
                        return;
                    }
                    Ok(terminal::AppInstallOutcome::UpdateInvalidated) => {
                        let handled = this.update(cx, |this, cx| {
                            if this.update_install_generation != generation
                                || !matches!(
                                    &this.update_status,
                                    updater::UpdateStatus::Applying {
                                        version: current_version
                                    } if current_version == &version
                                )
                            {
                                return false;
                            }
                            match trigger {
                                UpdateInstallTrigger::Launch => {
                                    this.update_status = updater::UpdateStatus::Idle;
                                    this.check_for_update(true, cx);
                                }
                                UpdateInstallTrigger::User => {
                                    this.update_status = updater::UpdateStatus::Failed(
                                        updater::UpdateFailure::Download,
                                    );
                                    cx.notify();
                                }
                                UpdateInstallTrigger::Quit => {
                                    this.update_status = updater::UpdateStatus::Idle;
                                    cx.notify();
                                }
                            }
                            true
                        });
                        if matches!(handled, Ok(true))
                            && matches!(trigger, UpdateInstallTrigger::Quit)
                        {
                            cx.update(request_app_quit);
                        }
                        return;
                    }
                    Ok(terminal::AppInstallOutcome::WaitingForSafeHandoff) => {
                        if !trigger.retries_while_busy() {
                            smelt_core::app_log::info(
                                "updater",
                                "退出安装遇到 ACP 回合占用，保留更新作业供下次启动恢复",
                            );
                            let _ = this.update(cx, |this, cx| {
                                if this.update_install_generation == generation {
                                    this.update_status =
                                        updater::UpdateStatus::ReadyToInstall(update.clone());
                                    cx.notify();
                                }
                            });
                            cx.update(request_app_quit);
                            return;
                        }
                        let waiting = this.update(cx, |this, cx| {
                            if this.update_install_generation != generation
                                || !matches!(
                                    &this.update_status,
                                    updater::UpdateStatus::Applying {
                                        version: current_version
                                    } if current_version == &version
                                )
                            {
                                return false;
                            }
                            this.update_status =
                                updater::UpdateStatus::WaitingForSafeHandoff(update.clone());
                            cx.notify();
                            true
                        });
                        if !matches!(waiting, Ok(true)) {
                            return;
                        }

                        wait_started_at.get_or_insert_with(std::time::Instant::now);
                        cx.background_executor()
                            .timer(updater::SAFE_HANDOFF_RETRY_INTERVAL)
                            .await;

                        if wait_started_at.is_some_and(|started| {
                            started.elapsed() >= updater::SAFE_HANDOFF_WAIT_TIMEOUT
                        }) {
                            let gave_up = this.update(cx, |this, cx| {
                                let is_current_wait = matches!(
                                    &this.update_status,
                                    updater::UpdateStatus::WaitingForSafeHandoff(current)
                                        if this.update_install_generation == generation
                                            && current == &update
                                );
                                if !is_current_wait {
                                    return false;
                                }
                                this.update_status =
                                    updater::UpdateStatus::ReadyToInstall(update.clone());
                                cx.notify();
                                true
                            });
                            if matches!(gave_up, Ok(true)) {
                                smelt_core::app_log::info(
                                    "updater",
                                    &format!(
                                        "等待 ACP 回合结束安装 {version} 超过 {} 秒，\
                                         暂停自动重试，更新作业仍可直接重试",
                                        updater::SAFE_HANDOFF_WAIT_TIMEOUT.as_secs()
                                    ),
                                );
                            }
                            return;
                        }

                        let retry = this.update(cx, |this, cx| {
                            let is_current_wait = matches!(
                                &this.update_status,
                                updater::UpdateStatus::WaitingForSafeHandoff(current)
                                    if this.update_install_generation == generation
                                        && current == &update
                            );
                            if !is_current_wait {
                                return false;
                            }
                            this.update_status = updater::UpdateStatus::Applying {
                                version: version.clone(),
                            };
                            cx.notify();
                            true
                        });
                        if !matches!(retry, Ok(true)) {
                            return;
                        }
                    }
                    Err(error) => {
                        smelt_core::app_log::error(
                            "updater",
                            &format!("安装更新 {version} 失败：{error:#}"),
                        );
                        let _ = this.update(cx, |this, cx| {
                            if this.update_install_generation == generation
                                && matches!(
                                    &this.update_status,
                                    updater::UpdateStatus::Applying {
                                        version: current_version
                                    } if current_version == &version
                                )
                            {
                                this.update_status =
                                    updater::UpdateStatus::InstallFailed(update.clone());
                                cx.notify();
                            }
                        });
                        if matches!(trigger, UpdateInstallTrigger::Quit) {
                            cx.update(request_app_quit);
                        }
                        return;
                    }
                }
            }
        })
        .detach();
    }

    /// 只在两次单次 handoff 尝试之间取消；已经进入 Applying 的原子落盘不能中途打断。
    pub(crate) fn cancel_update_install_wait(&mut self, cx: &mut Context<Self>) {
        let updater::UpdateStatus::WaitingForSafeHandoff(update) = self.update_status.clone()
        else {
            return;
        };
        self.update_install_generation = self.update_install_generation.wrapping_add(1);
        self.update_status = updater::UpdateStatus::ReadyToInstall(update);
        cx.notify();
    }

    pub(crate) fn retry_update_relaunch(&mut self, cx: &mut Context<Self>) {
        let updater::UpdateStatus::RestartRequired { version } = self.update_status.clone() else {
            return;
        };
        match updater::relaunch() {
            Ok(()) => request_app_quit(cx),
            Err(error) => smelt_core::app_log::error(
                "updater",
                &format!("再次尝试重启更新 {version} 失败：{error:#}"),
            ),
        }
    }

    /// 后台查一次守护是否落后于磁盘上的 smeltd 二进制，决定要不要在设置页/齿轮上
    /// 给出「重启守护」提示。本地 Unix socket 往返很快，但仍走后台线程，跟
    /// check_for_update 同款结构，别在 UI 线程里做阻塞 IO。
    pub(crate) fn check_daemon_outdated(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            // 只探测状态，不再在此 ensure/handoff（冷启动 ensure 由 restore 线程串行做完）。
            // 仍落后则无缝升级到磁盘最新 smeltd。
            let (outdated, info) = cx
                .background_executor()
                .spawn(async { (terminal::daemon_outdated(), terminal::daemon_info()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.daemon_info = info;
                this.daemon_outdated = Some(outdated);
                if outdated {
                    if this.any_agent_busy(cx) {
                        // 有 agent 正在跑：无缝升级虽然不丢会话，但会让所有终端
                        // 闪断重连——挑用户不在跑任务的时刻做，别背着用户换代
                        // （这正是「用着用着终端全卡」的体验来源之一）。
                        this.daemon_upgrade_pending = true;
                    } else {
                        this.daemon_upgrade_pending = false;
                        this.upgrade_daemon_seamless(cx);
                    }
                } else {
                    this.daemon_upgrade_pending = false;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 是否有 agent 正在跑（结构化 phase 处于 Thinking / ExecutingTool）。
    /// 空闲升级的门控：升级会让终端闪断重连，不该打断正在跑的任务。
    pub(crate) fn any_agent_busy(&self, cx: &App) -> bool {
        cx.try_global::<DaemonStates>()
            .map(|states| {
                states.map.lock().unwrap().values().any(|s| {
                    s.has_runtime()
                        && matches!(
                            s.phase,
                            crate::terminal::DaemonPhase::Thinking
                                | crate::terminal::DaemonPhase::ExecutingTool
                        )
                })
            })
            .unwrap_or(false)
    }

    /// subscribe 相位变化后调用：挂起的升级等所有 agent 空闲再发一次。
    pub(crate) fn maybe_flush_pending_daemon_upgrade(&mut self, cx: &mut Context<Self>) {
        if !self.daemon_upgrade_pending || self.daemon_upgrading || self.any_agent_busy(cx) {
            return;
        }
        self.daemon_upgrade_pending = false;
        self.upgrade_daemon_seamless(cx);
    }

    /// 逐 pane 在后台 reattach：会话 id 都还在，走正常重放恢复画面。无缝升级后
    /// 不能在 `update` 回调里同步握手，否则卡住的 daemon 会反过来拖住整个 UI。
    pub(crate) fn reconnect_all_terminals(&self, cx: &mut Context<Self>) {
        for sess in &self.sessions {
            let leaves = sess.term_leaves();
            for leaf in leaves {
                let (cwd, sid) = {
                    let view = leaf.read(cx);
                    (view.cwd(), view.session_id().to_string())
                };
                let leaf_for_result = leaf.clone();
                cx.spawn(async move |_, cx| {
                    let terminal =
                        cx.background_executor()
                            .spawn(async move {
                                terminal::Terminal::reattach(24, 80, cwd.as_deref(), &sid)
                            })
                            .await;
                    if let Ok(terminal) = terminal {
                        leaf_for_result.update(cx, |view, cx| {
                            view.adopt_terminal(terminal, cx);
                        });
                    }
                })
                .detach();
            }
        }
    }

    /// 无缝升级守护：守护 exec 新二进制、PTY fd 原地交接，会话不中断（见 smeltd 升级设计）。
    /// 成功后逐 pane reconnect——会话 id 都还在，走正常 reattach + 重放，画面最多闪一下。
    /// 正在跑的守护太旧不认识 upgrade op 时提示改用下面的硬重启。
    pub(crate) fn upgrade_daemon_seamless(&mut self, cx: &mut Context<Self>) {
        if self.daemon_upgrading {
            return;
        }
        self.daemon_upgrading = true;
        self.daemon_upgrade_msg = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async { terminal::upgrade_daemon() })
                .await;
            // exec 换代后 PID / 启动时刻都变了，跟版本一起重新问一遍。
            let (outdated, info) = cx
                .background_executor()
                .spawn(async { (terminal::daemon_outdated(), terminal::daemon_info()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.daemon_upgrading = false;
                this.daemon_info = info;
                this.daemon_outdated = Some(outdated);
                // exec 出来的新进程同样丢了网关和隧道（运行态不参与交接），理由与
                // 硬重启那条路径一样，见 confirm_restart_daemon 里的注释。
                if outcome == terminal::UpgradeOutcome::Upgraded
                    || outcome == terminal::UpgradeOutcome::Failed
                {
                    settings::spawn_remote_bootstrap(cx);
                }
                this.daemon_upgrade_msg = Some(match outcome {
                    terminal::UpgradeOutcome::Upgraded => {
                        this.reconnect_all_terminals(cx);
                        "已无缝升级，所有会话保持运行。".to_string()
                    }
                    terminal::UpgradeOutcome::Unsupported => {
                        // 守护完全没认这个 op，控制连接以外的东西没被碰过，各 pane
                        // 的流式连接照常连着，不需要重连。
                        "正在跑的守护版本过旧，不支持无缝升级；请用「重启守护进程」（会断开会话）。"
                            .to_string()
                    }
                    terminal::UpgradeOutcome::Busy => {
                        this.daemon_upgrade_pending = true;
                        "有 Agent 回合仍在运行；回合结束后会自动升级。".to_string()
                    }
                    terminal::UpgradeOutcome::Failed => {
                        // 守护回了 ok:true 才会 exec：旧连接多半已断。version
                        // 握手没看到新 pid/started_at 时按可能已断重连。
                        this.reconnect_all_terminals(cx);
                        "升级结果未确认（新守护未完成 version 握手），已尝试重连各终端；如仍无响应可重试或改用重启。".to_string()
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// GUI 侧栏当前认领的全部 session id。跟 smeltd list 查回来的全量做差集，
    /// 剩下的就是「守护持有但没有任何侧栏在追踪」的游离会话（如测试残留或遗忘的会话）。
    /// 终端会话看 `term_leaves`；ACP 会话同样托管在 smeltd 里，由同一份 `list` 汇总，
    /// 此处一并统计，避免开着的 ACP 对话被误标成游离会话。
    pub(crate) fn tracked_session_ids(&self, cx: &App) -> std::collections::HashSet<String> {
        self.sessions
            .iter()
            .flat_map(|s| s.term_leaves())
            .map(|t| t.read(cx).session_id().to_string())
            .chain(self.sessions.iter().filter_map(|s| match &s.kind {
                SessionKind::Conversation(view) => Some(view.read(cx).session_id().to_string()),
                _ => None,
            }))
            .collect()
    }

    /// 打开「会话管理」弹窗，读 subscribe 身份镜像（含侧栏没认领的游离会话）。
    pub(crate) fn open_session_manager(&mut self, cx: &mut Context<Self>) {
        self.session_manager_open = true;
        self.session_manager_tombstones.clear();
        self.session_manager_list = None;
        cx.notify();
        self.refresh_session_manager(cx);
    }

    pub(crate) fn refresh_session_manager(&mut self, cx: &mut Context<Self>) {
        if !DaemonStates::is_primed(cx) {
            self.session_manager_list = None;
            cx.notify();
            return;
        }
        self.session_manager_list = Some(smelt_core::daemon_state::apply_identity_tombstones(
            DaemonStates::snapshot(cx),
            &mut self.session_manager_tombstones,
        ));
        cx.notify();
    }

    /// 按 id 前缀分流到对应的 kill op——ACP 会话（`acp-` 前缀）只在
    /// `AcpSessions` 表里，终端的 `kill` op 认不出这个 id，会静默什么都不做
    /// 却照样回 `{"ok":true}`（真实教训：两条表分开存，用错 op 表面上「成功」
    /// 实际上会话根本没被杀掉，圈了个大坑）。
    pub(crate) fn kill_daemon_session(id: &str) {
        if id.starts_with("acp-") {
            smelt_core::acp_client::kill_acp_session(id);
        } else {
            terminal::kill_remote(id);
        }
    }

    /// 关掉守护进程里的一个会话（真杀底层 shell / ACP 子进程）。
    pub(crate) fn kill_session_in_manager(&mut self, id: String, cx: &mut Context<Self>) {
        // 这是用户明确点击“关闭会话”，不能把 kill 留给 App 退出时可能被丢弃的
        // detached 任务。两个控制 op 都有 5s socket 超时，主线程最多等待一次有界往返。
        Self::kill_daemon_session(&id);
        self.session_manager_tombstones.insert(id);
        self.refresh_session_manager(cx);
    }

    /// 批量清理「没有任何侧栏在追踪」的游离会话——不碰任何 GUI 认领的正常会话，
    /// 不需要走「重启守护进程」那种连坐所有会话的核选项。
    pub(crate) fn kill_all_detached_in_manager(&mut self, cx: &mut Context<Self>) {
        let tracked = self.tracked_session_ids(cx);
        let Some(list) = self.session_manager_list.clone() else {
            return;
        };
        let detached_ids: Vec<String> = list
            .into_iter()
            .map(|s| s.id)
            .filter(|id| !tracked.contains(id))
            .collect();
        if detached_ids.is_empty() {
            return;
        }
        self.session_manager_tombstones
            .extend(detached_ids.iter().cloned());
        self.refresh_session_manager(cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn(async move {
                    for id in &detached_ids {
                        Self::kill_daemon_session(id);
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| this.refresh_session_manager(cx));
        })
        .detach();
    }

    /// 用户在弹窗里点了「确定重启」：让守护退出（断开所有会话）、拉起磁盘上最新的
    /// smeltd、再刷新状态。
    ///
    /// **禁止**在 `update` 里同步 `Terminal::spawn`：握手含 sleep/轮询，多 pane 会把
    /// UI 卡死（「点重启守护就假死」）。流程：后台杀+拉起守护 → 后台按 cwd/sid 建
    /// Terminal → 主线程 `adopt_terminal` 挂回各 pane。
    pub(crate) fn confirm_restart_daemon(&mut self, cx: &mut Context<Self>) {
        self.show_daemon_restart_confirm = false;
        self.daemon_outdated = None;
        // 收集重建参数（Entity 可 Clone；真正 spawn 扔后台）。
        // 硬重启会清掉守护里的会话。终端 agent 不做语义恢复，缺失会话统一重开 shell。
        let mut jobs: Vec<(Entity<TerminalView>, Option<String>, String)> = Vec::new();
        for sess in &self.sessions {
            let leaves = sess.term_leaves();
            for leaf in leaves {
                let view = leaf.read(cx);
                let cwd = view.cwd();
                let sid = view.session_id().to_string();
                jobs.push((leaf, cwd, sid));
            }
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            // 硬重启后是全新进程，PID / 启动时刻 / 会话数都得重问。
            let (outdated, info) = cx
                .background_executor()
                .spawn(async {
                    terminal::restart_daemon();
                    terminal::ensure_daemon_running();
                    (terminal::daemon_outdated(), terminal::daemon_info())
                })
                .await;

            // 握手/重试全在后台；主线程只接结果
            let built = cx
                .background_executor()
                .spawn(async move {
                    let mut out = Vec::with_capacity(jobs.len());
                    for (entity, cwd, sid) in jobs {
                        let term = terminal::Terminal::spawn(24, 80, cwd.as_deref(), &sid, None);
                        out.push((entity, term));
                    }
                    out
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                this.daemon_info = info;
                this.daemon_outdated = Some(outdated);
                // 新守护进程里网关和隧道都是空的（运行态不参与交接）。守护自己会按
                // 配置自愈，但老版本守护不会，且 UI 手里的配对码此刻已经过期——这里
                // 幂等地补一发并刷新二维码。不补的话就是「重启守护后手机连不上，
                // 必须去设置页把远程关掉再打开」那个 bug。
                settings::spawn_remote_bootstrap(cx);
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
                    this.background_error = Some(format!(
                        "守护已重启，但有 {failed} 个终端没能重开（侧栏会话仍在，可关了再开）"
                    ));
                } else {
                    this.daemon_upgrade_msg =
                        Some("守护已硬重启，会话已按原目录/启动命令重建。".into());
                }
                // 布局没变，写盘刷新 launch_cmd 等字段即可。
                this.save_state(cx);
                cx.notify();
            });
        })
        .detach();
    }
}
