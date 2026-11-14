//! 会话的命令（`smelt.sessions/resume` 等，参数见 `smelt_plugin_api::sessions`）。
//!
//! 会话归工作台管，插件只说开什么；启动命令、环境变量、初始配置在这里按那台机器的
//! 智能体配置现算。

use smelt_model::agent_kind::{ConversationAgentKind, ConversationLaunchSpec, HistorySourceKind};
use smelt_plugin_api::CommandSpec;
use smelt_plugin_api::sessions::{self, Handoff, Resume, StartConversation};

use super::*;
use crate::plugin_host::{WorkspaceRef, in_main_window, main_window, update_workspace};

/// 宿主登记的会话命令，跟 `smelt.sessions` 的自带命令一起登记。
pub(crate) fn commands(workspace: &WorkspaceRef) -> Vec<CommandSpec> {
    let resume = workspace.clone();
    let in_terminal = workspace.clone();
    let handoff = workspace.clone();
    let send = workspace.clone();
    let activate = workspace.clone();
    let start = workspace.clone();
    vec![
        sessions::START_CONVERSATION.spec("跟智能体开对话", move |args, window, cx| {
            in_main_window(window, &start, cx, |this, window, cx| {
                this.start_conversation_with(args, window, cx)
            })?
        }),
        sessions::ACTIVATE.spec("跳到会话", move |args, window, cx| {
            in_main_window(window, &activate, cx, |this, window, cx| {
                this.request_notification_session_jump(args.session_id, window, cx)
            })
        }),
        sessions::RESUME.spec("续接历史会话", move |args, window, cx| {
            let window = main_window(window)?;
            update_workspace(&resume, cx, |this, cx| {
                this.resume_from_history(args, window, cx)
            })
        }),
        sessions::RESUME_IN_TERMINAL.spec("在终端里续接历史会话", move |args, _, cx| {
            update_workspace(&in_terminal, cx, |this, cx| {
                this.resume_from_history_in_terminal(args, cx)
            })
        }),
        sessions::HANDOFF.spec("交接给另一家 agent", move |args, window, cx| {
            let window = main_window(window)?;
            update_workspace(&handoff, cx, |this, cx| this.hand_off(args, window, cx))
        }),
        sessions::SEND_TEXT.spec("交给当前会话", move |args, window, cx| {
            let window = main_window(window)?;
            update_workspace(&send, cx, |this, cx| {
                this.send_to_session(&args.text, window, cx)
            })
        }),
    ]
}

impl Workspace {
    /// 跟一个智能体开一段新对话。启动命令、环境变量取智能体那台机器自己的。
    fn start_conversation_with(
        &mut self,
        args: StartConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let agent = settings::agent_definition(&args.machine, &args.agent_definition_id, cx)
            .ok_or_else(|| "智能体已经不存在".to_string())?;
        if agent.engine_kind().is_none() {
            return Err("请选择一个可用的执行引擎".to_string());
        }
        let (kind, launch) =
            settings::agent_host_settings(&args.machine, cx).resolve_agent(&agent)?;
        if !smelt_model::agent_definition::agent_engine_kinds().contains(&kind) {
            return Err(format!(
                "{} 暂未开放为产品级智能体引擎，请改用 Pi",
                kind.short_label()
            ));
        }
        let runtime = crate::runtimes::runtime(&args.machine, cx)
            .ok_or_else(crate::runtime_call::removed_text)?;
        self.add_acp_session(
            crate::workspace_sessions::NewAcpSessionRequest {
                runtime,
                agent: kind,
                launch_override: Some(launch),
                profile_id: None,
                agent_definition: Some(agent),
                fallback_cwd: Some(args.cwd),
                pending_prompt: None,
                automation_id: None,
                workset_id: None,
                activate: true,
            },
            window,
            cx,
        );
        Ok(())
    }

