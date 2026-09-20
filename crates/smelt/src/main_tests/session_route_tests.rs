use super::{
    AcpSaved, GitTab, MIN_TOOL_PANEL_WIDTH, PaneState, SessionRouteArchive, SessionState,
    SessionUiState, StageCover, WorkspaceNav, WorkspaceRoute, WsState, agents::agent_engine_kinds,
    normalize_saved_sessions, resync_session_with_project_ui, settings::ConversationAgentKind,
    swap_session_ui_state, tool_panel,
};
use std::collections::{HashMap, HashSet};

fn session_state_with_route(route: Option<SessionRouteArchive>) -> SessionState {
    SessionState {
        layout: PaneState::Leaf {
            cwd: None,
            id: None,
            custom_title: None,
            launch_label: None,
            launch_cmd: None,
        },
        active: 0,
        last_updated_at: 0,
        custom_title: None,
        acp: None,
        route,
    }
}

fn drawer_ui(tab: tool_panel::ToolPanelTab, git_tab: GitTab, expanded: &str) -> SessionUiState {
    SessionUiState {
        tool_panel_tab: tab,
        tool_panel_open: true,
        git_tab,
        expanded: HashSet::from([expanded.to_string()]),
        file_tree_selected: Some(format!("{expanded}/main.rs")),
        ..Default::default()
    }
}

#[test]
fn take_project_ui_keeps_panel_tabs_on_the_session() {
    let mut session = drawer_ui(
        tool_panel::ToolPanelTab::Git,
        GitTab::Log,
        "/tmp/project-a/src",
    );

    let parked = session.take_project_ui();
    assert!(matches!(parked.git_tab, GitTab::Log));
    assert!(matches!(
        parked.tool_panel_tab,
        tool_panel::ToolPanelTab::Git
    ));
    assert!(matches!(session.git_tab, GitTab::Changes));
    assert!(!session.tool_panel_open);
}

#[test]
fn same_project_session_switch_keeps_live_drawer() {
    let mut active = drawer_ui(
        tool_panel::ToolPanelTab::Git,
        GitTab::Log,
        "/tmp/project-a/src",
    );

    let mut old_session = SessionUiState::default();
    let mut new_session = drawer_ui(
        tool_panel::ToolPanelTab::Files,
        GitTab::Changes,
        "/tmp/project-a/stale",
    );

    let mut project_ui = HashMap::new();
    let mut active_key = Some("/tmp/project-a".into());
    resync_session_with_project_ui(
        &mut active,
        &mut old_session,
        &mut new_session,
        &mut project_ui,
        &mut active_key,
        Some("/tmp/project-a".into()),
    );

    assert!(matches!(active.git_tab, GitTab::Log));
    assert!(matches!(
        active.tool_panel_tab,
        tool_panel::ToolPanelTab::Git
    ));
    assert!(active.expanded.contains("/tmp/project-a/src"));
    assert_eq!(
        active.file_tree_selected.as_deref(),
        Some("/tmp/project-a/src/main.rs")
    );
    assert!(matches!(old_session.git_tab, GitTab::Log));
    assert_eq!(active_key.as_deref(), Some("/tmp/project-a"));
}

#[test]
fn different_project_session_switch_restores_each_drawer() {
    let mut active = drawer_ui(
        tool_panel::ToolPanelTab::Git,
        GitTab::Log,
        "/tmp/project-a/src",
    );

    let mut session_a = SessionUiState::default();
    let mut session_b = drawer_ui(
        tool_panel::ToolPanelTab::History,
        GitTab::Changes,
        "/tmp/project-b/lib",
    );

    let mut project_ui = HashMap::new();
    let mut active_key = Some("/tmp/project-a".into());
    resync_session_with_project_ui(
        &mut active,
        &mut session_a,
        &mut session_b,
        &mut project_ui,
        &mut active_key,
        Some("/tmp/project-b".into()),
    );

    assert!(matches!(
        active.tool_panel_tab,
        tool_panel::ToolPanelTab::History
    ));
    assert!(active.expanded.contains("/tmp/project-b/lib"));
    assert!(matches!(session_a.git_tab, GitTab::Log));

    let mut session_b_parked = SessionUiState::default();
    resync_session_with_project_ui(
        &mut active,
        &mut session_b_parked,
        &mut session_a,
        &mut project_ui,
        &mut active_key,
        Some("/tmp/project-a".into()),
    );

    assert!(matches!(active.git_tab, GitTab::Log));
    assert!(matches!(
        active.tool_panel_tab,
        tool_panel::ToolPanelTab::Git
    ));
    assert!(active.expanded.contains("/tmp/project-a/src"));
}

