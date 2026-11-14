//! 产品按钮：五种角色、三档尺寸，全是胶囊。规则见 `docs/ui-design-language.md` 第 4、6、7 节。
//!
//! 建在 gpui-base 的无样式按钮上：焦点、键盘、点击、无障碍归它，样子全在这里画。
//! 不包 gpui-component 的按钮，它的焦点线、悬停和按下规则跟设计语言对不上，
//! 包一层也盖不掉。调用点只选角色和尺寸，不写颜色、高度、圆角。

use std::rc::Rc;

use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, ElementId, FontWeight, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, RenderOnce, Rgba, SharedString, StatefulInteractiveElement,
    StyleRefinement, Styled, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_base::{Button as BaseButton, GlobalState};
use gpui_component::menu::DropdownMenu;
use gpui_component::select::Caret;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Disableable, Icon, Selectable, Sizable as _, Size, StyledExt as _};

use crate::{motion, ui_theme};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type HoverHandler = Box<dyn Fn(&bool, &mut Window, &mut App)>;

/// 按钮在这块界面里的角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Role {
    /// `ink` 实底。发送、提交、允许，一处一个。
    Primary,
    /// `soft` 底。和主操作并列的其它选择、工具栏上要看得见的按钮。
    #[default]
    Secondary,
    /// 透明底、`muted` 字。拒绝、取消、图标按钮、行内操作。
    Quiet,
    /// `bad` 实底。不可撤销操作的确认那一步。
    Danger,
    /// 透明底、`bad` 字。菜单和行里的删除、停止入口。
    DangerQuiet,
}

/// 高度 28 / 32 / 36。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Height {
    Small,
    #[default]
    Medium,
    Large,
}

/// 一种角色在静止、悬停、按下时的底色和字色。悬停、按下都比上一档深一档。
#[derive(Clone, Copy, Debug, PartialEq)]
struct RoleColors {
    bg: Option<Rgba>,
    fg: u32,
    hover_bg: Rgba,
    hover_fg: u32,
    press_bg: Rgba,
}

impl Role {
    fn colors(self) -> RoleColors {
        let solid = |color: u32| rgb(color);
        match self {
            Role::Primary => RoleColors {
                bg: Some(solid(ui_theme::ink())),
                fg: ui_theme::on_ink(),
                hover_bg: solid(ui_theme::ink_hover()),
                hover_fg: ui_theme::on_ink(),
                press_bg: solid(ui_theme::ink_press()),
            },
            Role::Secondary => RoleColors {
                bg: Some(solid(ui_theme::soft())),
                fg: ui_theme::text(),
                hover_bg: solid(ui_theme::soft_hover()),
                hover_fg: ui_theme::text(),
                press_bg: solid(ui_theme::soft_press()),
            },
            Role::Quiet => RoleColors {
                bg: None,
                fg: ui_theme::muted(),
                hover_bg: solid(ui_theme::soft()),
                hover_fg: ui_theme::text(),
                press_bg: solid(ui_theme::soft_hover()),
            },
            // 主题没有危险色的悬停、按下位：往正文色混一点，浅色下变深，深色下变亮，
            // 和 `ink_hover` 的方向一致。
            Role::Danger => RoleColors {
                bg: Some(solid(ui_theme::bad())),
                fg: ui_theme::on_bad(),
                hover_bg: solid(ui_theme::mix(ui_theme::bad(), ui_theme::text(), 0.12)),
                hover_fg: ui_theme::on_bad(),
                press_bg: solid(ui_theme::mix(ui_theme::bad(), ui_theme::text(), 0.24)),
            },
            Role::DangerQuiet => RoleColors {
                bg: None,
                fg: ui_theme::bad(),
                hover_bg: ui_theme::tint(ui_theme::bad(), 0x1f),
                hover_fg: ui_theme::bad(),
                press_bg: ui_theme::tint(ui_theme::bad(), 0x33),
            },
        }
    }
}

/// 一档尺寸落到像素上。
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    height: Pixels,
    padding_x: Pixels,
    text: Pixels,
    icon: Size,
    caret: Size,
}

impl Height {
    fn metrics(self) -> Metrics {
        match self {
            Height::Small => Metrics {
                height: px(28.),
                padding_x: px(12.),
                text: px(13.),
                icon: Size::Small,
                caret: Size::Small,
            },
            Height::Medium => Metrics {
                height: px(32.),
                padding_x: px(14.),
                text: px(14.),
                icon: Size::Medium,
                caret: Size::Medium,
            },
            Height::Large => Metrics {
                height: px(36.),
                padding_x: px(18.),
                text: px(14.),
                icon: Size::Medium,
                caret: Size::Medium,
            },
        }
    }
}

