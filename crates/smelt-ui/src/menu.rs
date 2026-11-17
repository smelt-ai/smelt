//! 独立菜单。调用方须先初始化 gpui-base，再在应用初始化时调用 [`init`]，并给每个
//! 触发器稳定且唯一的 ID。下拉触发元素自己负责可聚焦和禁用状态。
//!
//! 菜单每次打开时重新构建。输入框的右键菜单由 [`crate::field`] 自己接：点了先把焦点还给输入框再派发动作。

use std::{cell::Cell, rc::Rc, sync::Arc};

use gpui::{
    Anchor, AnyElement, App, Bounds, ClickEvent, Context, ElementId, Entity, FocusHandle,
    InteractiveElement as _, IntoElement, KeyBinding, MouseButton, ObjectFit, ParentElement as _,
    Pixels, Point, RenderOnce, Role, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    StyleRefinement, Styled, StyledImage as _, Subscription, Window, deferred, div, img,
    prelude::FluentBuilder as _, px, rgb, svg,
};
use gpui_base::{
    Align, DeferredPopover, ElementExt as _, GlobalState, Placement, Popup, Positioner,
    StyledExt as _,
};
use gpui_kit_assets::IconNamed as _;

use crate::kit::{GLYPHS, RADIUS, TYPE, elevation};
use crate::ui_theme;

gpui::actions!(smelt_menu, [Up, Down, Left, Right, Activate, Dismiss]);
const MENU_CONTEXT: &str = "SmeltMenu";
/// 紧凑菜单的行高（命令行、分组标题同高）和面板内边距。
const ROW_HEIGHT: f32 = 28.;
const SURFACE_PADDING: f32 = 2.;

/// 安装菜单的键盘绑定。每个 App 初始化一次。
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", Up, Some(MENU_CONTEXT)),
        KeyBinding::new("down", Down, Some(MENU_CONTEXT)),
        KeyBinding::new("left", Left, Some(MENU_CONTEXT)),
        KeyBinding::new("right", Right, Some(MENU_CONTEXT)),
        KeyBinding::new("enter", Activate, Some(MENU_CONTEXT)),
        KeyBinding::new("space", Activate, Some(MENU_CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(MENU_CONTEXT)),
    ]);
}

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type Builder = Rc<dyn Fn(&mut Window, &mut App) -> Menu>;
/// 触发元素，拿到菜单开没开再定样子。
type Trigger = Box<dyn FnOnce(bool) -> AnyElement>;

/// 一条命令。勾选是调用方提供的状态，不会由菜单自行切换。
pub struct MenuItem {
    label: SharedString,
    icon: Option<SharedString>,
    image: Option<Arc<gpui::Image>>,
    checked: Option<bool>,
    disabled: bool,
    danger: bool,
    handler: Option<Handler>,
    submenu: Option<Menu>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
}

impl MenuItem {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            image: None,
            checked: None,
            disabled: false,
            danger: false,
            handler: None,
            submenu: None,
            bounds: Rc::default(),
        }
    }

    pub fn icon(mut self, icon: impl gpui_kit_assets::IconNamed) -> Self {
        self.icon = Some(icon.path());
        self
    }

    /// 也可传入应用自己的 SVG 资源路径。
    pub fn icon_path(mut self, path: impl Into<SharedString>) -> Self {
        self.icon = Some(path.into());
        self
    }

    /// 彩色图标（如应用自己的图标），已经解码好的图片，原色画在图标位上。和线性图标都给时
    /// 画它。
    pub fn image(mut self, image: Arc<gpui::Image>) -> Self {
        self.image = Some(image);
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 删除、停止这类入口：字和图标用危险色。只换颜色，点了照常执行，要不要再确认由
    /// 处理函数自己定。禁用时和别的禁用项一样灰。
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.handler = Some(Rc::new(handler));
        self
    }

    /// 悬停或选择后展示子菜单；Enter/Space/方向键可进入。
    pub fn submenu(mut self, menu: Menu) -> Self {
        self.submenu = Some(menu);
        self
    }
}

enum Entry {
    Item(MenuItem),
    Label(SharedString),
    Separator,
}

/// 菜单内容，按需在触发时构建。
#[derive(Default)]
pub struct Menu {
    entries: Vec<Entry>,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    scroll: ScrollHandle,
    placement: Rc<Cell<Option<Placement>>>,
}

impl Menu {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn item(mut self, item: MenuItem) -> Self {
        self.entries.push(Entry::Item(item));
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.entries.push(Entry::Label(label.into()));
        self
    }

    pub fn separator(mut self) -> Self {
        if !self.entries.is_empty() && !matches!(self.entries.last(), Some(Entry::Separator)) {
            self.entries.push(Entry::Separator);
        }
        self
    }

    fn selectable(&self, index: usize) -> bool {
        matches!(self.entries.get(index), Some(Entry::Item(item)) if !item.disabled)
    }

    fn next(&self, current: Option<usize>, forward: bool) -> Option<usize> {
        let len = self.entries.len();
        if len == 0 {
            return None;
        }
        let start = current.filter(|ix| *ix < len);
        (0..len)
            .map(|step| match (start, forward) {
                (Some(ix), true) => (ix + step + 1) % len,
                (Some(ix), false) => (ix + len - 1 - step) % len,
                (None, true) => step,
                (None, false) => len - 1 - step,
            })
            .find(|ix| self.selectable(*ix))
    }

