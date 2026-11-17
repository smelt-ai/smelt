//! 会话的分屏（S2）：旧宿主的 `term_dock.rs` 搬到这里。一段会话的每一格——对话（`acp.sid`）
//! 和每格终端（`id`）——是 Dock 里的一个面板，里面装对话插件、终端插件按实例建的视图
//! （`peers::conversation::VIEW`、`peers::terminal::VIEW`）。分屏、叠标签、拖分隔条都由 Dock
//! 自己改；树一变 Dock 发 `LayoutChanged`，这里重算格子顺序、修正眼下那一格、存档。
//!
//! 有哪些格子只看工作区目录：[`Split::sync`] 按目录补格子、去格子，加减终端都是先改目录
//! （`lifecycle::update_conversation`），等目录推回来再到这里。怎么摆存在插件库
//! （`store::SavedLayout`，格式见 [`crate::split_layout`]），按守护会话 id 认格子。
//!
//! 分屏归会话视图（`session_view::SessionView`）：它建、它同步、它扔。眼下那一格换了发
//! [`SplitEvent::ActivePaneChanged`]，会话视图据此更新活动上下文；分屏自己不碰活动上下文。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyView, App, AppContext as _, Axis, Context, Div, Edges, Entity, EventEmitter,
    FocusHandle, Focusable, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    Render, SharedString, Stateful, StatefulInteractiveElement as _, StyleRefinement, Styled as _,
    Subscription, Window, div, hsla, px, rgb,
};
use gpui_base::dock::{
    DockArea, DockAreaRenderer, DockEvent, DockLayout, DockPlacement, DragPanel, DropIndicator,
    InsertTarget, NodeId, PaneNode, PaneRef, Panel, PanelEvent, PanelId, PanelInfo, PanelState,
    PanelView, TabGroupContext, TabGroupRenderer,
};
use gpui_base::{Placement, ResizeHandleContext};
use serde_json::Value;
use smelt_plugin_api::model::daemon_state::DaemonSessionState;
use smelt_plugin_api::model::workspace_directory::WorkspaceConversation;
use smelt_plugin_api::ui::dock::{
    TabLabel, active_panel, drag_preview, drop_indicator, split_handle, tab_strip,
};
use smelt_plugin_api::ui::tooltip::Tooltip;
use smelt_plugin_api::ui::truncated_line::truncated_line;
use smelt_plugin_api::ui::ui_theme;
use smelt_plugin_api::ui::window_bar::{TOP_BAR_HEIGHT, with_window_drag};
use smelt_plugin_api::{self as api, Adapt, HostEvent, RuntimeKey, SafeArea, ViewInstance};

use crate::model::{self, ConversationKey, Sessions};
use crate::peers::conversation::{self, ConversationArgs, ConversationStart};
use crate::peers::terminal::{self, TerminalArgs};
use crate::session_view::{Pane, PaneKind};
use crate::split_layout::{self, Node};
use crate::store::SavedLayout;

/// Dock 存档里认格子用的面板名。只在本插件里用（存档走 `split_layout`），但定了就不改。
const PANE_NAME: &str = "smelt.sessions.pane";

/// 只有终端的会话顶上那颗名字胶囊最宽多少。
const TITLE_ISLAND_MAX_WIDTH: f32 = 200.;

/// 一段会话的分屏。
pub(crate) struct Split {
    key: ConversationKey,
    area: Entity<DockArea>,
    /// 格子，按守护会话 id。
    members: HashMap<String, Member>,
    /// 树里的格子，深度优先序。Dock 的树变了才重算，轮换焦点按它。
    order: Vec<String>,
    /// 眼下那一格（守护会话 id）：焦点在会话里时是焦点那格，不在时是上次的。
    active: Option<String>,
    /// 这边刚分出去、还在等目录推回来的终端：推到时放在记下的那格旁边（没有就接在最
    /// 右边），焦点还在这段会话里就交给它。
    expected: HashMap<String, Option<(String, Axis)>>,
    /// 目录里的这条，[`Self::sync`] 时换成新的。只读缓存，算名字用。
    conversation: WorkspaceConversation,
    /// 会话的名字（只有终端时画在顶上）。会话状态变了重算。
    title: SharedString,
    /// 上次存进库的那份：一样就不再写。
    saved: SavedLayout,
    /// 会话视图分给分屏的 safe area（[`api::safe_area`]）的缓存，变了重分给各格。
    safe_area: SafeArea,
    /// 每组按它碰到分屏的哪几条边分到的那份，组的标题条按它给角让位。分的时候写（见
    /// [`Self::distribute_safe_area`]），画法只读。
    group_areas: GroupAreas,
    _subscriptions: Vec<Subscription>,
}

type GroupAreas = Rc<RefCell<HashMap<NodeId, SafeArea>>>;

/// 四条边都碰到。
const ALL_EDGES: Edges<bool> = Edges {
    top: true,
    right: true,
    bottom: true,
    left: true,
};

/// [`Split`] 发给会话视图的事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SplitEvent {
    /// 眼下那一格换了（[`Split::active_pane`]）。
    ActivePaneChanged,
}

impl EventEmitter<SplitEvent> for Split {}

struct Member {
    view: Entity<PaneView>,
    kind: PaneKind,
    /// 焦点进了这一格就把它记成眼下那一格。
    _focus_in: Subscription,
}

