use gpui::{Axis, px};
use gpui_component::dock::{PanelInfo, PanelState};

use super::{PaneState, SplitAxis};
use crate::term_dock::pane_state_from_dock;

fn leaf(id: &str) -> PaneState {
    PaneState::Leaf {
        cwd: Some("/repo".into()),
        id: Some(id.into()),
        custom_title: None,
        launch_label: None,
        launch_cmd: None,
    }
}

/// Dock 里一格终端的存档：名字固定，叶子整个写在 info 里。
fn pane(id: &str) -> PanelState {
    PanelState {
        info: PanelInfo::panel(serde_json::to_value(leaf(id)).unwrap()),
        ..PanelState::new("smelt.terminal")
    }
}

fn tabs(active: usize, children: Vec<PanelState>) -> PanelState {
    PanelState {
        children,
        info: PanelInfo::tabs(active),
        ..PanelState::new("TabPanel")
    }
}

fn stack(axis: Axis, sizes: &[f32], children: Vec<PanelState>) -> PanelState {
    PanelState {
        children,
        info: PanelInfo::stack(sizes.iter().map(|size| px(*size)).collect(), axis),
        ..PanelState::new("StackPanel")
    }
}

#[test]
fn single_terminal_groups_are_written_as_leaves() {
    let dock = stack(
        Axis::Horizontal,
        &[400., 600.],
        vec![tabs(0, vec![pane("a")]), tabs(0, vec![pane("b")])],
    );
    assert_eq!(
        pane_state_from_dock(&dock),
        Some(PaneState::Split {
            axis: SplitAxis::H,
            children: vec![leaf("a"), leaf("b")],
            sizes: vec![400., 600.],
        })
    );
    assert_eq!(
        pane_state_from_dock(&tabs(0, vec![pane("a")])),
        Some(leaf("a"))
    );
}

#[test]
fn stacked_terminals_keep_their_order_and_shown_tab() {
    let dock = stack(
        Axis::Vertical,
        &[300., 500.],
        vec![
            tabs(1, vec![pane("a"), pane("b")]),
            tabs(0, vec![pane("c")]),
        ],
    );
    assert_eq!(
        pane_state_from_dock(&dock),
        Some(PaneState::Split {
            axis: SplitAxis::V,
            children: vec![
                PaneState::Tabs {
                    active: 1,
                    children: vec![leaf("a"), leaf("b")],
                },
                leaf("c"),
            ],
            sizes: vec![300., 500.],
        })
    );
}

/// 认不出的格子丢掉。分屏里少了子块，尺寸就对不上号，整组改成均分；只剩一个就收掉这一层。
#[test]
fn unknown_panes_are_dropped_without_misaligning_sizes() {
    let unknown = PanelState::new("someone.else");
    let dock = stack(
        Axis::Horizontal,
        &[200., 300., 500.],
        vec![
            tabs(0, vec![pane("a")]),
            tabs(0, vec![unknown.clone()]),
            tabs(0, vec![pane("b")]),
        ],
    );
    assert_eq!(
        pane_state_from_dock(&dock),
        Some(PaneState::Split {
            axis: SplitAxis::H,
            children: vec![leaf("a"), leaf("b")],
            sizes: Vec::new(),
        })
    );
    let dock = stack(
        Axis::Horizontal,
        &[200., 800.],
        vec![tabs(0, vec![pane("a")]), tabs(0, vec![unknown])],
    );
    assert_eq!(pane_state_from_dock(&dock), Some(leaf("a")));
}

/// 运行时按 JSON 收发布局树：标签组要能原样往返。
#[test]
fn tab_groups_round_trip_through_the_saved_json() {
    let layout = PaneState::Split {
        axis: SplitAxis::H,
        children: vec![
            PaneState::Tabs {
                active: 1,
                children: vec![leaf("a"), leaf("b")],
            },
            leaf("c"),
        ],
        sizes: vec![400., 600.],
    };
    let json = serde_json::to_value(&layout).unwrap();
    assert_eq!(json["Split"]["children"][0]["Tabs"]["active"], 1);
    assert_eq!(serde_json::from_value::<PaneState>(json).unwrap(), layout);
}
