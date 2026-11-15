//! 页面和布局：主窗口开着哪一页、每页装哪份布局、存它被点成的样子、跟着 Dock 的变化同步
//! 工作台。
//!
//! 主窗口一次显示一页（`smelt_plugin_api::page`），每页一块 `DockArea`（画法见
//! `workspace_shell`），布局和存档都是它官方的 `DockAreaState`，面板名就是视图 id（见
//! `smelt_plugin_api::view`）。开过的页的 Dock 都留着，切回来还是走时的样子。Dock 是唯一的
//! 一份：哪组选中哪个、放大、拆分大小都只在它那里，这里只读它、让它改，再把 `dump()` 原样
//! 存进库。
//!
//! 页和布局都由插件登记（出厂的工作台在内置插件 `smelt-layouts`，设置那页是宿主登记的），
//! 宿主不认识任何一页、任何一份。启动进首页。每页装哪份：首页用外观里选的
//! （`Appearance::layout_id`）——登记着、又是给首页的；否则用给这页登记的第一份。没有首页时
//! 所有没摆进别的页的视图摆在一页里兜底（不存档）。选的布局暂时没登记（插件还没加载、被
//! 卸了）时不改选择，它登记上就换过去。
//!
//! 路由（开着哪页、来路、各页出口里放着什么）在 [`crate::router::Router`]，这里按它装 Dock、
//! 露出视图。打开一个视图（`Workspace::open_view`）：哪页的布局里摆了它就去那页露出来；
//! 哪页都没摆就放进出口（`smelt_plugin_api::OUTLET_VIEW`）——开着的这页有出口放这页的，
//! 没有就去有出口的那页（先首页）。出口里换内容不换页、不记来路，没放东西时显示这页的默认
//! 内容（页登记时给的 `index`）。页头的关闭退回来的那页。⌘W 不管路由：它关的是看得见的
//! 会话。来路只记在内存里，重启回首页；首页出口里放着什么随界面状态存。
//!
//! 几个信号：
//! - `DockEvent::LayoutChanged`（装布局、拖动、选中、拖分隔条）：存那一页的档；开着的页里
//!   出口放着的视图被摆进布局了，出口放回默认内容、露出布局里的它；
//! - Dock 的通知（放大、还原只发通知）：开着的页重算 [`ShellView`]；
//! - 页、布局登记变了，外观里换了布局：开着的页撤下了就回去，各页重新挑一份，和装着的不同
//!   就换。

use std::collections::HashSet;

use gpui::{App, AppContext as _, Context, Entity, Subscription, Window};
use gpui_component::dock::{DockArea, DockAreaState, DockEvent, DockPlacement};
use smelt_client_store::WorkspaceLayoutStateRecord;
use smelt_plugin_api as api;

use crate::router::PageKey;
use crate::settings::Appearance;
use crate::workspace_shell::{self, ShellView};
use crate::{Workspace, plugin_host};

/// 一页装着的布局。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum InstalledLayout {
    /// 还没装：工作台建好才装（见 [`Workspace::open_home_page`]）。
    #[default]
    NotInstalled,
    /// 兜底的那页：没摆进别的页的视图都摆在中间一组，不存档。记着摆了哪些，登记变了比一下。
    Fallback(Vec<String>),
    /// 登记着的这份，存档按它的 id 记。
    Registered(String),
}

