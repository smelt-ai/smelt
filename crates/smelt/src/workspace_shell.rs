//! 工作台外壳：外层 Dock，和 smelt 自己的那层画法。
//!
//! 外层 Dock 里摆什么由布局决定（`workspace_layout`）。停靠区的开合和宽度、拆分比例、
//! 各组选中的标签都只在 Dock 上；变了 Dock 发 `LayoutChanged`，工作台按布局落盘
//! （`workspace_layout::save_layout_state`）。Dock 锁住不让拖动和放下，排布只改布局文件。
//! 终端分屏是舞台里另一个 Dock（`term_dock`），锁住外层它的标签才不会被放进侧栏。
//!
//! 画法是组件库的 `DockSkin` 外面包一层（[`SmeltDockRenderer`]）：停靠区和中间之间留出壳的
//! 间距，标签条由 smelt 画（[`SmeltTabGroup`]）——角标、拖窗口的空白、给红黄绿按钮和
//! 窗口开关让位、放大按钮；分隔条、拖宽、放下提示、放大本身都交给皮肤和 Dock。

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, AnyView, App, Axis, Div, Entity, EventEmitter, ExternalPaths, FocusHandle,
    Focusable, Hsla, Pixels, Stateful, Subscription, WeakEntity, Window, div, prelude::*, px, rgb,
};
use gpui_base::ResizeHandleContext;
use gpui_component::dock::{
    BasePanel, BasePanelView, DockArea, DockAreaRenderer, DockContext, DockPlacement, DockSkin,
    DropIndicator, NodeId, PaneNode, PaneRef, Panel, PanelControl, PanelEvent, PanelHandle,
    PanelId, PanelState, TabGroupContext, TabGroupRenderer,
};
use gpui_component::empty::{
    Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle,
};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable as _, h_flex};
use smelt_client::layout::{self, Component};
pub(crate) use smelt_ui::Button;
use smelt_ui::IconAction as _;

use crate::settings::Appearance;
use crate::{
    AgentStatus, SessionKind, Workspace, WorkspaceRoute, terminal, ui_theme, workspace_frame,
};

const SHELL_VERSION: usize = 1;

/// 建好外层 Dock，套上 smelt 的画法。里面还是空的，布局由 `workspace_layout` 装。
pub(crate) fn new_area(
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<DockArea> {
    let mut skin = None;
    let area = cx.new(|cx| {
        let inner = DockSkin::new(cx);
        skin = Some(inner.clone());
        DockArea::new("smelt-shell", Some(SHELL_VERSION), window, cx).with_renderer(Rc::new(
            SmeltDockRenderer {
                skin: inner,
                workspace,
            },
        ))
    });
    // 上面的闭包在 `cx.new` 返回前就跑完了。
    let skin = skin.expect("DockSkin::new 在构造 Dock 时已经跑过");
    // 停靠区的开关是窗口上那两个按钮，标签也不让关。
    skin.set_toggle_button_visible(false, cx);
    skin.set_close_button_visible(false, cx);
    area
}

/// 外层 Dock 的画法：`DockSkin` 的全部，加上停靠区外沿的间距和 smelt 的标签条。
struct SmeltDockRenderer {
    skin: Rc<DockSkin>,
    workspace: WeakEntity<Workspace>,
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

    /// 停靠区朝中间那一边留出壳的间距，透出壳的底色。拖宽的把手由皮肤贴在停靠区边上。
    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let gap = ui_theme::chrome_gap();
        let padded = div()
            .size_full()
            .flex()
            .map(|this| match dock.placement() {
                DockPlacement::Left => this.pr(gap),
                DockPlacement::Right => this.pl(gap),
                DockPlacement::Bottom => this.pt(gap),
                DockPlacement::Center => this,
            })
            .child(content);
        DockAreaRenderer::render_dock(&*self.skin, dock, padded.into_any_element(), window, cx)
    }

    fn build_placeholder(
        &self,
        state: &PanelState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<dyn BasePanelView>> {
        DockAreaRenderer::build_placeholder(&*self.skin, state, window, cx)
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
    workspace: WeakEntity<Workspace>,
}

impl TabGroupRenderer for SmeltTabGroup {
    /// 组的底色是栏的底色，不用主题的背景色：主题色可能带透明度，会透出窗口后面。
    fn frame(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        let surface: Hsla = rgb(ui_theme::bg_column()).into();
        self.inner.frame(group, window, cx).bg(surface)
    }

    fn content_frame(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.inner.content_frame(group, window, cx)
    }

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
        let edges = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).tab_bar_edges(group.node(), window, cx))
            .unwrap_or_default();
        match visible.as_slice() {
            [] => gpui::Empty.into_any_element(),
            [ix] => {
                let panel = &group.panels()[*ix];
                // 自己画顶栏的面板（会话列表、舞台）单独一组时不要标题条。
                if PanelHandle::of(panel).is_some_and(|handle| !handle.title_bar(cx)) {
                    return gpui::Empty.into_any_element();
                }
                render_title_band(group, *ix, edges, window, cx)
            }
            _ => render_tabs(group, visible, edges, window, cx),
        }
    }

    fn render_active_panel(
        &self,
        panel: AnyView,
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

/// 标签条两头要让出的宽度：左头让红黄绿按钮和左栏开关，右头让右栏开关。
/// 让出来的地方照样拖窗口。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct TabBarEdges {
    pub(crate) leading: Pixels,
    pub(crate) trailing: Pixels,
}

