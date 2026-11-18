//! 页的外壳：主窗口的每一页是一块 `DockArea`（见 `workspace_layout`），这里是 smelt 给它的
//! 皮肤和面板。
//!
//! 摆什么、怎么切、选中哪个、放大都在 Dock 上（布局和存档就是它的 `DockAreaState`，见
//! `workspace_layout`）。这里只管：
//! - 面板（[`ViewPanel`]）：视图注册表里的一个视图（`smelt_plugin_api::view`），面板名就是
//!   视图 id。外壳不认识视图是什么，宿主自己登记的出口、设置也一样（见 `host_views`）。
//! - 占位（[`PlaceholderPanel`]）：没登记的视图、同一个视图摆了第二份。不显示，存档时原样
//!   写回：插件装回来就又出现了。
//! - 皮肤（[`SmeltDockRenderer`] / [`SmeltTabGroup`]）：分隔条、停靠区边、落点这些和会话分屏
//!   共用的取 `smelt_ui::dock`；标签条是这里的——角标、拖窗口的空白、给角上的控件让位、放大
//!   按钮、拖动重排。
//! - 读 Dock：[`ShellView`] 是摆着什么、看得见什么的快照，工作台缓存一份给 render 读。

use rust_i18n::t;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Axis, Context, Div, Edges, Entity, EventEmitter, FocusHandle,
    Focusable, Pixels, SharedString, Subscription, Window, div, prelude::*, px, rgb, size,
};
use gpui_base::dock::{
    DockArea, DockAreaRenderer, DockAreaState, DockContext, DockPlacement, DragPanel,
    DropIndicator, NodeId, PaneNode, PaneRef, Panel, PanelEvent, PanelId, PanelInfo, PanelState,
    PanelView, TabGroupContext, TabGroupRenderer,
};
use gpui_base::{ElementExt as _, ResizeHandleContext};
use serde_json::Value;
use smelt_plugin_api::{self as api, ViewHandle};
pub(crate) use smelt_ui::Button;
use smelt_ui::dock::{DockEdges, TabLabel, tab_strip};
use smelt_ui::window_bar::{
    BAR_MARGIN, SafeArea, TOP_BAR_HEIGHT, WINDOW_EDGE_MARGIN, with_window_drag,
};
use smelt_ui::{Badge, IconAction as _, IconName, ui_theme};

use crate::Workspace;
use crate::window_backdrop::SharedRegions;
use crate::workspace_frame::WindowCorners;
use smelt_ui::kit::{TYPE, TypeStyled as _};

/// Dock 的四个区域，按画的先后。
const PLACEMENTS: [DockPlacement; 4] = [
    DockPlacement::Center,
    DockPlacement::Left,
    DockPlacement::Right,
    DockPlacement::Bottom,
];

/// 左右停靠区开合按钮的宽度：小号图标按钮的边长。离窗口边、离内容的距离另算（见
/// [`ShellView::leading_inset`]）。
const DOCK_TOGGLE_WIDTH: f32 = 20.;

// ---- 面板 ----

thread_local! {
    /// 面板名要 `&'static str`，视图 id 是运行时登记的：每个 id 第一次见到时留一份。
    /// 只增不删，数量就是见过的视图 id 个数。
    static PANEL_NAMES: RefCell<HashSet<&'static str>> = RefCell::new(HashSet::new());
}

fn panel_name(id: &str) -> &'static str {
    PANEL_NAMES.with(|names| {
        let mut names = names.borrow_mut();
        if let Some(name) = names.get(id) {
            return *name;
        }
        let name: &'static str = Box::leak(id.to_string().into_boxed_str());
        names.insert(name);
        name
    })
}

/// 视图注册表里的一个视图摆进 Dock 的壳。视图本身归注册表，重新装布局时壳换新的，视图
/// 还是那一份，不丢状态。
pub(crate) struct ViewPanel {
    id: &'static str,
    title: SharedString,
    view: Rc<dyn ViewHandle>,
    /// 布局里它的 `info.panel`，原样写回存档。
    info: Value,
    /// 登记时说的 [`smelt_plugin_api::ViewSpec::draws_top_bar`]。
    owns_top_bar: bool,
    /// 出口里显示着的视图（只有出口有，见 [`show_in_outlet`]）。出口只是个位置：标题、角标、
    /// 顶上那条归谁画都跟着里面的视图，不然插件页放进出口就没人画标题栏。
    content: Option<OutletContent>,
    focus_handle: FocusHandle,
    /// 角标变了重画标签。
    _badges: Subscription,
}

/// 出口里显示着的视图登记时说的那几项。
#[derive(Clone, Debug, PartialEq)]
struct OutletContent {
    id: SharedString,
    title: SharedString,
    draws_top_bar: bool,
}

impl ViewPanel {
    fn shown_title(&self) -> SharedString {
        match &self.content {
            Some(content) => content.title.clone(),
            None => self.title.clone(),
        }
    }

    fn draws_top_bar(&self) -> bool {
        match &self.content {
            Some(content) => content.draws_top_bar,
            None => self.owns_top_bar,
        }
    }

    /// 角标按哪个视图 id 取。
    fn badge_view(&self) -> &str {
        match &self.content {
            Some(content) => &content.id,
            None => self.id,
        }
    }