/// 关闭控件（页签、标签上的 ×）：20 见方、圆角 6，比任何一档按钮都小，单独一种。
const DISMISS_BOX: f32 = 20.;
const DISMISS_RADIUS: f32 = 6.;
const GAP: f32 = 6.;
const FOCUS_GAP: f32 = 2.;
const FOCUS_RING: f32 = 2.;
const DISABLED_OPACITY: f32 = 0.4;

#[derive(IntoElement)]
pub struct Button {
    base: BaseButton,
    style: StyleRefinement,
    role: Role,
    height: Height,
    dismiss: bool,
    floating: bool,
    icon: Option<Icon>,
    label: Option<SharedString>,
    children: Vec<AnyElement>,
    accessibility_label: Option<SharedString>,
    tooltip: Option<(
        SharedString,
        Option<(Rc<dyn gpui::Action>, Option<SharedString>)>,
    )>,
    on_click: Option<ClickHandler>,
    on_hover: Option<HoverHandler>,
    dropdown_caret: bool,
    toggled: Option<bool>,
    loading: bool,
    disabled: bool,
    selected: bool,
}

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: BaseButton::new(id),
            style: StyleRefinement::default(),
            role: Role::default(),
            height: Height::default(),
            dismiss: false,
            floating: false,
            icon: None,
            label: None,
            children: Vec::new(),
            accessibility_label: None,
            tooltip: None,
            on_click: None,
            on_hover: None,
            dropdown_caret: false,
            toggled: None,
            loading: false,
            disabled: false,
            selected: false,
        }
    }

    fn role(mut self, role: Role) -> Self {
        self.role = role;
        self.dismiss = false;
        self
    }

    /// 主操作：发送、提交、允许。一处界面最多一个。
    pub fn primary(self) -> Self {
        self.role(Role::Primary)
    }

    /// 次要（默认）：和主操作并列的其它选择、工具栏上要看得见的按钮。
    pub fn secondary(self) -> Self {
        self.role(Role::Secondary)
    }

    /// 安静：拒绝、取消、图标按钮、行内操作。
    pub fn quiet(self) -> Self {
        self.role(Role::Quiet)
    }

    /// 危险实底：只用在不可撤销操作的确认那一步（确认对话框、行内二次确认）。
    pub fn danger(self) -> Self {
        self.role(Role::Danger)
    }

    /// 危险·安静：菜单和行里的删除、停止入口。
    pub fn danger_quiet(self) -> Self {
        self.role(Role::DangerQuiet)
    }

    /// 关闭控件：20 见方、圆角 6、`faint` 字，悬停出 `soft` 底。关的是视图，不是删数据，
    /// 所以不变红。
    pub fn dismiss(mut self) -> Self {
        self.role = Role::Quiet;
        self.dismiss = true;
        self
    }

    /// 浮在内容上（回到最新、预览上的关闭）：`surface` 底加浮起阴影。配次要角色用。
    pub fn floating(mut self) -> Self {
        self.floating = true;
        self
    }

    /// 28 高。工具条、设置行、行内操作。
    pub fn small(mut self) -> Self {
        self.height = Height::Small;
        self
    }

    /// 36 高。对话框底部。
    pub fn large(mut self) -> Self {
        self.height = Height::Large;
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    pub fn on_hover(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_hover = Some(Box::new(handler));
        self
    }

    /// 悬停提示，同时作为读屏的补充说明。
    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some((tooltip.into(), None));
        self
    }

    /// 悬停提示带上动作的快捷键。
    pub fn tooltip_with_action(
        mut self,
        tooltip: impl Into<SharedString>,
        action: &dyn gpui::Action,
        context: Option<&str>,
    ) -> Self {
        let action: Rc<dyn gpui::Action> = Rc::from(action.boxed_clone());
        self.tooltip = Some((
            tooltip.into(),
            Some((action, context.map(SharedString::new))),
        ));
        self
    }

    /// 读屏名称。不写时用 `label`；图标按钮必须写，或用 [`IconAction`](crate::IconAction)。
    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.accessibility_label = Some(label.into());
        self
    }

    /// 加载中：宽度不变，内容换成转圈，不能再点。
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// 末尾的下拉箭头，给打开菜单的按钮用。
    pub fn dropdown_caret(mut self, dropdown_caret: bool) -> Self {
        self.dropdown_caret = dropdown_caret;
        self
    }

    pub fn tab_index(mut self, tab_index: isize) -> Self {
        self.base = self.base.tab_index(tab_index);
        self
    }

    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.base = self.base.tab_stop(tab_stop);
        self
    }

    /// 开关型按钮的读屏状态。看上去的选中仍用 [`Self::selected`]。
    pub fn toggled(mut self, toggled: bool) -> Self {
        self.toggled = Some(toggled);
        self
    }

    pub fn disabled(self, disabled: bool) -> Self {
        Disableable::disabled(self, disabled)
    }

    pub fn selected(self, selected: bool) -> Self {
        Selectable::selected(self, selected)
    }

    fn icon_only(&self) -> bool {
        self.label.is_none() && self.children.is_empty()
    }
}

