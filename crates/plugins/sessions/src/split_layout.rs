//! 分屏的存档格式（S2）：旧宿主的 `conversation_layout.rs` 搬到这里。存进
//! `store::SavedLayout::layout` 的那段 JSON 带版本号，格子按守护会话 id 认（对话的 `sid`、
//! 终端的 `id`），对话和终端一样是一格。
//!
//! 存档只是「上次怎么摆」，有哪些格子以目录为准（[`restore`] 对账）：存档里有、目录里没了
//! 的格子丢掉；目录里有、存档里没有的接在最右边各占一列。读到不认识的版本、内容不合法，
//! 或对完账一格都不剩，就按默认摆（[`default_layout`]：每格一列），不报错。
//!
//! 只管格式和对账，不碰 Dock；Dock 和这里的树互转在 [`crate::split`]。

use std::collections::HashSet;

use gpui::Axis;
use serde::{Deserialize, Serialize};

/// 存档的版本。格式改了就加一，旧存档读到时按默认摆。
const VERSION: u32 = 1;

/// 分屏的一块。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Node {
    /// 单独一格（守护会话 id）。
    Pane(String),
    /// 叠成标签的几格；`active` 是露着的那格在 `panes` 里的下标。
    Tabs { panes: Vec<String>, active: usize },
    /// 左右（`Horizontal`）或上下排开的几块。`sizes` 是各块的像素宽或高，和 `children`
    /// 一一对应；对不上号是空的，按均分摆。
    Split {
        axis: SplitAxis,
        children: Vec<Node>,
        sizes: Vec<f32>,
    },
}

/// `gpui::Axis` 的存档写法（它本身不能序列化）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SplitAxis {
    Horizontal,
    Vertical,
}

impl From<Axis> for SplitAxis {
    fn from(axis: Axis) -> Self {
        match axis {
            Axis::Horizontal => Self::Horizontal,
            Axis::Vertical => Self::Vertical,
        }
    }
}

