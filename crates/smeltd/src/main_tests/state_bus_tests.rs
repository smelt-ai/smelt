use super::*;
use smelt_event_bus::Delivery;
use smelt_event_protocol::{
    CORE_TOPIC_REMOTE_SESSIONS_CHANGED, CORE_TOPIC_SESSION_REMOVED,
    CORE_TOPIC_SESSION_STATE_CHANGED, CORE_TOPIC_TERMINAL_NOTIFICATION, CoreProjectionEvent,
    PluginId, SessionStateRemoved, SubscriptionId, Topic,
};
use std::collections::BTreeSet;

fn subscribe(event_hub: &EventHubHandle, topics: &[&str]) -> smelt_event_bus::Subscription {
    event_hub
        .subscribe(
            PluginId::new("test.state-bus").unwrap(),
            SubscriptionId::new("projection").unwrap(),
            topics
                .iter()
                .map(|topic| Topic::new(*topic).unwrap())
                .collect::<BTreeSet<_>>(),
            &core_read_capabilities(),
        )
        .unwrap()
}

fn recv_event(subscription: &smelt_event_bus::Subscription) -> Arc<smelt_event_bus::StoredEvent> {
    loop {
        match subscription.recv().unwrap() {
            Delivery::Event(event) => return event,
            Delivery::Snapshot(_) => {}
            Delivery::Lag { dropped, .. } => panic!("unexpected lag: {dropped}"),
        }
    }
}

#[test]
fn lagged_event_hub_recovers_from_latest_snapshot() {
    let event_hub = new_event_hub();
    let subscription = subscribe(&event_hub, &[CORE_TOPIC_SESSION_STATE_CHANGED]);
    for revision in 1..=300 {
        broadcast_state(
            &event_hub,
            &SessionState {
                id: "busy".to_string(),
                revision,
                ..Default::default()
            },
        );
    }

    assert!(matches!(
        subscription.try_recv(),
        Ok(Delivery::Lag { dropped, .. }) if dropped > 0
    ));
    subscription.discard_pending();
    let snapshot = event_hub.legacy_snapshot().unwrap();
    assert_eq!(snapshot["sessions"][0]["revision"], 300);
}

#[test]
fn lag_recovery_discards_stale_buffered_events() {
    let event_hub = new_event_hub();
    let subscription = subscribe(&event_hub, &[CORE_TOPIC_SESSION_STATE_CHANGED]);
    for revision in 1..=300 {
        broadcast_state(
            &event_hub,
            &SessionState {
                id: "busy".to_string(),
                revision,
                ..Default::default()
            },
        );
    }
    assert!(matches!(subscription.try_recv(), Ok(Delivery::Lag { .. })));
    subscription.discard_pending();
    assert!(matches!(
        subscription.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));

    broadcast_state(
        &event_hub,
        &SessionState {
            id: "busy".to_string(),
            revision: 301,
            ..Default::default()
        },
    );
    let event = recv_event(&subscription);
    assert_eq!(event.envelope.payload["revision"], 301);
}

