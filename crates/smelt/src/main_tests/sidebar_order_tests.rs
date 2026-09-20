use std::collections::HashSet;

use super::{SessionState, WsState, indices_share_group, reorder_vec, session_state_cwd};
use crate::sidebar_order::{
    apply_pinned_project_drop, is_pinned_project, move_index_near, move_project_root_near,
    pinned_projects_first, project_header_click_toggles_collapse, project_header_is_faint,
    same_project_root, sidebar_empty_project_visible, toggle_pinned_project,
};

fn leaf(cwd: &str) -> crate::PaneState {
    crate::PaneState::Leaf {
        cwd: Some(cwd.into()),
        id: None,
        custom_title: None,
        launch_label: None,
        launch_cmd: None,
    }
}

fn session(cwd: &str) -> SessionState {
    SessionState {
        layout: leaf(cwd),
        active: 0,
        last_updated_at: 0,
        custom_title: None,
        acp: None,
        route: None,
    }
}

#[test]
fn move_index_near_rejects_noop_and_oob() {
    assert_eq!(move_index_near(3, 1, 1, true), None);
    assert_eq!(move_index_near(3, 3, 0, true), None);
    // 已经紧挨着目标前/后，挪完位置不变。
    assert_eq!(move_index_near(3, 0, 1, true), None);
    assert_eq!(move_index_near(3, 1, 0, false), None);
}

#[test]
fn reorder_vec_moves_item_before_and_after_target() {
    let mut items = vec!["a", "b", "c", "d"];
    assert!(reorder_vec(&mut items, 0, 2, true));
    assert_eq!(items, vec!["b", "a", "c", "d"]);

    let mut items = vec!["a", "b", "c", "d"];
    assert!(reorder_vec(&mut items, 0, 3, false));
    assert_eq!(items, vec!["b", "c", "d", "a"]);

    let mut items = vec!["a", "b", "c"];
    assert!(reorder_vec(&mut items, 2, 0, true));
    assert_eq!(items, vec!["c", "a", "b"]);
}

#[test]
fn project_root_match_ignores_trailing_slash() {
    assert!(same_project_root("/a/smelt/", "/a/smelt"));
    assert!(!same_project_root("/a/smelt", "/a/smelt-old"));
}

#[test]
fn project_header_click_toggles_an_inactive_expanded_project_immediately() {
    // 是否为当前项目不再是输入：只要有会话，第一次单击就必须切换。
    assert!(project_header_click_toggles_collapse(true));
    // 空项目没有子项，不记录一个用户看不见的折叠状态。
    assert!(!project_header_click_toggles_collapse(false));
}

#[test]
fn session_drag_stays_inside_the_same_project_group() {
    let groups = vec![vec![0, 1], vec![2, 3]];
    assert!(indices_share_group(&groups, 0, 1));
    assert!(!indices_share_group(&groups, 1, 2));
}

#[test]
fn project_drag_reorders_roots_and_ignores_trailing_slash() {
    let mut projects = vec!["/a/smelt/".into(), "/b/app".into(), "/c/docs".into()];
    assert!(move_project_root_near(
        &mut projects,
        "/c/docs/",
        "/a/smelt",
        true
    ));
    assert_eq!(
        projects
            .iter()
            .map(|p| p.trim_end_matches('/'))
            .collect::<Vec<_>>(),
        vec!["/c/docs", "/a/smelt", "/b/app"]
    );
}

#[test]
fn project_drag_promotes_implicit_root_into_persisted_list() {
    let mut projects = vec!["/a/smelt".into()];
    assert!(move_project_root_near(
        &mut projects,
        "/implicit/tmp",
        "/a/smelt",
        true
    ));
    assert_eq!(
        projects,
        vec!["/implicit/tmp".to_string(), "/a/smelt".into()]
    );
}

#[test]
fn workspace_state_roundtrip_keeps_manual_project_and_session_order() {
    let state = WsState {
        projects: vec!["/b".into(), "/a".into()],
        sessions: vec![session("/a/one"), session("/b"), session("/a/two")],
        ..Default::default()
    };

    let json = serde_json::to_string(&state).unwrap();
    let restored: WsState = serde_json::from_str(&json).unwrap();

    assert_eq!(restored.projects, vec!["/b", "/a"]);
    assert_eq!(
        restored
            .sessions
            .iter()
            .map(session_state_cwd)
            .collect::<Vec<_>>(),
        vec![
            Some("/a/one".into()),
            Some("/b".into()),
            Some("/a/two".into())
        ]
    );
}

#[test]
fn hide_empty_keeps_populated_and_pinned_or_active_empty() {
    assert!(sidebar_empty_project_visible(true, 2, false, false));
    assert!(sidebar_empty_project_visible(true, 0, true, false));
    assert!(sidebar_empty_project_visible(true, 0, false, true));
    assert!(!sidebar_empty_project_visible(true, 0, false, false));
    assert!(sidebar_empty_project_visible(false, 0, false, false));
}

#[test]
fn pin_toggle_is_keyed_by_normalized_root() {
    let mut pinned = HashSet::new();
    toggle_pinned_project(&mut pinned, "/repo/smelt/");
    assert!(is_pinned_project(&pinned, "/repo/smelt"));
    toggle_pinned_project(&mut pinned, "/repo/smelt");
    assert!(!is_pinned_project(&pinned, "/repo/smelt/"));
}

#[test]
fn empty_project_title_is_faint_unless_it_is_the_active_project() {
    assert!(project_header_is_faint(false, true));
    assert!(!project_header_is_faint(true, true));
    assert!(!project_header_is_faint(false, false));
}

#[test]
fn closing_project_drops_its_pin() {
    let mut pinned = HashSet::from(["/repo/smelt".to_string(), "/repo/pulse".to_string()]);
    pinned.retain(|path| !same_project_root(path, "/repo/smelt/"));
    assert!(!is_pinned_project(&pinned, "/repo/smelt"));
    assert!(is_pinned_project(&pinned, "/repo/pulse"));
}

#[test]
fn pinned_projects_sort_to_the_top_and_keep_relative_order() {
    let pinned = HashSet::from(["/smelt".to_string(), "/c".to_string()]);
    let ordered = pinned_projects_first(vec!["/a", "/smelt", "/b", "/c"], &pinned, |root| root);
    assert_eq!(ordered, vec!["/smelt", "/c", "/a", "/b"]);
}

#[test]
fn dropping_unpinned_onto_pinned_pins_it() {
    let mut projects = vec!["/a".into(), "/b".into(), "/smelt".into()];
    let mut pinned = HashSet::from(["/smelt".to_string()]);
    assert!(apply_pinned_project_drop(
        &mut projects,
        &mut pinned,
        "/b",
        "/smelt",
        false,
    ));
    assert!(is_pinned_project(&pinned, "/b"));
    assert!(is_pinned_project(&pinned, "/smelt"));
    assert_eq!(
        projects,
        vec!["/a".to_string(), "/smelt".into(), "/b".into()]
    );
}

#[test]
fn dropping_pinned_onto_unpinned_unpins_it() {
    let mut projects = vec!["/smelt".into(), "/a".into(), "/b".into()];
    let mut pinned = HashSet::from(["/smelt".to_string()]);
    assert!(apply_pinned_project_drop(
        &mut projects,
        &mut pinned,
        "/smelt",
        "/a",
        false,
    ));
    assert!(!is_pinned_project(&pinned, "/smelt"));
    assert_eq!(
        projects,
        vec!["/a".to_string(), "/smelt".into(), "/b".into()]
    );
}
