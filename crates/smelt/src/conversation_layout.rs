//! 一段对话的终端怎么分屏：这台设备自己的界面状态，存在客户端库里（设计文档 D1）。
//!
//! 运行时只记对话里有哪些终端、按什么顺序；这里记它们怎么摆。两边会对不上：别的设备或
//! 手机开关过终端，或者这台设备离线时有变化。所以恢复时以运行时的终端为准，把存下的
//! 布局对一遍：已经没有的终端去掉，布局里没有的终端补进第一个标签组，存档读不懂就用
//! 默认摆法（一个终端单独一格，几个终端叠成一组标签）。
//!
//! 存档是我们自己定义、带版本号的 JSON（AGENTS.md 的例外），叶子只记终端 id，不存组件库
//! 的导出格式，也不存终端的 cwd、标题这些归运行时的东西。

use std::collections::HashMap;

use crate::{PaneState, SplitAxis};

const LAYOUT_VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct DockLayout {
    v: u32,
    center: DockNode,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum DockNode {
    Terminal(String),
    Tabs {
        terminals: Vec<String>,
        active: usize,
    },
    Split {
        axis: SplitAxis,
        children: Vec<DockNode>,
        /// 各子块的像素尺寸，和 `children` 一一对应；空表示均分。
        sizes: Vec<f32>,
    },
}

/// 没存过布局时的摆法：一个终端单独一格，几个终端叠成一组标签。
pub(crate) fn default_layout(mut terminals: Vec<PaneState>) -> PaneState {
    match terminals.len() {
        0 => PaneState::Leaf {
            cwd: None,
            id: None,
            custom_title: None,
            launch_label: None,
            launch_cmd: None,
        },
        1 => terminals.remove(0),
        _ => PaneState::Tabs {
            active: 0,
            children: terminals,
        },
    }
}

/// 布局里的终端，按深度优先序。和恢复时建出来的终端视图同一个顺序。
pub(crate) fn terminals_of(layout: &PaneState) -> Vec<PaneState> {
    match layout {
        PaneState::Leaf { .. } => vec![layout.clone()],
        PaneState::Split { children, .. } | PaneState::Tabs { children, .. } => {
            children.iter().flat_map(terminals_of).collect()
        }
    }
}

/// 某个终端在布局里的位置（深度优先序）。找不到就是第一个。
pub(crate) fn terminal_position(layout: &PaneState, terminal_id: Option<&str>) -> usize {
    let Some(terminal_id) = terminal_id.filter(|id| !id.is_empty()) else {
        return 0;
    };
    terminals_of(layout)
        .iter()
        .position(|terminal| leaf_id(terminal) == Some(terminal_id))
        .unwrap_or(0)
}

/// 第 `position` 个终端的 id。
pub(crate) fn terminal_id_at(layout: &PaneState, position: usize) -> Option<String> {
    terminals_of(layout)
        .get(position)
        .and_then(leaf_id)
        .map(str::to_string)
}

fn leaf_id(pane: &PaneState) -> Option<&str> {
    match pane {
        PaneState::Leaf { id, .. } => id.as_deref().filter(|id| !id.is_empty()),
        _ => None,
    }
}

/// 存下这棵树的摆法。还没起过的终端没有 id，存不进去，恢复时会补回第一个标签组。
pub(crate) fn encode(layout: &PaneState) -> Option<String> {
    let center = node_of(layout)?;
    serde_json::to_string(&DockLayout {
        v: LAYOUT_VERSION,
        center,
    })
    .ok()
}

fn node_of(pane: &PaneState) -> Option<DockNode> {
    match pane {
        PaneState::Leaf { .. } => leaf_id(pane).map(|id| DockNode::Terminal(id.to_string())),
        PaneState::Tabs { active, children } => {
            let active_id = children.get(*active).and_then(leaf_id);
            let terminals = children
                .iter()
                .filter_map(leaf_id)
                .map(str::to_string)
                .collect::<Vec<_>>();
            match terminals.len() {
                0 => None,
                1 => terminals.into_iter().next().map(DockNode::Terminal),
                _ => Some(DockNode::Tabs {
                    active: active_id
                        .and_then(|id| terminals.iter().position(|terminal| terminal == id))
                        .unwrap_or(0),
                    terminals,
                }),
            }
        }
        PaneState::Split {
            axis,
            children,
            sizes,
        } => {
            let mut nodes = Vec::new();
            let mut kept_sizes = Vec::new();
            for (index, child) in children.iter().enumerate() {
                if let Some(node) = node_of(child) {
                    nodes.push(node);
                    if let Some(size) = sizes.get(index) {
                        kept_sizes.push(*size);
                    }
                }
            }
            split_or_single(*axis, nodes, kept_sizes)
        }
    }
}

fn split_or_single(axis: SplitAxis, mut nodes: Vec<DockNode>, sizes: Vec<f32>) -> Option<DockNode> {
    match nodes.len() {
        0 => None,
        1 => nodes.pop(),
        count => Some(DockNode::Split {
            axis,
            children: nodes,
            // 尺寸要么全有要么全无：缺了就均分，免得错位套到别的子块上。
            sizes: if sizes.len() == count {
                sizes
            } else {
                Vec::new()
            },
        }),
    }
}

/// 按运行时给的终端（存档里的顺序）把存下的布局对一遍，见模块说明。
pub(crate) fn restore(stored: Option<&str>, terminals: Vec<PaneState>) -> PaneState {
    let stored = stored
        .and_then(|raw| serde_json::from_str::<DockLayout>(raw).ok())
        .filter(|layout| layout.v == LAYOUT_VERSION);
    let Some(stored) = stored else {
        return default_layout(terminals);
    };
    let mut by_id = HashMap::new();
    let mut order = Vec::new();
    let mut unnamed = Vec::new();
    for terminal in terminals {
        match leaf_id(&terminal).map(str::to_string) {
            Some(id) if !by_id.contains_key(&id) => {
                order.push(id.clone());
                by_id.insert(id, terminal);
            }
            _ => unnamed.push(terminal),
        }
    }
    let tree = pane_of(stored.center, &mut by_id);
    // 布局里没摆到的终端按运行时的顺序补上，没有 id 的排在最后。
    let mut leftover = order
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect::<Vec<_>>();
    leftover.extend(unnamed);
    match tree {
        None => default_layout(leftover),
        Some(tree) if leftover.is_empty() => tree,
        Some(tree) => append_to_first_group(tree, leftover),
    }
}

fn pane_of(node: DockNode, terminals: &mut HashMap<String, PaneState>) -> Option<PaneState> {
    match node {
        DockNode::Terminal(id) => terminals.remove(&id),
        DockNode::Tabs {
            terminals: ids,
            active,
        } => {
            let active_id = ids.get(active).cloned();
            let mut children = Vec::new();
            let mut kept_ids = Vec::new();
            for id in ids {
                if let Some(terminal) = terminals.remove(&id) {
                    kept_ids.push(id);
                    children.push(terminal);
                }
            }
            match children.len() {
                0 => None,
                1 => children.pop(),
                _ => Some(PaneState::Tabs {
                    active: active_id
                        .and_then(|id| kept_ids.iter().position(|kept| *kept == id))
                        .unwrap_or(0),
                    children,
                }),
            }
        }
        DockNode::Split {
            axis,
            children,
            sizes,
        } => {
            let mut panes = Vec::new();
            let mut kept_sizes = Vec::new();
            for (index, child) in children.into_iter().enumerate() {
                if let Some(pane) = pane_of(child, terminals) {
                    panes.push(pane);
                    if let Some(size) = sizes.get(index) {
                        kept_sizes.push(*size);
                    }
                }
            }
            match panes.len() {
                0 => None,
                1 => panes.pop(),
                count => Some(PaneState::Split {
                    axis,
                    children: panes,
                    sizes: if kept_sizes.len() == count {
                        kept_sizes
                    } else {
                        Vec::new()
                    },
                }),
            }
        }
    }
}

/// 把多出来的终端叠进深度优先遇到的第一格：本来是单个终端就变成一组标签。
fn append_to_first_group(tree: PaneState, extra: Vec<PaneState>) -> PaneState {
    let mut extra = Some(extra);
    append_into(tree, &mut extra)
}

fn append_into(pane: PaneState, extra: &mut Option<Vec<PaneState>>) -> PaneState {
    match pane {
        PaneState::Leaf { .. } => match extra.take() {
            Some(more) => {
                let mut children = vec![pane];
                children.extend(more);
                PaneState::Tabs {
                    active: 0,
                    children,
                }
            }
            None => pane,
        },
        PaneState::Tabs {
            active,
            mut children,
        } => {
            if let Some(more) = extra.take() {
                children.extend(more);
            }
            PaneState::Tabs { active, children }
        }
        PaneState::Split {
            axis,
            children,
            sizes,
        } => PaneState::Split {
            axis,
            children: children
                .into_iter()
                .map(|child| append_into(child, extra))
                .collect(),
            sizes,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: &str) -> PaneState {
        PaneState::Leaf {
            cwd: Some(format!("/repo/{id}")),
            id: Some(id.to_string()),
            custom_title: None,
            launch_label: None,
            launch_cmd: None,
        }
    }

    fn ids(layout: &PaneState) -> Vec<String> {
        terminals_of(layout)
            .iter()
            .map(|terminal| leaf_id(terminal).unwrap_or("-").to_string())
            .collect()
    }

    /// 左格上下叠 t1、t2（当前 t2），右格 t3。
    fn split() -> PaneState {
        PaneState::Split {
            axis: SplitAxis::H,
            children: vec![
                PaneState::Tabs {
                    active: 1,
                    children: vec![leaf("t1"), leaf("t2")],
                },
                leaf("t3"),
            ],
            sizes: vec![400.0, 600.0],
        }
    }

    #[test]
    fn a_saved_layout_comes_back_as_it_was() {
        let stored = encode(&split()).unwrap();
        // 运行时给的顺序和布局不一样，布局照样摆回去。
        let restored = restore(Some(&stored), vec![leaf("t3"), leaf("t1"), leaf("t2")]);
        assert_eq!(restored, split());
        assert_eq!(terminal_position(&restored, Some("t3")), 2);
        assert_eq!(terminal_id_at(&restored, 1).as_deref(), Some("t2"));
    }

    #[test]
    fn terminals_gone_elsewhere_drop_out_and_new_ones_join_the_first_group() {
        let stored = encode(&split()).unwrap();
        // 别的设备关了 t1、开了 t4。
        let restored = restore(Some(&stored), vec![leaf("t2"), leaf("t3"), leaf("t4")]);
        assert_eq!(
            restored,
            PaneState::Split {
                axis: SplitAxis::H,
                children: vec![
                    PaneState::Tabs {
                        active: 0,
                        children: vec![leaf("t2"), leaf("t4")],
                    },
                    leaf("t3"),
                ],
                sizes: vec![400.0, 600.0],
            }
        );
    }

    #[test]
    fn a_split_left_with_one_side_collapses_into_it() {
        let stored = encode(&split()).unwrap();
        let restored = restore(Some(&stored), vec![leaf("t3")]);
        assert_eq!(restored, leaf("t3"));
    }

    #[test]
    fn unreadable_or_foreign_layouts_fall_back_to_tabs_in_runtime_order() {
        let terminals = || vec![leaf("t2"), leaf("t1")];
        let tabs = PaneState::Tabs {
            active: 0,
            children: terminals(),
        };
        assert_eq!(restore(None, terminals()), tabs);
        assert_eq!(restore(Some("{not json"), terminals()), tabs);
        let newer = r#"{"v":2,"center":{"terminal":"t1"}}"#;
        assert_eq!(restore(Some(newer), terminals()), tabs, "认不出的版本不猜");
        assert_eq!(restore(None, vec![leaf("t1")]), leaf("t1"));
    }

    #[test]
    fn terminals_without_an_id_are_not_stored_but_come_back() {
        let unnamed = PaneState::Leaf {
            cwd: Some("/repo".into()),
            id: None,
            custom_title: None,
            launch_label: None,
            launch_cmd: None,
        };
        let layout = PaneState::Split {
            axis: SplitAxis::V,
            children: vec![leaf("t1"), unnamed.clone()],
            sizes: vec![100.0, 200.0],
        };
        let stored = encode(&layout).unwrap();
        assert_eq!(stored, r#"{"v":1,"center":{"terminal":"t1"}}"#);
        let restored = restore(Some(&stored), vec![leaf("t1"), unnamed]);
        assert_eq!(ids(&restored), ["t1", "-"]);
    }
}
