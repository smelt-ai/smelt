use super::{acp_agent_from_cmd, cli_resume_command, command_matches_agent, shell_quote};
use crate::settings::{ConversationAgentKind, HistorySourceKind, TerminalAgentKind};

#[test]
fn automation_run_does_not_require_an_existing_conversation() {
    use crate::settings::{Automation, AutomationSchedule, AutomationTrigger};
    use smelt_model::automation::{AutomationCommand, AutomationFile, apply_automation_command};

    let workspace = std::env::temp_dir().to_string_lossy().into_owned();
    let automation = Automation {
        id: "open-scan".into(),
        name: "开盘扫描".into(),
        enabled: true,
        workspace_dir: Some(workspace.clone()),
        trigger: AutomationTrigger::schedule(AutomationSchedule::daily_morning()),
        action: smelt_model::automation::AutomationAction::Agent {
            agent_definition_id: "quant".into(),
            prompt: Some("拉取行情并输出".into()),
        },
        sinks: Vec::new(),
    };
    let mut file = AutomationFile::default();
    apply_automation_command(
        &mut file,
        AutomationCommand::Upsert {
            automation: Box::new(automation),
        },
        chrono::Utc::now(),
    )
    .unwrap();

    let run = apply_automation_command(
        &mut file,
        AutomationCommand::RunOnce {
            automation_id: "open-scan".into(),
        },
        chrono::Utc::now(),
    )
    .unwrap()
    .run()
    .unwrap();

    assert_eq!(run.context.prompt.as_deref(), Some("拉取行情并输出"));
    assert_eq!(run.context.action.agent_definition_id(), Some("quant"));
    assert_eq!(run.context.cwd, workspace);
    assert_eq!(run.context.automation_name, "开盘扫描");
}

#[test]
fn automation_definition_does_not_require_a_user_selected_workspace() {
    use crate::settings::{Automation, AutomationSchedule, AutomationTrigger};

    let definition = crate::settings::AgentDefinition {
        id: "quant".into(),
        name: "量化智能体".into(),
        description: String::new(),
        engine_kind_id: ConversationAgentKind::Pi.id().into(),
        prompt: "分析行情".into(),
        plugins: Vec::new(),
        context_folders: Vec::new(),
        context_links: Vec::new(),
        ..Default::default()
    };
    let automation = Automation {
        id: "open-scan".into(),
        name: "开盘扫描".into(),
        enabled: true,
        workspace_dir: None,
        trigger: AutomationTrigger::schedule(AutomationSchedule::daily_morning()),
        action: smelt_model::automation::AutomationAction::Agent {
            agent_definition_id: "quant".into(),
            prompt: Some("拉取行情并输出".into()),
        },
        sinks: Vec::new(),
    };

    assert!(automation.is_ready());
    assert!(automation.validate().is_ok());
    assert!(automation.validate_execution_context().is_err());
    assert_eq!(definition.prompt, "分析行情");
}

/// 旧存档反推：命令里带已知 agent 标识的归给对应 agent，其余当 Claude。
#[test]
fn agent_inferred_from_legacy_cmd() {
    assert_eq!(
        acp_agent_from_cmd("copilot --acp"),
        ConversationAgentKind::Copilot
    );
    assert_eq!(
        acp_agent_from_cmd("bunx --bun @zed-industries/codex-acp"),
        ConversationAgentKind::Codex
    );
    assert_eq!(
        acp_agent_from_cmd("bunx --bun @agentclientprotocol/codex-acp"),
        ConversationAgentKind::Codex
    );
    assert_eq!(
        acp_agent_from_cmd("bunx --bun pi-acp@0.0.33"),
        ConversationAgentKind::Pi
    );
    assert_eq!(
        acp_agent_from_cmd("some-other-agent"),
        ConversationAgentKind::Claude
    );
}

/// 存档标识必须往返得回来（改了 id 就等于把用户的会话认成别家 agent）。
#[test]
fn agent_id_roundtrips() {
    for a in ConversationAgentKind::ALL {
        assert_eq!(ConversationAgentKind::from_id(a.id()), Some(a));
    }
    assert_eq!(ConversationAgentKind::from_id("gemini"), None);
}