    fn at_path<'a>(&'a self, path: &[Option<usize>]) -> Option<&'a Menu> {
        let mut menu = self;
        for index in path {
            let Entry::Item(item) = menu.entries.get((*index)?)? else {
                return None;
            };
            if item.disabled {
                return None;
            }
            menu = item.submenu.as_ref()?;
        }
        Some(menu)
    }

    fn contains_open(&self, path: &[Option<usize>], position: Point<Pixels>) -> bool {
        if self.bounds.get().contains(&position) {
            return true;
        }
        let Some(Some(index)) = path.first() else {
            return false;
        };
        match self.entries.get(*index) {
            Some(Entry::Item(item)) if !item.disabled => item
                .submenu
                .as_ref()
                .is_some_and(|menu| menu.contains_open(&path[1..], position)),
            _ => false,
        }
    }
}

struct MenuState {
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    menu: Option<Menu>,
    position: Point<Pixels>,
    trigger_bounds: Rc<Cell<Bounds<Pixels>>>,
    deferred: Option<DeferredPopover>,
    // One selection per level; the last level receives keyboard input.
    selection: Vec<Option<usize>>,
    /// 下拉触发器上按下回车、空格时的焦点，松开时还是它才算一次键盘激活。
    key_press: Option<FocusHandle>,
    _focus_out: Subscription,
}

impl MenuState {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let focus_out = cx.on_focus_out(&focus, window, |state, _, window, cx| {
            state.close(window, cx);
        });
        Self {
            focus,
            previous_focus: None,
            menu: None,
            position: Point::default(),
            trigger_bounds: Rc::default(),
            deferred: None,
            selection: Vec::new(),
            key_press: None,
            _focus_out: focus_out,
        }
    }

    fn open(
        &mut self,
        menu: Menu,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.menu.is_none() || !self.focus.is_focused(window) {
            self.previous_focus = window.focused(cx);
        }
        if menu.entries.is_empty() {
            self.close(window, cx);
            return;
        }
        self.menu = Some(menu);
        if self.deferred.is_none() {
            self.deferred = Some(GlobalState::register_deferred_popover(cx));
        }
        self.position = position;
        self.selection = vec![None];
        self.focus.focus(window, cx);
        cx.notify();
        window.refresh();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_none() {
            return;
        }
        self.selection.clear();
        self.deferred = None;
        // A handler may have focused a dialog or another view; do not steal it.
        if self.focus.is_focused(window) || self.focus.contains_focused(window, cx) {
            if let Some(previous) = self.previous_focus.take() {
                previous.focus(window, cx);
            } else {
                window.blur(cx);
            }
        } else {
            self.previous_focus = None;
        }
        cx.notify();
        window.refresh();
    }

    fn select(&mut self, depth: usize, index: usize, cx: &mut Context<Self>) {
        if depth > self.selection.len() {
            return;
        }
        let Some(menu) = self
            .menu
            .as_ref()
            .and_then(|menu| menu.at_path(&self.selection[..depth]))
        else {
            return;
        };
        if !menu.selectable(index) {
            return;
        }
        menu.scroll.scroll_to_item(index);
        if self.selection.len() == depth {
            self.selection.push(None);
        }
        self.selection.truncate(depth + 1);
        self.selection[depth] = Some(index);
        cx.notify();
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(depth) = self.selection.len().checked_sub(1) else {
            return;
        };
        let next = self
            .menu
            .as_ref()
            .and_then(|menu| menu.at_path(&self.selection[..depth]))
            .and_then(|menu| menu.next(self.selection[depth], forward));
        if let Some(index) = next {
            self.select(depth, index, cx);
        }
    }

    fn enter_submenu(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(menu) = self
            .menu
            .as_ref()
            .and_then(|menu| menu.at_path(&self.selection))
        else {
            return false;
        };
        let first = menu.next(None, true);
        if let Some(index) = first {
            menu.scroll.scroll_to_item(index);
        }
        self.selection.push(first);
        cx.notify();
        true
    }

    fn horizontal(&mut self, right: bool, cx: &mut Context<Self>) {
        let depth = self.selection.len().saturating_sub(1);
        let Some(current) = self
            .menu
            .as_ref()
            .and_then(|menu| menu.at_path(&self.selection[..depth]))
        else {
            return;
        };
        let parent_on_right = current.placement.get() != Some(Placement::Left);
        let child_on_right = self
            .menu
            .as_ref()
            .and_then(|menu| menu.at_path(&self.selection))
            .is_none_or(|menu| menu.placement.get() != Some(Placement::Left));
        if depth > 0 && right != parent_on_right {
            self.selection.pop();
            cx.notify();
        } else if right == child_on_right {
            self.enter_submenu(cx);
        }
    }

    fn handler(&self) -> Option<Handler> {
        let depth = self.selection.len().checked_sub(1)?;
        let menu = self.menu.as_ref()?.at_path(&self.selection[..depth])?;
        match menu.entries.get(self.selection[depth]?)? {
            Entry::Item(item) if !item.disabled && item.submenu.is_none() => item.handler.clone(),
            _ => None,
        }
    }

    fn selected_is_leaf(&self) -> bool {
        let Some(depth) = self.selection.len().checked_sub(1) else {
            return false;
        };
        let Some(menu) = self
            .menu
            .as_ref()
            .and_then(|menu| menu.at_path(&self.selection[..depth]))
        else {
            return false;
        };
        matches!(self.selection[depth].and_then(|ix| menu.entries.get(ix)), Some(Entry::Item(item)) if !item.disabled && item.submenu.is_none())
    }
}

