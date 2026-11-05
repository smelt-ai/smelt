//! 工作台外壳用 `gpui-component` 的 Dock。
//!
//! 外层 Dock 里摆什么由布局决定（`workspace_layout`）：默认是左栏、舞台、工具面板三块。
//! 会话列表、工具面板所在停靠区的开合和宽度仍以原来的字段为准（`sidebar_open` /
//! `sidebar_w`，以及当前会话的 `tool_panel_open` / `tool_panel_w`），渲染前写进 Dock，
//! 用户拖动分隔条后再写回这些字段。Dock 锁住不让拖动和放下，排布只改布局文件。
//! 终端分屏是舞台里另一个 Dock（`term_dock`），锁住外层它的标签才不会被放进侧栏。

use gpui::{
    AnyElement, App, Entity, EventEmitter, ExternalPaths, FocusHandle, Focusable, Hsla, Pixels,
    Subscription, WeakEntity, Window, div, prelude::*, px, rgb,
};
use gpui_component::dock::{BasePanel, DockArea, DockPlacement, DockSkin, Panel, PanelEvent};

use gpui_component::{ActiveTheme, Icon};
use gpui_component::empty::{
    Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle,
};
pub(crate) use smelt_ui::Button;
use smelt_client::layout;

use crate::settings::Appearance;
use crate::workspace_layout::ShellLayout;
use crate::{
    AgentStatus, MIN_SIDEBAR_WIDTH, MIN_TOOL_PANEL_WIDTH, MIN_WORKSPACE_CONTENT_WIDTH, SessionKind,
    Workspace, WorkspaceRoute, terminal, ui_theme, workspace_frame,
};

const SHELL_VERSION: usize = 1;

/// 工作台三大块：布局文件里的名字 → 标题和画法。工具面板里的页签单独摆出来时不在这里，
/// 走 [`ShellContent::Tool`]，标题和画法沿用工具面板自己的登记表。
pub(crate) struct Builtin {
    name: &'static str,
    title: &'static str,
    render: fn(&mut Workspace, &mut Window, &mut Context<Workspace>) -> AnyElement,
}

static BUILTINS: [Builtin; 3] = [
    Builtin {
        name: layout::SESSIONS,
        title: "会话",
        render: Workspace::render_sidebar_dock,
    },
    Builtin {
        name: layout::STAGE,
        title: "舞台",
        render: Workspace::render_stage_dock,
    },
    Builtin {
        name: layout::TOOLS,
        title: "工具",
        render: Workspace::render_tool_dock,
    },
];

pub(crate) fn builtin(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|builtin| builtin.name == name)
}

pub(crate) struct InstalledShell {
    pub(crate) area: Entity<DockArea>,
    pub(crate) layout: ShellLayout,
    pub(crate) applied_sidebar: f32,
    pub(crate) applied_tool: f32,
}

/// 建好 Dock，装上默认布局并锁住。开合和宽度每次渲染前由 `align_shell_dock`
/// 从工作区状态写进来。
pub(crate) fn install(
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) -> InstalledShell {
    let (area, skin) = DockSkin::dock_area("smelt-shell", Some(SHELL_VERSION), window, cx);
    skin.set_toggle_button_visible(false, cx);
    skin.set_close_button_visible(false, cx);
    let layout = crate::workspace_layout::install_default(workspace, &area, window, cx);

    let applied_sidebar = area
        .read(cx)
        .dock_size(DockPlacement::Left)
        .map(|size| f32::from(size))
        .unwrap_or(crate::DEFAULT_SIDEBAR_WIDTH);
    let applied_tool = area
        .read(cx)
        .dock_size(DockPlacement::Right)
        .map(|size| f32::from(size))
        .unwrap_or(crate::DEFAULT_TOOL_PANEL_WIDTH);
    InstalledShell {
        area,
        layout,
        applied_sidebar,
        applied_tool,
    }
}

