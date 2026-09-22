use super::{SidebarGrouping, WsState};
use smelt_core::workspace_menu::{
    WorkspaceMenuSession, WorkspaceMenuSessionKind, WorkspaceMenuSnapshot,
};

#[test]
fn collapsed_sidebar_groups_roundtrip() {
    let state = WsState {
        collapsed_projects: vec!["/repo/smelt".into(), "/repo/pulse".into()],
        collapsed_agents: vec!["research".into(), "quant".into()],
        ..Default::default()
    };

    let json = serde_json::to_string(&state).unwrap();
    let restored: WsState = serde_json::from_str(&json).unwrap();

    assert_eq!(restored.collapsed_projects, state.collapsed_projects);
    assert_eq!(restored.collapsed_agents, state.collapsed_agents);
}

#[test]
fn shared_workspace_menu_roundtrips() {
    let state = WsState {
        menu: WorkspaceMenuSnapshot::current(
            vec![],
            vec![WorkspaceMenuSession {
                id: "acp-stable".into(),
                kind: WorkspaceMenuSessionKind::Acp,
                title: "会话文本".into(),
                custom_title: true,
                cwd: Some("/repo".into()),
                project_root: Some("/repo".into()),
                project_title: Some("repo".into()),
                project_order: 0,
                session_order: 0,
                leaf_order: 0,
                agent: Some("codex".into()),
            }],
        ),
        ..Default::default()
    };

    let json = serde_json::to_string(&state).unwrap();
    let restored: WsState = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.menu, state.menu);
}

#[cfg(test)]
mod sidebar_group_tests {
    use crate::{AgentStatus, ProjectGroup, SidebarGrouping, sidebar_groups};

    fn group(root: &str, sessions: &[usize]) -> ProjectGroup {
        ProjectGroup {
            root: root.into(),
            label: root.into(),
            sessions: sessions.to_vec(),
        }
    }

    #[test]
    fn status_grouping_order_matches_rendered_status_buckets() {
        let projects = vec![group("a", &[0, 1]), group("b", &[2, 3])];
        let statuses = vec![
            AgentStatus::Idle,
            AgentStatus::Running,
            AgentStatus::NeedsYou,
            AgentStatus::Idle,
        ];

        let groups = sidebar_groups(SidebarGrouping::Status, projects, &statuses, &[0; 4], 4);
        let order = groups
            .iter()
            .flat_map(|group| group.sessions.iter().copied())
            .collect::<Vec<_>>();

        assert_eq!(order, vec![2, 1, 0, 3]);
    }

    #[test]
    fn project_grouping_keeps_project_order() {
        let projects = vec![group("a", &[2, 0]), group("b", &[1])];

        let groups = sidebar_groups(
            SidebarGrouping::Project,
            projects,
            &[AgentStatus::Idle; 3],
            &[0; 3],
            3,
        );

        assert_eq!(groups[0].sessions, vec![2, 0]);
        assert_eq!(groups[1].sessions, vec![1]);
    }

    #[test]
    fn last_updated_grouping_uses_calendar_buckets_and_recent_order() {
        use chrono::{Datelike, Local, TimeZone};

        let today = Local::now().date_naive();
        let at_local_noon = |days_ago: u64| {
            let date = today - chrono::Days::new(days_ago);
            Local
                .with_ymd_and_hms(date.year(), date.month(), date.day(), 12, 0, 0)
                .single()
                .unwrap()
                .timestamp() as u64
        };
        let projects = vec![group("all", &[0, 1, 2, 3])];
        let statuses = [AgentStatus::Idle; 4];
        let last_updated_at = [
            at_local_noon(0),
            at_local_noon(2),
            at_local_noon(1),
            at_local_noon(8),
        ];

        let groups = sidebar_groups(
            SidebarGrouping::LastUpdated,
            projects,
            &statuses,
            &last_updated_at,
            4,
        );

        assert_eq!(
            groups
                .iter()
                .map(|group| group.label.as_str())
                .collect::<Vec<_>>(),
            vec!["今天", "昨天", "本周", "更早"]
        );
        assert_eq!(
            groups
                .iter()
                .flat_map(|group| group.sessions.iter().copied())
                .collect::<Vec<_>>(),
            vec![0, 2, 1, 3]
        );
    }
}

