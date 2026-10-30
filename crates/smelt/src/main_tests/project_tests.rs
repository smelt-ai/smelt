/// 面板（变更、文件树）跟着选中的项目走，舞台上的会话可能属于别的项目。
/// 评论只能交给这个项目里的会话，不能写进别的项目的终端或对话。
#[test]
fn panel_text_goes_only_to_a_session_of_the_panel_project() {
    use crate::workspace_sessions::panel_send_target;

    assert_eq!(panel_send_target(Some(2), Some(("smelt", &[1, 2]))), Ok(2));
    let refused = panel_send_target(Some(0), Some(("smelt", &[1, 2]))).unwrap_err();
    assert!(refused.contains("smelt"), "{refused}");
    assert!(
        panel_send_target(Some(0), Some(("空项目", &[]))).is_err(),
        "项目里还没有会话时也不能落到别处"
    );
    assert!(panel_send_target(None, Some(("smelt", &[1]))).is_err());
    assert_eq!(panel_send_target(Some(3), None), Ok(3));
}

#[test]
fn only_closing_a_running_session_asks_first() {
    use crate::workspace_sessions::closing_needs_confirm;

    assert!(closing_needs_confirm(crate::AgentStatus::Running));
    assert!(!closing_needs_confirm(crate::AgentStatus::Idle));
    assert!(!closing_needs_confirm(crate::AgentStatus::NeedsYou));
}