/// 内置面板里画什么。
pub(crate) enum ShellContent {
    /// 会话列表、舞台、工具面板。
    Region(&'static Builtin),
    /// 工具面板的一个页签，被布局单独摆了出来。
    Tool(crate::tool_panel::ToolPanelTab),
}

/// 一块内置组件的面板。内容由工作台画，这里只是 Dock 里的壳。
pub(crate) struct ShellPanel {
    workspace: WeakEntity<Workspace>,
    component: layout::Component,
    content: ShellContent,
    focus_handle: FocusHandle,
    /// Dock 皮肤把面板内容缓存起来，只在这块面板自己被通知时重画。
    /// 会话列表和舞台的数据在 Workspace 上，所以跟着它的通知一起失效。
    _workspace_refresh: Subscription,
}

impl ShellPanel {
    pub(crate) fn new(
        workspace: WeakEntity<Workspace>,
        component: layout::Component,
        content: ShellContent,
        cx: &mut Context<Self>,
    ) -> Self {
        let host = workspace.upgrade().expect("工作台外壳面板由工作台创建");
        let _workspace_refresh = cx.observe(&host, |_, _, cx| cx.notify());
        Self {
            workspace,
            component,
            content,
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
        match &self.content {
            ShellContent::Region(builtin) => builtin.name,
            ShellContent::Tool(_) => "smelt.tool",
        }
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }

    fn visible(&self, cx: &App) -> bool {
        if self.component != layout::Component::Tools {
            return true;
        }
        self.workspace.upgrade().is_none_or(|workspace| {
            let workspace = workspace.read(cx);
            workspace.tool_panel_open && !workspace.tool_panel_promoted()
        })
    }

    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        crate::workspace_layout::set_displayed(&self.workspace, self.component.clone(), active, cx);
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        crate::workspace_layout::set_displayed(&self.workspace, self.component.clone(), false, cx);
    }
}

impl Panel for ShellPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        match &self.content {
            ShellContent::Region(builtin) => builtin.title.to_string(),
            ShellContent::Tool(tab) => tab.label(),
        }
    }

    /// 这几块自己有顶栏和页签。Dock 的标题条只在同一组里有第二块面板时出现，
    /// 那时候用它拖动。
    fn title_bar(&self, _: &App) -> bool {
        false
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for ShellPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.content {
            ShellContent::Region(builtin) => Ok(builtin.render),
            ShellContent::Tool(tab) => Err(*tab),
        };
        self.workspace
            .update(cx, |workspace, cx| match content {
                Ok(render) => render(workspace, window, cx),
                Err(tab) => workspace.render_placed_tool(tab, window, cx),
            })
            .unwrap_or_else(|_| div().size_full().into_any_element())
    }
}

/// 分隔条拖动和窗口变宽时，Dock 上的像素与存档宽度怎么对齐。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShellSizeStep {
    pub(crate) preference: f32,
    pub(crate) applied: f32,
    pub(crate) dock: f32,
    pub(crate) display: f32,
    pub(crate) min: f32,
    /// 当前窗口下这条边最多能到多宽。比 `min` 还小的时候按 `min` 算。
    pub(crate) max: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ShellSizeAction {
    Keep,
    /// 用户拖过。`push` 有值时 Dock 比下限还窄，要把它顶回去。
    Adopt {
        preference: f32,
        push: Option<f32>,
    },
    /// 窗口变了，把 Dock 推到当前该显示的宽度，不改存档。
    Push(f32),
}

pub(crate) fn shell_size_action(step: ShellSizeStep) -> ShellSizeAction {
    let cap = step.max.max(step.min);
    if (step.dock - step.applied).abs() > 0.5 {
        let preference = step.dock.clamp(step.min, cap);
        let push = ((preference - step.dock).abs() > 0.5).then_some(preference);
        ShellSizeAction::Adopt { preference, push }
    } else if (step.dock - step.display).abs() > 0.5 {
        ShellSizeAction::Push(step.display.clamp(step.min, cap))
    } else {
        ShellSizeAction::Keep
    }
}

/// 鼠标还按着时不要把 Dock 推回去，否则会和正在拖的指针对着干。
/// 松手后的下一帧再按同一套规则归位。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShellSizeDecision {
    pub(crate) preference: f32,
    pub(crate) applied: f32,
    pub(crate) push: Option<f32>,
    pub(crate) dragged: bool,
}

pub(crate) fn decide_shell_size(step: ShellSizeStep, pointer_down: bool) -> ShellSizeDecision {
    match shell_size_action(step) {
        ShellSizeAction::Keep => ShellSizeDecision {
            preference: step.preference,
            applied: step.applied,
            push: None,
            dragged: false,
        },
        ShellSizeAction::Adopt { preference, push } => ShellSizeDecision {
            preference,
            applied: if pointer_down {
                step.dock
            } else {
                push.unwrap_or(preference)
            },
            push: if pointer_down { None } else { push },
            dragged: true,
        },
        ShellSizeAction::Push(next) => {
            if pointer_down {
                ShellSizeDecision {
                    preference: step.preference,
                    applied: step.applied,
                    push: None,
                    dragged: false,
                }
            } else {
                ShellSizeDecision {
                    preference: step.preference,
                    applied: next,
                    push: Some(next),
                    dragged: false,
                }
            }
        }
    }
}

