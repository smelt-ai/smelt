//! 工作台布局：装哪份布局、存它被点成的样子、跟着 Dock 的变化同步工作台。
//!
//! 整个工作台是一块 `DockArea`（`Workspace::shell_dock`，画法见 `workspace_shell`），布局和
//! 存档都是它官方的 `DockAreaState`，面板名就是视图 id（见 `smelt_plugin_api::view`）。
//! Dock 是唯一的一份：哪组选中哪个、放大、拆分大小都只在它那里，这里只读它、让它改，再把
//! `dump()` 原样存进库。
//!
//! 布局由插件登记（`smelt_plugin_api::register_layouts`，出厂的在内置插件 `smelt-layouts`），
//! 宿主不认识任何一份。装哪份：外观里选的（`Appearance::layout_id`）登记着就用它，否则用登记
//! 的第一份，一份都没有就把所有视图摆在中间一组（不存档）。选的布局暂时没登记（插件还没
//! 加载、被卸了）时不改选择，它登记上就换过去。
//!
//! 几个信号：
//! - `DockEvent::LayoutChanged`（装布局、拖动、选中、拖分隔条）：存档，离开已经摆进布局的页面；
//! - Dock 的通知（放大、还原只发通知）：重算 [`ShellView`]；
//! - 布局登记变了、外观里换了布局：重新挑一份，和装着的不同就换。

use gpui::{App, AppContext as _, Context, Entity, Subscription, Window};
use gpui_component::dock::{DockArea, DockAreaState, DockEvent, DockPlacement};
use smelt_client_store::WorkspaceLayoutStateRecord;
use smelt_plugin_api as api;

use crate::settings::Appearance;
use crate::workspace_shell::{self, ShellView};
use crate::{Workspace, WorkspaceRoute};

/// 工作台装着的布局。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum InstalledLayout {
    /// 还没装：工作台建好才装（见 [`Workspace::install_layout`]）。
    #[default]
    NotInstalled,
    /// 一份布局都没登记：所有视图摆在中间一组，不存档。
    Fallback,
    /// 登记着的这份，存档按它的 id 记。
    Registered(String),
}

/// 该装哪份：选的登记着就是它，否则是登记的第一份；一份都没有是 `None`。
fn resolve(chosen: Option<&str>, registered: &[api::LayoutInfo]) -> Option<String> {
    chosen
        .filter(|id| registered.iter().any(|info| info.id.as_ref() == *id))
        .or_else(|| registered.first().map(|info| info.id.as_ref()))
        .map(str::to_string)
}

/// 外观里选的布局。
fn chosen_layout(cx: &App) -> Option<String> {
    cx.try_global::<Appearance>()
        .and_then(|appearance| appearance.layout_id.clone())
}

/// 眼下该装的布局。只读内存，设置页显示「当前布局」也用它。
pub(crate) fn effective_layout(cx: &App) -> Option<String> {
    resolve(chosen_layout(cx).as_deref(), &api::layouts(cx))
}

/// 没有布局时的摆法：登记过的视图按登记顺序，全在中间一组。
fn fallback_layout(views: &[api::ViewSpec]) -> DockAreaState {
    let children = views
        .iter()
        .map(|view| {
            serde_json::json!({ "panel_name": view.id.as_ref(), "children": [], "info": { "panel": null } })
        })
        .collect::<Vec<_>>();
    let state = serde_json::json!({
        "version": api::LAYOUT_VERSION,
        "center": {
            "panel_name": "StackPanel",
            "info": { "stack": { "sizes": [820], "axis": 0 } },
            "children": [{
                "panel_name": "TabPanel",
                "info": { "tabs": { "active_index": 0 } },
                "children": children,
            }],
        },
    });
    serde_json::from_value(state).expect("兜底布局总是合法的")
}

/// 存档写进库里的文本。
fn to_text(state: &DockAreaState) -> String {
    serde_json::to_string(state).expect("DockAreaState 总能写成 JSON")
}

/// 存档能不能用：内容是当前版本的布局就用。布局后来改了也照用——那是用户自己摆的样子；
/// 要布局本来的样子，扔掉存档（恢复）就是。
fn usable_saved_state(record: &WorkspaceLayoutStateRecord) -> Option<DockAreaState> {
    match api::parse_layout(&record.state) {
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

/// 订阅整个工作台的 Dock，和外观里选的布局。
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
        // 外观的别的项（拖滑块）也会走到这里，比一下 id 就回去。
        cx.observe_global_in::<Appearance>(window, |this, window, cx| {
            this.sync_layout_choice(window, cx)
        }),
    ]
}

