//! 「新建会话」的启动动作目录：桌面弹层与移动端选择器共用这一份。
//!
//! 目录里只有具体动作：常用、终端、对话。「终端 / 对话」是动作的属性，不是
//! 一层模式切换；Pin（常用）绑定到动作本身，所以同一个 Agent 的对话和终端可以
//! 分别置顶。两端都读同一份 SQLite 偏好与同一套 key，桌面 Pin 完手机立刻同序。
//!
//! 这里只负责「有哪些动作、叫什么、怎么分组」。真正怎么把动作跑起来由各端自己
//! 决定：桌面走 GUI 的会话工作区，远程客户端走运行时的 `session.create`。

pub use smelt_model::new_session::*;

use crate::acp_conn::AcpRuntimeDiagnostics;
use crate::session_control::AcpAgentOption;

/// 落盘的启动项（无 GUI 进程也能读）。库里没有这份快照时退回出厂默认，
/// 否则守护侧会在用户第一次改设置之前把终端分组渲染成空的。
pub fn stored_launch_entries() -> Vec<LaunchEntry> {
    crate::sqlite_state::default_sqlite_store()
        .map(|store| stored_launch_entries_on(&store))
        .unwrap_or_else(|_| default_launch_entries())
}

fn stored_launch_entries_on(store: &smelt_store::Store) -> Vec<LaunchEntry> {
    crate::launch_settings::load_on(store).entries
}

fn conversation_options_with(
    snapshot: Option<&smelt_store::AgentUiSnapshot>,
) -> Vec<AcpAgentOption> {
    let mut options = crate::session_control::agent_definition_options_with(snapshot);
    options.extend(crate::session_control::agent_options_with(snapshot));
    options
}

fn load_preferences_on(store: &smelt_store::Store) -> NewSessionPreferences {
    match store.new_session_pins() {
        Ok(pinned) => NewSessionPreferences { pinned },
        Err(error) => {
            eprintln!("[new-session] 读取 Pin 偏好失败，使用默认值: {error}");
            NewSessionPreferences::default()
        }
    }
}

pub fn load_preferences() -> NewSessionPreferences {
    match crate::sqlite_state::default_sqlite_store() {
        Ok(store) => load_preferences_on(&store),
        Err(error) => {
            eprintln!("[new-session] 读取 Pin 偏好失败，使用默认值: {error}");
            NewSessionPreferences::default()
        }
    }
}

/// 切换一个动作的「常用」标记，返回切换后的偏好。偏好存在运行时：桌面和手机的新建
/// 菜单读的是同一份。
pub fn toggle_pin(params: PinParams) -> NewSessionPreferences {
    let Ok(store) = crate::sqlite_state::default_sqlite_store() else {
        let mut preferences = NewSessionPreferences::default();
        preferences.toggle(&params.key);
        return preferences;
    };
    let mut preferences = load_preferences_on(&store);
    preferences.toggle(&params.key);
    if let Err(error) = store.put_new_session_pins(&preferences.pinned) {
        eprintln!("[new-session] 保存 Pin 偏好失败: {error}");
    }
    preferences
}

/// 在给定 `store` 上一次性读完 UI 快照（智能体定义 + env + profile）、启动项与 Pin 偏好，
/// 拼出完整的对话选项与新建目录分段（只拿一次 SQLite 连接锁）。
pub fn stored_catalog_on(
    store: &smelt_store::Store,
    diagnostics: Option<&AcpRuntimeDiagnostics>,
) -> (Vec<AcpAgentOption>, NewSessionSections) {
    let snapshot = match store.get_agent_ui_snapshot() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("[new-session] 读取智能体快照失败，使用默认值: {error}");
            None
        }
    };
    let agents = conversation_options_with(snapshot.as_ref());
    let entries = stored_launch_entries_on(store);
    let preferences = load_preferences_on(store);
    let actions = new_session_actions(&agents, &entries, diagnostics);
    let sections = build_sections(&actions, &preferences);
    (agents, sections)
}