/// 开过的一页。
pub(crate) struct PageDock {
    page: PageKey,
    area: Entity<DockArea>,
    layout: InstalledLayout,
    /// 上次写进库的布局存档原文。一样就不再写。
    written: Option<String>,
    _subs: Vec<Subscription>,
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

/// 首页；没有首页是 `None`，就是兜底的那页。
pub(crate) fn home_page(cx: &App) -> PageKey {
    api::home_page(cx).map(|page| page.id.to_string())
}

/// 这一页出口的默认内容（页登记时给的 `index`）。兜底的那页没有。
fn page_index(page: &PageKey, cx: &App) -> Option<String> {
    let page = api::page(page.as_deref()?, cx)?;
    page.index.map(|index| index.to_string())
}

/// 找一个视图摆在哪页时看页的顺序：首页（没有首页是兜底的那页），再按登记顺序。
fn page_order(cx: &App) -> Vec<PageKey> {
    let home = home_page(cx);
    let mut pages = vec![home.clone()];
    pages.extend(
        api::pages(cx)
            .into_iter()
            .map(|page| Some(page.id.to_string()))
            .filter(|page| *page != home),
    );
    pages
}

/// 能放进出口的视图：登记着，不是设置页（设置页由设置摆）。
fn outlet_can_hold(id: &str, cx: &App) -> bool {
    api::view(id, cx).is_some_and(|view| view.settings.is_none())
}

/// 给这一页登记的布局，按登记顺序。只读内存。
pub(crate) fn page_layouts(page: &str, cx: &App) -> Vec<api::LayoutInfo> {
    api::layouts(cx)
        .into_iter()
        .filter(|info| info.page.as_ref() == page)
        .collect()
}

/// 给首页登记的布局，设置里让人从中选。
pub(crate) fn home_layouts(cx: &App) -> Vec<api::LayoutInfo> {
    home_page(cx)
        .map(|page| page_layouts(&page, cx))
        .unwrap_or_default()
}

/// 这一页眼下该装的布局；页没登记、没有给它的布局是 `None`。只读内存。
fn effective_layout(page: &str, cx: &App) -> Option<String> {
    if !api::pages(cx).iter().any(|info| info.id.as_ref() == page) {
        return None;
    }
    let chosen = (home_page(cx).as_deref() == Some(page))
        .then(|| chosen_layout(cx))
        .flatten();
    resolve(chosen.as_deref(), &page_layouts(page, cx))
}

/// 首页眼下该装的布局，设置页显示「当前布局」用。
pub(crate) fn effective_home_layout(cx: &App) -> Option<String> {
    effective_layout(&home_page(cx)?, cx)
}

/// 这一页眼下进得去：登记着、有给它的布局。兜底的那页只在首页进不去时进得去。
fn page_usable(page: &PageKey, cx: &App) -> bool {
    match page {
        Some(page) => effective_layout(page, cx).is_some(),
        None => effective_home_layout(cx).is_none(),
    }
}

/// 启动、来路都没了时落到的那页：首页，首页进不去就是兜底的那页。
fn landing_page(cx: &App) -> PageKey {
    home_page(cx).filter(|_| effective_home_layout(cx).is_some())
}

/// 一份布局里摆着的视图 id。面板树的叶子是视图，`StackPanel`、`TabPanel` 是框架。
fn layout_views(state: &DockAreaState) -> HashSet<String> {
    fn walk(node: &serde_json::Value, views: &mut HashSet<String>) {
        match node {
            serde_json::Value::Object(object) => {
                let leaf = object
                    .get("children")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(Vec::is_empty);
                if leaf
                    && let Some(name) = object.get("panel_name").and_then(serde_json::Value::as_str)
                    && name.contains('/')
                {
                    views.insert(name.to_string());
                }
                object.values().for_each(|child| walk(child, views));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|child| walk(child, views)),
            _ => {}
        }
    }
    let mut views = HashSet::new();
    walk(
        &serde_json::to_value(state).expect("DockAreaState 总能写成 JSON"),
        &mut views,
    );
    views
}

/// 兜底那页摆的视图：登记过的视图按登记顺序，设置页、出口（兜底那页没有默认内容给它放）、
/// 已经摆进哪页布局里的不摆。
fn fallback_views(views: &[api::ViewInfo], elsewhere: &HashSet<String>) -> Vec<String> {
    views
        .iter()
        .filter(|view| {
            view.settings.is_none()
                && view.id.as_ref() != api::OUTLET_VIEW
                && !elsewhere.contains(view.id.as_ref())
        })
        .map(|view| view.id.to_string())
        .collect()
}

/// 兜底那页眼下该摆的视图。只读内存。
fn current_fallback_views(cx: &App) -> Vec<String> {
    fallback_views(&api::views(cx), &registered_layout_views(cx))
}

/// 兜底那页的摆法：这些视图全在中间一组。
fn fallback_layout(views: &[String]) -> DockAreaState {
    let children = views
        .iter()
        .map(
            |id| serde_json::json!({ "panel_name": id, "children": [], "info": { "panel": null } }),
        )
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

/// 登记过的布局里摆着的视图，兜底那页不摆它们。
fn registered_layout_views(cx: &App) -> HashSet<String> {
    api::layouts(cx)
        .iter()
        .filter_map(|info| api::layout(&info.id, cx))
        .flat_map(|state| layout_views(&state))
        .collect()
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

impl Workspace {
    /// 一页空的 Dock，还没装布局。
    pub(crate) fn new_page_dock(
        page: PageKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PageDock {
        let area = workspace_shell::new_area(cx.weak_entity(), window, cx);
        let subs = Self::watch_page_dock(page.clone(), &area, window, cx);
        PageDock {
            page,
            area,
            layout: InstalledLayout::NotInstalled,
            written: None,
            _subs: subs,
        }
    }

    /// 订阅一页的 Dock：它的树改了存档，开着时跟着重算看得见什么。外观里换布局另订一次
    /// （[`Self::watch_layout_choice`]）。
    fn watch_page_dock(
        page: PageKey,
        area: &Entity<DockArea>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        let changed = page.clone();
        vec![
            cx.subscribe_in(
                area,
                window,
                move |this, _, event: &DockEvent, window, cx| {
                    if matches!(event, DockEvent::LayoutChanged) {
                        this.page_layout_changed(&changed, window, cx);
                    }
                },
            ),
            cx.observe(area, move |this, _, cx| {
                if this.router.page() == &page {
                    this.sync_shell_view(cx);
                }
            }),
        ]
    }

    /// 外观里选的布局变了就重挑。外观的别的项（拖滑块）也会走到这里，比一下就回去。
    pub(crate) fn watch_layout_choice(window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.observe_global_in::<Appearance>(window, |this, window, cx| this.sync_pages(window, cx))
    }

    /// 开着的那页的 Dock。
    pub(crate) fn shell_dock(&self) -> &Entity<DockArea> {
        &self.pages[self
            .page_ix(self.router.page())
            .expect("开着的那页总在 pages 里")]
        .area
    }

    fn page_ix(&self, page: &PageKey) -> Option<usize> {
        self.pages.iter().position(|dock| &dock.page == page)
    }

    /// 工作台建好后进首页，首页出口放上次放着的东西。视图要已经登记（自带视图在
    /// `Workspace::new` 里，插件在 `plugins::mount_all`），所以由 `Workspace::new` 推迟到
    /// 建完再调。
    pub(crate) fn open_home_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let landing = landing_page(cx);
        if let Some(view) = self.startup_outlet.take()
            && outlet_can_hold(&view, cx)
        {
            self.router.set_outlet(&landing, Some(view));
        }
        self.land(window, cx);
        // 建工作台时那页空的没用上就放掉。
        let current = self.router.page().clone();
        self.pages
            .retain(|dock| dock.page == current || dock.layout != InstalledLayout::NotInstalled);
    }

    /// 进一页，记下来路（见 [`crate::router::Router::navigate`]）。进不去（页没登记、没有
    /// 给它的布局）返回 `false`，还停在原来那页。
    pub(crate) fn navigate(
        &mut self,
        page: PageKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.router.page() == &page {
            return true;
        }
        if !self.prepare_page(&page, window, cx) {
            return false;
        }
        self.router.navigate(page);
        self.page_shown(window, cx);
        true
    }

    /// 回到来的那页（页头的关闭）；来路上进不去的跳过，都没了回首页。
    pub(crate) fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(back) = self.router.pop_back() {
            if page_usable(&back, cx) && self.prepare_page(&back, window, cx) {
                self.router.arrive(back);
                self.page_shown(window, cx);
                return;
            }
        }
        self.land(window, cx);
    }

    /// 落到首页（首页进不去是兜底的那页），来路清空。
    fn land(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let page = landing_page(cx);
        // 落脚的那页总进得去：首页进不去时是兜底的那页，它总能装上。
        self.prepare_page(&page, window, cx);
        self.router.land(page);
        self.page_shown(window, cx);
    }

    /// 备好这一页的 Dock：开过的直接用，没开过的建 Dock、装布局。装不上返回 `false`。开过的
    /// 页进不进得去由调用方先看（[`page_usable`]）。
    fn prepare_page(
        &mut self,
        page: &PageKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let ix = match self.page_ix(page) {
            Some(ix) => ix,
            None => {
                let dock = Self::new_page_dock(page.clone(), window, cx);
                self.pages.push(dock);
                self.pages.len() - 1
            }
        };
        if self.pages[ix].layout == InstalledLayout::NotInstalled
            && !self.install_layout(ix, window, cx)
        {
            self.pages.remove(ix);
            return false;
        }
        true
    }

    /// 路由换到了一页（或开着的这页刚装上）：焦点先收回工作台——可能还在换下去的那页里，
    /// 出口里的视图建好，露着舞台就把焦点还给会话。
    fn page_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        self.sync_outlet(window, cx);
        if self.view_shown(crate::builtin_views::Builtin::Stage.id()) {
            self.focus_active_stage(window, cx);
        }
        cx.notify();
    }

    /// 开着的这页的出口放这个视图；`None`、这页的默认内容都是放回默认内容。换内容不记来路。
    fn set_outlet(&mut self, view: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let page = self.router.page().clone();
        let view = view.filter(|view| page_index(&page, cx).as_ref() != Some(view));
        if self.router.set_outlet(&page, view) {
            self.sync_outlet(window, cx);
            self.save_state(cx);
            cx.notify();
        }
    }

    /// 出口对上注册表：撤下的视图不再放，出口里该放的视图建好（新建的打开一次，让它读
    /// 数据），再重算看得见什么。换页、出口换内容、视图登记变了时调。
    pub(crate) fn sync_outlet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dropped = self.router.retain_views(|id| outlet_can_hold(id, cx));
        self.sync_shell_view(cx);
        if let Some(id) = self.shell_view.outlet.clone()
            && let Some((view, true)) = api::ensure_view(&id, window, cx)
        {
            view.open(&serde_json::Value::Null, window, cx);
            cx.notify();
        }
        if dropped {
            self.save_state(cx);
        }
    }