impl Split {
    /// 按目录里的那条建：每格一个面板，按 `saved` 摆（读不懂、没有就每格一列），`start`
    /// 交给对话那一格（新对话、分叉一建好就拉起）。视图还没登记上的格子先空着。
    pub(crate) fn new(
        key: &ConversationKey,
        conversation: &WorkspaceConversation,
        saved: Option<SavedLayout>,
        start: Option<ConversationStart>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let group_areas = GroupAreas::default();
        let area = cx.new(|cx| {
            DockArea::new("smelt.sessions.split", None, window, cx).with_renderer(Rc::new(
                SplitRenderer {
                    group_areas: group_areas.clone(),
                },
            ))
        });
        let saved = saved.unwrap_or_default();
        let events = api::events(cx);
        let split = cx.new(|cx| {
            let subscriptions = vec![
                cx.subscribe(&area, |this: &mut Self, _, event: &DockEvent, cx| {
                    if matches!(event, DockEvent::LayoutChanged) {
                        this.layout_changed(cx);
                    }
                }),
                cx.subscribe(
                    &events,
                    |this: &mut Self, _, event: &HostEvent, cx| match event {
                        HostEvent::SessionsChanged => this.refresh_titles(cx),
                        HostEvent::SafeAreaChanged => {
                            let area = api::safe_area(cx.entity_id(), cx);
                            if area != this.safe_area {
                                this.safe_area = area;
                                this.distribute_safe_area(cx);
                            }
                        }
                        _ => {}
                    },
                ),
            ];
            Self {
                key: key.clone(),
                area: area.clone(),
                members: HashMap::new(),
                order: Vec::new(),
                active: saved.active_pane.clone(),
                expected: HashMap::new(),
                conversation: conversation.clone(),
                title: SharedString::default(),
                saved: saved.clone(),
                safe_area: api::safe_area(cx.entity_id(), cx),
                group_areas,
                _subscriptions: subscriptions,
            }
        });
        split.update(cx, |this, cx| this.populate(start, window, cx));
        split
    }

    fn populate(
        &mut self,
        start: Option<ConversationStart>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let specs = pane_specs(&self.key.machine, &self.conversation, start);
        let ids: Vec<String> = specs.iter().map(|spec| spec.session_id.clone()).collect();
        for spec in specs {
            let view = self.add_member(spec, window, cx);
            view.update(cx, |pane, cx| pane.ensure_instance(window, cx));
        }
        let layout = split_layout::restore(self.saved.layout.as_deref(), &ids)
            .and_then(|node| dock_layout(&node, &self.members));
        if let Some(layout) = layout {
            self.area
                .update(cx, |area, cx| area.set_center(layout, window, cx));
        }
        self.refresh_titles(cx);
        self.layout_changed(cx);
        // 存档里露着的那格可能叠在标签后面。
        if let Some(active) = self.active.clone() {
            self.reveal(&active, window, cx);
        }
    }

    /// 上次 [`Self::sync`] 时目录里的这条。
    pub(crate) fn conversation(&self) -> &WorkspaceConversation {
        &self.conversation
    }

    /// 目录里这条变了，或视图登记变了：拿掉目录里没了的格子，补建缺的视图实例，新格子
    /// 摆上去。
    pub(crate) fn sync(
        &mut self,
        conversation: &WorkspaceConversation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.conversation = conversation.clone();
        let specs = pane_specs(&self.key.machine, conversation, None);
        let wanted: HashSet<&str> = specs.iter().map(|spec| spec.session_id.as_str()).collect();
        let focused = self.focused_pane(window, cx);

        let gone: Vec<String> = self
            .members
            .keys()
            .filter(|id| !wanted.contains(id.as_str()))
            .cloned()
            .collect();
        for id in &gone {
            if let Some(member) = self.members.remove(id) {
                self.area
                    .update(cx, |area, cx| area.remove_panel(member.view, window, cx));
            }
            self.expected.remove(id);
        }

        for member in self.members.values() {
            member
                .view
                .update(cx, |pane, cx| pane.ensure_instance(window, cx));
        }

        let mut arrived = None;
        for spec in specs {
            if self.members.contains_key(&spec.session_id) {
                continue;
            }
            let id = spec.session_id.clone();
            let expected = self.expected.remove(&id);
            let view = self.add_member(spec, window, cx);
            view.update(cx, |pane, cx| pane.ensure_instance(window, cx));
            let beside = expected.clone().flatten();
            self.insert(&view, beside, window, cx);
            if expected.is_some() {
                arrived = Some(id);
            }
        }

        self.refresh_titles(cx);
        self.layout_changed(cx);
        // 焦点原本在这段会话里时才动它：新分出来的那格接过焦点；焦点那格被拿掉了，交给
        // 眼下那一格。不在这段会话里（看着别的会话、别的视图）就不抢。
        match (focused, arrived) {
            (Some(_), Some(id)) => {
                self.focus_session(&id, window, cx);
            }
            (Some(was), None) if gone.contains(&was) => {
                self.focus_active(window, cx);
            }
            _ => {}
        }
    }

    /// 焦点交给眼下那一格。没有能接焦点的是 `false`。
    pub(crate) fn focus_active(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.active.clone().or_else(|| self.order.first().cloned()) else {
            return false;
        };
        self.focus_session(&id, window, cx)
    }

    /// 焦点交给守护会话 id 是 `session_id` 的那一格（叠在标签后面的先翻到前面）。这段
    /// 会话里没有这一格是 `false`。
    pub(crate) fn focus_session(
        &mut self,
        session_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(view) = self.reveal(session_id, window, cx) else {
            return false;
        };
        view.update(cx, |pane, cx| pane.focus_now(window, cx));
        self.set_active(session_id, cx);
        true
    }

