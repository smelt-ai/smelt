//! 插件开会话用的命令（`smelt.sessions/resume` 等，参数见 `smelt_plugin_api::sessions`）。
//!
//! 会话归工作台管，插件只说开什么；启动命令、环境变量、初始配置在这里按那台机器的
//! 智能体配置现算。

use serde::de::DeserializeOwned;
use smelt_model::agent_kind::{ConversationAgentKind, ConversationLaunchSpec, HistorySourceKind};
use smelt_plugin_api::sessions::{self, Handoff, Resume};

use super::*;

impl Workspace {
    pub(crate) fn run_resume_command(
        &mut self,
        args: &serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(args) = self.command_args::<Resume>(sessions::RESUME, args) else {
            return;
        };
        let runtime = sessions::machine_key(&args.machine);
        let Some(agent) = HistorySourceKind::from_id(&args.agent).and_then(HistorySourceKind::acp)
        else {
            self.report_background_error(format!("「{}」不支持 ACP 续接", args.agent));
            return;
        };
        let launch_override = match profile_launch(&runtime, args.profile_id.as_deref(), cx) {
            Ok(launch) => launch,
            Err(error) => {
                self.report_background_error(error);
                return;
            }
        };
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
    }

    pub(crate) fn run_resume_in_terminal_command(
        &mut self,
        args: &serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        let Some(args) = self.command_args::<Resume>(sessions::RESUME_IN_TERMINAL, args) else {
            return;
        };
        let runtime = sessions::machine_key(&args.machine);
        let Some(agent) = HistorySourceKind::from_id(&args.agent) else {
            self.report_background_error(format!("认不出历史来源「{}」", args.agent));
            return;
        };
        let launch_override = match profile_launch(&runtime, args.profile_id.as_deref(), cx) {
            Ok(launch) => launch,
            Err(error) => {
                self.report_background_error(error);
                return;
            }
        };
        self.resume_cli_session(
            runtime,
            agent,
            launch_override,
            args.cwd,
            args.resume_id,
            cx,
        );
    }

    pub(crate) fn run_handoff_command(
        &mut self,
        args: &serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(args) = self.command_args::<Handoff>(sessions::HANDOFF, args) else {
            return;
        };
        let machine = sessions::machine_key(&args.machine);
        let Some(agent) = ConversationAgentKind::from_id(&args.agent) else {
            self.report_background_error(format!("认不出 agent「{}」，交接取消", args.agent));
            return;
        };
        // 交接会话开在源存档所在的那台机器上，用那台机器的启动命令。
        let Some(runtime) = crate::runtimes::runtime(&machine, cx) else {
            self.report_background_error("这台机器已经移除，交接取消");
            return;
        };
        let launch = match profile_launch(&machine, args.profile_id.as_deref(), cx) {
            Ok(Some(launch)) => launch,
            Ok(None) => settings::acp_launch_for(&machine, agent, cx),
            Err(error) => {
                self.report_background_error(error);
                return;
            }
        };
        self.add_acp_handoff_session(handoff_request(runtime, agent, launch, args), window, cx);
    }

    /// 参数解析不了是插件写错了：报出来，不开会话。
    fn command_args<T: DeserializeOwned>(
        &mut self,
        command: &str,
        args: &serde_json::Value,
    ) -> Option<T> {
        match serde_json::from_value(args.clone()) {
            Ok(args) => Some(args),
            Err(error) => {
                self.report_background_error(format!("命令 {command} 的参数不对：{error}"));
                None
            }
        }
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
            machine: None,
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