/// 同 [`stored_catalog_on`]，使用当前进程默认 SQLite 库；测试未启用 SQLite 时退回出厂默认。
pub fn stored_catalog(
    diagnostics: Option<&AcpRuntimeDiagnostics>,
) -> (Vec<AcpAgentOption>, NewSessionSections) {
    if let Ok(store) = crate::sqlite_state::default_sqlite_store() {
        return stored_catalog_on(&store, diagnostics);
    }
    let agents = conversation_options_with(None);
    let actions = new_session_actions(&agents, &default_launch_entries(), diagnostics);
    let sections = build_sections(&actions, &NewSessionPreferences::default());
    (agents, sections)
}

/// 按 key 一次读库解析出启动动作与对应的对话 Agent 选项（若目标是对话）。
///
/// 客户端只回传 key，命令永远在这台机器上解析——别让远程设备决定跑什么进程。
/// 探测状态不参与解析：目录里还在的启动项就能起，免得打开菜单后 CLI 被挪走，
/// 让刚点下的那一行变成「未知启动项」。
pub fn resolve_stored_action(key: &str) -> Option<(NewSessionAction, Option<AcpAgentOption>)> {
    let (agents, entries) = match crate::sqlite_state::default_sqlite_store() {
        Ok(store) => {
            let snapshot = store.get_agent_ui_snapshot().ok().flatten();
            (
                conversation_options_with(snapshot.as_ref()),
                stored_launch_entries_on(&store),
            )
        }
        Err(_) => (conversation_options_with(None), default_launch_entries()),
    };
    resolve_action_from(&agents, &entries, key)
}

fn resolve_action_from(
    agents: &[AcpAgentOption],
    entries: &[LaunchEntry],
    key: &str,
) -> Option<(NewSessionAction, Option<AcpAgentOption>)> {
    let actions = new_session_actions(agents, entries, None);
    let action = find_action(&actions, key)?.clone();
    let option = match &action.target {
        NewSessionTarget::Conversation {
            agent_option_id, ..
        } => agents
            .iter()
            .find(|option| option.id == *agent_option_id)
            .cloned(),
        NewSessionTarget::Terminal { .. } | NewSessionTarget::BlankTerminal => None,
    };
    Some((action, option))
}

