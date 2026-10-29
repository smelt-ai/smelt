use std::collections::HashSet;

use super::{indices_share_group, reorder_vec};
use crate::sidebar_order::{
    apply_pinned_project_drop, is_pinned_project, move_index_near, move_project_root_near,
    neighbor_in_group, pinned_projects_first, previous_viewed,
    project_header_click_toggles_collapse, project_header_is_dimmed, same_project_root,
    sidebar_empty_project_visible, toggle_pinned_project, toggle_pinned_project_at_boundary,
};

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
fn empty_project_title_is_dimmed_unless_it_is_the_active_project() {
    assert!(project_header_is_dimmed(false, true));
    assert!(!project_header_is_dimmed(true, true));
    assert!(!project_header_is_dimmed(false, false));
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

fn roots(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| item.to_string()).collect()
}

#[test]
fn pinning_from_menu_appends_to_the_end_of_the_pinned_section() {
    // 骨架里 /c 在 /a 前面；不挪的话固定后会排在 /a 前，夹进已固定的中间。
    let mut projects = roots(&["/c", "/a", "/b", "/d"]);
    let mut pinned = HashSet::from(["/a".to_string(), "/b".to_string()]);
    toggle_pinned_project_at_boundary(&mut projects, &mut pinned, "/c");
    assert!(is_pinned_project(&pinned, "/c"));
    assert_eq!(projects, roots(&["/a", "/b", "/c", "/d"]));
    let ordered = pinned_projects_first(projects.clone(), &pinned, String::as_str);
    assert_eq!(ordered, roots(&["/a", "/b", "/c", "/d"]));
}

#[test]
fn unpinning_from_menu_moves_to_the_top_of_the_unpinned_section() {
    let mut projects = roots(&["/a", "/b", "/c", "/d"]);
    let mut pinned = HashSet::from(["/a".to_string(), "/b".to_string()]);
    toggle_pinned_project_at_boundary(&mut projects, &mut pinned, "/a/");
    assert!(!is_pinned_project(&pinned, "/a"));
    assert_eq!(projects, roots(&["/b", "/a", "/c", "/d"]));
}

#[test]
fn unpinning_with_pins_scattered_in_old_order_still_lands_at_the_boundary() {
    // 旧数据：/x 没固定却排在固定的 /a、/b 前面，侧栏显示为 /a /b /x /y。
    let mut projects = roots(&["/x", "/a", "/b", "/y"]);
    let mut pinned = HashSet::from(["/a".to_string(), "/b".to_string()]);
    toggle_pinned_project_at_boundary(&mut projects, &mut pinned, "/a");
    let ordered = pinned_projects_first(projects.clone(), &pinned, String::as_str);
    assert_eq!(ordered, roots(&["/b", "/a", "/x", "/y"]));
    assert_eq!(projects, ordered, "存下来的顺序就是显示顺序");
}

#[test]
fn pinning_the_first_project_or_an_implicit_group_lands_at_the_boundary() {
    let mut projects = roots(&["/a", "/b"]);
    let mut pinned = HashSet::new();
    toggle_pinned_project_at_boundary(&mut projects, &mut pinned, "/b");
    assert_eq!(projects, roots(&["/b", "/a"]));

    // 隐式组（只因有会话才出现、不在骨架里）也补进骨架，排到固定区末尾。
    toggle_pinned_project_at_boundary(&mut projects, &mut pinned, "/tmp/x/");
    assert!(is_pinned_project(&pinned, "/tmp/x"));
    assert_eq!(projects, roots(&["/b", "/tmp/x", "/a"]));
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

#[test]
fn closing_the_current_session_goes_back_to_the_last_one_viewed() {
    let recent = [5, 2, 8, 3];
    assert_eq!(previous_viewed(&recent, 3, |_| true), Some(8));
    assert_eq!(
        previous_viewed(&recent, 3, |id| id != 8),
        Some(2),
        "关掉了的、不是同一类的跳过"
    );
    assert_eq!(previous_viewed(&[3], 3, |_| true), None, "还没看过别的");
}

#[test]
fn closing_the_current_session_lands_on_its_neighbour_in_the_same_project() {
    // 侧栏顺序和 sessions 下标不是一回事。
    let order = [4, 1, 7];
    assert_eq!(neighbor_in_group(&order, 1), Some(7), "优先下面那个");
    assert_eq!(
        neighbor_in_group(&order, 7),
        Some(1),
        "最后一个就落到上面那个"
    );
    assert_eq!(neighbor_in_group(&[3], 3), None, "组里只剩它");
    assert_eq!(neighbor_in_group(&order, 9), None);
}