impl Disableable for Button {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl Selectable for Button {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Button {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl InteractiveElement for Button {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.base.interactivity()
    }
}

impl StatefulInteractiveElement for Button {}

impl DropdownMenu for Button {}

/// 键盘焦点环：2px `bg` 间隙外再套 2px `focus`。环用实色，免得半透明的焦点色
/// 透出底下那圈间隙。
fn focus_ring() -> Vec<BoxShadow> {
    let bg = ui_theme::bg();
    let ring = ui_theme::flatten(ui_theme::focus(), bg);
    let spread = |width: f32, color: u32| {
        BoxShadow::new(px(0.), px(0.), rgb(color).into()).spread_radius(px(width))
    };
    // 后画的在上面：外环先画，间隙压在它里圈。
    vec![spread(FOCUS_GAP + FOCUS_RING, ring), spread(FOCUS_GAP, bg)]
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let icon_only = self.icon_only();
        let Button {
            base,
            style,
            role,
            height,
            dismiss,
            floating,
            icon,
            label,
            children,
            accessibility_label,
            tooltip,
            on_click,
            on_hover,
            dropdown_caret,
            toggled,
            loading,
            disabled,
            selected,
        } = self;
        let metrics = height.metrics();
        let colors = role.colors();
        let interactive = !disabled && !loading;
        let fg = if dismiss {
            ui_theme::faint()
        } else {
            colors.fg
        };
        // 选中只对透明底和 `soft` 底的角色有意义：`soft_hover` 底加正文色。
        let selected = selected && matches!(role, Role::Secondary | Role::Quiet);
        let rest_bg = if selected {
            Some(rgb(ui_theme::soft_hover()))
        } else if floating {
            Some(rgb(ui_theme::surface()))
        } else {
            colors.bg
        };

        let content = div()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(GAP))
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            // 加载时内容只是看不见，仍然占位，按钮宽度不变。
            .when(loading, |this| this.opacity(0.))
            .when_some(icon, |this, icon| {
                let size = if dismiss { Size::Small } else { metrics.icon };
                this.child(icon.with_size(size))
            })
            .when_some(label.clone(), |this, label| {
                this.child(div().min_w_0().text_ellipsis().child(label))
            })
            .children(children)
            .when(dropdown_caret, |this| {
                this.child(Caret::new(metrics.caret).text_color(rgb(fg).into()))
            });

        base.disabled(disabled)
            .selected(selected)
            .styles(|styles| styles.disabled(|style| style.opacity(DISABLED_OPACITY)))
            .relative()
            .flex_none()
            .cursor_default()
            .font_weight(FontWeight::MEDIUM)
            .text_size(metrics.text)
            .text_color(rgb(if selected { ui_theme::text() } else { fg }))
            .map(|this| {
                if dismiss {
                    this.size(px(DISMISS_BOX)).rounded(px(DISMISS_RADIUS))
                } else if icon_only {
                    this.size(metrics.height).rounded_full()
                } else {
                    this.h(metrics.height).px(metrics.padding_x).rounded_full()
                }
            })
            .when_some(rest_bg, |this, bg| this.bg(bg))
            .when(floating, |this| this.shadow(ui_theme::float_shadow()))
            .when(interactive, |this| {
                this.hover(move |style| style.bg(colors.hover_bg).text_color(rgb(colors.hover_fg)))
                    .active(move |style| style.bg(colors.press_bg))
            })
            .focus_visible(|style| style.shadow(focus_ring()))
            .when_some(accessibility_label.or(label), |this, label| {
                this.accessibility_label(label)
            })
            .when_some(
                tooltip.as_ref().map(|(text, _)| text.clone()),
                |this, text| this.aria_description(text),
            )
            .when_some(toggled, |this, toggled| {
                this.aria_toggled(if toggled {
                    gpui::accesskit::Toggled::True
                } else {
                    gpui::accesskit::Toggled::False
                })
            })
            .when(!disabled, |this| {
                this.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    if loading {
                        cx.stop_propagation();
                        return;
                    }
                    // 点按钮不抢焦点，输入框里的光标留在原处；也不开始窗口级的选字。
                    window.prevent_default();
                    GlobalState::suppress_text_selection(cx);
                })
            })
            .map(|this| match on_click {
                Some(on_click) if !loading => {
                    this.on_click(move |event, window, cx| on_click(event, window, cx))
                }
                // 加载中吞掉点击，不让它冒到外层的行上。
                _ if loading => this.on_click(|_, _, cx| cx.stop_propagation()),
                _ => this,
            })
            .when_some(on_hover.filter(|_| interactive), |this, on_hover| {
                this.on_hover(move |hovered, window, cx| on_hover(hovered, window, cx))
            })
            .when_some(tooltip, |this, (text, action)| {
                this.tooltip(move |window, cx| {
                    Tooltip::new(text.clone())
                        .when_some(action.clone(), |tooltip, (action, context)| {
                            tooltip.action(action.as_ref(), context.as_deref())
                        })
                        .build(window, cx)
                })
            })
            .child(content)
            .when(loading, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        // id 挂在按钮自己的元素路径下，同一层多个按钮不会撞。
                        .child(motion::ambient_spinner("button-loading", rgb(fg).into(), true)),
                )
            })
            .refine_style(&style)
    }
}

