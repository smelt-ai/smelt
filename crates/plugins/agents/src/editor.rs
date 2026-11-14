//! 编辑一个智能体：输入框、Pi 配置和记忆的读取、对定义的修改、删除，以及开对话。
//! 都是 [`AgentsPage`] 上的方法；页面本身见 `lib.rs`。

use super::*;

pub(crate) struct AgentEditor {
    pub(crate) agent: AgentRef,
    pub(crate) name: Entity<InputState>,
    /// 名字平时当标题看，点一下才变成输入框，避免一进编辑器就闪光标。
    pub(crate) instructions: Entity<TextareaState>,
    /// 绑定参考链接用的输入框。目录走选择框，链接只能手输。
    pub(crate) context_link: Entity<InputState>,
    /// 智能体那台机器的 Pi 配置：插件、MCP 服务器、可选模型都从这里取。编辑器自己读、
    /// 自己留一份，不和能力库共用。
    pub(crate) pi: AgentPiSnapshot,
    /// 同理，记忆不在渲染里扫：打开编辑器、运行时报记忆目录有变化、有对话停下来、
    /// 点刷新或删除后才重扫。
    pub(crate) memories: AgentMemories,
    pub(crate) tab: AgentEditorTab,
    /// 记忆页签的搜索框和筛选。
    pub(crate) memory_query: Entity<InputState>,
    pub(crate) memory_filter: MemoryFilter,
    /// 记忆页签里展开看全文的那条记忆。只是看，改仍走系统编辑器。
    pub(crate) expanded_memory: Option<(MemoryLayer, String)>,
    _subscriptions: Vec<Subscription>,
}

impl AgentEditor {
    /// 智能体那台机器上扫出的插件；还没读到是空的。
    pub(crate) fn pi_plugins(&self) -> &[api::model::pi_plugin_catalog::PiPlugin] {
        self.pi
            .config
            .as_ref()
            .map(|config| config.plugins.as_slice())
            .unwrap_or_default()
    }

    /// 那台机器 `~/.pi/agent/mcp.json` 里的服务器；还没读到或读不出来是空的。
    pub(crate) fn pi_mcp_servers(&self) -> &[api::model::pi_mcp::PiMcpServer] {
        self.pi
            .config
            .as_ref()
            .and_then(|config| config.mcp_servers.as_ref().ok())
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// 那台机器上可选的模型；还没读到是空的。
    pub(crate) fn pi_model_choice_groups(
        &self,
    ) -> &[api::model::pi_model_settings::PiModelChoiceGroup] {
        self.pi
            .config
            .as_ref()
            .map(|config| config.model_choice_groups.as_slice())
            .unwrap_or_default()
    }

    /// 还没读到配置时显示的话；读到了是 `None`。
    pub(crate) fn pi_pending_text(&self) -> Option<String> {
        if self.pi.config.is_some() {
            return None;
        }
        Some(
            self.pi
                .error
                .clone()
                .unwrap_or_else(|| "正在读取这台机器的 Pi 配置…".to_string()),
        )
    }
}

/// 等确认删除的记忆。
pub(crate) struct MemoryDeleteTarget {
    pub(crate) agent_id: String,
    pub(crate) layer: MemoryLayer,
    pub(crate) file_name: String,
    pub(crate) name: String,
}

impl AgentsPage {
    /// 改正在编辑的那个智能体的定义。编辑器已经换了人（菜单是旧的）就不改。
    fn edit_agent(
        &mut self,
        agent_id: &str,
        f: impl FnOnce(&mut AgentDefinition) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(runtime) = self
            .editor
            .as_ref()
            .filter(|editor| editor.agent.id == agent_id)
            .map(|editor| editor.agent.runtime.clone())
        else {
            return;
        };
        let id = agent_id.to_string();
        let task = api::edit_agent_settings(
            &runtime,
            move |config| {
                if let Some(agent) = config.agents.iter_mut().find(|agent| agent.id == id) {
                    f(agent);
                }
            },
            cx,
        );
        self.error = None;
        self.report_failure(task, cx);
        cx.notify();
    }