    /// 焦点移到下一格（`forward`）或上一格，按树的深度优先序，到头绕回。
    pub(crate) fn cycle(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.order.len();
        if len == 0 {
            return;
        }
        let ix = self
            .active
            .as_ref()
            .and_then(|active| self.order.iter().position(|id| id == active))
            .unwrap_or(0);
        let next = if forward {
            (ix + 1) % len
        } else {
            (ix + len - 1) % len
        };
        let id = self.order[next].clone();
        self.focus_session(&id, window, cx);
    }

    /// 眼下那一格。只读内存。
    pub(crate) fn active_pane(&self) -> Option<Pane> {
        let id = self.active.as_ref()?;
        let member = self.members.get(id)?;
        Some(Pane {
            session_id: id.clone(),
            kind: member.kind,
        })
    }

    /// 眼下那一格的键盘焦点落点。只读内存。
    pub(crate) fn focus_target(&self, cx: &App) -> Option<FocusHandle> {
        let id = self.active.as_ref().or_else(|| self.order.first())?;
        Some(self.members.get(id)?.view.read(cx).focus_handle(cx))
    }

    /// 摆着几格。
    pub(crate) fn len(&self) -> usize {
        self.order.len()
    }

    /// 那一格标签上的名字。
    pub(crate) fn pane_title(&self, session_id: &str, cx: &App) -> SharedString {
        self.members
            .get(session_id)
            .map(|member| member.view.read(cx).title.clone())
            .unwrap_or_default()
    }

    /// 这边刚往目录里加了一格终端 `session_id`：推回来时放在 `beside` 那格的右边
    /// （`Axis::Horizontal`）或下边，焦点还在这段会话里就交给它。
    pub(crate) fn expect_pane(&mut self, session_id: String, beside: Option<String>, axis: Axis) {
        self.expected
            .insert(session_id, beside.map(|beside| (beside, axis)));
    }

    /// 加那一格没成（目录操作失败了）。
    pub(crate) fn forget_pane(&mut self, session_id: &str) {
        self.expected.remove(session_id);
    }