    /// 首页出口里放着的视图，随界面状态存；远程桌面是现场，重启后不自动连回去。
    pub(crate) fn persisted_outlet(&self, cx: &App) -> Option<String> {
        self.router
            .outlet(&home_page(cx))
            .filter(|view| *view != crate::desktop_view::DESKTOP_VIEW)
            .map(str::to_string)
    }

    /// 给第 `ix` 页装眼下该装的布局：有能用的存档装存档，否则装布局本来的样子。登记过
    /// 的页没有给它的布局时装不了，返回 `false`。
    fn install_layout(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (installed, state, written) = match self.pages[ix].page.as_deref() {
            Some(page) => {
                let Some(id) = effective_layout(page, cx) else {
                    return false;
                };
                let (state, written) = match saved_state(&id) {
                    Some((state, text)) => (state, Some(text)),
                    None => (
                        api::layout(&id, cx).expect("effective_layout 只给登记着的布局"),
                        None,
                    ),
                };
                (InstalledLayout::Registered(id), state, written)
            }
            None => {
                let views = current_fallback_views(cx);
                let state = fallback_layout(&views);
                (InstalledLayout::Fallback(views), state, None)
            }
        };
        let dock = &mut self.pages[ix];
        // 按存档装的，装完 dump 出来和库里一样就不用再写。
        dock.written = written;
        dock.layout = installed;
        let area = dock.area.clone();
        load_state(&area, state, window, cx);
        true
    }

