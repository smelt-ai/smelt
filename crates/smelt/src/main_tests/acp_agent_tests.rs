use super::{AcpSaved, acp_agent_from_cmd, cli_resume_command, command_matches_agent, shell_quote};
use crate::settings::{ConversationAgentKind, HistorySourceKind, TerminalAgentKind};
use crate::workspace_sessions::prepare_agent_definition_launch;
use smelt_core::agent_kind::SMELT_AGENT_INSTRUCTIONS_ENV;

#[test]
fn product_agent_instructions_are_pi_system_prompt_input_not_a_user_message() {
    let definition = crate::settings::AgentDefinition {
        id: "quant".into(),
        name: "量化智能体".into(),
        description: String::new(),
        engine_kind_id: ConversationAgentKind::Pi.id().into(),
        prompt: "  先验证行情时间\n再给出结论  ".into(),
        plugins: Vec::new(),
        context_folders: Vec::new(),
        context_links: Vec::new(),
        ..Default::default()
    };
    let launch = smelt_core::agent_kind::ConversationLaunchSpec::from_command("smelt-pi-agent")
        .with_env("EXISTING", "keep");

    let launch = prepare_agent_definition_launch(launch, Some(&definition));

    assert_eq!(launch.env.get("EXISTING").map(String::as_str), Some("keep"));
    assert_eq!(
        launch
            .env
            .get(SMELT_AGENT_INSTRUCTIONS_ENV)
            .map(String::as_str),
        Some("先验证行情时间\n再给出结论")
    );
    assert_eq!(launch.command, "smelt-pi-agent");
}