#[test]
fn cli_resume_commands_use_each_agents_tui_syntax() {
    assert_eq!(
        cli_resume_command(
            ConversationAgentKind::Claude.into(),
            "claude --allow-all",
            "history-1"
        ),
        Some("claude --allow-all --resume 'history-1'".to_string())
    );
    assert_eq!(
        cli_resume_command(
            ConversationAgentKind::Copilot.into(),
            "copilot --allow-all",
            "history-1"
        ),
        Some("copilot --allow-all --resume='history-1'".to_string())
    );
    assert_eq!(
        cli_resume_command(
            ConversationAgentKind::Codex.into(),
            "codex --full-auto",
            "history-1"
        ),
        Some("codex --full-auto resume 'history-1'".to_string())
    );
    assert_eq!(
        cli_resume_command(
            ConversationAgentKind::Grok.into(),
            "grok --always-approve",
            "history-1"
        ),
        Some("grok --always-approve --resume 'history-1'".to_string())
    );
    assert_eq!(
        cli_resume_command(
            ConversationAgentKind::Cursor.into(),
            "cursor-agent --force",
            "history-1"
        ),
        Some("cursor-agent --force --resume 'history-1'".to_string())
    );
    assert_eq!(
        cli_resume_command(
            ConversationAgentKind::OpenCode.into(),
            "opencode --auto",
            "history-1"
        ),
        Some("opencode --auto --session 'history-1'".to_string())
    );
    assert_eq!(
        cli_resume_command(
            ConversationAgentKind::Kiro.into(),
            "kiro-cli chat --v3 --trust-all-tools",
            "history-1"
        ),
        Some("kiro-cli chat --v3 --trust-all-tools --resume-id 'history-1'".to_string())
    );
    assert_eq!(
        cli_resume_command(ConversationAgentKind::Pi.into(), "pi", "history-1"),
        Some("pi --session 'history-1'".to_string())
    );
}

#[test]
fn cli_resume_command_quotes_shell_arguments() {
    assert_eq!(shell_quote("a session's id"), "'a session'\\''s id'");
    assert_eq!(
        cli_resume_command(
            ConversationAgentKind::Claude.into(),
            "claude",
            "a session's id"
        ),
        Some("claude --resume 'a session'\\''s id'".to_string())
    );
}

/// 只有 TUI 的 agent 照样能在终端里续接——这正是它唯一的续接路径，不能因为
/// 它没有 ACP 就连 CLI 续接一起丢掉。实测 `agy --help`：`--conversation <id>`。
#[test]
fn cli_resume_command_works_for_a_terminal_only_agent() {
    let antigravity = HistorySourceKind::TerminalOnly(TerminalAgentKind::Antigravity);
    assert_eq!(
        cli_resume_command(antigravity, "agy --dangerously-skip-permissions", "conv-1"),
        Some("agy --dangerously-skip-permissions --conversation 'conv-1'".to_string())
    );
    // 起续接命令时要能认出它自己的启动项，否则会回退到凭空拼的命令。
    assert!(command_matches_agent("agy", antigravity));
}

#[test]
fn cli_command_matching_skips_environment_prefixes() {
    assert!(command_matches_agent(
        "CLAUDE_CONFIG_DIR='/tmp/claude' claude --allow-all",
        ConversationAgentKind::Claude.into()
    ));
    assert!(command_matches_agent(
        "env COPILOT_HOME=/tmp/copilot copilot --allow-all",
        ConversationAgentKind::Copilot.into()
    ));
    assert!(!command_matches_agent(
        "claude --allow-all",
        ConversationAgentKind::Codex.into()
    ));
    assert!(command_matches_agent(
        "cursor-agent acp",
        ConversationAgentKind::Cursor.into()
    ));
    assert!(command_matches_agent(
        "OPENCODE_CONFIG_DIR=/tmp/oc opencode --auto",
        ConversationAgentKind::OpenCode.into()
    ));
    assert!(command_matches_agent(
        "kiro-cli acp --trust-all-tools",
        ConversationAgentKind::Kiro.into()
    ));
    assert!(command_matches_agent(
        "pi",
        ConversationAgentKind::Pi.into()
    ));
}
