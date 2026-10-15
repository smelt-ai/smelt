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
        if tab != WorkspaceRoute::Agents {
            self.abandon_new_conversation_create(cx);
        } else if self.agent_surface.conversation_draft_id.is_some() {
            // 侧栏「智能体」回根页。草稿还没提交，离开编辑器就等于取消。
            self.cancel_conversation_draft(cx);
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
            self.automation_surface.selected_run_id = None;
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
            WorkspaceRoute::Agents | WorkspaceRoute::Automations | WorkspaceRoute::Capabilities
        ));
        if route == WorkspaceRoute::Agents {
            settings::reload_agent_definitions(cx);
        }
        self.activate_workspace_tab(route, window, cx);
    }

    pub(crate) fn open_agents_route(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_agent_product_route(WorkspaceRoute::Agents, window, cx);
    }

    /// 直接打开能力库的某个页签，比如智能体编辑器里「插件 · 管理」。
    pub(super) fn open_capabilities(
        &mut self,
        tab: CapabilitiesTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.nav.set_capabilities_tab(tab);
        self.open_agent_product_route(WorkspaceRoute::Capabilities, window, cx);
    }

    pub(super) fn select_capabilities_tab(&mut self, tab: CapabilitiesTab, cx: &mut Context<Self>) {
        if self.nav.capabilities_tab() != tab {
            self.nav.set_capabilities_tab(tab);
            cx.notify();
        }
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
        let saved = settings::try_apply_agent_host(
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
        if self.agent_surface.arm_conversation_on_create && saved {
            self.agent_surface.arm_conversation_on_create = false;
            self.agent_surface.conversation_draft_id = Some(selected.clone());
        }
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

    /// 勾选 / 取消勾选一台 MCP 服务器。新开的对话只连勾选的服务器，没勾就不连。
    pub(super) fn toggle_agent_mcp_server(
        &mut self,
        agent_id: String,
        server: String,
        cx: &mut Context<Self>,
    ) {
        settings::apply_agent_host(
            move |config| {
                let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == agent_id)
                else {
                    return;
                };
                if let Some(pos) = agent.mcp_servers.iter().position(|name| *name == server) {
                    agent.mcp_servers.remove(pos);
                } else {
                    agent.mcp_servers.push(server.clone());
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
            editor.mcp_servers = load_mcp_servers_for_editor();
        }
        cx.notify();
    }

    /// 重新扫描记忆目录。智能体在对话里写了新记忆后，不用重开编辑器就能看到。
    pub(super) fn refresh_agent_memories(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.agent_surface.editor.as_mut() {
            editor.memories = AgentMemories::scan(&editor.id);
        }
        cx.notify();
    }

    pub(super) fn select_agent_editor_tab(&mut self, tab: AgentEditorTab, cx: &mut Context<Self>) {
        if let Some(editor) = self.agent_surface.editor.as_mut() {
            editor.tab = tab;
        }
        cx.notify();
    }

    pub(super) fn set_agent_memory_filter(&mut self, filter: MemoryFilter, cx: &mut Context<Self>) {
        if let Some(editor) = self.agent_surface.editor.as_mut() {
            editor.memory_filter = filter;
        }
        cx.notify();
    }

    /// 一次只展开一条；再点同一条收起。
    pub(super) fn toggle_agent_memory_expanded(
        &mut self,
        layer: MemoryLayer,
        file_name: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.agent_surface.editor.as_mut() {
            let target = (layer, file_name);
            editor.expanded_memory = if editor.expanded_memory.as_ref() == Some(&target) {
                None
            } else {
                Some(target)
            };
        }
        cx.notify();
    }

    /// 使用次数和待确认状态存在主库，是智能体在一轮里写完文件之后才记的，目录监听
    /// 看不到。有对话停下来时这些已经落库，编辑器开着就重扫一次。
    pub(crate) fn refresh_agent_memories_after_turns(
        &mut self,
        states: &[smelt_core::daemon_state::DaemonSessionState],
        cx: &mut Context<Self>,
    ) {
        if self.agent_surface.editor.is_some()
            && states
                .iter()
                .any(|state| session_phase_settled(state.effective_phase()))
        {
            self.refresh_agent_memories(cx);
        }
    }

    /// 监听两层记忆目录。回调在 notify 的线程上，只发唤醒信号；一次写入会连着来好几个
    /// 事件，稍等一下合并成一次重扫。返回的 watcher 一 drop，发送端跟着释放，任务就结束。
    fn watch_agent_memories(
        &mut self,
        agent_id: &str,
        cx: &mut Context<Self>,
    ) -> Option<notify::RecommendedWatcher> {
        use notify::Watcher as _;

        let (wake_tx, wake_rx) = smol::channel::bounded::<()>(1);
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if res.is_ok() {
                let _ = wake_tx.try_send(());
            }
        })
        .ok()?;
        let mut watching = false;
        for layer in [MemoryLayer::Agent, MemoryLayer::User] {
            // 目录不存在就监听不了；先建好，智能体第一次写记忆时卡片也能刷新。
            let Some(dir) = layer.dir(agent_id) else {
                continue;
            };
            if std::fs::create_dir_all(&dir).is_ok()
                && watcher
                    .watch(&dir, notify::RecursiveMode::NonRecursive)
                    .is_ok()
            {
                watching = true;
            }
        }
        if !watching {
            return None;
        }
        cx.spawn(async move |this, cx| {
            while wake_rx.recv().await.is_ok() {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(200))
                    .await;
                while wake_rx.try_recv().is_ok() {}
                if this
                    .update(cx, |this, cx| this.refresh_agent_memories(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Some(watcher)
    }

    pub(super) fn open_agent_memory_file(
        &mut self,
        path: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        self.agent_surface.error = crate::ide::open_in_system_text_editor(&path).err();
        cx.notify();
    }

    pub(super) fn open_agent_memory_folder(
        &mut self,
        agent_id: String,
        layer: MemoryLayer,
        cx: &mut Context<Self>,
    ) {
        self.agent_surface.error = match layer.dir(&agent_id) {
            Some(dir) => std::fs::create_dir_all(&dir)
                .map_err(|error| format!("无法创建记忆目录：{error}"))
                .and_then(|()| crate::ide::open_in_file_manager(&dir))
                .err(),
            None => Some("找不到记忆目录".to_string()),
        };
        cx.notify();
    }

    /// 点删除只是弹确认；记忆删了就没了，全局记忆还会从所有智能体里消失。
    pub(super) fn request_delete_agent_memory(
        &mut self,
        target: MemoryDeleteTarget,
        cx: &mut Context<Self>,
    ) {
        self.agent_surface.pending_memory_delete = Some(target);
        cx.notify();
    }

    pub(crate) fn cancel_delete_agent_memory(&mut self, cx: &mut Context<Self>) {
        self.agent_surface.pending_memory_delete = None;
        cx.notify();
    }

    pub(crate) fn confirm_delete_agent_memory(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.agent_surface.pending_memory_delete.take() else {
            return;
        };
        let MemoryDeleteTarget {
            agent_id,
            layer,
            file_name,
            ..
        } = target;
        self.agent_surface.error = match layer.dir(&agent_id) {
            Some(dir) => {
                smelt_core::agent_memory::delete_memory(&dir, &layer.scope(&agent_id), &file_name)
            }
            .map_err(|error| format!("删除记忆失败：{error}"))
            .err(),
            None => Some("找不到记忆目录".to_string()),
        };
        self.refresh_agent_memories(cx);
    }

    pub(super) fn approve_agent_memory(
        &mut self,
        agent_id: String,
        layer: MemoryLayer,
        file_name: String,
        cx: &mut Context<Self>,
    ) {
        self.agent_surface.error =
            smelt_core::agent_memory::approve_memory(&layer.scope(&agent_id), &file_name)
                .map_err(|error| format!("采纳记忆失败：{error}"))
                .err();
        self.refresh_agent_memories(cx);
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

    pub(super) fn close_agent_settings(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_conversation_draft() {
            self.cancel_conversation_draft(cx);
            return;
        }
        self.close_agent_definition(cx);
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

        let memory_query =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索名称、描述或正文"));
        subscriptions.push(cx.subscribe(&memory_query, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        }));

        let memories = AgentMemories::scan(&id);
        let memory_watcher = self.watch_agent_memories(&id, cx);
        self.agent_surface.editor = Some(AgentEditor {
            id,
            name,
            name_editing: false,
            instructions,
            context_link,
            plugins: discover_plugins(),
            mcp_servers: load_mcp_servers_for_editor(),
            memories,
            tab: AgentEditorTab::default(),
            memory_query,
            memory_filter: MemoryFilter::default(),
            expanded_memory: None,
            _memory_watcher: memory_watcher,
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

    /// 侧栏「新对话」。有默认就直接开；没有可对话智能体就去创建；有人但没默认时
    /// 选择器自己处理，这里不猜。
    pub(super) fn begin_new_conversation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stored = stored_default_conversation_agent();
        let draft = self.agent_surface.conversation_draft_id.clone();
        let (intent, clear_stored, agents_empty) = {
            let agents = &cx.global::<settings::AgentHostState>().agents;
            let resolution = smelt_core::default_conversation::resolve_new_conversation(
                agents,
                stored.as_deref(),
                draft.as_deref(),
            );
            (
                resolution.intent,
                resolution.clear_stored_default,
                agents.is_empty(),
            )
        };
        if clear_stored {
            clear_default_conversation_agent();
        }
        match intent {
            smelt_core::default_conversation::NewConversationIntent::Start { agent_id } => {
                self.discard_uncommitted_draft(Some(&agent_id), cx);
                self.start_agent_conversation(agent_id, window, cx);
            }
            smelt_core::default_conversation::NewConversationIntent::PickAgent => {}
            smelt_core::default_conversation::NewConversationIntent::CreateAgent => {
                if self.agent_surface.conversation_draft_id.is_some() {
                    self.reveal_workspace_tab(WorkspaceRoute::Agents);
                    window.focus(&self.focus_handle, cx);
                    self.save_state(cx);
                    cx.notify();
                    return;
                }
                if agents_empty {
                    self.agent_surface.arm_conversation_on_create = true;
                }
                self.open_agents_route(window, cx);
            }
        }
    }

    /// 选择器里点了一个智能体。`start` 为真时同时开对话；更换默认时只改记录。
    ///
    /// 智能体页上的「新对话」不走这里，避免点一次专用智能体就把侧栏默认抢走。
    pub(super) fn choose_default_conversation_agent(
        &mut self,
        id: String,
        start: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ready = cx
            .global::<settings::AgentHostState>()
            .agents
            .iter()
            .any(|agent| agent.id == id && agent.is_conversation_ready());
        if !ready {
            if stored_default_conversation_agent().as_deref() == Some(id.as_str()) {
                clear_default_conversation_agent();
            }
            self.agent_surface.error = Some("这个智能体不能用来开对话".to_string());
            cx.notify();
            return;
        }
        self.discard_uncommitted_draft(Some(&id), cx);
        set_default_conversation_agent(&id);
        if start {
            self.start_agent_conversation(id, window, cx);
        } else {
            cx.notify();
        }
    }

    pub(super) fn commit_conversation_draft(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.agent_surface.conversation_draft_id.clone() else {
            return;
        };
        let ready = cx
            .global::<settings::AgentHostState>()
            .agents
            .iter()
            .any(|agent| agent.id == id && agent.is_conversation_ready());
        if !ready {
            self.agent_surface.error = Some("这个智能体还不能开始对话".to_string());
            cx.notify();
            return;
        }
        self.agent_surface.conversation_draft_id = None;
        self.agent_surface.arm_conversation_on_create = false;
        set_default_conversation_agent(&id);
        self.start_agent_conversation(id, window, cx);
    }

    pub(super) fn editing_conversation_draft(&self) -> bool {
        self.agent_surface
            .conversation_draft_id
            .as_deref()
            .is_some_and(|id| self.nav.agents().editor_id() == Some(id))
    }

    fn abandon_new_conversation_create(&mut self, cx: &mut Context<Self>) {
        self.agent_surface.arm_conversation_on_create = false;
        if self.agent_surface.conversation_draft_id.is_some() {
            self.cancel_conversation_draft(cx);
        }
    }

    /// `keep` 是这次要留下的智能体。草稿就是它时只清标记，不删除。
    fn discard_uncommitted_draft(&mut self, keep: Option<&str>, cx: &mut Context<Self>) {
        let Some(draft_id) = self.agent_surface.conversation_draft_id.clone() else {
            return;
        };
        if keep == Some(draft_id.as_str()) {
            self.agent_surface.conversation_draft_id = None;
            self.agent_surface.arm_conversation_on_create = false;
            return;
        }
        self.cancel_conversation_draft(cx);
    }

    fn cancel_conversation_draft(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.agent_surface.conversation_draft_id.take() else {
            return;
        };
        self.agent_surface.arm_conversation_on_create = false;
        let removed_id = id.clone();
        if !settings::try_apply_agent_host(
            move |config| config.agents.retain(|agent| agent.id != removed_id),
            cx,
        ) {
            self.agent_surface.conversation_draft_id = None;
            self.agent_surface.error = cx
                .global::<settings::AgentHostState>()
                .persistence_error
                .clone()
                .or_else(|| Some("取消新建失败，这个智能体还在".to_string()));
            cx.notify();
            return;
        }
        if stored_default_conversation_agent().as_deref() == Some(id.as_str()) {
            clear_default_conversation_agent();
        }
        self.nav.agents_mut().pop_to_root();
        self.agent_surface.selected_id = None;
        self.agent_surface.editor = None;
        self.agent_surface.error = None;
        self.save_state(cx);
        cx.notify();
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

    /// 开关智能体的长期记忆。关掉后新对话不注入、不写入；正在进行的对话下一轮生效。
    /// 已有记忆文件保留，重新打开后照常使用。
    pub(super) fn set_agent_memory_enabled(
        &mut self,
        id: String,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        settings::apply_agent_host(
            move |config| {
                if let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == id) {
                    agent.memory_enabled = enabled;
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

    pub(super) fn start_delete_agent(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(name) = cx
            .global::<settings::AgentHostState>()
            .agents
            .iter()
            .find(|agent| agent.id == id)
            .map(|agent| agent_display_name(&agent.name))
        else {
            return;
        };
        let conversations = self.agent_conversation_count(&id);
        self.agent_surface.pending_delete = Some(AgentDeleteTarget {
            id,
            name,
            conversations,
        });
        cx.notify();
    }

    pub(crate) fn cancel_delete_agent(&mut self, cx: &mut Context<Self>) {
        self.agent_surface.pending_delete = None;
        cx.notify();
    }

    pub(crate) fn confirm_delete_agent(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.agent_surface.pending_delete.take() {
            self.delete_agent(target.id, cx);
        }
        cx.notify();
    }

    fn delete_agent(&mut self, id: String, cx: &mut Context<Self>) {
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
        if self.agent_surface.conversation_draft_id.as_deref() == Some(id.as_str()) {
            self.agent_surface.conversation_draft_id = None;
            self.agent_surface.arm_conversation_on_create = false;
        }
        if stored_default_conversation_agent().as_deref() == Some(id.as_str()) {
            clear_default_conversation_agent();
        }
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

    fn attach_automation_run_session(
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

    /// 停在编辑器里，并选中这一次运行。有会话时右侧详情就是现场。
    pub(super) fn select_automation_run(
        &mut self,
        run_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = cx
            .global::<settings::AgentHostState>()
            .automation_runs
            .iter()
            .find(|run| run.id == run_id)
            .cloned()
        else {
            return;
        };
        self.automation_surface.selected_run_id = Some(run.id);
        if let Some(session_id) = run.session_id {
            self.attach_automation_run_session(&session_id, window, cx);
        } else {
            self.automation_surface.live_view = None;
            self.automation_surface.live_sub = None;
            self.automation_surface.error = None;
            cx.notify();
        }
    }

    /// 回到任务表单。运行列表还在，只是右侧不再是某一次运行。
    pub(super) fn show_automation_task(&mut self, cx: &mut Context<Self>) {
        self.automation_surface.selected_run_id = None;
        self.automation_surface.live_view = None;
        self.automation_surface.live_sub = None;
        cx.notify();
    }

    pub(super) fn open_catalog_automation_run(
        &mut self,
        run_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(automation_id) = cx
            .global::<settings::AgentHostState>()
            .automation_runs
            .iter()
            .find(|run| run.id == run_id)
            .map(|run| run.automation_id.clone())
        else {
            return;
        };
        self.navigate_to_automation_run(automation_id, run_id, window, cx);
    }

    pub(crate) fn navigate_to_automation_run(
        &mut self,
        automation_id: String,
        run_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reveal_workspace_tab(WorkspaceRoute::Automations);
        self.nav.automations_mut().open_editor(automation_id);
        self.select_automation_run(run_id, window, cx);
    }

    pub(super) fn close_automation_editor(&mut self, cx: &mut Context<Self>) {
        self.automation_surface.editor = None;
        self.automation_surface.selected_run_id = None;
        self.automation_surface.live_view = None;
        self.automation_surface.live_sub = None;
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
        let agent_id = self.ready_automation_agent_id(cx).unwrap_or_default();
        self.add_automation_for_agent(agent_id, window, cx);
    }

    /// 为指定智能体新建自动化。绑定的就是这一个，不改用当前选中或列表里的第一个。
    pub(super) fn add_automation_for_agent(
        &mut self,
        agent_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.automation_surface.catalog_tab = AutomationCatalogTab::Tasks;
        let action = if agent_id.trim().is_empty() {
            settings::AutomationAction::shell(String::new(), Vec::new())
        } else {
            settings::AutomationAction::Agent {
                agent_definition_id: agent_id,
                prompt: None,
            }
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
        let automation = settings::Automation {
            id: uuid::Uuid::new_v4().to_string(),
            name: template.name.to_string(),
            enabled: true,
            workspace_dir: None,
            trigger: settings::AutomationTrigger::schedule(template.schedule),
            action: settings::AutomationAction::Agent {
                agent_definition_id: self.ready_automation_agent_id(cx).unwrap_or_default(),
                prompt: Some(template.prompt.to_string()),
            },
            sinks: vec![settings::AutomationSink::Local],
        };
        self.open_automation_editor_for(automation, true, window, cx);
    }

    pub(super) fn create_automation_from_editor(&mut self, cx: &mut Context<Self>) {
        if self
            .automation_surface
            .editor
            .as_ref()
            .is_some_and(|editor| editor.is_new)
        {
            self.persist_automation_editor(None, cx);
        }
    }

    pub(super) fn start_delete_automation(
        &mut self,
        automation_id: String,
        cx: &mut Context<Self>,
    ) {
        let state = cx.global::<settings::AgentHostState>();
        let Some(automation) = state
            .automations
            .iter()
            .find(|automation| automation.id == automation_id)
        else {
            return;
        };
        let name = if automation.name.trim().is_empty() {
            "未命名自动化".to_string()
        } else {
            automation.name.clone()
        };
        let runs = state.automation_runs_for(&automation_id).len();
        self.automation_surface.pending_delete = Some(AutomationDeleteTarget {
            id: automation_id,
            name,
            runs,
        });
        cx.notify();
    }

    pub(crate) fn cancel_delete_automation(&mut self, cx: &mut Context<Self>) {
        self.automation_surface.pending_delete = None;
        cx.notify();
    }

    pub(crate) fn confirm_delete_automation(&mut self, cx: &mut Context<Self>) {
        if let Some(target) = self.automation_surface.pending_delete.take() {
            self.delete_automation(target.id, cx);
        }
        cx.notify();
    }

    fn delete_automation(&mut self, automation_id: String, cx: &mut Context<Self>) {
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
            self.automation_surface.selected_run_id = None;
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
        let automation_id = automation.id.clone();
        self.nav
            .automations_mut()
            .open_editor(automation_id.clone());
        let keep_run = self
            .automation_surface
            .selected_run_id
            .as_ref()
            .is_some_and(|run_id| {
                cx.global::<settings::AgentHostState>()
                    .automation_runs
                    .iter()
                    .any(|run| run.id == *run_id && run.automation_id == automation_id)
            });
        if !keep_run {
            self.automation_surface.selected_run_id = None;
            self.automation_surface.live_view = None;
            self.automation_surface.live_sub = None;
        }
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
                .placeholder("给它起个名字")
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

    /// 已创建的自动化改动后自动保存；新建的只记修改，等用户点「创建」。
    pub(super) fn touch_schedule_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.automation_surface.editor.as_mut() else {
            return;
        };
        editor.draft_revision = editor.draft_revision.wrapping_add(1);
        editor.error = None;
        let token = editor.draft_revision;
        let autosave = !editor.is_new;
        cx.notify();
        if !autosave {
            return;
        }
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
        if editor.saving_revision.is_some() || !editor.missing_fields(cx).is_empty() {
            return;
        }
        let Ok(trigger) = trigger_from_editor(editor, cx) else {
            return;
        };

        let automation_id = editor.automation_id.clone();
        let enabled = cx
            .global::<settings::AgentHostState>()
            .automations
            .iter()
            .find(|automation| automation.id == automation_id)
            .map(|automation| automation.enabled)
            .unwrap_or(true);
        let automation = settings::Automation {
            id: automation_id.clone(),
            name: editor.name.read(cx).value().trim().to_string(),
            enabled,
            workspace_dir: None,
            trigger,
            action: editor.action(cx),
            sinks: editor.notification.to_sinks(),
        };
        let created_name = editor.is_new.then(|| automation.name.clone());
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
                                if let Some(name) = &created_name {
                                    crate::status_item::notify_success(format!(
                                        "已创建自动化「{name}」"
                                    ));
                                }
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

    pub(super) fn agent_conversation_count(&self, agent_id: &str) -> usize {
        self.sessions
            .iter()
            .filter(|session| self.session_belongs_to_agent(session, agent_id))
            .count()
    }

    pub(super) fn session_belongs_to_agent(
        &self,
        session: &crate::Session,
        agent_id: &str,
    ) -> bool {
        bound_agent_definition_id(
            session.agent_definition_id.as_deref(),
            session.automation_id.as_deref(),
        ) == Some(agent_id)
    }
}