#[cfg(test)]
mod restore_order_tests {
    use std::time::Duration;

    use crate::{
        AcpSaved, PaneState, RESTORE_RETRY_LIMIT, SessionState, merge_restore_pending,
        persisted_active_position, planned_restore_insert_position, record_restored_index,
        resolve_restored_active_session, restore_error_blocks_remaining, restore_path_is_cancelled,
        restore_retry_delay, restored_active_position, restored_insert_position,
        session_state_persist_id, should_auto_resume_active_acp, should_retry_failed_restore,
        split_indexed_restore_queue,
    };

    fn state(name: &str, acp: bool) -> SessionState {
        SessionState {
            layout: PaneState::Leaf {
                cwd: Some(format!("/{name}")),
                id: None,
                custom_title: None,
                launch_label: None,
                launch_cmd: None,
            },
            active: 0,
            last_updated_at: crate::unix_now_secs(),
            custom_title: Some(name.into()),
            acp: acp.then(|| AcpSaved {
                cwd: Some(format!("/{name}")),
                launch: smelt_core::agent_kind::ConversationLaunchSpec::from_command("claude"),
                profile_id: None,
                agent: Some("claude".into()),
                agent_definition_id: None,
                history_session_id: None,
                sid: Some(format!("sid-{name}")),
                refresh_launch_from_settings: false,
                fork_origin: None,
                conversation_binding: smelt_core::conversation::ConversationBinding::Direct,
                agent_session: None,
                config_values: Vec::new(),
                pending_prompt: None,
                pending_delivery_id: None,
                pending_agent_preset: None,
                automation_id: None,
                session_title: None,
            }),
            route: None,
        }
    }

    #[test]
    fn failed_restore_retries_until_limit() {
        assert!(should_retry_failed_restore(9, 0));
        assert!(should_retry_failed_restore(1, RESTORE_RETRY_LIMIT - 1));
        assert!(!should_retry_failed_restore(9, RESTORE_RETRY_LIMIT));
        assert!(!should_retry_failed_restore(0, 0));
        assert_eq!(restore_retry_delay(), Duration::from_secs(2));
    }

    #[test]
    fn handshake_timeout_fails_remaining_restores_fast() {
        assert!(restore_error_blocks_remaining(
            "Resource temporarily unavailable (os error 35)"
        ));
        assert!(restore_error_blocks_remaining("smeltd 未就绪（已拉起）"));
        assert!(!restore_error_blocks_remaining("终端会话不存在"));
    }

