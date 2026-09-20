use super::{Workspace, acp_view, settings};
use smelt_core::agent_kind::ConversationLaunchSpec;

fn request(
    source_agent: Option<&str>,
    source_profile: Option<&str>,
    target: settings::ConversationAgentKind,
    target_profile: Option<&str>,
) -> acp_view::AcpHandoffRequest {
    live_request(source_agent, source_profile, target, target_profile)
}

fn live_request(
    source_agent: Option<&str>,
    source_profile: Option<&str>,
    target: settings::ConversationAgentKind,
    target_profile: Option<&str>,
) -> acp_view::AcpHandoffRequest {
    let mut req = history_request(source_agent, source_profile, target, target_profile);
    if let Some(source) = req.source.as_mut() {
        source.from_history = false;
    }
    req
}

fn history_request(
    source_agent: Option<&str>,
    source_profile: Option<&str>,
    target: settings::ConversationAgentKind,
    target_profile: Option<&str>,
) -> acp_view::AcpHandoffRequest {
    acp_view::AcpHandoffRequest {
        source: Some(acp_view::AcpForkOrigin {
            session_id: "acp-1".into(),
            title: "修复滚动".into(),
            agent: source_agent.map(str::to_string),
            profile_label: source_profile.map(str::to_string),
            from_history: true,
        }),
        cwd: Some("/repo".into()),
        agent: target,
        launch: ConversationLaunchSpec::from_command("cmd"),
        refresh_launch_from_settings: true,
        profile_id: target_profile.map(|_| "profile-1".to_string()),
        config_values: Vec::new(),
        ephemeral_env: Default::default(),
        prompt: String::new(),
        images: Vec::new(),
        profile_label: target_profile.map(str::to_string),
        resume_session_id: None,
        fork_session_id: None,
        fork_cut: None,
        conversation_binding: smelt_core::conversation::ConversationBinding::Direct,
        agent_session: None,
    }
}

#[test]
fn live_same_agent_fork_uses_the_fork_title() {
    let title = Workspace::handoff_session_title(&live_request(
        Some("pi"),
        None,
        settings::ConversationAgentKind::Pi,
        None,
    ));
    assert_eq!(title, "分叉：修复滚动");
}

#[test]
fn history_same_agent_keeps_the_plain_continue_title() {
    let title = Workspace::handoff_session_title(&history_request(
        Some("claude"),
        None,
        settings::ConversationAgentKind::Claude,
        None,
    ));
    assert_eq!(title, "继续：修复滚动");
}

#[test]
fn cross_agent_migration_names_both_ends() {
    let title = Workspace::handoff_session_title(&request(
        Some("claude"),
        None,
        settings::ConversationAgentKind::Codex,
        None,
    ));
    assert_eq!(title, "Claude→Codex：修复滚动");
}

#[test]
fn migrating_between_workspaces_of_one_agent_uses_profile_labels() {
    let title = Workspace::handoff_session_title(&request(
        Some("claude"),
        None,
        settings::ConversationAgentKind::Claude,
        Some("Claude Quant"),
    ));
    assert_eq!(title, "Claude→Claude Quant：修复滚动");
}

/// 旧存档的 fork_origin 没有 `agent` 字段：不知道来源就别硬凑箭头。
#[test]
fn legacy_origin_without_agent_falls_back_to_plain_title() {
    let title = Workspace::handoff_session_title(&history_request(
        None,
        None,
        settings::ConversationAgentKind::Codex,
        None,
    ));
    assert_eq!(title, "继续：修复滚动");
}

#[test]
fn task_launched_sessions_have_no_source() {
    let mut req = request(None, None, settings::ConversationAgentKind::Claude, None);
    req.source = None;
    assert_eq!(Workspace::handoff_session_title(&req), "继续");
}