#[test]
fn opaque_route_swap_restores_layout_and_open_places() {
    let mut active = SessionUiState {
        stage_cover: Some(StageCover::ToolPanel),
        tool_panel_tab: tool_panel::ToolPanelTab::Git,
        tool_panel_open: false,
        tool_panel_w: 512.0,
        git_tab: GitTab::Log,
        ..Default::default()
    };
    let mut parked = SessionUiState {
        stage_cover: Some(StageCover::ToolPanel),
        tool_panel_tab: tool_panel::ToolPanelTab::Files,
        tool_panel_w: 296.0,
        ..Default::default()
    };

    swap_session_ui_state(&mut active, &mut parked);

    assert!(matches!(active.stage_cover, Some(StageCover::ToolPanel)));
    assert_eq!(active.tool_panel_w, MIN_TOOL_PANEL_WIDTH);
    assert!(matches!(parked.stage_cover, Some(StageCover::ToolPanel)));
    assert_eq!(parked.tool_panel_w, 512.0);
    assert!(matches!(parked.git_tab, GitTab::Log));
}

#[test]
fn route_archive_roundtrips_without_workspace_knowing_its_fields() {
    let route = SessionUiState {
        stage_cover: Some(StageCover::ToolPanel),
        tool_panel_tab: tool_panel::ToolPanelTab::History,
        tool_panel_w: 488.0,
        file_tree_selected: Some("/tmp/project/src/main.rs".into()),
        file_tree_w: 312.0,
        file_tree_open: false,
        git_tab: GitTab::Log,
        ..Default::default()
    };

    let json = serde_json::to_string(&route.archive()).unwrap();
    let archive: SessionRouteArchive = serde_json::from_str(&json).unwrap();
    let restored = SessionUiState::restore(archive);

    assert!(matches!(restored.stage_cover, Some(StageCover::ToolPanel)));
    assert!(matches!(
        restored.tool_panel_tab,
        tool_panel::ToolPanelTab::History
    ));
    assert_eq!(restored.tool_panel_w, 488.0);
    assert_eq!(restored.file_tree_w, 312.0);
    assert_eq!(
        restored.file_tree_selected.as_deref(),
        Some("/tmp/project/src/main.rs")
    );
    assert!(!restored.file_tree_open);
    assert!(matches!(restored.git_tab, GitTab::Log));
}

#[test]
fn removed_hotspot_tab_migrates_to_changes() {
    let archive: SessionRouteArchive = serde_json::from_value(serde_json::json!({
        "version": 1,
        "git_tab": "hotspot"
    }))
    .unwrap();

    assert!(matches!(archive.git_tab, GitTab::Changes));
}

#[test]
fn new_session_route_starts_with_panels_closed() {
    let route = SessionUiState::default();
    assert!(!route.tool_panel_open);
}

#[test]
fn legacy_route_without_open_flags_uses_current_defaults() {
    let archive: SessionRouteArchive = serde_json::from_value(serde_json::json!({
        "version": 1
    }))
    .unwrap();

    let restored = SessionUiState::restore(archive);
    assert!(!restored.tool_panel_open);
}

