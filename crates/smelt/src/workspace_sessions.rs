//! 工作台会话生命周期：冷启动恢复、增删、分屏、项目、重命名。

use super::*;

pub(crate) struct AcpResumeRequest {
    pub agent: settings::ConversationAgentKind,
    pub launch_override: Option<smelt_model::agent_kind::ConversationLaunchSpec>,
    pub profile_id: Option<String>,
    pub cwd: String,
    pub resume_id: String,
}

/// 新建一段 ACP 对话或自动化 Run 所需的完整输入。产品 Agent 只是可选来源定义；
/// 智能体对话留在工作台对话区，定时 Run 默认无界面，都不进入项目会话列表。
pub(crate) struct NewAcpSessionRequest {
    /// 会话开在哪个运行时上。
    pub runtime: crate::runtimes::Runtime,
    pub agent: settings::ConversationAgentKind,
    pub launch_override: Option<smelt_model::agent_kind::ConversationLaunchSpec>,
    pub profile_id: Option<String>,
    pub agent_definition: Option<settings::AgentDefinition>,
    pub fallback_cwd: Option<String>,
    /// 握手完成后自动发出的 Run 输入。对话为 None；定时触发把任务内容放这里，
    /// 绝不能写进智能体长期指令。
    pub pending_prompt: Option<String>,
    /// 由哪条自动化创建。None 表示用户手动开的对话。
    pub automation_id: Option<String>,
    pub activate: bool,
}

impl Workspace {
    /// 手机要看一段 daemon 里没在跑的对话（典型是 daemon 冷重启后桌面只放了占位）。
    /// 照「用户在桌面点开它」的语义拉起来，但不切舞台、不抢焦点。
    /// 恢复还没把占位装回列表时先记下，装回后再兑现。
    pub(crate) fn wake_conversation_for_remote(
        &mut self,
        session_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = self
            .sessions
            .iter()
            .find_map(|session| match &session.kind {
                SessionKind::Conversation(view) if view.read(cx).session_id() == session_id => {
                    Some(view.clone())
                }
                _ => None,
            });
        match view {
            // 一次性闸门在 maybe_auto_resume 里：桌面已经拉起过或拉失败过就不再重试，
            // 和点开时的行为一致。
            Some(view) => view.update(cx, |view, cx| view.maybe_auto_resume(window, cx)),
            None if !self.local.sessions_restored
                && !self.pending_conversation_wakes.contains(&session_id) =>
            {
                self.pending_conversation_wakes.push(session_id);
            }
            None => {}
        }
    }

    pub(crate) fn consume_pending_conversation_wakes(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for session_id in std::mem::take(&mut self.pending_conversation_wakes) {
            self.wake_conversation_for_remote(session_id, window, cx);
        }
    }

    /// 冷启动：专用 OS 线程里 **先 ensure managed 守护，再 reattach 全部会话**。
    /// 完成后才 `check_daemon_outdated`（不与 restore 并行 upgrade）。
    pub(crate) fn schedule_session_restore(
        &mut self,
        runtime: crate::runtimes::Runtime,
        pending: Vec<SessionState>,
        active_session: usize,
        active_session_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.schedule_indexed_session_restore(
            runtime,
            pending.into_iter().enumerate().collect(),
            active_session,
            active_session_id,
            window,
            cx,
        );
    }

