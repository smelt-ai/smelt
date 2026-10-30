//! 侧栏键盘操作：⌘⇧E 把焦点放进侧栏；↑↓ 移动；← 收起，已经收起就回到上一级；→ 展开，
//! 已经展开就进到第一项；Enter 打开；Esc 回到正在看的会话；直接打字跳到名字开头对得上
//! 的那一行（一秒内连着打算同一个前缀）。
//!
//! 焦点句柄挂在列表里一个零大小的元素上，不挂在整个列表上：GPUI 会把焦点交给被点中的
//! 那个跟踪焦点的元素，挂在列表上的话点一下项目行，焦点就从终端跑到侧栏，接着打的字全
//! 进了侧栏。现在点行不动焦点，只有 ⌘⇧E 进来。
//!
//! 行的顺序由 [`Workspace::sidebar_items`] 给，跟 `render_session_list` 用同一份分组
//! （`sidebar_display_groups`）、同样的显隐规则。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use gpui::*;

use crate::{AgentStatus, ProjectRef, RuntimeKey, SidebarGrouping, Workspace};

/// 打字跳转时，隔多久再打就算新的前缀。
const TYPEAHEAD_WINDOW: Duration = Duration::from_secs(1);

/// 侧栏里能停的一行。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SidebarItem {
    /// 「对话」分组行。
    ConversationGroup,
    /// 一段智能体对话（会话 `ui_id`）。
    Conversation(u64),
    /// 一台机器的分段标题。
    Runtime(RuntimeKey),
    /// 分组行：项目，或按状态、时间分组时的那一组。
    Group(ProjectRef),
    /// 一个会话（`ui_id`）。
    Session(u64),
    /// 分屏会话里的一个分屏（会话 `ui_id`、分屏视图）。
    Pane(u64, EntityId),
}

impl SidebarItem {
    /// 滚动、焦点环认行用的键。会话和对话直接用会话的 `ui_id`，跟切会话时的滚动、「需要你」
    /// 提示共用；其余的散列后置最高位，跟 `ui_id` 这种从 1 数起的计数分开。
    pub(crate) fn key(&self) -> u64 {
        match self {
            Self::Session(ui_id) | Self::Conversation(ui_id) => *ui_id,
            other => {
                let mut hasher = DefaultHasher::new();
                other.hash(&mut hasher);
                hasher.finish() | (1 << 63)
            }
        }
    }
}

/// `items` 里第 `ix` 行的上一级：对话归「对话」分组，会话、分屏归它前面最近的分组行，
/// 分组行归它前面最近的机器标题。往上找时先碰到别的顶层行，说明它本身就在顶层（比如没有
/// 机器分段时的项目行、不分组时的会话），没有上一级。
pub(crate) fn parent_index(items: &[SidebarItem], ix: usize) -> Option<usize> {
    use SidebarItem::*;
    let item = items.get(ix)?;
    for (at, above) in items[..ix].iter().enumerate().rev() {
        match (item, above) {
            (Conversation(_), ConversationGroup)
            | (Session(_) | Pane(..), Group(_))
            | (Group(_), Runtime(_)) => return Some(at),
            (Group(_) | Session(_) | Pane(..), ConversationGroup | Runtime(_)) => return None,
            _ => {}
        }
    }
    None
}

/// 打字跳转：从 `from` 起（`include_from` 时包括它自己）往下找名字以 `prefix` 开头的
/// 一行，找到底再从头找。不分大小写。
pub(crate) fn typeahead_match(
    labels: &[String],
    from: usize,
    include_from: bool,
    prefix: &str,
) -> Option<usize> {
    if labels.is_empty() || prefix.is_empty() {
        return None;
    }
    let prefix = prefix.to_lowercase();
    let start = if include_from { from } else { from + 1 };
    (0..labels.len())
        .map(|step| (start + step) % labels.len())
        .find(|&ix| labels[ix].to_lowercase().starts_with(&prefix))
}