#[test]
fn legacy_workspace_panel_state_only_migrates_missing_session_routes() {
    let mut state: WsState = serde_json::from_value(serde_json::json!({
        "inspector_open": true,
        "inspector_w": 512.0,
        "file_tree_w": 280.0
    }))
    .unwrap();
    state.sessions = vec![
        session_state_with_route(None),
        session_state_with_route(Some(SessionRouteArchive {
            tool_panel_open: false,
            tool_panel_w: 444.0,
            ..Default::default()
        })),
    ];

    let (sessions, _) = normalize_saved_sessions(&state);
    let migrated = sessions[0].route.as_ref().unwrap();
    let existing = sessions[1].route.as_ref().unwrap();

    assert!(migrated.tool_panel_open);
    assert_eq!(migrated.tool_panel_w, 512.0);
    assert_eq!(migrated.file_tree_w, 280.0);
    assert!(!existing.tool_panel_open);
    assert_eq!(existing.tool_panel_w, 444.0);
}

#[test]
fn legacy_bottom_drawer_fields_are_ignored_on_restore() {
    // 删除底部面板后，旧存档里的 bottom_drawer_* 字段不再反序列化到任何运行时状态，
    // 直接丢弃，恢复出的 route 不含底部面板痕迹。
    let archive: SessionRouteArchive = serde_json::from_value(serde_json::json!({
        "version": 1,
        "bottom_drawer_open": true,
        "bottom_drawer_tabs": ["files", "terminal", "git"]
    }))
    .unwrap();
    let restored = SessionUiState::restore(archive);

    assert!(!restored.tool_panel_open);
}

#[test]
fn legacy_history_stage_migrates_to_the_tool_panel_wire_state() {
    let archive: SessionRouteArchive = serde_json::from_value(serde_json::json!({
        "version": 1,
        "stage_override": "history",
        "tool_panel_open": false,
        "tool_panel_tab": "files"
    }))
    .unwrap();
    let restored = SessionUiState::restore(archive);

    assert!(matches!(restored.stage_cover, Some(StageCover::ToolPanel)));
    assert_eq!(restored.tool_panel_tab, tool_panel::ToolPanelTab::History);
    assert!(!restored.tool_panel_open);
}

#[test]
fn legacy_tool_panel_stage_migrates_to_generic_stage_with_its_tab() {
    for (wire, tab) in [
        ("files", tool_panel::ToolPanelTab::Files),
        ("git", tool_panel::ToolPanelTab::Git),
        // 技能已迁成插件 tab：注册表里没有该插件（测试进程不加载插件包）就降级成 Files。
        ("skills", tool_panel::ToolPanelTab::Files),
        ("history", tool_panel::ToolPanelTab::History),
    ] {
        let archive: SessionRouteArchive = serde_json::from_value(serde_json::json!({
            "version": 1,
            "stage_override": wire,
            "tool_panel_tab": "files"
        }))
        .unwrap();
        let restored = SessionUiState::restore(archive);
        assert_eq!(restored.stage_cover, Some(StageCover::ToolPanel));
        assert_eq!(restored.tool_panel_tab, tab);
    }
}

#[test]
fn tool_panel_stage_keeps_the_legacy_wire_name() {
    assert_eq!(
        serde_json::to_string(&StageCover::ToolPanel).unwrap(),
        "\"inspector\""
    );
    assert_eq!(
        serde_json::from_str::<StageCover>("\"inspector\"").unwrap(),
        StageCover::ToolPanel
    );
    assert_eq!(
        serde_json::from_str::<StageCover>("\"tool_panel\"").unwrap(),
        StageCover::ToolPanel
    );

    let json = serde_json::to_string(
        &SessionUiState {
            stage_cover: Some(StageCover::ToolPanel),
            ..Default::default()
        }
        .archive(),
    )
    .unwrap();
    assert!(
        json.contains("\"stage_override\""),
        "JSON 键必须仍是 stage_override，实际: {json}"
    );
    assert!(
        !json.contains("\"stage_cover\""),
        "不能把 Rust 字段名写进存档: {json}"
    );
}