    /// 标签条上的样子。角标由插件推（`smelt_plugin_api::set_view_badge`）。
    fn tab_label(&self, cx: &App) -> TabLabel {
        let title = self.shown_title();
        TabLabel {
            text: title.clone(),
            label: title.into_any_element(),
            badge: smelt_plugin_api::view_badge(self.badge_view(), cx)
                .map(|badge| Badge::new().accent().child(badge).into_any_element()),
        }
    }
}

/// 出口里换了视图（`content`，空着是 `None`）：出口的标题、角标、顶上那条画不画换成那个
/// 视图登记的。返回顶上那条的归属变没变：变了，哪些视图贴着窗口顶要重算。布局里没摆出口
/// 时什么都不做。
pub(crate) fn show_in_outlet(area: &Entity<DockArea>, content: Option<&str>, cx: &mut App) -> bool {
    let dock = area.read(cx);
    let Some(panel) = find_view(dock, api::OUTLET_VIEW, cx)
        .and_then(|id| dock.panel(id))
        .and_then(|panel| panel.view().downcast::<ViewPanel>().ok())
    else {
        return false;
    };
    let content = content
        .and_then(|id| api::view(id, cx))
        .map(|info| OutletContent {
            id: info.id,
            title: info.title,
            draws_top_bar: info.draws_top_bar,
        });
    panel.update(cx, |panel, cx| {
        let before = panel.draws_top_bar();
        if panel.content != content {
            panel.content = content;
            cx.notify();
        }
        panel.draws_top_bar() != before
    })
}

impl EventEmitter<PanelEvent> for ViewPanel {}

impl Focusable for ViewPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for ViewPanel {
    fn panel_name(&self) -> &'static str {
        self.id
    }

    /// 不让关：还没有把视图加回布局的入口。
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        !self.draws_top_bar()
    }

    fn dump(&self, _: &App) -> PanelState {
        PanelState {
            panel_name: self.id.to_string(),
            children: Vec::new(),
            info: PanelInfo::panel(self.info.clone()),
        }
    }
}

impl Render for ViewPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.view.view())
    }
}

/// 建不出来的面板：视图没登记（插件没装、删了），或者同一个视图摆了第二份（一个视图只能
/// 画在一处）。不显示，存档时把原来的样子原样写回。
pub(crate) struct PlaceholderPanel {
    state: PanelState,
    focus_handle: FocusHandle,
}

impl EventEmitter<PanelEvent> for PlaceholderPanel {}

impl Focusable for PlaceholderPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for PlaceholderPanel {
    fn panel_name(&self) -> &'static str {
        "smelt.placeholder"
    }

    fn visible(&self, _: &App) -> bool {
        false
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }

    fn dump(&self, _: &App) -> PanelState {
        self.state.clone()
    }
}

impl Render for PlaceholderPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
    }
}

/// 一摞（[`api::STACK_PANEL`]）：几个视图按顺序排在一格里，没有分隔条，`fit` 的按内容定
/// 大小，其余平分。建不出来的项（没登记、摆了第二份）不画，存档时原文照写。
pub(crate) struct StackPanel {
    /// 布局里它的 `info.panel`，原样写回存档。
    info: Value,
    /// 建出来的那些项；一项都没建出来是 `None`，整摞不显示。
    root: Option<StackNode>,
    focus_handle: FocusHandle,
}

enum StackNode {
    View {
        id: String,
        title: SharedString,
        view: Rc<dyn ViewHandle>,
    },
    Stack {
        axis: api::StackAxis,
        /// 每项和它的 `fit`。
        items: Vec<(StackNode, bool)>,
    },
}

impl StackNode {
    /// 按布局里写的建，建不出来的项去掉，去空了的一摞也去掉。
    fn build(stack: &api::Stack, window: &mut Window, cx: &mut App) -> Option<Self> {
        let items: Vec<(StackNode, bool)> = stack
            .items
            .iter()
            .filter_map(|item| {
                let node = match &item.content {
                    api::StackContent::View(id) => {
                        let (spec, view) = place_view(id, window, cx)?;
                        StackNode::View {
                            id: id.clone(),
                            title: spec.title,
                            view,
                        }
                    }
                    api::StackContent::Stack(stack) => StackNode::build(stack, window, cx)?,
                };
                Some((node, item.fit))
            })
            .collect();
        (!items.is_empty()).then_some(StackNode::Stack {
            axis: stack.axis,
            items,
        })
    }

    fn views(&self, out: &mut Vec<String>) {
        match self {
            StackNode::View { id, .. } => out.push(id.clone()),
            StackNode::Stack { items, .. } => items.iter().for_each(|(node, _)| node.views(out)),
        }
    }

    fn titles(&self, out: &mut Vec<SharedString>) {
        match self {
            StackNode::View { title, .. } => out.push(title.clone()),
            StackNode::Stack { items, .. } => items.iter().for_each(|(node, _)| node.titles(out)),
        }
    }

    /// 把这一格的 safe area 按各项碰到哪几条边分下去。
    fn areas(&self, area: SafeArea, f: &mut impl FnMut(&str, SafeArea)) {
        match self {
            StackNode::View { id, .. } => f(id, area),
            StackNode::Stack { axis, items } => {
                let last = items.len().saturating_sub(1);
                for (ix, (node, _)) in items.iter().enumerate() {
                    let (first, end) = (ix == 0, ix == last);
                    let touches = match axis {
                        api::StackAxis::Vertical => Edges {
                            top: first,
                            bottom: end,
                            left: true,
                            right: true,
                        },
                        api::StackAxis::Horizontal => Edges {
                            top: true,
                            bottom: true,
                            left: first,
                            right: end,
                        },
                    };
                    node.areas(area.child(touches), f);
                }
            }
        }
    }

