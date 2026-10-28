use super::{
    GitTab, MIN_TOOL_PANEL_WIDTH, SessionRouteArchive, SessionUiState, StageCover, WorkspaceRoute,
    agents::agent_engine_kinds, resync_session_with_project_ui, settings::ConversationAgentKind,
    swap_session_ui_state, tool_panel,
};
use std::collections::{HashMap, HashSet};

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
fn retired_stage_covers_are_dropped() {
    for wire in ["files", "git", "skills", "history", "tasks"] {
        let archive: SessionRouteArchive = serde_json::from_value(serde_json::json!({
            "version": 1,
            "stage_override": wire,
            "tool_panel_open": false,
            "tool_panel_tab": "files"
        }))
        .unwrap();
        let restored = SessionUiState::restore(archive);
        assert_eq!(restored.stage_cover, None, "{wire} 不再是覆盖页");
        assert_eq!(restored.tool_panel_tab, tool_panel::ToolPanelTab::Files);
        assert!(!restored.tool_panel_open);
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
fn agent_creation_engines_come_from_the_provider_registry() {
    let actual = agent_engine_kinds();

    assert_eq!(actual, vec![ConversationAgentKind::Pi]);
}