/// 按 key 找一条启动动作（仅返回动作元数据）。
pub fn find_stored_action(key: &str) -> Option<NewSessionAction> {
    resolve_stored_action(key).map(|(action, _)| action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_kind::ConversationLaunchSpec;
    use crate::agent_kind::{ConversationAgentKind, TerminalAgentKind};

    #[test]
    fn stored_launch_entries_add_new_builtins_to_existing_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = smelt_store::Store::open_or_create(dir.path().join("smelt.db")).unwrap();
        store
            .put_launch_snapshot(&smelt_store::LaunchSnapshot {
                version: crate::launch_settings::LAUNCH_CONFIG_VERSION,
                entries: vec![smelt_store::LaunchEntryRecord {
                    label: "My shell".into(),
                    command: "zsh".into(),
                    provider: None,
                }],
            })
            .unwrap();
        let entries = stored_launch_entries_on(&store);
        assert!(entries.iter().any(|entry| entry.command == "zsh"));
        assert!(
            entries
                .iter()
                .any(|entry| entry.provider.as_deref() == Some("omp"))
        );
    }

    fn option(id: &str, kind: ConversationAgentKind, label: &str) -> AcpAgentOption {
        AcpAgentOption {
            id: id.into(),
            kind: kind.id().into(),
            label: label.into(),
            profile: id.starts_with("profile:"),
            agent_definition_id: id
                .strip_prefix("agent:")
                .map(str::to_string)
                .filter(|id| !id.is_empty()),
            launch: ConversationLaunchSpec::from_command(kind.default_cmd()),
            history_dir: None,
        }
    }

    fn entry(label: &str, command: &str, provider: &str) -> LaunchEntry {
        LaunchEntry {
            label: label.into(),
            command: command.into(),
            provider: Some(provider.into()),
        }
    }

    fn labels(actions: &[NewSessionAction]) -> Vec<String> {
        actions
            .iter()
            .map(|action| format!("{} · {}", action.label, action.kind.label()))
            .collect()
    }

    #[test]
    fn conversation_and_terminal_are_separate_actions_for_the_same_agent() {
        let actions = new_session_actions(
            &[option(
                "claude",
                ConversationAgentKind::Claude,
                "Claude Code",
            )],
            &[entry(
                "Claude Code",
                "claude --dangerously-skip-permissions",
                "claude",
            )],
            None,
        );
        assert_eq!(
            labels(&actions),
            vec![
                "Claude Code · Terminal".to_string(),
                "Shell · Terminal".to_string(),
                "Claude Code · Conversation".to_string(),
            ]
        );
        assert_ne!(actions[0].key, actions[2].key);
    }

    #[test]
    fn custom_launch_entries_sort_after_the_registry_order() {
        let actions = new_session_actions(
            &[],
            &[
                entry("我的脚本", "./run.sh", ""),
                entry(
                    "Antigravity",
                    "agy --dangerously-skip-permissions",
                    "antigravity",
                ),
                entry(
                    "Claude Code",
                    "claude --dangerously-skip-permissions",
                    "claude",
                ),
            ],
            None,
        );
        assert_eq!(
            actions
                .iter()
                .map(|action| action.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Claude Code", "Antigravity", "我的脚本", "Shell"]
        );
    }

    #[test]
    fn blank_and_empty_entries_never_collapse_into_each_other() {
        let actions = new_session_actions(
            &[],
            &[
                entry("", "claude", "claude"),
                entry("没有命令", "  ", "claude"),
            ],
            None,
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].key, BLANK_TERMINAL_KEY);
    }

    #[test]
    fn user_agents_with_the_same_engine_stay_distinct() {
        let actions = new_session_actions(
            &[
                option("agent:quant", ConversationAgentKind::Pi, "量化智能体"),
                option("agent:lark", ConversationAgentKind::Pi, "飞书助理"),
                option("pi", ConversationAgentKind::Pi, "Pi"),
                option("profile:work", ConversationAgentKind::Pi, "工作区 Pi"),
            ],
            &[],
            None,
        );
        let keys: Vec<_> = actions
            .iter()
            .filter(|action| action.kind == NewSessionActionKind::Conversation)
            .map(|action| action.key.as_str())
            .collect();
        assert_eq!(
            keys,
            vec![
                "conversation:agent:quant",
                "conversation:agent:lark",
                "conversation:pi",
                "conversation:pi:profile:work",
            ]
        );
    }

    #[test]
    fn pinned_actions_move_out_of_their_kind_section_and_keep_pin_order() {
        let actions = new_session_actions(
            &[
                option("claude", ConversationAgentKind::Claude, "Claude Code"),
                option("grok", ConversationAgentKind::Grok, "Grok"),
            ],
            &[entry("Claude Code", "claude", "claude")],
            None,
        );
        let preferences = NewSessionPreferences {
            pinned: vec![
                "missing".into(),
                "conversation:grok".into(),
                BLANK_TERMINAL_KEY.into(),
            ],
        };
        let sections = build_sections(&actions, &preferences);
        assert_eq!(
            sections
                .common
                .iter()
                .map(|action| action.key.as_str())
                .collect::<Vec<_>>(),
            vec!["conversation:grok", BLANK_TERMINAL_KEY]
        );
        assert!(sections.common.iter().all(|action| action.pinned));
        assert!(
            sections
                .terminal
                .iter()
                .all(|action| action.key != BLANK_TERMINAL_KEY)
        );
        assert!(
            sections
                .conversation
                .iter()
                .all(|action| action.key != "conversation:grok")
        );
    }

    #[test]
    fn undetected_agents_and_clis_drop_out_of_the_catalog() {
        use crate::acp_conn::RuntimeExecutable;
        let mut diagnostics = AcpRuntimeDiagnostics::default();
        diagnostics.record_terminal(
            TerminalAgentKind::Claude,
            RuntimeExecutable {
                program: "claude".into(),
                path: Some("/usr/bin/claude".into()),
                ..Default::default()
            },
        );
        diagnostics.record_terminal(TerminalAgentKind::Grok, RuntimeExecutable::default());
        let actions = new_session_actions(
            &[
                option("claude", ConversationAgentKind::Claude, "Claude Code"),
                option("grok", ConversationAgentKind::Grok, "Grok"),
            ],
            &[
                entry("Claude Code", "claude", "claude"),
                entry("Grok", "grok", "grok"),
                entry("我的脚本", "./run.sh", ""),
            ],
            Some(&diagnostics),
        );
        assert_eq!(
            actions
                .iter()
                .map(|action| action.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Claude Code", "我的脚本", "Shell", "Claude Code"]
        );
    }

    /// 偏好按行存在 SQLite 里，不再从 JSON 文件读；JSON 只出现在 `new_session.pins` 的回包里。
    /// 回包宽容读取：比客户端新的运行时多带的字段忽略，不能让新建菜单读不出 Pin。
    #[test]
    fn preferences_from_a_newer_runtime_still_read() {
        let newer = serde_json::json!({
            "pinned": ["conversation:claude"],
            "some_future_field": 1
        });
        let preferences = serde_json::from_value::<NewSessionPreferences>(newer).unwrap();
        assert_eq!(preferences.pinned, ["conversation:claude"]);
    }

    #[test]
    fn actions_serialize_with_a_flat_target_for_the_phone() {
        let actions = new_session_actions(
            &[option(
                "agent:quant",
                ConversationAgentKind::Pi,
                "量化智能体",
            )],
            &[entry("Claude Code", "claude", "claude")],
            None,
        );
        let json = serde_json::to_value(&actions).unwrap();
        assert_eq!(json[0]["target"], "terminal");
        assert_eq!(json[0]["kind"], "terminal");
        assert_eq!(json[0]["command"], "claude");
        assert_eq!(json[1]["target"], "blankTerminal");
        assert_eq!(json[2]["target"], "conversation");
        assert_eq!(json[2]["agentOptionId"], "agent:quant");
        assert_eq!(json[2]["agentKind"], "pi");
    }
}

#[cfg(test)]
mod runtime_detection_tests {
    use super::{acp_agent_detected, launch_entry_detected, terminal_agent_detected};
    use crate::acp_conn::{AcpRuntimeDiagnostics, RuntimeExecutable};
    use crate::agent_kind::{ConversationAgentKind, TerminalAgentKind};

    fn available(program: &str) -> RuntimeExecutable {
        RuntimeExecutable {
            program: program.to_string(),
            path: Some(format!("/usr/bin/{program}")),
            version: Some("1.0.0".into()),
            error: None,
        }
    }

    #[test]
    fn unprobed_runtime_shows_every_builtin_agent() {
        for kind in TerminalAgentKind::ALL {
            assert!(
                terminal_agent_detected(kind, None),
                "{} 在探测完成前应先显示",
                kind.id()
            );
        }
        for kind in ConversationAgentKind::ALL {
            assert!(acp_agent_detected(kind, None));
        }
        assert!(launch_entry_detected(
            "claude --dangerously-skip-permissions",
            None
        ));
    }

    #[test]
    fn built_in_pi_dialog_runtime_is_independent_of_pi_terminal_cli() {
        let mut diagnostics = AcpRuntimeDiagnostics::default();
        diagnostics.record_agent(ConversationAgentKind::Pi, available("smelt-pi-agent"));

        assert!(acp_agent_detected(
            ConversationAgentKind::Pi,
            Some(&diagnostics)
        ));
        assert!(
            !terminal_agent_detected(TerminalAgentKind::Pi, Some(&diagnostics)),
            "内置 Pi 对话 runtime 不应伪装成用户已安装 Pi 终端 CLI"
        );
    }

    #[test]
    fn probed_runtime_only_shows_installed_clis() {
        let mut diagnostics = AcpRuntimeDiagnostics::default();
        diagnostics.record_terminal(TerminalAgentKind::Grok, available("grok"));
        diagnostics.record_terminal(TerminalAgentKind::Cursor, available("cursor-agent"));

        assert!(acp_agent_detected(
            ConversationAgentKind::Grok,
            Some(&diagnostics)
        ));
        assert!(acp_agent_detected(
            ConversationAgentKind::Cursor,
            Some(&diagnostics)
        ));
        assert!(!acp_agent_detected(
            ConversationAgentKind::Claude,
            Some(&diagnostics)
        ));
        assert!(
            acp_agent_detected(ConversationAgentKind::Custom, Some(&diagnostics)),
            "通用 ACP 没有可探测的 CLI，配了就显示"
        );
        assert!(!terminal_agent_detected(
            TerminalAgentKind::Antigravity,
            Some(&diagnostics)
        ));
        assert!(launch_entry_detected(
            "grok --always-approve",
            Some(&diagnostics)
        ));
        assert!(!launch_entry_detected(
            "claude --dangerously-skip-permissions",
            Some(&diagnostics)
        ));
        assert!(
            launch_entry_detected("zsh -l", Some(&diagnostics)),
            "对不上内置 agent 的自定义命令应始终显示"
        );
    }

    #[test]
    fn stored_catalog_on_reads_agents_launches_and_pins_from_one_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = smelt_store::Store::open_or_create(dir.path().join("smelt.db")).unwrap();
        store
            .insert_agent_definition(&smelt_store::AgentDefinitionRecord {
                id: "assistant".into(),
                name: "开发助理".into(),
                description: String::new(),
                engine_kind_id: "pi".into(),
                prompt: "Be helpful".into(),
                plugins_json: b"[]".to_vec(),
                mcp_servers_json: b"[]".to_vec(),
                context_folders_json: b"[]".to_vec(),
                context_links_json: b"[]".to_vec(),
                model_provider: String::new(),
                model_id: String::new(),
                memory_enabled: true,
            })
            .unwrap();
        store
            .put_launch_snapshot(&smelt_store::LaunchSnapshot {
                version: crate::launch_settings::LAUNCH_CONFIG_VERSION,
                entries: vec![smelt_store::LaunchEntryRecord {
                    label: "Shell".into(),
                    command: "zsh".into(),
                    provider: None,
                }],
            })
            .unwrap();
        store
            .put_new_session_pins(&["conversation:agent:assistant".into()])
            .unwrap();

        let (agents, sections) = super::stored_catalog_on(&store, None);
        assert!(agents.iter().any(|a| a.id == "agent:assistant"));
        assert_eq!(
            sections
                .common
                .iter()
                .map(|a| a.key.as_str())
                .collect::<Vec<_>>(),
            vec!["conversation:agent:assistant"]
        );
        assert!(
            sections
                .conversation
                .iter()
                .all(|a| a.key != "conversation:agent:assistant")
        );
        assert!(
            sections
                .terminal
                .iter()
                .any(|a| a.key == super::BLANK_TERMINAL_KEY)
        );

        // 单次解析同时产出 launch action 与对应的 AcpAgentOption。
        let entries = [super::LaunchEntry {
            label: "Shell".into(),
            command: "zsh".into(),
            provider: None,
        }];
        let (action, option) =
            super::resolve_action_from(&agents, &entries, "conversation:agent:assistant")
                .expect("should resolve action and option from same snapshot");
        assert_eq!(action.key, "conversation:agent:assistant");
        assert_eq!(option.expect("agent option").id, "agent:assistant");
    }
}
