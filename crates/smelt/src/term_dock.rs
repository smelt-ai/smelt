//! 终端分屏：每个终端会话一棵 `gpui-component` 的 Dock。
//!
//! 布局树只在 Dock 里，这里不另存一份。分屏、关格子走 Dock 的编辑接口；拖标签换位置、
//! 拖到同一格叠成标签组、拖分隔条，都由 Dock 自己改。树一变 Dock 发 `LayoutChanged`，
//! 工作台按树重算格子列表（侧栏、状态聚合、Cmd+[ / ] 都读它）并落盘。
//!
//! 存档时把 `DockArea::dump` 转成运行时认得的 [`PaneState`]，不把组件库自己的存档格式
//! 交给运行时；恢复时反过来用 [`DockLayout`] 搭回去，不经面板注册表。

use gpui::{
    App, AppContext as _, Axis, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, WeakEntity, Window, div, hsla, prelude::FluentBuilder as _,
};
use gpui_component::Placement;
use gpui_component::dock::{
    BasePanel, DockArea, DockEvent, DockLayout, DockPlacement, DockSkin, InsertTarget, Panel,
    PanelEvent, PanelId, PanelInfo, PanelState, panel_handle,
};

use crate::terminal_view::TerminalView;
use crate::{PaneState, Workspace};

/// 存档里认格子用的名字。定了就不要改。
const TERM_PANE: &str = "smelt.terminal";

pub(crate) struct TermDock {
    area: Entity<DockArea>,
    /// 树里的格子，按深度优先序。只在树变了的时候重算，渲染和侧栏都读这份。
    panes: Vec<Entity<TermPane>>,
    /// 和 `panes` 一一对应的终端。
    leaves: Vec<Entity<TerminalView>>,
    _layout_changed: Subscription,
}