impl Workspace {
    /// 侧栏里现在看得见的行，从上到下，带着打字跳转用的名字。
    pub(crate) fn sidebar_items(&self, cx: &App) -> Vec<(SidebarItem, String)> {
        let mut items = Vec::new();
        let conversations = self.sidebar_conversation_order(cx);
        if !conversations.is_empty() {
            items.push((SidebarItem::ConversationGroup, "对话".to_string()));
            if !self.sidebar_conversations_collapsed {
                for ix in conversations {
                    let session = &self.sessions[ix];
                    items.push((SidebarItem::Conversation(session.ui_id), session.title(cx)));
                }
            }
        }

        let statuses: Vec<AgentStatus> = self.sessions.iter().map(|s| s.status(cx)).collect();
        let last_updated_at: Vec<u64> = self
            .sessions
            .iter()
            .map(|s| s.effective_updated_at(cx))
            .collect();
        let groups = self.sidebar_display_groups(&statuses, &last_updated_at, cx, |_, _| {});
        let runtime_sections = self.spaces.iter().any(|space| !space.runtime.is_local());
        let active_project = self.active_project_ref(cx);
        for key in self.space_keys() {
            let Some(space) = self.space(&key) else {
                continue;
            };
            if runtime_sections {
                items.push((
                    SidebarItem::Runtime(key.clone()),
                    crate::runtimes::runtime_name(&key, cx),
                ));
                if self.runtime_folded(&key) {
                    continue;
                }
            }
            for group in groups.iter().filter(|group| group.runtime == key) {
                let project = ProjectRef {
                    runtime: group.runtime.clone(),
                    root: group.root.clone(),
                };
                if self.sidebar_grouping == SidebarGrouping::Project {
                    let is_active = active_project.as_ref().is_some_and(|active| {
                        active.runtime == group.runtime
                            && crate::sidebar_order::same_project_root(&group.root, &active.root)
                    });
                    if !crate::sidebar_order::sidebar_empty_project_visible(
                        self.sidebar_hide_empty_projects,
                        group.sessions.len(),
                        self.is_project_pinned(&project),
                        is_active,
                    ) {
                        continue;
                    }
                }
                if self.sidebar_grouping != SidebarGrouping::None {
                    items.push((SidebarItem::Group(project), group.label.clone()));
                }
                if space.collapsed_projects.contains(&group.root) {
                    continue;
                }
                for &ix in &group.sessions {
                    let session = &self.sessions[ix];
                    if session.pane_count() <= 1 {
                        items.push((SidebarItem::Session(session.ui_id), session.title(cx)));
                    } else {
                        for leaf in session.term_leaves() {
                            let title = crate::pane_title(&leaf, cx);
                            items.push((SidebarItem::Pane(session.ui_id, leaf.entity_id()), title));
                        }
                    }
                }
            }
        }
        items
    }

