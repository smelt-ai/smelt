use super::*;
use smelt_core::acp_session::{
    AcpSessionState, AcpUserAction, BackgroundTaskStatus, BackgroundTaskView,
};
use smelt_core::daemon_state::DaemonPhase;

fn quiet_state() -> AcpSessionState {
    let mut state = AcpSessionState::default();
    state.phase = DaemonPhase::Idle;
    state
}

fn quiet_with(change: impl FnOnce(&mut AcpSessionState)) -> AcpSessionState {
    let mut state = quiet_state();
    change(&mut state);
    state
}

/// 过了闲置期的「现在」。
fn long_after_now() -> u64 {
    crate::event_hub::now_ms() + acp_idle::ACP_IDLE_SUSPEND_AFTER.as_millis() as u64 + 1_000
}

fn hosted_session(reduced: AcpSessionState) -> (AcpSession, UnixStream) {
    let session = make_acp_session_value("acp-idle", reduced);
    let (host, peer) = acp_runtime_host::HostedConversationHandle::test_stub();
    *session.hosted_handle.lock().unwrap() = Some(host);
    (session, peer)
}

#[test]
fn a_quiet_conversation_can_be_suspended() {
    assert_eq!(acp_idle::reduced_suspend_blocker(&quiet_state()), None);
}

#[test]
fn work_in_progress_blocks_suspension() {
    let cases: Vec<(&str, AcpSessionState)> = vec![
        (
            "connecting",
            quiet_with(|state| state.phase = DaemonPhase::Connecting),
        ),
        (
            "thinking",
            quiet_with(|state| state.phase = DaemonPhase::Thinking),
        ),
        (
            "turn clock still running",
            quiet_with(|state| state.turn_started_at_ms = Some(1)),
        ),
        ("compacting", quiet_with(|state| state.compacting = true)),
        (
            "replaying history",
            quiet_with(|state| state.replaying_history = true),
        ),
        (
            "queued follow-up",
            quiet_with(|state| state.queued_follow_up = vec!["next".into()]),
        ),
    ];
    for (name, state) in cases {
        assert!(
            acp_idle::reduced_suspend_blocker(&state).is_some(),
            "{name} 不能挂起"
        );
    }
}

#[test]
fn only_running_background_tasks_block_suspension() {
    let task = |status| BackgroundTaskView {
        id: "t".into(),
        title: "t".into(),
        command: "sleep 1".into(),
        status,
        exit_code: None,
        output: String::new(),
        started_at_ms: 0,
        finished_at_ms: None,
        tool_call_id: None,
    };
    let running =
        quiet_with(|state| state.background_tasks = vec![task(BackgroundTaskStatus::Running)]);
    let finished =
        quiet_with(|state| state.background_tasks = vec![task(BackgroundTaskStatus::Completed)]);
    assert!(acp_idle::reduced_suspend_blocker(&running).is_some());
    assert_eq!(acp_idle::reduced_suspend_blocker(&finished), None);
}

#[test]
fn a_hosted_conversation_idle_long_enough_can_be_suspended() {
    let (session, _peer) = hosted_session(quiet_state());
    assert_eq!(
        acp_idle::acp_suspend_blocker(&session, long_after_now(), acp_idle::ACP_IDLE_SUSPEND_AFTER),
        None
    );
}

#[test]
fn recent_activity_blocks_suspension() {
    let (session, _peer) = hosted_session(quiet_state());
    assert!(
        acp_idle::acp_suspend_blocker(
            &session,
            crate::event_hub::now_ms(),
            acp_idle::ACP_IDLE_SUSPEND_AFTER
        )
        .is_some(),
        "刚建好的会话不能在第一轮扫描就挂起"
    );
}

#[test]
fn sessions_without_a_hosted_runtime_are_left_alone() {
    let session = make_acp_session_value("acp-direct", quiet_state());
    assert_eq!(
        acp_idle::acp_suspend_blocker(&session, long_after_now(), acp_idle::ACP_IDLE_SUSPEND_AFTER),
        Some("no hosted runtime")
    );
}

#[test]
fn daemon_owned_conversations_are_never_suspended() {
    let (session, _peer) = hosted_session(quiet_state());
    *session.conversation_binding.lock().unwrap() =
        Some(smelt_core::conversation::ConversationBinding::Automation {
            run_id: "run".into(),
        });
    assert_eq!(
        acp_idle::acp_suspend_blocker(&session, long_after_now(), acp_idle::ACP_IDLE_SUSPEND_AFTER),
        Some("daemon-owned binding")
    );
}

#[test]
fn a_held_prompt_gate_blocks_suspension() {
    let (session, _peer) = hosted_session(quiet_state());
    session.prompt_in_flight.store(true, Ordering::SeqCst);
    assert!(
        acp_idle::acp_suspend_blocker(&session, long_after_now(), acp_idle::ACP_IDLE_SUSPEND_AFTER)
            .is_some()
    );
}

#[test]
fn reading_the_mirror_does_not_wake_a_conversation() {
    assert!(!acp_idle::acp_action_needs_runtime(&AcpUserAction::Refresh));
    assert!(!acp_idle::acp_action_needs_runtime(&AcpUserAction::Cancel));
    assert!(!acp_idle::acp_action_needs_runtime(
        &AcpUserAction::RefreshModels
    ));
    assert!(acp_idle::acp_action_needs_runtime(&AcpUserAction::Prompt {
        text: "hi".into(),
        images: Vec::new(),
        delivery_id: None,
    }));
    assert!(acp_idle::acp_action_needs_runtime(&AcpUserAction::Compact));
}

#[test]
fn waking_a_running_conversation_only_records_activity() {
    let acp_sessions = new_test_acp_sessions();
    let (slot, _) = acp_sessions.reserve_with("acp-awake", || {
        make_acp_session_value("acp-awake", quiet_state())
    });
    let subscribers = new_event_hub();
    assert_eq!(
        acp_idle::wake_suspended_acp_session("acp-awake", &slot, &acp_sessions, &subscribers),
        Ok(())
    );
    assert!(!slot.value.idle.is_suspended());
    assert!(
        slot.value.launch_spec.lock().unwrap().is_none(),
        "没挂起就不该重新拉起"
    );
}