    fn add_member(
        &mut self,
        spec: PaneSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<PaneView> {
        let id = spec.session_id.clone();
        let kind = spec.kind;
        let view = cx.new(|cx| PaneView::new(spec, cx));
        let focus = view.read(cx).focus.clone();
        let target = id.clone();
        let focus_in = cx.on_focus_in(&focus, window, move |this, _, cx| {
            this.set_active(&target, cx);
        });
        self.members.insert(
            id,
            Member {
                view: view.clone(),
                kind,
                _focus_in: focus_in,
            },
        );
        view
    }

    /// 把新格子放进 Dock：`beside` 那格还在就放在它右边或下边，否则接在最右边一列。
    fn insert(
        &mut self,
        view: &Entity<PaneView>,
        beside: Option<(String, Axis)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = PanelId::from(view.entity_id());
        let beside = beside.and_then(|(id, axis)| {
            let entity = self.members.get(&id)?.view.entity_id();
            Some((PanelId::from(entity), axis))
        });
        let handle: Arc<dyn PanelView> = Arc::new(view.clone());
        self.area.update(cx, |area, cx| {
            let was_empty = area
                .layout(DockPlacement::Center)
                .is_none_or(|tree| tree.panels().next().is_none());
            // Dock 只挪它登记过的面板：先并进第一组，紧接着挪到要去的地方。
            area.add_panel_view(handle, DockPlacement::Center, None, window, cx);
            if was_empty {
                return;
            }
            let Some(tree) = area.layout(DockPlacement::Center) else {
                return;
            };
            let (node, placement) = match beside
                .and_then(|(beside, axis)| Some((tree.find_panel_node(beside)?, axis)))
            {
                Some((node, Axis::Horizontal)) => (node, Placement::Right),
                Some((node, Axis::Vertical)) => (node, Placement::Bottom),
                None => (tree.root().id(), Placement::Right),
            };
            area.move_panel(
                panel,
                InsertTarget::Split {
                    node,
                    placement,
                    size: None,
                },
                window,
                cx,
            );
        });
    }

    /// 那一格叠在标签后面就翻到前面。没有这一格是 `None`。
    fn reveal(
        &mut self,
        session_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<PaneView>> {
        let view = self.members.get(session_id)?.view.clone();
        let panel = PanelId::from(view.entity_id());
        self.area
            .update(cx, |area, cx| area.select_panel(panel, window, cx));
        Some(view)
    }

    /// 焦点在哪一格里。
    fn focused_pane(&self, window: &Window, cx: &App) -> Option<String> {
        self.members
            .iter()
            .find(|(_, member)| member.view.read(cx).focus.contains_focused(window, cx))
            .map(|(id, _)| id.clone())
    }

    /// Dock 的树变了（分屏、拖动、关格子、翻标签）：重算顺序，修正眼下那一格，存档。
    fn layout_changed(&mut self, cx: &mut Context<Self>) {
        let area = self.area.read(cx);
        self.order = area
            .layout(DockPlacement::Center)
            .map(|tree| {
                tree.panels()
                    .filter_map(|panel| area.panel(panel)?.view().downcast::<PaneView>().ok())
                    .map(|view| view.read(cx).session_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        if !self
            .active
            .as_ref()
            .is_some_and(|active| self.order.contains(active))
        {
            let first = self.order.first().cloned();
            if first.is_some() {
                self.active = first;
                cx.emit(SplitEvent::ActivePaneChanged);
            }
        }
        self.push_roles(cx);
        self.distribute_safe_area(cx);
        self.save(cx);
        cx.notify();
    }

    /// 把分屏的 safe area 按每组碰到的边分下去：组的那份给标题条让位，组里格子的视图拿
    /// 内容那份。只有终端时顶上那条让给了名字胶囊，格子都不碰顶；分屏后每组顶上有标题条，
    /// 格子里的视图也不碰顶。
    fn distribute_safe_area(&mut self, cx: &mut Context<Self>) {
        let own = if self.conversation.acp.is_none() {
            self.safe_area.child(Edges {
                top: false,
                ..ALL_EDGES
            })
        } else {
            self.safe_area
        };
        let many = self.order.len() > 1;
        let mut groups = HashMap::new();
        let mut panes = Vec::new();
        {
            let area = self.area.read(cx);
            if let Some(tree) = area.layout(DockPlacement::Center) {
                group_edges(tree.root(), ALL_EDGES, &mut |node, touches| {
                    let group = own.child(touches);
                    groups.insert(node.id(), group);
                    let content = if many {
                        group.child(Edges {
                            top: false,
                            ..ALL_EDGES
                        })
                    } else {
                        group
                    };
                    if let PaneRef::Tabs { panels, .. } = node.kind() {
                        panes.extend(
                            panels
                                .iter()
                                .filter_map(|panel| area.panel(*panel))
                                .filter_map(|panel| panel.view().downcast::<PaneView>().ok())
                                .map(|pane| (pane, content)),
                        );
                    }
                });
            }
        }
        let changed = *self.group_areas.borrow() != groups;
        if changed {
            *self.group_areas.borrow_mut() = groups;
            self.area.update(cx, |_, cx| cx.notify());
        }
        for (pane, area) in panes {
            let view = pane.read(cx).instance.as_ref().map(ViewInstance::view);
            if let Some(view) = view {
                api::set_safe_area(&view, area, cx);
            }
        }
    }

    fn set_active(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.active.as_deref() == Some(session_id) || !self.members.contains_key(session_id) {
            return;
        }
        self.active = Some(session_id.to_string());
        self.push_roles(cx);
        self.save(cx);
        cx.emit(SplitEvent::ActivePaneChanged);
        cx.notify();
    }

    /// 告诉每格它分没分屏、是不是眼下那一格（压暗、标签条跟着变）。格子自己不回头读
    /// 分屏：Dock 改树时会问面板，那时分屏正被借着。
    fn push_roles(&self, cx: &mut Context<Self>) {
        let many = self.order.len() > 1;
        for (id, member) in &self.members {
            let active = self.active.as_ref() == Some(id);
            member
                .view
                .update(cx, |pane, cx| pane.set_role(many, active, cx));
        }
    }

    /// 存档变了就写库。
    fn save(&mut self, cx: &mut Context<Self>) {
        let layout = SavedLayout {
            active_pane: self.active.clone(),
            layout: node_from_dock(&self.area.read(cx).dump(cx).center)
                .map(|node| split_layout::encode(&node)),
        };
        if layout == self.saved {
            return;
        }
        Sessions::global(cx)
            .read(cx)
            .store
            .save_layout(&self.key, &layout);
        self.saved = layout;
    }

    /// 按眼下的会话状态重算会话和每格的名字。
    fn refresh_titles(&mut self, cx: &mut Context<Self>) {
        let states = api::host(cx).session_states(&self.key.machine, cx);
        let title = SharedString::from(model::title(&self.conversation, &states));
        if title != self.title {
            self.title = title;
            cx.notify();
        }
        for (id, member) in &self.members {
            let title = pane_title(&self.conversation, member.kind, id, &states);
            member.view.update(cx, |pane, cx| pane.set_title(title, cx));
        }
    }
}

impl Render for Split {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // 有对话时名字由对话那一格自己画在它顶上；只有终端时这里画，并给它让出那一条。
        let bare = self.conversation.acp.is_none();
        let title_bar = self.safe_area.title_bar;
        div()
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .when(bare, |this| this.pt(px(ui_theme::CHROME_ISLAND_BAND_PX)))
            // 先画：垫在格子和胶囊下面。
            .when(title_bar && top_is_free(bare, self.order.len()), |this| {
                this.child(drag_band())
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .child(self.area.clone()),
            )
            .when(bare && !self.order.is_empty(), |this| {
                this.child(title_island(self.title.clone(), title_bar))
            })
    }
}

/// 按结构算每组碰到分屏的哪几条边：横排里每个都碰上下，只有头一个碰左、末一个碰右；
/// 竖排反过来。
fn group_edges(node: &PaneNode, touches: Edges<bool>, f: &mut impl FnMut(&PaneNode, Edges<bool>)) {
    match node.kind() {
        PaneRef::Tabs { .. } => f(node, touches),
        PaneRef::Split { axis, children, .. } => {
            let last = children.len().saturating_sub(1);
            for (ix, child) in children.iter().enumerate() {
                let (first, end) = (ix == 0, ix == last);
                let child_touches = match axis {
                    Axis::Horizontal => Edges {
                        left: touches.left && first,
                        right: touches.right && end,
                        ..touches
                    },
                    Axis::Vertical => Edges {
                        top: touches.top && first,
                        bottom: touches.bottom && end,
                        ..touches
                    },
                };
                group_edges(child, child_touches, f);
            }
        }
    }
}

/// 只有终端的会话顶上的名字胶囊。顶上是窗口标题栏时按住拖窗口，双击缩放。
fn title_island(title: SharedString, title_bar: bool) -> Div {
    let tip = title.clone();
    let island = div()
        .id("sessions-title")
        .h(px(ui_theme::CHROME_ISLAND_HEIGHT_PX))
        .max_w(px(TITLE_ISLAND_MAX_WIDTH))
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        // 浮层实色：平时一档卡片底，指上去再抬到悬停色。
        .bg(rgb(ui_theme::surface()))
        .border_1()
        .border_color(ui_theme::hair())
        .hover(|island| island.bg(rgb(ui_theme::soft())))
        .shadow_lg()
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .child(
            div()
                .min_w(px(0.))
                .overflow_hidden()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(ui_theme::text()))
                .child(truncated_line(title)),
        );
    div()
        .absolute()
        .top(px(ui_theme::CHROME_ISLAND_TOP_PX))
        .left_0()
        .right_0()
        .h(px(ui_theme::CHROME_ISLAND_HEIGHT_PX))
        .flex()
        .items_center()
        .justify_center()
        .child(if title_bar {
            with_window_drag(island)
        } else {
            island
        })
}

/// 分屏顶上那一整条拖窗口的带子，和窗口顶栏一样高：按住拖窗口，双击缩放。会话视图
/// 自己画顶（`draws_top_bar`），外壳不再给它画标题带，只靠胶囊拖太窄。顶上是窗口标题栏
/// （safe area 的 `title_bar`）时才画。
///
/// 垫在内容下面（调用方先画它）：胶囊（名字、吸顶提问）画在上面，按下先拦住；按在格子里
/// 第一下是拿焦点（`prevent_default`），也不拖。别的按在这一条里、没拦的都拖窗口。
fn drag_band() -> Stateful<Div> {
    with_window_drag(
        div()
            .id("sessions-drag-band")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(TOP_BAR_HEIGHT),
    )
}

/// 顶上那一条能不能拖窗口：格子的标题条（分屏后每格一条，拖它是挪格子）不能被带子压住。
/// 只有终端时顶上让出了胶囊那一条，格子在它下面，怎么分都行；有对话时格子从顶上开始，
/// 只剩一格（没有标题条）才行。
fn top_is_free(terminal_only: bool, panes: usize) -> bool {
    terminal_only || panes <= 1
}

// ---- 格子 ----

/// 目录里的一格：建视图要的东西。
struct PaneSpec {
    session_id: String,
    kind: PaneKind,
    args: Value,
}

/// 目录里这条有哪些格子：先对话，再按目录的顺序排终端。还没起过、没有 id 的终端不算。
fn pane_specs(
    machine: &RuntimeKey,
    conversation: &WorkspaceConversation,
    start: Option<ConversationStart>,
) -> Vec<PaneSpec> {
    let lead = conversation.acp.as_ref().map(|acp| PaneSpec {
        session_id: acp.sid.clone(),
        kind: PaneKind::Conversation,
        args: to_args(&ConversationArgs {
            machine: machine.clone(),
            session_id: acp.sid.clone(),
            placeholder: None,
            start,
        }),
    });
    let terminals = conversation.terminals.iter().filter_map(|terminal| {
        let id = terminal.id.clone()?;
        Some(PaneSpec {
            session_id: id.clone(),
            kind: PaneKind::Terminal,
            args: to_args(&TerminalArgs {
                machine: machine.clone(),
                session_id: id,
                cwd: terminal.cwd.clone(),
                launch: terminal.launch_cmd.clone(),
            }),
        })
    });
    lead.into_iter().chain(terminals).collect()
}

fn to_args(args: &impl serde::Serialize) -> Value {
    serde_json::to_value(args).unwrap_or(Value::Null)
}

/// 一格标签上的名字：终端是用户起的名字、守护报的名字，再没有是「终端」；对话是守护
/// 算好的展示名，没有是「对话」。
fn pane_title(
    conversation: &WorkspaceConversation,
    kind: PaneKind,
    session_id: &str,
    states: &[DaemonSessionState],
) -> SharedString {
    let state = states.iter().find(|state| state.id == session_id);
    let named = match kind {
        PaneKind::Terminal => conversation
            .terminals
            .iter()
            .find(|terminal| terminal.id.as_deref() == Some(session_id))
            .and_then(|terminal| terminal.custom_title.clone())
            .or_else(|| state.and_then(|state| state.name.clone())),
        PaneKind::Conversation => {
            state.and_then(|state| state.conversation_name.clone().or(state.title.clone()))
        }
    };
    match named
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        Some(name) => SharedString::from(name.to_string()),
        None => match kind {
            PaneKind::Terminal => "终端".into(),
            PaneKind::Conversation => "对话".into(),
        },
    }
}

/// Dock 里的一格：装一个视图实例，点一下设成眼下那一格，分屏时压暗不是眼下的格子。
pub(crate) struct PaneView {
    session_id: String,
    kind: PaneKind,
    /// 建实例的参数。对话那格建成之后去掉 `start`：视图登记换了要重建时不再当新对话拉起。
    args: Value,
    instance: Option<ViewInstance>,
    /// 没有实例时显示的原因。
    missing: SharedString,
    title: SharedString,
    focus: FocusHandle,
    /// 会话分屏了（不止一格）。
    many: bool,
    /// 是眼下那一格。
    active: bool,
}

impl PaneView {
    fn new(spec: PaneSpec, cx: &mut Context<Self>) -> Self {
        Self {
            session_id: spec.session_id,
            kind: spec.kind,
            args: spec.args,
            instance: None,
            missing: SharedString::default(),
            title: SharedString::default(),
            focus: cx.focus_handle(),
            many: false,
            active: false,
        }
    }

    fn view_id(&self) -> &'static str {
        match self.kind {
            PaneKind::Terminal => terminal::VIEW,
            PaneKind::Conversation => conversation::VIEW,
        }
    }

    /// 没有实例就建（视图登记上了才建得成），视图撤下了就放掉。
    fn ensure_instance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view_id = self.view_id();
        let registered = api::is_registered(view_id, cx);
        if self.instance.is_some() {
            if registered {
                return;
            }
            self.instance = None;
        }
        if !registered {
            self.missing = match self.kind {
                PaneKind::Terminal => "终端插件还没装上".into(),
                PaneKind::Conversation => "对话插件还没装上".into(),
            };
            cx.notify();
            return;
        }
        match api::create_view(view_id, &self.args, window, cx) {
            Ok(instance) => {
                self.instance = Some(instance);
                if let Some(start) = self.args.get_mut("start") {
                    *start = Value::Null;
                }
            }
            Err(error) => {
                eprintln!("[sessions] 建不了「{}」那一格：{error}", self.session_id);
                self.missing = error.into();
            }
        }
        cx.notify();
    }

    /// 焦点交给实例；实例不接管焦点或还没建成，就落在格子自己身上。
    fn focus_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let taken = self
            .instance
            .as_ref()
            .is_some_and(|instance| instance.focus(window, cx));
        if !taken {
            window.focus(&self.focus, cx);
        }
    }