    /// 宿主的对话视图接上守护里那段会话，给插件嵌进自己的页面（见
    /// `smelt_plugin_api::Host::embed_conversation`）。事件照工作台里的对话处理。
    pub(crate) fn embed_conversation(
        &mut self,
        request: smelt_plugin_api::EmbedConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<smelt_plugin_api::EmbeddedConversation, String> {
        let agent = ConversationAgentKind::from_id(&request.agent)
            .ok_or_else(|| format!("认不出 agent「{}」", request.agent))?;
        let runtime = crate::runtimes::runtime(&request.machine, cx)
            .ok_or_else(crate::runtime_call::removed_text)?;
        let session_id = request.session_id;
        let view = cx.new(|cx| {
            acp_view::AcpView::placeholder(
                cx,
                acp_view::AcpViewOrigin {
                    runtime,
                    launch_prep: None,
                    agent,
                    // 接的是守护里正在跑的会话，不按这份规格重新起。
                    launch: agent.default_launch(),
                    refresh_launch_from_settings: false,
                    profile_id: None,
                    cwd: Some(request.cwd),
                    reason: request.reason,
                    entries: Vec::new(),
                    resume_session_id: None,
                    saved_sid: Some(session_id.clone()),
                },
            )
        });
        view.update(cx, |view, cx| {
            view.restore_conversation_binding(request.binding);
            view.maybe_auto_resume(window, cx);
        });
        let subscription = self.subscribe_acp_persist(&view, window, cx);
        Ok(smelt_plugin_api::EmbeddedConversation {
            view: view.into(),
            session_id,
            _subscription: subscription,
        })
    }

    fn resume_from_history(
        &mut self,
        args: Resume,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let runtime = args.machine;
        let agent = HistorySourceKind::from_id(&args.agent)
            .and_then(HistorySourceKind::acp)
            .ok_or_else(|| format!("「{}」不支持 ACP 续接", args.agent))?;
        let launch_override = profile_launch(&runtime, args.profile_id.as_deref(), cx)?;
        self.resume_acp_session(
            crate::workspace_sessions::AcpResumeRequest {
                runtime,
                agent,
                launch_override,
                profile_id: args.profile_id,
                cwd: args.cwd,
                resume_id: args.resume_id,
                agent_definition_id: args.agent_definition_id,
            },
            window,
            cx,
        );
        Ok(())
    }

    fn resume_from_history_in_terminal(
        &mut self,
        args: Resume,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let runtime = args.machine;
        let agent = HistorySourceKind::from_id(&args.agent)
            .ok_or_else(|| format!("认不出历史来源「{}」", args.agent))?;
        let launch_override = profile_launch(&runtime, args.profile_id.as_deref(), cx)?;
        self.resume_cli_session(
            runtime,
            agent,
            launch_override,
            args.cwd,
            args.resume_id,
            cx,
        );
        Ok(())
    }

    fn hand_off(
        &mut self,
        args: Handoff,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let agent = ConversationAgentKind::from_id(&args.agent)
            .ok_or_else(|| format!("认不出 agent「{}」，交接取消", args.agent))?;
        // 交接会话开在源存档所在的那台机器上，用那台机器的启动命令。
        let runtime = crate::runtimes::runtime(&args.machine, cx)
            .ok_or_else(|| "这台机器已经移除，交接取消".to_string())?;
        let launch = match profile_launch(&args.machine, args.profile_id.as_deref(), cx)? {
            Some(launch) => launch,
            None => settings::acp_launch_for(&args.machine, agent, cx),
        };
        self.add_acp_handoff_session(handoff_request(runtime, agent, launch, args), window, cx);
        Ok(())
    }
}

/// 自定义 Agent 的启动规格（它自己的命令和环境变量）；基础 agent 是 `None`，跟设置走。
fn profile_launch(
    machine: &RuntimeKey,
    profile_id: Option<&str>,
    cx: &App,
) -> Result<Option<ConversationLaunchSpec>, String> {
    let Some(id) = profile_id else {
        return Ok(None);
    };
    let config = settings::agent_host_settings(machine, cx);
    let profile = config
        .find_profile(id)
        .ok_or_else(|| format!("找不到自定义 Agent「{id}」，可能已经删了"))?;
    config.profile_launch_spec(profile).map(Some)
}

/// 交接请求：目标从零开始，第一句话是插件写好的交接说明。
fn handoff_request(
    runtime: crate::runtimes::Runtime,
    agent: ConversationAgentKind,
    launch: ConversationLaunchSpec,
    args: Handoff,
) -> acp_view::AcpHandoffRequest {
    acp_view::AcpHandoffRequest {
        runtime,
        launch_prep: None,
        source: Some(acp_view::AcpForkOrigin {
            session_id: args.source_session_id,
            // 源是磁盘上的历史存档，不是当前开着的某条 Smelt 会话。
            from_history: true,
        }),
        cwd: Some(args.cwd),
        agent,
        launch,
        // 基础 agent 跟设置页走，自定义 Agent 用自己的命令。
        refresh_launch_from_settings: args.profile_id.is_none(),
        profile_id: args.profile_id,
        // 模型/配置是各家私有取值，一律不跨会话搬。
        config_values: Vec::new(),
        ephemeral_env: Default::default(),
        prompt: args.prompt,
        // 文本交接，不搬图片。
        images: Vec::new(),
        resume_session_id: None,
        fork_session_id: None,
        fork_cut: None,
        conversation_binding: smelt_model::conversation::ConversationBinding::Direct,
    }
}

#[cfg(test)]
mod tests {
    use super::{ConversationAgentKind, ConversationLaunchSpec, Handoff, handoff_request};

    #[test]
    fn handoff_records_the_history_source_and_carries_no_private_config() {
        let args = Handoff {
            machine: smelt_plugin_api::RuntimeKey::Local,
            cwd: "/repo".into(),
            agent: "claude".into(),
            profile_id: None,
            prompt: "接着干".into(),
            source_session_id: "grok-session-1".into(),
        };
        let request = handoff_request(
            crate::runtimes::Runtime::local(),
            ConversationAgentKind::Claude,
            ConversationLaunchSpec::from_command("claude"),
            args,
        );
        let origin = request.source.expect("要记下源会话");
        assert_eq!(origin.session_id, "grok-session-1");
        assert!(origin.from_history);
        assert!(request.config_values.is_empty());
        assert!(request.refresh_launch_from_settings);
        assert_eq!(request.cwd.as_deref(), Some("/repo"));
        assert_eq!(request.prompt, "接着干");
    }
}