    /// 页、布局登记变了，外观里换了布局：进不去的页的 Dock 放掉、从来路上拿掉，开着的那页
    /// 进不去了就回去（有了首页，兜底的那页也算进不去）；各页该装的和装着的不同就换。
    /// 装着的那份已经随每次改动存过档，直接换。外观里拖滑块也走到这里，比一下就回去。
    pub(crate) fn sync_pages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .pages
            .iter()
            .all(|dock| dock.layout == InstalledLayout::NotInstalled)
        {
            // 还没进首页：进的时候自然按最新的登记挑。
            return;
        }
        let current = self.router.page().clone();
        self.pages
            .retain(|dock| dock.page == current || page_usable(&dock.page, cx));
        self.router.retain_pages(|page| page_usable(page, cx));
        if !page_usable(&current, cx) {
            self.go_back(window, cx);
            self.pages.retain(|dock| dock.page != current);
        }
        for ix in 0..self.pages.len() {
            let stale = match &self.pages[ix].layout {
                InstalledLayout::NotInstalled => false,
                InstalledLayout::Fallback(views) => *views != current_fallback_views(cx),
                InstalledLayout::Registered(id) => {
                    let page = self.pages[ix].page.as_deref();
                    page.and_then(|page| effective_layout(page, cx)).as_ref() != Some(id)
                }
            };
            if stale {
                self.install_layout(ix, window, cx);
            }
        }
        // 别的页摆着什么、出口的默认内容跟着登记变。
        self.sync_outlet(window, cx);
    }

    /// 扔掉首页装着的这份布局的存档，回到它本来的样子。首页还没开过就只扔存档；兜底的
    /// 那页没有存档，什么都不做。
    ///
    /// 多半从设置页的按钮进来，正占着 `Workspace`：装布局要用工作台窗口，推到这一轮
    /// 更新结束后再进去。
    pub(crate) fn restore_layout(&mut self, cx: &mut Context<Self>) {
        let Some(id) = effective_home_layout(cx) else {
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
                    let installed = InstalledLayout::Registered(id);
                    if let Some(ix) = this.pages.iter().position(|dock| dock.layout == installed) {
                        this.install_layout(ix, window, cx);
                    }
                });
            });
        });
    }

    /// 视图登记变了（插件加载、卸载、替换）：各页按 Dock 现在的样子重装一遍，新登记的
    /// 视图补上，撤下的变占位。兜底的那页按眼下的视图重摆。
    pub(crate) fn reload_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for ix in 0..self.pages.len() {
            match self.pages[ix].layout {
                // 还没装过布局：装的时候自然按最新的登记建。
                InstalledLayout::NotInstalled => {}
                InstalledLayout::Fallback(_) => {
                    self.install_layout(ix, window, cx);
                }
                InstalledLayout::Registered(_) => {
                    let area = self.pages[ix].area.clone();
                    let state = area.read(cx).dump(cx);
                    load_state(&area, state, window, cx);
                }
            }
        }
    }

    /// 一页的 Dock 树改了：存档；是开着的那页，出口里放着的视图被摆进布局了就不再放出口，
    /// 露出布局里的它。
    fn page_layout_changed(&mut self, page: &PageKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.page_ix(page) else {
            return;
        };
        self.save_layout_state(ix, cx);
        if self.router.page() != page {
            return;
        }
        self.sync_shell_view(cx);
        if let Some(id) = self.router.outlet(page).map(str::to_string)
            && self.shell_view.laid_out.contains(&id)
        {
            self.set_outlet(None, window, cx);
            self.show_view(&id, window, cx);
        }
    }

    /// 把第 `ix` 页 Dock 现在的样子（`dump()` 原样）存进库。和上次写的一样就不写。
    fn save_layout_state(&mut self, ix: usize, cx: &App) {
        let dock = &self.pages[ix];
        let InstalledLayout::Registered(layout_id) = dock.layout.clone() else {
            return;
        };
        let state = to_text(&dock.area.read(cx).dump(cx));
        if dock.written.as_deref() == Some(state.as_str()) {
            return;
        }
        let record = WorkspaceLayoutStateRecord {
            layout_id,
            state: state.clone(),
        };
        match smelt_client_store::client_store()
            .and_then(|store| store.put_workspace_layout_state(&record))
        {
            Ok(()) => self.pages[ix].written = Some(state),
            Err(error) => eprintln!("[layout] 存布局状态失败：{error}"),
        }
    }

    /// 重算开着的那页摆着什么、出口里放着什么、看得见什么。变了才重画；有视图刚露出来就
    /// 刷一次它要的缓存。出口里的视图建没建由 [`Self::sync_outlet`] 管。
    fn sync_shell_view(&mut self, cx: &mut Context<Self>) {
        let mut next = ShellView::of(self.shell_dock().read(cx), cx);
        next.elsewhere = self.views_on_other_pages(cx);
        next.outlet = self.outlet_content(&next.laid_out, cx);
        if let Some(content) = &next.outlet
            && next.shown.contains(api::OUTLET_VIEW)
        {
            next.shown.insert(content.clone());
        }
        if next == self.shell_view {
            return;
        }
        let revealed = next
            .shown
            .difference(&self.shell_view.shown)
            .next()
            .is_some();
        self.shell_view = next;
        plugin_host::set_window_shown(self.shell_view.shown.clone(), cx);
        self.note_active_context(cx);
        if revealed {
            self.schedule_visible_refresh(cx);
        }
        cx.notify();
    }

    /// 开着的这页出口里显示的视图：路由给这页放的，没放就是这页的默认内容。这页没摆出口、
    /// 视图没登记、它已经摆在这页的布局里（一个视图只画在一处）时是 `None`。只读内存。
    fn outlet_content(&self, laid_out: &HashSet<String>, cx: &App) -> Option<String> {
        if !laid_out.contains(api::OUTLET_VIEW) {
            return None;
        }
        let page = self.router.page();
        self.router
            .outlet(page)
            .map(str::to_string)
            .or_else(|| page_index(page, cx))
            .filter(|id| !laid_out.contains(id) && api::is_registered(id, cx))
    }

    /// 别的页的布局里摆着的视图：开过的看它的 Dock，没开过的看它会装的那份登记的样子
    /// （存档只改摆法、不增减视图）。兜底的那页不算：它摆的就是别处都没摆的。只读内存。
    fn views_on_other_pages(&self, cx: &App) -> HashSet<String> {
        let mut views = HashSet::new();
        for page in api::pages(cx) {
            let page: PageKey = Some(page.id.to_string());
            if &page == self.router.page() {
                continue;
            }
            if let Some(ix) = self.page_ix(&page)
                && self.pages[ix].layout != InstalledLayout::NotInstalled
            {
                let area = self.pages[ix].area.read(cx);
                views.extend(ShellView::of(area, cx).laid_out);
            } else if let Some(state) = page
                .as_deref()
                .and_then(|page| effective_layout(page, cx))
                .and_then(|layout| api::layout(&layout, cx))
            {
                views.extend(layout_views(&state));
            }
        }
        views
    }

    /// 打开一个视图：设置页在设置里打开、停在这一页；别的视图露出来（[`Self::reveal_view`]）。
    /// 视图交给它参数再打开一次。没登记过这个 id、哪都放不下返回 `false`。
    pub(crate) fn open_view(
        &mut self,
        id: &str,
        args: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(info) = api::view(id, cx) else {
            return false;
        };
        // 先建好再露出来：新建的只按这次的参数打开一次。
        let Some((view, _)) = api::ensure_view(id, window, cx) else {
            return false;
        };
        if info.settings.is_some() {
            self.open_settings_section(
                crate::settings::SettingsSection::Plugin(info.id),
                window,
                cx,
            );
        } else if !self.reveal_view(id, window, cx) {
            return false;
        }
        view.open(&args, window, cx);
        true
    }

    /// 让一个视图露出来：哪页的布局里摆了它就在那页露（先开着的这页，再按 [`page_order`]）；
    /// 哪页都没摆就放进出口——开着的这页有出口放这页的，没有就去有出口的那页。设置页、哪都
    /// 放不下返回 `false`。
    pub(crate) fn reveal_view(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.show_view(id, window, cx) {
            return true;
        }
        let current = self.router.page().clone();
        let pages = page_order(cx);
        if let Some(page) = pages.iter().find(|page| {
            **page != current && page_usable(page, cx) && self.page_lays_out(page, id, cx)
        }) {
            return self.navigate(page.clone(), window, cx) && self.show_view(id, window, cx);
        }
        if !outlet_can_hold(id, cx) {
            return false;
        }
        if !self.page_lays_out(&current, api::OUTLET_VIEW, cx) {
            let Some(page) = pages.into_iter().find(|page| {
                page != &current
                    && page_usable(page, cx)
                    && self.page_lays_out(page, api::OUTLET_VIEW, cx)
            }) else {
                return false;
            };
            if !self.navigate(page, window, cx) {
                return false;
            }
        }
        self.set_outlet(Some(id.to_string()), window, cx);
        self.show_view(id, window, cx)
    }

    /// 出口放回开着的这页的默认内容。远程桌面页头的关闭用。
    pub(crate) fn reset_outlet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_outlet(None, window, cx);
    }

    /// 这一页的布局里摆着这个视图。开过的看它的 Dock，没开过的看它会装的那份登记的样子
    /// （存档只改摆法、不增减视图）。只读内存。
    fn page_lays_out(&self, page: &PageKey, id: &str, cx: &App) -> bool {
        if let Some(ix) = self.page_ix(page)
            && self.pages[ix].layout != InstalledLayout::NotInstalled
        {
            return workspace_shell::find_view(self.pages[ix].area.read(cx), id, cx).is_some();
        }
        match page.as_deref() {
            Some(page) => effective_layout(page, cx)
                .and_then(|layout| api::layout(&layout, cx))
                .is_some_and(|state| layout_views(&state).contains(id)),
            None => current_fallback_views(cx).iter().any(|view| view == id),
        }
    }

    /// 让开着的这页里摆着的一个视图露出来：选中它所在组里的它（出口里显示着的就是出口），
    /// 它在收起的停靠区里就展开。这页没摆、出口里也不是它返回 `false`。
    pub(crate) fn show_view(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let area = self.shell_dock().clone();
        let panel = self.shell_view.panel_of(id);
        let Some(panel) = workspace_shell::find_view(area.read(cx), panel, cx) else {
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

    /// 视图此刻看得见：所在组显示着它，或者它在显示着的出口里。
    pub(crate) fn view_shown(&self, id: &str) -> bool {
        self.shell_view.shown.contains(id)
    }

    /// Esc：还原开着的这页放大的组。没放大着返回 `false`。
    pub(crate) fn unzoom_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let area = self.shell_dock().clone();
        if !area.read(cx).is_zoomed() {
            return false;
        }
        area.update(cx, |area, cx| area.set_zoomed_out(window, cx));
        true
    }
}