    /// 会话恢复到它们所在的运行时上。活动会话只按本机那份认；本机守护的拉起、升级
    /// 检查、远程目录投影也只有本机才做。
    fn schedule_indexed_session_restore(
        &mut self,
        runtime: crate::runtimes::Runtime,
        pending: Vec<(usize, SessionState)>,
        active_session: usize,
        active_session_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = runtime.key().clone();
        let is_local = runtime.is_local();
        let runtime_bg = runtime.clone();
        let restore_revision = self.session_list_revision;
        let restore_active_revision = self.active_session_revision;
        let (acp_saved, pending) = split_indexed_restore_queue(pending);
        // 逐个交货，别攒成一整包：会话之间互不依赖，攒一包等于让窗口空等最慢的那次
        // attach——表现为「冷启动后一个会话都不显示，过一会才全部冒出来」。改成恢复好
        // 一个就发一个，第一个会话立刻上屏，其余陆续补齐。unbounded 保证后台线程不会
        // 因为 UI 还没来得及收而卡住。
        let (tx, rx) = smol::channel::unbounded();
        let (daemon_ready_tx, daemon_ready_rx) = smol::channel::bounded(1);
        std::thread::Builder::new()
            .name("smelt-restore-sessions".into())
            .spawn(move || {
                // 1) 完整 ensure（可能 handoff）→ 2) 再 reattach。禁止与 UI 侧并行 upgrade。
                if runtime_bg.is_local() {
                    let _ = terminal::ensure_managed_daemon_current();
                    terminal::ensure_daemon_running();
                }
                // ACP 占位只有收到这道闸门后才会在 UI 线程创建；否则当前活动会话
                // 会立刻 auto-resume，随后 ensure/handoff 重启守护，把刚建的 socket
                // 踢成「与 smeltd 的连接已断开」。
                if daemon_ready_tx.send_blocking(()).is_err() {
                    return;
                }
                let mut daemon_ok = true;
                for (original_index, ss) in pending {
                    let outcome = if daemon_ok {
                        match spawn_layout_leaves(&runtime_bg, &ss.layout) {
                            Ok(leaves) => Ok(leaves),
                            Err(e) => {
                                if restore_error_blocks_remaining(&e) {
                                    daemon_ok = false;
                                }
                                Err(e)
                            }
                        }
                    } else {
                        Err("smeltd 未就绪（先前会话已失败）".to_string())
                    };
                    // 接收端没了（窗口已关）就别再白跑剩下的
                    if tx.send_blocking((original_index, ss, outcome)).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn smelt-restore-sessions 线程");

        cx.spawn_in(window, async move |this, cx| {
            let mut restored = 0usize;
            let mut restored_order = Vec::new();
            let mut restore_order_intact = true;

            // managed daemon 已经稳定后再把 ACP 会话放回视图树。当前活动 ACP 随后
            // 触发 maybe_auto_resume 时，连接面对的是最终守护进程，不会刚接上又断。
            if daemon_ready_rx.recv().await.is_err() {
                return;
            }
            // ACP 会话的启动命令、profile、智能体定义来自运行时的配置；没读到之前
            // 拼出来的是出厂命令，用户配的命令和环境变量会全部失效。守护已经就绪，
            // 正常一次往返就到。
            loop {
                match this.update(cx, |_, cx| settings::agent_host_loaded(cx)) {
                    Ok(true) => break,
                    Ok(false) => {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(50))
                            .await
                    }
                    Err(_) => return,
                }
            }
            if this
                .update_in(cx, |this, window, cx| {
                    for (original_index, ss) in acp_saved {
                        if this.restore_cancelled(&key, session_state_cwd(&ss).as_deref()) {
                            this.forget_pending_restore(&key, original_index);
                            continue;
                        }
                        let Some(saved) = ss.acp else { continue };
                        let reason = "正在恢复上次的对话…";
                        let agent = saved
                            .agent
                            .as_deref()
                            .and_then(settings::ConversationAgentKind::from_id)
                            .unwrap_or_else(|| acp_agent_from_cmd(&saved.launch.command));
                        let refresh_launch_from_settings = saved.refresh_launch_from_settings();
                        let conversation_binding = saved.conversation_binding.clone();
                        let config_values =
                            settings::initial_acp_config_for(agent, &saved.config_values, cx);
                        let pending_prompt = saved.pending_prompt.clone();
                        let pending_delivery_id = saved.pending_delivery_id.clone();
                        let pending_agent_preset = saved.pending_agent_preset.clone();
                        let agent_definition_id = saved.agent_definition_id.clone();
                        if let (Some(resume_id), Some(definition_id)) = (
                            saved.history_session_id.as_ref(),
                            agent_definition_id.as_deref(),
                        ) {
                            crate::session_history::remember_history_agent(agent.into(),
                                saved.profile_id.as_deref(),
                                &resume_id.to_string(),
                                definition_id, cx);
                        }
                        let automation_id = saved.automation_id.clone();
                        let session_title = saved.session_title.clone();
                        // 第一次连上前由运行时补齐：旧版重启丢掉的插件和 MCP 勾选按当前定义
                        // 补全，完好的快照原样沿用；记忆目录由智能体 id 决定，一并补上。
                        let launch_prep = acp_view::LaunchPrep {
                            agent_definition_id: agent_definition_id.clone(),
                            restore: true,
                        };
                        let view = cx.new(|cx| {
                            acp_view::AcpView::placeholder(
                                cx,
                                acp_view::AcpViewOrigin {
                                    runtime: runtime.clone(),
                                    launch_prep: Some(launch_prep),
                                    agent,
                                    launch: saved.launch,
                                    refresh_launch_from_settings,
                                    profile_id: saved.profile_id,
                                    cwd: saved.cwd,
                                    reason: reason.to_string(),
                                    entries: Vec::new(),
                                    resume_session_id: saved.history_session_id,
                                    saved_sid: saved.sid,
                                },
                            )
                        });
                        view.update(cx, |view, _cx| {
                            view.restore_conversation_binding(conversation_binding);
                            view.restore_config_values(config_values);
                            view.restore_pending_prompt(pending_prompt, pending_delivery_id);
                            view.restore_pending_agent_preset(pending_agent_preset);
                            view.restore_session_title(session_title);
                        });
                        let _acp_persist_sub = Some(this.subscribe_acp_persist(&view, window, cx));
                        if this.session_list_revision != restore_revision {
                            restore_order_intact = false;
                        }
                        let (position, insert_at) = this.restore_insert_at(
                            &key,
                            &restored_order,
                            original_index,
                            &mut restore_order_intact,
                            cx,
                        );
                        this.sessions.insert(
                            insert_at,
                            Session {
                                ui_id: next_session_ui_id(),
                                kind: SessionKind::Conversation(view),
                                last_updated_at: ss.last_updated_at,
                                custom_title: ss.custom_title,
                                agent_definition_id,
                                automation_id,
                                remote_owned: false,
                                _acp_persist_sub,
                                ui_state: ss
                                    .route
                                    .clone()
                                    .map(SessionUiState::restore)
                                    .unwrap_or_default(),
                            },
                        );
                        record_restored_index(
                            &mut restored_order,
                            position,
                            original_index,
                            restore_order_intact,
                        );
                        this.forget_pending_restore(&key, original_index);
                        restored += 1;
                    }
                    this.consume_pending_notification_session_jump(window, cx);
                    this.consume_pending_conversation_wakes(window, cx);
                    cx.notify();
                })
                .is_err()
            {
                return;
            }

            // 收一个渲染一个。后台线程跑完会 drop sender，recv 报错即代表全部处理完。
            while let Ok((original_index, ss, result)) = rx.recv().await {
                let outcome = this.update_in(cx, |this, window, cx| {
                    if this.restore_cancelled(&key, session_state_cwd(&ss).as_deref()) {
                        if let Ok(leaves) = result {
                            for leaf in leaves {
                                terminal::kill_remote(&runtime, &leaf.sid);
                            }
                        }
                        this.forget_pending_restore(&key, original_index);
                        return false;
                    }
                    let leaves = match result {
                        Ok(leaves) => leaves,
                        Err(e) => {
                            eprintln!("[workspace] 会话恢复失败，保留待恢复条目：{e}");
                            return false;
                        }
                    };
                    let mut leaf_iter = leaves.into_iter();
                    let mut tabs = Vec::new();
                    let Some(layout) =
                        rebuild_pane_ready(&runtime, &ss.layout, &mut leaf_iter, &mut tabs, cx)
                    else {
                        return false;
                    };
                    let Some(active) = tabs.get(ss.active).or_else(|| tabs.first()).cloned() else {
                        return false;
                    };
                    if this.session_list_revision != restore_revision {
                        restore_order_intact = false;
                    }
                    let (position, insert_at) = this.restore_insert_at(
                        &key,
                        &restored_order,
                        original_index,
                        &mut restore_order_intact,
                        cx,
                    );
                    this.sessions.insert(
                        insert_at,
                        Session {
                            ui_id: next_session_ui_id(),
                            kind: SessionKind::Term { layout, active },
                            last_updated_at: ss.last_updated_at,
                            custom_title: ss.custom_title,
                            agent_definition_id: None,
                            automation_id: None,
                            remote_owned: false,
                            _acp_persist_sub: None,
                            ui_state: ss
                                .route
                                .clone()
                                .map(SessionUiState::restore)
                                .unwrap_or_default(),
                        },
                    );
                    record_restored_index(
                        &mut restored_order,
                        position,
                        original_index,
                        restore_order_intact,
                    );
                    this.forget_pending_restore(&key, original_index);
                    this.consume_pending_notification_session_jump(window, cx);
                    // 让这一个立刻上屏，不等其余的
                    cx.notify();
                    true
                });
                match outcome {
                    Ok(true) => restored += 1,
                    Ok(false) => {}
                    Err(_) => return, // 窗口已关，收摊
                }
            }

            let _ = this.update_in(cx, |this, window, cx| {
                let Some(space) = this.space_mut(&key) else {
                    // 运行时在恢复期间被移除了，它的会话已经一起摘掉。
                    return;
                };
                space.sessions_restored = true;
                if is_local {
                    let live_ids = this
                        .sessions
                        .iter()
                        .map(|session| crate::live_session_persist_id(session, cx))
                        .collect::<Vec<_>>();
                    this.active_session = crate::resolve_restored_active_session(
                        &live_ids,
                        active_session_id.as_deref(),
                        active_session,
                        &restored_order,
                        restore_order_intact,
                        this.active_session_revision != restore_active_revision,
                        this.active_session,
                    );
                    this.sync_session_ui(window, cx);
                    this.consume_pending_notification_session_jump(window, cx);
                    this.consume_pending_conversation_wakes(window, cx);
                    if let Some(Session {
                        kind: SessionKind::Conversation(view),
                        ..
                    }) = this.sessions.get(this.active_session)
                    {
                        view.update(cx, |view, cx| view.maybe_auto_resume(window, cx));
                    }
                }
                this.save_state(cx);
                let Some(space) = this.space_mut(&key) else {
                    return;
                };
                let failed = space.restore_pending.len();
                eprintln!("[workspace] {key:?} 后台恢复完成：成功 {restored}，失败 {failed}");
                if should_retry_failed_restore(failed, space.restore_retry_attempt) {
                    space.restore_retry_attempt += 1;
                    let attempt = space.restore_retry_attempt;
                    let leftover = space.restore_pending.clone();
                    let delay = restore_retry_delay();
                    if is_local {
                        this.report_background_error(format!(
                            "{failed} 个会话暂时没挂上（守护握手超时），{} 秒后自动重试…",
                            delay.as_secs()
                        ));
                    }
                    let active_session = this.active_session;
                    let active_session_id = this.saved_active_session_id.clone();
                    eprintln!(
                        "[workspace] {failed} 个会话未能恢复，将在 {} 秒后重试（第 {attempt}/{} 次）",
                        delay.as_secs(),
                        RESTORE_RETRY_LIMIT
                    );
                    let runtime = runtime.clone();
                    cx.spawn_in(window, async move |this, cx| {
                        cx.background_executor().timer(delay).await;
                        let _ = this.update_in(cx, |this, window, cx| {
                            let pending = this
                                .space(runtime.key())
                                .map(|space| space.restore_pending.len())
                                .unwrap_or(0);
                            if pending == 0 {
                                if runtime.is_local() {
                                    this.check_daemon_outdated(cx);
                                }
                                return;
                            }
                            eprintln!("[workspace] 重试恢复 {pending} 个会话…");
                            this.schedule_indexed_session_restore(
                                runtime,
                                leftover,
                                active_session,
                                active_session_id,
                                window,
                                cx,
                            );
                        });
                    })
                    .detach();
                } else if is_local {
                    if failed > 0 {
                        this.report_background_error(format!(
                            "{failed} 个会话未能恢复，已保留在存档中。退出并重新打开 Smelt 会再试一次。"
                        ));
                        eprintln!(
                            "[workspace] {failed} 个会话未能恢复，已保留在存档中，下次启动会重试"
                        );
                    }
                    // restore 完成后再查/升级守护，避免与 reattach / 重试并行 handoff。
                    this.check_daemon_outdated(cx);
                    // 恢复完成后刷新会话管理器，但不要在启动时自动删除未连接会话。
                    // 未连接并不等于用户想删除：守护重启、网络短暂中断和恢复竞态都会
                    // 产生这种状态；删除应由用户在会话管理器中明确操作。
                    cx.spawn(async move |this, cx| {
                        let _ = this.update(cx, |this, cx| {
                            if this.session_manager_open {
                                this.refresh_session_manager(cx);
                            }
                        });
                    })
                    .detach();
                }
                if is_local {
                    // 恢复落定后再对一次远程目录：恢复期间我们故意跳过了还在排队的 id，
                    // 真的没恢复成功的（比如终端 reattach 失败）得在这里被重新投影回来，
                    // 不能干等下一次目录推送。
                    this.reconcile_remote_catalog_projection(window, cx);
                }
                this.schedule_visible_refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 当前活动会话（不可变引用）。
    pub(crate) fn cur(&self) -> Option<&Session> {
        self.sessions.get(self.active_session)
    }

    /// 当前会话在本机上的工作目录。本机 git、文件这些查询只能用它：远程会话的路径
    /// 在另一台机器上，拿来问本机要么查不到，要么查到同名的另一个目录。
    pub(crate) fn current_local_cwd(&self, cx: &App) -> Option<String> {
        self.cur()
            .filter(|session| session.runtime(cx).is_local())
            .and_then(|session| session.cwd(cx))
    }

    /// 活动项目在远程运行时上时，工具面板该说的话。面板目前只读本机，这时拿不到项目
    /// 根，但不能说成「没有项目」：项目明明打开着，只是在另一台机器上。
    pub(crate) fn remote_project_notice(&self, cx: &App) -> Option<String> {
        let project = self.active_project_ref(cx)?;
        if project.runtime == crate::RuntimeKey::Local {
            return None;
        }
        let name = crate::runtimes::runtime_name(&project.runtime, cx);
        Some(format!(
            "这是「{name}」上的项目，这个面板暂时只支持本机项目"
        ))
    }

    /// 活动 session 变化时，完整交换右侧工作区。用 session 自己的稳定 ui_id 判断，
    /// 不依赖数组下标，所以拖拽排序、关闭前面的 session 都不会把快照串给别人。
    pub(crate) fn sync_session_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(new_id) = self.cur().map(|session| session.ui_id) else {
            let changed = self.ui_session_id.is_some();
            self.park_active_project_ui();
            self.ui_session_id = None;
            if changed {
                self.schedule_visible_refresh(cx);
            }
            return;
        };
        if self.ui_session_id == Some(new_id) {
            return;
        }
        // 抽屉跟项目走：必须在会话交换之前停放，否则同项目切会话会把新会话
        // 存档里那份过期抽屉当成该项目的现场。
        let parked_key = self.park_active_project_ui();
        if let Some(old_id) = self.ui_session_id {
            if let Some(old_ix) = self.sessions.iter().position(|s| s.ui_id == old_id) {
                swap_session_ui_state(
                    &mut self.active_session_ui,
                    &mut self.sessions[old_ix].ui_state,
                );
                if let Some(key) = parked_key.as_ref()
                    && let Some(ui) = self.project_ui.get(key)
                {
                    ui.copy_persistable_into(&mut self.sessions[old_ix].ui_state);
                }
            }
            if let Some(new_ix) = self.sessions.iter().position(|s| s.ui_id == new_id) {
                swap_session_ui_state(
                    &mut self.active_session_ui,
                    &mut self.sessions[new_ix].ui_state,
                );
            }
        } else if let Some(new_ix) = self.sessions.iter().position(|s| s.ui_id == new_id) {
            // 空工作台没有任何会话可归属。首次挂载直接接管新会话的 UI 状态，
            // 不能把空态期间临时点开的面板存回第一条新会话。
            self.active_session_ui = std::mem::take(&mut self.sessions[new_ix].ui_state);
        }
        self.ui_session_id = Some(new_id);
        self.apply_active_project_ui(window, cx);
        self.schedule_visible_refresh(cx);
    }

    fn project_ui_key(&self, cx: &App) -> Option<String> {
        self.active_project_root(cx)
            .map(|root| crate::sidebar_order::normalize_project_root(&root))
            .filter(|root| !root.is_empty())
    }

    fn park_active_project_ui(&mut self) -> Option<String> {
        park_project_ui(
            &mut self.active_session_ui,
            &mut self.project_ui,
            &mut self.active_project_ui_key,
        )
    }

    fn apply_active_project_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let new_key = self.project_ui_key(cx);
        apply_project_ui_for_root(
            &mut self.active_session_ui,
            &mut self.project_ui,
            &mut self.active_project_ui_key,
            new_key,
        );
        self.restore_active_route_runtime(window, cx);
    }

    /// 抽屉现场按项目根停放。会话没变、只换了选中项目时走这里。
    pub(crate) fn sync_project_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let new_key = self.project_ui_key(cx);
        if self.active_project_ui_key == new_key {
            return;
        }
        self.park_active_project_ui();
        self.apply_active_project_ui(window, cx);
        self.schedule_visible_refresh(cx);
    }

    pub(crate) fn restore_active_route_runtime(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_session_ui.restored_from_archive = false;
        self.file_tree_drag_start = None;
        if let Some(path) = self.pending_restore_file.take() {
            self.view_file(path, window, cx);
        }
    }

    /// 某个 cwd 归属这个运行时里哪个已打开项目（见 project_root_of）。
    pub(crate) fn project_root_for_cwd(
        &self,
        runtime: &crate::runtimes::RuntimeKey,
        cwd: &str,
    ) -> Option<String> {
        project_root_of(&self.space(runtime)?.projects, cwd)
    }

    /// 确保一个会话 cwd 背后的项目是实体，而不是只靠 `project_groups` 临时推导出来的
    /// “隐式组”。否则最后一个会话关闭后，组会跟着 cwd 一起消失，看起来就像删会话
    /// 顺手删了项目。
    ///
    /// 已落在某个已打开项目之下时不新增子项目；只有完全无主的 cwd 才成为项目根。
    /// **git worktree 检出除外**：独立工作区即使落在 home/dev 等已打开
    /// 项目之下，也必须保留为独立项目——否则关闭它最后一个会话后，worktree
    /// 分组会随会话一起消失（`project_root_for_cwd` 会把它的 cwd 吸进父项目，
    /// 骨架里没有它，组就没了）。
    pub(crate) fn remember_session_project(&mut self, runtime: &RuntimeKey, cwd: Option<&str>) {
        let Some(cwd) = cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) else {
            return;
        };
        let root = cwd.trim_end_matches('/');
        // 仓库信息目前只有本机项目有。
        let is_worktree = *runtime == RuntimeKey::Local
            && self
                .repo_info
                .get(root)
                .and_then(|(_, info)| info.as_ref())
                .is_some_and(|info| info.is_worktree());
        if !is_worktree && self.project_root_for_cwd(runtime, root).is_some() {
            return;
        }
        let Some(space) = self.space_mut(runtime) else {
            return;
        };
        if !space
            .projects
            .iter()
            .any(|p| p.trim_end_matches('/') == root)
        {
            space.projects.push(root.to_string());
        }
    }

    /// 侧栏分组（见 ProjectGroup）。骨架是 `self.local.projects`——项目独立于会话存在，所以
    /// 一个会话都没有的项目照样出现（sessions 为空）。会话按 cwd 挂到所属项目下；挂不上
    /// 的（旧会话、临时目录）仍按自己的 cwd 自建隐式组接在后面。
    ///
    /// 分组身份一律用 **root 路径**：末段同名的两个目录是两个项目，各占一行、会话不混。
    /// 显示名重复时由 disambiguate_labels 补父目录段区分。
    ///
    /// 侧栏渲染和拖拽排序共用同一份算法，避免两处各算一遍、行为跑偏。worktree 检出
    /// 显示「仓库名 · 分支名」（见 group_info_for_cwd），且跟主仓库、其余 worktree 聚在
    /// 一起排序，不会散落在列表各处——组间相对顺序按「同一簇里最早出现的组」的先后来
    /// （stable_sort，不会无意义打乱手动拖拽过的顺序）。
    pub(crate) fn project_groups(
        &self,
        runtime: &crate::runtimes::RuntimeKey,
        cx: &App,
    ) -> Vec<ProjectGroup> {
        let Some(space) = self.space(runtime) else {
            return Vec::new();
        };
        let mut groups: Vec<ProjectGroup> = Vec::new();
        // 聚簇 key（worktree 与主仓库共享 common-dir）与消歧用的原始显示名，按组下标存。
        let mut clusters: Vec<Option<String>> = Vec::new();
        let mut bases: Vec<String> = Vec::new();
        let same_root = |a: &str, b: &str| a.trim_end_matches('/') == b.trim_end_matches('/');

        // 骨架：已打开的项目按列表顺序占位，先不管有没有会话。
        for root in &space.projects {
            if groups.iter().any(|g| same_root(&g.root, root)) {
                continue;
            }
            let (label, cluster) = self.group_info_for_cwd(root);
            bases.push(label.clone());
            clusters.push(cluster);
            groups.push(ProjectGroup {
                runtime: runtime.clone(),
                root: root.clone(),
                label,
                sessions: Vec::new(),
            });
        }
        // 会话挂到所属项目下；无主的按自己的 cwd 自建一组。所有 git worktree
        // 都以自身路径成组，避免被已打开的父目录项目吸收；这条规则不依赖创建
        // worktree 的具体插件。
        for (ix, s) in self.sessions.iter().enumerate() {
            if s.is_product_conversation(cx) || s.runtime(cx).key() != runtime {
                continue;
            }
            let cwd = s.cwd(cx).unwrap_or_default();
            let is_worktree = self
                .repo_info
                .get(&cwd)
                .and_then(|(_, info)| info.as_ref())
                .is_some_and(|info| info.is_worktree());
            let root = if is_worktree {
                cwd.clone()
            } else {
                self.project_root_for_cwd(runtime, &cwd)
                    .unwrap_or_else(|| cwd.clone())
            };
            match groups.iter_mut().find(|g| same_root(&g.root, &root)) {
                Some(g) => g.sessions.push(ix),
                None => {
                    let (label, cluster) = self.group_info_for_cwd(&root);
                    bases.push(label.clone());
                    clusters.push(cluster);
                    groups.push(ProjectGroup {
                        runtime: runtime.clone(),
                        root,
                        label,
                        sessions: vec![ix],
                    });
                }
            }
        }

        // 同仓库（主仓库 + 各 worktree）聚到一起：按「这一簇里最早出现的组」排。
        let key_of = |i: usize| {
            clusters[i]
                .clone()
                .unwrap_or_else(|| groups[i].root.clone())
        };
        let mut first_seen: HashMap<String, usize> = HashMap::new();
        for i in 0..groups.len() {
            first_seen.entry(key_of(i)).or_insert(i);
        }
        let mut order: Vec<usize> = (0..groups.len()).collect();
        order.sort_by_key(|&i| first_seen[&key_of(i)]);
        let mut sorted: Vec<ProjectGroup> = Vec::with_capacity(groups.len());
        let mut sorted_bases: Vec<String> = Vec::with_capacity(groups.len());
        for i in order {
            sorted.push(ProjectGroup {
                runtime: groups[i].runtime.clone(),
                root: groups[i].root.clone(),
                label: groups[i].label.clone(),
                sessions: groups[i].sessions.clone(),
            });
            sorted_bases.push(bases[i].clone());
        }
        disambiguate_labels(&mut sorted, &sorted_bases);
        sorted
    }

    /// 项目内会话拖拽排序。跨项目直接不动——会话归属由 cwd 决定，拖拽只改显示顺序。
    /// 成功后走 `save_state`：`sessions` 数组顺序就是落盘顺序。
    pub(crate) fn move_session_near(
        &mut self,
        dragged: u64,
        target: u64,
        before: bool,
        drop_top: Option<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if dragged == target {
            self.sess_drop_hint = None;
            return;
        }
        let Some(from_ix) = self
            .sessions
            .iter()
            .position(|session| session.ui_id == dragged)
        else {
            return;
        };
        let Some(target_ix) = self
            .sessions
            .iter()
            .position(|session| session.ui_id == target)
        else {
            return;
        };
        let groups: Vec<Vec<usize>> = self
            .all_project_groups(cx)
            .into_iter()
            .map(|group| group.sessions)
            .collect();
        if !indices_share_group(&groups, from_ix, target_ix) {
            self.sess_drop_hint = None;
            cx.notify();
            return;
        }
        let active_id = self.cur().map(|session| session.ui_id);
        if !reorder_vec(&mut self.sessions, from_ix, target_ix, before) {
            self.sess_drop_hint = None;
            return;
        }
        if let Some(id) = active_id
            && let Some(ix) = self.sessions.iter().position(|session| session.ui_id == id)
        {
            self.active_session = ix;
        }
        self.sess_drop_hint = None;
        self.sidebar_drag = None;
        self.sidebar_reorder_generation += 1;
        self.sidebar_drop_origin = drop_top.map(|top| (crate::SidebarDrag::Session(dragged), top));
        self.stop_sidebar_drag_scroll();
        self.save_state(cx);
        cx.notify();
    }

    /// 项目卡片拖拽排序。改的是 `projects` 骨架；会话仍挂在各自 cwd 下。
    /// 拖进固定区会固定、拖出则取消固定，避免置顶后拖下去又弹回顶部。
    /// `save_state` 会把这份有序根目录写进工作区类型化快照。
    /// 项目顺序是每个运行时各自的，跨运行时拖不动。
    pub(crate) fn move_project_near(
        &mut self,
        from: &ProjectRef,
        to: &ProjectRef,
        before: bool,
        drop_top: Option<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let moved = from.runtime == to.runtime
            && self.space_mut(&from.runtime).is_some_and(|space| {
                crate::sidebar_order::apply_pinned_project_drop(
                    &mut space.projects,
                    &mut space.pinned_projects,
                    &from.root,
                    &to.root,
                    before,
                )
            });
        if !moved {
            self.proj_drop_hint = None;
            return;
        }
        self.proj_drop_hint = None;
        self.sidebar_drag = None;
        self.sidebar_reorder_generation += 1;
        self.sidebar_drop_origin =
            drop_top.map(|top| (crate::SidebarDrag::Project(from.clone()), top));
        self.stop_sidebar_drag_scroll();
        self.save_state(cx);
        cx.notify();
    }

    /// 「+」/新建：开一个独立新会话（单终端），并切过去。
    pub(crate) fn add_session(
        &mut self,
        runtime: crate::runtimes::Runtime,
        cwd: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.add_session_with_launch(runtime, cwd, None, cx);
    }

    pub(crate) fn touch_acp_session(&mut self, view: &Entity<acp_view::AcpView>) {
        let view_id = view.entity_id();
        if let Some(session) = self.sessions.iter_mut().find(|session| {
            matches!(
                &session.kind,
                SessionKind::Conversation(existing) if existing.entity_id() == view_id
            )
        }) {
            session.last_updated_at = unix_now_secs();
        }
    }

    /// ACP 会话内容变化（AcpViewEvent::Changed）→ 立即 save_state。与侧栏/文件树
    /// resize 订阅同一惯用法（main.rs::new 里的 _resize_sub）。
    pub(crate) fn subscribe_acp_persist(
        &mut self,
        view: &Entity<acp_view::AcpView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Subscription {
        cx.subscribe_in(
            view,
            window,
            |this: &mut Self, _view, ev: &acp_view::AcpViewEvent, window, cx| match ev {
                acp_view::AcpViewEvent::Changed => {
                    this.touch_acp_session(_view);
                    this.persist_pending_history_title_for_view(_view, cx);
                    this.persist_history_agent_definition_for_view(_view, cx);
                    this.save_state(cx);
                    // save_state 只排队写盘，不会重绘。侧栏「运行中」读的是
                    // Session::status，视图相位一变就要立刻刷新，不能等守护
                    // 镜像 300ms 合并窗口。
                    this.sync_notification_surfaces(cx);
                    cx.notify();
                }
                acp_view::AcpViewEvent::ConfigSelected {
                    config_id,
                    value_id,
                } => {
                    let agent = _view.read(cx).agent_kind();
                    let config_id = config_id.clone();
                    let value_id = value_id.clone();
                    settings::apply_agent_host(
                        move |config| config.remember_acp_config_value(agent, config_id, value_id),
                        cx,
                    );
                    // 同一次选择既要成为该 agent 的下次默认，也要保存进当前会话，
                    // 后者在冷恢复时拥有更高优先级。
                    this.touch_acp_session(_view);
                    this.save_state(cx);
                    this.sync_notification_surfaces(cx);
                    cx.notify();
                }
                acp_view::AcpViewEvent::PreviewImage(image) => {
                    this.acp_image_preview = Some(image.clone());
                    // 解码进按图片 id 索引的缓存。完成时刷新窗口，渲染按当前这张图查缓存。
                    smelt_ui::image::ensure_preview(image, cx);
                    cx.notify();
                }
                acp_view::AcpViewEvent::NewSession(request) => {
                    let agent_definition = this.sessions.iter().find_map(|session| {
                        let SessionKind::Conversation(existing) = &session.kind else {
                            return None;
                        };
                        if existing.entity_id() != _view.entity_id() {
                            return None;
                        }
                        let id = session.agent_definition_id.as_deref()?;
                        cx.global::<settings::AgentHostState>()
                            .agents
                            .iter()
                            .find(|agent| agent.id == id)
                            .cloned()
                    });
                    this.add_acp_session(
                        NewAcpSessionRequest {
                            // 从哪段对话里新开，就开在那段对话所在的运行时上。
                            runtime: _view.read(cx).runtime().clone(),
                            agent: request.agent,
                            launch_override: Some(request.launch.clone()),
                            profile_id: request.profile_id.clone(),
                            agent_definition,
                            fallback_cwd: request.cwd.clone(),
                            pending_prompt: None,
                            automation_id: None,
                            activate: true,
                        },
                        window,
                        cx,
                    );
                }
                acp_view::AcpViewEvent::ForkConversation(request) => {
                    this.add_acp_handoff_session(request.as_ref().clone(), window, cx);
                }
                acp_view::AcpViewEvent::CompletedTurn { .. } => {
                    this.touch_acp_session(_view);
                }
                acp_view::AcpViewEvent::FailedTurn { .. } => {
                    this.touch_acp_session(_view);
                }
                acp_view::AcpViewEvent::Ended {
                    kind,
                    reason: _,
                    delivery_id,
                } => {
                    this.touch_acp_session(_view);
                    let sid = _view.read(cx).session_id().to_string();
                    if kind.is_terminated() {
                        // 会话已在 daemon 侧被终结（本机或手机点了删除）：只拆视图。
                        // 不能重连——重连会用同一个 sid 重新 acp_open，把删掉的会话
                        // 原地复活；也不必再发一次 kill。
                        if let Some(ix) = this.acp_session_index(&sid, cx) {
                            this.remove_session(ix, false, cx);
                        }
                        return;
                    }
                    if super::daemon_owns_acp_delivery_session(delivery_id.as_deref()) {
                        // daemon 会在同一进程内归约 Ended 并执行重连；多个
                        // GUI 只能旁观，否则每个窗口都会各发一次 restart。
                        return;
                    }
                    if kind.is_transient() {
                        // 当前没有 daemon 持有的执行时，不论会话采用哪个
                        // Agent/Controller，都沿用 GUI 的可视重连体验。
                        let view = _view.clone();
                        cx.spawn_in(window, async move |_this, cx| {
                            let executor = cx.background_executor().clone();
                            // 指数退避循环（500ms 起、翻倍到 8s 上限），不写死
                            // 单次等待——短故障（升级 exec 一两秒）很快重连，长
                            // 故障间隔自动拉长，预算在 AcpView 内（6 次）防风暴。
                            let mut delay = std::time::Duration::from_millis(500);
                            loop {
                                executor.timer(delay).await;
                                let step = view.update_in(cx, |view, window, cx| {
                                    // Starting 只是 provider 正在重建，不能重置预算；
                                    // 等 daemon 返回可用相位。
                                    match view.phase() {
                                        smelt_client::daemon_state::DaemonPhase::Connecting => {
                                            return AcpReconnectStep::Waiting;
                                        }
                                        smelt_client::daemon_state::DaemonPhase::Dead => {}
                                        _ => return AcpReconnectStep::Recovered,
                                    }
                                    if view.maybe_auto_reconnect(window, cx) {
                                        AcpReconnectStep::Retrying
                                    } else {
                                        AcpReconnectStep::GiveUp
                                    }
                                });
                                match step {
                                    // 视图销毁 / 会话已恢复：都不是失败，收尾即可。
                                    Err(_) | Ok(AcpReconnectStep::Recovered) => break,
                                    Ok(AcpReconnectStep::GiveUp) => {
                                        // 预算耗尽：只有会话仍 Ended 才清理普通交互
                                        // session；带 delivery 的 daemon 会话不会进入这个分支。
                                        let still_ended = view
                                            .update_in(cx, |view, _window, _cx| {
                                                matches!(
                                                    view.phase(),
                                                    smelt_client::daemon_state::DaemonPhase::Dead
                                                )
                                            })
                                            .unwrap_or(false);
                                        if still_ended {
                                            Self::kill_daemon_session(&sid);
                                        }
                                        break;
                                    }
                                    Ok(AcpReconnectStep::Retrying) => {
                                        delay = (delay * 2).min(std::time::Duration::from_secs(8));
                                    }
                                    Ok(AcpReconnectStep::Waiting) => {}
                                }
                            }
                        })
                        .detach();
                    } else {
                        // 普通交互会话不可恢复时只清理 runtime。
                        Self::kill_daemon_session(&sid);
                    }
                }
                acp_view::AcpViewEvent::Recovered => {
                    // 保留视图自身的快照更新。
                }
            },
        )
    }

    pub(crate) fn add_acp_handoff_session(
        &mut self,
        mut request: acp_view::AcpHandoffRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source_sid = request
            .source
            .as_ref()
            .filter(|origin| !origin.from_history)
            .map(|origin| origin.session_id.clone());
        let agent_definition = source_sid.as_deref().and_then(|sid| {
            self.sessions.iter().find_map(|session| {
                let SessionKind::Conversation(view) = &session.kind else {
                    return None;
                };
                if view.read(cx).session_id() != sid {
                    return None;
                }
                let id = session.agent_definition_id.as_deref()?;
                cx.global::<settings::AgentHostState>()
                    .agents
                    .iter()
                    .find(|agent| agent.id == id)
                    .cloned()
            })
        });
        let agent_definition_id = agent_definition.map(|definition| definition.id);
        if agent_definition_id.is_some() {
            request.launch_prep = Some(acp_view::LaunchPrep {
                agent_definition_id: agent_definition_id.clone(),
                restore: false,
            });
        }
        if !smelt_model::session_control::is_agent_conversation(
            None,
            agent_definition_id.as_deref(),
            request.cwd.as_deref(),
        ) {
            self.remember_session_project(&RuntimeKey::Local, request.cwd.as_deref());
        }
        if agent_definition_id.is_none() {
            request.config_values =
                settings::initial_acp_config_for(request.agent, &request.config_values, cx);
        }
        let view = cx.new(|cx| acp_view::AcpView::start_with_handoff(window, cx, request));
        let _acp_persist_sub = Some(self.subscribe_acp_persist(&view, window, cx));
        self.sessions.push(Session {
            ui_id: next_session_ui_id(),
            kind: SessionKind::Conversation(view.clone()),
            last_updated_at: unix_now_secs(),
            custom_title: None,
            agent_definition_id,
            automation_id: None,
            remote_owned: false,
            _acp_persist_sub,
            ui_state: SessionUiState::default(),
        });
        self.session_list_revision = self.session_list_revision.wrapping_add(1);
        self.active_session = self.sessions.len() - 1;
        self.sync_session_ui(window, cx);
        view.update(cx, |view, cx| view.focus_input(window, cx));
        self.save_state(cx);
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 「+」菜单「对话 · smelt 原生界面」下那几项：新建 ACP 会话（第二种会话类型，
    /// 结构化消息流）。`agent` 决定接哪家（Claude / Copilot / Codex），命令从对应的
    /// 全局配置取。spawn_acp 只起线程立即返回，不需要 add_session_with_launch 那套
    /// 后台三段舞。
    pub(crate) fn add_acp_session(
        &mut self,
        request: NewAcpSessionRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 启动命令、profile、智能体定义都来自运行时的配置。还没读到就不起会话：
        // 按出厂命令启动的话，用户配的命令和环境变量全不生效。
        if !settings::agent_host_loaded(cx) {
            crate::status_item::notify_error("正在读取运行时的智能体配置，请稍后再试");
            return;
        }
        let NewAcpSessionRequest {
            runtime,
            agent,
            launch_override,
            profile_id,
            agent_definition,
            fallback_cwd,
            pending_prompt,
            automation_id,
            activate,
        } = request;
        // 工作目录属于这次对话/Run，而非 AgentDefinition；项目菜单或触发器传来
        // 的上下文就是本次执行上下文。
        let cwd = fallback_cwd;
        // 设置里的启动命令、环境变量和初始配置都是本机的。远程那台机器有自己的 CLI 和
        // 配置，不能把本机的命令和环境变量（可能含密钥）带过去：先用引擎默认值，
        // 能力库按运行时切换后改成读那台机器自己的设置。
        let local = runtime.is_local();
        let launch = launch_override.unwrap_or_else(|| {
            if local {
                settings::acp_launch_for(agent, cx)
            } else {
                agent.default_launch()
            }
        });
        let agent_model = agent_definition
            .as_ref()
            .and_then(|definition| definition.model_config_value());
        let agent_definition_id = agent_definition.map(|definition| definition.id);
        let is_agent_conversation = smelt_model::session_control::is_agent_conversation(
            automation_id.as_deref(),
            agent_definition_id.as_deref(),
            cwd.as_deref(),
        );
        if !is_agent_conversation && automation_id.is_none() {
            self.remember_session_project(runtime.key(), cwd.as_deref());
        }
        let mut initial_config = if !local {
            Vec::new()
        } else if is_agent_conversation {
            settings::initial_agent_conversation_config_for(agent, cx)
        } else {
            settings::initial_acp_config_for(agent, &[], cx)
        };
        if let Some(value) = agent_model {
            initial_config.retain(|(id, _)| id != "model");
            initial_config.push(("model".to_string(), value));
        }
        // 智能体的指令、插件、MCP、记忆目录由运行时在第一次连接前补齐。
        let launch_prep = acp_view::LaunchPrep {
            agent_definition_id: agent_definition_id.clone(),
            restore: false,
        };
        let view = cx.new(|cx| {
            acp_view::AcpView::start(
                window,
                cx,
                runtime,
                agent,
                launch,
                Some(launch_prep),
                profile_id,
                cwd,
                None,
            )
        });
        view.update(cx, |view, _cx| {
            view.restore_config_values(initial_config);
            view.restore_pending_prompt(pending_prompt, None);
        });
        let _acp_persist_sub = Some(self.subscribe_acp_persist(&view, window, cx));
        self.sessions.push(Session {
            ui_id: next_session_ui_id(),
            kind: SessionKind::Conversation(view.clone()),
            last_updated_at: unix_now_secs(),
            custom_title: None,
            agent_definition_id,
            automation_id,
            remote_owned: false,
            _acp_persist_sub,
            ui_state: SessionUiState::default(),
        });
        self.session_list_revision = self.session_list_revision.wrapping_add(1);
        let ix = self.sessions.len() - 1;
        if activate {
            self.activate(ix, window, cx);
        } else {
            self.save_state(cx);
            self.schedule_visible_refresh(cx);
            cx.notify();
        }
    }

    /// 找当前已开的、匹配某个 agent+cwd+具体 session id 的 ACP 会话下标——「继续」
    /// 点击时和后台加载完成时各查一次，两处逻辑必须完全一致，抽出来避免漂移。
    pub(crate) fn find_open_acp_session(
        &self,
        agent: settings::ConversationAgentKind,
        cwd: &str,
        target_id: &agent_client_protocol::schema::v1::SessionId,
        cx: &App,
    ) -> Option<usize> {
        self.sessions.iter().position(|s| match &s.kind {
            SessionKind::Conversation(view) => {
                let v = view.read(cx);
                v.agent_kind() == agent
                    && v.cwd().as_deref() == Some(cwd)
                    && v.history_session_id_for_save().as_ref() == Some(target_id)
            }
            _ => false,
        })
    }

    /// 历史会话页「继续」：同一条 agent session 已经开着就直接跳过去，否则建
    /// 一个空的运行时投影并带上 `history_session_id`。激活后由 `session/load` 让
    /// agent 重放历史，Smelt 不再从各家私有 transcript 预填第二份消息快照。
    pub fn resume_acp_session(
        &mut self,
        request: AcpResumeRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let AcpResumeRequest {
            agent,
            launch_override,
            profile_id,
            cwd,
            resume_id,
        } = request;
        // 已经开着的必须是**这一条**历史会话（比对 agent + cwd + 具体 session id），
        // 不能只认 agent+cwd——同项目同 agent 可能同时开着好几条不同的历史会话，
        // 之前只按 agent+cwd 找，点哪条「继续」都会跳到"第一个凑巧匹配的"那条。
        let target_id = agent_client_protocol::schema::v1::SessionId::new(resume_id.clone());
        if let Some(ix) = self.find_open_acp_session(agent, &cwd, &target_id, cx) {
            let view = match &self.sessions[ix].kind {
                SessionKind::Conversation(view) => view.clone(),
                _ => unreachable!("find_open_acp_session only returns ACP sessions"),
            };
            self.activate(ix, window, cx);
            // “继续”是一次显式恢复请求，不能只激活可能已经断线、内容为空的旧
            // View。重新 attach 会让 smeltd 发送 offset=0 的完整权威快照。
            view.update(cx, |view, cx| view.reattach_to_daemon(window, cx));
            return;
        }

        // 续接必须按原来的智能体定义启动：space 目录或历史绑定都能认出主人。
        // 只认 space 的话，项目里开的智能体对话会退化成裸引擎，插件和人设全丢。
        let definition =
            smelt_model::agent_paths::agent_definition_id_for_space(std::path::Path::new(&cwd))
                .or_else(|| {
                    // 历史列表加载时运行时一并给了「这段会话属于哪个智能体」。
                    self.history_agent_bindings
                        .get(&crate::session_history::session_list_key(
                            agent.into(),
                            profile_id.as_deref(),
                            &cwd,
                        ))
                        .and_then(|bindings| bindings.get(&resume_id).cloned())
                })
                .and_then(|id| {
                    cx.global::<settings::AgentHostState>()
                        .agents
                        .iter()
                        .find(|agent| agent.id == id)
                        .cloned()
                });
        let launch = launch_override.unwrap_or_else(|| settings::acp_launch_for(agent, cx));
        let initial_config = if definition.is_some() {
            settings::initial_agent_conversation_config_for(agent, cx)
        } else {
            settings::initial_acp_config_for(agent, &[], cx)
        };
        // 智能体名是动态身份，不是用户给某段会话的命名覆盖层。无用户标题时，
        // Session::title 会按自动标题、当前智能体名依次回退。
        let custom_title = crate::HistoryTitles::get(
            settings::HistorySourceKind::from(agent).id(),
            profile_id.as_deref(),
            &resume_id,
            cx,
        );
        let agent_definition_id = definition.map(|definition| definition.id);
        if let Some(id) = agent_definition_id.as_deref() {
            crate::session_history::remember_history_agent(
                agent.into(),
                profile_id.as_deref(),
                &resume_id,
                id,
                cx,
            );
        }
        let view_profile_id = profile_id;
        let view = cx.new(|cx| {
            acp_view::AcpView::placeholder(
                cx,
                acp_view::AcpViewOrigin {
                    runtime: crate::runtimes::Runtime::local(),
                    launch_prep: Some(acp_view::LaunchPrep {
                        agent_definition_id: agent_definition_id.clone(),
                        restore: false,
                    }),
                    agent,
                    launch,
                    refresh_launch_from_settings: view_profile_id.is_none(),
                    profile_id: view_profile_id,
                    cwd: Some(cwd),
                    reason: "正在加载历史会话…".to_string(),
                    entries: Vec::new(),
                    resume_session_id: Some(target_id),
                    // 新起 smeltd 托管连接，靠 agent session id 做 session/load；
                    // 它不是已存在的守护会话，因此不能沿用 smeltd id。
                    saved_sid: None,
                },
            )
        });
        view.update(cx, |view, _cx| view.restore_config_values(initial_config));
        let _acp_persist_sub = Some(self.subscribe_acp_persist(&view, window, cx));
        self.sessions.push(Session {
            ui_id: next_session_ui_id(),
            kind: SessionKind::Conversation(view),
            last_updated_at: unix_now_secs(),
            custom_title,
            agent_definition_id,
            automation_id: None,
            remote_owned: false,
            _acp_persist_sub,
            ui_state: SessionUiState::default(),
        });
        self.session_list_revision = self.session_list_revision.wrapping_add(1);
        let ix = self.sessions.len() - 1;
        self.activate(ix, window, cx);
        self.save_state(cx);
    }

    /// 历史会话页「CLI/TUI 继续」：在 Smelt 的 PTY 中启动各家 CLI 自己的交互式
    /// 恢复命令。它不经过 ACP，也不复制历史文本；agent CLI 直接从自己的 session
    /// store 恢复原会话。profile 的环境变量沿用历史页当前 tab 的 ACP 配置。
    pub fn resume_cli_session(
        &mut self,
        agent: settings::HistorySourceKind,
        launch_override: Option<smelt_model::agent_kind::ConversationLaunchSpec>,
        cwd: String,
        resume_id: String,
        cx: &mut Context<Self>,
    ) {
        let base = cli_launch_entry_for_agent(agent, cx);
        // 没有终端 CLI 的 agent（dsh）在这里安静地什么都不做：起一个别家的
        // CLI，或者起一条拼不出来的空命令，都比不做更糟。
        let Some(mut command) = cli_resume_command(agent, &base.command, &resume_id) else {
            return;
        };
        if let Some(launch) = launch_override {
            for (name, value) in launch.env.iter().rev() {
                let value = smelt_model::workspace_override::expand_tilde(value);
                command = format!("{name}={} {command}", shell_quote(&value));
            }
        }
        // 历史页读的是本机运行时的会话存档。
        self.add_session_with_launch(
            crate::runtimes::Runtime::local(),
            Some(cwd),
            Some(settings::LaunchEntry {
                label: format!("{} TUI", agent.short_label()),
                command,
                provider: Some(agent.id().to_string()),
            }),
            cx,
        );
    }

    /// 项目行「+」下拉菜单的快捷入口：`launch` 编进 shell 的启动命令行（见
    /// terminal.rs::spawn / `smeltd::terminal_registry`），`label` 用作侧栏初始显示名。
    ///
    /// **禁止**在 UI/`update`/拖放 FFI 回调里同步 `Terminal::spawn`：连守护 + 握手
    /// 含 sleep/超时，拖文件夹进窗口会整窗 beachball（见 `confirm_restart_daemon`）。
    /// 专用 OS 线程做阻塞 spawn，主线程只接结果建 Entity（比塞进 async executor 更稳）。
    pub(crate) fn add_session_with_launch(
        &mut self,
        runtime: crate::runtimes::Runtime,
        cwd: Option<String>,
        entry: Option<settings::LaunchEntry>,
        cx: &mut Context<Self>,
    ) {
        self.remember_session_project(runtime.key(), cwd.as_deref());
        let runtime_bg = runtime.clone();
        // spawn 在后台；先把项目落盘，即使进程启动失败也不能让用户刚选中的项目消失。
        self.save_state(cx);
        let sid = new_sid();
        let cwd_bg = cwd.clone();
        let launch_owned = entry.as_ref().map(|entry| entry.command.clone());
        let label_owned = entry.as_ref().map(|entry| entry.label.clone());
        let sid_bg = sid.clone();
        let launch_bg = launch_owned.clone();
        eprintln!("[workspace] 新建会话 cwd={cwd:?} launch={launch_owned:?} sid={sid}");
        cx.notify();

        let (tx, rx) = smol::channel::bounded(1);
        std::thread::Builder::new()
            .name("smelt-spawn-session".into())
            .spawn(move || {
                let r = terminal::Terminal::spawn(
                    &runtime_bg,
                    24,
                    80,
                    cwd_bg.as_deref(),
                    &sid_bg,
                    launch_bg.as_deref(),
                );
                let _ = tx.send_blocking(r);
            })
            .expect("spawn smelt-spawn-session 线程");

        cx.spawn(async move |this, cx| {
            let result = match rx.recv().await {
                Ok(r) => r,
                Err(_) => {
                    let _ = this.update(cx, |this, cx| {
                        this.report_background_error("新建会话内部通道断开，请重试");
                        cx.notify();
                    });
                    return;
                }
            };
            let terminal = match result {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[workspace] 新建会话失败（{cwd:?}）：{e:#}");
                    let msg = format!("新建会话失败：{e:#}");
                    let _ = this.update(cx, |this, cx| {
                        this.report_background_error(msg);
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                let view = cx.new(|cx| {
                    TerminalView::from_terminal(
                        cx,
                        runtime.clone(),
                        terminal,
                        cwd.clone(),
                        sid,
                        launch_owned.as_deref(),
                        label_owned.as_deref(),
                    )
                });
                this.sessions.push(Session::single(view));
                this.session_list_revision = this.session_list_revision.wrapping_add(1);
                this.active_session = this.sessions.len() - 1;
                this.save_state(cx);
                this.schedule_visible_refresh(cx);
                eprintln!(
                    "[workspace] 新建会话成功，当前共 {} 个",
                    this.sessions.len()
                );
                cx.notify();
            });
        })
        .detach();
    }

    /// 在当前会话的活动 pane 上分屏：Horizontal=右侧并排，Vertical=下方堆叠。
    /// ACP 会话没有分屏树，直接忽略。
    pub(crate) fn split_active(&mut self, axis: Axis, cx: &mut Context<Self>) {
        let Some(sess) = self.cur() else { return };
        let Some(active) = sess.active_term() else {
            return;
        };
        // 新 pane 和当前 pane 在同一台机器上。退回本进程的当前目录只对本机有意义。
        let runtime = active.read(cx).runtime().clone();
        let cwd = active
            .read(cx)
            .cwd()
            .or_else(|| runtime.is_local().then(current_dir).flatten());
        let old = sess.anchor_id();
        let session_ix = self.active_session;
        let sid = new_sid();
        let cwd_bg = cwd.clone();
        let sid_bg = sid.clone();
        let runtime_bg = runtime.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    terminal::Terminal::spawn(&runtime_bg, 24, 80, cwd_bg.as_deref(), &sid_bg, None)
                })
                .await;
            let terminal = match result {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("[workspace] 分屏失败（{cwd:?}）：{e:#}");
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                // 分屏目标会话可能在握手期间被关掉——对不上就丢弃这个终端。
                if session_ix >= this.sessions.len() {
                    eprintln!("[workspace] 分屏目标会话已不存在，丢弃");
                    return;
                }
                let view = cx.new(|cx| {
                    TerminalView::from_terminal(cx, runtime, terminal, cwd, sid, None, None)
                });
                let state = cx.new(|_| ResizableState::default());
                let sess = &mut this.sessions[session_ix];
                // old 叶子若已被拆掉/关掉，split_leaf 找不到就不动。
                let Some(layout) = sess.term_layout_mut() else {
                    eprintln!("[workspace] 分屏目标会话不是终端会话，丢弃");
                    return;
                };
                if !split_leaf(layout, old, axis, state, view.clone()) {
                    eprintln!("[workspace] 分屏目标 pane 已不存在，丢弃");
                    return;
                }
                sess.set_active_term(view);
                this.save_state(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 把 PC 当前真实侧栏投影成无 UI 的共享菜单。项目标签使用 project_groups 的
    /// 消歧结果，会话标题直接使用 Session::title，因此移动端无需复制任何显示规则。
    /// 一个运行时那段侧栏的跨端菜单投影，发给那个运行时，连它的手机按它显示。
    pub(crate) fn workspace_menu_snapshot(
        &self,
        runtime: &RuntimeKey,
        cx: &App,
    ) -> smelt_model::workspace_menu::WorkspaceMenuSnapshot {
        use smelt_model::workspace_menu::{
            WorkspaceMenuProject, WorkspaceMenuSession, WorkspaceMenuSessionKind,
            WorkspaceMenuSnapshot,
        };

        let groups = self.project_groups(runtime, cx);
        let projects = groups
            .iter()
            .enumerate()
            .map(|(order, group)| WorkspaceMenuProject {
                root: group.root.clone(),
                title: group.label.clone(),
                order: order.min(u32::MAX as usize) as u32,
            })
            .collect();
        let mut membership = vec![None; self.sessions.len()];
        for (project_order, group) in groups.iter().enumerate() {
            for &session_index in &group.sessions {
                if let Some(slot) = membership.get_mut(session_index) {
                    *slot = Some((project_order, group));
                }
            }
        }

        let sessions = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| session.runtime(cx).key() == runtime)
            .flat_map(|(session_order, session)| {
                let project = membership.get(session_order).and_then(|value| *value);
                let project_root = project.map(|(_, group)| group.root.clone());
                let project_title = project.map(|(_, group)| group.label.clone());
                let project_order = project
                    .map(|(order, _)| order.min(u32::MAX as usize) as u32)
                    .unwrap_or(u32::MAX);
                let session_order = session_order.min(u32::MAX as usize) as u32;

                match &session.kind {
                    SessionKind::Term { .. } => {
                        let leaves = session.term_leaves();
                        let single_pane = leaves.len() == 1;
                        leaves
                            .into_iter()
                            .enumerate()
                            .map(|(leaf_order, pane)| WorkspaceMenuSession {
                                id: pane.read(cx).session_id().to_string(),
                                kind: WorkspaceMenuSessionKind::Terminal,
                                title: if single_pane {
                                    session.title(cx)
                                } else {
                                    pane_title(&pane, cx)
                                },
                                custom_title: if single_pane {
                                    session.custom_title.is_some()
                                        || pane.read(cx).custom_title().is_some()
                                } else {
                                    pane.read(cx).custom_title().is_some()
                                },
                                cwd: pane.read(cx).cwd(),
                                project_root: project_root.clone(),
                                project_title: project_title.clone(),
                                project_order,
                                session_order,
                                leaf_order: leaf_order.min(u32::MAX as usize) as u32,
                                agent: pane_provider_kind(&pane, cx)
                                    .map(|provider| provider.id().to_string()),
                            })
                            .collect::<Vec<_>>()
                    }
                    SessionKind::Conversation(view) => {
                        let view = view.read(cx);
                        vec![WorkspaceMenuSession {
                            id: view.session_id().to_string(),
                            kind: WorkspaceMenuSessionKind::Acp,
                            title: session.title(cx),
                            custom_title: session.custom_title.is_some(),
                            cwd: view.cwd(),
                            project_root,
                            project_title,
                            project_order,
                            session_order,
                            leaf_order: 0,
                            agent: Some(view.agent_kind().id().to_string()),
                        }]
                    }
                }
            })
            .collect();

        WorkspaceMenuSnapshot::current(projects, sessions).with_source(
            self.workspace_menu_source_id.clone(),
            self.next_workspace_menu_source_revision
                .fetch_add(1, Ordering::Relaxed),
        )
    }

    /// 「+」新建会话：继承当前会话的运行时和活动终端的目录。
    pub(crate) fn new_tab(&mut self, cx: &mut Context<Self>) {
        let runtime = self
            .cur()
            .map(|s| s.runtime(cx))
            .unwrap_or_else(crate::runtimes::Runtime::local);
        let cwd = self.cur().and_then(|s| s.cwd(cx)).or_else(|| {
            // 本机才能拿桌面进程的当前目录兜底；远程运行时交给它的默认目录。
            runtime.is_local().then(current_dir).flatten()
        });
        self.add_session(runtime, cwd, cx);
    }

    /// 切换左侧栏（会话列表与工作区）开合状态。宽度由 Dock 的左栏记住。
    pub(crate) fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        if !self.sidebar_open {
            self.focus_active_stage(window, cx);
        }
        self.save_state(cx);
        cx.notify();
    }

    /// 把一个目录加进这个运行时的项目列表并设为活动项目。已经在列表里就只切活动，不重复
    /// 加。**不建会话**——项目和会话是两回事，建会话是调用方另外的事。
    pub(crate) fn add_project(
        &mut self,
        runtime: RuntimeKey,
        root: String,
        cx: &mut Context<Self>,
    ) {
        if root.is_empty() {
            return;
        }
        let root = root.trim_end_matches('/').to_string();
        let Some(space) = self.space_mut(&runtime) else {
            return;
        };
        if !space
            .projects
            .iter()
            .any(|p| p.trim_end_matches('/') == root)
        {
            space.projects.push(root.clone());
        }
        self.active_project = Some(ProjectRef { runtime, root });
        // 打开项目 = 想看这个项目，收掉盖在舞台上的覆盖页。
        self.stage_cover = None;
        self.save_state(cx);
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    fn find_project_group(&self, project: &ProjectRef, cx: &App) -> Option<ProjectGroup> {
        self.project_groups(&project.runtime, cx)
            .into_iter()
            .find(|g| g.root.trim_end_matches('/') == project.root.trim_end_matches('/'))
    }

    /// 侧栏右键「关闭项目」：底下还有会话就先弹确认——关项目会连带 kill 掉那些 shell，
    /// 正在跑的活儿就没了，且找不回来。空项目无损，直接关。
    pub(crate) fn start_close_project(&mut self, project: ProjectRef, cx: &mut Context<Self>) {
        let Some(g) = self.find_project_group(&project, cx) else {
            return;
        };
        if g.sessions.is_empty() {
            self.close_project(&project, cx);
        } else {
            self.close_project_target = Some((g.label, project, g.sessions.len()));
            cx.notify();
        }
    }

    pub(crate) fn cancel_close_project(&mut self, cx: &mut Context<Self>) {
        self.close_project_target = None;
        cx.notify();
    }

    pub(crate) fn confirm_close_project(&mut self, cx: &mut Context<Self>) {
        let Some((_, project, _)) = self.close_project_target.take() else {
            return;
        };
        self.close_project(&project, cx);
    }

    /// 关闭项目：从列表移除，并连带关掉挂在它下面的所有会话。认运行时加 **root 路径**
    /// （分组身份，见 ProjectGroup）——用显示名的话，末段同名的另一个项目会被一起误伤。
    pub(crate) fn close_project(&mut self, project: &ProjectRef, cx: &mut Context<Self>) {
        let root = project.root.trim_end_matches('/').to_string();
        let Some(g) = self.find_project_group(project, cx) else {
            return;
        };
        // 降序关闭：前面的下标不受后面 remove 影响。
        let mut ixs = g.sessions;
        ixs.sort_unstable_by(|a, b| b.cmp(a));
        for ix in ixs {
            self.close_session(ix, cx);
        }
        if let Some(space) = self.space_mut(&project.runtime) {
            space.projects.retain(|p| p.trim_end_matches('/') != root);
            space
                .pinned_projects
                .retain(|p| !crate::sidebar_order::same_project_root(p, &root));
            space.collapsed_projects.remove(&root);
        }
        if self.active_project.as_ref().is_some_and(|active| {
            active.runtime == project.runtime && active.root.trim_end_matches('/') == root
        }) {
            self.active_project = None;
        }
        // 项目抽屉（文件树等）的界面状态目前只有本机项目有。
        if project.runtime == RuntimeKey::Local {
            let project_key = crate::sidebar_order::normalize_project_root(&root);
            self.project_ui.remove(&project_key);
            if self.active_project_ui_key.as_deref() == Some(project_key.as_str()) {
                self.active_project_ui_key = None;
                let _ = self.active_session_ui.take_project_ui();
            }
        }
        self.save_state(cx);
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 点选侧栏项目行：抽屉立刻绑到这个项目。此方法只更新项目上下文，不隐式改折叠
    /// 状态；标题事件会在同一次点击里显式调用 `toggle_project_collapsed`。不切舞台会话
    /// ——点项目只换项目上下文（文件树等），会话要点会话行才切。
    pub(crate) fn select_project(
        &mut self,
        project: ProjectRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let project = ProjectRef {
            root: project.root.trim_end_matches('/').to_string(),
            ..project
        };
        let exists = self.find_project_group(&project, cx).is_some();
        self.active_project = Some(project);
        if exists {
            self.sync_project_ui(window, cx);
        }
        self.save_state(cx);
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    pub(crate) fn is_project_pinned(&self, project: &ProjectRef) -> bool {
        self.space(&project.runtime).is_some_and(|space| {
            crate::sidebar_order::is_pinned_project(&space.pinned_projects, &project.root)
        })
    }

    pub(crate) fn toggle_project_pinned(&mut self, project: ProjectRef, cx: &mut Context<Self>) {
        let Some(space) = self.space_mut(&project.runtime) else {
            return;
        };
        crate::sidebar_order::toggle_pinned_project(&mut space.pinned_projects, &project.root);
        self.save_state(cx);
        cx.notify();
    }

    pub(crate) fn toggle_project_collapsed(
        &mut self,
        project: &ProjectRef,
        cx: &mut Context<Self>,
    ) {
        let root = crate::sidebar_order::normalize_project_root(&project.root);
        if root.is_empty() {
            return;
        }
        let Some(space) = self.space_mut(&project.runtime) else {
            return;
        };
        if let Some(existing) = space
            .collapsed_projects
            .iter()
            .find(|path| crate::sidebar_order::same_project_root(path, &root))
            .cloned()
        {
            space.collapsed_projects.remove(&existing);
        } else {
            space.collapsed_projects.insert(root);
        }
        self.save_state(cx);
        cx.notify();
    }

    /// 「打开项目」：在运行时那台机器上选一个目录，加进项目列表。**不自动建会话**——
    /// 打开项目只是把它放上工作台，要开终端还是开对话由分组行的「+」决定。
    pub(crate) fn open_project(&mut self, cx: &mut Context<Self>) {
        self.open_project_on(crate::runtimes::Runtime::local(), cx);
    }

    /// 在某个运行时上打开项目（侧栏运行时标题行的「+」）。
    pub(crate) fn open_project_on(
        &mut self,
        runtime: crate::runtimes::Runtime,
        cx: &mut Context<Self>,
    ) {
        self.open_dir_picker(
            runtime,
            crate::dir_picker::DirPickerPurpose::OpenProject,
            cx,
        );
    }

    /// 从 Finder 拖入的路径 = 打开项目：文件夹直接用，文件取其父目录，各加一条项目。
    /// **不自动建会话**——跟「+ 打开项目」同一套语义，开终端还是开对话由项目行的
    /// 「+」决定（两条路结果不一致的话，"打开项目"到底会发生什么就说不清了）。
    ///
    /// 路径判定（is_dir 要 stat）仍丢后台：`on_drop` / `on_open_urls` 在 ObjC FFI 栈
    /// 上，拖一大把文件时同步 stat 会把窗口卡成 beachball。
    pub(crate) fn open_paths(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        if paths.is_empty() {
            eprintln!("[workspace] open_paths: 空路径列表，忽略");
            return;
        }
        eprintln!(
            "[workspace] open_paths: 收到 {} 条路径 {:?}",
            paths.len(),
            paths
        );
        self.stage_cover = None;
        cx.notify();

        let paths: Vec<std::path::PathBuf> = paths.to_vec();
        let (tx, rx) = smol::channel::bounded(1);
        std::thread::Builder::new()
            .name("smelt-open-paths".into())
            .spawn(move || {
                let mut out: Vec<String> = Vec::with_capacity(paths.len());
                for p in paths {
                    let dir = if p.is_dir() {
                        p
                    } else {
                        match p.parent() {
                            Some(parent) => parent.to_path_buf(),
                            None => continue,
                        }
                    };
                    let Some(cwd) = dir.to_str().map(str::to_string) else {
                        continue;
                    };
                    // 一次拖进同目录的一堆文件 → 只加一条项目。
                    if !out.contains(&cwd) {
                        out.push(cwd);
                    }
                }
                let _ = tx.send_blocking(out);
            })
            .expect("spawn smelt-open-paths 线程");

        cx.spawn(async move |this, cx| {
            let dirs = match rx.recv().await {
                Ok(v) => v,
                Err(_) => {
                    let _ = this.update(cx, |this, cx| {
                        this.report_background_error("打开路径内部通道断开");
                        cx.notify();
                    });
                    return;
                }
            };

            let _ = this.update(cx, |this, cx| {
                if dirs.is_empty() {
                    this.report_background_error("拖入的路径无法作为项目目录打开");
                } else {
                    // Finder 拖进来的是本机目录。
                    for dir in dirs {
                        this.add_project(RuntimeKey::Local, dir, cx);
                    }
                }

                cx.notify();
            });
        })
        .detach();
    }

    /// 关闭第 ix 个会话。用户主动关 → 让守护杀掉这些 shell（区别于退出 GUI：
    /// 那时不杀，会话在 smeltd 里持久活着）。
    ///
    /// 允许关到一个会话都不剩：项目独立于会话存在，侧栏还有项目行撑着不会空白，
    /// 舞台落到「还没有会话」引导页。（以前硬性拒绝关最后一个，那是项目还没实体化、
    /// 关光就整个侧栏空掉的年代留下的保护。）
    pub(crate) fn close_session(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.remove_session(ix, true, cx);
    }

    /// 按 daemon 侧 session id 找侧栏里那条 ACP 会话。
    pub(crate) fn acp_session_index(&self, sid: &str, cx: &App) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| match &session.kind {
                SessionKind::Conversation(view) => view.read(cx).session_id() == sid,
                SessionKind::Term { .. } => false,
            })
    }

    /// 目录撤回后的投影拆除：只摘本地视图，不再发一遍 kill。拆卸已经在 daemon 完成。
    pub(crate) fn unproject_remote_session(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.remove_session(ix, false, cx);
    }

    pub(crate) fn remove_session(&mut self, ix: usize, kill_runtime: bool, cx: &mut Context<Self>) {
        if ix >= self.sessions.len() {
            return;
        }
        // 兼容旧存档/旧创建路径留下的隐式项目：在移除最后一个能提供 cwd 的会话前，
        // 先把它对应的项目实体化。这样“关闭会话”和“关闭项目”始终是两件事。
        let runtime = self.sessions[ix].runtime(cx).key().clone();
        let project_root = self
            .project_groups(&runtime, cx)
            .into_iter()
            .find(|g| g.sessions.contains(&ix))
            .map(|g| g.root);
        self.remember_session_project(&runtime, project_root.as_deref());
        let attention_ids: Vec<String> = match &self.sessions[ix].kind {
            SessionKind::Term { .. } => self.sessions[ix]
                .term_leaves()
                .iter()
                .map(|t| t.read(cx).session_id().to_string())
                .collect(),
            SessionKind::Conversation(view) => vec![view.read(cx).session_id().to_string()],
        };
        if kill_runtime {
            // 每个 pane 在自己的运行时上杀：远程会话的 shell 在那台机器上。
            let terminals = self.sessions[ix]
                .term_leaves()
                .iter()
                .map(|t| {
                    let t = t.read(cx);
                    (t.runtime().clone(), t.session_id().to_string())
                })
                .collect::<Vec<_>>();
            for (runtime, id) in terminals {
                // 用户主动关闭会话时要在移除视图前完成有界 kill 往返。
                // `terminal::kill_remote` 使用 5s socket 超时，确保 App 紧接着退出时
                // 不会把尚未送达的命令留给一个 detached 任务。
                terminal::kill_remote(&runtime, &id);
            }
            if let SessionKind::Conversation(view) = &self.sessions[ix].kind {
                view.update(cx, |v, cx| v.shutdown(cx));
            }
        }
        if cx.try_global::<AttentionGlobal>().is_some() {
            for id in attention_ids {
                AttentionGlobal::remove_session(&id, cx);
            }
        }
        self.pending_history_title_persist
            .remove(&self.sessions[ix].ui_id);
        let closed_conversation_sid = match &self.sessions[ix].kind {
            SessionKind::Conversation(view) => Some(view.read(cx).session_id().to_string()),
            SessionKind::Term { .. } => None,
        };
        let closed_current_conversation =
            closed_conversation_sid.as_deref() == self.nav.agents().conversation_sid();
        let next_conversation_ix = if closed_current_conversation {
            self.sessions
                .iter()
                .enumerate()
                .rev()
                .find_map(|(other, session)| {
                    (other != ix && session.is_agent_conversation(cx)).then_some(other)
                })
        } else {
            None
        };
        self.sessions.remove(ix);
        self.session_list_revision = self.session_list_revision.wrapping_add(1);
        if let Some(next) = next_conversation_ix {
            let next = if next > ix { next - 1 } else { next };
            self.active_session_revision = self.active_session_revision.wrapping_add(1);
            self.active_session = next;
            if let Some(sid) = self.sessions.get(next).and_then(|session| {
                session
                    .active_acp()
                    .map(|view| view.read(cx).session_id().to_string())
            }) {
                self.nav.agents_mut().open_conversation(sid);
            } else {
                self.nav.agents_mut().pop_to_root();
            }
        } else {
            if closed_current_conversation {
                self.nav.agents_mut().pop_to_root();
            }
            // 空列表时 active_session 归 0（各处都是 sessions.get(ix) 取，取不到就是无会话态）。
            if self.sessions.is_empty() {
                self.active_session = 0;
            } else if self.active_session >= self.sessions.len() {
                self.active_session = self.sessions.len() - 1;
            } else if self.active_session > ix {
                self.active_session -= 1;
            }
        }
        self.save_state(cx);
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 删 worktree 前先清掉 cwd 落在 `path`（或它子目录）下的所有会话，不然会留下
    /// 指向即将被删除目录的死会话。顺带把这个目录从项目列表里摘掉——目录都要没了，
    /// 留一条指向不存在路径的项目没有意义。
    pub(crate) fn close_sessions_under(&mut self, path: &str, cx: &mut Context<Self>) {
        let path = path.trim_end_matches('/').to_string();
        if !self
            .local
            .cancelled_restore_paths
            .iter()
            .any(|existing| existing == &path)
        {
            self.local.cancelled_restore_paths.push(path.clone());
        }
        self.local.restore_pending.retain(|(_, session)| {
            !restore_path_is_cancelled(
                session_state_cwd(session).as_deref(),
                std::slice::from_ref(&path),
            )
        });
        let prefix = format!("{path}/");
        let mut ixs: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let cwd = s.cwd(cx).unwrap_or_default();
                cwd == path || cwd.starts_with(&prefix)
            })
            .map(|(ix, _)| ix)
            .collect();
        // 降序关闭：前面的下标不受后面 remove 影响。
        ixs.sort_unstable_by(|a, b| b.cmp(a));
        for ix in ixs {
            self.close_session(ix, cx);
        }
        remove_projects_under(&mut self.local.projects, &path);
        self.save_state(cx);
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 关掉第 ix 个会话里的指定 pane：会话内还有别的 pane 就只拆这一个（守护真正杀掉
    /// 这个 shell，剩下的 pane 不受影响），只剩它一个时才退化成关整个会话。
    /// 侧栏 pane 行的 × 和 Cmd+W 都走这里。
    pub(crate) fn close_session_pane(
        &mut self,
        ix: usize,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = view.entity_id();
        let Some((count, was_active)) = self
            .sessions
            .get(ix)
            .map(|s| (s.pane_count(), s.anchor_id() == target))
        else {
            return;
        };
        if count <= 1 {
            self.close_session(ix, cx);
            self.focus_active(window, cx);
            return;
        }
        // 用户主动关 pane → 守护真正杀掉该 shell（区别于退出 GUI：那时保活）。
        let closed_session_id = view.read(cx).session_id().to_string();
        terminal::kill_remote(view.read(cx).runtime(), &closed_session_id);
        if cx.try_global::<AttentionGlobal>().is_some() {
            AttentionGlobal::remove_session(&closed_session_id, cx);
        }
        let sess = &mut self.sessions[ix];
        if let Some(layout) = sess.term_layout_mut() {
            remove_leaf(layout, target);
        }
        // 关掉的正是活动 pane 才需要改指向，关别的 pane 时当前视图不该跳走。
        if was_active && let Some(first) = sess.term_leaves().first().cloned() {
            sess.set_active_term(first);
        }
        self.focus_active(window, cx);
        self.save_state(cx);
        cx.notify();
    }

    /// Cmd+W：会话内多 pane 时关掉活动 pane（切到相邻），否则关整个会话。
    pub(crate) fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.cur().and_then(|s| s.active_term().cloned()) {
            Some(view) => self.close_session_pane(self.active_session, view, window, cx),
            // ACP 会话没有分屏树，只能整个关。
            None => {
                self.close_session(self.active_session, cx);
                self.focus_active(window, cx);
            }
        }
    }

    /// 点击 pane：把它设为当前会话的活动 pane 并聚焦（不换会话）。
    pub(crate) fn activate_pane(
        &mut self,
        e: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(sess) = self.sessions.get_mut(self.active_session) {
            sess.set_active_term(e.clone());
        }
        e.update(cx, |terminal, cx| terminal.mark_read(cx));
        let h = e.read(cx).focus_handle();
        window.focus(&h, cx);
        self.save_state(cx);
        cx.notify();
    }

    /// 聚焦当前会话的活动终端（ACP 会话的聚焦走视图自身，这里跳过）。
    /// 设置/收回舞台覆盖页并处理焦点：全屏页自己没有可聚焦元素，焦点认领到根
    /// 让全局快捷键仍收得到；收回时把焦点还给活动会话（终端 pane / ACP 输入框）。
    pub(crate) fn set_stage_cover(
        &mut self,
        v: Option<StageCover>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.stage_cover != v;
        if v.is_some() {
            self.nav.set_active(WorkspaceRoute::Session);
        }
        self.stage_cover = v;
        match v {
            Some(_) => window.focus(&self.focus_handle, cx),
            None => self.focus_active_stage(window, cx),
        }
        if changed {
            // Esc / 返回条也走这里。只改内存不落盘的话，冷启动会按存档再进全屏。
            self.save_state(cx);
            self.schedule_visible_refresh(cx);
        }
        cx.notify();
    }

    /// 焦点还给活动会话：Term → 活动 pane；ACP → 消息流输入框。
    pub(crate) fn focus_active_stage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Session {
            kind: SessionKind::Conversation(view),
            ..
        }) = self.sessions.get(self.active_session)
        {
            let view = view.clone();
            view.update(cx, |v, cx| v.focus_input(window, cx));
        } else {
            self.focus_active(window, cx);
        }
    }

    pub(crate) fn focus_active(&self, window: &mut Window, cx: &mut App) {
        if let Some(active) = self.cur().and_then(|s| s.active_term()) {
            let h = active.read(cx).focus_handle();
            window.focus(&h, cx);
        }
    }

    /// 侧栏展开会话看到的分屏子行：点击某个 pane → 切到它所在会话，并把该 pane
    /// 设为会话内的活动 pane（分屏树本身不变，只是换了「当前看哪个」）。
    pub(crate) fn activate_session_pane(
        &mut self,
        ix: usize,
        pane: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(ix, window, cx);
        self.activate_pane(&pane, window, cx);
    }

    /// 切换到第 ix 个会话并聚焦。
    /// 按侧栏「分组展平后的视觉顺序」切上/下一个会话（delta=-1/1），到头循环。
    /// 快捷键 cmd-up / cmd-down；顺序跟眼睛看到的一致（跨项目一路顺下去），
    /// 不是 self.sessions 的数组序——那个跟侧栏显示序未必一致，切起来会乱跳。
    pub(crate) fn cycle_session(
        &mut self,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let order = self.sidebar_session_order(cx);
        if order.is_empty() {
            return;
        }
        let cur = order
            .iter()
            .position(|&ix| ix == self.active_session)
            .unwrap_or(0);
        let n = order.len() as isize;
        let next = (cur as isize + delta).rem_euclid(n) as usize;
        self.activate(order[next], window, cx);
    }

    pub(crate) fn viewed_session_id(&self, cx: &App) -> Option<String> {
        if self.stage_cover.is_some() {
            return None;
        }
        if !route_views_session(self.active_tab(), self.nav.agents().conversation_sid()) {
            return None;
        }
        match &self.sessions.get(self.active_session)?.kind {
            SessionKind::Term { active, .. } => Some(active.read(cx).session_id().to_string()),
            SessionKind::Conversation(view) => Some(view.read(cx).session_id().to_string()),
        }
    }

    /// 用户正在看的会话标已读。切会话、窗口回到前台、或正在看时新关注到达。
    pub(crate) fn mark_visible_session_read(&mut self, cx: &mut Context<Self>) {
        if cx.try_global::<AttentionGlobal>().is_none() {
            return;
        }
        if let Some(sid) = self.viewed_session_id(cx) {
            status_item::remove_system_notification(&sid);
            AttentionGlobal::mark_read(&sid, cx);
        }
    }

    pub(crate) fn note_window_activation(&mut self, window: &Window, cx: &mut Context<Self>) {
        let application_active = status_item::is_app_active();
        smelt_ui::motion::set_ambient_application_active(cx, application_active);
        let active = application_active && window.is_window_active();
        let became_active = active && !self.workspace_window_active;
        self.workspace_window_active = active;
        if became_active {
            status_item::refresh_system_notification_authorization();
            self.mark_visible_session_read(cx);
        }
    }

    pub(crate) fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.sessions.len() && self.sessions[ix].is_agent_conversation(cx) {
            self.open_agent_conversation(ix, window, cx);
            return;
        }
        if ix < self.sessions.len() && self.sessions[ix].is_product_conversation(cx) {
            return;
        }
        if ix < self.sessions.len() {
            self.active_session_revision = self.active_session_revision.wrapping_add(1);
            self.active_session = ix;
            // 活动项目跟着活动会话走：切到别的项目的会话时侧栏高亮同步换过去。
            if let Some(g) = self
                .all_project_groups(cx)
                .into_iter()
                .find(|g| g.sessions.contains(&ix))
            {
                self.active_project = Some(ProjectRef {
                    runtime: g.runtime,
                    root: g.root,
                });
            }
            self.sync_session_ui(window, cx);
            self.nav.set_active(WorkspaceRoute::Session);
            let route_has_overlay = self.stage_cover.is_some();
            // 切到会话即视为看过一次性通知；结构化等待状态仍由 daemon phase 保留。
            if let SessionKind::Conversation(view) = &self.sessions[ix].kind {
                view.update(cx, |v, cx| {
                    // 冷恢复占位第一次被切到 → 自动启动（免手点「重新开始」）。
                    if should_auto_resume_active_acp(self.local.sessions_restored) {
                        v.maybe_auto_resume(window, cx);
                    }
                    if !route_has_overlay {
                        v.mark_read(cx);
                        v.focus_input(window, cx);
                    }
                });
            } else {
                if !route_has_overlay && let Some(view) = self.sessions[ix].active_term().cloned() {
                    view.update(cx, |terminal, cx| terminal.mark_read(cx));
                }
                if !route_has_overlay {
                    self.focus_active(window, cx);
                }
            }
            if route_has_overlay {
                window.focus(&self.focus_handle, cx);
            }
            self.save_state(cx);
            self.schedule_visible_refresh(cx);
            cx.notify();
        }
    }

    fn next_project_session(&self, from: usize, delta: i32, cx: &App) -> Option<usize> {
        let n = self.sessions.len();
        if n == 0 {
            return None;
        }
        for step in 1..=n {
            let ix = (from as i32 + delta * step as i32).rem_euclid(n as i32) as usize;
            if !self.sessions[ix].is_product_conversation(cx) {
                return Some(ix);
            }
        }
        None
    }

    pub(crate) fn next_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.next_project_session(self.active_session, 1, cx) {
            self.activate(ix, window, cx);
        }
    }

    /// 侧栏右键「强制重启」：ACP 会话卡死（`session/cancel` 打不断正在跑的
    /// 工具调用）时的兜底，见 `AcpView::force_restart` 注释。非 ACP 会话
    /// （`active_acp` 返回 None）是 no-op，调用方（右键菜单）也只在 ACP
    /// 会话上才显示这一项。
    pub(crate) fn force_restart_acp_session(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(view) = self.sessions.get(ix).and_then(|s| s.active_acp()).cloned() {
            view.update(cx, |v, cx| v.force_restart(cx));
        }
    }

    pub(crate) fn prev_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.next_project_session(self.active_session, -1, cx) {
            self.activate(ix, window, cx);
        }
    }

    /// Cmd+[ / Cmd+] 在当前会话的分屏树里循环切换活动 pane（对齐 iTerm2 默认键位：
    /// 这两个键管「同一会话内切哪个格子」，会话本身的切换交给 Cmd+1~9）。
    /// 只有一个 pane（没分屏）时什么都不做。
    pub(crate) fn cycle_pane(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sess) = self.cur() else { return };
        let leaves = sess.term_leaves();
        if leaves.len() < 2 {
            return;
        }
        let cur_id = sess.anchor_id();
        let Some(ix) = leaves.iter().position(|l| l.entity_id() == cur_id) else {
            return;
        };
        let n = leaves.len() as i32;
        let next = (ix as i32 + delta).rem_euclid(n) as usize;
        let target = leaves[next].clone();
        self.activate_pane(&target, window, cx);
    }