impl Workspace {
    /// 装眼下该装的布局（见 [`effective_layout`]）：有能用的存档装存档，否则装布局本来的
    /// 样子。视图要已经登记（自带视图在 `Workspace::new` 里，插件在 `plugins::mount_all`），
    /// 所以由 `Workspace::new` 推迟到建完再调。
    pub(crate) fn install_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (installed, state, written) = match effective_layout(cx) {
            Some(id) => {
                let (state, written) = match saved_state(&id) {
                    Some((state, text)) => (state, Some(text)),
                    None => (
                        api::layout(&id, cx).expect("effective_layout 只给登记着的布局"),
                        None,
                    ),
                };
                (InstalledLayout::Registered(id), state, written)
            }
            None => (
                InstalledLayout::Fallback,
                fallback_layout(&api::views(cx)),
                None,
            ),
        };
        // 按存档装的，装完 dump 出来和库里一样就不用再写。
        self.layout_state_written = written;
        self.layout = installed;
        self.load_shell_state(state, window, cx);
    }

    /// 布局登记变了、外观里换了布局：该装的和装着的不是同一份就换。装着的那份已经随每次
    /// 改动存过档，直接换。
    pub(crate) fn sync_layout_choice(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let installed = match &self.layout {
            // 还没装：装的时候自然按最新的选择挑。
            InstalledLayout::NotInstalled => return,
            InstalledLayout::Fallback => None,
            InstalledLayout::Registered(id) => Some(id.as_str()),
        };
        if effective_layout(cx).as_deref() != installed {
            self.install_layout(window, cx);
        }
    }

    /// 扔掉装着的这份布局的存档，回到它本来的样子。兜底的那份没有存档，什么都不做。
    ///
    /// 多半从设置窗口的按钮进来，正占着 `Workspace`：装布局要用工作台窗口，推到这一轮
    /// 更新结束后再进去。
    pub(crate) fn restore_layout(&mut self, cx: &mut Context<Self>) {
        let InstalledLayout::Registered(id) = self.layout.clone() else {
            return;
        };
        if let Err(error) = smelt_client_store::client_store()
            .and_then(|store| store.delete_workspace_layout_state(&id))
        {
            crate::status_item::notify_error(format!("恢复布局失败：{error}"));
            return;
        }
        let handle = self.window_handle;
        let workspace = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = cx.update_window(handle, |_, window, cx| {
                let _ = workspace.update(cx, |this, cx| {
                    // 等的这一会儿换了布局，就不是要恢复的那份了。
                    if this.layout == InstalledLayout::Registered(id) {
                        this.install_layout(window, cx);
                    }
                });
            });
        });
    }

    /// 视图登记变了（插件加载、卸载、替换）：按 Dock 现在的样子重装一遍，新登记的视图
    /// 补上，撤下的变占位。兜底的那份按眼下的视图重摆。
    pub(crate) fn reload_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.layout {
            // 还没装过布局：装的时候自然按最新的登记建。
            InstalledLayout::NotInstalled => {}
            InstalledLayout::Fallback => self.install_layout(window, cx),
            InstalledLayout::Registered(_) => {
                let state = self.shell_dock.read(cx).dump(cx);
                self.load_shell_state(state, window, cx);
            }
        }
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
        let InstalledLayout::Registered(layout_id) = self.layout.clone() else {
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
        self.shell_view = next;
        self.note_shown_views(cx);
        self.note_active_context(cx);
        if revealed {
            self.schedule_visible_refresh(cx);
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
    use super::{fallback_layout, resolve, to_text, usable_saved_state};
    use smelt_client_store::WorkspaceLayoutStateRecord;
    use smelt_plugin_api as api;

    fn view(id: &str) -> api::ViewSpec {
        api::ViewSpec {
            id: id.to_string().into(),
            title: id.to_string().into(),
            icon: "".into(),
            per_machine: false,
            draws_top_bar: false,
            build: std::rc::Rc::new(
                |_: &mut gpui::Window, _: &mut gpui::App| -> std::rc::Rc<dyn api::ViewHandle> {
                    unreachable!("测试里不建视图")
                },
            ),
        }
    }

    fn info(id: &str) -> api::LayoutInfo {
        api::LayoutInfo {
            id: id.to_string().into(),
            title: id.to_string().into(),
        }
    }

    /// 选的登记着就用它；没登记（插件还没加载）用第一份；一份都没有就是兜底。
    #[test]
    fn the_chosen_layout_is_used_only_while_registered() {
        let registered = [info("smelt/default"), info("trader/two")];
        assert_eq!(
            resolve(Some("trader/two"), &registered).as_deref(),
            Some("trader/two")
        );
        assert_eq!(
            resolve(Some("gone/x"), &registered).as_deref(),
            Some("smelt/default")
        );
        assert_eq!(resolve(None, &registered).as_deref(), Some("smelt/default"));
        assert_eq!(resolve(Some("trader/two"), &[]), None);
    }

    /// 兜底的那份能按官方格式解析，视图按登记顺序都在中间一组。
    #[test]
    fn the_fallback_puts_every_view_in_the_center() {
        let state = fallback_layout(&[view("sessions"), view("stage"), view("files")]);
        assert_eq!(state.version, Some(api::LAYOUT_VERSION));
        assert!(state.left_dock.is_none() && state.right_dock.is_none());
        let tabs = &state.center.children[0];
        assert_eq!(tabs.panel_name, "TabPanel");
        let names: Vec<_> = tabs
            .children
            .iter()
            .map(|panel| panel.panel_name.as_str())
            .collect();
        assert_eq!(names, ["sessions", "stage", "files"]);
        assert_eq!(api::parse_layout(&to_text(&state)), Ok(state));
    }

    /// 存档只要是当前版本的布局就用，和布局现在长什么样无关；版本不对、解析不了不用。
    #[test]
    fn a_saved_state_is_used_while_its_version_is_current() {
        let record = |state: String| WorkspaceLayoutStateRecord {
            layout_id: "smelt/default".into(),
            state,
        };
        // 和哪份登记的布局都不一样，照样用。
        let moved = fallback_layout(&[view("files")]);
        assert_eq!(
            usable_saved_state(&record(to_text(&moved))),
            Some(moved.clone())
        );
        let mut newer = moved;
        newer.version = Some(api::LAYOUT_VERSION + 1);
        assert_eq!(usable_saved_state(&record(to_text(&newer))), None);
        assert_eq!(usable_saved_state(&record("{}".into())), None);
    }
}
