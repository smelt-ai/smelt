//! 侧栏拖拽排序的纯函数：项目序改 `projects`，会话序改 `sessions` 下标。
//!
//! 渲染和落盘共用同一份顺序——`save_state` 按这两个数组写出，不另存排序字段。

use std::collections::HashSet;

/// 去掉路径末尾斜杠，避免 `/a/b` 和 `/a/b/` 被当成两个项目。
pub(crate) fn normalize_project_root(root: &str) -> String {
    root.trim_end_matches('/').to_string()
}

pub(crate) fn same_project_root(a: &str, b: &str) -> bool {
    !a.is_empty() && normalize_project_root(a) == normalize_project_root(b)
}

/// 把 `from` 挪到 `to` 旁边之后，在「已取出 from」的坐标系里该插入的位置。
/// 越界、自己拖自己、或挪完位置不变时返回 None。
pub(crate) fn move_index_near(len: usize, from: usize, to: usize, before: bool) -> Option<usize> {
    if from >= len || to >= len || from == to {
        return None;
    }
    let adjusted_to = if from < to { to - 1 } else { to };
    let insert_at = adjusted_to + usize::from(!before);
    (insert_at != from).then_some(insert_at)
}

/// 按 `move_index_near` 重排。成功改动返回 true。
pub(crate) fn reorder_vec<T>(items: &mut Vec<T>, from: usize, to: usize, before: bool) -> bool {
    let Some(insert_at) = move_index_near(items.len(), from, to, before) else {
        return false;
    };
    let item = items.remove(from);
    items.insert(insert_at.min(items.len()), item);
    true
}

/// 点项目标题整行时要不要切换折叠。有会话就始终切换，当前是否选中不参与判断；
/// 空项目没有可展示的子项，只更新项目上下文，不写入一个看不见的折叠状态。
pub(crate) fn project_header_click_toggles_collapse(has_sessions: bool) -> bool {
    has_sessions
}

/// 两个会话下标是否落在同一项目组。跨组拖会话不生效——会话归属由 cwd 决定，
/// 不能靠拖拽改项目。
pub(crate) fn indices_share_group(groups: &[Vec<usize>], a: usize, b: usize) -> bool {
    groups
        .iter()
        .any(|group| group.contains(&a) && group.contains(&b))
}

/// 把 `from` 项目挪到 `to` 旁边。隐式组（还没进 `projects` 骨架）被拖动或当作
/// 落点时，先补进列表再换位，这样手动排序能落盘。
pub(crate) fn move_project_root_near(
    projects: &mut Vec<String>,
    from: &str,
    to: &str,
    before: bool,
) -> bool {
    let from = normalize_project_root(from);
    let to = normalize_project_root(to);
    if from.is_empty() || to.is_empty() || from == to {
        return false;
    }
    let has = |root: &str, items: &[String]| {
        items
            .iter()
            .any(|item| normalize_project_root(item) == root)
    };
    if !has(&to, projects) {
        projects.push(to.clone());
    }
    if !has(&from, projects) {
        projects.push(from.clone());
    }
    let Some(from_ix) = projects
        .iter()
        .position(|item| normalize_project_root(item) == from)
    else {
        return false;
    };
    let Some(to_ix) = projects
        .iter()
        .position(|item| normalize_project_root(item) == to)
    else {
        return false;
    };
    reorder_vec(projects, from_ix, to_ix, before)
}

/// 隐藏无会话项目时，这一组还要不要画。
///
/// 有会话永远显示。空组只在被固定、或正是当前活动项目时保留——刚打开还没建会话
/// 的落地页不能跟着筛掉，固定是用户明确要留在列表里的出口。
pub(crate) fn sidebar_empty_project_visible(
    hide_empty: bool,
    session_count: usize,
    pinned: bool,
    is_active: bool,
) -> bool {
    if !hide_empty || session_count > 0 {
        return true;
    }
    pinned || is_active
}

pub(crate) fn is_pinned_project(pinned: &HashSet<String>, root: &str) -> bool {
    pinned.iter().any(|path| same_project_root(path, root))
}

/// 按 root 切换固定。已固定的再点一次取消；路径末尾斜杠不另算一个项目。
pub(crate) fn toggle_pinned_project(pinned: &mut HashSet<String>, root: &str) {
    let root = normalize_project_root(root);
    if root.is_empty() {
        return;
    }
    if let Some(existing) = pinned
        .iter()
        .find(|path| same_project_root(path, &root))
        .cloned()
    {
        pinned.remove(&existing);
    } else {
        pinned.insert(root);
    }
}

/// 空项目标题再降一档；当前项目即使没会话也保持可读（落地页）。
pub(crate) fn project_header_is_faint(is_active: bool, is_empty: bool) -> bool {
    is_empty && !is_active
}

/// 固定项目提到最前，组内相对顺序仍跟 `projects` 骨架走。
pub(crate) fn pinned_projects_first<T>(
    items: Vec<T>,
    pinned: &HashSet<String>,
    root_of: impl Fn(&T) -> &str,
) -> Vec<T> {
    let mut head = Vec::new();
    let mut tail = Vec::new();
    for item in items {
        if is_pinned_project(pinned, root_of(&item)) {
            head.push(item);
        } else {
            tail.push(item);
        }
    }
    head.append(&mut tail);
    head
}

/// 拖到固定区 = 固定，拖出固定区 = 取消固定；同区内只改顺序。
/// 跨区时即使落点已经紧挨着（move 是 no-op），固定状态仍要改。
pub(crate) fn apply_pinned_project_drop(
    projects: &mut Vec<String>,
    pinned: &mut HashSet<String>,
    from: &str,
    to: &str,
    before: bool,
) -> bool {
    let from_pinned = is_pinned_project(pinned, from);
    let to_pinned = is_pinned_project(pinned, to);
    let mut changed = false;
    if from_pinned != to_pinned {
        toggle_pinned_project(pinned, from);
        changed = true;
    }
    let moved = move_project_root_near(projects, from, to, before);
    changed || moved
}