impl From<SplitAxis> for Axis {
    fn from(axis: SplitAxis) -> Self {
        match axis {
            SplitAxis::Horizontal => Axis::Horizontal,
            SplitAxis::Vertical => Axis::Vertical,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    v: u32,
    root: Node,
}

/// 只读版本号：版本不对时不按这一版的形状去解。
#[derive(Deserialize)]
struct Version {
    v: u32,
}

/// 存档的 JSON。
pub(crate) fn encode(root: &Node) -> String {
    serde_json::to_string(&Stored {
        v: VERSION,
        root: root.clone(),
    })
    .expect("分屏存档总能写成 JSON")
}

/// 默认摆法：每格一列，左右排开。没有格子是 `None`。
pub(crate) fn default_layout(panes: &[String]) -> Option<Node> {
    append_columns(None, dedup(panes))
}

/// 按存档摆 `panes`（目录里眼下的格子，按目录的顺序），对账规则见模块文档。没有格子是
/// `None`。
pub(crate) fn restore(stored: Option<&str>, panes: &[String]) -> Option<Node> {
    let Some(root) = stored.and_then(decode) else {
        return default_layout(panes);
    };
    let wanted: HashSet<&str> = panes.iter().map(String::as_str).collect();
    let mut placed = HashSet::new();
    let kept = retain(root, &wanted, &mut placed);
    if kept.is_none() {
        return default_layout(panes);
    }
    let rest = dedup(panes)
        .into_iter()
        .filter(|pane| !placed.contains(pane))
        .collect();
    append_columns(kept, rest)
}

/// 树里的格子，深度优先序。
#[cfg(test)]
fn panes_of(node: &Node) -> Vec<String> {
    let mut panes = Vec::new();
    collect(node, &mut panes);
    panes
}

#[cfg(test)]
fn collect(node: &Node, out: &mut Vec<String>) {
    match node {
        Node::Pane(id) => out.push(id.clone()),
        Node::Tabs { panes, .. } => out.extend(panes.iter().cloned()),
        Node::Split { children, .. } => children.iter().for_each(|child| collect(child, out)),
    }
}

fn decode(stored: &str) -> Option<Node> {
    let version: Version = serde_json::from_str(stored).ok()?;
    if version.v != VERSION {
        return None;
    }
    serde_json::from_str::<Stored>(stored)
        .ok()
        .map(|stored| stored.root)
}

/// 只留 `wanted` 里的格子，同一格只留第一次出现的。空的块去掉，只剩一块的拆掉这一层；
/// 分屏里少了块，尺寸就对不上号了，整组改成均分，免得把 A 的宽度套到 B 头上。
fn retain(node: Node, wanted: &HashSet<&str>, placed: &mut HashSet<String>) -> Option<Node> {
    match node {
        Node::Pane(id) => keep(id, wanted, placed).map(Node::Pane),
        Node::Tabs { panes, active } => {
            let shown = panes.get(active).cloned();
            let panes: Vec<String> = panes
                .into_iter()
                .filter_map(|id| keep(id, wanted, placed))
                .collect();
            match panes.len() {
                0 => None,
                1 => panes.into_iter().next().map(Node::Pane),
                len => {
                    let active = shown
                        .and_then(|shown| panes.iter().position(|id| *id == shown))
                        .unwrap_or(active.min(len - 1));
                    Some(Node::Tabs { panes, active })
                }
            }
        }
        Node::Split {
            axis,
            children,
            sizes,
        } => {
            let total = children.len();
            let kept: Vec<Node> = children
                .into_iter()
                .filter_map(|child| retain(child, wanted, placed))
                .collect();
            let sizes = if kept.len() == total && sizes.len() == total {
                sizes
            } else {
                Vec::new()
            };
            match kept.len() {
                0 => None,
                1 => kept.into_iter().next(),
                _ => Some(Node::Split {
                    axis,
                    children: kept,
                    sizes,
                }),
            }
        }
    }
}

fn keep(id: String, wanted: &HashSet<&str>, placed: &mut HashSet<String>) -> Option<String> {
    (wanted.contains(id.as_str()) && placed.insert(id.clone())).then_some(id)
}

/// 把 `rest` 各占一列接在 `root` 最右边。
fn append_columns(root: Option<Node>, rest: Vec<String>) -> Option<Node> {
    if rest.is_empty() {
        return root;
    }
    let mut children = match root {
        None => Vec::new(),
        Some(Node::Split {
            axis: SplitAxis::Horizontal,
            children,
            ..
        }) => children,
        Some(other) => vec![other],
    };
    children.extend(rest.into_iter().map(Node::Pane));
    if children.len() == 1 {
        return children.pop();
    }
    Some(Node::Split {
        axis: SplitAxis::Horizontal,
        children,
        sizes: Vec::new(),
    })
}

fn dedup(panes: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    panes
        .iter()
        .filter(|pane| seen.insert(pane.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn pane(id: &str) -> Node {
        Node::Pane(id.to_string())
    }

    fn saved() -> Node {
        Node::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                pane("acp"),
                Node::Split {
                    axis: SplitAxis::Vertical,
                    children: vec![
                        pane("t1"),
                        Node::Tabs {
                            panes: ids(&["t2", "t3"]),
                            active: 1,
                        },
                    ],
                    sizes: vec![300., 200.],
                },
            ],
            sizes: vec![600., 400.],
        }
    }

    #[test]
    fn a_saved_layout_comes_back_as_it_was() {
        let stored = encode(&saved());
        let restored = restore(Some(&stored), &ids(&["acp", "t1", "t2", "t3"]));
        assert_eq!(restored, Some(saved()));
    }

    #[test]
    fn panes_gone_from_the_directory_drop_out_and_their_split_goes_even() {
        let stored = encode(&saved());
        let restored = restore(Some(&stored), &ids(&["acp", "t1", "t3"]));
        assert_eq!(
            restored,
            Some(Node::Split {
                axis: SplitAxis::Horizontal,
                children: vec![
                    pane("acp"),
                    Node::Split {
                        axis: SplitAxis::Vertical,
                        children: vec![pane("t1"), pane("t3")],
                        sizes: vec![300., 200.],
                    },
                ],
                sizes: vec![600., 400.],
            })
        );

        // 一侧整块没了：只剩一块的分屏收成那一块，外层尺寸对不上号改均分。
        let restored = restore(Some(&stored), &ids(&["acp"]));
        assert_eq!(restored, Some(pane("acp")));
    }

    #[test]
    fn tabs_keep_showing_the_same_pane_when_others_go() {
        let stored = encode(&Node::Tabs {
            panes: ids(&["a", "b", "c"]),
            active: 2,
        });
        assert_eq!(
            restore(Some(&stored), &ids(&["b", "c"])),
            Some(Node::Tabs {
                panes: ids(&["b", "c"]),
                active: 1,
            })
        );
    }

    #[test]
    fn panes_new_to_the_layout_join_as_columns_on_the_right() {
        let stored = encode(&Node::Split {
            axis: SplitAxis::Horizontal,
            children: vec![pane("a"), pane("b")],
            sizes: vec![500., 500.],
        });
        assert_eq!(
            restore(Some(&stored), &ids(&["a", "b", "c"])),
            Some(Node::Split {
                axis: SplitAxis::Horizontal,
                children: vec![pane("a"), pane("b"), pane("c")],
                sizes: Vec::new(),
            })
        );

        let stored = encode(&Node::Split {
            axis: SplitAxis::Vertical,
            children: vec![pane("a"), pane("b")],
            sizes: Vec::new(),
        });
        assert_eq!(
            restore(Some(&stored), &ids(&["a", "b", "c"])),
            Some(Node::Split {
                axis: SplitAxis::Horizontal,
                children: vec![
                    Node::Split {
                        axis: SplitAxis::Vertical,
                        children: vec![pane("a"), pane("b")],
                        sizes: Vec::new(),
                    },
                    pane("c"),
                ],
                sizes: Vec::new(),
            })
        );
    }

    #[test]
    fn unreadable_or_foreign_layouts_fall_back_to_columns() {
        let panes = ids(&["acp", "t1"]);
        let columns = Some(Node::Split {
            axis: SplitAxis::Horizontal,
            children: vec![pane("acp"), pane("t1")],
            sizes: Vec::new(),
        });
        assert_eq!(restore(None, &panes), columns);
        assert_eq!(restore(Some("not json"), &panes), columns);
        assert_eq!(
            restore(Some(r#"{"v":99,"root":{"pane":"acp"}}"#), &panes),
            columns
        );
        assert_eq!(
            restore(Some(r#"{"v":1,"root":{"weird":1}}"#), &panes),
            columns
        );
        // 存档里的格子目录里都没有了。
        let stale = encode(&pane("gone"));
        assert_eq!(restore(Some(&stale), &panes), columns);
    }

    #[test]
    fn the_default_is_one_column_per_pane() {
        assert_eq!(default_layout(&[]), None);
        assert_eq!(default_layout(&ids(&["a"])), Some(pane("a")));
        assert_eq!(default_layout(&ids(&["a", "a"])), Some(pane("a")));
    }

    #[test]
    fn a_pane_listed_twice_is_placed_once() {
        let stored = encode(&Node::Split {
            axis: SplitAxis::Horizontal,
            children: vec![pane("a"), pane("a"), pane("b")],
            sizes: vec![1., 2., 3.],
        });
        let restored = restore(Some(&stored), &ids(&["a", "b"]));
        assert_eq!(
            restored,
            Some(Node::Split {
                axis: SplitAxis::Horizontal,
                children: vec![pane("a"), pane("b")],
                sizes: Vec::new(),
            })
        );
        assert_eq!(panes_of(&restored.unwrap()), ids(&["a", "b"]));
    }
}