    fn render(&self) -> AnyElement {
        match self {
            StackNode::View { view, .. } => view.view().into_any_element(),
            StackNode::Stack { axis, items } => {
                let vertical = *axis == api::StackAxis::Vertical;
                div()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .when(vertical, |stack| stack.flex_col())
                    .when(!vertical, |stack| stack.flex_row())
                    .children(items.iter().map(|(node, fit)| {
                        let item = div().min_w_0().min_h_0().flex().flex_col();
                        let item = match (vertical, *fit) {
                            (true, true) => item.flex_none().w_full(),
                            (false, true) => item.flex_none().h_full(),
                            (true, false) => item.flex_1().w_full(),
                            (false, false) => item.flex_1().h_full(),
                        };
                        item.child(node.render())
                    }))
                    .into_any_element()
            }
        }
    }
}

impl StackPanel {
    /// 建出来的项里的视图，按先后。
    pub(crate) fn views(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(root) = &self.root {
            root.views(&mut out);
        }
        out
    }

    /// 和别的面板叠成标签时标签上的样子：各项的标题连起来。一摞自己没有标题。
    fn tab_label(&self) -> TabLabel {
        let mut titles = Vec::new();
        if let Some(root) = &self.root {
            root.titles(&mut titles);
        }
        let text: SharedString = titles.join(" · ").into();
        TabLabel {
            text: text.clone(),
            label: text.into_any_element(),
            badge: None,
        }
    }

    /// 整摞的 safe area 分给各项的视图。
    fn areas(&self, area: SafeArea, f: &mut impl FnMut(&str, SafeArea)) {
        if let Some(root) = &self.root {
            root.areas(area, f);
        }
    }
}

/// 面板是一摞就取出来。
fn stack_of(panel: &Arc<dyn PanelView>) -> Option<Entity<StackPanel>> {
    panel.view().downcast::<StackPanel>().ok()
}

/// 面板是一个视图就取出来。
fn view_panel_of(panel: &Arc<dyn PanelView>) -> Option<Entity<ViewPanel>> {
    panel.view().downcast::<ViewPanel>().ok()
}

/// 组里只有这一块时，组顶上画不画标题带：视图自己不画顶（`draws_top_bar` 为假）才画。一摞的
/// 各项自己画顶，占位看不见。
fn has_title_band(panel: &Arc<dyn PanelView>, cx: &App) -> bool {
    view_panel_of(panel).is_some_and(|panel| !panel.read(cx).draws_top_bar())
}

/// 面板在标签条上的样子。
fn tab_label(panel: &Arc<dyn PanelView>, cx: &App) -> TabLabel {
    if let Some(view) = view_panel_of(panel) {
        return view.read(cx).tab_label(cx);
    }
    if let Some(stack) = stack_of(panel) {
        return stack.read(cx).tab_label();
    }
    let text: SharedString = panel.panel_name(cx).into();
    TabLabel {
        text: text.clone(),
        label: text.into_any_element(),
        badge: None,
    }
}

impl EventEmitter<PanelEvent> for StackPanel {}

impl Focusable for StackPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for StackPanel {
    fn panel_name(&self) -> &'static str {
        api::STACK_PANEL
    }

    fn visible(&self, _: &App) -> bool {
        self.root.is_some()
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    /// 各项自己画顶，整摞没有标题带，也就没有放大按钮。
    fn zoomable(&self, _: &App) -> bool {
        false
    }

    fn dump(&self, _: &App) -> PanelState {
        PanelState {
            panel_name: api::STACK_PANEL.to_string(),
            children: Vec::new(),
            info: PanelInfo::panel(self.info.clone()),
        }
    }
}

impl Render for StackPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .children(self.root.as_ref().map(StackNode::render))
    }
}

/// 一次 `load` 期间记的东西：这次摆出来的视图（第二份变占位），和这次新建的视图（装完
/// 要 `open` 一次，让它读数据）。
#[derive(Default)]
struct LoadPass {
    placed: HashSet<String>,
    built: Vec<Rc<dyn ViewHandle>>,
}

thread_local! {
    static LOAD_PASS: RefCell<Option<LoadPass>> = const { RefCell::new(None) };
}

/// 布局里摆的一个视图：这次装布局里第一次摆、登记着、能摆进布局，就建出来（已经建过的
/// 取那一份）。新建的记下，装完打开一次。
fn place_view(
    id: &str,
    window: &mut Window,
    cx: &mut App,
) -> Option<(api::ViewInfo, Rc<dyn ViewHandle>)> {
    let first = LOAD_PASS.with(|pass| {
        pass.borrow_mut()
            .as_mut()
            .is_none_or(|pass| pass.placed.insert(id.to_string()))
    });
    if !first {
        eprintln!("[layout] 视图 `{id}` 摆了不止一份，多出来的不显示");
        return None;
    }
    // 设置页由设置摆，按实例建的由别的插件带参数建，布局里摆了也当不认识。
    let spec = api::view(id, cx).filter(|spec| spec.settings.is_none() && !spec.instances)?;
    let (view, new) = api::ensure_view(id, window, cx)?;
    if new {
        LOAD_PASS.with(|pass| {
            if let Some(pass) = pass.borrow_mut().as_mut() {
                pass.built.push(view.clone());
            }
        });
    }
    Some((spec, view))
}