fn activate(state: &Entity<MenuState>, event: &ClickEvent, window: &mut Window, cx: &mut App) {
    let (leaf, handler) = state.update(cx, |state, cx| {
        if state.enter_submenu(cx) {
            (false, None)
        } else {
            (state.selected_is_leaf(), state.handler())
        }
    });
    if leaf {
        if let Some(handler) = handler {
            handler(event, window, cx);
        }
        state.update(cx, |state, cx| state.close(window, cx));
    }
    window.refresh();
}

#[derive(Clone, Copy)]
enum TriggerKind {
    Dropdown(Anchor),
    Context,
}

/// 下拉或右键触发器。它包装触发元素，不改调用者。
#[derive(IntoElement)]
pub struct MenuTrigger {
    id: ElementId,
    trigger: Trigger,
    kind: TriggerKind,
    builder: Builder,
    style: StyleRefinement,
    disabled: bool,
}

impl MenuTrigger {
    /// 下拉菜单。菜单开着时触发器标成打开（[`gpui_base::Selectable::open`]），按钮保持
    /// 按下的样子，看得出弹层是它带出来的。
    pub fn dropdown(
        id: impl Into<ElementId>,
        trigger: impl gpui_base::Selectable + IntoElement + 'static,
        builder: impl Fn(&mut Window, &mut App) -> Menu + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            trigger: Box::new(move |open| trigger.open(open).into_any_element()),
            kind: TriggerKind::Dropdown(Anchor::TopLeft),
            builder: Rc::new(builder),
            style: StyleRefinement::default(),
            disabled: false,
        }
    }

    pub fn context(
        id: impl Into<ElementId>,
        trigger: impl IntoElement,
        builder: impl Fn(&mut Window, &mut App) -> Menu + 'static,
    ) -> Self {
        let trigger = trigger.into_any_element();
        Self {
            id: id.into(),
            trigger: Box::new(move |_| trigger),
            kind: TriggerKind::Context,
            builder: Rc::new(builder),
            style: StyleRefinement::default(),
            disabled: false,
        }
    }

    /// 禁用时不打开菜单。触发元素自己的禁用样子由调用方另外设（如 `Button::disabled`）。
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 下拉菜单相对触发器的锚点，与 `gpui_base::Popup` 一致。
    pub fn anchor(mut self, anchor: Anchor) -> Self {
        if matches!(self.kind, TriggerKind::Dropdown(_)) {
            self.kind = TriggerKind::Dropdown(anchor);
        }
        self
    }
}

impl gpui_base::Disableable for MenuTrigger {
    fn disabled(self, disabled: bool) -> Self {
        MenuTrigger::disabled(self, disabled)
    }
}

