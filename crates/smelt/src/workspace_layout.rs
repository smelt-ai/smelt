//! 工作台布局：装哪份布局、存它被点成的样子、跟着 Dock 的变化同步工作台。
//!
//! 整个工作台是一块 `DockArea`（`Workspace::shell_dock`，画法见 `workspace_shell`），布局文件
//! 和存档都是它官方的 `DockAreaState`，面板名就是视图 id（见 `smelt_plugin_api::view`）。
//! Dock 是唯一的一份：哪组选中哪个、放大、拆分大小都只在它那里，这里只读它、让它改，再把
//! `dump()` 原样存进库。眼下只有自带的默认布局（`workspace_layout/default.json`）。
//!
//! 两个信号：
//! - `DockEvent::LayoutChanged`（装布局、拖动、选中、拖分隔条）：存档，离开已经摆进布局的页面；
//! - Dock 的通知（放大、还原只发通知）：重算 [`ShellView`]。

use gpui::{App, Context, Entity, Subscription, Window};
use gpui_component::dock::{DockArea, DockAreaState, DockEvent, DockPlacement};
use smelt_client_store::WorkspaceLayoutStateRecord;

use crate::workspace_shell::{self, ShellView};
use crate::{Workspace, WorkspaceRoute};

/// 默认布局的 id，存档按它记。
const DEFAULT_ID: &str = "default";
/// 布局文件和存档的 `version`。面板的数据结构不兼容地变了才加。
pub(crate) const VERSION: usize = 1;
/// 程序里嵌的默认布局。
const DEFAULT_LAYOUT: &str = include_str!("workspace_layout/default.json");

fn default_layout() -> DockAreaState {
    parse_state(DEFAULT_LAYOUT).expect("内置默认布局是合法的")
}

/// 按官方格式解析，`version` 要是 [`VERSION`]。布局文件和存档共用。
fn parse_state(text: &str) -> Result<DockAreaState, String> {
    let state: DockAreaState =
        serde_json::from_str(text).map_err(|error| format!("不是合法的布局：{error}"))?;
    match state.version {
        Some(VERSION) => Ok(state),
        Some(version) => Err(format!("`version` 是 {version}，这个版本只认 {VERSION}")),
        None => Err(format!("没写 `version`，要写 {VERSION}")),
    }
}

/// 存档写进库里的文本。
fn to_text(state: &DockAreaState) -> String {
    serde_json::to_string(state).expect("DockAreaState 总能写成 JSON")
}

/// 存档能不能用：内容是当前版本的布局就用。布局文件后来改了也照用——那是用户自己摆的
/// 样子；要新版本的布局，扔掉存档（恢复）就是。
fn usable_saved_state(record: &WorkspaceLayoutStateRecord) -> Option<DockAreaState> {
    match parse_state(&record.state) {
        Ok(state) => Some(state),
        Err(error) => {
            eprintln!("[layout] 布局 `{}` 的存档不用：{error}", record.layout_id);
            None
        }
    }
}

/// 库里这份布局的存档和它的原文。读不出来、用不了都当没存过。
fn saved_state(id: &str) -> Option<(DockAreaState, String)> {
    let record = match smelt_client_store::client_store()
        .and_then(|store| store.workspace_layout_state(id))
    {
        Ok(record) => record?,
        Err(error) => {
            eprintln!("[layout] 读布局 `{id}` 的存档失败：{error}");
            return None;
        }
    };
    let state = usable_saved_state(&record)?;
    Some((state, record.state))
}

