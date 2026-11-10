//! 工作台外壳：整个工作台是一块 `DockArea`，这里是 smelt 给它的皮肤和面板。
//!
//! 摆什么、怎么切、选中哪个、放大都在 Dock 上（布局和存档就是它的 `DockAreaState`，见
//! `workspace_layout`）。这里只管：
//! - 面板（[`ViewPanel`]）：视图注册表里的一个视图（`smelt_plugin_api::view`），面板名就是
//!   视图 id。外壳不认识视图是什么，自带的也一样（见 `builtin_views`）。
//! - 占位（[`PlaceholderPanel`]）：没登记的视图、同一个视图摆了第二份。不显示，存档时原样
//!   写回：插件装回来就又出现了。
//! - 皮肤（[`SmeltDockRenderer`] / [`SmeltTabGroup`]）：组件库 `DockSkin` 外面包一层，标签条
//!   由 smelt 画——角标、拖窗口的空白、给红黄绿按钮让位、放大按钮、拖动重排。
//! - 读 Dock：[`ShellView`] 是摆着什么、看得见什么的快照，工作台缓存一份给 render 读。
//!
//! 终端分屏是舞台里另一个 Dock（`term_dock`），和这里无关。

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Axis, Context, Div, Entity, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, Hsla, Pixels, SharedString, Stateful, Window, div, prelude::*, px, rgb,
    size,
};
use gpui_base::ResizeHandleContext;
use gpui_component::dock::{
    BasePanel, BasePanelView, DockArea, DockAreaRenderer, DockAreaState, DockContext,
    DockPlacement, DockSkin, DragPanel, DropIndicator, NodeId, PaneNode, PaneRef, Panel,
    PanelControl, PanelEvent, PanelHandle, PanelId, PanelInfo, PanelState, TabGroupContext,
    TabGroupRenderer, panel_handle,
};
use gpui_component::empty::{
    Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle,
};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable as _, h_flex};
use serde_json::Value;
use smelt_plugin_api::{self as api, ViewHandle};
pub(crate) use smelt_ui::Button;
use smelt_ui::IconAction as _;

use crate::builtin_views::Builtin;
use crate::settings::Appearance;
use crate::{
    AgentStatus, SessionKind, Workspace, WorkspaceRoute, terminal, ui_theme, workspace_frame,
};

/// Dock 的四个区域，按画的先后。
const PLACEMENTS: [DockPlacement; 4] = [
    DockPlacement::Center,
    DockPlacement::Left,
    DockPlacement::Right,
    DockPlacement::Bottom,
];

/// 红黄绿按钮占的宽度。
const TRAFFIC_LIGHTS_WIDTH: f32 = 80.;

/// 拖标签时跟着鼠标的那块的大小。和组件库的一样。
const DRAG_PREVIEW_SIZE: gpui::Size<Pixels> = size(px(96.), px(30.));

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
    focus_handle: FocusHandle,
}

impl ViewPanel {
    /// 会话列表、舞台自己管顶上那一条：单独一组时不要标题带，也不放大。
    fn owns_top_bar(&self) -> bool {
        Builtin::of(self.id).is_some_and(Builtin::owns_top_bar)
    }
}

impl EventEmitter<PanelEvent> for ViewPanel {}

impl Focusable for ViewPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for ViewPanel {
    fn panel_name(&self) -> &'static str {
        self.id
    }

    /// 不让关：还没有把视图加回布局的入口。
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        !self.owns_top_bar()
    }

    fn dump(&self, _: &App) -> PanelState {
        PanelState {
            panel_name: self.id.to_string(),
            children: Vec::new(),
            info: PanelInfo::panel(self.info.clone()),
        }
    }
}

impl Panel for ViewPanel {
    fn tab_name(&self, _: &App) -> Option<SharedString> {
        Some(self.title.clone())
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.title.clone()
    }

    /// 角标由视图自己给（`PluginView::badge`）。
    fn title_suffix(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        self.view
            .badge(cx)
            .map(|badge| Tag::primary().small().rounded_full().child(badge))
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        (!self.owns_top_bar()).then_some(PanelControl::Toolbar)
    }