/// 布局里的一个面板名换成面板。Dock 自己的注册表里什么都没登记，所有面板都从这里建
/// （[`SmeltDockRenderer::build_placeholder`]）。
fn build_panel(state: &PanelState, window: &mut Window, cx: &mut App) -> Arc<dyn PanelView> {
    let placeholder = |cx: &mut App| -> Arc<dyn PanelView> {
        Arc::new(cx.new(|cx| PlaceholderPanel {
            state: state.clone(),
            focus_handle: cx.focus_handle(),
        }))
    };
    if let Some(stack) = api::Stack::of(state) {
        let (Ok(stack), PanelInfo::Panel(info)) = (stack, &state.info) else {
            return placeholder(cx);
        };
        let root = StackNode::build(&stack, window, cx);
        return Arc::new(cx.new(|cx| StackPanel {
            info: info.clone(),
            root,
            focus_handle: cx.focus_handle(),
        }));
    }
    let id = state.panel_name.as_str();
    let Some((spec, view)) = place_view(id, window, cx) else {
        return placeholder(cx);
    };
    let info = match &state.info {
        PanelInfo::Panel(info) => info.clone(),
        PanelInfo::Stack { .. } | PanelInfo::Tabs { .. } => Value::Null,
    };
    Arc::new(cx.new(|cx| ViewPanel {
        id: panel_name(id),
        title: spec.title,
        owns_top_bar: spec.draws_top_bar,
        content: None,
        view,
        info,
        focus_handle: cx.focus_handle(),
        _badges: cx.subscribe(
            &smelt_plugin_api::events(cx),
            |_, _, event: &smelt_plugin_api::HostEvent, cx| {
                if *event == smelt_plugin_api::HostEvent::ViewBadgesChanged {
                    cx.notify();
                }
            },
        ),
    }))
}

/// 按 `state` 装布局。这次新建的视图装完打开一次。
pub(crate) fn load(
    area: &Entity<DockArea>,
    state: DockAreaState,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<()> {
    LOAD_PASS.with(|pass| *pass.borrow_mut() = Some(LoadPass::default()));
    let loaded = area.update(cx, |area, cx| area.load(state, window, cx));
    let pass = LOAD_PASS
        .with(|pass| pass.borrow_mut().take())
        .unwrap_or_default();
    for view in pass.built {
        view.open(&Value::Null, window, cx);
    }
    loaded
}

// ---- 读 Dock ----

/// 布局里的面板，按树的先后（中间、左、右、下）。
fn all_panels(area: &DockArea) -> impl Iterator<Item = PanelId> + '_ {
    PLACEMENTS
        .into_iter()
        .filter_map(|placement| area.layout(placement))
        .flat_map(|tree| tree.panels())
}

/// 布局里摆着这个视图的面板，在一摞里的是那一摞。占位不算。
pub(crate) fn find_view(area: &DockArea, id: &str, cx: &App) -> Option<PanelId> {
    all_panels(area).find(|&panel| {
        area.panel(panel).is_some_and(|panel| {
            panel.visible(cx)
                && match stack_of(panel) {
                    Some(stack) => stack.read(cx).views().iter().any(|view| view == id),
                    None => panel.panel_name(cx) == id,
                }
        })
    })
}

/// 放着这块面板的组。
pub(crate) fn group_of(area: &DockArea, panel: PanelId) -> Option<NodeId> {
    PLACEMENTS
        .into_iter()
        .find_map(|placement| area.layout(placement)?.find_panel_node(panel))
}

/// 这块面板在哪个区域：中间，还是左、右、下停靠区。
pub(crate) fn placement_of(area: &DockArea, panel: PanelId) -> Option<DockPlacement> {
    PLACEMENTS.into_iter().find(|&placement| {
        area.layout(placement)
            .is_some_and(|tree| tree.find_panel_node(panel).is_some())
    })
}

/// 开着的那页摆着什么、看得见什么。Dock 一变、换页就重算（`Workspace::sync_shell_view`），
/// render 只读它。
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ShellView {
    /// 开着的那页的布局里摆着的视图。
    pub(crate) laid_out: HashSet<String>,
    /// 此刻看得见的视图：所在组显示着的那个（显示着一摞就是摞里的每一个），没被另一组的
    /// 放大挡住，也不在收起的 dock 里。
    pub(crate) shown: HashSet<String>,
    /// 看得见的视图的 safe area。所在组既没有标签条也没有标题带、又贴着窗口上沿的：它自己的
    /// 顶上就是窗口顶，拖窗口、标题这些归它自己画，占着窗口上面的角还要给角上的控件让位。
    /// 贴着窗口左下、右下角的：那个角让给窗口圆角，有没有标签条都一样（标签条在顶上）。不在
    /// 这里的全为 0。
    areas: HashMap<String, SafeArea>,
    /// 显示着的组的 safe area：贴着窗口上沿（或放大着）的组顶上是标题栏，贴着窗口左上、右上
    /// 角的组那个角让给红黄绿按钮和停靠区开合按钮。组有标签条、标题带时由它们按这个让位，
    /// 没有时整份给显示着的视图（[`Self::areas`]）。不在这里的全为 0。
    groups: HashMap<NodeId, SafeArea>,
    /// 左、右停靠区的开合按钮：`Some(开着没有)` 是要画，`None` 是布局里没有这个停靠区、
    /// 或者有组放大着（放大时别的都让开，按钮也不画）。
    pub(crate) left_dock: Option<bool>,
    pub(crate) right_dock: Option<bool>,
    /// 这页的出口（`smelt_plugin_api::OUTLET_VIEW`）里显示的视图；这页没摆出口是 `None`。
    /// 上面几项说的是 Dock 里的面板，出口里的视图要按出口查（[`Self::panel_of`]）；
    /// 只有 `shown` 把它也算进去。
    pub(crate) outlet: Option<String>,
    /// 窗口自己占掉的角：红黄绿按钮（全屏、Windows 下是 0）、窗口圆角。窗口进出全屏时工作台
    /// 更新它（见 `Workspace::window_corners`）。
    corners: WindowCorners,
}

