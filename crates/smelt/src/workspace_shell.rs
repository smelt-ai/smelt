//! 工作台外壳：整个工作台是一个 `DockArea`，这里是 smelt 给它的皮肤和内置面板。
//!
//! 摆什么、怎么切、选中哪个都在 Dock 上（布局文件和存档就是它的 `DockAreaState`，见
//! `layout_files`、`workspace_layout`）。这里只管：
//! - 皮肤（[`SmeltDockRenderer`] / [`SmeltTabGroup`]）：组件库 `DockSkin` 外面包一层，
//!   标签条由 smelt 画——角标、拖窗口的空白、给红黄绿按钮让位、放大按钮、拖动重排；
//!   分隔条、拖宽、放下提示、放大本身交给皮肤和 Dock。
//! - 内置面板（[`ShellPanel`]）：会话列表、舞台、文件、变更、历史，内容由工作台画；
//! - 占位（[`PlaceholderPanel`]）：没注册的面板名、重复摆的内置面板，存档时原样写回。
//!
//! 终端分屏是舞台里另一个 Dock（`term_dock`），和这里无关。

use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, Axis, Div, Entity, EventEmitter, ExternalPaths,
    FocusHandle, Focusable, Global, Hsla, Pixels, SharedString, Stateful, Subscription, WeakEntity,
    Window, div, prelude::*, px, rgb, size,
};
use gpui_base::ResizeHandleContext;
use gpui_component::dock::{
    BasePanel, BasePanelView, DockArea, DockAreaRenderer, DockAreaState, DockContext,
    DockPlacement, DockSkin, DragPanel, DropIndicator, NodeId, PaneNode, PaneRef, Panel,
    PanelBuildContext, PanelControl, PanelEvent, PanelHandle, PanelId, PanelInfo, PanelState,
    TabGroupContext, TabGroupRenderer, panel_handle, register_panel,
};
use gpui_component::empty::{
    Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle,
};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Icon, IconName, Sizable as _, h_flex};
use serde_json::Value;
pub(crate) use smelt_ui::Button;
use smelt_ui::IconAction as _;

use crate::layout_files::{self, panel};
use crate::settings::Appearance;
use crate::workspace_stages::StageShows;
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

// ---- 内置面板 ----

/// 内置面板。注册名、标签名、画法都在这里。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Builtin {
    Sessions,
    Stage,
    Files,
    Git,
    History,
}

impl Builtin {
    pub(crate) const ALL: [Self; 5] = [
        Self::Sessions,
        Self::Stage,
        Self::Files,
        Self::Git,
        Self::History,
    ];