/// 样式落在包住触发元素的那层上：触发器要参与父级布局（如 `flex_1`）时在这里写。
impl Styled for MenuTrigger {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for MenuTrigger {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state((self.id.clone(), "menu-state"), cx, |window, cx| {
            MenuState::new(window, cx)
        });
        let disabled = self.disabled;
        // 开着时被禁用（比如正在保存），菜单跟着收起，不留一个点了也没用的菜单。
        let open = state.read(cx).menu.is_some() && !disabled;
        let kind = self.kind;
        let builder = self.builder.clone();
        let trigger_bounds = state.read(cx).trigger_bounds.clone();
        let trigger = div()
            .id((self.id.clone(), "menu-trigger"))
            .relative()
            .refine_style(&self.style)
            .on_prepaint(move |bounds, _, _| trigger_bounds.set(bounds))
            .child((self.trigger)(open));
        let trigger = if disabled {
            trigger
        } else {
            trigger
                .on_mouse_down(
                    match kind {
                        TriggerKind::Dropdown(_) => MouseButton::Left,
                        TriggerKind::Context => MouseButton::Right,
                    },
                    {
                        let state = state.clone();
                        let builder = builder.clone();
                        move |event, window, cx| {
                            cx.stop_propagation();
                            if matches!(kind, TriggerKind::Dropdown(_))
                                && state.read(cx).menu.is_some()
                            {
                                state.update(cx, |state, cx| state.close(window, cx));
                            } else {
                                let menu = builder(window, cx);
                                state.update(cx, |state, cx| {
                                    state.open(menu, event.position, window, cx)
                                });
                            }
                        }
                    },
                )
                // 只有下拉按钮用键盘打开；右键菜单常包着编辑器这类内容，回车、空格是它们的。
                // 照 gpui 原生的键盘点击：按下记下焦点，松开时焦点没挪才开关。触发按钮自己的
                // 键盘点击照常发生；禁用的按钮把激活键拦在自己身上（见 `Button`），菜单不开，
                // 和指针一致。不用按键绑定：绑定先于按键派发，会把按钮的键盘点击吞掉。
                .when(matches!(kind, TriggerKind::Dropdown(_)), |trigger| {
                    trigger
                        .on_key_down({
                            let state = state.clone();
                            move |event, window, cx| {
                                let press = crate::button::is_activation_key(&event.keystroke)
                                    .then(|| window.focused(cx))
                                    .flatten();
                                state.update(cx, |state, _| state.key_press = press);
                            }
                        })
                        .on_key_up({
                            let state = state.clone();
                            move |event, window, cx| {
                                let press = state.update(cx, |state, _| state.key_press.take());
                                if !crate::button::is_activation_key(&event.keystroke)
                                    || press.is_none()
                                    || press != window.focused(cx)
                                {
                                    return;
                                }
                                cx.stop_propagation();
                                if state.read(cx).menu.is_some() {
                                    state.update(cx, |state, cx| state.close(window, cx));
                                } else {
                                    let menu = builder(window, cx);
                                    state.update(cx, |state, cx| {
                                        state.open(menu, Point::default(), window, cx)
                                    });
                                }
                            }
                        })
                })
        };
        let content = open.then(|| {
            render_menu(
                &state,
                0,
                window.viewport_size().height,
                matches!(kind, TriggerKind::Dropdown(_)),
                cx,
            )
        });
        match kind {
            TriggerKind::Dropdown(anchor) => {
                // 和子菜单离父菜单一样，离触发器 4px，不贴着。
                let popup = Popup::new(self.id, trigger).anchor(anchor).offset(px(4.));
                match content {
                    Some(content) => popup.content(content).into_any_element(),
                    None => popup.into_any_element(),
                }
            }
            // 左上角落在按下的地方；下面放不下就翻到上面，左右放不下往里推（以按下的点为
            // 锚、BottomStart）。
            TriggerKind::Context => trigger
                .children(content.map(|content| {
                    let at = Bounds::new(state.read(cx).position, Default::default());
                    deferred(
                        Positioner::side(at)
                            .placement(Placement::Bottom)
                            .align(Align::Start)
                            .offset(px(0.))
                            .margin(px(8.))
                            .occlude()
                            .child(content),
                    )
                    .with_priority(gpui_base::POPUP_PRIORITY)
                }))
                .into_any_element(),
        }
    }
}

