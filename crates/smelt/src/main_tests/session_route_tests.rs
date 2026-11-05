use super::{
    GitTab, ProjectRef, RuntimeKey, SessionRouteArchive, SessionUiState, WorkspaceRoute,
    agents::agent_engine_kinds, resync_session_with_project_ui, settings::ConversationAgentKind,
    swap_session_ui_state, tool_panel,
};
use std::collections::{HashMap, HashSet};

fn local(root: &str) -> ProjectRef {
    ProjectRef {
        runtime: RuntimeKey::Local,
        root: root.to_string(),
    }
}

fn drawer_ui(git_tab: GitTab, expanded: &str) -> SessionUiState {
    SessionUiState {
        git_tab,
        expanded: HashSet::from([expanded.to_string()]),
        file_tree_selected: Some(format!("{expanded}/main.rs")),
        ..Default::default()
    }
}

#[test]
fn take_project_ui_moves_the_drawer_off_the_session() {
    let mut session = drawer_ui(GitTab::Log, "/tmp/project-a/src");

    let parked = session.take_project_ui();
    assert!(matches!(parked.git_tab, GitTab::Log));
    assert!(parked.expanded.contains("/tmp/project-a/src"));
    assert!(matches!(session.git_tab, GitTab::Changes));
    assert!(session.expanded.is_empty());
}

#[test]
fn same_project_session_switch_keeps_live_drawer() {
    let mut active = drawer_ui(GitTab::Log, "/tmp/project-a/src");

    let mut old_session = SessionUiState::default();
    let mut new_session = drawer_ui(GitTab::Changes, "/tmp/project-a/stale");

    let mut project_ui = HashMap::new();
    let mut active_key = Some(local("/tmp/project-a"));
    resync_session_with_project_ui(
        &mut active,
        &mut old_session,
        &mut new_session,
        &mut project_ui,
        &mut active_key,
        Some(local("/tmp/project-a")),
    );

    assert!(matches!(active.git_tab, GitTab::Log));
    assert!(active.expanded.contains("/tmp/project-a/src"));
    assert_eq!(
        active.file_tree_selected.as_deref(),
        Some("/tmp/project-a/src/main.rs")
    );
    assert!(matches!(old_session.git_tab, GitTab::Log));
    assert_eq!(active_key, Some(local("/tmp/project-a")));
}

#[test]
fn different_project_session_switch_restores_each_drawer() {
    let mut active = drawer_ui(GitTab::Log, "/tmp/project-a/src");

    let mut session_a = SessionUiState::default();
    let mut session_b = drawer_ui(GitTab::Changes, "/tmp/project-b/lib");

    let mut project_ui = HashMap::new();
    let mut active_key = Some(local("/tmp/project-a"));
    resync_session_with_project_ui(
        &mut active,
        &mut session_a,
        &mut session_b,
        &mut project_ui,
        &mut active_key,
        Some(local("/tmp/project-b")),
    );

    assert!(matches!(active.git_tab, GitTab::Changes));
    assert!(active.expanded.contains("/tmp/project-b/lib"));
    assert!(matches!(session_a.git_tab, GitTab::Log));

    let mut session_b_parked = SessionUiState::default();
    resync_session_with_project_ui(
        &mut active,
        &mut session_b_parked,
        &mut session_a,
        &mut project_ui,
        &mut active_key,
        Some(local("/tmp/project-a")),
    );

    assert!(matches!(active.git_tab, GitTab::Log));
    assert!(active.expanded.contains("/tmp/project-a/src"));
}

#[test]
fn same_path_on_another_machine_parks_its_own_drawer() {
    let mut active = drawer_ui(GitTab::Log, "/home/u/app/src");
    let mut local_session = SessionUiState::default();
    let mut remote_session = drawer_ui(GitTab::Changes, "/home/u/app/lib");
    let remote = ProjectRef {
        runtime: RuntimeKey::Remote("box".into()),
        root: "/home/u/app".into(),
    };

    let mut project_ui = HashMap::new();
    let mut active_key = Some(local("/home/u/app"));
    resync_session_with_project_ui(
        &mut active,
        &mut local_session,
        &mut remote_session,
        &mut project_ui,
        &mut active_key,
        Some(remote.clone()),
    );

    // 另一台机器上同一路径的项目没有停放过抽屉，不能拿本机那份盖上来。
    assert!(active.expanded.contains("/home/u/app/lib"));
    assert!(project_ui.contains_key(&local("/home/u/app")));
    assert_eq!(active_key, Some(remote));
}

#[test]
fn route_swap_clamps_file_tree_width() {
    let mut active = SessionUiState {
        file_tree_w: tool_panel::MAX_FILE_TREE_WIDTH + 100.,
        git_tab: GitTab::Log,
        ..Default::default()
    };
    let mut parked = SessionUiState {
        file_tree_w: 1.,
        ..Default::default()
    };

    swap_session_ui_state(&mut active, &mut parked);

    assert_eq!(active.file_tree_w, tool_panel::MIN_FILE_TREE_WIDTH);
    assert_eq!(parked.file_tree_w, tool_panel::MAX_FILE_TREE_WIDTH);
    assert!(matches!(parked.git_tab, GitTab::Log));
}

#[test]
fn route_archive_roundtrips_without_workspace_knowing_its_fields() {
    let route = SessionUiState {
        file_tree_selected: Some("/tmp/project/src/main.rs".into()),
        file_tree_w: 312.0,
        file_tree_open: false,
        git_tab: GitTab::Log,
        ..Default::default()
    };

    let json = serde_json::to_string(&route.archive()).unwrap();
    let archive: SessionRouteArchive = serde_json::from_str(&json).unwrap();
    let restored = SessionUiState::restore(archive);

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
fn retired_panel_fields_are_ignored_on_restore() {
    // 停靠区开合、宽度、选中的标签都归 Dock 管，存在布局状态里；旧存档里的这些键直接丢。
    let archive: SessionRouteArchive = serde_json::from_value(serde_json::json!({
        "version": 1,
        "stage_override": "inspector",
        "tool_panel_open": true,
        "tool_panel_tab": "git",
        "tool_panel_w": 512.0,
        "bottom_drawer_open": true,
        "git_tab": "log"
    }))
    .unwrap();
    let json = serde_json::to_string(&archive).unwrap();

    assert!(matches!(archive.git_tab, GitTab::Log));
    for key in ["stage_override", "tool_panel", "bottom_drawer"] {
        assert!(!json.contains(key), "{key} 不该再写进存档: {json}");
    }
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