    /// 侧栏右键「重命名」：弹出文本框，预填当前标题。回车 / 点「确定」提交，见
    /// `confirm_rename`；提交前的输入放在独立的 rename_input，不影响目标对象
    /// 本身，点「取消」（走 cancel_rename）就等于什么都没发生。
    ///
    /// 注意：这里故意不监听 `InputEvent::Blur` 去自动提交——点「取消」按钮本身会先
    /// 让输入框失焦，若失焦也提交，「取消」就会在关闭前先把文本框里的内容存下来，
    /// 跟按钮的字面意思相反。所以提交只认 Enter 或显式点「确定」。
    pub(crate) fn start_rename(
        &mut self,
        target: RenameTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::input::{InputEvent, InputState};
        let current = match &target {
            RenameTarget::Session(ix) => {
                let Some(s) = self.sessions.get(*ix) else {
                    return;
                };
                s.title(cx)
            }
            RenameTarget::Pane(view) => pane_title(view, cx),
            RenameTarget::History { current_title, .. } => current_title.clone(),
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        // 先记下打开前的焦点，再把焦点交给输入框。顺序反了，关掉时会把输入框当成返回点。
        self.begin_main_modal_focus(window, cx);
        input.update(cx, |s, cx| s.focus(window, cx));
        self._rename_sub = Some(cx.subscribe_in(
            &input,
            window,
            |this, _input, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.confirm_rename(window, cx);
                }
            },
        ));
        self.rename_target = Some(target);
        self.rename_input = Some(input);
        cx.notify();
    }

    /// 提交重命名：空输入等于清掉自定义名，回退到自动推导的标题。
    pub(crate) fn confirm_rename(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.rename_target.take() else {
            return;
        };
        let Some(input) = self.rename_input.take() else {
            return;
        };
        self._rename_sub = None;
        let text = input.read(cx).value().trim().to_string();
        let custom_title = (!text.is_empty()).then_some(text);
        match target {
            RenameTarget::Session(ix) => {
                let mut history_identity = None;
                let mut pending_ui_id = None;
                let terminal_identity = self.sessions.get(ix).and_then(|s| match &s.kind {
                    SessionKind::Term { active, .. } => {
                        let (agent, conversation_id) = pane_conversation_identity(active, cx)?;
                        Some((agent, s.cwd(cx).unwrap_or_default(), conversation_id))
                    }
                    SessionKind::Conversation(_) => None,
                });
                if let Some(s) = self.sessions.get_mut(ix) {
                    // 终端会话认得出当前对话时，名字属于那段对话而不是这个标签：
                    // 用户中途退出、换到别的对话，这一行就该跟着换名字，所以
                    // 这里不能再留下会话级 pin 把新对话的名字盖住。
                    s.custom_title = if terminal_identity.is_some() {
                        None
                    } else {
                        custom_title.clone()
                    };
                    if let SessionKind::Conversation(view) = &s.kind {
                        let view = view.read(cx);
                        if let Some(resume_id) = view.history_session_id_for_save() {
                            history_identity = Some((
                                settings::HistorySourceKind::from(view.agent_kind()),
                                view.profile_id().map(String::from),
                                view.cwd().unwrap_or_default(),
                                resume_id.to_string(),
                            ));
                        } else {
                            pending_ui_id = Some(s.ui_id);
                        }
                    }
                }
                // 智能体对话的名字属于这段会话本身，改名要同步给 agent，
                // 让 agent 侧的会话档也叫这个名字；不支持改名的 agent 静默忽略。
                if let Some(SessionKind::Conversation(view)) =
                    self.sessions.get(ix).map(|session| &session.kind)
                {
                    let view = view.clone();
                    let title = custom_title.clone();
                    view.update(cx, |view, _cx| view.rename_session(title));
                }
                let runtime = self.sessions.get(ix).map(|session| session.runtime(cx));
                if let Some((agent, cwd, conversation_id)) = terminal_identity
                    && let Some(runtime) = runtime.as_ref()
                {
                    self.set_history_custom_title(
                        runtime,
                        agent,
                        None,
                        cwd,
                        conversation_id,
                        custom_title.clone(),
                        cx,
                    );
                } else if let Some((agent, profile_id, cwd, resume_id)) = history_identity
                    && let Some(runtime) = runtime.as_ref()
                {
                    if let Some(session) = self.sessions.get(ix) {
                        self.pending_history_title_persist.remove(&session.ui_id);
                    }
                    self.set_history_custom_title(
                        runtime,
                        agent,
                        profile_id,
                        cwd,
                        resume_id,
                        custom_title.clone(),
                        cx,
                    );
                } else if let Some(ui_id) = pending_ui_id {
                    if custom_title.is_some() {
                        self.pending_history_title_persist.insert(ui_id);
                    } else {
                        self.pending_history_title_persist.remove(&ui_id);
                    }
                }
            }
            RenameTarget::Pane(view) => {
                // 与会话行同理：pane 里跑着一段可识别的对话时，改名写给那段对话。
                match pane_conversation_identity(&view, cx) {
                    Some((agent, conversation_id)) => {
                        let cwd = view.read(cx).cwd().unwrap_or_default();
                        let runtime = view.read(cx).runtime().clone();
                        view.update(cx, |t, _| t.set_custom_title(None));
                        self.set_history_custom_title(
                            &runtime,
                            agent,
                            None,
                            cwd,
                            conversation_id,
                            custom_title.clone(),
                            cx,
                        );
                    }
                    None => view.update(cx, |t, _| t.set_custom_title(custom_title)),
                }
            }
            RenameTarget::History {
                agent,
                profile_id,
                cwd,
                resume_id,
                ..
            } => {
                // 历史页目前只读本机运行时的存档。
                self.set_history_custom_title(
                    &crate::runtimes::Runtime::local(),
                    agent,
                    profile_id,
                    cwd,
                    resume_id,
                    custom_title,
                    cx,
                );
            }
        }
        self.save_state(cx);
        cx.notify();
    }

    /// 把用户命名覆盖层读进 `HistoryTitles` 缓存。
    ///
    /// 改名走写穿，本地改完立即可见；这里只负责兜住不经过本窗口的写入（历史页、
    /// 移动端、另一扇窗口），因此 5 秒一次足够，且必须在后台线程读存储——侧栏
    /// 渲染路径上不允许出现同步存储访问。内容变了由 `HistoryTitles` 的观察者重绘
    /// 工作台，这里不再单独 notify。
    pub(crate) fn refresh_history_titles(&mut self, cx: &mut Context<Self>) {
        let fresh = self
            .history_titles_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(5));
        if fresh || self.history_titles_inflight {
            return;
        }
        self.history_titles_inflight = true;
        cx.spawn(async move |this, cx| {
            let titles = cx
                .background_executor()
                .spawn(async move {
                    use smelt_runtime_client::protocol::control::EmptyParams;
                    use smelt_runtime_client::protocol::history::HistoryTitlesCall;
                    crate::runtime_call::call::<HistoryTitlesCall>(
                        EmptyParams::default(),
                        crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                    )
                    .map(|result| {
                        result
                            .titles
                            .into_iter()
                            .map(|t| ((t.agent, t.profile_id, t.resume_id), t.title))
                            .collect::<std::collections::HashMap<_, _>>()
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.history_titles_inflight = false;
                this.history_titles_at = Some(Instant::now());
                // 取不到时保留上一份，5 秒后再试；空表会把侧栏标题全打回原始值。
                match titles {
                    Ok(titles) => crate::HistoryTitles::replace_all(titles, cx),
                    Err(error) => eprintln!(
                        "[history] 读取会话标题失败：{}",
                        crate::runtime_call::error_text(error)
                    ),
                }
            });
        })
        .detach();
    }

    /// `runtime`：这段对话的存档在哪台机器上。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn set_history_custom_title(
        &mut self,
        runtime: &crate::runtimes::Runtime,
        agent: settings::HistorySourceKind,
        profile_id: Option<String>,
        cwd: String,
        resume_id: String,
        custom_title: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = smelt_client::session_ops::rename_history_title(
            runtime.client(),
            agent,
            profile_id.as_deref(),
            &resume_id,
            custom_title.as_deref(),
            Some(cwd.as_str()).filter(|value| !value.is_empty()),
        ) {
            eprintln!("[session-metadata] 保存会话名称失败: {error}");
            return;
        }
        crate::HistoryTitles::set(
            agent.id(),
            profile_id.as_deref(),
            &resume_id,
            custom_title.as_deref(),
            cx,
        );

        let key = session_history::session_list_key(agent, profile_id.as_deref(), &cwd);
        if let Some((_, list)) = self.session_list.get_mut(&key) {
            let mut updated = list.sessions.as_ref().clone();
            for session in &mut updated {
                if session.resume_id == resume_id {
                    session.custom_title = custom_title.clone();
                    session.title = custom_title
                        .clone()
                        .unwrap_or_else(|| session.agent_title.clone());
                }
            }
            *list = session_history::HistoryListSnapshot::new(updated);
        }

        for session in &mut self.sessions {
            let SessionKind::Conversation(view) = &session.kind else {
                continue;
            };
            let view = view.read(cx);
            if settings::HistorySourceKind::from(view.agent_kind()) == agent
                && view.profile_id() == profile_id.as_deref()
                && view
                    .history_session_id_for_save()
                    .as_ref()
                    .map(ToString::to_string)
                    == Some(resume_id.clone())
            {
                session.custom_title = custom_title.clone();
            }
        }
        self.save_state(cx);
        cx.notify();
    }

    pub(crate) fn persist_pending_history_title_for_view(
        &mut self,
        changed_view: &Entity<acp_view::AcpView>,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.anchor_id() == changed_view.entity_id())
        else {
            return;
        };
        if !self.pending_history_title_persist.contains(&session.ui_id) {
            return;
        }
        let ui_id = session.ui_id;
        let custom_title = session.custom_title.clone();
        let view = changed_view.read(cx);
        let Some(resume_id) = view.history_session_id_for_save() else {
            return;
        };
        let identity = (
            settings::HistorySourceKind::from(view.agent_kind()),
            view.profile_id().map(String::from),
            view.cwd().unwrap_or_default(),
            resume_id.to_string(),
        );
        let runtime = view.runtime().clone();
        self.pending_history_title_persist.remove(&ui_id);
        self.set_history_custom_title(
            &runtime,
            identity.0,
            identity.1,
            identity.2,
            identity.3,
            custom_title,
            cx,
        );
    }

    pub(crate) fn persist_history_agent_definition_for_view(
        &self,
        changed_view: &Entity<acp_view::AcpView>,
        cx: &Context<Self>,
    ) {
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.anchor_id() == changed_view.entity_id())
        else {
            return;
        };
        let Some(agent_definition_id) = session.agent_definition_id.as_deref() else {
            return;
        };
        let view = changed_view.read(cx);
        let Some(resume_id) = view.history_session_id_for_save() else {
            return;
        };
        crate::session_history::remember_history_agent(
            view.agent_kind().into(),
            view.profile_id(),
            &resume_id.to_string(),
            agent_definition_id,
            cx,
        );
    }

    /// 取消重命名：不落地任何改动。
    pub(crate) fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.rename_target = None;
        self.rename_input = None;
        self._rename_sub = None;
        cx.notify();
    }
}