/// 订阅整个工作台的 Dock。
pub(crate) fn watch_shell_dock(
    area: &Entity<DockArea>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Vec<Subscription> {
    vec![
        cx.subscribe_in(area, window, |this, _, event: &DockEvent, window, cx| {
            if matches!(event, DockEvent::LayoutChanged) {
                this.shell_layout_changed(window, cx);
            }
        }),
        cx.observe(area, |this, _, cx| this.sync_shell_view(cx)),
    ]
}

impl Workspace {
    /// 装布局：有能用的存档装存档，否则装文件。视图要已经登记（自带视图在
    /// `Workspace::new` 里，插件在 `plugins::mount_all`），所以由 `Workspace::new` 推迟到
    /// 建完再调。
    pub(crate) fn install_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (state, written) = match saved_state(DEFAULT_ID) {
            Some((state, text)) => (state, Some(text)),
            None => (default_layout(), None),
        };
        // 按存档装的，装完 dump 出来和库里一样就不用再写。
        self.layout_state_written = written;
        self.layout_id = Some(DEFAULT_ID.to_string());
        self.load_shell_state(state, window, cx);
    }

    /// 视图登记变了（插件加载、卸载、替换）：按 Dock 现在的样子重装一遍，新登记的视图
    /// 补上，撤下的变占位。
    pub(crate) fn reload_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 还没装过布局：装的时候自然按最新的登记建。
        if self.layout_id.is_none() {
            return;
        }
        let state = self.shell_dock.read(cx).dump(cx);
        self.load_shell_state(state, window, cx);
    }

    fn load_shell_state(
        &mut self,
        state: DockAreaState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let area = self.shell_dock.clone();
        if let Err(error) = workspace_shell::load(&area, state, window, cx) {
            crate::status_item::notify_error(format!("布局没装上：{error:#}"));
        }
    }

    /// Dock 的树改了：存档；摆进布局的视图不再当舞台上的页面开着。
    fn shell_layout_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_shell_view(cx);
        self.save_layout_state(cx);
        if let WorkspaceRoute::Page(id) = self.active_tab()
            && self.shell_view.laid_out.contains(id.as_str())
        {
            let id = id.clone();
            self.activate_workspace_tab(WorkspaceRoute::Session, window, cx);
            self.show_view(&id, window, cx);
        }
    }

    /// 把 Dock 现在的样子（`dump()` 原样）存进库。和上次写的一样就不写。
    fn save_layout_state(&mut self, cx: &App) {
        let Some(layout_id) = self.layout_id.clone() else {
            return;
        };
        let state = to_text(&self.shell_dock.read(cx).dump(cx));
        if self.layout_state_written.as_deref() == Some(state.as_str()) {
            return;
        }
        let record = WorkspaceLayoutStateRecord {
            layout_id,
            state: state.clone(),
        };
        match smelt_client_store::client_store()
            .and_then(|store| store.put_workspace_layout_state(&record))
        {
            Ok(()) => self.layout_state_written = Some(state),
            Err(error) => eprintln!("[layout] 存布局状态失败：{error}"),
        }
    }

    /// 重算摆着什么、看得见什么。变了才重画；有视图刚露出来就刷一次它要的缓存。
    fn sync_shell_view(&mut self, cx: &mut Context<Self>) {
        let next = ShellView::of(self.shell_dock.read(cx), cx);
        if next == self.shell_view {
            return;
        }
        let revealed = next
            .shown
            .difference(&self.shell_view.shown)
            .next()
            .is_some();
        let stage_revealed = next
            .shown
            .contains(crate::builtin_views::Builtin::Stage.id())
            && !self
                .shell_view
                .shown
                .contains(crate::builtin_views::Builtin::Stage.id());
        self.shell_view = next;
        if revealed {
            self.schedule_visible_refresh(cx);
        }
        // 舞台被挡着时正在看的会话不算看过，露出来这一刻补标。
        if stage_revealed {
            self.mark_visible_session_read(cx);
        }
        cx.notify();
    }

    /// 让一个摆在布局里的视图露出来：选中它所在组里的它，它在收起的停靠区里就展开。不在
    /// 布局里返回 `false`。
    pub(crate) fn show_view(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let area = self.shell_dock.clone();
        let Some(panel) = workspace_shell::find_view(area.read(cx), id, cx) else {
            return false;
        };
        area.update(cx, |area, cx| {
            // 别的组放大着会挡住它。
            if area
                .zoomed_group()
                .is_some_and(|zoomed| workspace_shell::group_of(area, panel) != Some(zoomed))
            {
                area.set_zoomed_out(window, cx);
            }
            // 收起的停靠区里的先展开。
            if let Some(placement) = workspace_shell::placement_of(area, panel)
                && placement != DockPlacement::Center
                && !area.is_dock_open(placement)
            {
                area.toggle_dock(placement, window, cx);
            }
            area.select_panel(panel, window, cx);
        });
        true
    }

    /// 视图此刻看得见。
    pub(crate) fn view_shown(&self, id: &str) -> bool {
        self.shell_view.shown.contains(id)
    }

    /// Esc：还原放大的组。没放大着返回 `false`。
    pub(crate) fn unzoom_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.shell_dock.read(cx).is_zoomed() {
            return false;
        }
        self.shell_dock
            .update(cx, |area, cx| area.set_zoomed_out(window, cx));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{VERSION, default_layout, parse_state, to_text, usable_saved_state};
    use gpui_component::dock::{DockPlacement, PanelState};
    use smelt_client_store::WorkspaceLayoutStateRecord;

    fn views(state: &PanelState) -> Vec<String> {
        fn walk(state: &PanelState, names: &mut Vec<String>) {
            if state.children.is_empty() {
                names.push(state.panel_name.clone());
            }
            for child in &state.children {
                walk(child, names);
            }
        }
        let mut names = Vec::new();
        walk(state, &mut names);
        names
    }

    /// 会话列表在左停靠区、文件那组在右停靠区，都开着、能收起；舞台在中间。
    #[test]
    fn the_default_layout_puts_the_side_views_in_open_side_docks() {
        let state = default_layout();
        assert_eq!(state.version, Some(VERSION));
        assert_eq!(views(&state.center), ["stage"]);
        let left = state.left_dock.as_ref().expect("有左停靠区");
        assert_eq!(left.placement(), DockPlacement::Left);
        assert!(left.open());
        assert_eq!(views(left.panel()), ["sessions"]);
        let right = state.right_dock.as_ref().expect("有右停靠区");
        assert_eq!(right.placement(), DockPlacement::Right);
        assert!(right.open());
        assert_eq!(
            views(right.panel()),
            ["files", "git", "history", "com.smelt.skills"]
        );
        assert!(state.bottom_dock.is_none());
        for name in ["stage", "sessions", "files", "git", "history"] {
            assert!(
                crate::builtin_views::Builtin::ALL
                    .iter()
                    .any(|builtin| builtin.id() == name),
                "{name}"
            );
        }
    }

    /// 存档只要是当前版本的布局就用，和布局文件现在长什么样无关；版本不对、解析不了不用。
    #[test]
    fn a_saved_state_is_used_while_its_version_is_current() {
        let record = |state: String| WorkspaceLayoutStateRecord {
            layout_id: "default".into(),
            state,
        };
        let mut moved = default_layout();
        moved.left_dock = None;
        assert_eq!(
            usable_saved_state(&record(to_text(&moved))),
            Some(moved.clone())
        );
        let mut newer = moved;
        newer.version = Some(VERSION + 1);
        assert_eq!(usable_saved_state(&record(to_text(&newer))), None);
        assert_eq!(usable_saved_state(&record("{}".into())), None);
        assert!(parse_state("{}").is_err());
    }
}
