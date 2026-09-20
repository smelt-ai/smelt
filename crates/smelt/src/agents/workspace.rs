//! 智能体 / 自动化的 Workspace 数据修改：创建、编辑、运行、触发器。
//!
//! 页面渲染在 `view.rs` / `automation_view.rs`。字段仍由 main.rs 的 Workspace 持有。
use super::*;

impl Workspace {
    pub(crate) fn activate_workspace_tab(
        &mut self,
        tab: WorkspaceRoute,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if tab.releases_plugin_key() {
            smelt_webview::release_focus(window);
        }
        self.nav.set_active(tab.clone());
        if tab.sidebar_opens_root() {
            self.pop_active_tab_to_root();
        }
        if !self.active_tab().is_session() {
            self.stage_cover = None;
        }
        window.focus(&self.focus_handle, cx);
        self.save_state(cx);
        cx.notify();
    }

    pub(super) fn reveal_workspace_tab(&mut self, tab: WorkspaceRoute) {
        self.nav.set_active(tab);
        if !self.active_tab().is_session() {
            self.stage_cover = None;
        }
    }

    pub(super) fn pop_active_tab_to_root(&mut self) {
        self.nav.pop_active_to_root();
        if self.active_tab() == &WorkspaceRoute::Automations {
            self.automation_surface.live_view = None;
            self.automation_surface.live_sub = None;
            self.automation_surface.editor = None;
            self.automation_surface.return_to_catalog = false;
        }
        if self.active_tab().is_session() {
            self.stage_cover = None;
        }
    }

    pub(super) fn open_agent_product_route(
        &mut self,
        route: WorkspaceRoute,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        debug_assert!(matches!(
            route,
            WorkspaceRoute::Agents | WorkspaceRoute::Automations
        ));
        if route == WorkspaceRoute::Agents {
            settings::reload_agent_definitions(cx);
        }
        self.activate_workspace_tab(route, window, cx);
    }

    pub(crate) fn open_agents_route(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_agent_product_route(WorkspaceRoute::Agents, window, cx);
    }

    pub(super) fn toggle_agent_conversations_collapsed(
        &mut self,
        agent_id: &str,
        cx: &mut Context<Self>,
    ) {
        if agent_id.is_empty() {
            return;
        }
        if !self.collapsed_agents.remove(agent_id) {
            self.collapsed_agents.insert(agent_id.to_string());
        }
        self.save_state(cx);
        cx.notify();
    }

    pub(super) fn create_agent(
        &mut self,
        kind: settings::ConversationAgentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = uuid::Uuid::new_v4().to_string();
        let existing = cx.global::<settings::AgentHostState>().agents.len();
        let name = if existing == 0 {
            format!("{} 智能体", kind.short_label())
        } else {
            format!("{} 智能体 {}", kind.short_label(), existing + 1)
        };
        let selected = id.clone();
        settings::apply_agent_host(
            move |config| {
                config.agents.push(settings::AgentDefinition {
                    id,
                    name,
                    engine_kind_id: kind.id().to_string(),
                    ..Default::default()
                });
            },
            cx,
        );
        self.open_agent_definition(selected, window, cx);
    }

    pub(super) fn open_agent_definition(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.agent_surface.selected_id = Some(id.clone());
        self.nav.agents_mut().open_editor(id);
        self.agent_surface.editor = None;
        self.agent_surface.error = None;
        window.focus(&self.focus_handle, cx);
        self.save_state(cx);
        cx.notify();
    }

