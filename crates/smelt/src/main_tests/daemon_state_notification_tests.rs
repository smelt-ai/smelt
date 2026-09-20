use super::{
    AttentionKind, WorkspaceRoute, agent_notification_enabled, route_views_session,
    should_defer_notification_session_jump,
};

#[test]
fn notification_switches_are_independent() {
    let mut config = crate::settings::AgentHostState::default();
    config.notify_success = false;

    assert!(!agent_notification_enabled(&config, AttentionKind::Success));
    assert!(agent_notification_enabled(&config, AttentionKind::Approval));
    assert!(agent_notification_enabled(&config, AttentionKind::Input));
    assert!(agent_notification_enabled(&config, AttentionKind::Failure));
}

#[test]
fn agent_conversation_is_a_visible_session_for_notification_suppression() {
    assert!(route_views_session(
        &WorkspaceRoute::Agents,
        Some("acp-session-1"),
    ));
    assert!(!route_views_session(&WorkspaceRoute::Agents, None));
    assert!(route_views_session(&WorkspaceRoute::Session, None));
    assert!(!route_views_session(&WorkspaceRoute::Automations, None));
}

#[test]
fn notification_jump_waits_for_cold_restore_only_when_target_is_missing() {
    assert!(should_defer_notification_session_jump(false, false));
    assert!(!should_defer_notification_session_jump(false, true));
    assert!(!should_defer_notification_session_jump(true, false));
}