    /// 布局文件里的面板名。
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Sessions => panel::SESSIONS,
            Self::Stage => panel::STAGE,
            Self::Files => panel::FILES,
            Self::Git => panel::GIT,
            Self::History => panel::HISTORY,
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|builtin| builtin.name() == name)
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Sessions => "会话",
            Self::Stage => "舞台",
            Self::Files => "文件",
            Self::Git => "变更",
            Self::History => "历史",
        }
    }

    /// 会话列表和舞台自己管顶上：单独一组时不要标题带。
    pub(crate) fn draws_own_top_bar(self) -> bool {
        matches!(self, Self::Sessions | Self::Stage)
    }

    /// 会话列表和舞台不放大：舞台本来就是主角，会话列表铺满没有意义。
    pub(crate) fn zoomable(self) -> bool {
        !matches!(self, Self::Sessions | Self::Stage)
    }

    fn render(
        self,
        stage: &str,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> AnyElement {
        match self {
            Self::Sessions => workspace.render_sidebar_dock(window, cx),
            Self::Stage => workspace.render_stage_dock(stage, window, cx),
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

/// 一块内置面板。内容和它的状态（滚动位置、终端、打开的文件）都在工作台上，这里只是
/// Dock 里的壳，所以重建它不丢东西。
pub(crate) struct ShellPanel {
    workspace: WeakEntity<Workspace>,
    builtin: Builtin,
    /// 布局里它的 `info.panel`，原样写回存档。舞台的 `accepts` 在这里。
    info: Value,
    focus_handle: FocusHandle,
    /// Dock 把面板内容缓存起来，只在这块面板自己被通知时重画。内容的数据在工作台上，
    /// 所以跟着它的通知一起失效。
    _workspace_refresh: Subscription,
}

impl ShellPanel {
    fn new(
        workspace: WeakEntity<Workspace>,
        builtin: Builtin,
        info: Value,
        cx: &mut Context<Self>,
    ) -> Self {
        // 布局在工作台建好以后才装（`Workspace::install_initial_layout` 推迟到建完），
        // 面板不会比工作台早出生。
        let host = workspace.upgrade().expect("工作台外壳面板由工作台创建");
        let _workspace_refresh = cx.observe(&host, |_, _, cx| cx.notify());
        Self {
            workspace,
            builtin,
            info,
            focus_handle: cx.focus_handle(),
            _workspace_refresh,
        }
    }

    pub(crate) fn builtin(&self) -> Builtin {
        self.builtin
    }

    pub(crate) fn info(&self) -> &Value {
        &self.info
    }
}

/// 舞台的 id：运行时给的，跟着面板实体，拖动重排不变；重新装布局、重启会变。
pub(crate) fn stage_id(panel: &Entity<ShellPanel>) -> String {
    format!("stage-{}", panel.entity_id().as_u64())
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

    /// 内置面板不让关：没有入口能再把它加回原处，拖坏了用 `layout.reset`。
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        self.builtin.zoomable()
    }

    fn dump(&self, _: &App) -> PanelState {
        PanelState {
            panel_name: self.builtin.name().to_string(),
            children: Vec::new(),
            info: PanelInfo::panel(self.info.clone()),
        }
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
        let _timing = smelt_ui::frame_stats::RenderTiming::start(builtin.name());
        let stage = match builtin {
            Builtin::Stage => stage_id(&cx.entity()),
            _ => String::new(),
        };
        self.workspace
            .update(cx, |workspace, cx| {
                builtin.render(&stage, workspace, window, cx)
            })
            .unwrap_or_else(|_| div().size_full().into_any_element())
    }
}

/// 一块建不出来的面板：面板名没注册（插件没装、删了、没注册），或者内置面板摆了第二份。
/// 只画一行原因，存档时把原来的样子原样写回。
pub(crate) struct PlaceholderPanel {
    state: PanelState,
    reason: SharedString,
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .bg(rgb(ui_theme::bg_stage()))
            .child(crate::placeholder_view(
                &self.reason,
                cx.theme().muted_foreground,
            ))
    }
}

/// 一块占位面板。`state` 是布局里原来写的，存档时原样写回。
pub(crate) fn placeholder(
    state: PanelState,
    reason: impl Into<SharedString>,
    cx: &mut App,
) -> Arc<dyn BasePanelView> {
    let reason = reason.into();
    Arc::new(cx.new(|cx| PlaceholderPanel {
        state,
        reason,
        focus_handle: cx.focus_handle(),
    }))
}

/// 插件面板的注册名 `shell:<插件 id>/<面板>` 拆开。插件 id 里没有 `/`。
pub(crate) fn plugin_panel(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix("shell:")?.split_once('/')
}

/// 布局里一个面板名建不出来的原因，给占位显示、写日志。
pub(crate) fn unbuilt_reason(name: &str) -> String {
    let Some((plugin, panel)) = plugin_panel(name) else {
        return format!("不认识的面板 `{name}`");
    };
    let reason = match crate::plugins::panel_target(plugin, panel) {
        Err(reason) => reason,
        Ok(_) => format!(
            "插件 `{plugin}` 声明了面板 `{panel}`，但代码里没有注册：在根视图的 `init()` 里调 \
             `DockArea.register_panel(\"{panel}\", 类)`。"
        ),
    };
    eprintln!("[plugins] {plugin}/{panel}：{reason}");
    reason
}

/// 一次 `load` 期间交给内置面板构建器的东西。构建时 Dock 和工作台都正被借着，读不了，
/// 所以由 [`load`] 先备好放在这里，装完就拿走。
struct LoadPass {
    workspace: WeakEntity<Workspace>,
    /// 装之前 Dock 里的内置面板，按树的先后。新布局里同名的依次接着用：舞台 id 跟着实体，
    /// 插件重新加载（dump 再 load）后焦点和各块显示的不乱。
    reuse: HashMap<Builtin, VecDeque<Entity<ShellPanel>>>,
    /// 这次已经摆出来的内置面板（舞台除外）。同一个实体画在两处，焦点、滚动、任务都会乱，
    /// 第二份变占位。
    placed: HashSet<Builtin>,
}

impl Global for LoadPass {}

/// 把内置面板注册进 Dock 的注册表。注册表是全局的，重复调只是覆盖成一样的构建器。
pub(crate) fn register_builtins(cx: &mut App) {
    for builtin in Builtin::ALL {
        register_panel(cx, builtin.name(), move |context, _window, cx| {
            build_builtin(builtin, &context, cx)
        });
    }
}

fn build_builtin(
    builtin: Builtin,
    context: &PanelBuildContext,
    cx: &mut App,
) -> Arc<dyn BasePanelView> {
    let state = context.state().clone();
    let info = match context.info() {
        PanelInfo::Panel(info) => info.clone(),
        PanelInfo::Stack { .. } | PanelInfo::Tabs { .. } => Value::Null,
    };
    if !cx.has_global::<LoadPass>() {
        // 只有工作台装布局时才建内置面板；`layout.add` 直接建，不经注册表。
        return placeholder(state, "内置面板只能由工作台装", cx);
    }
    let pass = cx.global_mut::<LoadPass>();
    if builtin != Builtin::Stage && !pass.placed.insert(builtin) {
        return placeholder(state, format!("{}已经摆过一份", builtin.title()), cx);
    }
    let workspace = pass.workspace.clone();
    let reused = pass.reuse.get_mut(&builtin).and_then(VecDeque::pop_front);
    let entity = match reused {
        Some(entity) => {
            entity.update(cx, |panel, _| panel.info = info);
            entity
        }
        None => cx.new(|cx| ShellPanel::new(workspace, builtin, info, cx)),
    };
    panel_handle(entity)
}

/// 新建一块内置面板，`layout.add` 用。
pub(crate) fn new_builtin(
    workspace: WeakEntity<Workspace>,
    builtin: Builtin,
    cx: &mut App,
) -> Arc<dyn BasePanelView> {
    panel_handle(cx.new(|cx| ShellPanel::new(workspace, builtin, Value::Null, cx)))
}

/// 按 `state` 装布局。内置面板按名字接着用装之前的实体，见 [`LoadPass`]。
pub(crate) fn load(
    area: &Entity<DockArea>,
    workspace: WeakEntity<Workspace>,
    state: DockAreaState,
    window: &mut Window,
    cx: &mut App,
) -> anyhow::Result<()> {
    let mut reuse: HashMap<Builtin, VecDeque<Entity<ShellPanel>>> = HashMap::new();
    for (_, panel) in shell_panels(area.read(cx)) {
        let builtin = panel.read(cx).builtin;
        reuse.entry(builtin).or_default().push_back(panel);
    }
    cx.set_global(LoadPass {
        workspace,
        reuse,
        placed: HashSet::new(),
    });
    let loaded = area.update(cx, |area, cx| area.load(state, window, cx));
    cx.remove_global::<LoadPass>();
    loaded
}

// ---- 读 Dock ----

/// Dock 里的内置面板，按树的先后（中间、左、右、下）。只读内存。
pub(crate) fn shell_panels(area: &DockArea) -> Vec<(PanelId, Entity<ShellPanel>)> {
    PLACEMENTS
        .into_iter()
        .filter_map(|placement| area.layout(placement))
        .flat_map(|tree| tree.panels())
        .filter_map(|id| Some((id, shell_panel(area, id)?)))
        .collect()
}

fn shell_panel(area: &DockArea, id: PanelId) -> Option<Entity<ShellPanel>> {
    area.panel(id)?.view().downcast::<ShellPanel>().ok()
}

/// 此刻看得见的面板：所在组显示着的那个，没被另一组的放大挡住，也不在收起的 dock 里。
pub(crate) fn shown_panels(area: &DockArea) -> HashSet<PanelId> {
    let zoomed = area.zoomed_group();
    let mut shown = HashSet::new();
    for placement in PLACEMENTS {
        if placement != DockPlacement::Center && !area.is_dock_open(placement) {
            continue;
        }
        let Some(tree) = area.layout(placement) else {
            continue;
        };
        tree.root().walk(&mut |node| {
            if let PaneRef::Tabs { panels, active_ix } = node.kind()
                && zoomed.is_none_or(|zoomed| zoomed == node.id())
                && let Some(panel) = panels.get(active_ix)
            {
                shown.insert(*panel);
            }
        });
    }
    shown
}

/// 放着这块面板的组。
pub(crate) fn group_of(area: &DockArea, panel: PanelId) -> Option<NodeId> {
    PLACEMENTS
        .into_iter()
        .find_map(|placement| area.layout(placement)?.find_panel_node(panel))
}

/// 这块面板在哪块区域（中间、左、右、下）。
pub(crate) fn placement_of(area: &DockArea, panel: PanelId) -> Option<DockPlacement> {
    PLACEMENTS.into_iter().find(|&placement| {
        area.layout(placement)
            .is_some_and(|tree| tree.find_panel_node(panel).is_some())
    })
}

/// 布局里按面板名找面板，按树的先后取第一块。占位面板按它原来的名字算。
pub(crate) fn find_panel(area: &DockArea, name: &str, cx: &App) -> Option<PanelId> {
    PLACEMENTS
        .into_iter()
        .filter_map(|placement| area.layout(placement))
        .flat_map(|tree| tree.panels())
        .find(|&id| {
            area.panel(id)
                .is_some_and(|panel| layout_name(panel, cx) == name)
        })
}

/// 面板在布局文件里的名字（`panel_name`）。占位面板是它原来的名字，不是占位的。
pub(crate) fn layout_name(panel: &Arc<dyn BasePanelView>, cx: &App) -> String {
    match panel.view().downcast::<PlaceholderPanel>() {
        Ok(placeholder) => placeholder.read(cx).state.panel_name.clone(),
        Err(_) => panel.panel_name(cx).to_string(),
    }
}

/// 这一组里的面板。
pub(crate) fn group_panels(area: &DockArea, node: NodeId) -> Option<&[PanelId]> {
    PLACEMENTS
        .into_iter()
        .find_map(|placement| area.layout(placement)?.find_node(node))
        .and_then(|found| match found.kind() {
            PaneRef::Tabs { panels, .. } => Some(panels),
            PaneRef::Split { .. } => None,
        })
}

/// 贴着窗口上沿两个角的组。放大时都是放大的那组；不然从中间的树找（左 / 右 dock 开着时
/// 从它找）：横排取第一个 / 最后一个子节点，竖排取最上面一个。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChromeSlots {
    pub(crate) top_left: Option<NodeId>,
    pub(crate) top_right: Option<NodeId>,
}