    fn title_bar(&self, _: &App) -> bool {
        !self.owns_top_bar()
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
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

impl BasePanel for PlaceholderPanel {
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

impl Panel for PlaceholderPanel {}

impl Render for PlaceholderPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui::Empty
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

/// 布局里的一个面板名换成面板。Dock 自己的注册表里什么都没登记，所有面板都从这里建
/// （[`SmeltDockRenderer::build_placeholder`]）。
fn build_panel(state: &PanelState, window: &mut Window, cx: &mut App) -> Arc<dyn BasePanelView> {
    let id = state.panel_name.as_str();
    let first = LOAD_PASS.with(|pass| {
        pass.borrow_mut()
            .as_mut()
            .is_none_or(|pass| pass.placed.insert(id.to_string()))
    });
    let spec = api::views(cx)
        .into_iter()
        .find(|spec| spec.id.as_ref() == id);
    let built = first
        .then(|| spec.and_then(|spec| Some((spec, api::ensure_view(id, window, cx)?))))
        .flatten();
    let Some((spec, (view, new))) = built else {
        if !first {
            eprintln!("[layout] 视图 `{id}` 摆了不止一份，多出来的不显示");
        }
        return Arc::new(cx.new(|cx| PlaceholderPanel {
            state: state.clone(),
            focus_handle: cx.focus_handle(),
        }));
    };
    if new {
        LOAD_PASS.with(|pass| {
            if let Some(pass) = pass.borrow_mut().as_mut() {
                pass.built.push(view.clone());
            }
        });
    }
    let info = match &state.info {
        PanelInfo::Panel(info) => info.clone(),
        PanelInfo::Stack { .. } | PanelInfo::Tabs { .. } => Value::Null,
    };
    panel_handle(cx.new(|cx| ViewPanel {
        id: panel_name(id),
        title: spec.title,
        view,
        info,
        focus_handle: cx.focus_handle(),
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

/// 布局里摆着这个视图的面板。占位不算。
pub(crate) fn find_view(area: &DockArea, id: &str, cx: &App) -> Option<PanelId> {
    all_panels(area).find(|&panel| {
        area.panel(panel)
            .is_some_and(|panel| panel.visible(cx) && panel.panel_name(cx) == id)
    })
}

/// 放着这块面板的组。
pub(crate) fn group_of(area: &DockArea, panel: PanelId) -> Option<NodeId> {
    PLACEMENTS
        .into_iter()
        .find_map(|placement| area.layout(placement)?.find_panel_node(panel))
}

/// 摆着什么、看得见什么。Dock 一变就重算（`Workspace::sync_shell_view`），render 只读它。
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ShellView {
    /// 布局里摆着的视图。
    pub(crate) laid_out: HashSet<String>,
    /// 此刻看得见的视图：所在组显示着的那个，没被另一组的放大挡住，也不在收起的 dock 里。
    pub(crate) shown: HashSet<String>,
    /// 看得见、所在组既没有标签条也没有标题带、又贴着窗口上沿的视图：它自己的顶上就是
    /// 窗口顶，拖窗口、标题这些归它自己画。
    pub(crate) bare: HashSet<String>,
    /// 贴着窗口左上角的组，和它显示着的视图。那组开头要给红黄绿按钮让位。
    pub(crate) top_left_group: Option<NodeId>,
    pub(crate) top_left: Option<String>,
}

impl ShellView {
    pub(crate) fn of(area: &DockArea, cx: &App) -> Self {
        let zoomed = area.zoomed_group();
        let mut view = Self::default();
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
                let visible: Vec<(usize, &Arc<dyn BasePanelView>)> = panels
                    .iter()
                    .enumerate()
                    .filter_map(|(ix, id)| Some((ix, area.panel(*id)?)))
                    .filter(|(_, panel)| panel.visible(cx))
                    .collect();
                for (_, panel) in &visible {
                    view.laid_out.insert(panel.panel_name(cx).to_string());
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
                let id = displayed.panel_name(cx).to_string();
                let no_bar = visible.len() == 1
                    && !PanelHandle::of(displayed).is_some_and(|handle| handle.title_bar(cx));
                if no_bar && (zoomed.is_some() || top_groups.contains(&node.id())) {
                    view.bare.insert(id.clone());
                }
                if Some(node.id()) == top_left_of(area) {
                    view.top_left = Some(id.clone());
                }
                view.shown.insert(id);
            });
        }
        view.top_left_group = top_left_of(area);
        view
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
    let root = |placement| {
        let open = placement == DockPlacement::Center || area.is_dock_open(placement);
        open.then(|| area.layout(placement))
            .flatten()
            .map(|tree| tree.root())
    };
    root(DockPlacement::Left)
        .or(root(DockPlacement::Center))
        .and_then(top_left_group)
}

fn top_left_group(node: &PaneNode) -> Option<NodeId> {
    match node.kind() {
        PaneRef::Tabs { .. } => Some(node.id()),
        PaneRef::Split { children, .. } => children.first().and_then(top_left_group),
    }
}

/// 红黄绿按钮占的宽度：全屏、Windows 下是 0。
pub(crate) fn lights_width(window: &Window) -> Pixels {
    if workspace_frame::traffic_lights_visible(window) {
        px(TRAFFIC_LIGHTS_WIDTH)
    } else {
        px(0.)
    }
}

// ---- 皮肤 ----

/// 建好整个工作台的 Dock，套上 smelt 的皮肤。里面还是空的，布局由 `workspace_layout` 装。
pub(crate) fn new_area(
    workspace: gpui::WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<DockArea> {
    let mut skin = None;
    let area = cx.new(|cx| {
        let inner = DockSkin::new(cx);
        skin = Some(inner.clone());
        DockArea::new(
            "smelt-shell",
            Some(crate::workspace_layout::VERSION),
            window,
            cx,
        )
        .with_renderer(Rc::new(SmeltDockRenderer {
            skin: inner,
            workspace,
        }))
    });
    // 上面的闭包在 `cx.new` 返回前就跑完了。
    let skin = skin.expect("DockSkin::new 在构造 Dock 时已经跑过");
    // 不做收起，面板也不让关：皮肤自带的这两个按钮都不要。
    skin.set_toggle_button_visible(false, cx);
    skin.set_close_button_visible(false, cx);
    area
}

/// 外层 Dock 的画法：`DockSkin` 的全部，加上 smelt 的标签条和面板。
struct SmeltDockRenderer {
    skin: Rc<DockSkin>,
    workspace: gpui::WeakEntity<Workspace>,
}

impl DockAreaRenderer for SmeltDockRenderer {
    fn frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        DockAreaRenderer::frame(&*self.skin, window, cx)
    }

    fn center_frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        DockAreaRenderer::center_frame(&*self.skin, window, cx)
    }

    fn split_frame(
        &self,
        node: NodeId,
        axis: Axis,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        DockAreaRenderer::split_frame(&*self.skin, node, axis, window, cx)
    }

    fn render_split_handle(
        &self,
        handle: &ResizeHandleContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        DockAreaRenderer::render_split_handle(&*self.skin, handle, window, cx)
    }

    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        DockAreaRenderer::render_dock(&*self.skin, dock, content, window, cx)
    }

    /// Dock 自己的注册表是空的，每个面板都走到这里，按面板名去视图注册表里取。
    fn build_placeholder(
        &self,
        state: &PanelState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<dyn BasePanelView>> {
        Some(build_panel(state, window, cx))
    }

    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(SmeltTabGroup {
            inner: DockAreaRenderer::tab_group_renderer(&*self.skin),
            workspace: self.workspace.clone(),
        })
    }
}

/// 标签组的画法：标签条是 smelt 的，其余交给皮肤。
struct SmeltTabGroup {
    inner: Rc<dyn TabGroupRenderer>,
    workspace: gpui::WeakEntity<Workspace>,
}

impl TabGroupRenderer for SmeltTabGroup {
    /// 组的底色是栏的底色，不用主题的背景色：主题色可能带透明度，会透出窗口后面。
    fn frame(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.inner
            .frame(group, window, cx)
            .bg(Hsla::from(rgb(ui_theme::bg_column())))
    }

    fn content_frame(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.inner.content_frame(group, window, cx)
    }

    /// 组里两个以上面板画一排标签；只有一个时画一条居中的标题带，自己管顶上的视图
    /// （会话列表、舞台）什么都不画。
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
        let top_left = self.workspace.upgrade().is_some_and(|workspace| {
            workspace.read(cx).shell_view.top_left_group == Some(group.node())
        });
        let leading = if top_left {
            lights_width(window)
        } else {
            px(0.)
        };
        match visible.as_slice() {
            [] => gpui::Empty.into_any_element(),
            [ix] => {
                let panel = &group.panels()[*ix];
                if !PanelHandle::of(panel).is_some_and(|handle| handle.title_bar(cx)) {
                    return gpui::Empty.into_any_element();
                }
                render_title_band(group, *ix, leading, window, cx)
            }
            _ => render_tabs(group, visible, leading, window, cx),
        }
    }

    fn render_active_panel(
        &self,
        panel: gpui::AnyView,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.inner.render_active_panel(panel, group, window, cx)
    }

    fn render_drop_indicator(
        &self,
        indicator: DropIndicator,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.inner.render_drop_indicator(indicator, window, cx)
    }

    fn render_empty(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.inner.render_empty(group, window, cx)
    }
}

/// 只有一块面板的组：一条居中的标题，整条拖窗口；别的组的标签拖到这里，并进这一组。
fn render_title_band(
    group: &TabGroupContext,
    ix: usize,
    leading: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let bar_h = workspace_frame::TOP_BAR_HEIGHT;
    let panel = &group.panels()[ix];
    let title = panel_label(panel, window, cx);
    let badge = PanelHandle::of(panel).and_then(|handle| handle.title_suffix(window, cx));
    let muted = cx.theme().muted_foreground;
    let droppable = group.is_droppable();
    let target = group.clone();
    workspace_frame::with_window_drag(
        h_flex()
            .id("smelt-title-band")
            .flex_none()
            .w_full()
            .h(bar_h)
            .pl(leading)
            .when(droppable, |this| {
                this.drag_over::<DragPanel>(|this, _, _, cx| this.bg(cx.theme().tokens.drop_target))
                    .on_drop(move |drag: &DragPanel, window, cx| {
                        target.drop_panel(drag.clone(), None, true, window, cx);
                    })
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .gap_1()
                    .justify_center()
                    .text_sm()
                    .text_color(muted)
                    .child(title)
                    .children(badge),
            )
            .children(zoom_button(group, cx)),
    )
    .into_any_element()
}

/// 两块以上面板的组：一排标签，拖标签重排、拖进别的组。标签本身不拖窗口，开头的让位和
/// 标签后的空白拖。
fn render_tabs(
    group: &TabGroupContext,
    visible: Vec<usize>,
    leading: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let bar_h = workspace_frame::TOP_BAR_HEIGHT;
    let selected = visible.iter().position(|ix| *ix == group.active_ix());
    let draggable = group.is_draggable();
    let droppable = group.is_droppable();
    let tabs: Vec<Tab> = visible
        .iter()
        .map(|&ix| {
            let panel = group.panels()[ix].clone();
            // 角标由面板自己给（`Panel::title_suffix`），这里不认具体是哪块。
            let badge = PanelHandle::of(&panel).and_then(|handle| handle.title_suffix(window, cx));
            let label = panel_label(&panel, window, cx);
            let drag = draggable.then(|| group.drag_panel(ix, cx)).flatten();
            Tab::new()
                .child(label)
                .when_some(badge, |tab, badge| tab.suffix(badge))
                .when_some(drag, |tab, drag| {
                    tab.on_drag(drag, move |drag, offset, _window, cx| {
                        cx.stop_propagation();
                        drag.set_drag_offset(offset);
                        drag.set_preview_size(DRAG_PREVIEW_SIZE);
                        let title: SharedString = panel_text(&panel, cx);
                        cx.new(|_| DragPreview { title })
                    })
                })
                .when(droppable, |tab| {
                    let group = group.clone();
                    tab.drag_over::<DragPanel>(|this, _, _, cx| {
                        this.border_l_2().border_color(cx.theme().drag_border)
                    })
                    .on_drop(move |drag: &DragPanel, window, cx| {
                        group.drop_panel(drag.clone(), Some(ix), true, window, cx);
                    })
                })
        })
        .collect();
    let tabs_count = group.panels().len();
    let select = group.clone();
    let tail = group.clone();
    let node = group.node();
    TabBar::new("smelt-tab-bar")
        .underline()
        .xsmall()
        .w_full()
        .h(bar_h)
        .flex_none()
        .when_some(selected, |bar, ix| bar.selected_index(ix))
        // 标签条按显示的先后编号，隐藏的面板不占号，换回组里的下标再选。
        .on_click(move |shown: &usize, window, cx| {
            if let Some(&ix) = visible.get(*shown) {
                select.select_tab(ix, window, cx);
            }
        })
        .children(tabs)
        .prefix(workspace_frame::with_window_drag(
            div().w(leading + px(16.)).h(bar_h).flex_none(),
        ))
        // 标签后的空白：拖窗口，也收拖过来的标签（放到最后）。
        .last_empty_space(workspace_frame::with_window_drag(
            div()
                .id("smelt-tab-bar-empty")
                .flex_1()
                .min_w_0()
                .h(bar_h)
                .when(droppable, |this| {
                    this.drag_over::<DragPanel>(|this, _, _, cx| {
                        this.bg(cx.theme().tokens.drop_target)
                    })
                    .on_drop(move |drag: &DragPanel, window, cx| {
                        // 自己组里的拖到最后一格；别处来的追加在后面，不抢显示。
                        let ix = (drag.source() == node).then(|| tabs_count - 1);
                        tail.drop_panel(drag.clone(), ix, false, window, cx);
                    })
                }),
        ))
        .suffix(
            h_flex()
                .flex_none()
                .h(bar_h)
                .children(zoom_button(group, cx)),
        )
        .into_any_element()
}

/// 拖标签时跟着鼠标的那块。组件库的 `DragPanelPreview` 建不出来（字段私有），照它的样子画。
struct DragPreview {
    title: SharedString,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .id("smelt-drag-panel")
            .cursor_grab()
            .py_1()
            .px_3()
            .w_24()
            .overflow_hidden()
            .whitespace_nowrap()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .text_color(theme.tab_foreground)
            .bg(theme.tokens.tab_active)
            .opacity(0.75)
            .child(self.title.clone())
    }
}

/// 标签上的名字：面板给的短名，没有就用它的标题。
fn panel_label(panel: &Arc<dyn BasePanelView>, window: &mut Window, cx: &mut App) -> AnyElement {
    match PanelHandle::of(panel) {
        Some(handle) => match handle.tab_name(cx) {
            Some(name) => name.into_any_element(),
            None => handle.title(window, cx),
        },
        None => panel_text(panel, cx).into_any_element(),
    }
}

/// 面板名字的纯文本，拖动预览用。只读内存。
fn panel_text(panel: &Arc<dyn BasePanelView>, cx: &App) -> SharedString {
    PanelHandle::of(panel)
        .and_then(|handle| handle.tab_name(cx))
        .unwrap_or_else(|| panel.panel_name(cx).into())
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
            .px_1()
            // 在拖窗口的条上：按下先拦住，不让它变成拖窗口。
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new("smelt-dock-zoom")
                    .quiet()
                    .compact()
                    .selected(zoomed)
                    .toggled(zoomed)
                    .icon(if zoomed {
                        IconName::Minimize
                    } else {
                        IconName::Maximize
                    })
                    .icon_action(if zoomed { "还原" } else { "放大" })
                    .on_click(move |_, window, cx| group.toggle_zoom(window, cx)),
            )
            .into_any_element(),
    )
}

// ---- 会话列表和舞台的画法 ----

impl Workspace {
    /// 会话列表。自己顶着窗口上沿时（单独一组、贴顶）让出顶栏的高度，空白处拖窗口；
    /// 贴左上角时拖窗口的条从红黄绿按钮右边开始。
    pub(crate) fn render_sidebar_dock(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sessions = Builtin::Sessions.id();
        let animate = self.workspace_window_active && self.view_shown(sessions);
        let at_top = self.shell_view.bare.contains(sessions);
        let drag_left = if self.shell_view.top_left.as_deref() == Some(sessions) {
            lights_width(window)
        } else {
            px(0.)
        };
        let statuses: Vec<AgentStatus> = self
            .sessions
            .iter()
            .map(|session| session.status(cx))
            .collect();
        let focused_key = self.sidebar_focused_key(window);
        let list =
            self.render_session_list(cx.entity(), window, cx, &statuses, animate, focused_key);
        workspace_frame::card(rgb(ui_theme::bg_column()).into())
            .when(at_top, |card| card.pt(workspace_frame::TOP_BAR_HEIGHT))
            // 从访达拖文件夹进会话列表 = 打开项目。不挂整个窗口：对话输入框要自己收文件。
            .drag_over::<ExternalPaths>(|style, _paths, _window, cx| {
                style.bg(cx.theme().drop_target)
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                this.open_paths(paths.paths(), cx);
            }))
            .child(div().size_full().min_w_0().child(list))
            .when(at_top, |card| {
                card.child(workspace_frame::with_window_drag(
                    div()
                        .absolute()
                        .top_0()
                        .left(drag_left)
                        .right_0()
                        .h(workspace_frame::TOP_BAR_HEIGHT),
                ))
            })
            .into_any_element()
    }