    fn set_role(&mut self, many: bool, active: bool, cx: &mut Context<Self>) {
        if (self.many, self.active) != (many, active) {
            self.many = many;
            self.active = active;
            cx.notify();
        }
    }

    fn set_title(&mut self, title: SharedString, cx: &mut Context<Self>) {
        if self.title != title {
            self.title = title;
            cx.notify();
        }
    }

    /// 格子在组顶上的样子：小字、淡色，不跟内容抢眼；眼下那一格稍亮，和压暗别的格子一个
    /// 意思。组只画看得见的格子，这里不判断 `many`。
    fn label(&self) -> TabLabel {
        TabLabel {
            text: self.title.clone(),
            label: div()
                .text_xs()
                .text_color(rgb(if self.active {
                    ui_theme::muted()
                } else {
                    ui_theme::faint()
                }))
                .child(self.title.clone())
                .into_any_element(),
            badge: None,
        }
    }
}

impl EventEmitter<PanelEvent> for PaneView {}

impl Focusable for PaneView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.instance
            .as_ref()
            .and_then(|instance| instance.focus_handle(cx))
            .unwrap_or_else(|| self.focus.clone())
    }
}

impl Panel for PaneView {
    fn panel_name(&self) -> &'static str {
        PANE_NAME
    }

    /// 关格子走会话的命令：正在跑的要先确认，关掉还要改目录、让守护收掉进程。
    fn closable(&self, _: &App) -> bool {
        false
    }

    /// 只记守护会话 id，别的都按目录来。
    fn dump(&self, _: &App) -> PanelState {
        PanelState {
            info: PanelInfo::panel(serde_json::json!({ "id": self.session_id })),
            ..PanelState::new(PANE_NAME)
        }
    }
}