impl ShellView {
    /// 视图画在哪个面板里：出口里显示着的是出口，别的是它自己。
    pub(crate) fn panel_of<'a>(&self, id: &'a str) -> &'a str {
        if self.outlet.as_deref() == Some(id) {
            smelt_plugin_api::OUTLET_VIEW
        } else {
            id
        }
    }

    /// 视图的 safe area：它贴着窗口顶、顶上没有标签条和标题带时顶上是标题栏，占着窗口
    /// 左上、右上角时那个角让给红黄绿按钮和停靠区开合按钮。别的视图（在标签条下面、没摆
    /// 着、藏着）全为 0。只读缓存。
    pub(crate) fn safe_area_of(&self, id: &str) -> SafeArea {
        self.areas
            .get(self.panel_of(id))
            .copied()
            .unwrap_or_default()
    }

    /// 组的 safe area，标签条、标题带按它让位。只读缓存。
    pub(crate) fn group_area(&self, node: NodeId) -> SafeArea {
        self.groups.get(&node).copied().unwrap_or_default()
    }

    /// 只看这页的 Dock。`corners` 是窗口自己此刻占掉的角。
    pub(crate) fn of(area: &DockArea, corners: WindowCorners, cx: &App) -> Self {
        let zoomed = area.zoomed_group();
        let toggle = |placement| {
            (zoomed.is_none() && area.is_dock_collapsible(placement))
                .then(|| area.is_dock_open(placement))
        };
        let (top_left, top_right) = (top_left_of(area), top_right_of(area));
        let (bottom_left, bottom_right) = (
            bottom_corner_of(area, DockPlacement::Left),
            bottom_corner_of(area, DockPlacement::Right),
        );
        let rounded = size(corners.rounded, corners.rounded);
        let mut view = Self {
            corners,
            left_dock: toggle(DockPlacement::Left),
            right_dock: toggle(DockPlacement::Right),
            ..Self::default()
        };
        let (leading, trailing) = (view.leading_inset(), view.trailing_inset());
        let mut top_groups = HashSet::new();
        for placement in PLACEMENTS {
            let open = placement == DockPlacement::Center || area.is_dock_open(placement);
            let Some(tree) = area.layout(placement) else {
                continue;
            };
            if open && placement != DockPlacement::Bottom {
                top_edge_groups(tree.root(), &mut top_groups);
            }
            tree.root().walk(&mut |node| {
                let PaneRef::Tabs { panels, active_ix } = node.kind() else {
                    return;
                };
                let visible: Vec<(usize, &Arc<dyn PanelView>)> = panels
                    .iter()
                    .enumerate()
                    .filter_map(|(ix, id)| Some((ix, area.panel(*id)?)))
                    .filter(|(_, panel)| panel.visible(cx))
                    .collect();
                for (_, panel) in &visible {
                    match stack_of(panel) {
                        Some(stack) => view.laid_out.extend(stack.read(cx).views()),
                        None => {
                            view.laid_out.insert(panel.panel_name(cx).to_string());
                        }
                    }
                }
                if !open || zoomed.is_some_and(|zoomed| zoomed != node.id()) {
                    return;
                }
                // 选中的那个被藏起来时，Dock 画第一个看得见的。
                let Some((_, displayed)) = visible
                    .iter()
                    .find(|(ix, _)| *ix == active_ix)
                    .or(visible.first())
                else {
                    return;
                };
                let mut group = SafeArea::default();
                if zoomed.is_some() || top_groups.contains(&node.id()) {
                    group.title_bar = true;
                    if top_left == Some(node.id()) {
                        group.corners.top_left = size(leading, TOP_BAR_HEIGHT);
                    }
                    if top_right == Some(node.id()) {
                        group.corners.top_right = size(trailing, TOP_BAR_HEIGHT);
                    }
                    view.groups.insert(node.id(), group);
                }
                // 有标签条、标题带的组顶上归它们，里面的视图在它们下面，上面什么都不用让。
                let bare = visible.len() == 1 && !has_title_band(displayed, cx);
                let mut shown = if bare { group } else { SafeArea::default() };
                // 下面两个角是窗口圆角，标签条在顶上碰不到，都归显示着的视图。
                if bottom_left == Some(node.id()) {
                    shown.corners.bottom_left = rounded;
                }
                if bottom_right == Some(node.id()) {
                    shown.corners.bottom_right = rounded;
                }
                let mut show = |id: &str, area: SafeArea| {
                    if area != SafeArea::default() {
                        view.areas.insert(id.to_string(), area);
                    }
                    view.shown.insert(id.to_string());
                };
                match stack_of(displayed) {
                    Some(stack) => stack.read(cx).areas(shown, &mut show),
                    None => show(displayed.panel_name(cx), shown),
                }
            });
        }
        view
    }

    /// 左开合按钮离 Dock 左边多远：红黄绿后面；没有红黄绿（全屏、Windows）时和右边的对称，
    /// 离窗口边 [`WINDOW_EDGE_MARGIN`]。
    fn left_toggle_x(&self) -> Pixels {
        self.corners.lights.max(WINDOW_EDGE_MARGIN)
    }

    /// 贴左上角的组开头让出的宽度，量到内容开始的地方：红黄绿按钮，再加左开合按钮和它后面的
    /// 间距。
    fn leading_inset(&self) -> Pixels {
        match self.left_dock {
            Some(_) => self.left_toggle_x() + px(DOCK_TOGGLE_WIDTH) + BAR_MARGIN,
            None => self.corners.lights,
        }
    }

    /// 贴右上角的组末尾让出的宽度，和左边对称：离窗口边的距离、右开合按钮、和内容的间距。
    fn trailing_inset(&self) -> Pixels {
        match self.right_dock {
            Some(_) => WINDOW_EDGE_MARGIN + px(DOCK_TOGGLE_WIDTH) + BAR_MARGIN,
            None => px(0.),
        }
    }
}