    /// 舞台：当前会话，或宿主自己的页面（智能体、自动化、远程桌面），或没摆进布局的插件视图。
    pub(crate) fn render_stage_dock(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stage_content: AnyElement = match self.active_tab().clone() {
            WorkspaceRoute::Agents => self.render_agents_page(window, cx),
            WorkspaceRoute::Automations => self.render_automations_page(window, cx),
            // 摆进布局的视图画在它自己的面板里，一个视图不能画两处。
            WorkspaceRoute::Page(id) if self.shell_view.laid_out.contains(id.as_str()) => {
                div().flex_1().into_any_element()
            }
            WorkspaceRoute::Page(id) => self.render_page(&id, cx),
            WorkspaceRoute::Desktop => self.render_desktop_page(window, cx),
            WorkspaceRoute::Session => self.render_stage_session(cx),
        };
        let content = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(stage_content);
        // 标题胶囊和拖窗口的顶带浮在舞台内容上沿，只盖舞台自己。
        let chrome = self.render_stage_chrome(self.chrome_left_guard(window), cx);
        workspace_frame::card(rgb(ui_theme::bg_stage()).into())
            .child(content)
            .child(chrome)
            .into_any_element()
    }

    fn render_stage_session(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(session) = self
            .sessions
            .get(self.active_session)
            .filter(|session| !session.is_product_conversation(cx))
        else {
            return self.render_stage_empty(cx);
        };
        let body = match &session.kind {
            SessionKind::Term { dock, .. } => div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .bg(rgb(terminal::default_bg()))
                .pt(px(ui_theme::CHROME_ISLAND_BAND_PX))
                .child(dock.area().clone())
                .into_any_element(),
            SessionKind::Conversation(view) => {
                let appearance = cx.global::<Appearance>();
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .bg(rgb(ui_theme::bg_stage()))
                    .children(appearance.bg_image.as_deref().map(|path| {
                        workspace_frame::background_image_layer(
                            path,
                            appearance.bg_image_opacity,
                            cx,
                        )
                    }))
                    .child(
                        view.clone()
                            .cached(gpui::StyleRefinement::default().size_full()),
                    )
                    .into_any_element()
            }
        };
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(div().flex_1().min_w_0().min_h_0().flex().child(body))
            .into_any_element()
    }