impl ChromeSlots {
    pub(crate) fn of(area: &DockArea) -> Self {
        if let Some(node) = area.zoomed_group() {
            return Self {
                top_left: Some(node),
                top_right: Some(node),
            };
        }
        let root = |placement| {
            let open = placement == DockPlacement::Center || area.is_dock_open(placement);
            open.then(|| area.layout(placement))
                .flatten()
                .map(|tree| tree.root())
        };
        let center = root(DockPlacement::Center);
        Self {
            top_left: root(DockPlacement::Left)
                .or(center)
                .and_then(top_left_group),
            top_right: root(DockPlacement::Right)
                .or(center)
                .and_then(top_right_group),
        }
    }
}

fn top_left_group(node: &PaneNode) -> Option<NodeId> {
    match node.kind() {
        PaneRef::Tabs { .. } => Some(node.id()),
        PaneRef::Split { children, .. } => children.first().and_then(top_left_group),
    }
}

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

/// 红黄绿按钮占的宽度：全屏、Windows 下是 0。
pub(crate) fn lights_width(window: &Window) -> Pixels {
    if workspace_frame::traffic_lights_visible(window) {
        px(TRAFFIC_LIGHTS_WIDTH)
    } else {
        px(0.)
    }
}

impl Workspace {
    /// 窗口两个角压着哪几组。只读 Dock，渲染里随时算。
    pub(crate) fn chrome_slots(&self, cx: &App) -> ChromeSlots {
        ChromeSlots::of(self.shell_dock.read(cx))
    }