fn sidebar_cap(viewport: Pixels) -> f32 {
    let shell = f32::from(ui_theme::shell_padding()) * 2.0;
    (f32::from(viewport) - shell - MIN_WORKSPACE_CONTENT_WIDTH).max(MIN_SIDEBAR_WIDTH)
}

fn tool_cap(viewport: Pixels, sidebar_on_screen: f32) -> f32 {
    let shell = f32::from(ui_theme::shell_padding()) * 2.0;
    (f32::from(viewport) - shell - sidebar_on_screen - MIN_WORKSPACE_CONTENT_WIDTH)
        .max(MIN_TOOL_PANEL_WIDTH)
}

fn tool_display(preference: f32, viewport: Pixels, sidebar_on_screen: f32) -> f32 {
    preference.clamp(MIN_TOOL_PANEL_WIDTH, tool_cap(viewport, sidebar_on_screen))
}

impl Workspace {
    /// 渲染前把存档里的开合、宽度写进 Dock；用户拖动分隔条后把新宽度写回存档。
    pub(crate) fn align_shell_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let viewport = window.viewport_size().width;
        let pointer_down = self.shell_pointer_down;
        let sidebar_open = self.sidebar_open;
        let mut sidebar_w = self.sidebar_w;
        let mut applied_sidebar = self.shell_applied_sidebar;
        let tool_open = self.tool_panel_open && !self.tool_panel_promoted();
        let mut tool_w = self.tool_panel_w;
        let mut applied_tool = self.shell_applied_tool;
        let mut dragged = false;

        let sidebar_display = crate::sidebar_width_for_viewport(sidebar_w, viewport);
        let sessions_dock = self.shell_layout.sessions_dock;
        let tools_dock = self.shell_layout.tools_dock;
        let left_open = self.shell_dock.update(cx, |dock, cx| {
            if let Some(placement) = sessions_dock {
                sync_edge(
                    dock,
                    placement,
                    sidebar_open,
                    &mut sidebar_w,
                    &mut applied_sidebar,
                    sidebar_display,
                    MIN_SIDEBAR_WIDTH,
                    sidebar_cap(viewport),
                    pointer_down,
                    &mut dragged,
                    window,
                    cx,
                );
            }
            if let Some(placement) = tools_dock {
                let across = opposite_on_screen(dock, placement);
                let tool_shown = tool_display(tool_w, viewport, across);
                sync_edge(
                    dock,
                    placement,
                    tool_open,
                    &mut tool_w,
                    &mut applied_tool,
                    tool_shown,
                    MIN_TOOL_PANEL_WIDTH,
                    tool_cap(viewport, across),
                    pointer_down,
                    &mut dragged,
                    window,
                    cx,
                );
            }
            dock.is_dock_open(DockPlacement::Left)
        });
        // 左停靠区收起或没有时，中间区域贴着窗口左上角。
        self.shell_stage_top_left = !left_open && self.shell_layout.stage_top_left_of_center;

