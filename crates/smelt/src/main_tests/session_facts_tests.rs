use super::{AgentStatus, PaneFacts, SessionFacts, replace_changed_facts};
use smelt_client::daemon_state::{TerminalProgress, TerminalProgressState};

fn pane(progress: Option<TerminalProgress>) -> PaneFacts {
    PaneFacts {
        title: "cargo".into(),
        status: AgentStatus::Running,
        updated_at: 100,
        icon: None,
        progress,
    }
}

fn session(status: AgentStatus, updated_at: u64, panes: Vec<PaneFacts>) -> SessionFacts {
    SessionFacts {
        title: "修侧栏".into(),
        status,
        updated_at,
        cwd: Some("/repo".into()),
        session_id: Some("sid-1".into()),
        icon: Some("claude"),
        model: None,
        panes,
    }
}

#[test]
fn pushes_that_change_nothing_visible_do_not_notify_the_workspace() {
    let mut shown = Vec::new();
    assert!(replace_changed_facts(
        &mut shown,
        vec![session(AgentStatus::Running, 100, Vec::new())]
    ));
    assert!(
        !replace_changed_facts(
            &mut shown,
            vec![session(AgentStatus::Running, 100, Vec::new())]
        ),
        "正文增量、守护版本号这类推送没改侧栏能看到的东西，不该让侧栏和工具面板整块重画"
    );
}

#[test]
fn status_and_update_time_changes_notify_the_workspace() {
    let mut shown = vec![session(AgentStatus::Running, 100, Vec::new())];
    assert!(replace_changed_facts(
        &mut shown,
        vec![session(AgentStatus::Idle, 100, Vec::new())]
    ));
    assert!(
        replace_changed_facts(
            &mut shown,
            vec![session(AgentStatus::Idle, 101, Vec::new())]
        ),
        "按时间排序要精确到秒"
    );
}

#[test]
fn a_split_pane_progress_change_notifies_the_workspace() {
    let progress = |percent| TerminalProgress {
        state: TerminalProgressState::Normal,
        percent: Some(percent),
    };
    let mut shown = vec![session(
        AgentStatus::Running,
        100,
        vec![pane(Some(progress(10)))],
    )];
    assert!(replace_changed_facts(
        &mut shown,
        vec![session(
            AgentStatus::Running,
            100,
            vec![pane(Some(progress(20)))]
        )]
    ));
}

#[test]
fn opening_or_closing_a_session_notifies_the_workspace() {
    let mut shown = vec![session(AgentStatus::Idle, 100, Vec::new())];
    assert!(replace_changed_facts(&mut shown, Vec::new()));
}