    #[test]
    fn split_indexed_restore_queue_retains_original_indices() {
        let pending = vec![
            state("term-0", false),
            state("acp-1", true),
            state("term-2", false),
            state("acp-3", true),
        ]
        .into_iter()
        .enumerate()
        .collect();

        let (acp, terminals) = split_indexed_restore_queue(pending);

        assert_eq!(
            acp.iter().map(|(ix, _)| *ix).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(
            terminals.iter().map(|(ix, _)| *ix).collect::<Vec<_>>(),
            vec![0, 2]
        );
    }

    #[test]
    fn incremental_restore_inserts_sessions_in_saved_order() {
        let mut restored = vec![1, 3];

        let pos = restored_insert_position(&restored, 0);
        restored.insert(pos, 0);
        let pos = restored_insert_position(&restored, 2);
        restored.insert(pos, 2);

        assert_eq!(restored, vec![0, 1, 2, 3]);
        assert_eq!(restored_active_position(&restored, 2), 2);
    }

    #[test]
    fn missing_active_session_falls_back_to_last_restored_session() {
        assert_eq!(restored_active_position(&[0, 1, 3], 2), 2);
    }

    #[test]
    fn restore_pending_are_merged_at_their_saved_indices() {
        let live = vec![state("acp-1", true), state("acp-3", true)];
        let pending = vec![(0, state("term-0", false)), (2, state("term-2", false))];

        let merged = merge_restore_pending(live, &pending);
        let names = merged
            .iter()
            .map(|session| session.custom_title.as_deref().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["term-0", "acp-1", "term-2", "acp-3"]);
        assert_eq!(persisted_active_position(1, &pending, true), 3);
    }

    #[test]
    fn active_acp_waits_for_full_restore_before_auto_resuming() {
        assert!(!should_auto_resume_active_acp(false));
        assert!(should_auto_resume_active_acp(true));
    }

    /// 存档里出现两条指向同一个 smeltd 会话的记录时，读档就要收敛掉：它们会
    /// 各自 attach 同一个 ACP 会话、互相顶掉对方，表现为反复断开重连。
    #[test]
    fn duplicate_saved_sessions_for_one_daemon_session_collapse_on_load() {
        let twin = state("acp-1", true);
        let saved = crate::WsState {
            sessions: vec![twin.clone(), state("acp-2", true), twin],
            active_session: 2,
            ..Default::default()
        };

        let (sessions, active) = crate::normalize_saved_sessions(&saved);

        assert_eq!(
            sessions
                .iter()
                .map(|session| session_state_persist_id(session).unwrap())
                .collect::<Vec<_>>(),
            vec!["sid-acp-1", "sid-acp-2"]
        );
        // 当时停在被丢掉的那条上，就改停在它的双胞胎上，而不是滑到邻居。
        assert_eq!(active, 0);
    }

    #[test]
    fn dedupe_keeps_the_active_session_when_an_earlier_twin_is_dropped() {
        let twin = state("acp-1", true);
        let saved = crate::WsState {
            sessions: vec![twin.clone(), twin, state("acp-2", true)],
            active_session: 2,
            ..Default::default()
        };

        let (sessions, active) = crate::normalize_saved_sessions(&saved);

        assert_eq!(sessions.len(), 2);
        assert_eq!(
            session_state_persist_id(&sessions[active]).unwrap(),
            "sid-acp-2"
        );
    }

    /// 启动时远程目录比存档恢复先到。排队中的会话必须算作「已占用这个 id」，
    /// 否则手机建的会话会被投影一份、恢复再装一份。
    #[test]
    fn pending_restores_reserve_their_daemon_session_ids() {
        let mut terminal = state("term-0", false);
        terminal.layout = PaneState::Leaf {
            cwd: Some("/term-0".into()),
            id: Some("term-sid".into()),
            custom_title: None,
            launch_label: None,
            launch_cmd: None,
        };
        let pending = vec![(0, terminal), (1, state("acp-1", true))];

        let ids = crate::pending_restore_session_ids(&pending);

        assert!(ids.contains("term-sid"));
        assert!(ids.contains("sid-acp-1"));
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn user_session_mutation_disables_saved_index_insertion() {
        assert_eq!(planned_restore_insert_position(&[1, 3], 2, 2), Some(1));
        assert_eq!(planned_restore_insert_position(&[1, 3], 2, 0), None);
        assert_eq!(planned_restore_insert_position(&[1], 2, 2), None);

        let mut restored = vec![1, 3];
        record_restored_index(&mut restored, 4, 2, false);
        assert_eq!(restored, vec![1, 3]);
    }

    #[test]
    fn session_state_persist_id_prefers_acp_sid_then_terminal_leaf() {
        assert_eq!(
            session_state_persist_id(&state("acp-1", true)).as_deref(),
            Some("sid-acp-1")
        );

        let mut term = state("term-0", false);
        if let crate::PaneState::Leaf { id, .. } = &mut term.layout {
            *id = Some("term-leaf-0".into());
        }
        assert_eq!(
            session_state_persist_id(&term).as_deref(),
            Some("term-leaf-0")
        );
        assert_eq!(session_state_persist_id(&state("term-0", false)), None);
    }

    #[test]
    fn restored_active_session_follows_stable_id_when_acp_is_inserted_first() {
        // 活列表是 ACP 先到的顺序：[对话, 项目终端]，存档下标 0 已经不是用户点的那条。
        let live_ids = [Some("conversation".into()), Some("project-term".into())];
        assert_eq!(
            resolve_restored_active_session(
                &live_ids,
                Some("project-term"),
                0,
                &[0, 1],
                true,
                false,
                0,
            ),
            1
        );
    }

    #[test]
    fn restored_active_session_keeps_user_click_during_restore() {
        let live_ids = [Some("conversation".into()), Some("project-term".into())];
        assert_eq!(
            resolve_restored_active_session(
                &live_ids,
                Some("project-term"),
                1,
                &[0, 1],
                true,
                true,
                0,
            ),
            0
        );
    }

    #[test]
    fn restored_active_session_falls_back_to_saved_index_without_id() {
        assert_eq!(
            resolve_restored_active_session(
                &[Some("a".into()), Some("b".into()), Some("c".into()),],
                None,
                2,
                &[0, 1, 2],
                true,
                false,
                0
            ),
            2
        );
    }

    #[test]
    fn deleting_worktree_cancels_nested_pending_restores() {
        assert!(restore_path_is_cancelled(
            Some("/repo-worktrees/feature/sub"),
            &["/repo-worktrees/feature".into()]
        ));
        assert!(!restore_path_is_cancelled(
            Some("/repo-worktrees/feature-old"),
            &["/repo-worktrees/feature".into()]
        ));
    }
}

#[test]
fn old_archive_without_collapsed_groups_still_loads() {
    let restored: WsState = serde_json::from_str(r#"{"projects":["/repo/smelt"]}"#).unwrap();

    assert!(restored.collapsed_projects.is_empty());
    assert!(restored.collapsed_agents.is_empty());
    assert!(restored.pinned_projects.is_empty());
    assert!(!restored.sidebar_hide_empty_projects);
    assert_eq!(restored.sidebar_grouping, SidebarGrouping::Project);
}

#[test]
fn sidebar_grouping_roundtrip() {
    let state = WsState {
        sidebar_grouping: SidebarGrouping::Status,
        ..Default::default()
    };

    let json = serde_json::to_string(&state).unwrap();
    let restored: WsState = serde_json::from_str(&json).unwrap();

    assert_eq!(restored.sidebar_grouping, SidebarGrouping::Status);
}

#[test]
fn pinned_projects_and_hide_empty_roundtrip() {
    let state = WsState {
        pinned_projects: vec!["/repo/smelt".into(), "/repo/pulse".into()],
        sidebar_hide_empty_projects: true,
        ..Default::default()
    };

    let json = serde_json::to_string(&state).unwrap();
    let restored: WsState = serde_json::from_str(&json).unwrap();

    assert_eq!(restored.pinned_projects, state.pinned_projects);
    assert!(restored.sidebar_hide_empty_projects);
}

#[test]
fn sqlite_workspace_snapshot_keeps_live_sidebar_fields() {
    let json = serde_json::json!({
        "active_session": 1,
        "active_session_id": "sid-1",
        "collapsed_agents": ["research"],
        "pinned_projects": ["/repo/smelt"],
        "sidebar_hide_empty_projects": true,
        "projects": ["/repo/smelt"],
        "sessions": []
    });
    let snapshot =
        smelt_store::Store::workspace_snapshot_from_json(&serde_json::to_vec(&json).unwrap())
            .unwrap();
    let restored: WsState =
        serde_json::from_slice(&smelt_store::Store::workspace_snapshot_to_json(&snapshot).unwrap())
            .unwrap();
    assert_eq!(restored.active_session_id.as_deref(), Some("sid-1"));
    assert_eq!(restored.collapsed_agents, vec!["research".to_string()]);
    assert_eq!(restored.pinned_projects, vec!["/repo/smelt".to_string()]);
    assert!(restored.sidebar_hide_empty_projects);
}

#[test]
fn load_ws_state_reads_sqlite_snapshot() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("smelt-ws-sqlite-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let database = dir.join(smelt_store::DATABASE_FILE_NAME);
    let store = smelt_store::Store::open_or_create(&database).unwrap();
    let snapshot = smelt_store::Store::workspace_snapshot_from_json(
        br#"{"active_session_id":"sid-1","pinned_projects":["/repo"],"sidebar_hide_empty_projects":true,"sessions":[]}"#,
    )
    .unwrap();
    store.put_workspace_snapshot(&snapshot).unwrap();

    let crate::workspace_persist::WorkspaceLoad::Loaded(restored) =
        crate::workspace_persist::load_ws_state_from_database(&database)
    else {
        panic!("应读出工作区快照");
    };
    assert_eq!(restored.active_session_id.as_deref(), Some("sid-1"));
    assert_eq!(restored.pinned_projects, vec!["/repo".to_string()]);
    assert!(restored.sidebar_hide_empty_projects);
    std::fs::remove_dir_all(dir).unwrap();
}
