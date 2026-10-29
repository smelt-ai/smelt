//! UI-independent ACP session catalog and lifecycle requests.
//!
//! Desktop and mobile render different controls, but agent/profile resolution, transcript
//! discovery and smeltd lifecycle operations belong here. Mobile clients only send stable ids;
//! launch commands and local paths never need to be assembled on the phone.

pub use smelt_model::session_control::*;
pub use smelt_runtime_client::local::daemon_request;

use crate::agent_kind::{
    AcpProfile, ConversationAgentKind, ConversationLaunchSpec, HistorySourceKind,
};
use crate::control_api::SessionList;
use serde_json::{Value, json};
use smelt_runtime_client::protocol::control::EmptyParams;
use smelt_runtime_client::protocol::daemon::DaemonOperation;
use std::collections::BTreeMap;
use std::time::Duration;

fn load_agent_ui_snapshot() -> Option<smelt_store::AgentUiSnapshot> {
    crate::sqlite_state::default_sqlite_store()
        .ok()?
        .get_agent_ui_snapshot()
        .ok()
        .flatten()
}

fn configured_command(
    snapshot: Option<&smelt_store::AgentUiSnapshot>,
    kind: ConversationAgentKind,
) -> String {
    let key = kind.descriptor().config_key;
    snapshot
        .and_then(|snapshot| {
            snapshot
                .commands
                .iter()
                .find(|command| command.command_key == key)
                .map(|command| command.command.clone())
        })
        .filter(|command| !command.trim().is_empty())
        .unwrap_or_else(|| kind.default_cmd())
}

