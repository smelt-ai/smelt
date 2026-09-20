use super::daemon_owns_acp_delivery_session;

#[test]
fn only_an_active_delivery_blocks_gui_reconnect() {
    assert!(daemon_owns_acp_delivery_session(Some("run-1")));
    assert!(!daemon_owns_acp_delivery_session(None));
}