impl From<Button> for AnyElement {
    fn from(button: Button) -> Self {
        button.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heights_and_padding_follow_the_three_sizes() {
        let sizes: Vec<_> = [Height::Small, Height::Medium, Height::Large]
            .into_iter()
            .map(|height| {
                let m = height.metrics();
                (m.height, m.padding_x)
            })
            .collect();
        assert_eq!(
            sizes,
            vec![(px(28.), px(12.)), (px(32.), px(14.)), (px(36.), px(18.)),]
        );
    }

    #[test]
    fn default_is_secondary_medium() {
        let button = Button::new("b");
        assert_eq!(button.role, Role::Secondary);
        assert_eq!(button.height, Height::Medium);
    }

    #[test]
    fn roles_read_their_palette_slots() {
        let primary = Role::Primary.colors();
        assert_eq!(primary.bg, Some(rgb(ui_theme::ink())));
        assert_eq!(primary.fg, ui_theme::on_ink());
        assert_eq!(primary.hover_bg, rgb(ui_theme::ink_hover()));
        assert_eq!(primary.press_bg, rgb(ui_theme::ink_press()));

        let secondary = Role::Secondary.colors();
        assert_eq!(secondary.bg, Some(rgb(ui_theme::soft())));
        assert_eq!(secondary.hover_bg, rgb(ui_theme::soft_hover()));

        let quiet = Role::Quiet.colors();
        assert_eq!(quiet.bg, None);
        assert_eq!(quiet.fg, ui_theme::muted());
        assert_eq!(quiet.hover_bg, rgb(ui_theme::soft()));

        let danger = Role::Danger.colors();
        assert_eq!(danger.bg, Some(rgb(ui_theme::bad())));
        assert_eq!(danger.fg, ui_theme::on_bad());
        assert_ne!(danger.hover_bg, rgb(ui_theme::bad()));

        let danger_quiet = Role::DangerQuiet.colors();
        assert_eq!(danger_quiet.bg, None);
        assert_eq!(danger_quiet.fg, ui_theme::bad());
    }

    #[test]
    fn later_role_wins_and_clears_dismiss() {
        let button = Button::new("b").dismiss().primary();
        assert_eq!(button.role, Role::Primary);
        assert!(!button.dismiss);
        let button = Button::new("b").danger().dismiss();
        assert_eq!(button.role, Role::Quiet);
        assert!(button.dismiss);
    }

    #[test]
    fn icon_only_means_no_label_and_no_children() {
        assert!(
            Button::new("b")
                .icon(gpui_component::IconName::Close)
                .icon_only()
        );
        assert!(!Button::new("b").label("关闭").icon_only());
        assert!(!Button::new("b").child(div()).icon_only());
    }

    #[test]
    fn focus_ring_is_gap_inside_ring() {
        let shadows = focus_ring();
        assert_eq!(shadows.len(), 2);
        assert_eq!(shadows[0].spread_radius, px(FOCUS_GAP + FOCUS_RING));
        assert_eq!(shadows[1].spread_radius, px(FOCUS_GAP));
        assert_eq!(shadows[1].color, rgb(ui_theme::bg()).into());
    }
}