    /// 「对话」分组里的对话，按最近活动排。侧栏画这一组和键盘走这一组用同一份顺序。
    pub(crate) fn sidebar_conversation_order(&self, cx: &App) -> Vec<usize> {
        let product: Vec<usize> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| session.is_agent_conversation(cx))
            .map(|(ix, _)| ix)
            .collect();
        let updated: Vec<u64> = product
            .iter()
            .map(|&ix| self.sessions[ix].effective_updated_at(cx))
            .collect();
        crate::agents::recent_conversation_order(&updated)
            .into_iter()
            .map(|order| product[order])
            .collect()
    }

    /// 键盘焦点在侧栏里时，焦点环落在哪一行（它的键）。
    pub(crate) fn sidebar_focused_key(&self, window: &Window) -> Option<u64> {
        if !self.sidebar_focus.is_focused(window) {
            return None;
        }
        self.sidebar_cursor.as_ref().map(SidebarItem::key)
    }

    /// ⌘⇧E：把焦点放进侧栏，停在正在看的那一行上；侧栏收着就先打开。
    pub(crate) fn focus_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sidebar_open {
            self.sidebar_open = true;
            self.save_state(cx);
        }
        let items: Vec<SidebarItem> = self
            .sidebar_items(cx)
            .into_iter()
            .map(|(item, _)| item)
            .collect();
        let current = self.cur().map(|session| {
            if session.is_agent_conversation(cx) {
                SidebarItem::Conversation(session.ui_id)
            } else {
                SidebarItem::Session(session.ui_id)
            }
        });
        let cursor = current
            .filter(|item| items.contains(item))
            .or_else(|| {
                self.sidebar_cursor
                    .clone()
                    .filter(|item| items.contains(item))
            })
            .or_else(|| items.first().cloned());
        window.focus(&self.sidebar_focus, cx);
        self.set_sidebar_cursor(cursor, cx);
    }

    fn set_sidebar_cursor(&mut self, cursor: Option<SidebarItem>, cx: &mut Context<Self>) {
        if let Some(item) = &cursor {
            self.sidebar_reveal.request(item.key());
        }
        self.sidebar_cursor = cursor;
        cx.notify();
    }

    /// 光标在 `items` 里的位置；光标那一行已经不在了（被收起、关掉）就从头算。
    fn sidebar_cursor_index(&self, items: &[SidebarItem]) -> Option<usize> {
        self.sidebar_cursor
            .as_ref()
            .and_then(|cursor| items.iter().position(|item| item == cursor))
    }

    /// ↑↓。
    pub(crate) fn sidebar_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let items: Vec<SidebarItem> = self
            .sidebar_items(cx)
            .into_iter()
            .map(|(item, _)| item)
            .collect();
        if items.is_empty() {
            return;
        }
        let next = match self.sidebar_cursor_index(&items) {
            Some(ix) => ix.saturating_add_signed(delta).min(items.len() - 1),
            None if delta < 0 => items.len() - 1,
            None => 0,
        };
        self.set_sidebar_cursor(Some(items[next].clone()), cx);
    }

    /// ←：展开着的分组先收起，已经收起或者是一行会话就回到上一级。
    pub(crate) fn sidebar_collapse(&mut self, cx: &mut Context<Self>) {
        let items: Vec<SidebarItem> = self
            .sidebar_items(cx)
            .into_iter()
            .map(|(item, _)| item)
            .collect();
        let Some(ix) = self.sidebar_cursor_index(&items) else {
            return self.sidebar_move(0, cx);
        };
        if self.sidebar_container_open(&items[ix], cx) == Some(true) {
            self.toggle_sidebar_container(&items[ix], cx);
        } else if let Some(parent) = parent_index(&items, ix) {
            self.set_sidebar_cursor(Some(items[parent].clone()), cx);
        }
    }

    /// →：收起的分组先展开，已经展开就进到它的第一项。
    pub(crate) fn sidebar_expand(&mut self, cx: &mut Context<Self>) {
        let items: Vec<SidebarItem> = self
            .sidebar_items(cx)
            .into_iter()
            .map(|(item, _)| item)
            .collect();
        let Some(ix) = self.sidebar_cursor_index(&items) else {
            return self.sidebar_move(0, cx);
        };
        match self.sidebar_container_open(&items[ix], cx) {
            Some(false) => self.toggle_sidebar_container(&items[ix], cx),
            Some(true) => {
                if let Some(child) = items
                    .get(ix + 1)
                    .filter(|_| parent_index(&items, ix + 1) == Some(ix))
                {
                    self.set_sidebar_cursor(Some(child.clone()), cx);
                }
            }
            None => {}
        }
    }

    /// Enter：会话、对话、分屏打开（焦点跟着进去）；分组行、机器标题、「对话」开合。
    pub(crate) fn sidebar_open_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cursor) = self.sidebar_cursor.clone() else {
            return;
        };
        let position = |ui_id: u64| self.sessions.iter().position(|s| s.ui_id == ui_id);
        match &cursor {
            SidebarItem::Session(ui_id) => {
                if let Some(ix) = position(*ui_id) {
                    self.activate(ix, window, cx);
                }
            }
            SidebarItem::Conversation(ui_id) => {
                if let Some(ix) = position(*ui_id) {
                    self.open_agent_conversation(ix, window, cx);
                }
            }
            SidebarItem::Pane(ui_id, pane_id) => {
                let pane = position(*ui_id).and_then(|ix| {
                    self.sessions[ix]
                        .term_leaves()
                        .into_iter()
                        .find(|leaf| leaf.entity_id() == *pane_id)
                        .map(|leaf| (ix, leaf))
                });
                if let Some((ix, pane)) = pane {
                    self.activate_session_pane(ix, pane, window, cx);
                }
            }
            SidebarItem::Group(project) if self.sidebar_grouping == SidebarGrouping::Project => {
                // 跟点项目行一样：选中这个项目，有会话才开合。
                self.select_project(project.clone(), window, cx);
                if self.sidebar_container_open(&cursor, cx).is_some() {
                    self.toggle_sidebar_container(&cursor, cx);
                }
            }
            container => {
                if self.sidebar_container_open(container, cx).is_some() {
                    self.toggle_sidebar_container(container, cx);
                }
            }
        }
    }

    /// Esc：焦点回到正在看的会话。
    pub(crate) fn sidebar_exit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_active_stage(window, cx);
        cx.notify();
    }

    /// 打字跳转。一秒内连着打的字拼成一个前缀；新前缀从下一行找，接着打的从当前行找，
    /// 这样连按同一个字母能在同字母开头的几行间轮着走。只比时间戳，不起定时器。
    pub(crate) fn sidebar_typeahead(&mut self, text: &str, cx: &mut Context<Self>) {
        let now = Instant::now();
        let continuing = self
            .sidebar_typeahead
            .as_ref()
            .is_some_and(|(_, at)| now.duration_since(*at) < TYPEAHEAD_WINDOW);
        let prefix = match self.sidebar_typeahead.take() {
            Some((mut prefix, _)) if continuing && prefix != text => {
                prefix.push_str(text);
                prefix
            }
            _ => text.to_string(),
        };
        let (items, labels): (Vec<SidebarItem>, Vec<String>) =
            self.sidebar_items(cx).into_iter().unzip();
        let from = self.sidebar_cursor_index(&items).unwrap_or(0);
        let include_from = prefix.chars().count() > 1;
        if let Some(found) = typeahead_match(&labels, from, include_from, &prefix) {
            self.set_sidebar_cursor(Some(items[found].clone()), cx);
        }
        self.sidebar_typeahead = Some((prefix, now));
    }

    /// 容器行现在是不是展开的；不能开合的（空项目、会话行）是 `None`。
    fn sidebar_container_open(&self, item: &SidebarItem, cx: &App) -> Option<bool> {
        match item {
            SidebarItem::ConversationGroup => Some(!self.sidebar_conversations_collapsed),
            SidebarItem::Runtime(key) => Some(!self.runtime_folded(key)),
            SidebarItem::Group(project) => {
                let has_sessions = self
                    .project_groups(&project.runtime, cx)
                    .iter()
                    .any(|group| group.root == project.root && !group.sessions.is_empty())
                    || self.sidebar_grouping != SidebarGrouping::Project;
                let collapsed = self
                    .space(&project.runtime)
                    .is_some_and(|space| space.collapsed_projects.contains(&project.root));
                has_sessions.then_some(!collapsed)
            }
            _ => None,
        }
    }

    fn toggle_sidebar_container(&mut self, item: &SidebarItem, cx: &mut Context<Self>) {
        match item {
            SidebarItem::ConversationGroup => {
                self.sidebar_conversations_collapsed = !self.sidebar_conversations_collapsed;
                cx.notify();
            }
            SidebarItem::Runtime(key) => self.toggle_runtime_fold(key, cx),
            SidebarItem::Group(project) => self.toggle_project_collapsed(project, cx),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    // 不用 `use super::*`：它连带把上面 `use gpui::*` 里的 `gpui::test` 引进来，盖掉标准的
    // `#[test]`；`gpui::test` 展开出来又是 `#[test]`，编测试时无限展开。
    use super::{SidebarItem, parent_index, typeahead_match};
    use crate::{ProjectRef, RuntimeKey};

    fn group(root: &str) -> SidebarItem {
        SidebarItem::Group(ProjectRef {
            runtime: RuntimeKey::Local,
            root: root.to_string(),
        })
    }

    #[test]
    fn parents_follow_what_the_sidebar_draws() {
        let items = vec![
            SidebarItem::ConversationGroup,
            SidebarItem::Conversation(1),
            SidebarItem::Runtime(RuntimeKey::Local),
            group("/a"),
            SidebarItem::Session(2),
            SidebarItem::Session(3),
            group("/b"),
        ];
        assert_eq!(parent_index(&items, 1), Some(0));
        assert_eq!(parent_index(&items, 5), Some(3));
        assert_eq!(parent_index(&items, 3), Some(2));
        assert_eq!(parent_index(&items, 0), None);
        let without_machines = vec![
            SidebarItem::ConversationGroup,
            SidebarItem::Conversation(1),
            group("/a"),
        ];
        assert_eq!(
            parent_index(&without_machines, 2),
            None,
            "没有机器分段时项目行是顶层，不能回到「对话」"
        );
    }

    #[test]
    fn typeahead_wraps_and_cycles_through_same_letter() {
        let labels: Vec<String> = ["smelt", "employee", "smelt-cli", "pulse"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            typeahead_match(&labels, 0, false, "s"),
            Some(2),
            "新前缀从下一行找"
        );
        assert_eq!(
            typeahead_match(&labels, 2, false, "s"),
            Some(0),
            "找到底从头找"
        );
        assert_eq!(
            typeahead_match(&labels, 0, true, "SM"),
            Some(0),
            "不分大小写"
        );
        assert_eq!(typeahead_match(&labels, 0, true, "x"), None);
    }

    #[test]
    fn session_and_conversation_keys_are_their_ui_ids() {
        assert_eq!(SidebarItem::Session(7).key(), 7);
        assert_eq!(SidebarItem::Conversation(7).key(), 7);
        assert!(group("/a").key() >= 1 << 63);
        assert_ne!(group("/a").key(), group("/b").key());
    }
}