/// 贴着窗口上沿的组：横排的每一个子节点都贴着，竖排只有最上面一个。
fn top_edge_groups(node: &PaneNode, groups: &mut HashSet<NodeId>) {
    match node.kind() {
        PaneRef::Tabs { .. } => {
            groups.insert(node.id());
        }
        PaneRef::Split { axis, children, .. } => match axis {
            Axis::Horizontal => children
                .iter()
                .for_each(|child| top_edge_groups(child, groups)),
            Axis::Vertical => {
                if let Some(first) = children.first() {
                    top_edge_groups(first, groups);
                }
            }
        },
    }
}

/// 贴着窗口左上角的组。放大时是放大的那组；不然左 dock 开着从它找，否则从中间找。
fn top_left_of(area: &DockArea) -> Option<NodeId> {
    if let Some(node) = area.zoomed_group() {
        return Some(node);
    }
    open_root(area, DockPlacement::Left)
        .or(open_root(area, DockPlacement::Center))
        .and_then(|root| corner_group(root, false, false))
}

/// 贴着窗口右上角的组。放大时是放大的那组；不然右 dock 开着从它找，否则从中间找。
fn top_right_of(area: &DockArea) -> Option<NodeId> {
    if let Some(node) = area.zoomed_group() {
        return Some(node);
    }
    open_root(area, DockPlacement::Right)
        .or(open_root(area, DockPlacement::Center))
        .and_then(|root| corner_group(root, true, false))
}

/// 贴着窗口左下（`side` 是左）、右下角的组。放大时是放大的那组；不然那一侧的停靠区开着从它
/// 找（左右停靠区占满整个高）。收起着时角在中间那一列的底上：有下停靠区从它找，它收起着就
/// 只剩一条标签条，组里的视图看不见，不给；没有下停靠区从中间找。
fn bottom_corner_of(area: &DockArea, side: DockPlacement) -> Option<NodeId> {
    if let Some(node) = area.zoomed_group() {
        return Some(node);
    }
    let root = match open_root(area, side) {
        Some(root) => Some(root),
        None if area.layout(DockPlacement::Bottom).is_some() => {
            open_root(area, DockPlacement::Bottom)
        }
        None => open_root(area, DockPlacement::Center),
    };
    root.and_then(|root| corner_group(root, side == DockPlacement::Right, true))
}

/// 区域的树根。收起的停靠区不算。
fn open_root(area: &DockArea, placement: DockPlacement) -> Option<&PaneNode> {
    let open = placement == DockPlacement::Center || area.is_dock_open(placement);
    open.then(|| area.layout(placement))
        .flatten()
        .map(|tree| tree.root())
}

/// 树里贴着某个角的组：横排按 `right` 取最右或最左一个，竖排按 `bottom` 取最下或最上一个。
fn corner_group(node: &PaneNode, right: bool, bottom: bool) -> Option<NodeId> {
    match node.kind() {
        PaneRef::Tabs { .. } => Some(node.id()),
        PaneRef::Split { axis, children, .. } => {
            let last = match axis {
                Axis::Horizontal => right,
                Axis::Vertical => bottom,
            };
            let child = if last {
                children.last()
            } else {
                children.first()
            };
            child.and_then(|child| corner_group(child, right, bottom))
        }
    }
}

// ---- 皮肤 ----

/// 建好整个工作台的 Dock，套上 smelt 的皮肤。里面还是空的，布局由 `workspace_layout` 装。
///
/// 面板不让关，停靠区的开合按钮由外壳画在窗口上角（`Workspace::render_dock_toggles`），标签条
/// 里都不画。
pub(crate) fn new_area(
    workspace: gpui::WeakEntity<Workspace>,
    regions: SharedRegions,
    window: &mut Window,
    cx: &mut App,
) -> Entity<DockArea> {
    cx.new(|cx| {
        let renderer = SmeltDockRenderer {
            workspace,
            edges: DockEdges::new(cx.weak_entity()),
            regions,
        };
        DockArea::new(
            "smelt-shell",
            Some(smelt_plugin_api::LAYOUT_VERSION),
            window,
            cx,
        )
        .with_renderer(Rc::new(renderer))
    })
}