impl Render for PaneView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.instance {
            Some(instance) => instance
                .view()
                .cached(StyleRefinement::default().size_full())
                .into_any_element(),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(rgb(ui_theme::muted()))
                .child(self.missing.clone())
                .into_any_element(),
        };
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus)
            // 点格子就设成眼下那一格：焦点进了这一格，分屏那边的 `on_focus_in` 接着记下。
            // 点在实例接管焦点的地方由它自己拿；点空了（或被外层这个可聚焦的框先拿走）
            // 就转交给实例，免得焦点停在框上、键盘没人接。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.focus_handle(cx).contains_focused(window, cx) {
                        this.focus_now(window, cx);
                        window.prevent_default();
                    }
                }),
            )
            .child(body)
            // 不描边：分屏时压暗不是眼下的格子就分得出谁是眼下的。
            .when(self.many && !self.active, |this| {
                this.child(div().absolute().inset_0().bg(hsla(0., 0., 0., 0.28)))
            })
    }
}

// ---- Dock 和存档互转 ----

/// Dock 的树 → 存档的树。认不出的格子丢掉，规则同 [`split_layout`]：只剩一块收掉这一层，
/// 分屏少了块整组均分。
fn node_from_dock(state: &PanelState) -> Option<Node> {
    match &state.info {
        PanelInfo::Stack { sizes, .. } => {
            let axis = state.info.axis()?;
            let children: Vec<Node> = state.children.iter().filter_map(node_from_dock).collect();
            let sizes = if children.len() == state.children.len() && sizes.len() == children.len() {
                sizes.iter().map(|size| f32::from(*size)).collect()
            } else {
                Vec::new()
            };
            match children.len() {
                0 => None,
                1 => children.into_iter().next(),
                _ => Some(Node::Split {
                    axis: axis.into(),
                    children,
                    sizes,
                }),
            }
        }
        PanelInfo::Tabs { active_index } => {
            let panes: Vec<String> = state
                .children
                .iter()
                .filter_map(node_from_dock)
                .filter_map(|node| match node {
                    Node::Pane(id) => Some(id),
                    _ => None,
                })
                .collect();
            match panes.len() {
                0 => None,
                1 => panes.into_iter().next().map(Node::Pane),
                len => Some(Node::Tabs {
                    active: (*active_index).min(len - 1),
                    panes,
                }),
            }
        }
        PanelInfo::Panel(info) => (state.panel_name == PANE_NAME)
            .then(|| info.get("id")?.as_str().map(str::to_string))
            .flatten()
            .map(Node::Pane),
    }
}

