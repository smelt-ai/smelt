use super::{WorkspaceRoute, route_views_session, should_defer_notification_session_jump};

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
