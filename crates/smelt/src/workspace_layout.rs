//! 页面和布局：主窗口开着哪一页、每页装哪份布局、存它被点成的样子、跟着 Dock 的变化同步
//! 工作台。
//!
//! 主窗口一次显示一页（`smelt_plugin_api::page`），每页一块 `DockArea`（画法见
//! `workspace_shell`），布局和存档都是它官方的 `DockAreaState`，面板名就是视图 id（见
//! `smelt_plugin_api::view`）。开过的页的 Dock 都留着，切回来还是走时的样子。Dock 是唯一的
//! 一份：哪组选中哪个、放大、拆分大小都只在它那里，这里只读它、让它改，再把 `dump()` 原样
//! 存进库。
//!
//! 页和布局都由插件登记（出厂的工作台在内置插件 `plugins/layouts`，设置那页是宿主登记的），
//! 宿主不认识任何一页、任何一份。启动进首页。每页装哪份：首页用外观里选的
//! （`Appearance::layout_id`）——登记着、又是给首页的；否则用给这页登记的第一份。没有可用
//! 的首页布局时不硬拼视图，直接展示空态/排错页并引导打开设置的「面板插件」；选的布局暂时没
//! 登记（插件还没加载、被卸了）时不改选择，它登记上就自动换过去。
//!
//! 路由（开着哪页、来路、各页出口里放着什么）在 [`crate::router::Router`]，这里按它装 Dock、
//! 露出视图。打开一个视图（`Workspace::open_view`）：哪页的布局里摆了它就去那页露出来；
//! 哪页都没摆就放进出口（`smelt_plugin_api::OUTLET_VIEW`）——开着的这页有出口放这页的，
//! 没有就去有出口的那页（先首页）。出口里换内容不换页、不记来路，没放东西时显示这页的默认
//! 内容（页登记时给的 `index`）。页头的关闭退回来的那页。⌘W 不管路由。来路只记在内存里，
//! 重启回首页；首页出口里放着什么随界面状态存。
//!
//! 几个信号：
//! - `DockEvent::LayoutChanged`（装布局、拖动、选中、拖分隔条）：存那一页的档；开着的页里
//!   出口放着的视图被摆进布局了，出口放回默认内容、露出布局里的它；
//! - Dock 的通知（放大、还原只发通知）：开着的页重算 [`ShellView`]；
//! - 页、布局登记变了，外观里换了布局：开着的页撤下了就回去，各页重新挑一份，和装着的不同
//!   就换。

use rust_i18n::t;
use std::collections::HashSet;

use gpui::{
    App, AppContext as _, Context, Div, Focusable as _, InteractiveElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, WindowControlArea, div, px, rgb, svg,
};
use smelt_client_store::WorkspaceLayoutStateRecord;
use smelt_plugin_api::{self as api, DockAreaState};
use smelt_ui::{
    IconName,
    kit::{RADIUS, TYPE, TypeStyled as _},
};

use crate::settings::Appearance;
use crate::workspace_shell::ShellView;
use crate::{Workspace, plugin_host};