/// 存档的树 → Dock 的布局。树里的格子都在 `members` 里（[`split_layout::restore`] 对过账）；
/// 万一不在就跳过，分屏少了块整组均分。
fn dock_layout(node: &Node, members: &HashMap<String, Member>) -> Option<DockLayout> {
    match node {
        Node::Pane(id) => {
            let view = members.get(id)?.view.clone();
            Some(DockLayout::tabs().panel(view))
        }
        Node::Tabs { panes, active } => {
            let mut layout = DockLayout::tabs();
            let mut kept = 0;
            for id in panes {
                if let Some(member) = members.get(id) {
                    layout = layout.panel(member.view.clone());
                    kept += 1;
                }
            }
            (kept > 0).then(|| layout.active_index((*active).min(kept - 1)))
        }
        Node::Split {
            axis,
            children,
            sizes,
        } => {
            let mut kept: Vec<DockLayout> = children
                .iter()
                .filter_map(|child| dock_layout(child, members))
                .collect();
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
                        let size = sized.then(|| sizes[ix]).filter(|size| *size > 0.).map(px);
                        layout = layout.child(child, size);
                    }
                    Some(layout)
                }
            }
        }
    }
}

// ---- 画法 ----

/// 分屏 Dock 的画法。框用 gpui-base 的光框（底只由窗口铺一层），分隔条、落点、页签条取
/// `smelt_ui::dock`。分屏没有停靠区，也不放大格子。
struct SplitRenderer {
    group_areas: GroupAreas,
}

impl DockAreaRenderer for SplitRenderer {
    fn render_split_handle(
        &self,
        handle: &ResizeHandleContext,
        _: &mut Window,
        _: &mut App,
    ) -> Option<AnyElement> {
        Some(split_handle(handle))
    }

    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(SplitTabGroup {
            group_areas: self.group_areas.clone(),
        })
    }
}

/// 格子组的画法：组顶上那条，和组里显示着的那格。
struct SplitTabGroup {
    group_areas: GroupAreas,
}

impl TabGroupRenderer for SplitTabGroup {
    /// 分了屏才有：组里一格画一条格子标题（拖它把格子挪走），几格叠着画一排页签。只有一格
    /// 的会话什么都不画，格子自己管顶（[`Split::distribute_safe_area`]）。在窗口标题栏上时
    /// 两头给角上的控件让位，让出的那两段和空白拖窗口。
    fn render_tab_bar(&self, group: &TabGroupContext, _: &mut Window, cx: &mut App) -> AnyElement {
        let panes: Vec<(usize, Entity<PaneView>)> = group
            .panels()
            .iter()
            .enumerate()
            .filter(|(_, panel)| panel.visible(cx))
            .filter_map(|(ix, panel)| Some((ix, panel.view().downcast::<PaneView>().ok()?)))
            .collect();
        let area = self
            .group_areas
            .borrow()
            .get(&group.node())
            .copied()
            .unwrap_or_default();
        let bar = match panes.as_slice() {
            [] => return gpui::Empty.into_any_element(),
            [(ix, pane)] => {
                let pane = pane.read(cx);
                if !pane.many {
                    return gpui::Empty.into_any_element();
                }
                pane_title_bar(group, *ix, pane.label(), area.title_bar, cx)
            }
            _ => {
                let tabs = panes
                    .iter()
                    .map(|(ix, pane)| (*ix, pane.read(cx).label()))
                    .collect();
                tab_strip(group, tabs, area.title_bar, cx).into_any_element()
            }
        };
        let insets = area.insets(Adapt::Horizontal);
        let gutter = |id: &'static str, width| {
            let gutter = div().id(id).flex_none().w(width).h_full();
            if area.title_bar {
                with_window_drag(gutter)
            } else {
                gutter
            }
        };
        div()
            .flex()
            .items_center()
            .w_full()
            .h(TOP_BAR_HEIGHT)
            .flex_none()
            .child(gutter("split-bar-leading", insets.left + px(8.)))
            .child(bar)
            .child(gutter("split-bar-trailing", insets.right + px(8.)))
            .into_any_element()
    }

    fn render_active_panel(
        &self,
        panel: AnyView,
        group: &TabGroupContext,
        _: &mut Window,
        _: &mut App,
    ) -> AnyElement {
        active_panel(panel, group)
    }

    fn render_drop_indicator(
        &self,
        indicator: DropIndicator,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        Some(drop_indicator(indicator, window, cx))
    }
}