/// 只有一块面板的组：一条居中的标题，整条拖窗口。
fn render_title_band(
    group: &TabGroupContext,
    ix: usize,
    edges: TabBarEdges,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let bar_h = workspace_frame::TOP_BAR_HEIGHT;
    let panel = &group.panels()[ix];
    let title = panel_label(panel, window, cx);
    let muted = cx.theme().muted_foreground;
    workspace_frame::with_window_drag(
        h_flex()
            .id("smelt-title-band")
            .flex_none()
            .w_full()
            .h(bar_h)
            .pl(edges.leading)
            .pr(edges.trailing)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(muted)
                    .child(title),
            )
            .children(zoom_button(group, cx)),
    )
    .into_any_element()
}

/// 两块以上面板的组：一排标签。标签本身不拖窗口，两头的让位和标签后的空白拖。
fn render_tabs(
    group: &TabGroupContext,
    visible: Vec<usize>,
    edges: TabBarEdges,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let bar_h = workspace_frame::TOP_BAR_HEIGHT;
    let selected = visible.iter().position(|ix| *ix == group.active_ix());
    let tabs: Vec<Tab> = visible
        .iter()
        .map(|&ix| {
            let panel = &group.panels()[ix];
            // 角标由面板自己给（`Panel::title_suffix`），这里不认具体是哪块。
            let badge = PanelHandle::of(panel).and_then(|handle| handle.title_suffix(window, cx));
            Tab::new()
                .child(panel_label(panel, window, cx))
                .when_some(badge, |tab, badge| tab.suffix(badge))
        })
        .collect();
    let select = group.clone();
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
            div().w(edges.leading + px(16.)).h(bar_h).flex_none(),
        ))
        .last_empty_space(workspace_frame::with_window_drag(
            div().flex_1().min_w_0().h(bar_h),
        ))
        .suffix(
            h_flex()
                .flex_none()
                .h(bar_h)
                .children(zoom_button(group, cx))
                .child(workspace_frame::with_window_drag(
                    div().w(edges.trailing).h(bar_h).flex_none(),
                )),
        )
        .into_any_element()
}

/// 标签上的名字：面板给的短名，没有就用它的标题。
fn panel_label(panel: &Arc<dyn BasePanelView>, window: &mut Window, cx: &mut App) -> AnyElement {
    match PanelHandle::of(panel) {
        Some(handle) => match handle.tab_name(cx) {
            Some(name) => name.into_any_element(),
            None => handle.title(window, cx),
        },
        None => panel.panel_name(cx).into_any_element(),
    }
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

/// 窗口四角上压着东西的那几组：左上角有红黄绿按钮和左栏开关，右上角有右栏开关。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChromeSlots {
    pub(crate) top_left: Option<NodeId>,
    pub(crate) top_right: Option<NodeId>,
    /// 左上角那一组只有舞台。舞台没有标签条，它自己的内容和胶囊要让开左上角。
    pub(crate) stage_top_left: bool,
    /// 右上角那一组只有舞台，要让开右栏开关。
    pub(crate) stage_top_right: bool,
}

impl ChromeSlots {
    /// 从 Dock 此刻的样子算：停靠区展开就是它的角，收起或没有就落到中间；放大时就是放大的那组。
    pub(crate) fn of(area: &DockArea, stage: Option<PanelId>) -> Self {
        let (top_left, top_right) = match area.zoomed_group() {
            Some(node) => (Some(node), Some(node)),
            None => {
                let root = |placement| {
                    let open = placement == DockPlacement::Center || area.is_dock_open(placement);
                    open.then(|| area.layout(placement))
                        .flatten()
                        .map(|tree| tree.root())
                };
                let center = root(DockPlacement::Center);
                (
                    root(DockPlacement::Left)
                        .or(center)
                        .and_then(top_left_group),
                    root(DockPlacement::Right)
                        .or(center)
                        .and_then(top_right_group),
                )
            }
        };
        let only_stage = |node: Option<NodeId>| match (node, stage) {
            (Some(node), Some(stage)) => group_panels(area, node) == Some(&[stage][..]),
            _ => false,
        };
        Self {
            top_left,
            top_right,
            stage_top_left: only_stage(top_left),
            stage_top_right: only_stage(top_right),
        }
    }
}