fn render_menu(
    state: &Entity<MenuState>,
    depth: usize,
    viewport_height: Pixels,
    dropdown: bool,
    cx: &App,
) -> AnyElement {
    let data = state.read(cx);
    let menu = data
        .menu
        .as_ref()
        .and_then(|menu| menu.at_path(&data.selection[..depth]))
        .expect("open menu path");
    let selected = data.selection.get(depth).copied().flatten();
    let focus = data.focus.clone();
    let bounds = menu.bounds.clone();
    let scroll = menu.scroll.clone();
    let has_icon = menu.entries.iter().any(|entry| {
        matches!(entry, Entry::Item(item)
            if item.icon.is_some() || item.image.is_some() || item.checked == Some(true))
    });
    let mut rows = Vec::new();
    for (index, entry) in menu.entries.iter().enumerate() {
        match entry {
            // 分隔线上下各 2，横向顶到面板边。
            Entry::Separator if index + 1 < menu.entries.len() => rows.push(
                div()
                    .h(px(1.))
                    .my(px(2.))
                    .mx(px(-SURFACE_PADDING))
                    .bg(ui_theme::hair())
                    .into_any_element(),
            ),
            Entry::Separator => {}
            Entry::Label(label) => rows.push(
                TYPE.small_medium
                    .apply(div())
                    .h(px(ROW_HEIGHT))
                    .flex()
                    .items_center()
                    .px(px(12.))
                    .text_color(rgb(ui_theme::faint()))
                    .child(label.clone())
                    .into_any_element(),
            ),
            Entry::Item(item) => {
                let is_selected = selected == Some(index);
                // svg 不继承文字颜色，图标要显式给同一个色。
                let foreground = rgb(if item.disabled {
                    ui_theme::muted()
                } else if item.danger {
                    // 和危险·安静按钮一样，按这一行此刻的底配到 4.5:1。
                    let behind = if is_selected {
                        ui_theme::soft_hover()
                    } else {
                        ui_theme::soft()
                    };
                    ui_theme::legible(ui_theme::bad(), behind, 4.5)
                } else {
                    ui_theme::text()
                });
                let mut row = div()
                    .id(index)
                    .relative()
                    .role(if item.checked.is_some() {
                        Role::MenuItemCheckBox
                    } else {
                        Role::MenuItem
                    })
                    .debug_selector(move || format!("smelt-menu-item-{depth}-{index}").into())
                    .aria_label(item.label.clone())
                    .aria_selected(is_selected)
                    .when_some(item.checked, |row, checked| {
                        row.aria_toggled(if checked {
                            gpui::accesskit::Toggled::True
                        } else {
                            gpui::accesskit::Toggled::False
                        })
                    })
                    .h(px(ROW_HEIGHT))
                    .pl(px(12.))
                    .pr(px(6.))
                    .rounded(px(RADIUS.sm))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_color(foreground)
                    .when(is_selected && !item.disabled, |row| {
                        row.bg(rgb(ui_theme::soft_hover()))
                    })
                    .on_prepaint({
                        let bounds = item.bounds.clone();
                        let submenu_open = is_selected && !item.disabled && item.submenu.is_some();
                        move |value, window, _| {
                            if bounds.get() != value {
                                bounds.set(value);
                                // Positioner uses the previous measured row bounds. Follow a
                                // scrolled or moved row on the next frame, only when it moved.
                                if submenu_open {
                                    window.request_animation_frame();
                                }
                            }
                        }
                    });
                if has_icon {
                    let glyph = match (item.checked == Some(true), &item.image) {
                        (true, _) => Some(svg_glyph(GLYPHS.check.path(), foreground)),
                        (false, Some(image)) => Some(
                            img(image.clone())
                                .size(px(16.))
                                .object_fit(ObjectFit::Contain)
                                .when(item.disabled, |image| image.opacity(0.5))
                                .into_any_element(),
                        ),
                        (false, None) => item.icon.clone().map(|path| svg_glyph(path, foreground)),
                    };
                    row = row.child(
                        div()
                            .w(px(16.))
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .children(glyph),
                    );
                }
                row = row.child(div().flex_1().child(item.label.clone()));
                if item.submenu.is_some() {
                    row = row.child(
                        svg()
                            .path(GLYPHS.chevron_right.path())
                            .size(px(16.))
                            .flex_shrink_0()
                            .text_color(foreground),
                    );
                }
                if !item.disabled {
                    row = row
                        .on_hover({
                            let state = state.clone();
                            move |hovered, window, cx| {
                                if *hovered {
                                    state.update(cx, |state, cx| state.select(depth, index, cx));
                                    window.refresh();
                                }
                            }
                        })
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click({
                            let state = state.clone();
                            move |event, window, cx| {
                                cx.stop_propagation();
                                state.update(cx, |state, cx| state.select(depth, index, cx));
                                activate(&state, event, window, cx);
                            }
                        });
                }
                if is_selected && !item.disabled {
                    if let Some(submenu) = &item.submenu {
                        let child_bounds = item.bounds.get();
                        let placement = submenu.placement.clone();
                        let child = render_menu(state, depth + 1, viewport_height, dropdown, cx);
                        row = row.child(
                            deferred(
                                Positioner::side(child_bounds)
                                    .placement(Placement::Right)
                                    .align(Align::Start)
                                    .margin(px(0.))
                                    .occlude()
                                    .on_position(move |position| placement.set(position.placement))
                                    .child(child),
                            )
                            .with_priority(gpui_base::POPUP_PRIORITY + depth + 1),
                        );
                    }
                }
                rows.push(row.into_any_element());
            }
        }
    }
    let max_height = (viewport_height * 0.5).min(px(450.)).max(px(0.));
    let surface = div()
        .id(("menu", depth))
        .role(Role::Menu)
        .debug_selector(move || format!("smelt-menu-{depth}").into())
        .on_prepaint(move |value, _, _| bounds.set(value))
        .relative()
        .occlude()
        .p(px(SURFACE_PADDING))
        .min_w(px(160.))
        .max_w(px(500.))
        .max_h(max_height)
        .overflow_y_scroll()
        .track_scroll(&scroll)
        .flex()
        .flex_col()
        .rounded(px(RADIUS.md))
        // 右键、下拉这类紧凑菜单用第一层底，比带边框的弹层（`surface`）更跳出窗口底。
        .bg(rgb(ui_theme::soft()))
        .map(|surface| TYPE.ui_compact.apply(surface))
        // 细圈在浮起层级的投影里，不再另画边框。
        .shadow(elevation::floating())
        .children(rows);
    if depth != 0 {
        return surface.into_any_element();
    }
    surface
        .key_context(MENU_CONTEXT)
        .track_focus(&focus)
        .on_action({
            let state = state.clone();
            move |_: &Up, window, cx| {
                cx.stop_propagation();
                state.update(cx, |state, cx| state.step(false, cx));
                window.refresh();
            }
        })
        .on_action({
            let state = state.clone();
            move |_: &Down, window, cx| {
                cx.stop_propagation();
                state.update(cx, |state, cx| state.step(true, cx));
                window.refresh();
            }
        })
        .on_action({
            let state = state.clone();
            move |_: &Right, window, cx| {
                cx.stop_propagation();
                state.update(cx, |state, cx| state.horizontal(true, cx));
                window.refresh();
            }
        })
        .on_action({
            let state = state.clone();
            move |_: &Left, window, cx| {
                cx.stop_propagation();
                state.update(cx, |state, cx| state.horizontal(false, cx));
                window.refresh();
            }
        })
        .on_action({
            let state = state.clone();
            move |_: &Activate, window, cx| {
                cx.stop_propagation();
                activate(&state, &ClickEvent::default(), window, cx);
            }
        })
        .on_action({
            let state = state.clone();
            move |_: &Dismiss, window, cx| {
                cx.stop_propagation();
                state.update(cx, |state, cx| state.close(window, cx));
            }
        })
        .on_mouse_down_out({
            let state = state.clone();
            move |event, window, cx| {
                let data = state.read(cx);
                let inside = data
                    .menu
                    .as_ref()
                    .is_some_and(|menu| menu.contains_open(&data.selection, event.position));
                let on_trigger = dropdown && data.trigger_bounds.get().contains(&event.position);
                if !inside && !on_trigger {
                    state.update(cx, |state, cx| state.close(window, cx));
                }
            }
        })
        .into_any_element()
}