        if (self.sidebar_w - sidebar_w).abs() > 0.5 {
            self.sidebar_w = sidebar_w;
        }
        if (self.tool_panel_w - tool_w).abs() > 0.5 {
            self.tool_panel_w = tool_w;
        }
        self.shell_applied_sidebar = applied_sidebar;
        self.shell_applied_tool = applied_tool;
        if dragged {
            self.shell_needs_save = true;
        }
    }

    pub(crate) fn commit_shell_pointer(
        &mut self,
        window: &mut Window,
        viewport: Pixels,
        cx: &mut Context<Self>,
    ) {
        let gesture_sidebar = self.shell_gesture_sidebar_w.take();
        let gesture_tool = self.shell_gesture_tool_w.take();
        self.shell_pointer_down = false;
        if let Some(width) = self.sidebar_collapse_width(gesture_sidebar, cx) {
            self.collapse_sidebar(width, window, cx);
        }
        if let Some(width) = self.tool_panel_collapse_width(gesture_tool, cx) {
            self.collapse_tool_panel(width, window, cx);
        }
        let snap = self.shell_edges_need_snap(viewport, cx);
        if self.shell_needs_save {
            self.shell_needs_save = false;
            self.save_state(cx);
        }
        // 拖动过程中故意没把越界宽度推回去。松手后重画一帧，align 再归位。
        if snap {
            cx.notify();
        }
    }

    /// 左栏还开着，并且松手时窄于收起线，就回到按下时的宽度并收起。
    /// 宽于收起线时返回 `None`，宽度仍按拖动规则夹回最小宽度。
    fn sidebar_collapse_width(&self, gesture_width: Option<f32>, cx: &App) -> Option<f32> {
        if !self.sidebar_open {
            return None;
        }
        let placement = self.shell_layout.sessions_dock?;
        let dock_width = open_dock_width(&self.shell_dock, placement, cx)?;
        sidebar_width_after_collapse(gesture_width.unwrap_or(self.sidebar_w), dock_width)
    }

    /// 右栏还停在边上（没有铺进舞台），并且松手时窄于同一条收起线。
    fn tool_panel_collapse_width(&self, gesture_width: Option<f32>, cx: &App) -> Option<f32> {
        if !self.tool_panel_open || self.tool_panel_promoted() {
            return None;
        }
        let placement = self.shell_layout.tools_dock?;
        let dock_width = open_dock_width(&self.shell_dock, placement, cx)?;
        tool_panel_width_after_collapse(gesture_width.unwrap_or(self.tool_panel_w), dock_width)
    }

    fn collapse_sidebar(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_w = width;
        self.sidebar_open = false;
        self.shell_applied_sidebar = width;
        if let Some(placement) = self.shell_layout.sessions_dock {
            self.close_dock_keeping_width(placement, width, window, cx);
        }
        self.focus_active_stage(window, cx);
        self.shell_needs_save = true;
    }

    fn collapse_tool_panel(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.tool_panel_w = width;
        self.set_tool_panel_open(false, cx);
        self.shell_applied_tool = width;
        if let Some(placement) = self.shell_layout.tools_dock {
            self.close_dock_keeping_width(placement, width, window, cx);
        }
        self.focus_active_stage(window, cx);
        self.shell_needs_save = true;
    }

    /// 先收起再写回宽度。开着的时候改宽度会先闪回拖动前的尺寸。
    fn close_dock_keeping_width(
        &mut self,
        placement: DockPlacement,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shell_dock.update(cx, |dock, cx| {
            if dock.is_dock_open(placement) {
                dock.toggle_dock(placement, window, cx);
            }
            dock.set_dock_size(placement, px(width), window, cx);
        });
    }

    fn shell_edges_need_snap(&self, viewport: Pixels, cx: &App) -> bool {
        let sidebar_open = self.sidebar_open;
        let sidebar_display = crate::sidebar_width_for_viewport(self.sidebar_w, viewport);
        let tool_open = self.tool_panel_open && !self.tool_panel_promoted();
        let tool_w = self.tool_panel_w;
        let dock = self.shell_dock.read(cx);
        let sessions_snap = self.shell_layout.sessions_dock.is_some_and(|placement| {
            edge_needs_snap(dock, placement, sidebar_open, sidebar_display)
        });
        let tools_snap = self.shell_layout.tools_dock.is_some_and(|placement| {
            let across = opposite_on_screen(dock, placement);
            edge_needs_snap(
                dock,
                placement,
                tool_open,
                tool_display(tool_w, viewport, across),
            )
        });
        sessions_snap || tools_snap
    }

    pub(crate) fn render_sidebar_dock(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let surface: Hsla = rgb(ui_theme::bg_column()).into();
        let animate = self.workspace_window_active && self.sidebar_open;
        let statuses: Vec<AgentStatus> = self
            .sessions
            .iter()
            .map(|session| session.status(cx))
            .collect();
        let focused_key = self.sidebar_focused_key(window);
        let list = self.render_session_list(cx.entity(), cx, &statuses, animate, focused_key);
        // 间距留在卡片外面，透出壳的底色。加在卡片上会被卡片自己的底色填满。
        div()
            .size_full()
            .pr(ui_theme::chrome_gap())
            .child(
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
                    )),
            )
            .into_any_element()
    }

    pub(crate) fn render_tool_dock(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let surface: Hsla = rgb(ui_theme::bg_column()).into();
        div()
            .size_full()
            .pl(ui_theme::chrome_gap())
            .child(workspace_frame::card(surface).child(self.render_tool_panel(window, cx)))
            .into_any_element()
    }

    pub(crate) fn render_stage_dock(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stage_surface: Hsla = rgb(ui_theme::bg_stage()).into();
        let left_guard = self.chrome_left_guard(window);
        let stage_content: AnyElement = match self.active_tab() {
            WorkspaceRoute::Agents => self.render_agents_page(window, cx),
            WorkspaceRoute::Automations => self.render_automations_page(window, cx),
            WorkspaceRoute::Capabilities => self.render_capabilities_page(window, cx),
            WorkspaceRoute::Desktop => self.render_desktop_page(window, cx),
            WorkspaceRoute::Session => {
                if self.tool_panel_promoted() {
                    self.render_tool_panel_stage(left_guard, window, cx)
                } else {
                    let background_image = cx.global::<Appearance>().bg_image.clone();
                    let content = if self
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
            }
        };
        let stage = div()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(stage_content);
        // 顶带不画在舞台面板里。舞台和右栏共用的那一条由工作台盖在两列上沿。
        div()
            .size_full()
            .child(workspace_frame::card(stage_surface).child(stage))
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

/// 松手时窄于收起线，就用按下前的宽度，并把它抬到这一栏的最小宽度以上。
/// 否则这次松手不收起。
fn width_after_collapse(gesture_width: f32, dock_width: f32, min_width: f32) -> Option<f32> {
    if dock_width < crate::DOCK_COLLAPSE_WIDTH {
        Some(gesture_width.max(min_width))
    } else {
        None
    }
}

fn sidebar_width_after_collapse(gesture_width: f32, dock_width: f32) -> Option<f32> {
    width_after_collapse(gesture_width, dock_width, crate::MIN_SIDEBAR_WIDTH)
}

fn tool_panel_width_after_collapse(gesture_width: f32, dock_width: f32) -> Option<f32> {
    width_after_collapse(gesture_width, dock_width, crate::MIN_TOOL_PANEL_WIDTH)
}

fn open_dock_width(dock: &Entity<DockArea>, placement: DockPlacement, cx: &App) -> Option<f32> {
    let dock = dock.read(cx);
    if !dock.is_dock_open(placement) {
        return None;
    }
    dock.dock_size(placement).map(|size| f32::from(size))
}

/// 对面那个停靠区在屏幕上占多宽。工具面板的上限要让开它。
fn opposite_on_screen(dock: &DockArea, placement: DockPlacement) -> f32 {
    let across = match placement {
        DockPlacement::Left => DockPlacement::Right,
        _ => DockPlacement::Left,
    };
    if dock.is_dock_open(across) {
        dock.dock_size(across).map_or(0., f32::from)
    } else {
        0.
    }
}

fn edge_needs_snap(
    dock: &DockArea,
    placement: DockPlacement,
    want_open: bool,
    display: f32,
) -> bool {
    if !want_open || !dock.is_dock_open(placement) {
        return false;
    }
    dock.dock_size(placement)
        .is_some_and(|size| (f32::from(size) - display).abs() > 0.5)
}

fn sync_edge(
    dock: &mut DockArea,
    placement: DockPlacement,
    want_open: bool,
    preference: &mut f32,
    applied: &mut f32,
    display: f32,
    min: f32,
    max: f32,
    pointer_down: bool,
    dragged: &mut bool,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
    if !dock.has_dock(placement) {
        return;
    }
    if dock.is_dock_open(placement) != want_open {
        dock.toggle_dock(placement, window, cx);
    }
    if !want_open {
        return;
    }
    let Some(size) = dock.dock_size(placement) else {
        return;
    };
    let dock_w = f32::from(size);
    let decision = decide_shell_size(
        ShellSizeStep {
            preference: *preference,
            applied: *applied,
            dock: dock_w,
            display,
            min,
            max,
        },
        pointer_down,
    );
    *preference = decision.preference;
    *applied = decision.applied;
    if decision.dragged {
        *dragged = true;
    }
    if let Some(push) = decision.push {
        dock.set_dock_size(placement, px(push), window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ShellSizeAction, ShellSizeDecision, ShellSizeStep, decide_shell_size, shell_size_action,
        sidebar_width_after_collapse, tool_panel_width_after_collapse,
    };

    fn step(preference: f32, applied: f32, dock: f32, display: f32) -> ShellSizeStep {
        ShellSizeStep {
            preference,
            applied,
            dock,
            display,
            min: 240.,
            max: 10_000.,
        }
    }

    fn capped(dock: f32, max: f32) -> ShellSizeStep {
        ShellSizeStep {
            preference: 320.,
            applied: 320.,
            dock,
            display: 320.,
            min: 240.,
            max,
        }
    }

    /// 布局格式认的每个内置组件，外壳都要有面板画它。
    #[test]
    fn every_builtin_component_has_a_panel() {
        use crate::tool_panel::ToolPanelTab;
        use smelt_client::layout::Component;
        for component in [
            Component::Sessions,
            Component::Stage,
            Component::Tools,
            Component::Files,
            Component::Git,
            Component::History,
        ] {
            let name = component.name();
            let tool = ToolPanelTab::from_builtin_component(&component).is_some();
            let region = super::builtin(&name).is_some();
            assert!(
                tool != region,
                "{name} 要么是工具页签、要么是外壳大块，恰好一种"
            );
        }
    }

    /// 仓库里自带的示例布局都要合法。
    #[test]
    fn repo_layouts_are_valid() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../layouts");
        let entries = smelt_client::layout::discover(&[dir]);
        assert!(!entries.is_empty());
        for entry in entries {
            if let Err(error) = entry.layout {
                panic!("{}：{error}", entry.path.display());
            }
        }
    }

    #[test]
    fn drag_adopts_the_dock_width() {
        assert_eq!(
            shell_size_action(step(240., 240., 320., 240.)),
            ShellSizeAction::Adopt {
                preference: 320.,
                push: None,
            }
        );
    }

    #[test]
    fn drag_below_the_minimum_is_pushed_back() {
        assert_eq!(
            shell_size_action(step(240., 240., 100., 240.)),
            ShellSizeAction::Adopt {
                preference: 240.,
                push: Some(240.),
            }
        );
    }

    #[test]
    fn a_narrow_window_pushes_the_display_and_keeps_the_preference() {
        assert_eq!(
            shell_size_action(step(360., 360., 360., 280.)),
            ShellSizeAction::Push(280.)
        );
    }

    #[test]
    fn growing_the_window_restores_the_preference() {
        assert_eq!(
            shell_size_action(step(360., 280., 280., 360.)),
            ShellSizeAction::Push(360.)
        );
    }

    #[test]
    fn an_aligned_dock_is_left_alone() {
        assert_eq!(
            shell_size_action(step(320., 320., 320., 320.)),
            ShellSizeAction::Keep
        );
    }

    #[test]
    fn a_drag_past_the_cap_is_clamped() {
        assert_eq!(
            shell_size_action(capped(700., 536.)),
            ShellSizeAction::Adopt {
                preference: 536.,
                push: Some(536.),
            }
        );
    }

    #[test]
    fn a_held_pointer_follows_the_dock_and_does_not_push() {
        assert_eq!(
            decide_shell_size(capped(700., 536.), true),
            ShellSizeDecision {
                preference: 536.,
                applied: 700.,
                push: None,
                dragged: true,
            }
        );
    }

    #[test]
    fn releasing_the_pointer_pushes_a_capped_drag_back() {
        assert_eq!(
            decide_shell_size(capped(700., 536.), false),
            ShellSizeDecision {
                preference: 536.,
                applied: 536.,
                push: Some(536.),
                dragged: true,
            }
        );
    }

    #[test]
    fn releasing_below_the_collapse_line_keeps_the_width_from_before_the_drag() {
        assert_eq!(sidebar_width_after_collapse(360., 100.), Some(360.));
        assert_eq!(sidebar_width_after_collapse(360., 159.9), Some(360.));
    }

    #[test]
    fn releasing_on_the_collapse_line_stays_open() {
        assert_eq!(
            sidebar_width_after_collapse(360., crate::DOCK_COLLAPSE_WIDTH),
            None
        );
        assert_eq!(sidebar_width_after_collapse(360., 240.), None);
    }

    #[test]
    fn a_collapse_repairs_a_gesture_width_below_the_minimum() {
        assert_eq!(sidebar_width_after_collapse(100., 100.), Some(240.));
    }

    #[test]
    fn releasing_a_narrow_tool_panel_keeps_the_width_from_before_the_drag() {
        assert_eq!(tool_panel_width_after_collapse(480., 100.), Some(480.));
        assert_eq!(tool_panel_width_after_collapse(480., 159.9), Some(480.));
        assert_eq!(
            tool_panel_width_after_collapse(480., crate::DOCK_COLLAPSE_WIDTH),
            None
        );
        assert_eq!(tool_panel_width_after_collapse(200., 100.), Some(320.));
    }

    #[test]
    fn a_held_pointer_does_not_push_a_narrow_window() {
        assert_eq!(
            decide_shell_size(step(360., 360., 360., 280.), true),
            ShellSizeDecision {
                preference: 360.,
                applied: 360.,
                push: None,
                dragged: false,
            }
        );
    }
}
