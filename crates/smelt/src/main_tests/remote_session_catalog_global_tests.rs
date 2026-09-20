use super::{accepts_remote_catalog_incremental, recognized_remote_acp_sessions};
use smelt_core::agent_kind::{ConversationAgentKind, ConversationLaunchSpec};
use smelt_core::session_control::{RemoteAcpSession, RemoteSessionSnapshot};

#[test]
fn legacy_unversioned_catalog_updates_are_not_deduplicated() {
    let current = RemoteSessionSnapshot {
        revision: 0,
        sessions: Vec::new(),
    };
    let incoming = RemoteSessionSnapshot {
        revision: 0,
        sessions: Vec::new(),
    };
    assert!(accepts_remote_catalog_incremental(
        Some(&current),
        &incoming
    ));

    let newer = RemoteSessionSnapshot {
        revision: 2,
        sessions: Vec::new(),
    };
    let older = RemoteSessionSnapshot {
        revision: 1,
        sessions: Vec::new(),
    };
    assert!(!accepts_remote_catalog_incremental(Some(&newer), &older));
}

#[test]
fn remote_catalog_drops_unknown_acp_agents_instead_of_projecting_claude() {
    let record = |id: &str, agent: &str| RemoteAcpSession {
        id: id.into(),
        cwd: "/repo".into(),
        title: String::new(),
        agent_option_id: agent.into(),
        agent: agent.into(),
        launch: ConversationLaunchSpec::from_command(agent),
        resume_id: None,
        created_at: 1,
        lifecycle: Default::default(),
        hidden: false,
    };

    let recognized = recognized_remote_acp_sessions(vec![
        record("known", "claude"),
        record("future", "future-agent"),
    ]);

    assert_eq!(recognized.len(), 1);
    assert_eq!(recognized[0].0.id, "known");
    assert_eq!(recognized[0].1, ConversationAgentKind::Claude);
}