fn configured_env(
    snapshot: Option<&smelt_store::AgentUiSnapshot>,
    kind: ConversationAgentKind,
) -> BTreeMap<String, String> {
    snapshot
        .map(|snapshot| {
            snapshot
                .env
                .iter()
                .filter(|record| record.engine_kind_id == kind.id())
                .map(|record| (record.name.clone(), record.value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn configured_launch(
    snapshot: Option<&smelt_store::AgentUiSnapshot>,
    kind: ConversationAgentKind,
) -> ConversationLaunchSpec {
    let mut launch = kind.default_launch();
    launch.command = configured_command(snapshot, kind);
    launch.env.extend(configured_env(snapshot, kind));
    launch
}

pub fn agent_options() -> Vec<AcpAgentOption> {
    let snapshot = load_agent_ui_snapshot();
    let snapshot_ref = snapshot.as_ref();
    let mut options = ConversationAgentKind::ALL
        .into_iter()
        .map(|kind| AcpAgentOption {
            id: kind.id().to_string(),
            kind: kind.id().to_string(),
            label: kind.label().to_string(),
            profile: false,
            agent_definition_id: None,
            launch: configured_launch(snapshot_ref, kind),
            history_dir: None,
        })
        .collect::<Vec<_>>();
    let profiles = snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .profiles
                .iter()
                .map(|profile| AcpProfile {
                    id: profile.id.clone(),
                    kind_id: profile.kind_id.clone(),
                    label: profile.label.clone(),
                    workspace_dir: profile.workspace_dir.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for profile in profiles {
        let Some(kind) = profile.kind() else {
            continue;
        };
        let Ok(mut launch) = profile.launch_spec() else {
            continue;
        };
        launch.command = configured_command(snapshot_ref, kind);
        launch.env.extend(configured_env(snapshot_ref, kind));
        // profile 的 workspace 变量定义身份，必须在通用 agent 环境之后写回。
        if let Ok(name) = profile.env_var() {
            launch
                .env
                .insert(name.into(), profile.workspace_dir.clone());
        }
        options.push(AcpAgentOption {
            id: format!("profile:{}", profile.id),
            kind: kind.id().to_string(),
            label: profile.label,
            profile: true,
            agent_definition_id: None,
            launch,
            history_dir: Some(crate::workspace_override::expand_tilde(
                &profile.workspace_dir,
            )),
        });
    }
    options
}

/// 产品智能体的新建对话选项。与裸引擎/profile 分开产出：那两类是「用哪个 CLI」，
/// 这一类是「用哪个智能体」，混进 `agent_options()` 会让按 kind 找 profile 的
/// 调用方误命中。launch 在这里就把工作方式与插件注入好，桌面与移动端同一份。
pub fn agent_definition_options() -> Vec<AcpAgentOption> {
    let snapshot = load_agent_ui_snapshot();
    let snapshot_ref = snapshot.as_ref();
    crate::agent_definition_store::load_agent_definitions()
        .into_iter()
        .filter(|definition| definition.is_conversation_ready())
        .filter_map(|definition| {
            let kind = definition.engine_kind()?;
            let launch = crate::agent_definition_store::prepare_agent_definition_launch(
                configured_launch(snapshot_ref, kind),
                Some(&definition),
            );
            Some(AcpAgentOption {
                id: format!("agent:{}", definition.id),
                kind: kind.id().to_string(),
                label: definition.name.clone(),
                profile: false,
                agent_definition_id: Some(definition.id),
                launch,
                history_dir: None,
            })
        })
        .collect()
}

pub fn find_agent_option(id: &str) -> Option<AcpAgentOption> {
    if agent_definition_id_from_option_id(id).is_some() {
        return agent_definition_options()
            .into_iter()
            .find(|option| option.id == id);
    }
    agent_options().into_iter().find(|option| option.id == id)
}

pub fn find_agent_option_for(
    kind: ConversationAgentKind,
    profile_id: Option<&str>,
) -> Option<AcpAgentOption> {
    agent_options()
        .into_iter()
        .find(|option| option.kind == kind.id() && option.profile_id() == profile_id)
}

/// smeltd 当前活着的会话 id（终端 + ACP，靠前缀区分）。任务对账用：判断绑定会话
/// 是否还活着，不活的任务标失败，避免「会话没了但任务永远卡 Running」。
pub fn list_sessions() -> Result<Vec<String>, String> {
    // 只读探测可以安全回退：老守护不认识 `control` 会关闭第一条连接，第二条连接仍走
    // 稳定的 legacy `list`。有副作用的调用绝不能使用这种“失败后换协议重试”。
    if let Ok(result) = crate::daemon_state::local_runtime()
        .call::<SessionList>(&EmptyParams::default(), Duration::from_secs(5))
    {
        return Ok(result.sessions);
    }
    let response = daemon_request(json!({ "op": DaemonOperation::List }))?;
    Ok(response
        .get("sessions")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default())
}

pub fn publish_automation_event(
    event: &crate::automation::AutomationInboundEvent,
) -> Result<serde_json::Value, String> {
    daemon_request(serde_json::json!({
        "op": DaemonOperation::EventPublish,
        "event": event,
    }))
}

/// 运行时开一场产品智能体对话之前，按定义组好它：校验定义能开对话、建好智能体的 space
/// 作为工作目录、按定义解析启动规格。
pub fn agent_conversation_session(agent_definition_id: &str) -> Result<DaemonConversation, String> {
    let agent_definition_id = agent_definition_id.trim();
    if agent_definition_id.is_empty() {
        return Err("智能体 id 不能为空".into());
    }
    let definition = crate::agent_definition_store::load_agent_definitions()
        .into_iter()
        .find(|definition| definition.id == agent_definition_id)
        .ok_or_else(|| format!("智能体不存在: {agent_definition_id}"))?;
    if !definition.is_conversation_ready() {
        return Err("智能体还不能开对话：需要已注册的产品引擎（目前是 Pi）".into());
    }
    let option = find_agent_option(&format!("agent:{agent_definition_id}"))
        .ok_or_else(|| "智能体不可开对话".to_string())?;
    let cwd = crate::agent_definition_store::ensure_agent_space(agent_definition_id)
        .ok_or_else(|| "无法创建智能体工作区".to_string())?;
    Ok(DaemonConversation {
        sid: format!("acp-{}", uuid::Uuid::new_v4()),
        cwd: cwd.to_string_lossy().into_owned(),
        option,
        resume_id: None,
    })
}

pub fn list_history(option: &AcpAgentOption, cwd: &str) -> Vec<HistorySessionSummary> {
    let Some(kind) = ConversationAgentKind::from_id(&option.kind) else {
        return Vec::new();
    };
    list_history_for(
        kind.into(),
        option.profile_id(),
        cwd,
        option.history_dir.as_deref(),
    )
}

pub fn list_history_for(
    kind: HistorySourceKind,
    profile_id: Option<&str>,
    cwd: &str,
    override_dir: Option<&str>,
) -> Vec<HistorySessionSummary> {
    let mut sessions = crate::session_history::history_provider(kind)
        .list_sessions(cwd, override_dir)
        .into_iter()
        .map(|session| HistorySessionSummary {
            path: session.path,
            resume_id: session.resume_id,
            title: session.agent_title,
            custom_title: None,
            started_at: session.started_at,
            last_active_at: session.last_active_at,
            message_count: session.message_count,
            total_tokens: session.total_tokens,
        })
        .collect::<Vec<_>>();
    // 用户改过的名字在这一层贴上去，桌面端和移动端就拿到同一份展示标题。
    let mut custom_titles = crate::session_metadata::custom_titles(kind, profile_id);
    if !custom_titles.is_empty() {
        for session in &mut sessions {
            session.custom_title = custom_titles.remove(&session.resume_id);
        }
    }
    sessions.sort_by_key(|b| std::cmp::Reverse(b.last_active_at));
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_model::workspace_directory::{
        WorkspaceAcp, WorkspaceConversation, WorkspaceDirectory, WorkspaceTerminal,
    };

    #[test]
    fn agent_conversation_rejects_a_missing_definition() {
        let error = agent_conversation_session("missing-agent").unwrap_err();
        assert!(error.contains("不存在"), "{error}");
    }

    #[test]
    fn product_conversations_stay_out_of_the_project_session_list() {
        assert!(is_product_conversation(Some("auto-1"), None, None, None));
        assert!(is_product_conversation(None, Some("agent-1"), None, None));
        assert!(is_product_conversation(
            None,
            None,
            Some("acp-automation-abc"),
            None
        ));
        assert!(!is_product_conversation(None, None, Some("acp-abc"), None));
        assert!(!is_product_conversation(None, None, None, None));
        assert!(is_background_acp_session_id("acp-automation-abc"));
        assert!(!is_background_acp_session_id("acp-abc"));
        assert!(is_agent_conversation(None, Some("agent-1"), None));
        assert!(!is_agent_conversation(
            Some("auto-1"),
            Some("agent-1"),
            None
        ));
        assert!(!is_agent_conversation(Some("auto-1"), None, None));
        assert!(!is_agent_conversation(None, None, Some("/repo")));
    }

    /// 项目里新建对话即使选了某个智能体，cwd 仍是用户仓库——那是项目会话，
    /// 不能因为绑了定义就把行从项目列表拽到「对话」栏。
    #[test]
    fn project_cwd_keeps_an_agent_backed_session_in_the_project_list() {
        assert!(!is_agent_conversation(
            None,
            Some("agent-1"),
            Some("/Users/me/dev/smelt")
        ));
        assert!(!is_product_conversation(
            None,
            Some("agent-1"),
            None,
            Some("/Users/me/dev/smelt")
        ));
    }

    /// 归属字段缺失（旧库没落盘）时，托管对话目录本身就足以认出智能体对话，
    /// 不会退化成按 cwd 成组的「项目」。智能体的 space 目录同样是 Smelt 托管
    /// 的，结构上不可能是用户项目；少了它，换成 space 当工作目录之后对话会重新
    /// 掉进「项目」分组。
    #[test]
    fn agent_space_cwd_alone_marks_an_agent_conversation() {
        let Some(root) = crate::agent_definition_store::agent_space_root("space-agent") else {
            return;
        };
        let cwd = root.to_string_lossy().to_string();
        assert!(is_agent_conversation(None, None, Some(cwd.as_str())));
        assert!(is_agent_conversation(
            None,
            Some("space-agent"),
            Some(cwd.as_str())
        ));
        assert!(!is_agent_conversation(
            None,
            None,
            Some("/Users/me/Desktop/project")
        ));
    }

    #[test]
    fn workbench_conversation_cwd_alone_marks_an_agent_conversation() {
        let Some(root) = crate::agent_definition_store::workbench_conversation_workspace_root()
        else {
            return;
        };
        let cwd = root.join("92560dd2-23d6-4bc5-8d19-a12c3fc83340");
        let cwd = cwd.to_string_lossy().to_string();
        assert!(is_agent_conversation(None, None, Some(cwd.as_str())));
        assert!(is_product_conversation(
            None,
            None,
            None,
            Some(cwd.as_str())
        ));
        // 自动化 Run 借用同一目录时仍然归自动化，不进「对话」。
        assert!(!is_agent_conversation(
            Some("auto-1"),
            None,
            Some(cwd.as_str())
        ));
    }

    #[test]
    fn conversation_submit_error_kind_survives_the_daemon_response() {
        let rejected = parse_conversation_submit_response(serde_json::json!({
            "ok": false,
            "error": {"kind": "rejected", "message": "not accepted"}
        }))
        .unwrap_err();
        assert_eq!(
            rejected.kind,
            crate::conversation::ConversationSubmitErrorKind::Rejected
        );

        let unknown = parse_conversation_submit_response(serde_json::json!({
            "ok": false,
            "error": {"kind": "unknown", "message": "connection closed"}
        }))
        .unwrap_err();
        assert_eq!(
            unknown.kind,
            crate::conversation::ConversationSubmitErrorKind::Unknown
        );
    }

    #[test]
    fn default_agent_options_use_shared_agent_definitions() {
        let options = agent_options();
        for kind in ConversationAgentKind::ALL {
            let option = options
                .iter()
                .find(|option| option.id == kind.id())
                .unwrap();
            assert_eq!(option.kind, kind.id());
            assert!(!option.launch.command.trim().is_empty());
        }
    }

    #[test]
    fn remote_opencode_launch_keeps_full_access_and_allows_explicit_override() {
        let default = configured_launch(None, ConversationAgentKind::OpenCode);
        assert_eq!(
            default
                .env
                .get("OPENCODE_CONFIG_CONTENT")
                .map(String::as_str),
            Some(r#"{"permission":"allow"}"#)
        );

        let snapshot = smelt_store::AgentUiSnapshot {
            agent_hooks_enabled: true,
            commands: Vec::new(),
            env: vec![smelt_store::AgentAcpEnvRecord {
                engine_kind_id: "opencode".into(),
                name: "OPENCODE_CONFIG_CONTENT".into(),
                value: r#"{"permission":"ask"}"#.into(),
            }],
            config_memory: Vec::new(),
            agents: Vec::new(),
            profiles: Vec::new(),
        };
        let configured = configured_launch(Some(&snapshot), ConversationAgentKind::OpenCode);
        assert_eq!(
            configured
                .env
                .get("OPENCODE_CONFIG_CONTENT")
                .map(String::as_str),
            Some(r#"{"permission":"ask"}"#)
        );
    }

    /// option id 与 `session_metadata` 的 profile id 之间的换算：默认 agent 没有
    /// profile，手动加的 workspace profile 要把 `profile:` 前缀剥掉——两端重命名
    /// 落到同一条记录，全靠这一步。
    #[test]
    fn profile_id_strips_the_option_prefix() {
        let options = agent_options();
        for kind in ConversationAgentKind::ALL {
            let option = options
                .iter()
                .find(|option| option.id == kind.id())
                .unwrap();
            assert_eq!(option.profile_id(), None);
        }

        let profile = AcpAgentOption {
            id: "profile:quant".to_string(),
            kind: ConversationAgentKind::Codex.id().to_string(),
            label: "Quant".to_string(),
            profile: true,
            agent_definition_id: None,
            launch: ConversationLaunchSpec::from_command("codex".to_string()),
            history_dir: None,
        };
        assert_eq!(profile.profile_id(), Some("quant"));
        assert_eq!(
            find_agent_option_for(ConversationAgentKind::Codex, None).map(|option| option.id),
            Some(ConversationAgentKind::Codex.id().to_string())
        );
    }

    #[test]
    fn the_agent_prefix_is_the_only_marker_of_a_product_conversation() {
        let option = AcpAgentOption {
            id: "agent:writer".to_string(),
            kind: ConversationAgentKind::Pi.id().to_string(),
            label: "写作助手".to_string(),
            profile: false,
            agent_definition_id: Some("writer".to_string()),
            launch: ConversationLaunchSpec::from_command("pi".to_string()),
            history_dir: None,
        };
        assert_eq!(option.agent_definition_id(), Some("writer"));
        // 智能体不是 profile：按 kind 找 profile 的调用方不能把它当账号。
        assert_eq!(option.profile_id(), None);

        assert_eq!(agent_definition_id_from_option_id("pi"), None);
        assert_eq!(agent_definition_id_from_option_id("profile:quant"), None);
        assert_eq!(agent_definition_id_from_option_id("agent:"), None);
        assert_eq!(agent_definition_id_from_option_id("agent:   "), None);
    }

    #[test]
    fn display_title_prefers_the_user_name() {
        let mut session = HistorySessionSummary {
            path: "/tmp/session.jsonl".to_string(),
            resume_id: "session-1".to_string(),
            title: "Fix the flaky test".to_string(),
            custom_title: None,
            started_at: None,
            last_active_at: None,
            message_count: 0,
            total_tokens: 0,
        };
        assert_eq!(session.display_title(), "Fix the flaky test");
        session.custom_title = Some("夜里那次排查".to_string());
        assert_eq!(session.display_title(), "夜里那次排查");
    }

    #[test]
    fn truncates_history_titles_without_breaking_unicode() {
        let input = "你".repeat(81);
        assert_eq!(
            crate::session_history::truncate_title(&input)
                .chars()
                .count(),
            81
        );
        assert!(crate::session_history::truncate_title(&input).ends_with('…'));
    }

    /// 桌面建的会话不在远程目录里，删它时按工作区目录认：终端按每格的 id，ACP 按会话 id。
    #[test]
    fn directory_knows_which_session_ids_are_terminals_and_acp() {
        let directory = WorkspaceDirectory {
            revision: 1,
            epoch: String::new(),
            projects: Vec::new(),
            conversations: vec![
                WorkspaceConversation {
                    id: "c-term".into(),
                    terminals: vec![
                        WorkspaceTerminal {
                            id: Some("t1".into()),
                            ..WorkspaceTerminal::default()
                        },
                        WorkspaceTerminal::default(),
                    ],
                    ..WorkspaceConversation::default()
                },
                WorkspaceConversation {
                    id: "c-acp".into(),
                    acp: Some(WorkspaceAcp {
                        sid: "a1".into(),
                        ..WorkspaceAcp::default()
                    }),
                    ..WorkspaceConversation::default()
                },
            ],
        };
        assert_eq!(
            directory_session_kind(&directory, "t1"),
            Some(SessionDeleteKind::Terminal)
        );
        assert_eq!(
            directory_session_kind(&directory, "a1"),
            Some(SessionDeleteKind::Acp)
        );
        assert_eq!(directory_session_kind(&directory, "c-term"), None);
        assert_eq!(directory_session_kind(&directory, "missing"), None);
    }

}