fn load_state(area: &Entity<DockArea>, state: DockAreaState, window: &mut Window, cx: &mut App) {
    if let Err(error) = workspace_shell::load(area, state, window, cx) {
        crate::status_item::notify_error(format!("布局没装上：{error:#}"));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        fallback_layout, fallback_views, layout_views, resolve, to_text, usable_saved_state,
    };
    use smelt_client_store::WorkspaceLayoutStateRecord;
    use smelt_plugin_api as api;

    fn view(id: &str) -> api::ViewInfo {
        api::ViewInfo {
            id: id.to_string().into(),
            title: id.to_string().into(),
            icon: "".into(),
            per_machine: false,
            draws_top_bar: false,
            settings: None,
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
            page: "smelt.layouts/workbench".into(),
            title: id.to_string().into(),
        }
    }

    /// 选的登记着就用它；没登记（插件还没加载）用第一份；一份都没有就是兜底。
    #[test]
    fn the_chosen_layout_is_used_only_while_registered() {
        let registered = [info("smelt.layouts/standard"), info("trader/two")];
        assert_eq!(
            resolve(Some("trader/two"), &registered).as_deref(),
            Some("trader/two")
        );
        assert_eq!(
            resolve(Some("gone/x"), &registered).as_deref(),
            Some("smelt.layouts/standard")
        );
        assert_eq!(
            resolve(None, &registered).as_deref(),
            Some("smelt.layouts/standard")
        );
        assert_eq!(resolve(Some("trader/two"), &[]), None);
    }

    /// 兜底的那页能按官方格式解析，视图按登记顺序都在中间一组；设置页、摆进别的页的视图
    /// 不摆。
    #[test]
    fn the_fallback_puts_every_view_in_the_center() {
        let mut page = view("notify/main");
        page.settings = Some(api::SettingsEntry::default());
        let elsewhere = HashSet::from(["smelt/settings".to_string()]);
        let views = fallback_views(
            &[
                view("smelt.sessions/list"),
                page,
                view("smelt/settings"),
                view("smelt.sessions/stage"),
                view("smelt.files/main"),
            ],
            &elsewhere,
        );
        let state = fallback_layout(&views);
        assert_eq!(state.version, Some(api::LAYOUT_VERSION));
        assert!(state.left_dock.is_none() && state.right_dock.is_none());
        let tabs = &state.center.children[0];
        assert_eq!(tabs.panel_name, "TabPanel");
        let names: Vec<_> = tabs
            .children
            .iter()
            .map(|panel| panel.panel_name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "smelt.sessions/list",
                "smelt.sessions/stage",
                "smelt.files/main"
            ]
        );
        assert_eq!(api::parse_layout(&to_text(&state)), Ok(state));
    }

    /// 布局里摆着的视图是面板树的叶子，框架（`StackPanel`、`TabPanel`）不算，各个停靠区里
    /// 的都算。
    #[test]
    fn layout_views_are_the_leaves_of_every_dock() {
        let state = api::parse_layout(
            r#"{
              "version": 1,
              "center": { "panel_name": "StackPanel", "info": { "stack": { "sizes": [820], "axis": 0 } },
                "children": [{ "panel_name": "TabPanel", "info": { "tabs": { "active_index": 0 } },
                  "children": [{ "panel_name": "smelt.sessions/stage", "children": [], "info": { "panel": null } }] }] },
              "left_dock": { "placement": "left", "size": 260.0, "open": false,
                "panel": { "panel_name": "TabPanel", "info": { "tabs": { "active_index": 0 } },
                  "children": [{ "panel_name": "smelt.sessions/list", "children": [], "info": { "panel": null } }] } }
            }"#,
        )
        .unwrap();
        assert_eq!(
            layout_views(&state),
            HashSet::from([
                "smelt.sessions/stage".to_string(),
                "smelt.sessions/list".to_string()
            ])
        );
    }

    /// 存档只要是当前版本的布局就用，和布局现在长什么样无关；版本不对、解析不了不用。
    #[test]
    fn a_saved_state_is_used_while_its_version_is_current() {
        let record = |state: String| WorkspaceLayoutStateRecord {
            layout_id: "smelt.layouts/standard".into(),
            state,
        };
        // 和哪份登记的布局都不一样，照样用。
        let moved = fallback_layout(&["smelt.files/main".to_string()]);
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
