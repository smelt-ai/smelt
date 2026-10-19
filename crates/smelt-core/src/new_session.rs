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
    let snapshot = crate::sqlite_state::default_sqlite_store()
        .ok()
        .and_then(|store| store.get_launch_snapshot().ok().flatten());
    let Some(snapshot) = snapshot else {
        return default_launch_entries();
    };
    snapshot
        .entries
        .into_iter()
        .map(|entry| LaunchEntry {
            label: entry.label,
            command: entry.command,
            provider: entry.provider,
        })
        .collect()
}

pub fn load_preferences() -> NewSessionPreferences {
    match crate::sqlite_state::default_sqlite_store()
        .and_then(|store| store.new_session_pins().map_err(|error| error.to_string()))
    {
        Ok(pinned) => NewSessionPreferences { pinned },
        Err(error) => {
            eprintln!("[new-session] 读取 Pin 偏好失败，使用默认值: {error}");
            NewSessionPreferences::default()
        }
    }
}

/// 切换一个动作的「常用」标记，返回切换后的偏好。偏好存在运行时：桌面和手机的新建
/// 菜单读的是同一份。
pub fn toggle_pin(params: PinParams) -> NewSessionPreferences {
    let mut preferences = load_preferences();
    preferences.toggle(&params.key);
    save_preferences(&preferences);
    preferences
}

fn save_preferences(preferences: &NewSessionPreferences) {
    if let Err(error) = crate::sqlite_state::default_sqlite_store().and_then(|store| {
        store
            .put_new_session_pins(&preferences.pinned)
            .map_err(|error| error.to_string())
    }) {
        eprintln!("[new-session] 保存 Pin 偏好失败: {error}");
    }
}

/// 无 GUI 进程时的对话入口清单：产品智能体在前，裸引擎与 profile 在后——与桌面
/// 新建菜单同序，别让同一份清单在两端读出不同的推荐顺序。
pub fn stored_conversation_options() -> Vec<AcpAgentOption> {
    let mut options = crate::session_control::agent_definition_options();
    options.extend(crate::session_control::agent_options());
    options
}

/// 守护侧的完整目录：读同一份落盘配置与同一份 Pin 偏好。`conversation_options`
/// 由调用方读一次传进来（[`stored_conversation_options`]），和它一起下发的选项清单
/// 才是同一份快照。
pub fn stored_new_session_sections(
    conversation_options: &[AcpAgentOption],
    diagnostics: Option<&AcpRuntimeDiagnostics>,
) -> NewSessionSections {
    let actions = new_session_actions(conversation_options, &stored_launch_entries(), diagnostics);
    build_sections(&actions, &load_preferences())
}

/// 按 key 找一条启动动作。客户端只回传 key，命令永远在这台机器上解析——别让远程
/// 设备决定跑什么进程。探测状态不参与解析：目录里还在的启动项就能起，免得探测缓存
/// 过期让刚点下的那一行变成「未知启动项」。
pub fn find_stored_action(key: &str) -> Option<NewSessionAction> {
    let actions = new_session_actions(
        &stored_conversation_options(),
        &stored_launch_entries(),
        None,
    );
    find_action(&actions, key).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_kind::ConversationLaunchSpec;
    use crate::agent_kind::{ConversationAgentKind, TerminalAgentKind};

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
                "Claude Code · 终端".to_string(),
                "空白终端 · 终端".to_string(),
                "Claude Code · 对话".to_string(),
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
            vec!["Claude Code", "Antigravity", "我的脚本", "空白终端"]
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
            vec!["Claude Code", "我的脚本", "空白终端", "Claude Code"]
        );
    }

    #[test]
    fn preferences_do_not_accept_the_legacy_versioned_shape() {
        let legacy = serde_json::json!({
            "version": 1,
            "pinned": ["conversation:claude"]
        });
        assert!(serde_json::from_value::<NewSessionPreferences>(legacy).is_err());
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

    /// dsh 没有 CLI，但有一个确定的入口文件——探测结果必须能说"没搭好"。
    ///
    /// 这里曾经无条件返回 true：设置页显示就绪，启动却停在"启动中"不动，
    /// 真正的原因（运行时目录不存在）一个字也到不了用户面前。
    #[test]
    fn dsh_reports_missing_managed_runtime() {
        let mut diagnostics = AcpRuntimeDiagnostics::default();
        assert!(
            !acp_agent_detected(ConversationAgentKind::Dsh, Some(&diagnostics)),
            "受管运行时缺失时 dsh 必须显示成未就绪"
        );

        diagnostics.record_agent(ConversationAgentKind::Dsh, available("dsh-acp-rich"));
        assert!(
            acp_agent_detected(ConversationAgentKind::Dsh, Some(&diagnostics)),
            "运行时就位后 dsh 必须显示成已就绪"
        );
    }

    /// dsh 的就绪与否只由它自己的探测决定，不受别家 CLI 装没装影响。
    #[test]
    fn dsh_detection_is_independent_of_terminal_clis() {
        let mut diagnostics = AcpRuntimeDiagnostics::default();
        diagnostics.record_terminal(TerminalAgentKind::Claude, available("claude"));
        diagnostics.record_terminal(TerminalAgentKind::Codex, available("codex"));
        assert!(!acp_agent_detected(
            ConversationAgentKind::Dsh,
            Some(&diagnostics)
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
}