    pub(super) fn finish_agent_name_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.agent_surface.editor.as_mut() {
            editor.name_editing = false;
        }
        cx.notify();
    }

    /// 勾选 / 取消勾选一个 Pi 插件。写回定义后，新开的对话只会加载勾选项。
    pub(super) fn toggle_agent_plugin(
        &mut self,
        agent_id: String,
        plugin_id: String,
        cx: &mut Context<Self>,
    ) {
        settings::apply_agent_host(
            move |config| {
                let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == agent_id)
                else {
                    return;
                };
                if let Some(pos) = agent.plugins.iter().position(|id| *id == plugin_id) {
                    agent.plugins.remove(pos);
                } else {
                    agent.plugins.push(plugin_id.clone());
                }
            },
            cx,
        );
        cx.notify();
    }

    /// 挑一个或多个目录绑定到智能体。Pi 只认单个 cwd，所以这些目录不会变成
    /// 工作目录，而是以绝对路径写进 system prompt 让它自己去读。
    pub(super) fn pick_agent_context_folders(&mut self, agent_id: String, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("选择要绑定的目录".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let picked: Vec<String> = paths
                .into_iter()
                .filter_map(|path| path.to_str().map(String::from))
                .collect();
            if picked.is_empty() {
                return;
            }
            this.update(cx, |this, cx| {
                this.add_agent_context_folders(agent_id, picked, cx)
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn add_agent_context_folders(
        &mut self,
        agent_id: String,
        folders: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        settings::apply_agent_host(
            move |config| {
                let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == agent_id)
                else {
                    return;
                };
                for folder in &folders {
                    let folder = folder.trim_end_matches('/');
                    if folder.is_empty() || agent.context_folders.iter().any(|it| it == folder) {
                        continue;
                    }
                    agent.context_folders.push(folder.to_string());
                }
            },
            cx,
        );
        cx.notify();
    }

    pub(super) fn remove_agent_context_folder(
        &mut self,
        agent_id: String,
        folder: String,
        cx: &mut Context<Self>,
    ) {
        settings::apply_agent_host(
            move |config| {
                if let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == agent_id) {
                    agent.context_folders.retain(|it| *it != folder);
                }
            },
            cx,
        );
        cx.notify();
    }

    pub(super) fn add_agent_context_link(
        &mut self,
        agent_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let link = self
            .agent_surface
            .editor
            .as_ref()
            .map(|editor| editor.context_link.read(cx).value().trim().to_string())
            .unwrap_or_default();
        if link.is_empty() {
            return;
        }
        settings::apply_agent_host(
            move |config| {
                let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == agent_id)
                else {
                    return;
                };
                if !agent.context_links.contains(&link) {
                    agent.context_links.push(link.clone());
                }
            },
            cx,
        );
        if let Some(editor) = self.agent_surface.editor.as_ref() {
            let input = editor.context_link.clone();
            input.update(cx, |state, cx| state.set_value("", window, cx));
        }
        cx.notify();
    }

    pub(super) fn remove_agent_context_link(
        &mut self,
        agent_id: String,
        link: String,
        cx: &mut Context<Self>,
    ) {
        settings::apply_agent_host(
            move |config| {
                if let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == agent_id) {
                    agent.context_links.retain(|it| *it != link);
                }
            },
            cx,
        );
        cx.notify();
    }

    /// 重新扫描插件目录。用户在设置里装完插件后不用重开编辑器。
    pub(super) fn refresh_agent_plugins(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.agent_surface.editor.as_mut() {
            editor.plugins = discover_plugins();
        }
        cx.notify();
    }

    pub(super) fn finish_automation_name_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.automation_surface.editor.as_mut() {
            editor.name_editing = false;
        }
        cx.notify();
    }

    pub(super) fn close_agent_definition(&mut self, cx: &mut Context<Self>) {
        self.nav.agents_mut().pop_to_root();
        self.agent_surface.editor = None;
        self.agent_surface.error = None;
        self.save_state(cx);
        cx.notify();
    }

    pub(super) fn ensure_agent_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.nav.agents().editor_id().map(str::to_string) else {
            self.agent_surface.editor = None;
            return;
        };
        if self
            .agent_surface
            .editor
            .as_ref()
            .is_some_and(|editor| editor.id == id)
        {
            return;
        }
        let Some(agent) = cx
            .global::<settings::AgentHostState>()
            .agents
            .iter()
            .find(|agent| agent.id == id)
            .cloned()
        else {
            self.agent_surface.editor = None;
            return;
        };

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("给它起个名字")
                .default_value(agent.name)
        });
        let instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(8, 18)
                .placeholder("它负责什么、怎么做事、什么时候该停下来问你。")
                .default_value(agent.prompt)
        });

        let context_link =
            cx.new(|cx| InputState::new(window, cx).placeholder("粘贴一个参考链接，回车添加"));

        let mut subscriptions = Vec::new();
        let context_link_id = id.clone();
        subscriptions.push(cx.subscribe_in(
            &context_link,
            window,
            move |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.add_agent_context_link(context_link_id.clone(), window, cx);
                }
            },
        ));
        let name_id = id.clone();
        subscriptions.push(cx.subscribe_in(
            &name,
            window,
            move |this, input, event: &InputEvent, window, cx| {
                if save_on_input_event(event) {
                    let value = input.read(cx).value().to_string();
                    let id = name_id.clone();
                    settings::apply_agent_host(
                        move |config| {
                            if let Some(agent) =
                                config.agents.iter_mut().find(|agent| agent.id == id)
                            {
                                agent.name = value;
                            }
                        },
                        cx,
                    );
                    cx.notify();
                }
                if matches!(event, InputEvent::PressEnter { .. }) {
                    window.focus(&this.focus_handle, cx);
                }
                if identity_name_should_finish_edit(event) {
                    this.finish_agent_name_edit(cx);
                }
            },
        ));

        let instructions_id = id.clone();
        subscriptions.push(
            cx.subscribe(&instructions, move |_, input, event: &InputEvent, cx| {
                if !save_on_input_event(event) {
                    return;
                }
                let value = input.read(cx).value().to_string();
                let id = instructions_id.clone();
                settings::apply_agent_host(
                    move |config| {
                        if let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == id) {
                            agent.prompt = value;
                        }
                    },
                    cx,
                );
                cx.notify();
            }),
        );

        self.agent_surface.editor = Some(AgentEditor {
            id,
            name,
            name_editing: false,
            instructions,
            context_link,
            plugins: discover_plugins(),
            _subscriptions: subscriptions,
        });
        window.focus(&self.focus_handle, cx);
    }

    /// 智能体本身不“启动”。这个动作总是新建一段引用该定义的对话，并留在工作台
    /// 对话区；同一智能体可以并存任意多段上下文，且不进入项目会话列表。
    pub(super) fn start_agent_conversation(
        &mut self,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let config = cx.global::<settings::AgentHostState>().clone();
        let Some(agent) = config.agents.iter().find(|agent| agent.id == id).cloned() else {
            self.agent_surface.error = Some("智能体已经不存在".to_string());
            cx.notify();
            return;
        };
        if let Some(reason) = agent_conversation_start_error(&agent) {
            self.agent_surface.error = Some(reason.to_string());
            cx.notify();
            return;
        }
        let (kind, launch) = match config.resolve_agent(&agent) {
            Ok(resolved) => resolved,
            Err(error) => {
                self.agent_surface.error = Some(error);
                cx.notify();
                return;
            }
        };
        if !agent_engine_kinds().contains(&kind) {
            self.agent_surface.error = Some(format!(
                "{} 暂未开放为产品级智能体引擎，请改用 Pi",
                kind.short_label()
            ));
            cx.notify();
            return;
        }
        self.agent_surface.error = None;
        let Some(fallback_cwd) = ensure_agent_conversation_cwd(&agent.id) else {
            self.agent_surface.error = Some("无法创建智能体工作区".to_string());
            cx.notify();
            return;
        };
        self.add_acp_session(
            NewAcpSessionRequest {
                agent: kind,
                launch_override: Some(launch),
                profile_id: None,
                agent_definition: Some(agent),
                fallback_cwd: Some(fallback_cwd),
                pending_prompt: None,
                automation_id: None,
                activate: true,
            },
            window,
            cx,
        );
    }

    /// 侧栏最顶「聊天」：用 Pi 裸引擎新开一段工作台对话。
    /// 进「对话」栏，不进项目列表，也不绑某个产品智能体定义。
    pub(super) fn start_workbench_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(fallback_cwd) = ensure_workbench_conversation_cwd() else {
            self.agent_surface.error = Some("无法创建对话工作区".to_string());
            cx.notify();
            return;
        };
        self.agent_surface.error = None;
        self.add_acp_session(
            NewAcpSessionRequest {
                agent: settings::ConversationAgentKind::Pi,
                launch_override: None,
                profile_id: None,
                agent_definition: None,
                fallback_cwd: Some(fallback_cwd),
                pending_prompt: None,
                automation_id: None,
                activate: true,
            },
            window,
            cx,
        );
    }

    pub(crate) fn open_agent_conversation(
        &mut self,
        session_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(session) = self.sessions.get(session_ix) else {
            return;
        };
        if !session.is_agent_conversation(cx) {
            return;
        }
        let agent_id = session.agent_definition_id.clone();
        let view = session.active_acp().cloned();
        let Some(sid) = view
            .as_ref()
            .map(|view| view.read(cx).session_id().to_string())
        else {
            return;
        };
        if self.active_session != session_ix {
            self.active_session_revision = self.active_session_revision.wrapping_add(1);
            self.active_session = session_ix;
        }
        if let Some(agent_id) = agent_id {
            self.agent_surface.selected_id = Some(agent_id);
        }
        // 右侧文件树/变更/历史/技能都读 active_project_root；对话的托管工作目录
        // 就是它这一刻的上下文，不写进去工具面板会停在上一个项目上。
        if let Some(cwd) = self
            .sessions
            .get(session_ix)
            .and_then(|session| session.cwd(cx))
            .filter(|cwd| !cwd.is_empty())
        {
            self.active_project = Some(cwd.trim_end_matches('/').to_string());
        }
        self.nav.agents_mut().open_conversation(sid);
        self.agent_surface.editor = None;
        self.agent_surface.error = None;
        self.reveal_workspace_tab(WorkspaceRoute::Agents);
        self.sync_session_ui(window, cx);
        if let Some(view) = view {
            view.update(cx, |view, cx| {
                view.maybe_auto_resume(window, cx);
                view.mark_read(cx);
                view.focus_input(window, cx);
            });
        }
        self.save_state(cx);
        cx.notify();
    }

    pub(crate) fn selected_agent_conversation_view(
        &self,
        cx: &App,
    ) -> Option<(usize, Entity<acp_view::AcpView>)> {
        let sid = self.nav.agents().conversation_sid()?;
        self.sessions.iter().enumerate().find_map(|(ix, session)| {
            if !session.is_agent_conversation(cx) {
                return None;
            }
            let view = session.active_acp()?;
            (view.read(cx).session_id() == sid).then(|| (ix, view.clone()))
        })
    }

    pub(super) fn set_agent_engine(
        &mut self,
        id: String,
        kind: settings::ConversationAgentKind,
        cx: &mut Context<Self>,
    ) {
        if cx
            .global::<settings::AgentHostState>()
            .agents
            .iter()
            .find(|agent| agent.id == id)
            .and_then(|agent| agent.engine_kind())
            == Some(kind)
        {
            return;
        }
        settings::apply_agent_host(
            move |config| {
                if let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == id) {
                    agent.engine_kind_id = kind.id().to_string();
                }
            },
            cx,
        );
        self.agent_surface.error = None;
        cx.notify();
    }

    pub(super) fn set_agent_model(
        &mut self,
        id: String,
        model_provider: String,
        model_id: String,
        cx: &mut Context<Self>,
    ) {
        let model_provider = model_provider.trim().to_string();
        let model_id = model_id.trim().to_string();
        if cx
            .global::<settings::AgentHostState>()
            .agents
            .iter()
            .find(|agent| agent.id == id)
            .is_some_and(|agent| {
                agent.model_provider == model_provider && agent.model_id == model_id
            })
        {
            return;
        }
        settings::apply_agent_host(
            move |config| {
                if let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == id) {
                    agent.model_provider = model_provider;
                    agent.model_id = model_id;
                }
            },
            cx,
        );
        self.agent_surface.error = None;
        cx.notify();
    }

    pub(super) fn delete_agent(&mut self, id: String, cx: &mut Context<Self>) {
        self.agent_surface.error = None;
        let config = cx.global::<settings::AgentHostState>();
        if let Some(error) = &config.automation_store_error {
            self.agent_surface.error =
                Some(format!("自动化存储不可用，暂时不能删除智能体：{error}"));
            cx.notify();
            return;
        }
        if config.automation_store_id.is_empty() {
            self.agent_surface.error = Some("正在读取关联自动化，请稍后再试".to_string());
            cx.notify();
            return;
        }
        let automation_count = config.automations_for(&id).len();
        if automation_count != 0 {
            self.agent_surface.error = Some(format!(
                "请先删除该智能体关联的 {automation_count} 条自动化"
            ));
            cx.notify();
            return;
        }

        let deleted_id = id.clone();
        if !settings::try_apply_agent_host(
            move |config| config.agents.retain(|agent| agent.id != deleted_id),
            cx,
        ) {
            self.agent_surface.error = cx
                .global::<settings::AgentHostState>()
                .persistence_error
                .clone();
            cx.notify();
            return;
        }
        self.collapsed_agents.remove(&id);
        if self
            .automation_surface
            .editor
            .as_ref()
            .is_some_and(|editor| editor.agent_id == id)
        {
            self.automation_surface.editor = None;
        }
        self.nav.agents_mut().pop_to_root();
        self.agent_surface.selected_id = None;
        self.agent_surface.editor = None;
        self.save_state(cx);
        cx.notify();
    }

    pub(super) fn automation_snapshot_is_current_store(
        &self,
        snapshot: &smelt_core::automation::AutomationFile,
        cx: &App,
    ) -> bool {
        let config = cx.global::<settings::AgentHostState>();
        !config.is_retired_automation_store(&snapshot.store_id)
            && (config.automation_store_id.is_empty()
                || config.automation_store_id == snapshot.store_id)
    }

    pub(super) fn submit_automation_command(
        &mut self,
        command: smelt_core::automation::AutomationCommand,
        cx: &mut Context<Self>,
    ) {
        self.automation_surface.error = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(
                    async move { smelt_core::session_control::submit_automation_command(&command) },
                )
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => {
                    // 自动化权威状态只由 EventHub 投影写入，命令回包不再写同一份数据。
                    cx.notify();
                }
                Err(error) => {
                    this.automation_surface.error = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn run_automation_once(&mut self, automation_id: String, cx: &mut Context<Self>) {
        self.submit_automation_command(
            smelt_core::automation::AutomationCommand::RunOnce { automation_id },
            cx,
        );
    }

    pub(super) fn cancel_automation_run(&mut self, run_id: String, cx: &mut Context<Self>) {
        self.submit_automation_command(
            smelt_core::automation::AutomationCommand::CancelRun { run_id },
            cx,
        );
    }

    pub(crate) fn open_automation_run_session(
        &mut self,
        session_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let run = cx
            .global::<settings::AgentHostState>()
            .automation_runs
            .iter()
            .find(|run| run.session_id.as_deref() == Some(session_id))
            .cloned();
        let Some(run) = run else {
            self.automation_surface.error = Some("找不到对应的运行记录".to_string());
            cx.notify();
            return;
        };
        self.nav
            .automations_mut()
            .open_live(run.automation_id.clone(), run.id.clone());
        self.reveal_workspace_tab(WorkspaceRoute::Automations);
        if self
            .automation_surface
            .live_view
            .as_ref()
            .is_some_and(|view| view.read(cx).session_id() == session_id)
        {
            self.automation_surface.error = None;
            cx.notify();
            return;
        }
        let Some(record) = self.remote_acp_record(session_id, cx) else {
            self.automation_surface.live_view = None;
            self.automation_surface.live_sub = None;
            self.automation_surface.error = None;
            cx.notify();
            return;
        };
        let Some(agent) = settings::ConversationAgentKind::from_id(&record.agent) else {
            self.automation_surface.live_view = None;
            self.automation_surface.live_sub = None;
            self.automation_surface.error = Some("无法识别该运行使用的智能体引擎".to_string());
            cx.notify();
            return;
        };
        let view = cx.new(|cx| {
            acp_view::AcpView::placeholder(
                cx,
                acp_view::AcpViewOrigin {
                    agent,
                    launch: record.launch,
                    refresh_launch_from_settings: false,
                    profile_id: record
                        .agent_option_id
                        .strip_prefix("profile:")
                        .map(String::from),
                    cwd: Some(record.cwd),
                    reason: "正在打开自动化运行现场…".to_string(),
                    entries: Vec::new(),
                    resume_session_id: record
                        .resume_id
                        .map(agent_client_protocol::schema::v1::SessionId::new),
                    saved_sid: Some(record.id),
                },
            )
        });
        view.update(cx, |view, cx| {
            view.restore_conversation_binding(
                smelt_core::conversation::ConversationBinding::Automation { run_id: run.id },
            );
            view.maybe_auto_resume(window, cx);
        });
        self.automation_surface.live_sub = Some(self.subscribe_acp_persist(&view, window, cx));
        self.automation_surface.live_view = Some(view);
        self.automation_surface.error = None;
        cx.notify();
    }

    pub(crate) fn open_automation_run_detail(&mut self, run_id: String, cx: &mut Context<Self>) {
        self.automation_surface.return_to_catalog = false;
        let Some(automation_id) = cx
            .global::<settings::AgentHostState>()
            .automation_runs
            .iter()
            .find(|run| run.id == run_id)
            .map(|run| run.automation_id.clone())
        else {
            return;
        };
        self.navigate_to_automation_run(automation_id, run_id, cx);
    }

    pub(super) fn open_catalog_automation_run(&mut self, run_id: String, cx: &mut Context<Self>) {
        let Some(automation_id) = cx
            .global::<settings::AgentHostState>()
            .automation_runs
            .iter()
            .find(|run| run.id == run_id)
            .map(|run| run.automation_id.clone())
        else {
            return;
        };
        self.automation_surface.return_to_catalog = true;
        self.navigate_to_automation_run(automation_id, run_id, cx);
    }

    pub(crate) fn navigate_to_automation_run(
        &mut self,
        automation_id: String,
        run_id: String,
        cx: &mut Context<Self>,
    ) {
        self.reveal_workspace_tab(WorkspaceRoute::Automations);
        self.nav.automations_mut().open_run(automation_id, run_id);
        cx.notify();
    }

    pub(super) fn close_automation_run_detail(&mut self, cx: &mut Context<Self>) {
        if self.automation_surface.return_to_catalog {
            self.nav.automations_mut().pop_to_root();
            self.automation_surface.return_to_catalog = false;
        } else {
            self.nav.automations_mut().back();
        }
        self.automation_surface.live_view = None;
        self.automation_surface.live_sub = None;
        cx.notify();
    }

    pub(super) fn open_automation_run_history(
        &mut self,
        automation_id: String,
        cx: &mut Context<Self>,
    ) {
        self.automation_surface.return_to_catalog = false;
        self.nav.automations_mut().open_history(automation_id);
        self.automation_surface.live_view = None;
        self.automation_surface.live_sub = None;
        cx.notify();
    }

    pub(super) fn open_catalog_automation_history(
        &mut self,
        automation_id: String,
        cx: &mut Context<Self>,
    ) {
        self.automation_surface.return_to_catalog = true;
        self.nav.automations_mut().open_history(automation_id);
        self.automation_surface.live_view = None;
        self.automation_surface.live_sub = None;
        cx.notify();
    }

    pub(super) fn close_automation_run_history(&mut self, cx: &mut Context<Self>) {
        if self.automation_surface.return_to_catalog {
            self.nav.automations_mut().pop_to_root();
            self.automation_surface.return_to_catalog = false;
        } else {
            self.nav.automations_mut().back();
        }
        self.automation_surface.live_view = None;
        self.automation_surface.live_sub = None;
        cx.notify();
    }

    pub(super) fn close_automation_editor(&mut self, cx: &mut Context<Self>) {
        self.automation_surface.editor = None;
        self.nav.automations_mut().pop_to_root();
        cx.notify();
    }

    fn ready_automation_agent_id(&self, cx: &App) -> Option<String> {
        let ready_ids = cx
            .global::<settings::AgentHostState>()
            .agents
            .iter()
            .filter(|agent| agent.is_ready())
            .map(|agent| agent.id.clone())
            .collect::<Vec<_>>();
        self.agent_surface
            .selected_id
            .as_ref()
            .filter(|id| ready_ids.iter().any(|ready| ready == *id))
            .cloned()
            .or_else(|| ready_ids.into_iter().next())
    }

    pub(super) fn set_automation_catalog_tab(
        &mut self,
        tab: AutomationCatalogTab,
        cx: &mut Context<Self>,
    ) {
        self.automation_surface.catalog_tab = tab;
        cx.notify();
    }

    pub(super) fn set_automation_template_filter(
        &mut self,
        filter: Option<super::automation_templates::AutomationTemplateCategory>,
        cx: &mut Context<Self>,
    ) {
        self.automation_surface.template_filter = filter;
        cx.notify();
    }

    pub(super) fn add_automation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.automation_surface.catalog_tab = AutomationCatalogTab::Tasks;
        let action = if let Some(agent_id) = self.ready_automation_agent_id(cx) {
            settings::AutomationAction::Agent {
                agent_definition_id: agent_id,
                prompt: None,
            }
        } else {
            settings::AutomationAction::shell(String::new(), Vec::new())
        };
        let automation = settings::Automation {
            id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            enabled: true,
            workspace_dir: None,
            trigger: settings::AutomationTrigger::default(),
            action,
            sinks: vec![settings::AutomationSink::Local],
        };
        self.open_automation_editor_for(automation, true, window, cx);
    }

    pub(super) fn add_automation_from_template(
        &mut self,
        template_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(template) = super::automation_templates::automation_template(template_id) else {
            return;
        };
        self.automation_surface.catalog_tab = AutomationCatalogTab::Tasks;
        let agent_id = self.ready_automation_agent_id(cx);
        let automation = settings::Automation {
            id: uuid::Uuid::new_v4().to_string(),
            name: template.name.to_string(),
            enabled: true,
            workspace_dir: None,
            trigger: settings::AutomationTrigger::schedule(template.schedule),
            action: settings::AutomationAction::Agent {
                agent_definition_id: agent_id.clone().unwrap_or_default(),
                prompt: Some(template.prompt.to_string()),
            },
            sinks: vec![settings::AutomationSink::Local],
        };
        if agent_id.is_some() {
            self.submit_automation_command(
                smelt_core::automation::AutomationCommand::Upsert {
                    automation: Box::new(automation),
                },
                cx,
            );
        } else {
            self.open_automation_editor_for(automation, true, window, cx);
        }
    }

    pub(super) fn delete_automation(&mut self, automation_id: String, cx: &mut Context<Self>) {
        if self
            .automation_surface
            .editor
            .as_ref()
            .is_some_and(|editor| editor.automation_id == automation_id)
        {
            self.automation_surface.editor = None;
        }
        if self.nav.automations().automation_id() == Some(automation_id.as_str()) {
            self.nav.automations_mut().pop_to_root();
            self.automation_surface.live_view = None;
            self.automation_surface.live_sub = None;
        }
        self.submit_automation_command(
            smelt_core::automation::AutomationCommand::Delete { automation_id },
            cx,
        );
    }

    pub(super) fn set_automation_enabled(
        &mut self,
        automation_id: String,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        self.submit_automation_command(
            smelt_core::automation::AutomationCommand::SetEnabled {
                automation_id,
                enabled,
            },
            cx,
        );
    }

    pub(super) fn open_automation_editor(
        &mut self,
        automation_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .automation_surface
            .editor
            .as_ref()
            .is_some_and(|editor| editor.automation_id == automation_id)
        {
            return;
        }
        let Some(automation) = cx
            .global::<settings::AgentHostState>()
            .automations
            .iter()
            .find(|automation| automation.id == automation_id)
            .cloned()
        else {
            self.automation_surface.editor = None;
            return;
        };
        self.open_automation_editor_for(automation, false, window, cx);
    }

    pub(super) fn open_automation_editor_for(
        &mut self,
        automation: settings::Automation,
        is_new: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.nav
            .automations_mut()
            .open_editor(automation.id.clone());
        let action_kind = settings::AutomationActionKindId::from_action(&automation.action);
        let agent_id = automation
            .agent_definition_id()
            .unwrap_or_default()
            .to_string();
        let command_default = match &automation.action {
            settings::AutomationAction::Shell { command, args } if args.is_empty() => {
                command.clone()
            }
            settings::AutomationAction::Shell { command, args } => {
                std::iter::once(command.as_str())
                    .chain(args.iter().map(String::as_str))
                    .collect::<Vec<_>>()
                    .join(" ")
            }
            settings::AutomationAction::Agent { .. } => String::new(),
        };
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("我的自动化任务")
                .default_value(automation.name.clone())
        });
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("例如：汇总今日进展并列出待办事项")
                .default_value(automation.prompt().unwrap_or_default().to_string())
        });
        let command = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder("例如：curl -fsS https://example.com/health")
                .default_value(command_default)
        });
        let retained_events = automation.trigger.events.clone();
        let mut entries = automation
            .trigger
            .schedules
            .iter()
            .map(|schedule| {
                new_trigger_entry(
                    window,
                    cx,
                    if schedule.is_interval() {
                        settings::AutomationTriggerKindId::Interval
                    } else {
                        settings::AutomationTriggerKindId::Calendar
                    },
                    Some(schedule),
                    None,
                )
            })
            .collect::<Vec<_>>();
        for webhook in &automation.trigger.webhooks {
            let secret = webhook_ingress_secret(webhook);
            entries.push(new_trigger_entry(
                window,
                cx,
                settings::AutomationTriggerKindId::Webhook,
                None,
                Some(secret),
            ));
        }
        let notification = settings::AutomationNotificationPreset::from_sinks(&automation.sinks);
        let feishu_chat_default = automation.feishu_chat_id().unwrap_or("").to_string();
        let feishu_chat = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("飞书会话 ID，例如 oc_xxx")
                .default_value(feishu_chat_default)
        });
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(&name, window, {
            move |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.clear_automation_editor_error(cx);
                }
                if matches!(event, InputEvent::PressEnter { .. }) {
                    window.focus(&this.focus_handle, cx);
                }
                if identity_name_should_finish_edit(event) {
                    this.finish_automation_name_edit(cx);
                }
            }
        }));
        for entry in &entries {
            subscribe_trigger_entry_inputs(&mut subscriptions, entry, cx);
        }
        subscriptions.push(cx.subscribe(&prompt, {
            move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.clear_automation_editor_error(cx);
                }
            }
        }));
        subscriptions.push(cx.subscribe(&command, {
            move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.clear_automation_editor_error(cx);
                }
            }
        }));
        subscriptions.push(cx.subscribe(&feishu_chat, {
            move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.clear_automation_editor_error(cx);
                }
            }
        }));
        self.automation_surface.editor = Some(AutomationEditor {
            instance_id: uuid::Uuid::new_v4(),
            action_kind,
            agent_id,
            automation_id: automation.id,
            is_new,
            name,
            name_editing: is_new && automation.name.trim().is_empty(),
            prompt,
            command,
            retained_events,
            entries,
            notification,
            feishu_chat,
            draft_revision: 0,
            saving_revision: None,
            quiet_save: false,
            error: None,
            _subscriptions: subscriptions,
        });
        let name_input = self
            .automation_surface
            .editor
            .as_ref()
            .map(|editor| (editor.name_editing, editor.name.clone()));
        match name_input {
            Some((true, name)) => name.update(cx, |state, cx| state.focus(window, cx)),
            _ => window.focus(&self.focus_handle, cx),
        }
        cx.notify();
    }

    pub(super) fn clear_automation_editor_error(&mut self, cx: &mut Context<Self>) {
        self.touch_schedule_editor(cx);
    }

    pub(super) fn set_trigger_schedule_kind(
        &mut self,
        index: usize,
        kind: ScheduleKind,
        cx: &mut Context<Self>,
    ) {
        if let Some(rule) = self
            .automation_surface
            .editor
            .as_mut()
            .and_then(|editor| editor.entries.get_mut(index))
            .map(|entry| &mut entry.schedule)
        {
            rule.kind = kind;
            if kind == ScheduleKind::Weekdays {
                rule.weekday_mask = smelt_core::automation::SCHEDULE_WEEKDAYS_MASK;
                rule.interval_day_scope = IntervalDayScope::Weekdays;
            } else if kind == ScheduleKind::Weekly {
                if rule.weekday_mask == 0 {
                    rule.weekday_mask = smelt_core::automation::SCHEDULE_WEEKDAYS_MASK;
                }
                rule.interval_day_scope = IntervalDayScope::Weekly;
            } else if kind == ScheduleKind::Daily {
                rule.weekday_mask = smelt_core::automation::SCHEDULE_ALL_DAYS_MASK;
                rule.interval_day_scope = IntervalDayScope::Everyday;
            } else {
                rule.interval_day_scope = IntervalDayScope::from_days(rule.weekday_mask);
            }
        }
        self.touch_schedule_editor(cx);
    }

    pub(super) fn set_interval_unit(
        &mut self,
        index: usize,
        unit: IntervalUnit,
        cx: &mut Context<Self>,
    ) {
        if let Some(rule) = self
            .automation_surface
            .editor
            .as_mut()
            .and_then(|editor| editor.entries.get_mut(index))
            .map(|entry| &mut entry.schedule)
        {
            rule.interval_unit = unit;
        }
        self.touch_schedule_editor(cx);
    }

    pub(super) fn set_interval_day_scope(
        &mut self,
        index: usize,
        scope: IntervalDayScope,
        cx: &mut Context<Self>,
    ) {
        if let Some(rule) = self
            .automation_surface
            .editor
            .as_mut()
            .and_then(|editor| editor.entries.get_mut(index))
            .map(|entry| &mut entry.schedule)
        {
            rule.interval_day_scope = scope;
            match scope {
                IntervalDayScope::Everyday => {
                    rule.weekday_mask = smelt_core::automation::SCHEDULE_ALL_DAYS_MASK;
                }
                IntervalDayScope::Weekdays => {
                    rule.weekday_mask = smelt_core::automation::SCHEDULE_WEEKDAYS_MASK;
                }
                IntervalDayScope::Weekly => {
                    if rule.weekday_mask == 0 {
                        rule.weekday_mask = smelt_core::automation::SCHEDULE_WEEKDAYS_MASK;
                    }
                }
            }
        }
        self.touch_schedule_editor(cx);
    }

    pub(super) fn set_interval_window_open(
        &mut self,
        index: usize,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(rule) = self
            .automation_surface
            .editor
            .as_mut()
            .and_then(|editor| editor.entries.get_mut(index))
            .map(|entry| &mut entry.schedule)
        {
            rule.window_open = open;
            if !open {
                rule.window_start
                    .update(cx, |state, cx| state.set_value("", window, cx));
                rule.window_end
                    .update(cx, |state, cx| state.set_value("", window, cx));
            }
        }
        self.touch_schedule_editor(cx);
    }

    pub(super) fn toggle_schedule_weekday(
        &mut self,
        index: usize,
        day_bit: u8,
        cx: &mut Context<Self>,
    ) {
        if let Some(rule) = self
            .automation_surface
            .editor
            .as_mut()
            .and_then(|editor| editor.entries.get_mut(index))
            .map(|entry| &mut entry.schedule)
        {
            rule.weekday_mask ^= day_bit;
        }
        self.touch_schedule_editor(cx);
    }

    pub(super) fn add_trigger_preset(
        &mut self,
        preset: settings::AutomationTriggerPresetId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let schedule = preset.schedule();
        let entry = new_trigger_entry(window, cx, preset.trigger_kind(), schedule.as_ref(), None);
        let Some(editor) = self.automation_surface.editor.as_mut() else {
            return;
        };
        subscribe_trigger_entry_inputs(&mut editor._subscriptions, &entry, cx);
        editor.entries.push(entry);
        self.touch_schedule_editor(cx);
    }

    pub(super) fn remove_trigger_entry(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(editor) = self.automation_surface.editor.as_mut()
            && index < editor.entries.len()
        {
            editor.entries.remove(index);
        }
        self.touch_schedule_editor(cx);
    }

    pub(super) fn set_automation_notification(
        &mut self,
        preset: settings::AutomationNotificationPreset,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.automation_surface.editor.as_mut() {
            editor.notification = preset;
        }
        self.touch_schedule_editor(cx);
    }

    pub(super) fn touch_schedule_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.automation_surface.editor.as_mut() else {
            return;
        };
        editor.draft_revision = editor.draft_revision.wrapping_add(1);
        editor.error = None;
        let token = editor.draft_revision;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(400))
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(editor) = this.automation_surface.editor.as_ref() else {
                    return;
                };
                if editor.draft_revision != token {
                    return;
                }
                this.persist_automation_editor(None, cx);
            });
        })
        .detach();
    }

    pub(super) fn regenerate_automation_webhook_secret(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let should_save = self
            .automation_surface
            .editor
            .as_ref()
            .is_some_and(|editor| !editor.is_new);
        if let Some(entry) = self
            .automation_surface
            .editor
            .as_mut()
            .and_then(|editor| editor.entries.get_mut(index))
        {
            entry.webhook_secret = smelt_core::automation::new_webhook_secret();
        }
        if should_save {
            if let Some(editor) = self.automation_surface.editor.as_mut() {
                editor.draft_revision = editor.draft_revision.wrapping_add(1);
                editor.error = None;
            }
            self.persist_automation_editor(
                Some((format!("automation-regen-webhook-{index}"), "已重新生成 ✓")),
                cx,
            );
        } else {
            self.touch_schedule_editor(cx);
        }
    }

    pub(super) fn set_automation_action_kind(
        &mut self,
        kind: settings::AutomationActionKindId,
        cx: &mut Context<Self>,
    ) {
        let fallback_agent = if kind == settings::AutomationActionKindId::Agent {
            let ready_ids = cx
                .global::<settings::AgentHostState>()
                .agents
                .iter()
                .filter(|agent| agent.is_ready())
                .map(|agent| agent.id.clone())
                .collect::<Vec<_>>();
            self.agent_surface
                .selected_id
                .as_ref()
                .filter(|id| ready_ids.iter().any(|ready| ready == *id))
                .cloned()
                .or_else(|| ready_ids.into_iter().next())
        } else {
            None
        };
        if let Some(editor) = self.automation_surface.editor.as_mut() {
            editor.action_kind = kind;
            if editor.agent_id.is_empty()
                && let Some(agent_id) = fallback_agent
            {
                editor.agent_id = agent_id;
            }
        }
        self.touch_schedule_editor(cx);
    }

    pub(super) fn set_automation_agent(&mut self, agent_id: String, cx: &mut Context<Self>) {
        if let Some(editor) = self.automation_surface.editor.as_mut() {
            editor.agent_id = agent_id;
        }
        self.touch_schedule_editor(cx);
    }

    pub(super) fn test_automation_editor(&mut self, cx: &mut Context<Self>) {
        let Some(automation_id) = self.automation_surface.editor.as_ref().and_then(|editor| {
            (!editor.is_new && editor.saving_revision.is_none())
                .then(|| editor.automation_id.clone())
        }) else {
            return;
        };
        self.run_automation_once(automation_id, cx);
    }

    pub(super) fn persist_automation_editor(
        &mut self,
        flash: Option<(String, &'static str)>,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.automation_surface.editor.as_ref() else {
            return;
        };
        if editor.saving_revision.is_some() {
            return;
        }
        let name = editor.name.read(cx).value().trim().to_string();
        let prompt_raw = editor.prompt.read(cx).value().trim().to_string();
        let prompt = if prompt_raw.is_empty() {
            None
        } else {
            Some(prompt_raw)
        };
        let command = editor.command.read(cx).value().trim().to_string();
        let trigger = trigger_from_editor(editor, cx);
        let agent_exists = cx
            .global::<settings::AgentHostState>()
            .agents
            .iter()
            .any(|agent| agent.id == editor.agent_id);
        let incomplete = name.is_empty()
            || (editor.action_kind == settings::AutomationActionKindId::Agent
                && (editor.agent_id.is_empty() || !agent_exists))
            || (editor.action_kind == settings::AutomationActionKindId::Shell
                && command.is_empty())
            || trigger.is_err();
        if incomplete {
            return;
        }

        let automation_id = editor.automation_id.clone();
        let enabled = cx
            .global::<settings::AgentHostState>()
            .automations
            .iter()
            .find(|automation| automation.id == automation_id)
            .map(|automation| automation.enabled)
            .unwrap_or(true);
        let action = match editor.action_kind {
            settings::AutomationActionKindId::Agent => settings::AutomationAction::Agent {
                agent_definition_id: editor.agent_id.clone(),
                prompt,
            },
            settings::AutomationActionKindId::Shell => {
                settings::AutomationAction::shell(command, Vec::new())
            }
        };
        let sinks = editor
            .notification
            .to_sinks(&editor.feishu_chat.read(cx).value());
        let automation = settings::Automation {
            id: automation_id.clone(),
            name,
            enabled,
            workspace_dir: None,
            trigger: trigger.expect("validated trigger"),
            action,
            sinks,
        };
        let editor_instance_id = editor.instance_id;
        let draft_revision = editor.draft_revision;
        if let Some(editor) = self.automation_surface.editor.as_mut() {
            editor.saving_revision = Some(draft_revision);
            editor.quiet_save = flash.is_none();
        }
        self.automation_surface.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let command = smelt_core::automation::AutomationCommand::Upsert {
                automation: Box::new(automation),
            };
            let result = cx
                .background_executor()
                .spawn(
                    async move { smelt_core::session_control::submit_automation_command(&command) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                let retry = match result {
                    Ok((_result, snapshot)) => {
                        let current_store =
                            this.automation_snapshot_is_current_store(&snapshot, cx);
                        if let Some(editor) =
                            this.automation_surface.editor.as_mut().filter(|editor| {
                                editor.automation_id == automation_id
                                    && editor.instance_id == editor_instance_id
                            })
                        {
                            editor.saving_revision = None;
                            if !current_store {
                                editor.error =
                                    Some("自动化服务已切换，请在当前版本重新保存".to_string());
                                false
                            } else {
                                editor.is_new = false;
                                if let Some((flash_id, flash_label)) = flash {
                                    settings::flash_button(flash_id, flash_label, cx);
                                }
                                editor.draft_revision != draft_revision
                            }
                        } else {
                            false
                        }
                    }
                    Err(error) => {
                        if let Some(editor) =
                            this.automation_surface.editor.as_mut().filter(|editor| {
                                editor.automation_id == automation_id
                                    && editor.instance_id == editor_instance_id
                            })
                        {
                            editor.saving_revision = None;
                            editor.error = Some(error);
                        }
                        false
                    }
                };
                if retry {
                    this.persist_automation_editor(None, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn agent_conversation_count(&self, agent_id: &str, cx: &App) -> usize {
        self.sessions
            .iter()
            .filter(|session| {
                session.is_agent_conversation(cx)
                    && session.agent_definition_id.as_deref() == Some(agent_id)
            })
            .count()
    }
}
