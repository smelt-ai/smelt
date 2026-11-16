use super::should_defer_notification_session_jump;

#[test]
fn notification_jump_waits_for_cold_restore_only_when_target_is_missing() {
    assert!(should_defer_notification_session_jump(false, false));
    assert!(!should_defer_notification_session_jump(false, true));
    assert!(!should_defer_notification_session_jump(true, false));
}