impl TermDock {
    /// 只有一个终端的会话。
    pub(crate) fn single(
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Self {
        let pane = TermPane::new(view, cx);
        let layout = DockLayout::tabs().panel_view(panel_handle(pane), cx);
        Self::install(layout, window, cx)
    }

    /// 按 `layout` 搭一棵 Dock。恢复存档时 `layout` 由 [`dock_layout_from_state`] 给出。
    pub(crate) fn install(
        layout: DockLayout,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Self {
        let (area, _skin) = DockSkin::dock_area("smelt-term", None, window, cx);
        area.update(cx, |area, cx| area.set_center(layout, window, cx));
        let _layout_changed = cx.subscribe_in(
            &area,
            window,
            |this: &mut Workspace, area, event: &DockEvent, _, cx| {
                if matches!(event, DockEvent::LayoutChanged) {
                    this.term_layout_changed(area, cx);
                }
            },
        );
        let mut dock = Self {
            area,
            panes: Vec::new(),
            leaves: Vec::new(),
            _layout_changed,
        };
        dock.refresh(cx);
        dock
    }

    pub(crate) fn area(&self) -> &Entity<DockArea> {
        &self.area
    }

    /// 会话里的全部终端，深度优先序（= 存档里活动格的下标基准）。
    pub(crate) fn leaves(&self) -> &[Entity<TerminalView>] {
        &self.leaves
    }

    /// 按 Dock 现在的树重算格子列表。
    pub(crate) fn refresh(&mut self, cx: &App) {
        let area = self.area.read(cx);
        self.panes = area
            .layout(DockPlacement::Center)
            .map(|tree| {
                tree.panels()
                    .filter_map(|id| area.panel(id)?.view().downcast::<TermPane>().ok())
                    .collect()
            })
            .unwrap_or_default();
        self.leaves = self
            .panes
            .iter()
            .map(|pane| pane.read(cx).view.clone())
            .collect();
    }

    /// 在 `beside` 那一格旁边新开一格放 `view`：`Horizontal` 放右边，`Vertical` 放下面。
    /// `beside` 已经不在树里（新终端握手期间被关掉了）就什么都不做，返回 `false`。
    pub(crate) fn split(
        &mut self,
        beside: &Entity<TerminalView>,
        view: Entity<TerminalView>,
        axis: Axis,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> bool {
        let Some(beside) = self.pane_of(beside).cloned() else {
            return false;
        };
        let placement = match axis {
            Axis::Horizontal => Placement::Right,
            Axis::Vertical => Placement::Bottom,
        };
        let pane = TermPane::new(view, cx);
        let id = PanelId::from(pane.entity_id());
        let handle = panel_handle(pane);
        let added = self.area.update(cx, |area, cx| {
            let Some(node) = area
                .layout(DockPlacement::Center)
                .and_then(|tree| tree.find_panel_node(PanelId::from(beside.entity_id())))
            else {
                return false;
            };
            // Dock 只挪它登记过的面板。登记时新格子先并进第一组，紧接着挪到旁边。
            area.add_panel_view(handle, DockPlacement::Center, None, window, cx);
            area.move_panel(
                id,
                InsertTarget::Split {
                    node,
                    placement,
                    size: None,
                },
                window,
                cx,
            );
            true
        });
        self.refresh(cx);
        added
    }

    /// 把 `view` 那一格从树里拿掉。终端进程归调用方处理。
    pub(crate) fn remove(
        &mut self,
        view: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let Some(pane) = self.pane_of(view).cloned() else {
            return;
        };
        self.area
            .update(cx, |area, cx| area.remove_panel(pane, window, cx));
        self.refresh(cx);
    }

    /// 存档用的布局树。树里一格都没有时返回 `None`。
    pub(crate) fn pane_state(&self, cx: &App) -> Option<PaneState> {
        pane_state_from_dock(&self.area.read(cx).dump(cx).center)
    }

    fn pane_of(&self, view: &Entity<TerminalView>) -> Option<&Entity<TermPane>> {
        let ix = self.leaves.iter().position(|leaf| leaf == view)?;
        self.panes.get(ix)
    }
}

impl Workspace {
    /// 某个会话的分屏树变了（拖动、分屏、关格子都会走到这里）：重算格子列表，活动格
    /// 不在了就换成第一格，再落盘。
    pub(crate) fn term_layout_changed(&mut self, area: &Entity<DockArea>, cx: &mut Context<Self>) {
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.term_dock().is_some_and(|dock| dock.area() == area))
        else {
            return;
        };
        let crate::SessionKind::Term { dock, active } = &mut session.kind else {
            return;
        };
        dock.refresh(cx);
        if !dock.leaves().contains(active)
            && let Some(first) = dock.leaves().first()
        {
            *active = first.clone();
        }
        self.save_state(cx);
        cx.notify();
    }

    /// `(会话分屏了, 这一格是活动格)`，只看当前会话。
    pub(crate) fn term_pane_role(&self, view: &Entity<TerminalView>) -> (bool, bool) {
        let Some(session) = self.cur() else {
            return (false, false);
        };
        let Some(dock) = session.term_dock() else {
            return (false, false);
        };
        if !dock.leaves().contains(view) {
            return (false, false);
        }
        (
            dock.leaves().len() > 1,
            session.anchor_id() == view.entity_id(),
        )
    }
}

/// 一个终端在存档里的样子：叶子读终端当前的 cwd、名字和启动命令。
pub(crate) fn pane_leaf_state(view: &Entity<TerminalView>, cx: &App) -> PaneState {
    let view = view.read(cx);
    PaneState::Leaf {
        cwd: view.cwd(),
        id: Some(view.session_id().to_string()),
        custom_title: view.custom_title().map(str::to_string),
        launch_label: view.launch_label().map(str::to_string),
        launch_cmd: view.launch_cmd().map(str::to_string),
    }
}

/// Dock 的存档 → 运行时的布局树。
///
/// 只有一个终端的标签组直接写成叶子，只剩一个子块的分屏收掉这一层。认不出的格子丢掉；
/// 分屏里丢了子块，尺寸就对不上号了，整组改成均分，免得把 A 的宽度套到 B 头上。
pub(crate) fn pane_state_from_dock(state: &PanelState) -> Option<PaneState> {
    match &state.info {
        PanelInfo::Stack { sizes, .. } => {
            let axis = state.info.axis()?;
            let children: Vec<PaneState> = state
                .children
                .iter()
                .filter_map(pane_state_from_dock)
                .collect();
            let sizes = if children.len() == state.children.len() && sizes.len() == children.len() {
                sizes.iter().map(|size| f32::from(*size)).collect()
            } else {
                Vec::new()
            };
            collapse(children, |children| PaneState::Split {
                axis: axis.into(),
                children,
                sizes,
            })
        }
        PanelInfo::Tabs { active_index } => {
            let children: Vec<PaneState> = state
                .children
                .iter()
                .filter_map(pane_state_from_dock)
                .filter(|child| matches!(child, PaneState::Leaf { .. }))
                .collect();
            let active = (*active_index).min(children.len().saturating_sub(1));
            collapse(children, |children| PaneState::Tabs { active, children })
        }
        PanelInfo::Panel(info) => (state.panel_name == TERM_PANE)
            .then(|| serde_json::from_value::<PaneState>(info.clone()).ok())
            .flatten()
            .filter(|leaf| matches!(leaf, PaneState::Leaf { .. })),
    }
}

/// 没有子块就没有这一层；只剩一个就用它本身；两个以上才包一层。
fn collapse(
    mut children: Vec<PaneState>,
    wrap: impl FnOnce(Vec<PaneState>) -> PaneState,
) -> Option<PaneState> {
    match children.len() {
        0 => None,
        1 => children.pop(),
        _ => Some(wrap(children)),
    }
}

/// 存档布局树 → Dock 布局。`next_view` 按深度优先序给出每个叶子的终端，`None` 表示
/// 这一格没起来；分屏里丢了子块就整组均分（同 [`pane_state_from_dock`]）。
pub(crate) fn dock_layout_from_state(
    state: &PaneState,
    next_view: &mut dyn FnMut(&PaneState, &mut Context<Workspace>) -> Option<Entity<TerminalView>>,
    cx: &mut Context<Workspace>,
) -> Option<DockLayout> {
    match state {
        PaneState::Leaf { .. } => {
            let view = next_view(state, cx)?;
            let pane = TermPane::new(view, cx);
            Some(DockLayout::tabs().panel_view(panel_handle(pane), cx))
        }
        PaneState::Tabs { active, children } => {
            let mut layout = DockLayout::tabs();
            let mut kept = 0;
            for child in children {
                if !matches!(child, PaneState::Leaf { .. }) {
                    continue;
                }
                let Some(view) = next_view(child, cx) else {
                    continue;
                };
                let pane = TermPane::new(view, cx);
                layout = layout.panel_view(panel_handle(pane), cx);
                kept += 1;
            }
            (kept > 0).then(|| layout.active_index((*active).min(kept - 1)))
        }
        PaneState::Split {
            axis,
            children,
            sizes,
        } => {
            let mut kept = Vec::new();
            for child in children {
                if let Some(layout) = dock_layout_from_state(child, &mut *next_view, cx) {
                    kept.push(layout);
                }
            }
            match kept.len() {
                0 => None,
                1 => kept.pop(),
                _ => {
                    let sized = kept.len() == children.len() && sizes.len() == kept.len();
                    let mut layout = match Axis::from(*axis) {
                        Axis::Horizontal => DockLayout::h_split(),
                        Axis::Vertical => DockLayout::v_split(),
                    };
                    for (ix, child) in kept.into_iter().enumerate() {
                        let size = sized
                            .then(|| sizes[ix])
                            .filter(|size| *size > 0.)
                            .map(gpui::px);
                        layout = layout.child(child, size);
                    }
                    Some(layout)
                }
            }
        }
    }
}

/// Dock 里的一格：包一个终端，负责点击设活动格、分屏时压暗非活动格。
pub(crate) struct TermPane {
    view: Entity<TerminalView>,
    workspace: WeakEntity<Workspace>,
    /// 活动格是工作台上的状态，换了活动格要跟着重画压暗层。
    _workspace_refresh: Subscription,
}

impl TermPane {
    fn new(view: Entity<TerminalView>, cx: &mut Context<Workspace>) -> Entity<Self> {
        let workspace = cx.entity();
        cx.new(|cx| Self {
            view,
            workspace: workspace.downgrade(),
            _workspace_refresh: cx.observe(&workspace, |_, _, cx| cx.notify()),
        })
    }

    /// `(会话分屏了, 这一格是活动格)`。不在当前会话里的格子不在屏上，按没分屏算。
    fn role(&self, cx: &App) -> (bool, bool) {
        self.workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).term_pane_role(&self.view))
            .unwrap_or((false, false))
    }
}