fn top_left_group(node: &PaneNode) -> Option<NodeId> {
    match node.kind() {
        PaneRef::Tabs { .. } => Some(node.id()),
        PaneRef::Split { children, .. } => children.first().and_then(top_left_group),
    }
}

/// 横排取最后一个，竖排取最上面一个。
fn top_right_group(node: &PaneNode) -> Option<NodeId> {
    match node.kind() {
        PaneRef::Tabs { .. } => Some(node.id()),
        PaneRef::Split { axis, children, .. } => match axis {
            Axis::Horizontal => children.last(),
            Axis::Vertical => children.first(),
        }
        .and_then(top_right_group),
    }
}

fn group_panels(area: &DockArea, node: NodeId) -> Option<&[PanelId]> {
    [
        DockPlacement::Center,
        DockPlacement::Left,
        DockPlacement::Right,
        DockPlacement::Bottom,
    ]
    .into_iter()
    .find_map(|placement| area.layout(placement)?.find_node(node))
    .and_then(|found| match found.kind() {
        PaneRef::Tabs { panels, .. } => Some(panels),
        PaneRef::Split { .. } => None,
    })
}

/// 红黄绿按钮和左栏开关占的宽度。没有左停靠区就没有左栏开关。
pub(crate) fn top_left_guard(window: &Window, has_left_dock: bool) -> Pixels {
    let lights = if workspace_frame::traffic_lights_visible(window) {
        80.
    } else {
        0.
    };
    let toggle = if has_left_dock { 48. } else { 0. };
    px(lights + toggle)
}

impl Workspace {
    /// 窗口四角压着哪几组。只读 Dock，渲染里随时算。
    pub(crate) fn chrome_slots(&self, cx: &App) -> ChromeSlots {
        let stage = self.shell_layout.panel_id(&Component::Stage, cx);
        ChromeSlots::of(self.shell_dock.read(cx), stage)
    }

    fn tab_bar_edges(&self, node: NodeId, window: &Window, cx: &App) -> TabBarEdges {
        let slots = self.chrome_slots(cx);
        let area = self.shell_dock.read(cx);
        TabBarEdges {
            leading: if slots.top_left == Some(node) {
                top_left_guard(window, area.has_dock(DockPlacement::Left))
            } else {
                px(0.)
            },
            // 右栏开关只在布局有右停靠区时画。
            trailing: if slots.top_right == Some(node) && area.has_dock(DockPlacement::Right) {
                px(crate::stage::CHROME_TOGGLE_RESERVE)
            } else {
                px(0.)
            },
        }
    }

    /// 舞台此刻看得见：它是所在组显示着的那块，也没被放大的另一组盖住。
    /// 看不见时，正在看的会话不算被看着（未读照常记）。
    pub(crate) fn stage_visible(&self, cx: &App) -> bool {
        if !self.shell_displayed.contains(&Component::Stage) {
            return false;
        }
        let area = self.shell_dock.read(cx);
        let Some(zoomed) = area.zoomed_group() else {
            return true;
        };
        let stage = self.shell_layout.panel_id(&Component::Stage, cx);
        group_panels(area, zoomed)
            .is_some_and(|panels| stage.is_some_and(|stage| panels.contains(&stage)))
    }
}

/// 内置组件。布局文件里的名字、标签名、画法都在这里。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    Sessions,
    Stage,
    Files,
    Git,
    History,
}

impl Builtin {
    pub(crate) fn of(component: &Component) -> Option<Self> {
        Some(match component {
            Component::Sessions => Self::Sessions,
            Component::Stage => Self::Stage,
            Component::Files => Self::Files,
            Component::Git => Self::Git,
            Component::History => Self::History,
            Component::Plugin(_) | Component::OtherPlugins => return None,
        })
    }

    pub(crate) fn component(self) -> Component {
        match self {
            Self::Sessions => Component::Sessions,
            Self::Stage => Component::Stage,
            Self::Files => Component::Files,
            Self::Git => Component::Git,
            Self::History => Component::History,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Sessions => layout::SESSIONS,
            Self::Stage => layout::STAGE,
            Self::Files => layout::FILES,
            Self::Git => layout::GIT,
            Self::History => layout::HISTORY,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Sessions => "会话",
            Self::Stage => "舞台",
            Self::Files => "文件",
            Self::Git => "变更",
            Self::History => "历史",
        }
    }