    /// 标签条、标题带开头要让出的宽度：贴左上角的那组让红黄绿按钮。
    fn tab_bar_leading(&self, node: NodeId, window: &Window, cx: &App) -> Pixels {
        if self.chrome_slots(cx).top_left == Some(node) {
            lights_width(window)
        } else {
            px(0.)
        }
    }

    /// 这块舞台单独一组、贴着左上角：它没有标签条，自己的内容和胶囊要让开红黄绿按钮。
    pub(crate) fn stage_at_top_left(&self, stage: &str, cx: &App) -> bool {
        let Some(panel) = self.stages.panel_of(stage) else {
            return false;
        };
        let area = self.shell_dock.read(cx);
        ChromeSlots::of(area)
            .top_left
            .and_then(|node| group_panels(area, node))
            == Some(&[panel][..])
    }
}

// ---- 皮肤 ----

/// 建好整个工作台的 Dock，套上 smelt 的皮肤。里面还是空的，布局由 `workspace_layout` 装。
pub(crate) fn new_area(
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<DockArea> {
    register_builtins(cx);
    let mut skin = None;
    let area = cx.new(|cx| {
        let inner = DockSkin::new(cx);
        skin = Some(inner.clone());
        DockArea::new("smelt-shell", Some(layout_files::VERSION), window, cx).with_renderer(
            Rc::new(SmeltDockRenderer {
                skin: inner,
                workspace,
            }),
        )
    });
    // 上面的闭包在 `cx.new` 返回前就跑完了。
    let skin = skin.expect("DockSkin::new 在构造 Dock 时已经跑过");
    // 不做收起，面板也不让关：皮肤自带的这两个按钮都不要。
    skin.set_toggle_button_visible(false, cx);
    skin.set_close_button_visible(false, cx);
    area
}

/// 外层 Dock 的画法：`DockSkin` 的全部，加上 smelt 的标签条和占位。
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

    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        DockAreaRenderer::render_dock(&*self.skin, dock, content, window, cx)
    }

    /// 没注册的面板名：画一行原因，存档时原样写回。插件装回来就好了。
    fn build_placeholder(
        &self,
        state: &PanelState,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<dyn BasePanelView>> {
        let reason = unbuilt_reason(&state.panel_name);
        Some(placeholder(state.clone(), reason, cx))
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

    /// 组里两个以上面板画一排标签；只有一个时，内置的工具面板画一条居中的标题带，
    /// 会话列表、舞台、插件面板、占位什么都不画。
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
        let leading = self.workspace.upgrade().map_or(px(0.), |workspace| {
            workspace.read(cx).tab_bar_leading(group.node(), window, cx)
        });
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

    /// 内置面板和占位自己铺底。插件面板铺舞台的底；脚本出错没建出来的，说一句原因。
    fn render_active_panel(
        &self,
        panel: AnyView,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let kind = panel.entity_type();
        if kind == std::any::TypeId::of::<ShellPanel>()
            || kind == std::any::TypeId::of::<PlaceholderPanel>()
        {
            return self.inner.render_active_panel(panel, group, window, cx);
        }
        let surface = div().size_full().flex().bg(rgb(ui_theme::bg_stage()));
        if kind != std::any::TypeId::of::<gpui_shell::dock::ScriptPanel>() {
            let name = group
                .active_panel()
                .map(|panel| panel.panel_name(cx))
                .unwrap_or_default();
            let name = plugin_panel(name).map_or(name, |(_, panel)| panel);
            return surface
                .child(crate::placeholder_view(
                    &format!("面板 `{name}` 的脚本出错了，没有建出来，原因见日志。"),
                    cx.theme().muted_foreground,
                ))
                .into_any_element();
        }
        surface
            .child(self.inner.render_active_panel(panel, group, window, cx))
            .into_any_element()
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
                    tab.on_drag(drag, move |drag, offset, window, cx| {
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

/// 标签上的名字：面板给的短名，没有就用它的标题；插件面板用它在 `smelt.json` 里声明的标题。
fn panel_label(panel: &Arc<dyn BasePanelView>, window: &mut Window, cx: &mut App) -> AnyElement {
    match PanelHandle::of(panel) {
        Some(handle) => match handle.tab_name(cx) {
            Some(name) => name.into_any_element(),
            None => handle.title(window, cx),
        },
        None => panel_text(panel, cx).into_any_element(),
    }
}

/// 面板名字的纯文本：拖动预览、给插件的 `layout()` 用。只读内存。
pub(crate) fn panel_text(panel: &Arc<dyn BasePanelView>, cx: &App) -> SharedString {
    if let Some(handle) = PanelHandle::of(panel)
        && let Some(name) = handle.tab_name(cx)
    {
        return name;
    }
    if let Some(builtin) = panel
        .view()
        .downcast::<ShellPanel>()
        .ok()
        .map(|entity| entity.read(cx).builtin)
    {
        return builtin.title().into();
    }
    let name = layout_name(panel, cx);
    match plugin_panel(&name) {
        Some((plugin, panel)) => crate::plugins::panel_target(plugin, panel)
            .map(|target| target.title)
            .unwrap_or_else(|_| panel.to_string())
            .into(),
        None => name.into(),
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

// ---- 会话列表和舞台的画法 ----

impl Workspace {
    pub(crate) fn render_sidebar_dock(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let animate =
            self.workspace_window_active && self.shell_displayed.contains(&Builtin::Sessions);
        let statuses: Vec<AgentStatus> = self
            .sessions
            .iter()
            .map(|session| session.status(cx))
            .collect();
        let focused_key = self.sidebar_focused_key(window);
        let list = self.render_session_list(cx.entity(), cx, &statuses, animate, focused_key);
        workspace_frame::card(rgb(ui_theme::bg_column()).into())
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
                    .left(px(TRAFFIC_LIGHTS_WIDTH))
                    .right_0()
                    .h(workspace_frame::TOP_BAR_HEIGHT),
            ))
            .into_any_element()
    }

    /// 一块舞台。有焦点的那块画当前会话或页面，别的画各自停着的（见 `workspace_stages`）。
    pub(crate) fn render_stage_dock(
        &mut self,
        stage: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shows = self.stage_shows(stage, cx);
        let stage_content: AnyElement = match &shows {
            StageShows::Page(WorkspaceRoute::Agents) => self.render_agents_page(window, cx),
            StageShows::Page(WorkspaceRoute::Automations) => {
                self.render_automations_page(window, cx)
            }
            StageShows::Page(WorkspaceRoute::Capabilities) => {
                self.render_capabilities_page(window, cx)
            }
            StageShows::Page(WorkspaceRoute::Desktop) => self.render_desktop_page(window, cx),
            StageShows::Session(ix) => div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    div().flex_1().min_w_0().min_h_0().flex().child(
                        match &self.sessions[*ix].kind {
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
                                self.render_conversation_surface(view.clone(), cx)
                            }
                        },
                    ),
                )
                .into_any_element(),
            // `Session` 不会是停着的页面，`stage_shows` 只给会话下标。
            StageShows::Page(WorkspaceRoute::Session) | StageShows::Empty => {
                let empty = if self.spaces.is_empty() {
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
                } else if stage == self.focused_stage()
                    && self.current_conversation.is_some()
                    && self.cur().is_none()
                    && !self.sessions_restore_settled()
                {
                    // 冷启动时上次的会话还没恢复出来：它到了就显示在这儿，别说「没有会话」。
                    stage_empty_state(
                        gpui_kit_assets::IconName::LoaderCircle,
                        "正在恢复",
                        "上次的会话马上回来",
                        None,
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
                empty.into_any_element()
            }
        };
        let content = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(stage_content);
        // 舞台没有标签条。标题胶囊和拖窗口的顶带浮在舞台内容上沿，只盖舞台自己。
        let chrome = self.render_stage_chrome(stage, &shows, window, cx);
        // 在没有焦点的舞台里按下，焦点先换过去，按下本身照常落到里面的终端或对话上。
        let focus_here = stage.to_string();
        div()
            .size_full()
            .capture_any_mouse_down(cx.listener(move |this, _, window, cx| {
                this.focus_stage(&focus_here, window, cx);
            }))
            .child(
                workspace_frame::card(rgb(ui_theme::bg_stage()).into())
                    .child(content)
                    .child(chrome),
            )
            .into_any_element()
    }

    /// 舞台上的一段对话：会话、智能体、自动化运行都用这一份。底色归舞台卡片铺
    /// （[`Workspace::render_stage_dock`]），这里只铺设置里的背景图。
    pub(crate) fn render_conversation_surface(
        &self,
        view: Entity<crate::acp_view::AcpView>,
        cx: &App,
    ) -> AnyElement {
        let appearance = cx.global::<Appearance>();
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .children(appearance.bg_image.as_deref().map(|path| {
                workspace_frame::background_image_layer(path, appearance.bg_image_opacity, cx)
            }))
            .child(view.cached(gpui::StyleRefinement::default().size_full()))
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
    use super::{Builtin, plugin_panel};

    /// 每个内置面板的注册名都认得回来，和 `layout_files::panel` 里的一致。
    #[test]
    fn every_builtin_round_trips_its_name() {
        for builtin in Builtin::ALL {
            assert_eq!(Builtin::from_name(builtin.name()), Some(builtin));
            assert!(builtin.name().starts_with("smelt."));
        }
        assert_eq!(Builtin::from_name("shell:demo/topbar"), None);
    }

    /// 只有工具类面板能放大，会话列表和舞台不能；自己管顶上的不要标题带。
    #[test]
    fn zoom_and_title_band_follow_the_panel() {
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

    #[test]
    fn plugin_panel_names_split_at_the_first_slash() {
        assert_eq!(plugin_panel("shell:demo/topbar"), Some(("demo", "topbar")));
        assert_eq!(plugin_panel("shell:demo/a/b"), Some(("demo", "a/b")));
        assert_eq!(plugin_panel("smelt.stage"), None);
        assert_eq!(plugin_panel("shell:nopanel"), None);
    }
}