#[test]
fn automation_run_does_not_require_an_existing_conversation() {
    use crate::settings::{Automation, AutomationSchedule, AutomationTrigger};
    use smelt_core::automation::{AutomationCommand, AutomationFile, apply_automation_command};

    let automation = Automation {
        id: "open-scan".into(),
        name: "开盘扫描".into(),
        enabled: true,
        workspace_dir: Some("/tmp".into()),
        trigger: AutomationTrigger::schedule(AutomationSchedule::daily_morning()),
        action: smelt_core::automation::AutomationAction::Agent {
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
    assert_eq!(run.context.cwd, "/tmp");
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
        action: smelt_core::automation::AutomationAction::Agent {
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

/// 多 agent 之前的 ACP 存档没有 `agent` 字段：必须读得进来（None），
/// 不能整条会话解析失败——那等于用户重开 GUI 少一个会话。
#[test]
fn old_acp_archive_without_agent_field_still_loads() {
    let old = r#"{"cwd":"/tmp/x","cmd":"bunx --bun @agentclientprotocol/claude-agent-acp@0.59.0"}"#;
    let back: AcpSaved = serde_json::from_str(old).unwrap();
    assert!(back.agent.is_none(), "旧存档不该凭空冒出 agent 字段");
    assert!(
        back.config_values.is_empty(),
        "旧存档缺少 ACP 配置时应安全回退到 provider 默认值"
    );
    assert_eq!(
        acp_agent_from_cmd(&back.launch.command),
        ConversationAgentKind::Claude
    );
}

#[test]
fn acp_saved_round_trip_preserves_profile_and_launch_spec() {
    let agent_session: smelt_plugin_api::AgentSessionBinding =
        serde_json::from_value(serde_json::json!({
            "agent": {
                "plugin_id": "com.example.quant",
                "contribution_id": "quant-agent"
            },
            "controller": {
                "plugin_id": "com.example.quant",
                "contribution_id": "quant-session"
            },
            "instance": {
                "plugin_id": "com.example.quant",
                "resource_type": "strategy",
                "resource_id": "strategy-1"
            }
        }))
        .unwrap();
    let saved = AcpSaved {
        cwd: Some("/repo".into()),
        launch: smelt_core::agent_kind::ConversationLaunchSpec::from_command("claude")
            .with_env("CLAUDE_CONFIG_DIR", "~/Claude Workspaces/quant"),
        profile_id: Some("quant".into()),
        agent: Some("claude".into()),
        agent_definition_id: Some("reviewer".into()),
        history_session_id: Some(agent_client_protocol::schema::v1::SessionId::new(
            "canonical-history",
        )),
        sid: Some("acp-1".into()),
        refresh_launch_from_settings: false,
        fork_origin: None,
        conversation_binding: smelt_core::conversation::ConversationBinding::Plugin {
            plugin_id: smelt_plugin_api::PluginId::new("com.example.chat").unwrap(),
            route: smelt_plugin_api::PluginInputRouteBinding {
                contribution_id: smelt_plugin_api::ContributionId::new("thread-input").unwrap(),
                context: serde_json::json!({"thread_id": "thread-1"}),
            },
        },
        agent_session: Some(agent_session.clone()),
        config_values: vec![
            ("model".into(), "claude-sonnet-4-6".into()),
            ("effort".into(), "high".into()),
            ("mode".into(), "bypassPermissions".into()),
        ],
        pending_prompt: Some("待发送首包".into()),
        pending_delivery_id: Some("run-1".into()),
        pending_agent_preset: Some("先检查正确性".into()),
        automation_id: None,
        session_title: Some("季度复盘".into()),
    };

    let value = serde_json::to_value(&saved).unwrap();
    assert!(value.get("cmd").is_none(), "新存档不该再写旧 cmd 字段");
    assert!(
        value.get("entries").is_none(),
        "agent transcript 是历史唯一来源，新存档不该再写 ACP entries"
    );
    assert_eq!(value["history_session_id"], "canonical-history");
    assert!(value.get("resume_session_id").is_none());
    let restored: AcpSaved = serde_json::from_value(value).unwrap();

    assert_eq!(restored.profile_id.as_deref(), Some("quant"));
    assert_eq!(restored.agent_definition_id.as_deref(), Some("reviewer"));
    assert_eq!(
        restored.history_session_id.as_ref().map(|id| id.0.as_ref()),
        Some("canonical-history")
    );
    assert_eq!(
        restored
            .launch
            .env
            .get("CLAUDE_CONFIG_DIR")
            .map(String::as_str),
        Some("~/Claude Workspaces/quant")
    );
    assert_eq!(restored.pending_prompt.as_deref(), Some("待发送首包"));
    assert!(restored.automation_id.is_none());
    // 标题必须同时出现在 AcpSaved 和 AcpSavedWire 上，少一处就会在存档往返里
    // 悄悄丢掉，重启后同一智能体的多段对话又会变成一个名字。
    assert_eq!(restored.session_title.as_deref(), Some("季度复盘"));
    assert_eq!(
        restored.pending_agent_preset.as_deref(),
        Some("先检查正确性")
    );
    assert_eq!(
        restored.config_values,
        vec![
            ("model".to_string(), "claude-sonnet-4-6".to_string()),
            ("effort".to_string(), "high".to_string()),
            ("mode".to_string(), "bypassPermissions".to_string()),
        ]
    );
    assert_eq!(
        restored.conversation_binding,
        smelt_core::conversation::ConversationBinding::Plugin {
            plugin_id: smelt_plugin_api::PluginId::new("com.example.chat").unwrap(),
            route: smelt_plugin_api::PluginInputRouteBinding {
                contribution_id: smelt_plugin_api::ContributionId::new("thread-input").unwrap(),
                context: serde_json::json!({"thread_id": "thread-1"}),
            },
        }
    );
    assert_eq!(restored.agent_session, Some(agent_session));
}

#[test]
fn legacy_resume_session_id_migrates_to_history_session_id() {
    let restored: AcpSaved = serde_json::from_value(serde_json::json!({
        "cwd": "/repo",
        "launch": { "command": "claude", "env": {} },
        "agent": "claude",
        "resume_session_id": "legacy-canonical"
    }))
    .unwrap();

    assert_eq!(
        restored.history_session_id.as_ref().map(|id| id.0.as_ref()),
        Some("legacy-canonical")
    );
    let migrated = serde_json::to_value(restored).unwrap();
    assert_eq!(migrated["history_session_id"], "legacy-canonical");
    assert!(migrated.get("resume_session_id").is_none());
}

#[test]
fn legacy_entries_are_readable_but_not_written_back() {
    let legacy: AcpSaved = serde_json::from_value(serde_json::json!({
        "cwd": "/repo",
        "launch": { "command": "claude", "env": {} },
        "agent": "claude",
        "entries": [
            { "User": "old question" },
            { "Assistant": { "text": "old answer", "thought": false } }
        ]
    }))
    .expect("旧 entries 字段不能让整个 ACP 会话存档解析失败");

    let migrated = serde_json::to_value(&legacy).unwrap();
    assert!(
        migrated.get("entries").is_none(),
        "读取旧存档后必须停止写回本地历史副本"
    );
}

#[test]
fn legacy_acp_saved_cmd_deserializes_into_launch_spec() {
    let restored: AcpSaved = serde_json::from_value(serde_json::json!({
        "cwd": "/repo",
        "cmd": "claude --flag",
        "agent": "claude"
    }))
    .unwrap();

    assert_eq!(restored.launch.command, "claude --flag");
    assert!(restored.profile_id.is_none());
}

#[test]
fn legacy_cmd_archive_keeps_saved_launch_on_restart() {
    let restored: AcpSaved = serde_json::from_value(serde_json::json!({
        "cwd": "/repo",
        "cmd": "CLAUDE_CONFIG_DIR=~/Claude Workspaces/quant claude",
        "agent": "claude"
    }))
    .unwrap();

    assert!(
        !restored.refresh_launch_from_settings(),
        "旧 cmd 存档缺少 profile_id 时也不能被当成普通会话刷新成当前默认命令"
    );
}

#[test]
fn structured_launch_without_profile_id_refreshes_from_settings() {
    let restored: AcpSaved = serde_json::from_value(serde_json::json!({
        "cwd": "/repo",
        "launch": { "command": "claude", "env": {} },
        "agent": "claude"
    }))
    .unwrap();

    assert!(
        restored.refresh_launch_from_settings(),
        "新存档的普通会话仍应沿用按当前设置刷新的行为"
    );
}

#[test]
fn legacy_cmd_archive_round_trip_preserves_restart_refresh_behavior() {
    let legacy: AcpSaved = serde_json::from_value(serde_json::json!({
        "cwd": "/repo",
        "cmd": "CLAUDE_CONFIG_DIR=~/Claude Workspaces/quant claude",
        "agent": "claude"
    }))
    .unwrap();

    let value = serde_json::to_value(&legacy).unwrap();
    let restored: AcpSaved = serde_json::from_value(value).unwrap();

    assert!(
        !restored.refresh_launch_from_settings(),
        "旧 cmd 存档升级成新格式后也必须继续保留原 launch，不得在下一次重启时退化成按默认设置刷新"
    );
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

/// dsh 没有可在终端里继续的 CLI，它只有 ACP 桥。这里必须是 None——拼出一条
/// 别家的命令会把用户带进一个完全无关的 agent。
#[test]
fn cli_resume_command_has_no_answer_for_an_acp_only_agent() {
    assert_eq!(
        cli_resume_command(ConversationAgentKind::Dsh.into(), "", "history-1"),
        None
    );
    assert!(!command_matches_agent(
        "dsh",
        ConversationAgentKind::Dsh.into()
    ));
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