/// 图标位上的线性图标。svg 不继承文字颜色，要显式给同一个色。
fn svg_glyph(path: SharedString, color: gpui::Rgba) -> AnyElement {
    svg()
        .path(path)
        .size(px(14.))
        .text_color(color)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{KeyDownEvent, KeyUpEvent, Keystroke, VisualTestContext};

    /// 按下再松开。下拉触发器和 gpui 的键盘点击一样，松开时才生效；`simulate_keystrokes`
    /// 只发按下。
    fn press(window: &mut VisualTestContext, key: &str) {
        let keystroke =
            window.update(|_, cx| Keystroke::parse_with_os(key, cx.platform_info().os).unwrap());
        window.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        window.simulate_event(KeyUpEvent { keystroke });
    }

    fn sample() -> Menu {
        Menu::new()
            .label("Group")
            .item(MenuItem::new("Disabled").disabled(true))
            .separator()
            .item(MenuItem::new("First"))
            .item(MenuItem::new("Second").checked(true))
    }

    #[test]
    fn navigation_skips_non_items_and_wraps() {
        let menu = sample();
        assert_eq!(menu.next(None, true), Some(3));
        assert_eq!(menu.next(None, false), Some(4));
        assert_eq!(menu.next(Some(3), true), Some(4));
        assert_eq!(menu.next(Some(4), true), Some(3));
        assert_eq!(menu.next(Some(3), false), Some(4));
        assert_eq!(
            Menu::new()
                .item(MenuItem::new("No").disabled(true))
                .next(None, true),
            None
        );
    }

    #[test]
    fn submenu_path_rejects_disabled_and_non_submenus() {
        let menu = Menu::new()
            .item(MenuItem::new("More").submenu(sample()))
            .item(MenuItem::new("Disabled").disabled(true).submenu(sample()));
        assert!(menu.at_path(&[Some(0)]).is_some());
        assert!(menu.at_path(&[Some(1)]).is_none());
        assert!(menu.at_path(&[None]).is_none());
        assert!(menu.at_path(&[Some(0), Some(3)]).is_none());
    }

    #[test]
    fn separators_do_not_duplicate_or_start_a_menu() {
        let menu = Menu::new()
            .separator()
            .item(MenuItem::new("A"))
            .separator()
            .separator();
        assert_eq!(menu.entries.len(), 2);
    }

    /// 下拉触发器要能标成打开；测试里的普通 div 不在乎这个状态。
    #[derive(IntoElement)]
    struct TriggerProbe(gpui::Stateful<gpui::Div>);

    impl RenderOnce for TriggerProbe {
        fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
            self.0
        }
    }

    impl gpui_base::Selectable for TriggerProbe {
        fn selected(self, _: bool) -> Self {
            self
        }

        fn is_selected(&self) -> bool {
            false
        }
    }

    struct Harness {
        focus: FocusHandle,
        calls: Rc<Cell<usize>>,
        context: bool,
    }

    impl gpui::Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let trigger = TriggerProbe(
                div()
                    .id("trigger")
                    .track_focus(&self.focus)
                    .w(px(120.))
                    .h(px(30.))
                    .child("Open"),
            );
            let calls = self.calls.clone();
            let build = move |_: &mut Window, _: &mut App| {
                let calls = calls.clone();
                Menu::new()
                    .label("Group")
                    .item(MenuItem::new("Disabled").disabled(true))
                    .item(MenuItem::new("More").submenu(Menu::new().item(
                        MenuItem::new("Run").on_click(move |_, _, _| calls.set(calls.get() + 1)),
                    )))
            };
            div().size_full().child(if self.context {
                MenuTrigger::context("test-menu", trigger, build)
            } else {
                MenuTrigger::dropdown("test-menu", trigger, build)
            })
        }
    }

    /// A focus transfer which is not a pointer press must also dismiss the menu.
    #[gpui::test]
    fn focus_leaving_menu_dismisses_it_without_stealing_focus(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            init(cx);
        });
        let (view, window) = cx.add_window_view(|_, cx| Harness {
            focus: cx.focus_handle(),
            calls: Rc::new(Cell::new(0)),
            context: true,
        });
        // 测试窗口默认没激活，没激活的窗口不派发焦点离开。
        window.update(|window, _| window.activate_window());
        window.run_until_parked();
        window.update(|window, cx| window.draw(cx).clear(cx));
        window.simulate_mouse_down(
            gpui::point(px(20.), px(10.)),
            MouseButton::Right,
            Default::default(),
        );
        window.update(|window, cx| window.draw(cx).clear(cx));
        assert!(window.debug_bounds("smelt-menu-0").is_some());
        window.update(|window, cx| {
            let focus = view.read(cx).focus.clone();
            focus.focus(window, cx);
        });
        window.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert!(view.read(cx).focus.is_focused(window));
        });
        assert!(window.debug_bounds("smelt-menu-0").is_none());
    }

    struct DisabledTriggerHarness {
        disabled: bool,
    }

    impl gpui::Render for DisabledTriggerHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().tab_group().child(MenuTrigger::dropdown(
                "disabled-menu",
                crate::Button::new("disabled-trigger")
                    .label("Open")
                    .disabled(self.disabled),
                |_, _| Menu::new().item(MenuItem::new("Run")),
            ))
        }
    }

    /// The button can retain focus when it becomes disabled. Enter and Space
    /// must not activate a menu listener on an ancestor of that button.
    #[gpui::test]
    fn focused_trigger_disabled_after_render_cannot_open_menu(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            init(cx);
        });
        let (view, window) = cx.add_window_view(|_, _| DisabledTriggerHarness { disabled: false });
        window.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.focus_next(cx);
            assert!(window.focused(cx).is_some());
            window.draw(cx).clear(cx);
        });
        view.update(window, |view, cx| {
            view.disabled = true;
            cx.notify();
        });
        window.update(|window, cx| window.draw(cx).clear(cx));
        for key in ["enter", "space"] {
            press(window, key);
            window.update(|window, cx| window.draw(cx).clear(cx));
            assert!(window.debug_bounds("smelt-menu-0").is_none(), "{key}");
        }
    }

    #[gpui::test]
    fn dropdown_keyboard_enters_submenu_activates_and_restores_focus(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            init(cx);
        });
        let calls = Rc::new(Cell::new(0));
        let (view, window) = cx.add_window_view({
            let calls = calls.clone();
            move |_, cx| Harness {
                focus: cx.focus_handle(),
                calls,
                context: false,
            }
        });
        window.update(|window, cx| {
            window.draw(cx).clear(cx);
            let focus = view.read(cx).focus.clone();
            focus.focus(window, cx);
        });
        press(window, "enter");
        window.update(|window, cx| window.draw(cx).clear(cx));
        window.simulate_keystrokes("down");
        window.update(|window, cx| window.draw(cx).clear(cx));
        assert!(window.debug_bounds("smelt-menu-1").is_some());
        window.simulate_keystrokes("right");
        window.simulate_keystrokes("space");
        window.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert!(view.read(cx).focus.is_focused(window));
        });
        assert_eq!(calls.get(), 1);
        assert!(window.debug_bounds("smelt-menu-0").is_none());
    }

    struct ActivationProbe {
        builds: Rc<Cell<usize>>,
        clicks: Rc<Cell<usize>>,
    }

    impl gpui::Render for ActivationProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let builds = self.builds.clone();
            let clicks = self.clicks.clone();
            div().size_full().tab_group().child(MenuTrigger::dropdown(
                "trigger-probe",
                crate::Button::new("probe-button")
                    .label("Open")
                    .on_click(move |_, _, _| clicks.set(clicks.get() + 1)),
                move |_, _| {
                    builds.set(builds.get() + 1);
                    Menu::new().item(MenuItem::new("Run"))
                },
            ))
        }
    }

    #[gpui::test]
    fn trigger_pointer_and_keyboard_activate_once(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            init(cx);
        });
        let builds = Rc::new(Cell::new(0));
        let clicks = Rc::new(Cell::new(0));
        let (_, window) = cx.add_window_view({
            let builds = builds.clone();
            let clicks = clicks.clone();
            move |_, _| ActivationProbe { builds, clicks }
        });
        window.update(|window, cx| window.draw(cx).clear(cx));
        window.simulate_click(gpui::point(px(10.), px(10.)), Default::default());
        window.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(builds.get(), 1);
        assert!(window.debug_bounds("smelt-menu-0").is_some());
        assert_eq!(clicks.get(), 1, "the pointer must activate the button once");
        window.simulate_keystrokes("escape");
        window.update(|window, cx| {
            window.draw(cx).clear(cx);
            window.focus_next(cx);
            window.draw(cx).clear(cx);
        });
        press(window, "enter");
        window.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(builds.get(), 2);
        assert!(window.debug_bounds("smelt-menu-0").is_some());
        assert_eq!(
            clicks.get(),
            2,
            "the keyboard must activate the button once"
        );
    }

    struct FlipProbe {
        calls: Rc<Cell<usize>>,
    }

    impl gpui::Render for FlipProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let calls = self.calls.clone();
            MenuTrigger::context("flip-probe", div().size_full(), move |_, _| {
                let calls = calls.clone();
                Menu::new().item(MenuItem::new("More").submenu(Menu::new().item(
                    MenuItem::new("Run").on_click(move |_, _, _| {
                        calls.set(calls.get() + 1);
                    }),
                )))
            })
        }
    }

    #[gpui::test]
    fn submenu_navigation_follows_resolved_side(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            init(cx);
        });
        for (x, flipped) in [(20., false), (470., true)] {
            let calls = Rc::new(Cell::new(0));
            let (_, window) = cx.add_window_view({
                let calls = calls.clone();
                move |_, _| FlipProbe { calls }
            });
            window.simulate_resize(gpui::size(px(500.), px(300.)));
            window.update(|window, cx| window.draw(cx).clear(cx));
            window.simulate_mouse_down(
                gpui::point(px(x), px(30.)),
                MouseButton::Right,
                Default::default(),
            );
            window.update(|window, cx| window.draw(cx).clear(cx));
            window.simulate_keystrokes("down");
            window.update(|window, cx| window.draw(cx).clear(cx));
            window.update(|window, cx| window.draw(cx).clear(cx));
            let parent = window.debug_bounds("smelt-menu-item-0-0").unwrap();
            let child = window.debug_bounds("smelt-menu-1").unwrap();
            assert_eq!(child.center().x < parent.center().x, flipped);
            window.simulate_keystrokes(if flipped { "right" } else { "left" });
            window.simulate_keystrokes("enter");
            window.update(|window, cx| window.draw(cx).clear(cx));
            assert_eq!(calls.get(), 0, "wrong direction must not enter the submenu");
            // Enter above entered the submenu. Return to the parent and enter
            // using the direction in which the submenu was actually placed.
            window.simulate_keystrokes(if flipped { "right" } else { "left" });
            window.simulate_keystrokes(if flipped { "left" } else { "right" });
            window.simulate_keystrokes("enter");
            window.update(|window, cx| window.draw(cx).clear(cx));
            assert_eq!(calls.get(), 1);
        }
    }

    /// 右键菜单的左上角落在按下的地方；下面放不下就翻到按下处的上面，不盖住它。
    #[gpui::test]
    fn context_menu_opens_at_pointer_and_flips_above_near_the_bottom(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            init(cx);
        });
        for y in [30., 280.] {
            let (_, window) = cx.add_window_view(|_, _| FlipProbe {
                calls: Rc::default(),
            });
            window.simulate_resize(gpui::size(px(500.), px(300.)));
            window.update(|window, cx| window.draw(cx).clear(cx));
            let at = gpui::point(px(40.), px(y));
            window.simulate_mouse_down(at, MouseButton::Right, Default::default());
            window.update(|window, cx| window.draw(cx).clear(cx));
            let menu = window.debug_bounds("smelt-menu-0").unwrap();
            assert_eq!(menu.left(), at.x);
            if y < 100. {
                assert_eq!(menu.top(), at.y);
            } else {
                assert_eq!(menu.bottom(), at.y);
            }
        }
    }

    #[gpui::test]
    fn pointer_opens_submenu_and_clicks_its_item(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_base::init(cx);
            init(cx);
        });
        let calls = Rc::new(Cell::new(0));
        let (_, window) = cx.add_window_view({
            let calls = calls.clone();
            move |_, cx| Harness {
                focus: cx.focus_handle(),
                calls,
                context: false,
            }
        });
        window.update(|window, cx| window.draw(cx).clear(cx));
        window.simulate_mouse_down(
            gpui::point(px(20.), px(10.)),
            MouseButton::Left,
            Default::default(),
        );
        window.update(|window, cx| window.draw(cx).clear(cx));
        let parent = window.debug_bounds("smelt-menu-item-0-2").unwrap().center();
        window.simulate_mouse_move(parent, None, Default::default());
        window.update(|window, cx| window.draw(cx).clear(cx));
        let child = window.debug_bounds("smelt-menu-item-1-0").unwrap().center();
        window.simulate_mouse_move(child, None, Default::default());
        window.simulate_click(child, Default::default());
        window.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(calls.get(), 1);
        assert!(window.debug_bounds("smelt-menu-0").is_none());
    }

    #[gpui::test]
    fn context_menu_outside_and_escape_close_without_running_commands(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            init(cx);
        });
        let calls = Rc::new(Cell::new(0));
        let (_, window) = cx.add_window_view({
            let calls = calls.clone();
            move |_, cx| Harness {
                focus: cx.focus_handle(),
                calls,
                context: true,
            }
        });
        window.update(|window, cx| window.draw(cx).clear(cx));
        window.simulate_mouse_down(
            gpui::point(px(20.), px(10.)),
            MouseButton::Right,
            Default::default(),
        );
        window.update(|window, cx| window.draw(cx).clear(cx));
        assert!(window.debug_bounds("smelt-menu-0").is_some());
        window.simulate_keystrokes("escape");
        window.update(|window, cx| window.draw(cx).clear(cx));
        assert!(window.debug_bounds("smelt-menu-0").is_none());
        window.simulate_mouse_down(
            gpui::point(px(20.), px(10.)),
            MouseButton::Right,
            Default::default(),
        );
        window.update(|window, cx| window.draw(cx).clear(cx));
        window.simulate_mouse_down(
            gpui::point(px(500.), px(400.)),
            MouseButton::Left,
            Default::default(),
        );
        window.update(|window, cx| window.draw(cx).clear(cx));
        assert!(window.debug_bounds("smelt-menu-0").is_none());
        assert_eq!(calls.get(), 0);
    }
}