/// 外层 Dock 的画法。区域、分栏、中间的框用 gpui-base 的光框：底只由窗口根按地方铺
/// （`window_backdrop::paint`），栏与栏之间靠分隔条那根 `hair` 分开。各停靠区和中间那一列
/// 排好后把位置记进 `regions`，窗口根按布局说的材质铺底。
struct SmeltDockRenderer {
    workspace: gpui::WeakEntity<Workspace>,
    edges: DockEdges,
    regions: SharedRegions,
}

impl SmeltDockRenderer {
    /// 排好后把这块地方的位置记下来。
    fn record(
        &self,
        placement: DockPlacement,
    ) -> impl FnOnce(gpui::Bounds<Pixels>, &mut Window, &mut App) + 'static {
        let regions = self.regions.clone();
        move |bounds, _, _| regions.borrow_mut().record(placement, bounds)
    }
}

impl DockAreaRenderer for SmeltDockRenderer {
    fn render_split_handle(
        &self,
        handle: &ResizeHandleContext,
        _: &mut Window,
        _: &mut App,
    ) -> Option<AnyElement> {
        Some(smelt_ui::dock::split_handle(handle))
    }

    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        _: &mut Window,
        _: &mut App,
    ) -> AnyElement {
        div()
            .relative()
            .size_full()
            .child(self.edges.render(dock, content))
            .on_prepaint(self.record(dock.placement()))
            .into_any_element()
    }

    fn center_frame(&self, _: &mut Window, _: &mut App) -> gpui::Stateful<Div> {
        div()
            .id("dock-area-center")
            .relative()
            .on_prepaint(self.record(DockPlacement::Center))
    }

    /// Dock 自己的注册表是空的，每个面板都走到这里，按面板名去视图注册表里取。
    fn build_placeholder(
        &self,
        state: &PanelState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<dyn PanelView>> {
        Some(build_panel(state, window, cx))
    }

    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(SmeltTabGroup {
            workspace: self.workspace.clone(),
        })
    }
}

/// 标签组的画法。组不铺底，不按组里摆的是什么（出口还是别的视图）分深浅：布局由插件定，
/// 宿主按位置猜「这是侧栏」就会猜错。
struct SmeltTabGroup {
    workspace: gpui::WeakEntity<Workspace>,
}

impl TabGroupRenderer for SmeltTabGroup {
    /// 组里两个以上面板画一排标签；只有一个时画一条居中的标题带，自己管顶上的视图
    /// （`draws_top_bar`）和一摞什么都不画。两头按组的 safe area 给角上的控件让位。
    fn render_tab_bar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let visible: Vec<usize> = group
            .panels()
            .iter()
            .enumerate()
            .filter(|(_, panel)| panel.visible(cx))
            .map(|(ix, _)| ix)
            .collect();
        let area = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).shell_view.group_area(group.node()))
            .unwrap_or_default();
        match visible.as_slice() {
            [] => gpui::Empty.into_any_element(),
            [ix] => {
                if !has_title_band(&group.panels()[*ix], cx) {
                    return gpui::Empty.into_any_element();
                }
                render_title_band(group, *ix, &area, window, cx)
            }
            _ => render_tabs(group, visible, &area, cx),
        }
    }

    fn render_active_panel(
        &self,
        panel: gpui::AnyView,
        group: &TabGroupContext,
        _: &mut Window,
        _: &mut App,
    ) -> AnyElement {
        smelt_ui::dock::active_panel(panel, group)
    }

    fn render_drop_indicator(
        &self,
        indicator: DropIndicator,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        Some(smelt_ui::dock::drop_indicator(indicator, window, cx))
    }
}

/// 只有一块面板的组：一条居中的标题；在窗口标题栏上时整条拖窗口。别的组的标签拖到这里，
/// 并进这一组。
fn render_title_band(
    group: &TabGroupContext,
    ix: usize,
    area: &SafeArea,
    _: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let insets = area.top_bar_insets();
    let label = tab_label(&group.panels()[ix], cx);
    let droppable = group.is_droppable();
    let target = group.clone();
    let band = div()
        .id("smelt-title-band")
        .flex()
        .items_center()
        .flex_none()
        .w_full()
        .h(TOP_BAR_HEIGHT)
        .pl(insets.left)
        .pr(insets.right)
        .when(droppable, |this| {
            this.drag_over::<DragPanel>(|this, _, _, _| this.bg(ui_theme::row_selected()))
                .on_drop(move |drag: &DragPanel, window, cx| {
                    target.drop_panel(drag.clone(), None, true, window, cx);
                })
        })
        .child(
            div()
                .flex()
                .items_center()
                .flex_1()
                .min_w_0()
                .h_full()
                .gap_1()
                .justify_center()
                .type_style(TYPE.ui)
                .text_color(rgb(ui_theme::muted()))
                .child(label.label)
                .children(label.badge),
        )
        .children(zoom_button(group, cx));
    if area.title_bar {
        with_window_drag(band).into_any_element()
    } else {
        band.into_any_element()
    }
}