    /// 不等结果的操作：失败了把原因显示在页面上。
    pub(crate) fn report_failure<T: 'static>(
        &mut self,
        task: Task<Result<T, String>>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(error);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(crate) fn create_agent(
        &mut self,
        runtime: RuntimeKey,
        kind: ConversationAgentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = uuid::Uuid::new_v4().to_string();
        let existing = agents_on(&runtime, cx).len();
        let name = if existing == 0 {
            format!("{} 智能体", kind.short_label())
        } else {
            format!("{} 智能体 {}", kind.short_label(), existing + 1)
        };
        let agent = AgentRef::new(runtime.clone(), id.clone());
        let task = api::edit_agent_settings(
            &runtime,
            move |config| {
                config.agents.push(AgentDefinition {
                    id,
                    name,
                    engine_kind_id: kind.id().to_string(),
                    ..Default::default()
                });
            },
            cx,
        );
        let as_draft = std::mem::take(&mut self.arm_conversation_on_create);
        cx.spawn_in(window, async move |this, cx| {
            let saved = task.await;
            let _ = this.update_in(cx, |this, window, cx| match saved {
                Ok(()) => {
                    if as_draft {
                        this.conversation_draft = Some(agent.clone());
                    }
                    this.open_agent(agent, AgentEditorTab::default(), window, cx);
                }
                Err(error) => {
                    this.arm_conversation_on_create = as_draft;
                    this.error = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 打开智能体的编辑页，停在 `tab`。编辑器（输入框）在这里建，不在渲染里。
    pub(crate) fn open_agent(
        &mut self,
        agent: AgentRef,
        tab: AgentEditorTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.error = None;
        if let Some(editor) = self.editor.as_mut().filter(|editor| editor.agent == agent) {
            editor.tab = tab;
            cx.notify();
            return;
        }
        let Some(definition) = agent.definition(cx) else {
            self.close_editor(cx);
            return;
        };
        self.editor = Some(self.build_editor(agent, definition, tab, window, cx));
        self.refresh_agent_memories(cx);
        self.refresh_agent_pi_config(cx);
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn build_editor(
        &mut self,
        agent: AgentRef,
        definition: AgentDefinition,
        tab: AgentEditorTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AgentEditor {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("给它起个名字")
                .default_value(definition.name)
        });
        let instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(8, 18)
                .placeholder("它负责什么、怎么做事、什么时候该停下来问你。")
                .default_value(definition.prompt)
        });
        let context_link =
            cx.new(|cx| InputState::new(window, cx).placeholder("粘贴一个参考链接，回车添加"));
        let memory_query =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索名称、描述或正文"));

        let link_agent = agent.id.clone();
        let subscriptions = vec![
            cx.subscribe_in(
                &context_link,
                window,
                move |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.add_agent_context_link(link_agent.clone(), window, cx);
                    }
                },
            ),
            cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| {
                if save_on_input_event(event) {
                    this.save_editor_text(cx);
                }
                if matches!(event, InputEvent::PressEnter { .. }) {
                    window.focus(&this.focus_handle, cx);
                }
            }),
            cx.subscribe(&instructions, |this, _, event: &InputEvent, cx| {
                if save_on_input_event(event) {
                    this.save_editor_text(cx);
                }
            }),
            cx.subscribe(&memory_query, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        AgentEditor {
            agent,
            name,
            instructions,
            context_link,
            pi: AgentPiSnapshot::default(),
            memories: AgentMemories::default(),
            tab,
            memory_query,
            memory_filter: MemoryFilter::default(),
            expanded_memory: None,
            _subscriptions: subscriptions,
        }
    }

    /// 名字和指令边打边存。
    fn save_editor_text(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let name = editor.name.read(cx).value().to_string();
        let prompt = editor.instructions.read(cx).value().to_string();
        let id = editor.agent.id.clone();
        self.edit_agent(
            &id,
            move |agent| {
                agent.name = name;
                agent.prompt = prompt;
            },
            cx,
        );
    }

    /// 勾选 / 取消勾选一个 Pi 插件。写回定义后，新开的对话只会加载勾选项。
    pub(crate) fn toggle_agent_plugin(
        &mut self,
        agent_id: String,
        plugin_id: String,
        cx: &mut Context<Self>,
    ) {
        self.edit_agent(
            &agent_id,
            move |agent| {
                if let Some(pos) = agent.plugins.iter().position(|id| *id == plugin_id) {
                    agent.plugins.remove(pos);
                } else {
                    agent.plugins.push(plugin_id);
                }
            },
            cx,
        );
    }

    /// 勾选 / 取消勾选一台 MCP 服务器。新开的对话只连勾选的服务器，没勾就不连。
    pub(crate) fn toggle_agent_mcp_server(
        &mut self,
        agent_id: String,
        server: String,
        cx: &mut Context<Self>,
    ) {
        self.edit_agent(
            &agent_id,
            move |agent| {
                if let Some(pos) = agent.mcp_servers.iter().position(|name| *name == server) {
                    agent.mcp_servers.remove(pos);
                } else {
                    agent.mcp_servers.push(server);
                }
            },
            cx,
        );
    }

    /// 挑一个或多个目录绑定到智能体。Pi 只认单个 cwd，所以这些目录不会变成工作目录，而是
    /// 以绝对路径写进 system prompt 让它自己去读。目录在智能体那台机器上，在那台机器上选。
    pub(crate) fn pick_agent_context_folders(&mut self, agent_id: String, cx: &mut Context<Self>) {
        let Some(runtime) = self
            .editor
            .as_ref()
            .filter(|editor| editor.agent.id == agent_id)
            .map(|editor| editor.agent.runtime.clone())
        else {
            return;
        };
        let picked = api::PICK_PATHS.run(
            &api::PathRequest {
                machine: runtime,
                title: "选择要绑定的目录".to_string(),
                confirm: "绑定".to_string(),
                files: false,
                multiple: true,
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let picked = picked.await;
            let _ = this.update(cx, |this, cx| match picked {
                Ok(Some(folders)) => this.add_agent_context_folders(agent_id, folders, cx),
                Ok(None) => {}
                Err(error) => {
                    this.error = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn add_agent_context_folders(
        &mut self,
        agent_id: String,
        folders: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.edit_agent(
            &agent_id,
            move |agent| {
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
    }

    pub(crate) fn remove_agent_context_folder(
        &mut self,
        agent_id: String,
        folder: String,
        cx: &mut Context<Self>,
    ) {
        self.edit_agent(
            &agent_id,
            move |agent| agent.context_folders.retain(|it| *it != folder),
            cx,
        );
    }

    pub(crate) fn add_agent_context_link(
        &mut self,
        agent_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self
            .editor
            .as_ref()
            .filter(|editor| editor.agent.id == agent_id)
            .map(|editor| editor.context_link.clone())
        else {
            return;
        };
        let link = input.read(cx).value().trim().to_string();
        if link.is_empty() {
            return;
        }
        self.edit_agent(
            &agent_id,
            move |agent| {
                if !agent.context_links.contains(&link) {
                    agent.context_links.push(link);
                }
            },
            cx,
        );
        input.update(cx, |state, cx| state.set_value("", window, cx));
    }

    pub(crate) fn remove_agent_context_link(
        &mut self,
        agent_id: String,
        link: String,
        cx: &mut Context<Self>,
    ) {
        self.edit_agent(
            &agent_id,
            move |agent| agent.context_links.retain(|it| *it != link),
            cx,
        );
    }

    pub(crate) fn set_agent_engine(
        &mut self,
        id: String,
        kind: ConversationAgentKind,
        cx: &mut Context<Self>,
    ) {
        self.edit_agent(
            &id,
            move |agent| agent.engine_kind_id = kind.id().to_string(),
            cx,
        );
    }

    /// 开关智能体的长期记忆。关掉后新对话不注入、不写入；正在进行的对话下一轮生效。
    /// 已有记忆文件保留，重新打开后照常使用。
    pub(crate) fn set_agent_memory_enabled(
        &mut self,
        id: String,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        self.edit_agent(&id, move |agent| agent.memory_enabled = enabled, cx);
    }

    pub(crate) fn set_agent_model(
        &mut self,
        id: String,
        model_provider: String,
        model_id: String,
        cx: &mut Context<Self>,
    ) {
        let model_provider = model_provider.trim().to_string();
        let model_id = model_id.trim().to_string();
        self.edit_agent(
            &id,
            move |agent| {
                agent.model_provider = model_provider;
                agent.model_id = model_id;
            },
            cx,
        );
    }

    /// 请运行时重新扫描插件和 MCP。用户在能力库里装完插件后不用重开编辑器。
    pub(crate) fn refresh_agent_plugins(&mut self, cx: &mut Context<Self>) {
        self.refresh_agent_pi_config(cx);
    }

    /// 向智能体那台机器读一次 Pi 配置给编辑器。回来时编辑器已经换了智能体，或者又发了
    /// 一次读取，这份就丢掉。
    pub(crate) fn refresh_agent_pi_config(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        editor.pi.generation += 1;
        let generation = editor.pi.generation;
        let agent = editor.agent.clone();
        let task = api::call::<api::model::control_api::PiConfigGet>(
            &agent.runtime,
            api::client::protocol::control::EmptyParams::default(),
            api::CALL_TIMEOUT,
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ =
                this.update(cx, |this, cx| {
                    let Some(editor) = this.editor.as_mut().filter(|editor| {
                        editor.agent == agent && editor.pi.generation == generation
                    }) else {
                        return;
                    };
                    match result {
                        Ok(config) => {
                            editor.pi.config = Some(config);
                            editor.pi.error = None;
                        }
                        Err(error) => {
                            editor.pi.error = Some(format!("读取这台机器的 Pi 配置失败：{error}"));
                        }
                    }
                    cx.notify();
                });
        })
        .detach();
    }

    /// 向运行时重新取当前编辑器那个智能体的记忆。回来时编辑器已换人就丢掉。
    ///
    /// 第一次取时顺带请运行时监听它的记忆目录（`agent.memory.watch`）：之后智能体在对话里
    /// 写了新记忆、用户在外部编辑器改了文件，运行时推 `agent_memory.changed`。先登记再取，
    /// 登记之后的变化不会漏。
    pub(crate) fn refresh_agent_memories(&mut self, cx: &mut Context<Self>) {
        use api::model::control_api::{AgentDefinitionIdParams, AgentMemoryList, AgentMemoryWatch};

        let Some(agent) = self.editor.as_ref().map(|editor| editor.agent.clone()) else {
            return;
        };
        let params = AgentDefinitionIdParams {
            id: agent.id.clone(),
        };
        let watch = self.watched_memories.insert(agent.clone()).then(|| {
            api::call::<AgentMemoryWatch>(&agent.runtime, params.clone(), api::CALL_TIMEOUT, cx)
        });
        let runtime = agent.runtime.clone();
        cx.spawn(async move |this, cx| {
            let watched = match watch {
                Some(watch) => watch.await.map(|_| ()),
                None => Ok(()),
            };
            // 登记完再列，登记之后的变化都会推过来。
            let listed = cx
                .update(|cx| api::call::<AgentMemoryList>(&runtime, params, api::CALL_TIMEOUT, cx))
                .await;
            let _ =
                this.update(cx, |this, cx| {
                    if let Err(error) = watched {
                        // 没登记上：下次重取时再试。
                        eprintln!("[agents] 请运行时监听记忆失败：{error}");
                        this.watched_memories.remove(&agent);
                    }
                    match listed {
                        Ok(memories) => {
                            // 重取大多数时候什么都没变，没变就不重绘。
                            if let Some(editor) = this.editor.as_mut().filter(|editor| {
                                editor.agent == agent && editor.memories != memories
                            }) {
                                editor.memories = memories;
                                cx.notify();
                            }
                        }
                        Err(error) => {
                            this.error = Some(format!("读取记忆失败：{error}"));
                            cx.notify();
                        }
                    }
                });
        })
        .detach();
    }

    /// 删除或采纳一条记忆，完成后重取列表。发给编辑器里那个智能体的机器。
    fn submit_memory_change<C>(
        &mut self,
        params: C::Params,
        failure: &'static str,
        cx: &mut Context<Self>,
    ) where
        C: api::client::protocol::control::ControlCall + 'static,
        C::Params: Send + 'static,
        C::Result: Send + 'static,
    {
        let Some(runtime) = self
            .editor
            .as_ref()
            .map(|editor| editor.agent.runtime.clone())
        else {
            return;
        };
        let task = api::call::<C>(&runtime, params, api::CALL_TIMEOUT, cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.error = result.err().map(|error| format!("{failure}：{error}"));
                this.refresh_agent_memories(cx);
            });
        })
        .detach();
    }

    pub(crate) fn select_agent_editor_tab(&mut self, tab: AgentEditorTab, cx: &mut Context<Self>) {
        if let Some(editor) = self.editor.as_mut() {
            editor.tab = tab;
        }
        cx.notify();
    }

    pub(crate) fn set_agent_memory_filter(&mut self, filter: MemoryFilter, cx: &mut Context<Self>) {
        if let Some(editor) = self.editor.as_mut() {
            editor.memory_filter = filter;
        }
        cx.notify();
    }

    /// 一次只展开一条；再点同一条收起。
    pub(crate) fn toggle_agent_memory_expanded(
        &mut self,
        layer: MemoryLayer,
        file_name: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.editor.as_mut() {
            let target = (layer, file_name);
            editor.expanded_memory = if editor.expanded_memory.as_ref() == Some(&target) {
                None
            } else {
                Some(target)
            };
        }
        cx.notify();
    }

    pub(crate) fn open_agent_memory_file(
        &mut self,
        path: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        // 记忆文件在智能体那台机器上，路径只在本机才打得开。
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.agent.runtime != RuntimeKey::Local)
        {
            self.error = Some("记忆文件在那台机器上，这台电脑打不开；可以展开直接查看".to_string());
            cx.notify();
            return;
        }
        let opened = api::OPEN_IN_TEXT_EDITOR.run(&api::OpenInTextEditor { path }, cx);
        self.report_failure(opened, cx);
    }

    pub(crate) fn open_agent_memory_folder(
        &mut self,
        agent_id: String,
        layer: MemoryLayer,
        cx: &mut Context<Self>,
    ) {
        let Some(runtime) = self
            .editor
            .as_ref()
            .filter(|editor| editor.agent.id == agent_id)
            .map(|editor| editor.agent.runtime.clone())
        else {
            return;
        };
        // 目录在运行时那台机器上：让运行时建好并给出路径，只有本机运行时打得开。
        if runtime != RuntimeKey::Local {
            self.error =
                Some("记忆目录在那台机器上，这台电脑打不开；可以在下面直接查看记忆".to_string());
            cx.notify();
            return;
        }
        let ensured = api::call::<api::model::control_api::AgentMemoryEnsureDir>(
            &runtime,
            api::model::control_api::AgentMemoryDirParams {
                agent_id,
                layer: layer.wire(),
            },
            api::CALL_TIMEOUT,
            cx,
        );
        cx.spawn(async move |this, cx| {
            let ensured = ensured.await;
            let _ = this.update(cx, |this, cx| match ensured {
                Ok(dir) => {
                    let opened = api::OPEN_IN_FILE_MANAGER.run(
                        &api::OpenInFileManager {
                            path: dir.path.into(),
                        },
                        cx,
                    );
                    this.report_failure(opened, cx);
                }
                Err(error) => {
                    this.error = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 点删除先确认；记忆删了就没了，全局记忆还会从所有智能体里消失。
    pub(crate) fn request_delete_agent_memory(
        &mut self,
        target: MemoryDeleteTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scope = match target.layer {
            MemoryLayer::Agent => "这个智能体之后不会再用到它。",
            MemoryLayer::User => "这是全局记忆，所有智能体之后都不会再用到它。",
        };
        let message = format!(
            "将删除记忆「{}」和它的使用记录。{scope}此操作不可撤销。",
            target.name
        );
        let page = cx.entity();
        let target = Rc::new(RefCell::new(Some(target)));
        window.open_alert_dialog(cx, move |alert, _, _| {
            let page = page.clone();
            let target = target.clone();
            alert
                .confirm()
                .title("确定删除这条记忆吗？")
                .child(message.clone())
                .ok_text("确定删除")
                .on_ok(move |_, _, cx| {
                    if let Some(target) = target.borrow_mut().take() {
                        page.update(cx, |page, cx| page.delete_agent_memory(target, cx));
                    }
                    true
                })
        });
    }

    fn delete_agent_memory(&mut self, target: MemoryDeleteTarget, cx: &mut Context<Self>) {
        if self
            .editor
            .as_ref()
            .is_none_or(|editor| editor.agent.id != target.agent_id)
        {
            return;
        }
        self.submit_memory_change::<api::model::control_api::AgentMemoryDelete>(
            api::model::control_api::AgentMemoryFileParams {
                agent_id: target.agent_id,
                layer: target.layer.wire(),
                file_name: target.file_name,
            },
            "删除记忆失败",
            cx,
        );
    }

    pub(crate) fn approve_agent_memory(
        &mut self,
        agent_id: String,
        layer: MemoryLayer,
        file_name: String,
        cx: &mut Context<Self>,
    ) {
        if self
            .editor
            .as_ref()
            .is_none_or(|editor| editor.agent.id != agent_id)
        {
            return;
        }
        self.submit_memory_change::<api::model::control_api::AgentMemoryApprove>(
            api::model::control_api::AgentMemoryFileParams {
                agent_id,
                layer: layer.wire(),
                file_name,
            },
            "采纳记忆失败",
            cx,
        );
    }

    /// 回到目录。
    pub(crate) fn close_editor(&mut self, cx: &mut Context<Self>) {
        self.editor = None;
        self.error = None;
        cx.notify();
    }

    /// 编辑页左上角的返回：草稿是取消新建，否则回到目录。
    pub(crate) fn close_agent_settings(&mut self, cx: &mut Context<Self>) {
        if self.editing_conversation_draft() {
            self.cancel_conversation_draft(cx);
            return;
        }
        self.close_editor(cx);
    }

    /// 跟它开一段新对话。失败原因显示在页面上。
    pub(crate) fn start_agent_conversation(&mut self, agent: AgentRef, cx: &mut Context<Self>) {
        self.error = None;
        let task = start_conversation(agent, cx);
        self.report_failure(task, cx);
        cx.notify();
    }

    /// 选择器里点了一个智能体。`start` 为真时同时开对话；更换默认时只改记录。
    ///
    /// 智能体页上的「新对话」不走这里，避免点一次专用智能体就把默认抢走。
    pub(crate) fn choose_default_conversation_agent(
        &mut self,
        agent: AgentRef,
        start: bool,
        cx: &mut Context<Self>,
    ) {
        let ready = agent
            .definition(cx)
            .is_some_and(|definition| definition.is_conversation_ready());
        if !ready {
            if is_default_conversation_agent(&agent, cx) {
                clear_default_conversation_agent(&agent.runtime, cx);
            }
            self.error = Some("这个智能体不能用来开对话".to_string());
            cx.notify();
            return;
        }
        self.discard_uncommitted_draft(Some(&agent), cx);
        self.picking_for_conversation = false;
        set_default_conversation_agent(&agent, cx);
        if start {
            self.start_agent_conversation(agent, cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn commit_conversation_draft(&mut self, cx: &mut Context<Self>) {
        let Some(agent) = self.conversation_draft.clone() else {
            return;
        };
        let ready = agent
            .definition(cx)
            .is_some_and(|definition| definition.is_conversation_ready());
        if !ready {
            self.error = Some("这个智能体还不能开始对话".to_string());
            cx.notify();
            return;
        }
        self.conversation_draft = None;
        self.arm_conversation_on_create = false;
        set_default_conversation_agent(&agent, cx);
        self.close_editor(cx);
        self.start_agent_conversation(agent, cx);
    }

    pub(crate) fn editing_conversation_draft(&self) -> bool {
        self.conversation_draft
            .as_ref()
            .is_some_and(|draft| self.editor.as_ref().map(|editor| &editor.agent) == Some(draft))
    }

    /// `keep` 是这次要留下的智能体。草稿就是它时只清标记，不删除。
    pub(crate) fn discard_uncommitted_draft(
        &mut self,
        keep: Option<&AgentRef>,
        cx: &mut Context<Self>,
    ) {
        self.arm_conversation_on_create = false;
        let Some(draft) = self.conversation_draft.as_ref() else {
            return;
        };
        if keep == Some(draft) {
            self.conversation_draft = None;
            return;
        }
        self.cancel_conversation_draft(cx);
    }

    /// 草稿不要了：删掉那个智能体，回到目录。
    fn cancel_conversation_draft(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.conversation_draft.take() else {
            return;
        };
        self.arm_conversation_on_create = false;
        if is_default_conversation_agent(&draft, cx) {
            clear_default_conversation_agent(&draft.runtime, cx);
        }
        let removed_id = draft.id.clone();
        let task = api::edit_agent_settings(
            &draft.runtime,
            move |config| config.agents.retain(|agent| agent.id != removed_id),
            cx,
        );
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.agent == draft)
        {
            self.close_editor(cx);
        }
        cx.spawn(async move |this, cx| {
            if let Err(error) = task.await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("取消新建失败，这个智能体还在：{error}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// 点删除先确认。引用它的自动化还在时不让删：自动化会跑不起来。
    pub(crate) fn start_delete_agent(
        &mut self,
        agent: AgentRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = agent
            .definition(cx)
            .map(|definition| agent_display_name(&definition.name))
        else {
            return;
        };
        if let Err(error) = self.automations_block_delete(&agent) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        let conversations = agent_conversation_count(&agent, cx);
        let kept = if conversations == 0 {
            String::new()
        } else {
            format!("已有的 {conversations} 段对话会保留。")
        };
        let message = format!("将删除智能体「{name}」和它的指令、插件设置。{kept}此操作不可撤销。");
        let page = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let page = page.clone();
            let agent = agent.clone();
            alert
                .confirm()
                .title("确定删除这个智能体吗？")
                .child(message.clone())
                .ok_text("确定删除")
                .on_ok(move |_, _, cx| {
                    let agent = agent.clone();
                    page.update(cx, |page, cx| page.delete_agent(agent, cx));
                    true
                })
        });
    }

    /// 自动化库还没读到、读不出来，或还有自动化挂着它时，不能删。引用它的自动化只可能在
    /// 同一台机器上。
    fn automations_block_delete(&self, agent: &AgentRef) -> Result<(), String> {
        if self.machines_without_automations.contains(&agent.runtime) {
            return Ok(());
        }
        let machine = self.automations.get(&agent.runtime);
        if let Some(error) = machine.and_then(|machine| machine.store_error.as_ref()) {
            return Err(format!("自动化存储不可用，暂时不能删除智能体：{error}"));
        }
        let Some(machine) = machine.filter(|machine| machine.is_ready()) else {
            return Err("正在读取关联自动化，请稍后再试".to_string());
        };
        match machine.automations_for(&agent.id).len() {
            0 => Ok(()),
            count => Err(format!("请先删除该智能体关联的 {count} 条自动化")),
        }
    }

    fn delete_agent(&mut self, agent: AgentRef, cx: &mut Context<Self>) {
        // 确认框开着的时候可能刚有自动化挂上它。
        if let Err(error) = self.automations_block_delete(&agent) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        self.error = None;
        if self.conversation_draft.as_ref() == Some(&agent) {
            self.conversation_draft = None;
            self.arm_conversation_on_create = false;
        }
        if is_default_conversation_agent(&agent, cx) {
            clear_default_conversation_agent(&agent.runtime, cx);
        }
        let deleted_id = agent.id.clone();
        let task = api::edit_agent_settings(
            &agent.runtime,
            move |config| {
                config
                    .agents
                    .retain(|definition| definition.id != deleted_id)
            },
            cx,
        );
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.agent == agent)
        {
            self.close_editor(cx);
        }
        self.report_failure(task, cx);
    }

    /// 挂着它的自动化有几条；那台机器的自动化库还没读到时算 0。
    pub(crate) fn automation_count(&self, agent: &AgentRef) -> usize {
        self.automations
            .get(&agent.runtime)
            .map_or(0, |machine| machine.automations_for(&agent.id).len())
    }

    /// 去能力库管那台机器的插件、MCP。
    pub(crate) fn open_capabilities(&mut self, tab: &str, cx: &mut Context<Self>) {
        let machine = self
            .editor
            .as_ref()
            .map_or(RuntimeKey::Local, |editor| editor.agent.runtime.clone());
        self.open_view(
            CAPABILITIES_VIEW,
            serde_json::json!({
                "machine": api::machine_to_json(&machine),
                "tab": tab,
            }),
            cx,
        );
    }

    fn open_view(&mut self, view: &str, args: serde_json::Value, cx: &mut Context<Self>) {
        let opened = api::OPEN_VIEW.run(
            &api::OpenView {
                view: view.to_string(),
                args,
            },
            cx,
        );
        self.report_failure(opened, cx);
    }
}

/// 智能体本身不「启动」：每次都新开一段引用它的对话，开在它那台机器上它自己的工作目录里。
/// 目录由那台机器的运行时建好并给出路径；起对话（引擎、模型、启动命令）归宿主。
pub(crate) fn start_conversation(agent: AgentRef, cx: &mut App) -> Task<Result<(), String>> {
    let Some(definition) = agent.definition(cx) else {
        return Task::ready(Err("智能体已经不存在".to_string()));
    };
    if let Some(reason) = agent_conversation_start_error(&definition) {
        return Task::ready(Err(reason.to_string()));
    }
    let space = api::call::<api::model::control_api::AgentSpaceEnsure>(
        &agent.runtime,
        api::model::control_api::AgentDefinitionIdParams {
            id: agent.id.clone(),
        },
        api::CALL_TIMEOUT,
        cx,
    );
    cx.spawn(async move |cx| {
        let space = space
            .await
            .map_err(|error| format!("无法创建智能体工作区：{error}"))?;
        cx.update(|cx| {
            api::sessions::START_CONVERSATION.run(
                &api::sessions::StartConversation {
                    machine: agent.runtime,
                    agent_definition_id: agent.id,
                    cwd: space.path,
                },
                cx,
            )
        })
        .await
    })
}

/// 这个智能体的对话有几段：同一台机器上、绑定了它的、没归档的。自动化的运行不算。
pub(crate) fn agent_conversation_count(agent: &AgentRef, cx: &App) -> usize {
    api::host(cx)
        .workspace_directory(&agent.runtime, cx)
        .map_or(0, |directory| {
            directory
                .conversations
                .iter()
                .filter(|conversation| {
                    conversation.archived_at.is_none()
                        && conversation.acp.as_ref().is_some_and(|acp| {
                            acp.automation_id.is_none()
                                && acp.agent_definition_id.as_deref() == Some(agent.id.as_str())
                        })
                })
                .count()
        })
}
