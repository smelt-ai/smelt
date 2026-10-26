//! 应用更新、守护重启、会话管理器。

use super::*;

impl Workspace {
    #[cfg(target_os = "macos")]
    /// 设置入口与设置页共用它决定要不要显示更新提示。
    pub(crate) fn update_available(&self) -> bool {
        self.update_status.staged_update().is_some()
            || matches!(
                self.update_status,
                updater::UpdateStatus::Available { .. }
                    | updater::UpdateStatus::Downloading { .. }
                    | updater::UpdateStatus::Installing { .. }
                    | updater::UpdateStatus::Applying { .. }
            )
    }

    #[cfg(target_os = "macos")]
    /// App 放的位置装不上更新时，所有更新入口都停在「需要先移到『应用程序』」，不检查、
    /// 不下载、不安装：在那儿每一步都白做，最后还是写不进去。
    fn blocked_by_app_location(&mut self, cx: &mut Context<Self>) -> bool {
        if self.app_location_issue.is_none() {
            return false;
        }
        self.update_status = updater::UpdateStatus::Failed(updater::UpdateFailure::Location);
        cx.notify();
        true
    }

    #[cfg(target_os = "macos")]
    /// 「移到『应用程序』并重新打开」：拷过去、安排好等这个进程退出后从新位置重开，再退出。
    /// 后台会话在守护里，重开后照常接上。失败就留在原地，把原因给人看。
    pub(crate) fn move_app_to_applications(&mut self, cx: &mut Context<Self>) {
        if self.moving_app {
            return;
        }
        self.moving_app = true;
        self.move_app_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async {
                    let app = smelt_client::app_location::move_to_applications()?;
                    smelt_client::app_location::relaunch_after_exit(&app)?;
                    anyhow::Ok(app)
                })
                .await;
            match result {
                Ok(app) => {
                    smelt_sys::app_log::info(
                        "app-location",
                        &format!("已移到 {}，退出后从那里重新打开", app.display()),
                    );
                    cx.update(request_app_quit);
                }
                Err(error) => {
                    smelt_sys::app_log::error(
                        "app-location",
                        &format!("移到「应用程序」失败：{error:#}"),
                    );
                    let _ = this.update(cx, |this, cx| {
                        this.moving_app = false;
                        this.move_app_error = Some(format!("没移成：{error:#}").into());
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    #[cfg(target_os = "macos")]
    /// 检查是否有新版本。`silent` 区分启动时的后台静默检查（离线/失败时不打扰用户，
    /// 悄悄退回 Idle）和设置页手动点「检查更新」（失败要如实展示原因）。
    ///
    /// 开着「自动下载安装」时发现新版本会直接接上后台静默下载，不需要用户二次确认；
    /// 关掉则停在 `Available`，只亮出版本号等用户自己决定何时下载。
    /// 检查本身两种情况下都照做——不然关掉之后连"有新版"都无从得知。
    pub(crate) fn check_for_update(&mut self, silent: bool, cx: &mut Context<Self>) {
        if self.blocked_by_app_location(cx) {
            return;
        }
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
                smelt_sys::app_log::error(
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
                    smelt_sys::block_on::block_on_tokio(updater::fetch_latest(channel))
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
                        smelt_sys::app_log::error(
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

    #[cfg(target_os = "macos")]
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

    #[cfg(target_os = "macos")]
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
                    smelt_sys::app_log::info(
                        "updater",
                        &format!(
                            "更新源已替换安装失败的包 {}，作废旧作业并继续检查结果",
                            failed.version
                        ),
                    );
                    this.apply_update_check_outcome(outcome, cx);
                }
                Err(error) => {
                    smelt_sys::app_log::error(
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

    #[cfg(target_os = "macos")]
    /// 恢复失败时先收敛原事务；安装失败则由 check_for_update 拉取 manifest，只有
    /// 确认发布 URL 已变化后才作废旧作业，不能绕过持久化事务直接另起下载。
    pub(crate) fn check_or_recover_update(&mut self, silent: bool, cx: &mut Context<Self>) {
        if self.blocked_by_app_location(cx) {
            return;
        }
        if self.update_status.can_retry_recovery() {
            self.recover_update_at_launch(silent, cx);
        } else {
            self.check_for_update(silent, cx);
        }
    }

    #[cfg(target_os = "macos")]
    /// 后台静默下载新版 ZIP 并暂存好 `.app`，完成后置 `ReadyToInstall`（不重启、不打断）。
    /// 下载线程通过 channel 往回推字节进度，UI 线程照单刷新状态；发送端随下载任务结束而
    /// drop，`recv` 收到 Err 即代表下载收尾，此时再 `await` 任务拿最终结果。
    pub(crate) fn start_update_download(
        &mut self,
        candidate: updater::UpdateCandidate,
        cx: &mut Context<Self>,
    ) {
        if self.blocked_by_app_location(cx) {
            return;
        }
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
                smelt_sys::block_on::block_on_tokio(updater::download_and_stage(
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
                        smelt_sys::app_log::error(
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

    #[cfg(target_os = "macos")]
    /// 用户点击、启动恢复、退出安装都只从这里进入同一条安装状态机。
    /// 没有进行中的会话时直接安装；有的话先在设置窗确认。
    pub(crate) fn begin_ready_update_install(&mut self, cx: &mut Context<Self>) {
        if self.blocked_by_app_location(cx) {
            return;
        }
        let Some(update) = self.update_status.ready_update().cloned() else {
            return;
        };
        let names = self.busy_work_names(cx);
        if super::daemon_restart_prompt::should_confirm(&names) {
            self.stop_prompt_names = names;
            self.show_update_install_confirm = true;
            cx.notify();
            return;
        }
        self.start_update_install(update, UpdateInstallTrigger::User, cx);
    }

    /// 更新事务只在 macOS 上存在；别的平台不会弹这个确认，这里只是关掉它。
    pub(crate) fn confirm_update_install(&mut self, cx: &mut Context<Self>) {
        self.show_update_install_confirm = false;
        #[cfg(target_os = "macos")]
        if let Some(update) = self.update_status.ready_update().cloned() {
            self.start_update_install(update, UpdateInstallTrigger::User, cx);
            return;
        }
        cx.notify();
    }

    pub(crate) fn busy_work_names(&self, cx: &App) -> Vec<String> {
        super::daemon_restart_prompt::busy_work_names(&DaemonStates::snapshot(
            &crate::RuntimeKey::Local,
            cx,
        ))
    }

    #[cfg(target_os = "macos")]
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
                    smelt_sys::app_log::info(
                        "updater",
                        &format!("恢复待安装更新 {}，等待用户显式重试", update.version),
                    );
                    // helper 的持续性失败不能演变成“拉起旧 GUI → 自动退出 → 再失败”循环。
                    // 恢复只呈现 Ready 作业，由用户明确点击后才创建新 attempt。
                    this.update_status = if update.installer_failed() {
                        updater::UpdateStatus::InstallFailed(update)
                    } else {
                        updater::UpdateStatus::ReadyToInstall(update)
                    };
                    cx.notify();
                }
                Ok(None) => {
                    this.update_status = updater::UpdateStatus::Idle;
                    this.check_for_update(silent_check, cx);
                }
                Err(error) => {
                    smelt_sys::app_log::error(
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

    #[cfg(target_os = "macos")]
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
            let relaunch = trigger.relaunches();
            let update_for_installer = update.clone();
            let result = cx
                .background_executor()
                .spawn(async move { updater::launch_installer(&update_for_installer, relaunch) })
                .await;
            match result {
                Ok(ticket) => {
                    smelt_sys::app_log::info(
                        "updater",
                        &format!(
                            "installer 已接管更新 {version}（job={} attempt={}），等待 GUI 退出",
                            ticket.update_id, ticket.attempt_id
                        ),
                    );
                    // helper 已成功派生并持有精确 attempt。先停掉旧守护，再退出。
                    // 安装器换 `.app` 时文件不能还被旧进程占着。派生失败不走这里。
                    let stopped = cx
                        .background_executor()
                        .spawn(async { updater::shutdown_local_daemon() })
                        .await;
                    if !stopped {
                        smelt_sys::app_log::error(
                            "updater",
                            "退出前没能确认旧守护已退出，安装器换包前会再停一次",
                        );
                    }
                    cx.update(request_app_quit);
                }
                Err(error) => {
                    smelt_sys::app_log::error(
                        "updater",
                        &format!("启动 installer 失败，当前 App 未替换：{error:#}"),
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
                            this.update_status = updater::UpdateStatus::InstallFailed(update);
                            cx.notify();
                        }
                    });
                    if matches!(trigger, UpdateInstallTrigger::Quit) {
                        cx.update(request_app_quit);
                    }
                }
            }
        })
        .detach();
    }

    /// 后台查一次守护是否落后于磁盘上的 smeltd 二进制，决定要不要在设置页/齿轮上
    /// 给出「重启守护」提示。本地 Unix socket 往返很快，但仍走后台线程，跟
    /// check_for_update 同款结构，别在 UI 线程里做阻塞 IO。
    pub(crate) fn check_daemon_outdated(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            // 只探测状态，不再在此 ensure（冷启动 ensure 由 restore 线程串行做完）。
            let (outdated, info) = cx
                .background_executor()
                .spawn(async { (crate::local_daemon::daemon_outdated(), crate::local_daemon::daemon_info()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.daemon_info = info;
                this.daemon_outdated = Some(outdated);
                // 磁盘上的映像已经不是正在跑的这份。没有进行中的会话就直接重启。
                // 有的话不在后台打断它们，也不弹窗（定时检查会反复弹）：设置页的
                // 「重启以升级」走确认，空闲下来后下一次检查再重启。
                if outdated
                    && !crate::runtimes::local_upgrade_in_hand(cx)
                    && !this.daemon_upgrading
                    && !super::daemon_restart_prompt::should_confirm(&this.busy_work_names(cx))
                {
                    this.restart_daemon_and_rebuild_terminals(cx);
                }
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
        if !DaemonStates::is_primed(&crate::RuntimeKey::Local, cx) {
            self.session_manager_list = None;
            cx.notify();
            return;
        }
        self.session_manager_list = Some(smelt_client::daemon_state::apply_identity_tombstones(
            DaemonStates::snapshot(&crate::RuntimeKey::Local, cx),
            &mut self.session_manager_tombstones,
        ));
        cx.notify();
    }

    /// 按 id 前缀分流到对应的 kill op——ACP 会话（`acp-` 前缀）只在
    /// `AcpSessions` 表里，终端的 `kill` op 认不出这个 id，会静默什么都不做
    /// 却照样回 `{"ok":true}`（真实教训：两条表分开存，用错 op 表面上「成功」
    /// 实际上会话根本没被杀掉，圈了个大坑）。
    pub(crate) fn kill_daemon_session(id: &str) {
        let runtime = crate::runtimes::Runtime::local();
        if id.starts_with("acp-") {
            smelt_client::acp_client::kill_acp_session(runtime.client(), id);
        } else {
            terminal::kill_remote(&runtime, id);
        }
    }

    /// 关掉守护进程里的一个会话（真杀底层 shell / ACP 子进程）。kill 在专用线程里发，
    /// App 退出时会等它发完（见 `smelt_client::session_kill`）。
    pub(crate) fn kill_session_in_manager(&mut self, id: String, cx: &mut Context<Self>) {
        {
            let id = id.clone();
            smelt_client::session_kill::spawn("smelt-session-kill", move || {
                Self::kill_daemon_session(&id)
            });
        }
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
    /// 设置里的「重启以升级」和「重启守护进程」都走这里。有进行中的会话时先确认。
    pub(crate) fn request_daemon_restart(&mut self, cx: &mut Context<Self>) {
        if self.daemon_upgrading {
            return;
        }
        let names = self.busy_work_names(cx);
        if super::daemon_restart_prompt::should_confirm(&names) {
            self.stop_prompt_names = names;
            self.show_daemon_restart_confirm = true;
            cx.notify();
            return;
        }
        self.restart_daemon_and_rebuild_terminals(cx);
    }

    pub(crate) fn confirm_restart_daemon(&mut self, cx: &mut Context<Self>) {
        self.show_daemon_restart_confirm = false;
        self.restart_daemon_and_rebuild_terminals(cx);
    }

    pub(crate) fn restart_daemon_and_rebuild_terminals(&mut self, cx: &mut Context<Self>) {
        if self.daemon_upgrading {
            return;
        }
        self.daemon_upgrading = true;
        self.show_daemon_restart_confirm = false;
        self.daemon_outdated = None;
        self.daemon_upgrade_msg = None;
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
        cx.notify();
        cx.spawn(async move |this, cx| {
            // 硬重启后是全新进程，PID / 启动时刻 / 会话数都得重问。
            let (outdated, info) = cx
                .background_executor()
                .spawn(async {
                    crate::local_daemon::restart_daemon();
                    crate::local_daemon::ensure_daemon_running();
                    (crate::local_daemon::daemon_outdated(), crate::local_daemon::daemon_info())
                })
                .await;

            // 握手/重试全在后台；主线程只接结果
            let runtime = crate::runtimes::Runtime::local();
            let built = cx
                .background_executor()
                .spawn(async move {
                    let mut out = Vec::with_capacity(jobs.len());
                    for (entity, cwd, sid) in jobs {
                        let term = terminal::Terminal::spawn(
                            &runtime,
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
                this.daemon_info = info;
                this.daemon_outdated = Some(outdated);
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
                } else {
                    this.daemon_upgrade_msg = Some(
                        "守护已重启。进行中的回合已中断，历史记录还在，终端已按原目录重建。".into(),
                    );
                }
                // 布局没变，写盘刷新 launch_cmd 等字段即可。
                this.save_state(cx);
                cx.notify();
            });
        })
        .detach();
    }
}