/// 两块以上面板的组：一排标签，拖标签重排、拖进别的组。在窗口标题栏上时开头的让位和标签
/// 后的空白拖窗口，标签本身不拖。
fn render_tabs(
    group: &TabGroupContext,
    visible: Vec<usize>,
    area: &SafeArea,
    cx: &mut App,
) -> AnyElement {
    let insets = area.top_bar_insets();
    let tabs = visible
        .into_iter()
        .map(|ix| (ix, tab_label(&group.panels()[ix], cx)))
        .collect();
    let lead = div().w(insets.left).h_full().flex_none();
    div()
        .flex()
        .items_center()
        .w_full()
        .h(TOP_BAR_HEIGHT)
        .flex_none()
        .child(if area.title_bar {
            with_window_drag(lead)
        } else {
            lead
        })
        .child(tab_strip(group, tabs, area.title_bar, cx))
        .child(
            div()
                .flex()
                .items_center()
                .flex_none()
                .h_full()
                .pr(insets.right)
                .children(zoom_button(group, cx)),
        )
        .into_any_element()
}

/// 放大 / 还原这一组。放大是 Dock 自己的：这一组铺满整个工作台，别的都让开。
fn zoom_button(group: &TabGroupContext, cx: &App) -> Option<AnyElement> {
    let zoomed = group.is_zoomed();
    if !zoomed && !group.active_panel().is_some_and(|panel| panel.zoomable(cx)) {
        return None;
    }
    let group = group.clone();
    Some(
        div()
            .flex_none()
            // 在拖窗口的条上：按下先拦住，不让它变成拖窗口。
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new("smelt-dock-zoom")
                    .quiet()
                    .small()
                    .selected(zoomed)
                    .toggled(zoomed)
                    .icon(if zoomed {
                        IconName::Minimize
                    } else {
                        IconName::Maximize
                    })
                    .icon_action(if zoomed {
                        t!("pane.restore")
                    } else {
                        t!("pane.zoom")
                    })
                    .on_click(move |_, window, cx| group.toggle_zoom(window, cx)),
            )
            .into_any_element(),
    )
}

// ---- 停靠区开合 ----

impl Workspace {
    /// 左右停靠区的开合按钮，浮在窗口左上角（红黄绿按钮右边）和右上角，收起、展开都在同一个
    /// 位置。按停靠区给，不认里面摆的是什么视图：布局把什么放进左右停靠区，什么就能收起。
    /// 停靠区收起后宽度是 0，什么都画不出来，重新打开只能靠这里。
    ///
    /// 左右对称：右边的离窗口边 [`WINDOW_EDGE_MARGIN`]，和红黄绿离左边一样；左边的在红黄绿
    /// 后面，没有红黄绿时也离窗口边这么远。贴着这两个角的组按它的 safe area 给按钮让了位
    /// （[`ShellView::group_area`]）。`inset` 是 Dock 外面那圈内边距。
    pub(crate) fn render_dock_toggles(&self, inset: Pixels) -> Vec<AnyElement> {
        let left = self.shell_view.left_dock.map(|open| {
            dock_toggle(DockPlacement::Left, open, self.shell_dock().clone())
                .left(inset + self.shell_view.left_toggle_x())
        });
        let right = self.shell_view.right_dock.map(|open| {
            dock_toggle(DockPlacement::Right, open, self.shell_dock().clone())
                .right(inset + WINDOW_EDGE_MARGIN)
        });
        [left, right]
            .into_iter()
            .flatten()
            .map(|toggle| toggle.top(inset).into_any_element())
            .collect()
    }
}

/// 一个停靠区的开合按钮。横向位置由调用方给。
fn dock_toggle(placement: DockPlacement, open: bool, area: Entity<DockArea>) -> Div {
    let (id, label) = match (placement, open) {
        (DockPlacement::Left, true) => ("dock-toggle-left", t!("dock.collapse_left")),
        (DockPlacement::Left, false) => ("dock-toggle-left", t!("dock.expand_left")),
        (_, true) => ("dock-toggle-right", t!("dock.collapse_right")),
        (_, false) => ("dock-toggle-right", t!("dock.expand_right")),
    };
    let button = Button::new(id).quiet().small();
    let button = match (placement, open) {
        (DockPlacement::Left, true) => button.icon(IconName::PanelLeftFilled),
        (DockPlacement::Left, false) => button.icon(IconName::PanelLeft),
        (_, true) => button.icon(IconName::PanelRightFilled),
        (_, false) => button.icon(IconName::PanelRight),
    };
    div()
        .absolute()
        .w(px(DOCK_TOGGLE_WIDTH))
        .h(TOP_BAR_HEIGHT)
        .flex()
        .items_center()
        .justify_center()
        // 浮在拖窗口的顶带上：按下先拦住，不让它变成拖窗口。
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            button
                .icon_action(label)
                .on_click(move |_, window, cx| {
                    area.update(cx, |area, cx| area.toggle_dock(placement, window, cx));
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::panel_name;

    #[test]
    fn panel_names_are_interned_once_per_id() {
        let first = panel_name("com.example.view");
        let again = panel_name(&String::from("com.example.view"));
        assert!(std::ptr::eq(first, again));
        assert_eq!(first, "com.example.view");
    }
}
