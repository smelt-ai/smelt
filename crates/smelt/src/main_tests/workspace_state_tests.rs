#[cfg(test)]
mod sidebar_group_tests {
    use crate::{AgentStatus, ProjectGroup, RuntimeKey, SidebarGrouping, sidebar_groups};

    fn group(root: &str, sessions: &[usize]) -> ProjectGroup {
        ProjectGroup {
            runtime: RuntimeKey::Local,
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

        let groups = sidebar_groups(
            &RuntimeKey::Local,
            SidebarGrouping::Status,
            projects,
            &statuses,
            &[0; 4],
            &[0, 1, 2, 3],
        );
        let order = groups
            .iter()
            .flat_map(|group| group.sessions.iter().copied())
            .collect::<Vec<_>>();

        assert_eq!(order, vec![2, 1, 0, 3]);
    }

    /// 按状态分组时一个运行时那段只收自己的会话，别的运行时的会话不混进来。
    #[test]
    fn status_grouping_only_takes_the_runtime_members() {
        let remote = RuntimeKey::Remote("rt-linux".to_string());
        let statuses = vec![
            AgentStatus::Idle,
            AgentStatus::Running,
            AgentStatus::NeedsYou,
            AgentStatus::Idle,
        ];

        let groups = sidebar_groups(
            &remote,
            SidebarGrouping::Status,
            Vec::new(),
            &statuses,
            &[0; 4],
            &[1, 3],
        );

        assert!(groups.iter().all(|group| group.runtime == remote));
        assert_eq!(
            groups
                .iter()
                .map(|group| group.sessions.clone())
                .collect::<Vec<_>>(),
            vec![vec![1], vec![3]]
        );
    }

    #[test]
    fn project_grouping_keeps_project_order() {
        let projects = vec![group("a", &[2, 0]), group("b", &[1])];

        let groups = sidebar_groups(
            &RuntimeKey::Local,
            SidebarGrouping::Project,
            projects,
            &[AgentStatus::Idle; 3],
            &[0; 3],
            &[0, 1, 2],
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
            &RuntimeKey::Local,
            SidebarGrouping::LastUpdated,
            projects,
            &statuses,
            &last_updated_at,
            &[0, 1, 2, 3],
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
        AcpSaved, PaneState, RESTORE_RETRY_LIMIT, SessionState, persisted_active_position,
        planned_restore_insert_position, record_restored_index, resolve_restored_active_session,
        restore_error_blocks_remaining, restore_retry_delay, restored_active_position,
        restored_insert_position, should_auto_resume_active_acp, should_retry_failed_restore,
        split_indexed_restore_queue, workspace_persist::merge_restore_pending,
    };

    fn state(name: &str, acp: bool) -> SessionState {
        SessionState {
            id: format!("conv-{name}"),
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
                launch: smelt_model::agent_kind::ConversationLaunchSpec::from_command("claude"),
                profile_id: None,
                agent: Some("claude".into()),
                agent_definition_id: None,
                history_session_id: None,
                sid: Some(format!("sid-{name}")),
                refresh_launch_from_settings: false,
                conversation_binding: smelt_model::conversation::ConversationBinding::Direct,
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
}