    fn render_stage_empty(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.spaces.is_empty() {
            return stage_empty_state(
                gpui_kit_assets::IconName::Server,
                "添加远程机器",
                "配对一台 Linux 或 macOS 机器后，就可以在这里打开项目、对话和终端。",
                Some(
                    Button::new("empty-add-runtime")
                        .commit()
                        .label("添加远程机器")
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.open_settings_section(
                                crate::settings::SettingsSection::Remote,
                                cx,
                            );
                        })),
                ),
            )
            .into_any_element();
        }
        stage_empty_state(
            gpui_kit_assets::IconName::Coffee,
            "休息一下",
            "没有打开的会话",
            None,
        )
        .into_any_element()
    }
}

/// 舞台空态。
fn stage_empty_state(
    icon: impl Into<Icon>,
    title: &'static str,
    description: &'static str,
    action: Option<Button>,
) -> Div {
    let empty = Empty::new()
        .header(
            EmptyHeader::new()
                .media(
                    EmptyMedia::new()
                        .with_variant(EmptyMediaVariant::Icon)
                        .child(Icon::new(icon)),
                )
                .title(EmptyTitle::new().child(title))
                .description(EmptyDescription::new().max_w(px(420.)).child(description)),
        )
        .when_some(action, |empty, action| {
            empty.content(EmptyContent::new().child(action))
        });
    div().flex_1().flex().child(empty)
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