#[test]
fn remove_session_publishes_removed_fact_and_updates_snapshot() {
    let event_hub = new_event_hub();
    let subscription = subscribe(
        &event_hub,
        &[CORE_TOPIC_SESSION_STATE_CHANGED, CORE_TOPIC_SESSION_REMOVED],
    );
    broadcast_state(
        &event_hub,
        &SessionState {
            id: "keep".into(),
            revision: 1,
            instance: 2,
            ..Default::default()
        },
    );
    broadcast_state(
        &event_hub,
        &SessionState {
            id: "gone".into(),
            revision: 1,
            instance: 3,
            ..Default::default()
        },
    );
    let _ = recv_event(&subscription);
    let _ = recv_event(&subscription);
    event_hub.remove_session("gone", 3).unwrap();
    let event = recv_event(&subscription);
    assert_eq!(event.envelope.topic.as_str(), CORE_TOPIC_SESSION_REMOVED);
    let removed: SessionStateRemoved =
        serde_json::from_value(event.envelope.payload.clone()).unwrap();
    assert_eq!(removed.session_id, "gone");

    let snapshot = event_hub.legacy_snapshot().unwrap();
    assert_eq!(snapshot["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["sessions"][0]["id"], "keep");
}

#[test]
fn instance_less_remove_cannot_remove_a_live_replacement() {
    let event_hub = new_event_hub();
    broadcast_state(
        &event_hub,
        &SessionState {
            id: "reused-id".into(),
            instance: 42,
            revision: 1,
            ..Default::default()
        },
    );

    assert!(
        !event_hub.remove_session("reused-id", 0).unwrap(),
        "无实例清理不能覆盖已经绑定 runtime 的替换实例"
    );
    let snapshot = event_hub.legacy_snapshot().unwrap();
    assert_eq!(snapshot["sessions"][0]["generation"], 42);
}

#[test]
fn remote_catalog_replacement_updates_event_hub_snapshot() {
    let event_hub = new_event_hub();
    let subscription = subscribe(&event_hub, &[CORE_TOPIC_REMOTE_SESSIONS_CHANGED]);
    let remote = RemoteSessionSnapshot {
        revision: 7,
        sessions: Vec::new(),
    };

    broadcast_remote_sessions(&event_hub, &remote);

    let event = recv_event(&subscription);
    let payload: CoreProjectionEvent =
        serde_json::from_value(event.envelope.payload.clone()).unwrap();
    assert_eq!(payload.revision, 7);
    let snapshot = event_hub.legacy_snapshot().unwrap();
    assert_eq!(snapshot["remote_sessions"]["revision"], 7);
}

#[test]
fn projection_synchronization_cannot_regress_published_revisions() {
    let event_hub = new_event_hub();
    broadcast_state(
        &event_hub,
        &SessionState {
            id: "session-1".to_string(),
            instance: 2,
            revision: 8,
            ..Default::default()
        },
    );
    broadcast_remote_sessions(
        &event_hub,
        &RemoteSessionSnapshot {
            revision: 7,
            sessions: Vec::new(),
        },
    );

    event_hub.synchronize_projection(DaemonProjectionSeed {
        sessions: vec![SessionState {
            id: "session-1".to_string(),
            instance: 2,
            revision: 6,
            ..Default::default()
        }],
        remote_sessions: Some(RemoteSessionSnapshot {
            revision: 5,
            sessions: Vec::new(),
        }),
        workspace_menu: None,
        workspace: None,
        automations: None,
    });

    let snapshot = event_hub.legacy_snapshot().unwrap();
    assert_eq!(snapshot["sessions"][0]["revision"], 8);
    assert_eq!(snapshot["remote_sessions"]["revision"], 7);
}

#[test]
fn startup_reconciliation_removes_unrestored_remote_sessions() {
    let remote_sessions = new_test_remote_sessions();
    mutate_remote_catalog(&remote_sessions, |catalog| {
        catalog.upsert_terminal(RemoteTerminalSession {
            id: "term-from-old-daemon".to_string(),
            cwd: "/work/project".to_string(),
            title: String::new(),
            created_at: 1,
            lifecycle: RemoteSessionLifecycle::Active,
        })
    })
    .unwrap();
    let sessions: Sessions = new_sessions();
    let acp_sessions = new_test_acp_sessions();
    let event_hub = new_event_hub();
    let subscription = subscribe(&event_hub, &[CORE_TOPIC_REMOTE_SESSIONS_CHANGED]);

    reconcile_remote_catalog_runtime(&remote_sessions, &sessions, &acp_sessions, &event_hub);

    let snapshot = remote_session_snapshot(&remote_sessions).unwrap();
    assert!(
        snapshot.sessions.is_empty(),
        "没有 runtime 的目录项应删除，不要标 Failed 永久躺着"
    );
    assert_eq!(
        recv_event(&subscription).envelope.topic.as_str(),
        CORE_TOPIC_REMOTE_SESSIONS_CHANGED
    );
}

#[test]
fn stale_runtime_cleanup_cannot_remove_replacement_catalog_entry() {
    let remote_sessions = new_test_remote_sessions();
    let event_hub = new_event_hub();
    mutate_remote_catalog(&remote_sessions, |catalog| {
        catalog.upsert_terminal(RemoteTerminalSession {
            id: "same-id".to_string(),
            cwd: "/old".to_string(),
            title: String::new(),
            created_at: 1,
            lifecycle: RemoteSessionLifecycle::Active,
        })
    })
    .unwrap();
    bind_remote_session_instance(&remote_sessions, RemoteSessionKind::Terminal, "same-id", 11);

    mutate_remote_catalog(&remote_sessions, |catalog| {
        catalog.upsert_terminal(RemoteTerminalSession {
            id: "same-id".to_string(),
            cwd: "/old".to_string(),
            title: String::new(),
            created_at: 1,
            lifecycle: RemoteSessionLifecycle::Active,
        })
    })
    .unwrap();
    bind_remote_session_instance(&remote_sessions, RemoteSessionKind::Terminal, "same-id", 12);

    assert!(
        !remove_remote_session_for_instance(
            &remote_sessions,
            &event_hub,
            RemoteSessionKind::Terminal,
            "same-id",
            11,
        )
        .unwrap(),
        "旧 instance 的迟到清理必须被拒绝"
    );
    assert!(
        !remove_remote_session_if_unbound(
            &remote_sessions,
            &event_hub,
            RemoteSessionKind::Terminal,
            "same-id",
        )
        .unwrap(),
        "无 instance 的迟到回滚也不能删除已绑定的新 runtime"
    );
    let snapshot = remote_session_snapshot(&remote_sessions).unwrap();
    let replacement = snapshot
        .sessions
        .iter()
        .find(|session| session.id == "same-id")
        .expect("替换实例的目录记录必须保留");
    assert_eq!(replacement.cwd, "/old");
    assert_eq!(
        remote_sessions
            .lock()
            .unwrap()
            .runtime_instances
            .get(&(RemoteSessionKind::Terminal, "same-id".to_string()))
            .copied(),
        Some(12)
    );
}

#[test]
fn failed_runtime_cleanup_releases_binding_for_a_later_retry() {
    let kind = RemoteSessionKind::Terminal;
    let id = "dead-runtime".to_string();
    // 这条用例要的是「目录始终读不出来」，所以 loader 必须恒失败：默认的
    // load_default 会去读开发机上真实的 ~/.smelt，把用例变成看环境脸色。
    fn unavailable() -> Result<RemoteSessionCatalog, String> {
        Err("test catalog failure".to_string())
    }
    let remote_sessions = Arc::new(Mutex::new(RemoteCatalogState {
        catalog: None,
        load_error: Some("test catalog failure".to_string()),
        loader: unavailable,
        runtime_instances: HashMap::from([((kind, id.clone()), 7)]),
    }));
    let event_hub = new_event_hub();

    let error = remove_remote_session_for_instance(&remote_sessions, &event_hub, kind, &id, 7)
        .expect_err("目录失败必须向调用方报告");
    assert!(error.contains("test catalog failure"));
    assert!(
        !remote_sessions
            .lock()
            .unwrap()
            .runtime_instances
            .contains_key(&(kind, id))
    );
}

#[test]
fn frozen_subscription_does_not_block_state_producers() {
    let event_hub = new_event_hub();
    let _frozen = subscribe(&event_hub, &[CORE_TOPIC_SESSION_STATE_CHANGED]);
    let started = Instant::now();
    for revision in 1..=2_000 {
        broadcast_state(
            &event_hub,
            &SessionState {
                id: "slow-client".to_string(),
                revision,
                ..Default::default()
            },
        );
    }
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(
        event_hub.legacy_snapshot().unwrap()["sessions"][0]["revision"],
        2_000
    );
}

#[test]
fn terminal_notifications_reach_subscribers_until_hooks_take_over() {
    let event_hub = new_event_hub();
    let subscription = subscribe(&event_hub, &[CORE_TOPIC_TERMINAL_NOTIFICATION]);
    let state = Mutex::new(SessionState {
        id: "term".to_string(),
        ..Default::default()
    });

    publish_terminal_notifications(&state, "term", &event_hub, vec!["构建完成".to_string()]);
    let event = recv_event(&subscription);
    assert_eq!(
        event.envelope.topic.as_str(),
        CORE_TOPIC_TERMINAL_NOTIFICATION
    );
    assert_eq!(event.envelope.payload["session_id"], "term");
    assert_eq!(event.envelope.payload["message"], "构建完成");

    state.lock().unwrap().structured_events = true;
    publish_terminal_notifications(&state, "term", &event_hub, vec!["已完成".to_string()]);
    assert!(
        matches!(
            subscription.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "hook 接通后通知以 hook 为准，终端通知不再发"
    );
}

/// 进度只在变化时发。后连上的订阅者靠订阅时的补发看到已经在跑的进度条；终端结束后
/// 不再补发它的，同 id 的新实例不受旧实例收尾影响。
#[test]
fn late_subscribers_get_the_current_terminal_progress_replayed() {
    use smelt_event_protocol::{
        CORE_TOPIC_TERMINAL_PROGRESS, TerminalProgress, TerminalProgressState,
    };
    let event_hub = new_event_hub();
    let progress = TerminalProgress {
        state: TerminalProgressState::Normal,
        percent: Some(40),
    };
    // 还没有人订阅时发的那次也要记下。
    event_hub
        .publish_terminal_progress("term", 7, Some(progress))
        .unwrap();

    let subscription = subscribe(&event_hub, &[CORE_TOPIC_TERMINAL_PROGRESS]);
    event_hub.replay_terminal_progress().unwrap();
    let event = recv_event(&subscription);
    assert_eq!(event.envelope.payload["session_id"], "term");
    assert_eq!(event.envelope.payload["progress"]["percent"], 40);

    event_hub.forget_terminal_progress("term", 6);
    event_hub.replay_terminal_progress().unwrap();
    assert_eq!(
        recv_event(&subscription).envelope.payload["progress"]["percent"],
        40,
        "旧实例收尾不能清掉新实例的进度条"
    );

    event_hub.forget_terminal_progress("term", 7);
    event_hub.replay_terminal_progress().unwrap();
    assert!(
        matches!(
            subscription.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ),
        "终端结束后不再补发它的进度条"
    );
}
