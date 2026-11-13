//! 会话的命令（`smelt.sessions/resume` 等，参数见 `smelt_plugin_api::sessions`）。
//!
//! 会话归工作台管，插件只说开什么；启动命令、环境变量、初始配置在这里按那台机器的
//! 智能体配置现算。

use smelt_model::agent_kind::{ConversationAgentKind, ConversationLaunchSpec, HistorySourceKind};
use smelt_plugin_api::CommandSpec;
use smelt_plugin_api::sessions::{self, Handoff, Resume};

use super::*;
use crate::plugin_host::{WorkspaceRef, update_workspace};

/// 宿主登记的会话命令，跟 `smelt.sessions` 的自带命令一起登记。
pub(crate) fn commands(workspace: &WorkspaceRef) -> Vec<CommandSpec> {
    let resume = workspace.clone();
    let in_terminal = workspace.clone();
    let handoff = workspace.clone();
    let send = workspace.clone();
    vec![
        sessions::RESUME.spec("续接历史会话", move |args, window, cx| {
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
            update_workspace(&handoff, cx, |this, cx| this.hand_off(args, window, cx))
        }),
        sessions::SEND_TEXT.spec("交给当前会话", move |args, window, cx| {
            update_workspace(&send, cx, |this, cx| {
                this.send_to_session(&args.text, window, cx)
            })
        }),
    ]
}

impl Workspace {
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