/// 开过的一页（布局树由 `Workspace::nsh_shell` 托管，这里记页 id、已装布局和存档原文）。
pub(crate) struct PageDock {
    page: String,
    /// 当前装上的已登记布局 id，存档按它的 id 记。
    layout_id: String,
    /// 装的那份登记时摆着的视图和摞。插件替换了同一份布局、这些变了，就得重装。
    frame: LayoutFrame,
    /// 上次写进库的布局存档原文。一样就不再写。
    written: Option<String>,
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

/// 首页；没有登记首页时是 `None`。
pub(crate) fn home_page(cx: &App) -> Option<String> {
    api::home_page(cx).map(|page| page.id.to_string())
}

/// 这一页出口的默认内容（页登记时给的 `index`）。
fn page_index(page: &str, cx: &App) -> Option<String> {
    let page = api::page(page, cx)?;
    page.index.map(|index| index.to_string())
}

/// 找一个视图摆在哪页时看页的顺序：先首页，再按登记顺序。
fn page_order(cx: &App) -> Vec<String> {
    let home = home_page(cx);
    let mut pages = Vec::new();
    if let Some(home_id) = &home {
        pages.push(home_id.clone());
    }
    pages.extend(
        api::pages(cx)
            .into_iter()
            .map(|page| page.id.to_string())
            .filter(|page| home.as_deref() != Some(page.as_str())),
    );
    pages
}

/// 能按 id 放进出口的视图：登记着，不是设置页（设置页由设置摆），也不按实例建（那种带着
/// 建好的一份放进来，见 [`Workspace::open_instance`]）。
fn outlet_can_hold(id: &str, cx: &App) -> bool {
    api::view(id, cx).is_some_and(|view| view.settings.is_none() && !view.instances)
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

/// 这一页眼下进得去：登记着、有给它的布局。
fn page_usable(page: &str, cx: &App) -> bool {
    effective_layout(page, cx).is_some()
}

/// 启动、来路都没了时落到的那页：有可用布局的首页；否则 `None`。
fn landing_page(cx: &App) -> Option<String> {
    home_page(cx).filter(|_| effective_home_layout(cx).is_some())
}

/// 一份布局里存档动不了的部分：摆了哪些视图，各摞（`api::STACK_PANEL`）里排的什么。存档
/// 只改摆法（组、顺序、大小、选中），摞里拖不动；这部分和登记的不同，就是布局升级了。
#[derive(Clone, Debug, Default, PartialEq)]
struct LayoutFrame {
    views: HashSet<String>,
    /// 各摞写回 JSON 的样子，排好序：摞整块挪到别处不算变。
    stacks: Vec<String>,
}

fn layout_frame(state: &DockAreaState) -> LayoutFrame {
    let mut stacks: Vec<String> = api::layout_stacks(state)
        .iter()
        .map(|stack| serde_json::to_string(stack).expect("摞总能写成 JSON"))
        .collect();
    stacks.sort();
    LayoutFrame {
        views: layout_views(state),
        stacks,
    }
}

/// 一份布局里摆着的视图 id：面板树的叶子，和摞里的每一项。`StackPanel`、`TabPanel` 是框架。
fn layout_views(state: &DockAreaState) -> HashSet<String> {
    fn walk(panel: &api::PanelState, views: &mut HashSet<String>) {
        if let Some(stack) = api::Stack::of(panel) {
            if let Ok(stack) = stack {
                views.extend(stack.views().into_iter().map(str::to_string));
            }
            return;
        }
        if panel.children.is_empty() && panel.panel_name.contains('/') {
            views.insert(panel.panel_name.clone());
        }
        panel.children.iter().for_each(|child| walk(child, views));
    }
    let mut views = HashSet::new();
    walk(&state.center, &mut views);
    for dock in [&state.left_dock, &state.right_dock, &state.bottom_dock]
        .into_iter()
        .flatten()
    {
        walk(dock.panel(), &mut views);
    }
    views
}

/// 各页眼下该装的布局里摆着的视图。没摆的视图打开时放进出口。按登记的样子算：存档只改摆法、
/// 不增减视图（见 [`saved_state`]）。只读内存。
fn laid_out_views(cx: &App) -> HashSet<String> {
    page_order(cx)
        .iter()
        .filter(|page| page_usable(page, cx))
        .flat_map(|page| {
            effective_layout(page, cx)
                .and_then(|id| api::layout(&id, cx))
                .map(|state| layout_views(&state))
                .unwrap_or_default()
        })
        .collect()
}

/// 重算各页摆着的视图，交给插件（`Host::view_laid_out`）。页、布局登记变了，外观里换了
/// 布局时调；没变不发事件。
pub(crate) fn sync_laid_out_views(cx: &mut App) {
    plugin_host::set_laid_out(laid_out_views(cx), cx);
}

/// 存档写进库里的文本。
fn to_text(state: &DockAreaState) -> String {
    serde_json::to_string(state).expect("DockAreaState 总能写成 JSON")
}

/// 存档能不能用：是当前版本的布局、摆的视图和摞（[`LayoutFrame`]）和布局本来的一样就用。
/// 拖动、换组、改大小都只改摆法，照用；布局登记后增减了视图、改了摞（升级带来新的一块、
/// 把几格并成一摞），存档就过时了，不用，装布局本来的样子——不然布局的升级永远到不了已经
/// 存过档的人那里。
fn usable_saved_state(
    record: &WorkspaceLayoutStateRecord,
    registered: &DockAreaState,
) -> Option<DockAreaState> {
    let state = match api::parse_layout(&record.state) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("[layout] 布局 `{}` 的存档不用：{error}", record.layout_id);
            return None;
        }
    };
    if layout_frame(&state) != layout_frame(registered) {
        eprintln!(
            "[layout] 布局 `{}` 摆的视图或摞变了，存档不用",
            record.layout_id
        );
        return None;
    }
    Some(state)
}

/// 库里这份布局的存档和它的原文。读不出来、用不了都当没存过。`registered` 是这份布局登记的
/// 样子。
fn saved_state(id: &str, registered: &DockAreaState) -> Option<(DockAreaState, String)> {
    let record = match smelt_client_store::client_store()
        .and_then(|store| store.workspace_layout_state(id))
    {
        Ok(record) => record?,
        Err(error) => {
            eprintln!("[layout] 读布局 `{id}` 的存档失败：{error}");
            return None;
        }
    };
    let state = usable_saved_state(&record, registered)?;
    Some((state, record.state))
}

impl Workspace {
    /// 外观里选的布局变了就重挑。外观的别的项（拖滑块）也会走到这里，比一下就回去。
    pub(crate) fn watch_layout_choice(window: &mut Window, cx: &mut Context<Self>) -> Subscription {
        cx.observe_global_in::<Appearance>(window, |this, window, cx| this.sync_pages(window, cx))
    }