impl EventEmitter<PanelEvent> for TermPane {}

impl Focusable for TermPane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.read(cx).focus_handle()
    }
}

impl BasePanel for TermPane {
    fn panel_name(&self) -> &'static str {
        TERM_PANE
    }

    /// 关格子走工作台：正在跑命令的要先确认，关掉还要让守护杀掉进程。
    fn closable(&self, _: &App) -> bool {
        false
    }

    /// 叶子整个写进去，存档转换时不用再回头找终端。
    fn dump(&self, cx: &App) -> PanelState {
        PanelState {
            info: PanelInfo::panel(
                serde_json::to_value(pane_leaf_state(&self.view, cx)).unwrap_or_default(),
            ),
            ..PanelState::new(TERM_PANE)
        }
    }
}

impl Panel for TermPane {
    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        Some(crate::pane_title(&self.view, cx).into())
    }

    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from(crate::pane_title(&self.view, cx))
    }

    /// 标题条是拖动格子的把手。没分屏时没有别的格子可去，不画。
    fn title_bar(&self, cx: &App) -> bool {
        self.role(cx).0
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for TermPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (split, active) = self.role(cx);
        let view = self.view.clone();
        let workspace = self.workspace.clone();
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            // 点格子就设成当前会话的活动格（终端自己也会抢焦点，二者一致）。
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                let _ = workspace.update(cx, |workspace, cx| {
                    workspace.activate_pane(&view, window, cx)
                });
            })
            .child(self.view.clone())
            // 不描边（iTerm2 也不描）：分屏时压暗非活动格就分得出谁是活动的。
            .when(split && !active, |this| {
                this.child(div().absolute().inset_0().bg(hsla(0., 0., 0., 0.28)))
            })
    }
}