#[test]
fn product_and_session_routes_are_independent_top_level_routes() {
    assert_eq!(
        serde_json::to_string(&WorkspaceRoute::Agents).unwrap(),
        "\"agents\""
    );
    assert_eq!(
        serde_json::to_string(&WorkspaceRoute::Automations).unwrap(),
        "\"automations\""
    );
    assert_eq!(
        serde_json::to_string(&WorkspaceRoute::Session).unwrap(),
        "\"session\""
    );
    assert_eq!(
        serde_json::from_str::<WorkspaceRoute>("\"session\"").unwrap(),
        WorkspaceRoute::Session
    );
    assert!(
        serde_json::from_str::<WorkspaceRoute>("\"retired-plugin\"").is_err(),
        "未知插件 route 不能作为旧存档兼容值"
    );
}

#[test]
fn legacy_plugin_surface_field_becomes_a_first_class_tab() {
    let state: WsState = serde_json::from_value(serde_json::json!({
        "route": "agents",
        "active_workspace_surface": "com.example/board"
    }))
    .unwrap();
    let nav = WorkspaceNav::from_persisted(state.route, state.active_workspace_surface);
    assert_eq!(
        nav.active(),
        &WorkspaceRoute::Plugin {
            key: "com.example/board".into()
        }
    );
    assert_eq!(
        nav.persist(),
        (WorkspaceRoute::Session, Some("com.example/board".into()))
    );
}

#[test]
fn agent_creation_engines_come_from_the_provider_registry() {
    let actual = agent_engine_kinds();

    assert_eq!(actual, vec![ConversationAgentKind::Pi]);
}

#[test]
fn legacy_workspace_defaults_to_no_singleton_agent_runs() {
    let state: WsState = serde_json::from_value(serde_json::json!({})).unwrap();

    assert!(state.legacy_agent_runs.is_empty());
    assert!(!state._legacy_agent_runtime_visible);
}

#[test]
fn agent_conversation_binding_round_trips_with_the_conversation() {
    let mut conversation = session_state_with_route(None);
    conversation.acp = Some(AcpSaved {
        cwd: Some("/repo".into()),
        launch: smelt_core::agent_kind::ConversationLaunchSpec::from_command("smelt-pi-agent"),
        profile_id: None,
        agent: Some("pi".into()),
        agent_definition_id: Some("quant".into()),
        history_session_id: None,
        sid: Some("acp-agent-1".into()),
        refresh_launch_from_settings: true,
        fork_origin: None,
        conversation_binding: smelt_core::conversation::ConversationBinding::Direct,
        agent_session: None,
        config_values: Vec::new(),
        pending_prompt: None,
        pending_delivery_id: None,
        pending_agent_preset: Some("分析股票".into()),
        automation_id: Some("open-scan".into()),
        session_title: None,
    });
    let state = WsState {
        sessions: vec![conversation],
        ..Default::default()
    };

    let restored: WsState = serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
    assert_eq!(restored.sessions.len(), 1);
    assert_eq!(
        restored.sessions[0]
            .acp
            .as_ref()
            .and_then(|saved| saved.agent_definition_id.as_deref()),
        Some("quant")
    );
    assert_eq!(
        restored.sessions[0]
            .acp
            .as_ref()
            .and_then(|saved| saved.automation_id.as_deref()),
        Some("open-scan")
    );
}

#[test]
fn legacy_singleton_agent_run_migrates_to_a_regular_conversation() {
    let state: WsState = serde_json::from_value(serde_json::json!({
        "agent_runs": [{
            "definition_id": "quant",
            "acp": {
                "cwd": "/repo",
                "launch": {"command": "smelt-pi-agent"},
                "agent": "pi",
                "sid": "acp-agent-1",
                "pending_agent_preset": "分析股票"
            }
        }],
        "agent_runtime_visible": true
    }))
    .unwrap();

    let (sessions, _) = normalize_saved_sessions(&state);

    assert_eq!(sessions.len(), 1);
    let saved = sessions[0].acp.as_ref().unwrap();
    assert_eq!(saved.agent_definition_id.as_deref(), Some("quant"));
    assert_eq!(saved.sid.as_deref(), Some("acp-agent-1"));
    let rewritten = serde_json::to_value(state).unwrap();
    assert!(rewritten.get("agent_runs").is_none());
    assert!(rewritten.get("agent_runtime_visible").is_none());
}