    /// 开着的那页的布局说哪几块地方垫什么材质。没有可用页时整页实色。只读内存，render 里调。
    pub(crate) fn open_page_materials(&self, cx: &App) -> api::LayoutMaterials {
        let Some(page) = self.router.page() else {
            return api::LayoutMaterials::default();
        };
        let Some(ix) = self.page_ix(page) else {
            return api::LayoutMaterials::default();
        };
        let id = self.pages[ix].layout_id.as_str();
        api::layouts(cx)
            .into_iter()
            .find(|info| info.id.as_ref() == id)
            .map(|info| info.materials)
            .unwrap_or_default()
    }

    fn page_ix(&self, page: &str) -> Option<usize> {
        self.pages.iter().position(|dock| dock.page == page)
    }

    /// 没有可用首页布局时的空态/排错页：直接说明原因、展示出错插件，并给出打开「面板插件」的入口。
    /// 纯读内存缓存，不做 IO。
    pub(crate) fn render_no_layout_state(&self, cx: &mut Context<Self>) -> Div {
        let failed: Vec<(String, String)> = self
            .nsh_shell
            .read(cx)
            .installed_plugins()
            .into_iter()
            .filter_map(|p| p.error.map(|err| (p.id, err)))
            .take(4)
            .collect();

        let error_box = (!failed.is_empty()).then(|| {
            let mut box_div = div()
                .w_full()
                .p_3()
                .rounded(px(RADIUS.md))
                .bg(rgb(crate::ui_theme::surface()))
                .border_1()
                .border_color(crate::ui_theme::hair())
                .flex()
                .flex_col()
                .gap_1p5();
            for (id, err) in failed {
                box_div = box_div.child(
                    div()
                        .type_style(TYPE.text_xsmall)
                        .text_color(rgb(crate::ui_theme::bad()))
                        .child(SharedString::from(format!("{id}: {err}"))),
                );
            }
            box_div
        });

        div()
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(34.0))
                    .w_full()
                    .flex_shrink_0()
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .px_6()
                    .pb(px(34.0))
                    .child(
                        div()
                            .w_full()
                            .max_w(px(440.0))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_3()
                            .child(
                                svg()
                                    .path(IconName::AppWindow.path())
                                    .size(px(28.0))
                                    .text_color(rgb(crate::ui_theme::muted())),
                            )
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(TYPE.ui_medium.weight)
                                    .text_color(rgb(crate::ui_theme::text()))
                                    .text_center()
                                    .child(t!("layout.empty_title")),
                            )
                            .child(
                                div()
                                    .type_style(TYPE.text_small)
                                    .text_color(rgb(crate::ui_theme::muted()))
                                    .text_center()
                                    .child(t!("layout.empty_hint")),
                            )
                            .children(error_box)
                            .child(
                                div().pt_1().child(
                                    div()
                                        .id("open-panel-plugins")
                                        .px_3()
                                        .h(px(28.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(RADIUS.md))
                                        .bg(crate::ui_theme::palette().button_primary[0])
                                        .hover(|s| {
                                            s.bg(crate::ui_theme::palette().button_primary[1])
                                        })
                                        .text_color(rgb(
                                            crate::ui_theme::palette().button_primary_foreground
                                        ))
                                        .type_style(TYPE.ui_medium)
                                        .cursor_pointer()
                                        .child(t!("layout.open_plugins"))
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.open_settings_page(
                                                crate::settings::PANEL_PLUGINS_VIEW.into(),
                                                window,
                                                cx,
                                            );
                                        })),
                                ),
                            ),
                    ),
            )
    }

    fn restore_startup_outlet(&mut self, cx: &App) {
        if let Some(landing) = landing_page(cx)
            && let Some(view) = self.startup_outlet.take()
            && outlet_can_hold(&view, cx)
        {
            self.router.set_outlet(&landing, Some(view));
        }
    }

    /// 工作台建好后进首页，首页出口放上次放着的东西。首批 JS 插件还没落定时先等
    /// `MainShellEvent::InitialReady`，避免启动瞬间闪一帧空态或提前扔掉 `startup_outlet`。
    pub(crate) fn open_home_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.home_opened {
            return;
        }
        if !self.nsh_shell.read(cx).initial_plugins_settled() {
            return;
        }
        self.home_opened = true;
        sync_laid_out_views(cx);
        self.restore_startup_outlet(cx);
        self.land(window, cx);
    }

    /// 进一页，记下来路（见 [`crate::router::Router::navigate`]）。进不去（页没登记、没有
    /// 给它的布局）返回 `false`，还停在原来那页。
    pub(crate) fn navigate(
        &mut self,
        page: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.router.page() == Some(page.as_str()) {
            return true;
        }
        if !self.prepare_page(&page, cx) {
            return false;
        }
        self.router.navigate(page);
        self.page_shown(window, cx);
        true
    }

    /// 回到来的那页（页头的关闭）；来路上进不去的跳过，都没了回首页。
    pub(crate) fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(back) = self.router.pop_back() {
            if page_usable(&back, cx) && self.prepare_page(&back, cx) {
                self.router.arrive(back);
                self.page_shown(window, cx);
                return;
            }
        }
        self.land(window, cx);
    }

    /// 落到首页（没有可用首页布局时落在空态），来路清空。
    fn land(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let page = landing_page(cx).filter(|page| self.prepare_page(page, cx));
        self.router.land(page);
        self.page_shown(window, cx);
    }

    /// 备好这一页的 Dock：开过的直接用，没开过的装上布局并登记条目。装不上返回 `false`。
    /// 开过的页进不进得去由调用方先看（[`page_usable`]）。
    fn prepare_page(&mut self, page: &str, cx: &mut Context<Self>) -> bool {
        if self.page_ix(page).is_some() {
            return true;
        }
        let Some(dock) = self.load_page_dock(page, cx) else {
            return false;
        };
        self.pages.push(dock);
        true
    }

    /// 路由换到了一页（或落入空态）：焦点先收回工作台——可能还在换下去的那页里，
    /// 出口里的视图建好、露着，再把焦点交给它（它接管焦点时）。
    fn page_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        self.sync_outlet(window, cx);
        self.focus_outlet(window, cx);
        cx.notify();
    }

    /// 把键盘焦点交给开着的这页出口里显示着的视图，见 [`api::PluginView::focus_target`]。
    /// 出口没露着、视图不接管焦点就什么都不做。
    fn focus_outlet(&self, window: &mut Window, cx: &mut App) {
        let Some(id) = self.shell_view.outlet.as_deref() else {
            return;
        };
        if !self.view_shown(id) {
            return;
        }
        if self.nsh_shell.read(cx).is_js_view(id) {
            window.focus(&self.nsh_shell.focus_handle(cx), cx);
            return;
        }
        if let Some(instance) = self.outlet_instance() {
            instance.focus(window, cx);
        } else if let Some((view, _)) = api::ensure_view(id, window, cx)
            && let Some(handle) = view.focus_target(cx)
        {
            window.focus(&handle, cx);
        }
    }

    /// 开着的这页的出口放这个视图；`None`、这页的默认内容都是放回默认内容。换内容不记来路。
    fn set_outlet(&mut self, view: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self.router.page().map(str::to_owned) else {
            return;
        };
        let view = view.filter(|view| page_index(&page, cx).as_ref() != Some(view));
        if self.router.set_outlet(&page, view) {
            self.sync_outlet(window, cx);
            self.save_state(cx);
            cx.notify();
        }
    }

    /// 出口对上注册表：撤下的视图不再放，出口里该放的视图建好（新建的打开一次，让它读
    /// 数据），再重算看得见什么。按实例建的那份，出口换了内容、登记撤下了就放掉。换页、
    /// 出口换内容、视图登记变了时调。
    pub(crate) fn sync_outlet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let router = &self.router;
        self.outlet_instances.retain(|page, instance| {
            router.outlet(page) == Some(&**instance.id())
                && api::view(instance.id(), cx).is_some_and(|view| view.instances)
        });
        let instances = &self.outlet_instances;
        let dropped = self.router.retain_views(|page, id| {
            outlet_can_hold(id, cx)
                || instances.contains_key(page)
                || self
                    .nsh_outlet_args
                    .get(page)
                    .is_some_and(|(target, _, _)| target == id && api::is_registered(id, cx))
        });
        self.nsh_outlet_args
            .retain(|page, (target, _, _)| self.router.outlet(page) == Some(target.as_str()));
        self.sync_shell_view(cx);
        if let Some(id) = self.shell_view.outlet.clone()
            && !self.nsh_shell.read(cx).is_js_view(&id)
            && let Some((view, true)) = api::ensure_view(&id, window, cx)
        {
            self.publish_safe_areas(vec![id], cx);
            view.open(&serde_json::Value::Null, window, cx);
            cx.notify();
        }
        if dropped {
            self.save_state(cx);
        }
    }

    /// 首页出口里放着的视图，随界面状态存；按实例建的那份参数不存，也不放回去。
    pub(crate) fn persisted_outlet(&self, cx: &App) -> Option<String> {
        let home = home_page(cx)?;
        self.router
            .outlet(&home)
            .filter(|view| outlet_can_hold(view, cx))
            .map(str::to_string)
    }

    /// 开着的这页出口里放着的、按实例建的那份。出口画它（见 `host_views`）。只读内存。
    pub(crate) fn outlet_instance(&self) -> Option<&api::ViewInstance> {
        let page = self.router.page()?;
        self.outlet_instances.get(page)
    }

    /// 按登记状态或可用存档为 `page` 装载布局到 `nsh_shell`，返回新的 `PageDock`。
    fn load_page_dock(&self, page: &str, cx: &mut Context<Self>) -> Option<PageDock> {
        let id = effective_layout(page, cx)?;
        let registered = api::layout(&id, cx).expect("effective_layout 只给登记着的布局");
        let frame = layout_frame(&registered);
        let (state, written) = match saved_state(&id, &registered) {
            Some((state, text)) => (state, Some(text)),
            None => (registered, None),
        };
        if let Err(error) = self
            .nsh_shell
            .update(cx, |shell, cx| shell.load_page(page.to_owned(), &state, cx))
        {
            crate::status_item::notify_error(&t!("layout.load_failed", error = error));
        }
        Some(PageDock {
            page: page.to_owned(),
            layout_id: id,
            frame,
            written,
        })
    }

    /// 给第 `ix` 页重装眼下该装的布局：有能用的存档装存档，否则装布局本来的样子。
    fn install_layout(&mut self, ix: usize, cx: &mut Context<Self>) -> bool {
        let page = self.pages[ix].page.clone();
        let Some(dock) = self.load_page_dock(&page, cx) else {
            return false;
        };
        self.pages[ix] = dock;
        true
    }

    /// 页、布局登记变了，外观里换了布局：进不去的页放掉、从来路上拿掉，开着的那页
    /// 进不去了就回去；原本停在空态而现在有了可用首页布局则自动进首页；各页该装的
    /// 和装着的不同（换了一份，或插件替换了这份、摆的视图变了）就换。外观里拖滑块也
    /// 走到这里，比一下就回去。
    pub(crate) fn sync_pages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        sync_laid_out_views(cx);
        if !self.home_opened {
            // 还没进首页：进的时候自然按最新的登记挑。
            return;
        }
        let current = self.router.page().map(str::to_owned);
        let mut dropped = Vec::new();
        self.pages.retain(|dock| {
            let keep =
                current.as_deref() == Some(dock.page.as_str()) || page_usable(&dock.page, cx);
            if !keep {
                dropped.push(dock.page.clone());
            }
            keep
        });
        self.router.retain_pages(|page| page_usable(page, cx));
        match current {
            Some(ref cur) if !page_usable(cur, cx) => {
                self.go_back(window, cx);
                self.pages.retain(|dock| {
                    let keep = dock.page != *cur;
                    if !keep {
                        dropped.push(dock.page.clone());
                    }
                    keep
                });
            }
            None if landing_page(cx).is_some() => {
                self.restore_startup_outlet(cx);
                self.land(window, cx);
            }
            _ => {}
        }
        if !dropped.is_empty() {
            self.nsh_shell.update(cx, |shell, cx| {
                for key in dropped {
                    shell.remove_page(&key, cx);
                }
            });
        }
        for ix in 0..self.pages.len() {
            let dock = &self.pages[ix];
            let stale = effective_layout(&dock.page, cx).as_deref()
                != Some(dock.layout_id.as_str())
                || api::layout(&dock.layout_id, cx)
                    .is_some_and(|state| layout_frame(&state) != dock.frame);
            if stale {
                self.install_layout(ix, cx);
            }
        }
        // 出口的默认内容跟着页的登记变。
        self.sync_outlet(window, cx);
    }

    /// 扔掉首页装着的这份布局的存档，回到它本来的样子。首页还没开过就只扔存档。
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
            crate::status_item::notify_error(&t!("layout.reset_failed", error = error));
            return;
        }
        let handle = self.window_handle;
        let workspace = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = cx.update_window(handle, |_, _window, cx| {
                let _ = workspace.update(cx, |this, cx| {
                    // 等的这一会儿换了布局，就不是要恢复的那份了。
                    if let Some(ix) = this.pages.iter().position(|dock| dock.layout_id == id) {
                        this.install_layout(ix, cx);
                    }
                });
            });
        });
    }

    /// 视图登记变了（插件加载、卸载、替换）：各页按现在的样子重装一遍，新登记的
    /// 视图补上，撤下的变占位。
    pub(crate) fn reload_layout(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        for ix in 0..self.pages.len() {
            let key = self.pages[ix].page.clone();
            if let Some(state) = self.nsh_shell.read(cx).dump_page(&key)
                && let Err(error) = self
                    .nsh_shell
                    .update(cx, |shell, cx| shell.load_page(key, &state, cx))
            {
                crate::status_item::notify_error(&t!("layout.load_failed", error = error));
            }
        }
    }

    /// 一页的 Dock 树改了：存档；是开着的那页，出口里放着的视图被摆进布局了就不再放出口，
    /// 露出布局里的它。
    pub(crate) fn page_layout_changed(
        &mut self,
        page: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.page_ix(page) else {
            return;
        };
        self.save_layout_state(ix, cx);
        if self.router.page() != Some(page) {
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
        let Some(dumped) = self.nsh_shell.read(cx).dump_page(&dock.page) else {
            return;
        };
        let state = to_text(&dumped);
        if dock.written.as_deref() == Some(state.as_str()) {
            return;
        }
        let record = WorkspaceLayoutStateRecord {
            layout_id: dock.layout_id.clone(),
            state: state.clone(),
        };
        match smelt_client_store::client_store()
            .and_then(|store| store.put_workspace_layout_state(&record))
        {
            Ok(()) => self.pages[ix].written = Some(state),
            Err(error) => eprintln!("[layout] 存布局状态失败：{error}"),
        }
    }

    /// 重算开着的那页摆着什么、出口里放着什么、看得见什么。变了才重画。出口里的视图建没建
    /// 由 [`Self::sync_outlet`] 管。
    pub(crate) fn sync_shell_view(&mut self, cx: &mut Context<Self>) {
        let page = self.router.page().map(str::to_owned);
        let key = page.clone().unwrap_or_default();
        let laid_out = if key.is_empty() {
            HashSet::new()
        } else {
            self.nsh_shell.read(cx).page_laid_out(&key)
        };
        let outlet = page
            .as_deref()
            .and_then(|page| self.outlet_content(page, &laid_out, cx));
        let target = page.as_deref().zip(outlet.as_ref()).map(|(page, id)| {
            let managed_rust_instance = self
                .outlet_instances
                .get(page)
                .is_some_and(|instance| instance.id().as_ref() == id.as_str());
            let (args, seq) = self
                .nsh_outlet_args
                .get(page)
                .filter(|(target_id, _, _)| target_id == id)
                .map(|(_, args, seq)| (args.clone(), *seq))
                .unwrap_or((serde_json::Value::Null, 0));
            smelt_nsh::OutletTarget {
                id: id.clone(),
                args,
                seq,
                managed_rust_instance,
            }
        });
        self.nsh_shell
            .update(cx, |shell, cx| shell.set_active_page(key, target, cx));
        self.sync_backdrop_placements(cx);
        let next = ShellView::from_nsh(self.nsh_shell.read(cx).shell_view());
        let before = std::mem::replace(&mut self.shell_view, next);
        let ids: Vec<String> = [&before, &self.shell_view]
            .into_iter()
            .flat_map(|view| view.laid_out.iter().chain(&view.outlet))
            .cloned()
            .collect();
        self.publish_safe_areas(ids, cx);
        self.sync_host_settings(cx);
        if self.shell_view == before {
            return;
        }
        plugin_host::set_window_shown(self.shell_view.shown.clone(), cx);
        cx.notify();
    }

    /// 把这几个视图此刻的 safe area（[`ShellView::safe_area_of`]）设给它们。不在这页了的
    /// 设回全零。还没建的跳过，建好后再来一遍（见 [`Self::sync_outlet`]）。
    fn publish_safe_areas(&self, ids: Vec<String>, cx: &mut App) {
        for id in ids {
            let view = match self.outlet_instance() {
                Some(instance) if **instance.id() == *id => Some(instance.view()),
                _ => api::built_view(&id, cx).map(|view| view.view()),
            };
            if let Some(view) = view {
                api::set_safe_area(&view, self.shell_view.safe_area_of(&id), cx);
            }
        }
    }

    /// 开着的这页出口里显示的视图：路由给这页放的，没放就是这页的默认内容。这页没摆出口、
    /// 视图没登记、它已经摆在这页的布局里（一个视图只画在一处）时是 `None`。只读内存。
    fn outlet_content(&self, page: &str, laid_out: &HashSet<String>, cx: &App) -> Option<String> {
        if !laid_out.contains(api::OUTLET_VIEW) {
            return None;
        }
        self.router
            .outlet(page)
            .map(str::to_string)
            .or_else(|| page_index(page, cx))
            .filter(|id| !laid_out.contains(id) && api::is_registered(id, cx))
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
            self.open_settings_page(info.id, window, cx);
        } else if !self.reveal_view(id, window, cx) {
            return false;
        }
        if self.nsh_shell.read(cx).is_js_view(id)
            && self.shell_view.outlet.as_deref() == Some(id)
            && let Some(page) = self.router.page().map(str::to_owned)
        {
            self.nsh_outlet_seq += 1;
            self.nsh_outlet_args
                .insert(page, (id.to_owned(), args.clone(), self.nsh_outlet_seq));
            self.sync_shell_view(cx);
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
        let current = self.router.page().map(str::to_owned);
        let pages = page_order(cx);
        if let Some(page) = pages.iter().find(|page| {
            current.as_deref() != Some(page.as_str())
                && page_usable(page, cx)
                && self.page_lays_out(page, id, cx)
        }) {
            return self.navigate(page.clone(), window, cx) && self.show_view(id, window, cx);
        }
        if !outlet_can_hold(id, cx) || !self.go_to_outlet(window, cx) {
            return false;
        }
        self.set_outlet(Some(id.to_string()), window, cx);
        self.show_view(id, window, cx)
    }

    /// 打开按实例建的视图（`OPEN_VIEW` 给的是这种时）：按 `args` 新建一份放进出口（找出口
    /// 同 [`Self::reveal_view`]），出口原来放着的放掉，焦点交给它。没登记、建不出来、没有
    /// 能放的出口是 `Err`（给人看的原因）。
    pub(crate) fn open_instance(
        &mut self,
        id: &str,
        args: &serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.nsh_shell.read(cx).is_js_view(id) {
            if !api::view(id, cx).is_some_and(|v| v.instances) {
                return Err(t!("layout.no_outlet", id = id).into_owned());
            }
            if !self.go_to_outlet(window, cx) {
                return Err(t!("layout.no_outlet", id = id).into_owned());
            }
            let Some(page) = self.router.page().map(str::to_owned) else {
                return Err(t!("layout.no_outlet", id = id).into_owned());
            };
            self.outlet_instances.remove(&page);
            self.nsh_outlet_seq += 1;
            self.nsh_outlet_args.insert(
                page.clone(),
                (id.to_owned(), args.clone(), self.nsh_outlet_seq),
            );
            self.router.set_outlet(&page, Some(id.to_owned()));
            self.sync_outlet(window, cx);
            self.save_state(cx);
            cx.notify();
            self.show_view(id, window, cx);
            window.focus(&self.nsh_shell.focus_handle(cx), cx);
            return Ok(());
        }
        let instance = api::create_view(id, args, window, cx)?;
        if !self.go_to_outlet(window, cx) {
            return Err(t!("layout.no_outlet", id = id).into_owned());
        }
        let Some(page) = self.router.page().map(str::to_owned) else {
            return Err(t!("layout.no_outlet", id = id).into_owned());
        };
        let id = instance.id().to_string();
        self.outlet_instances.insert(page.clone(), instance);
        self.nsh_outlet_seq += 1;
        self.nsh_outlet_args.insert(
            page.clone(),
            (id.clone(), serde_json::Value::Null, self.nsh_outlet_seq),
        );
        // 同一个视图换一份时路由不变，照样要重建出口、重画。
        self.router.set_outlet(&page, Some(id.clone()));
        self.sync_outlet(window, cx);
        self.save_state(cx);
        cx.notify();
        self.show_view(&id, window, cx);
        if let Some(instance) = self.outlet_instances.get(&page) {
            instance.focus(window, cx);
        }
        Ok(())
    }

    /// 去有出口的页：开着的这页有出口就留在这页，没有就去有出口的那页（按 [`page_order`]）。
    /// 哪页都没有、进不去返回 `false`。
    fn go_to_outlet(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let current = self.router.page().map(str::to_owned);
        if let Some(ref cur) = current
            && self.page_lays_out(cur, api::OUTLET_VIEW, cx)
        {
            return true;
        }
        let Some(page) = page_order(cx).into_iter().find(|page| {
            current.as_deref() != Some(page.as_str())
                && page_usable(page, cx)
                && self.page_lays_out(page, api::OUTLET_VIEW, cx)
        }) else {
            return false;
        };
        self.navigate(page, window, cx)
    }

    /// 这一页的布局里摆着这个视图。开过的看它的 Dock，没开过的看它会装的那份登记的样子
    /// （存档只改摆法、不增减视图）。只读内存。
    fn page_lays_out(&self, page: &str, id: &str, cx: &App) -> bool {
        if self.page_ix(page).is_some() {
            return self.nsh_shell.read(cx).has_view(page, id);
        }
        effective_layout(page, cx)
            .and_then(|layout| api::layout(&layout, cx))
            .is_some_and(|state| layout_views(&state).contains(id))
    }

    /// 让开着的这页里摆着的一个视图露出来：选中它所在组里的它（出口里显示着的就是出口），
    /// 它在收起的停靠区里就展开。这页没摆、出口里也不是它返回 `false`。
    pub(crate) fn show_view(
        &mut self,
        id: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(key) = self.router.page().map(str::to_owned) else {
            return false;
        };
        let shown = self
            .nsh_shell
            .update(cx, |shell, cx| shell.show_view(&key, id, cx));
        if shown {
            self.sync_shell_view(cx);
        }
        shown
    }

    /// 视图此刻看得见：所在组显示着它，或者它在显示着的出口里。
    pub(crate) fn view_shown(&self, id: &str) -> bool {
        self.shell_view.shown.contains(id)
    }

    /// Esc：还原开着的这页放大的组。没放大着返回 `false`。
    pub(crate) fn unzoom_layout(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        let unzoomed = self
            .nsh_shell
            .update(cx, |shell, cx| shell.unzoom_active(cx));
        if unzoomed {
            self.sync_shell_view(cx);
        }
        unzoomed
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{layout_views, resolve, to_text, usable_saved_state};
    use smelt_client_store::WorkspaceLayoutStateRecord;
    use smelt_plugin_api as api;
    use smelt_plugin_api::DockAreaState;

    fn info(id: &str) -> api::LayoutInfo {
        api::LayoutInfo {
            id: id.to_string().into(),
            page: "example.layouts/workbench".into(),
            title: id.to_string().into(),
            materials: api::LayoutMaterials::default(),
        }
    }

    fn center_tabs_layout(views: &[&str]) -> DockAreaState {
        let children = views
            .iter()
            .map(|id| serde_json::json!({ "panel_name": id, "children": [], "info": { "panel": null } }))
            .collect::<Vec<_>>();
        serde_json::from_value(serde_json::json!({
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
        }))
        .unwrap()
    }

    /// 选的登记着就用它；没登记（插件还没加载）用第一份；一份都没有是 `None`。
    #[test]
    fn the_chosen_layout_is_used_only_while_registered() {
        let registered = [info("example.layouts/standard"), info("trader/two")];
        assert_eq!(
            resolve(Some("trader/two"), &registered).as_deref(),
            Some("trader/two")
        );
        assert_eq!(
            resolve(Some("gone/x"), &registered).as_deref(),
            Some("example.layouts/standard")
        );
        assert_eq!(
            resolve(None, &registered).as_deref(),
            Some("example.layouts/standard")
        );
        assert_eq!(resolve(Some("trader/two"), &[]), None);
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
                  "children": [{ "panel_name": "example.a/main", "children": [], "info": { "panel": null } }] }] },
              "left_dock": { "placement": "left", "size": 260.0, "open": false,
                "panel": { "panel_name": "TabPanel", "info": { "tabs": { "active_index": 0 } },
                  "children": [{ "panel_name": "example.b/list", "children": [], "info": { "panel": null } }] } }
            }"#,
        )
        .unwrap();
        assert_eq!(
            layout_views(&state),
            HashSet::from(["example.a/main".to_string(), "example.b/list".to_string()])
        );
    }

    /// 存档是当前版本、摆的视图和登记的一样就用，摆法（组、顺序、大小）随它；摆的视图变了
    /// （布局升级加了一块）、版本不对、解析不了不用。
    #[test]
    fn a_saved_state_is_used_while_it_lays_out_the_same_views() {
        let record = |state: String| WorkspaceLayoutStateRecord {
            layout_id: "example.layouts/standard".into(),
            state,
        };
        let registered = center_tabs_layout(&["a/main", "b/main"]);
        // 换了顺序：只是摆法，照用。
        let moved = center_tabs_layout(&["b/main", "a/main"]);
        assert_eq!(
            usable_saved_state(&record(to_text(&moved)), &registered),
            Some(moved.clone())
        );
        // 布局后来多摆了一个视图，旧存档里没有：过时了。
        let older = center_tabs_layout(&["a/main"]);
        assert_eq!(
            usable_saved_state(&record(to_text(&older)), &registered),
            None
        );
        let mut newer = moved;
        newer.version = Some(api::LAYOUT_VERSION + 1);
        assert_eq!(
            usable_saved_state(&record(to_text(&newer)), &registered),
            None
        );
        assert_eq!(usable_saved_state(&record("{}".into()), &registered), None);
    }

    /// 侧栏原来是三格可拖的组（入口、会话列表、设置和额度），现在并成一摞：视图一样，旧存档
    /// 也不能用，不然存过档的人永远停在旧侧栏。摞里的视图算摆着的；摞原样存回去的存档照用。
    #[test]
    fn a_saved_state_without_the_new_stack_is_stale() {
        let leaf = |name: &str| {
            format!(r#"{{"panel_name":"{name}","children":[],"info":{{"panel":null}}}}"#)
        };
        let tabs = |children: &[String]| {
            format!(
                r#"{{"panel_name":"TabPanel","info":{{"tabs":{{"active_index":0}}}},"children":[{}]}}"#,
                children.join(",")
            )
        };
        let split = |axis: u8, children: &[String]| {
            format!(
                r#"{{"panel_name":"StackPanel","info":{{"stack":{{"sizes":[],"axis":{axis}}}}},"children":[{}]}}"#,
                children.join(",")
            )
        };
        let layout = |left: String| {
            api::parse_layout(&format!(
                r#"{{"version":1,"center":{},"left_dock":{{"placement":"left","size":260.0,"open":true,"panel":{left}}}}}"#,
                tabs(&[leaf("smelt/outlet")])
            ))
            .unwrap()
        };
        let old = layout(split(
            1,
            &[
                tabs(&[leaf("a/nav")]),
                tabs(&[leaf("b/list")]),
                split(0, &[tabs(&[leaf("a/settings")]), tabs(&[leaf("c/quota")])]),
            ],
        ));
        let stack = r#"{"panel_name":"smelt/stack","children":[],"info":{"panel":{"axis":"vertical","items":[
            {"view":"a/nav","fit":true},{"view":"b/list"},
            {"stack":{"axis":"horizontal","items":[{"view":"a/settings"},{"view":"c/quota","fit":true}]},"fit":true}
        ]}}}"#;
        let registered = layout(tabs(&[stack.to_string()]));
        assert_eq!(layout_views(&old), layout_views(&registered));
        assert!(layout_views(&registered).contains("c/quota"));
        assert!(!layout_views(&registered).contains(api::STACK_PANEL));
        let record = |state: &api::DockAreaState| WorkspaceLayoutStateRecord {
            layout_id: "example.layouts/standard".into(),
            state: to_text(state),
        };
        assert_eq!(usable_saved_state(&record(&old), &registered), None);
        assert_eq!(
            usable_saved_state(&record(&registered), &registered),
            Some(registered.clone())
        );
    }
}