    /// 会话列表和舞台自己画顶栏，单独一组时不要标题条。
    fn draws_own_top_bar(self) -> bool {
        matches!(self, Self::Sessions | Self::Stage)
    }

    /// 会话列表和舞台不放大：舞台本来就在中间，会话列表铺满没有意义。
    fn zoomable(self) -> bool {
        !matches!(self, Self::Sessions | Self::Stage)
    }

    fn render(
        self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> AnyElement {
        match self {
            Self::Sessions => workspace.render_sidebar_dock(window, cx),
            Self::Stage => workspace.render_stage_dock(window, cx),
            Self::Files => panel_body(workspace.render_tool_panel_files(window, cx)),
            Self::Git => panel_body(workspace.render_tool_panel_git(window, cx)),
            Self::History => panel_body(workspace.render_history_view(cx)),
        }
    }
}

/// 标签条下面的内容区：竖排铺满，内容自己滚。
fn panel_body(content: AnyElement) -> AnyElement {
    div()
        .size_full()
        .min_h_0()
        .flex()
        .flex_col()
        .child(content)
        .into_any_element()
}

/// 一块内置组件的面板。内容由工作台画，这里只是 Dock 里的壳。
pub(crate) struct ShellPanel {
    workspace: WeakEntity<Workspace>,
    builtin: Builtin,
    focus_handle: FocusHandle,
    /// 皮肤把面板内容缓存起来，只在这块面板自己被通知时重画。
    /// 内容的数据在 Workspace 上，所以跟着它的通知一起失效。
    _workspace_refresh: Subscription,
}

impl ShellPanel {
    pub(crate) fn new(
        workspace: WeakEntity<Workspace>,
        builtin: Builtin,
        cx: &mut Context<Self>,
    ) -> Self {
        let host = workspace.upgrade().expect("工作台外壳面板由工作台创建");
        let _workspace_refresh = cx.observe(&host, |_, _, cx| cx.notify());
        Self {
            workspace,
            builtin,
            focus_handle: cx.focus_handle(),
            _workspace_refresh,
        }
    }
}

impl EventEmitter<PanelEvent> for ShellPanel {}

impl Focusable for ShellPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for ShellPanel {
    fn panel_name(&self) -> &'static str {
        self.builtin.name()
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        self.builtin.zoomable()
    }

    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        crate::workspace_layout::set_displayed(
            &self.workspace,
            self.builtin.component(),
            active,
            cx,
        );
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        crate::workspace_layout::set_displayed(
            &self.workspace,
            self.builtin.component(),
            false,
            cx,
        );
    }
}

impl Panel for ShellPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.builtin.title()
    }

    /// 变更标签上的角标：当前项目里所有仓库的改动文件数。
    fn title_suffix(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.builtin != Builtin::Git {
            return None;
        }
        let count = self.workspace.upgrade().map_or(0, |workspace| {
            crate::tool_panel::git_changes_badge(workspace.read(cx), cx)
        });
        (count > 0).then(|| {
            Tag::primary()
                .small()
                .rounded_full()
                .child(count.to_string())
        })
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        self.builtin.zoomable().then_some(PanelControl::Toolbar)
    }

    fn title_bar(&self, _: &App) -> bool {
        !self.builtin.draws_own_top_bar()
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for ShellPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let builtin = self.builtin;
        self.workspace
            .update(cx, |workspace, cx| builtin.render(workspace, window, cx))
            .unwrap_or_else(|_| div().size_full().into_any_element())
    }
}