/// 组里只有一格时的标题：名字是拖动格子的把手，后面的空白收拖过来的格子（叠进这一组），
/// `drag_window` 时空白也拖窗口。
fn pane_title_bar(
    group: &TabGroupContext,
    ix: usize,
    label: TabLabel,
    drag_window: bool,
    cx: &App,
) -> AnyElement {
    let drag = group
        .is_draggable()
        .then(|| group.drag_panel(ix, cx))
        .flatten();
    let title = label.text;
    let handle = div()
        .id("split-pane-title")
        .flex()
        .items_center()
        .min_w_0()
        .h_full()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(label.label)
        .when_some(drag, |this, drag| {
            this.cursor_grab()
                .on_drag(drag, move |drag, offset, _, cx| {
                    drag_preview(drag, offset, title.clone(), cx)
                })
        });
    let target = group.clone();
    let rest = div()
        .id("split-pane-title-rest")
        .flex_1()
        .min_w_0()
        .h_full()
        .when(group.is_droppable(), |this| {
            this.drag_over::<DragPanel>(|this, _, _, _| this.bg(ui_theme::row_selected()))
                .on_drop(move |drag: &DragPanel, window, cx| {
                    target.drop_panel(drag.clone(), None, true, window, cx);
                })
        });
    div()
        .flex()
        .items_center()
        .flex_1()
        .min_w_0()
        .h_full()
        .child(handle)
        .child(if drag_window {
            with_window_drag(rest)
        } else {
            rest
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::split_layout::SplitAxis;
    use smelt_plugin_api::model::workspace_directory::{WorkspaceAcp, WorkspaceTerminal};

    fn pane(id: &str) -> PanelState {
        PanelState {
            info: PanelInfo::panel(serde_json::json!({ "id": id })),
            ..PanelState::new(PANE_NAME)
        }
    }

    fn tabs(active: usize, children: Vec<PanelState>) -> PanelState {
        PanelState {
            panel_name: String::new(),
            children,
            info: PanelInfo::tabs(active),
        }
    }

    fn stack(axis: Axis, sizes: &[f32], children: Vec<PanelState>) -> PanelState {
        PanelState {
            panel_name: String::new(),
            children,
            info: PanelInfo::stack(sizes.iter().copied().map(px).collect(), axis),
        }
    }

    #[test]
    fn the_dock_tree_becomes_the_saved_tree() {
        let dump = stack(
            Axis::Horizontal,
            &[600., 400.],
            vec![
                tabs(0, vec![pane("acp")]),
                tabs(1, vec![pane("t1"), pane("t2")]),
            ],
        );
        assert_eq!(
            node_from_dock(&dump),
            Some(Node::Split {
                axis: SplitAxis::Horizontal,
                children: vec![
                    Node::Pane("acp".into()),
                    Node::Tabs {
                        panes: vec!["t1".into(), "t2".into()],
                        active: 1,
                    },
                ],
                sizes: vec![600., 400.],
            })
        );
    }

    #[test]
    fn foreign_panels_drop_out_and_their_split_collapses() {
        let foreign = PanelState {
            info: PanelInfo::panel(serde_json::json!({ "id": "x" })),
            ..PanelState::new("someone.else")
        };
        let dump = stack(
            Axis::Vertical,
            &[300., 300.],
            vec![tabs(0, vec![pane("t1")]), tabs(0, vec![foreign])],
        );
        assert_eq!(node_from_dock(&dump), Some(Node::Pane("t1".into())));
        assert_eq!(node_from_dock(&tabs(0, Vec::new())), None);
    }

    #[test]
    fn panes_list_the_conversation_first_then_started_terminals() {
        let conversation = WorkspaceConversation {
            id: "c".into(),
            acp: Some(WorkspaceAcp {
                sid: "acp".into(),
                ..Default::default()
            }),
            terminals: vec![
                WorkspaceTerminal {
                    id: Some("t1".into()),
                    cwd: Some("/repo".into()),
                    ..Default::default()
                },
                // 还没起过的不算一格。
                WorkspaceTerminal::default(),
            ],
            ..Default::default()
        };
        let start = ConversationStart {
            fork: None,
            prefill: "hi".into(),
        };
        let specs = pane_specs(&RuntimeKey::Local, &conversation, Some(start));
        let ids: Vec<(&str, PaneKind)> = specs
            .iter()
            .map(|spec| (spec.session_id.as_str(), spec.kind))
            .collect();
        assert_eq!(
            ids,
            vec![("acp", PaneKind::Conversation), ("t1", PaneKind::Terminal)]
        );
        assert_eq!(specs[0].args["start"]["prefill"], "hi");
        assert_eq!(specs[1].args["cwd"], "/repo");
    }

    #[test]
    fn the_drag_band_stays_off_pane_title_bars() {
        // 只有终端：格子在胶囊那条下面，分不分屏都能拖。
        assert!(top_is_free(true, 1));
        assert!(top_is_free(true, 3));
        // 有对话：一格没有标题条，能拖；分屏后顶上是各格的标题条，让给它们。
        assert!(top_is_free(false, 0));
        assert!(top_is_free(false, 1));
        assert!(!top_is_free(false, 2));
    }

    #[test]
    fn pane_titles_fall_back_to_the_kind() {
        let conversation = WorkspaceConversation {
            terminals: vec![WorkspaceTerminal {
                id: Some("t1".into()),
                custom_title: Some("构建".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            pane_title(&conversation, PaneKind::Terminal, "t1", &[]),
            "构建"
        );
        assert_eq!(
            pane_title(&conversation, PaneKind::Terminal, "t2", &[]),
            "终端"
        );
        assert_eq!(
            pane_title(&conversation, PaneKind::Conversation, "acp", &[]),
            "对话"
        );
    }
}