impl Workspace {
    pub(crate) fn render_sidebar_dock(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let surface: Hsla = rgb(ui_theme::bg_column()).into();
        let animate =
            self.workspace_window_active && self.shell_displayed.contains(&Component::Sessions);
        let statuses: Vec<AgentStatus> = self
            .sessions
            .iter()
            .map(|session| session.status(cx))
            .collect();
        let focused_key = self.sidebar_focused_key(window);
        let list = self.render_session_list(cx.entity(), cx, &statuses, animate, focused_key);
        workspace_frame::card(surface)
            .pt(workspace_frame::TOP_BAR_HEIGHT)
            // 从访达拖文件夹进侧栏 = 打开项目。只认侧栏，不挂整个窗口：对话输入框要自己收文件。
            .drag_over::<ExternalPaths>(|style, _paths, _window, cx| {
                style.bg(cx.theme().drop_target)
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                this.open_paths(paths.paths(), cx);
            }))
            .child(list)
            .child(workspace_frame::with_window_drag(
                div()
                    .absolute()
                    .top_0()
                    .left(px(80.))
                    .right_0()
                    .h(workspace_frame::TOP_BAR_HEIGHT),
            ))
            .into_any_element()
    }

    pub(crate) fn render_stage_dock(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stage_surface: Hsla = rgb(ui_theme::bg_stage()).into();
        let stage_content: AnyElement = match self.active_tab() {
            WorkspaceRoute::Agents => self.render_agents_page(window, cx),
            WorkspaceRoute::Automations => self.render_automations_page(window, cx),
            WorkspaceRoute::Capabilities => self.render_capabilities_page(window, cx),
            WorkspaceRoute::Desktop => self.render_desktop_page(window, cx),
            WorkspaceRoute::Session => {
                let background_image = cx.global::<Appearance>().bg_image.clone();
                let content =
                    if self
                        .sessions
                        .get(self.active_session)
                        .is_some_and(|session| !session.is_product_conversation(cx))
                    {
                        div().flex_1().min_w_0().min_h_0().flex().flex_col().child(
                            div().flex_1().min_w_0().min_h_0().flex().child(
                                match &self.sessions[self.active_session].kind {
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
                                        let bg_image_opacity =
                                            cx.global::<Appearance>().bg_image_opacity;
                                        div()
                                            .relative()
                                            .flex_1()
                                            .min_w_0()
                                            .min_h_0()
                                            .flex()
                                            .bg(rgb(ui_theme::bg_stage()))
                                            .children(background_image.as_deref().map(|path| {
                                                workspace_frame::background_image_layer(
                                                    path,
                                                    bg_image_opacity,
                                                    cx,
                                                )
                                            }))
                                            .child(view.clone().cached(
                                                gpui::StyleRefinement::default().size_full(),
                                            ))
                                            .into_any_element()
                                    }
                                },
                            ),
                        )
                    } else if self.spaces.is_empty() {
                        stage_empty_state(
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
                    } else {
                        // 舞台上没有会话时的通用空态，不分为什么空、也不催下一步：
                        // 开对话、开项目的入口都在侧栏。
                        stage_empty_state(
                            gpui_kit_assets::IconName::Coffee,
                            "休息一下",
                            "没有打开的会话",
                            None,
                        )
                    };
                content.into_any_element()
            }
        };
        let stage = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(stage_content);
        // 舞台没有标签条。标题胶囊和拖窗口的顶带浮在舞台内容上沿，只盖舞台自己。
        let chrome = self.render_stage_chrome(window, cx);
        div()
            .size_full()
            .child(
                workspace_frame::card(stage_surface)
                    .child(stage)
                    .child(chrome),
            )
            .into_any_element()
    }
}

/// 舞台空态。用组件库的 `Empty`，各种空态长一个样；只有非做不可的下一步才给按钮。
fn stage_empty_state(
    icon: impl Into<Icon>,
    title: &'static str,
    description: &'static str,
    action: Option<Button>,
) -> gpui::Div {
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
    use super::Builtin;
    use smelt_client::layout::{self, Component};

    /// 布局格式认的每个内置组件，外壳都有面板画它，名字和布局文件里的一致。
    #[test]
    fn every_builtin_component_has_a_panel() {
        for component in [
            Component::Sessions,
            Component::Stage,
            Component::Files,
            Component::Git,
            Component::History,
        ] {
            let builtin = Builtin::of(&component).expect("内置组件都有面板");
            assert_eq!(builtin.component(), component);
            assert_eq!(builtin.name(), component.name());
        }
        assert_eq!(Builtin::of(&Component::OtherPlugins), None);
    }

    /// 只有工具类面板能放大，会话列表和舞台不能；自己画顶栏的不要标题条。
    #[test]
    fn zoom_and_title_bar_follow_the_component() {
        for (builtin, zoomable) in [
            (Builtin::Sessions, false),
            (Builtin::Stage, false),
            (Builtin::Files, true),
            (Builtin::Git, true),
            (Builtin::History, true),
        ] {
            assert_eq!(builtin.zoomable(), zoomable, "{}", builtin.name());
            assert_eq!(builtin.draws_own_top_bar(), !zoomable, "{}", builtin.name());
        }
    }

    /// 仓库里自带的示例布局都要合法。
    #[test]
    fn repo_layouts_are_valid() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../layouts");
        let entries = layout::discover(&[dir]);
        assert!(!entries.is_empty());
        for entry in entries {
            if let Err(error) = entry.layout {
                panic!("{}：{error}", entry.path.display());
            }
        }
    }
}
