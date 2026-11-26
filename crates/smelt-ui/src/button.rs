//! 产品按钮：五种角色，五档尺寸 [`ControlSize`]；度量、字、投影都从 [`crate::kit`] 取。
//!
//! 建在 gpui-base 的无样式按钮上：焦点、键盘、点击、无障碍归它，样子全在这里画。
//! 不包 gpui-component 的按钮，它的焦点线、悬停和按下规则跟设计语言对不上，
//! 包一层也盖不掉。调用点只选角色和尺寸，不写颜色、高度、圆角。

use std::rc::Rc;

use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, ElementId, Hsla, InteractiveElement, IntoElement,
    MouseButton, ParentElement, RenderOnce, Rgba, SharedString, StatefulInteractiveElement,
    StyleRefinement, Styled, Window, div, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, px, rems, rgb, svg,
};
use gpui_base::{Button as BaseButton, Disableable, GlobalState, Selectable, StyledExt as _};

pub use crate::kit::ControlSize;
use crate::kit::{
    RADIUS, TYPE, TextStyle,
    elevation::{self, Elevation},
};
use crate::{IconNamed, motion, tooltip::Tooltip, ui_theme};

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type HoverHandler = Box<dyn Fn(&bool, &mut Window, &mut App)>;

/// 按钮在这块界面里的角色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Role {
    /// 按钮 contrast 底。发送、提交、允许，一处一个。
    Primary,
    /// `soft` 底。和主操作并列的其它选择、工具栏上要看得见的按钮。
    #[default]
    Secondary,
    /// 透明底、`muted` 字。拒绝、取消、图标按钮、行内操作。
    Quiet,
    /// `soft` 底、`bad` 字。不可撤销操作的确认那一步。
    Danger,
    /// 透明底、`bad` 字。菜单和行里的删除、停止入口。
    DangerQuiet,
}

/// 尺寸。`small()`、默认、`large()` 是三档简写，落到
/// [`ControlSize`]：文字按钮 小→Regular（24）、默认→Large（28）、大→ExtraLarge（36）；
/// 图标按钮 小→Small（20）、默认→Regular（24）、大→Large（28）。`control_size` 直接给档。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Height {
    Small,
    #[default]
    Medium,
    Large,
    Exact(ControlSize),
}

impl Height {
    fn control_size(self, icon_only: bool) -> ControlSize {
        match (self, icon_only) {
            (Height::Exact(size), _) => size,
            (Height::Small, false) => ControlSize::Regular,
            (Height::Medium, false) => ControlSize::Large,
            (Height::Large, false) => ControlSize::ExtraLarge,
            (Height::Small, true) => ControlSize::Small,
            (Height::Medium, true) => ControlSize::Regular,
            (Height::Large, true) => ControlSize::Large,
        }
    }
}

/// 一种角色在静止、悬停、按下时的底色和字色。悬停、按下都比上一档深一档。
#[derive(Clone, Copy, Debug, PartialEq)]
struct RoleColors {
    bg: Option<Rgba>,
    fg: u32,
    hover_bg: Rgba,
    hover_fg: u32,
    press_bg: Rgba,
    press_fg: u32,
}

impl Role {
    fn disabled_opacity(self) -> f32 {
        if self == Self::Primary { 0.4 } else { 0.5 }
    }

    /// 只有图标的主按钮（发送、提交）用图标主按钮那套底和图标色，带字的用主按钮那套。
    fn colors_from(self, palette: ui_theme::Palette, icon_only: bool) -> RoleColors {
        let danger_text = |background| ui_theme::legible(palette.bad, background, 4.5);
        let neutral = palette.button_secondary;
        // 透明底的按钮悬停、按下叠一层正文色，和列表行同一套（压在窗口底上保底看得出）。
        let (quiet_hover, quiet_press) = (palette.row_hover, palette.row_selected);
        let on_bg = |fill: Rgba| ui_theme::flatten(fill, palette.bg);
        let (primary, primary_fg) = if icon_only {
            (palette.button_submit, palette.button_submit_foreground)
        } else {
            (palette.button_primary, palette.button_primary_foreground)
        };
        match self {
            Role::Primary => RoleColors {
                bg: Some(primary[0]),
                fg: primary_fg,
                hover_bg: primary[1],
                hover_fg: primary_fg,
                press_bg: primary[2],
                press_fg: primary_fg,
            },
            Role::Secondary => RoleColors {
                bg: Some(neutral[0]),
                fg: palette.button_secondary_foreground,
                hover_bg: neutral[1],
                hover_fg: palette.button_secondary_foreground,
                press_bg: neutral[2],
                press_fg: palette.button_secondary_foreground,
            },
            Role::Quiet => RoleColors {
                bg: None,
                fg: palette.muted,
                hover_bg: quiet_hover,
                hover_fg: palette.text,
                press_bg: quiet_press,
                press_fg: palette.text,
            },
            // 危险按钮使用中性底和危险色文字。半透明底只能按主题窗口底估算；
            // 实际父背景可能不同，不能保证对比度。
            Role::Danger => RoleColors {
                bg: Some(neutral[0]),
                fg: danger_text(on_bg(neutral[0])),
                hover_bg: neutral[1],
                hover_fg: danger_text(on_bg(neutral[1])),
                press_bg: neutral[2],
                press_fg: danger_text(on_bg(neutral[2])),
            },
            Role::DangerQuiet => RoleColors {
                bg: None,
                fg: danger_text(palette.bg),
                hover_bg: quiet_hover,
                hover_fg: danger_text(on_bg(quiet_hover)),
                press_bg: quiet_press,
                press_fg: danger_text(on_bg(quiet_press)),
            },
        }
    }
}

/// 一档尺寸落到像素上：文字按钮和图标按钮各一套。
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    /// 文字按钮：高、左右内边距、圆角、图标、图文间距、字。
    height: f32,
    padding_x: f32,
    radius: f32,
    icon: f32,
    gap: f32,
    text: TextStyle,
    /// 图标按钮：边长、圆角、图标。
    icon_button_size: f32,
    icon_button_radius: f32,
    icon_button_glyph: f32,
}

fn metrics(size: ControlSize) -> Metrics {
    let (height, padding_x, radius, icon) = match size {
        ControlSize::Mini => (16., 8., RADIUS.sm, 12.),
        ControlSize::Small => (20., 10., RADIUS.sm, 14.),
        ControlSize::Regular => (24., 12., RADIUS.md, 16.),
        ControlSize::Large => (28., 14., RADIUS.lg, 16.),
        ControlSize::ExtraLarge => (36., 18., RADIUS.xl, 16.),
    };
    let (icon_button_size, icon_button_radius, icon_button_glyph) = match size {
        ControlSize::Mini => (16., RADIUS.sm, 12.),
        ControlSize::Small => (20., RADIUS.sm, 16.),
        ControlSize::Regular => (24., RADIUS.md, 16.),
        ControlSize::Large => (28., RADIUS.md, 16.),
        ControlSize::ExtraLarge => (36., RADIUS.lg, 18.),
    };
    let text = match size {
        ControlSize::Mini => TextStyle {
            size: 9.,
            ..TYPE.small_medium
        },
        ControlSize::Small => TYPE.small_medium,
        _ => TYPE.ui_medium,
    };
    Metrics {
        height,
        padding_x,
        radius,
        icon,
        gap: if size == ControlSize::Mini { 4. } else { 6. },
        text,
        icon_button_size,
        icon_button_radius,
        icon_button_glyph,
    }
}

/// 关闭控件（页签、标签上的 ×）：20 见方、圆角 6，比任何一档按钮都小，单独一种。
/// 对话框的关闭钮同样是 Regular 图标按钮收到 20。
const DISMISS_BOX: f32 = 20.;
const DISMISS_RADIUS: f32 = RADIUS.md;
const FOCUS_GAP: f32 = 2.;
const FOCUS_RING: f32 = 2.;
pub(crate) const DISABLED_OPACITY: f32 = 0.4;

#[derive(IntoElement)]
pub struct Button {
    base: BaseButton,
    style: StyleRefinement,
    role: Role,
    height: Height,
    dismiss: bool,
    floating: bool,
    icon: Option<SharedString>,
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
    /// 挂着的菜单或弹层开着。和选中画得一样，但不改读屏的选中状态。
    open: bool,
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
            open: false,
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

    /// 危险：只用在不可撤销操作的确认那一步（确认对话框、行内二次确认）。
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

    /// 浮在内容上（回到最新、预览上的关闭）：`surface` 底；各状态不透底时加浮起阴影。配次要角色用。
    pub fn floating(mut self) -> Self {
        self.floating = true;
        self
    }

    /// 紧凑：文字按钮 24 高（Regular），图标按钮 20 高（Small）。
    pub fn small(mut self) -> Self {
        self.height = Height::Small;
        self
    }

    /// 加大：文字按钮 36 高（ExtraLarge），图标按钮 28 高（Large）。对话框动作是
    /// 默认档 Regular（见 [`crate::dialog`] 的确认框），新代码直接用 `control_size`。
    pub fn large(mut self) -> Self {
        self.height = Height::Large;
        self
    }

    /// 直接给尺寸档，文字按钮和图标按钮都按这一档。
    pub fn control_size(mut self, size: ControlSize) -> Self {
        self.height = Height::Exact(size);
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// 资源图标随按钮文字色和界面字号变化。
    pub fn icon(self, icon: impl IconNamed) -> Self {
        self.icon_path(icon.path())
    }

    /// 使用应用自己的 SVG 图标资源。
    pub fn icon_path(mut self, path: impl Into<SharedString>) -> Self {
        self.icon = Some(path.into());
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

    /// 菜单开着时按钮保持按下的样子，说明弹层是它带出来的；不碰调用方给的选中。
    fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    fn is_open(&self) -> bool {
        self.open
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

/// 和 [`crate::Icon`] 一样，在绘制时取继承的文字颜色与大小。
#[derive(IntoElement)]
struct ButtonIcon(SharedString);

impl RenderOnce for ButtonIcon {
    fn render(self, window: &mut Window, _: &mut App) -> impl IntoElement {
        let text = window.text_style();
        svg()
            .path(self.0)
            .size(text.font_size.to_pixels(window.rem_size()))
            .text_color(text.color)
            .flex_shrink_0()
    }
}

/// 图标格子：边长就是图标大小（px，界面字号 16 时），随 rem 缩放。
fn icon_slot(icon: SharedString, size: f32) -> gpui::Div {
    div()
        .size(rems(size / 16.))
        .text_size(rems(size / 16.))
        .flex_shrink_0()
        .child(ButtonIcon(icon))
}

/// 按钮的浮起层级（[`elevation::Elevation`]）：中性、强调各一档，安静按钮不浮起。加载中换成一圈内细边。
fn button_shadows(role: Role, elevation: &Elevation, loading: bool, hair: Rgba) -> Vec<BoxShadow> {
    if loading {
        return vec![inset_ring(hair, 1.)];
    }
    match role {
        Role::Secondary | Role::Danger => elevation.button_neutral.clone(),
        Role::Primary => elevation.button_contrast.clone(),
        Role::Quiet | Role::DangerQuiet => Vec::new(),
    }
}

/// GPUI 的外阴影也画在元素内部；只要任一可达状态透底，就不能用它。
fn drop_shadows_safe(
    rest_bg: Option<Rgba>,
    colors: RoleColors,
    interactive: bool,
    reduced_opacity: bool,
    custom_background: bool,
) -> bool {
    !reduced_opacity
        && !custom_background
        && rest_bg.is_some_and(|bg| bg.a >= 1.)
        && (!interactive || (colors.hover_bg.a >= 1. && colors.press_bg.a >= 1.))
}

/// 按钮该画的投影：加载中一圈内细边；任一状态透底时只留内细圈；浮起按钮 `raised`；其余按角色。
fn role_shadows(
    role: Role,
    floating: bool,
    loading: bool,
    safe_drop_shadows: bool,
) -> Vec<BoxShadow> {
    let theme_elevation = elevation::current();
    if loading {
        button_shadows(role, &theme_elevation, true, ui_theme::hair())
    } else if !safe_drop_shadows {
        translucent_button_shadows(role, &theme_elevation, floating, ui_theme::hair())
    } else if floating {
        theme_elevation.raised.clone()
    } else {
        button_shadows(role, &theme_elevation, false, ui_theme::hair())
    }
}

/// 别处画的按钮要和 [`Button`] 一样浮起时取的那组投影。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonShadow {
    /// 中性、危险按钮。
    Neutral,
    /// 主操作。
    Primary,
    /// 浮在内容上的按钮（`floating()`）。
    Floating,
}

/// 一颗可点的按钮静止时的投影，和 [`Button`] 自己画的同一份（含「任一状态透底就只留内细圈」）。
/// NSH 插件的按钮经主题令牌拿它，不自己推。
pub fn resting_shadows(kind: ButtonShadow) -> Vec<BoxShadow> {
    let (role, floating) = match kind {
        ButtonShadow::Neutral => (Role::Secondary, false),
        ButtonShadow::Primary => (Role::Primary, false),
        ButtonShadow::Floating => (Role::Secondary, true),
    };
    let colors = role.colors_from(ui_theme::palette(), false);
    let rest_bg = if floating {
        Some(rgb(ui_theme::surface()))
    } else {
        colors.bg
    };
    let safe = drop_shadows_safe(rest_bg, colors, true, false, false);
    role_shadows(role, floating, false, safe)
}

fn inset_ring(color: impl Into<Hsla>, width: f32) -> BoxShadow {
    BoxShadow::new(px(0.), px(0.), color.into())
        .spread_radius(px(width))
        .inset()
}

fn translucent_button_shadows(
    role: Role,
    elevation: &Elevation,
    floating: bool,
    hair: Rgba,
) -> Vec<BoxShadow> {
    // 只留层级里那圈细边（不偏移、不模糊、往外扩），换成内环。
    let ring = |layers: &[BoxShadow]| {
        layers
            .iter()
            .find(|layer| {
                !layer.inset
                    && layer.offset.y == px(0.)
                    && layer.blur_radius == px(0.)
                    && layer.spread_radius > px(0.)
            })
            .map(|layer| layer.color)
    };
    let color = if floating {
        Some(hair.into())
    } else {
        match role {
            Role::Secondary | Role::Danger => ring(&elevation.button_neutral),
            Role::Primary => ring(&elevation.button_contrast),
            Role::Quiet | Role::DangerQuiet => None,
        }
    };
    // GPUI 对 inset 内矩形取整，0.5 逻辑像素在 1x 的直边会消失。
    color
        .map(|color| vec![inset_ring(color, 1.)])
        .unwrap_or_default()
}

fn focused_button_shadows(
    shadows: &[BoxShadow],
    safe_drop_shadows: bool,
    palette: ui_theme::Palette,
    rest_bg: Option<Rgba>,
) -> Vec<BoxShadow> {
    if safe_drop_shadows {
        let mut focused = focus_ring();
        focused.extend(shadows.iter().cloned());
        focused
    } else {
        let mut focused = shadows.to_vec();
        // 外焦点环同样会透进底色；用内环并按主题背景估算按钮底，保证焦点可见。
        // 实际父背景不可知，半透明底只能按主题背景估算，不能保证所有自定义底色。
        let backdrop = rest_bg.map_or(palette.bg, |bg| ui_theme::flatten(bg, palette.bg));
        let themed = ui_theme::flatten(palette.focus, backdrop);
        let color = ui_theme::legible(themed, backdrop, 4.5);
        // 悬停、按下或自定义父底可能和静止底不同；加一条与焦点色对比的内边，
        // 即使焦点色接近实际底色，两条线之间仍可辨认。先画内边，再画外环。
        let inner = ui_theme::legible(palette.bg, color, 4.5);
        focused.push(inset_ring(rgb(inner), FOCUS_RING + 1.));
        focused.push(inset_ring(rgb(color), FOCUS_RING));
        focused
    }
}

/// 激活按钮的键：不带修饰的回车、空格，和 gpui 的键盘点击同一组。
pub(crate) fn is_activation_key(keystroke: &gpui::Keystroke) -> bool {
    matches!(keystroke.key.as_str(), "enter" | "space") && !keystroke.modifiers.modified()
}

/// 键盘焦点环：2px `bg` 间隙外再套 2px `focus`。环用实色，免得半透明的焦点色
/// 透出底下那圈间隙。
pub(crate) fn focus_ring() -> Vec<BoxShadow> {
    let bg = ui_theme::bg();
    let ring = ui_theme::flatten(ui_theme::focus(), bg);
    let spread = |width: f32, color: u32| {
        BoxShadow::new(px(0.), px(0.), rgb(color).into()).spread_radius(px(width))
    };
    // 后画的在上面：外环先画，间隙压在它里圈。
    vec![spread(FOCUS_GAP + FOCUS_RING, ring), spread(FOCUS_GAP, bg)]
}

impl Button {
    fn build(self) -> BaseButton {
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
            open,
        } = self;
        let metrics = metrics(height.control_size(icon_only));
        // 透明底、淡字的安静按钮，字不加重。
        let text = match role {
            Role::Quiet if metrics.text == TYPE.small_medium => TYPE.small,
            Role::Quiet if metrics.text == TYPE.ui_medium => TYPE.ui,
            _ => metrics.text,
        };
        let colors = role.colors_from(ui_theme::palette(), icon_only);
        let interactive = !disabled && !loading;
        let fg = if loading {
            ui_theme::text()
        } else if dismiss {
            ui_theme::faint()
        } else {
            colors.fg
        };
        // 选中只对透明底和 `soft` 底的角色有意义：`soft_hover` 底加正文色。
        let selects = matches!(role, Role::Secondary | Role::Quiet);
        let selected = selected && selects;
        let pressed = (selected || open) && selects;
        let rest_bg = if loading {
            Some(rgb(ui_theme::soft_hover()))
        } else if pressed {
            Some(rgb(ui_theme::soft_hover()))
        } else if floating {
            Some(rgb(ui_theme::surface()))
        } else {
            colors.bg
        };
        let safe_drop_shadows = drop_shadows_safe(
            rest_bg,
            colors,
            interactive,
            (disabled && !loading) || style.opacity.is_some_and(|opacity| opacity < 1.),
            style.background.is_some(),
        );
        let shadows = role_shadows(role, floating, loading, safe_drop_shadows);
        let focused_shadows =
            focused_button_shadows(&shadows, safe_drop_shadows, ui_theme::palette(), rest_bg);

        let content = div()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(metrics.gap))
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            // 加载时内容只是看不见，仍然占位，按钮宽度不变。
            .when(loading, |this| this.opacity(0.))
            .when_some(icon, |this, icon| {
                let size = if dismiss {
                    14.
                } else if icon_only {
                    metrics.icon_button_glyph
                } else {
                    metrics.icon
                };
                this.child(icon_slot(icon, size))
            })
            .when_some(label.clone(), |this, label| {
                this.child(div().min_w_0().text_ellipsis().child(label))
            })
            .children(children)
            .when(dropdown_caret, |this| {
                this.child(icon_slot(crate::IconName::ChevronDown.path(), metrics.icon))
            });

        base.disabled(!interactive)
            .selected(selected)
            .styles(|styles| {
                styles.disabled(|style| style.opacity(if loading { 1. } else { role.disabled_opacity() }))
            })
            .relative()
            .flex_none()
            .cursor_default()
            .map(|this| text.apply(this))
            .text_color(rgb(if pressed { ui_theme::text() } else { fg }))
            .map(|this| {
                if dismiss {
                    this.size(px(DISMISS_BOX))
                        .min_w(rems(DISMISS_BOX / 16.))
                        .min_h(rems(DISMISS_BOX / 16.))
                        .rounded(px(DISMISS_RADIUS))
                } else if icon_only {
                    // 界面字号调大时跟着 rem 长，不比图标小。
                    let min = rems(metrics.icon_button_size / 16.);
                    this.size(px(metrics.icon_button_size))
                        .min_w(min)
                        .min_h(min)
                        .rounded(px(metrics.icon_button_radius))
                } else {
                    // 自定义内容可能是多行；按钮的可点击区域必须包住它。
                    this.min_h(px(metrics.height))
                        .px(px(metrics.padding_x))
                        .rounded(px(metrics.radius))
                }
            })
            .when_some(rest_bg, |this, bg| this.bg(bg))
            .shadow(shadows)
            // 主按钮上沿的高光，往下淡到透明，叠在各状态底色上；不改父背景。带字的是白色
            // 20%，只有图标的是主题的 `submit_highlight`。
            .when(role == Role::Primary && !loading, |this| {
                let radius = px(if dismiss {
                    DISMISS_RADIUS
                } else if icon_only {
                    metrics.icon_button_radius
                } else {
                    metrics.radius
                });
                let c = ui_theme::colors();
                let white = |a: f32| Rgba { a, ..c.white };
                let top = if icon_only { c.submit_highlight } else { white(0.2) };
                this.child(div().absolute().inset_0().rounded(radius).bg(linear_gradient(
                    180.,
                    linear_color_stop(top, 0.),
                    linear_color_stop(white(0.), 1.),
                )))
            })
            .when(interactive, |this| {
                this.hover(move |style| style.bg(colors.hover_bg).text_color(rgb(colors.hover_fg)))
                    .active(move |style| style.bg(colors.press_bg).text_color(rgb(colors.press_fg)))
            })
            .focus_visible(move |style| style.shadow(focused_shadows.clone()))
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
            .when(interactive, |this| {
                this.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    // 点按钮不抢焦点，输入框里的光标留在原处；也不开始窗口级的选字。
                    window.prevent_default();
                    GlobalState::suppress_text_selection(cx);
                })
            })
            // 禁用、加载中的按钮可能还留着焦点。回车、空格拦在它身上，外层（如下拉菜单的
            // 触发器）不替它响应；指针按下 gpui-base 已经这样拦了。
            .when(!interactive, |this| {
                this.on_key_down(|event, _, cx| {
                    if is_activation_key(&event.keystroke) {
                        cx.stop_propagation();
                    }
                })
            })
            .map(|this| match on_click {
                Some(on_click) if interactive => {
                    this.on_click(move |event, window, cx| on_click(event, window, cx))
                }
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

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.build()
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
    use gpui::{
        AppContext as _, Context, Element as _, Entity, KeyDownEvent, KeyUpEvent, Keystroke,
        Modifiers, Render, TestAppContext, VisualTestContext, accesskit, canvas, point, rgba,
    };
    use std::{
        cell::Cell,
        sync::{Arc, Mutex},
    };

    /// 随 App 带的 smelt 主题的投影，不经全局主题。
    fn builtin_elevation(light: bool) -> Arc<Elevation> {
        use crate::theme_config::ThemeMode;
        let mode = if light {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        crate::theme_catalog::builtin_for_mode(mode)
            .elevation()
            .clone()
    }

    struct ButtonHarness {
        disabled: bool,
        loading: bool,
        clicks: Rc<Cell<usize>>,
    }

    impl Render for ButtonHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let clicks = self.clicks.clone();
            div().tab_group().size(px(100.)).child(
                Button::new("button")
                    .label("Button")
                    .size_full()
                    .disabled(self.disabled)
                    .loading(self.loading)
                    .on_click(move |_, _, _| clicks.set(clicks.get() + 1)),
            )
        }
    }

    #[cfg(debug_assertions)]
    struct CustomContent;

    #[cfg(debug_assertions)]
    impl Render for CustomContent {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(
                Button::new("custom-content")
                    .debug_selector(|| "CUSTOM_CONTENT_BUTTON".into())
                    .child(div().w(px(60.)).h(px(40.))),
            )
        }
    }

    #[cfg(debug_assertions)]
    #[gpui::test]
    fn button_grows_to_fit_custom_content(cx: &mut TestAppContext) {
        cx.update(gpui_base::init);
        let (_, cx) = cx.add_window_view(|_, _| CustomContent);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let bounds = cx
            .debug_bounds("CUSTOM_CONTENT_BUTTON")
            .expect("button rendered");
        assert!(
            bounds.size.height >= px(40.),
            "button does not cover its content: {bounds:?}"
        );
    }

    fn harness(
        cx: &mut TestAppContext,
        disabled: bool,
        loading: bool,
    ) -> (
        Entity<ButtonHarness>,
        &mut VisualTestContext,
        Rc<Cell<usize>>,
    ) {
        cx.update(gpui_base::init);
        let clicks = Rc::new(Cell::new(0));
        let (view, cx) = cx.add_window_view({
            let clicks = clicks.clone();
            move |_, _| ButtonHarness {
                disabled,
                loading,
                clicks,
            }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (view, cx, clicks)
    }

    fn press_activation_keys(cx: &mut VisualTestContext) {
        for key in ["enter", "space"] {
            let keystroke =
                cx.update(|_, cx| Keystroke::parse_with_os(key, cx.platform_info().os).unwrap());
            cx.simulate_event(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            cx.simulate_event(KeyUpEvent { keystroke });
        }
    }

    #[gpui::test]
    fn pointer_and_keyboard_activate_button(cx: &mut TestAppContext) {
        let (_, cx, clicks) = harness(cx, false, false);
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        assert_eq!(clicks.get(), 1);

        cx.update(|window, cx| {
            window.focus_next(cx);
            assert!(window.focused(cx).is_some());
            window.draw(cx).clear(cx);
        });
        press_activation_keys(cx);
        assert_eq!(clicks.get(), 3);
    }

    #[gpui::test]
    fn all_roles_keep_pointer_and_keyboard_activation(cx: &mut TestAppContext) {
        struct Probe(Role, bool, bool, Rc<Cell<usize>>);
        impl Render for Probe {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let clicks = self.3.clone();
                let mut button = Button::new("button")
                    .role(self.0)
                    .label("Button")
                    .size_full()
                    .selected(self.2)
                    .on_click(move |_, _, _| clicks.set(clicks.get() + 1));
                if self.1 {
                    button = button.floating();
                }
                div().tab_group().size(px(100.)).child(button)
            }
        }
        cx.update(gpui_base::init);
        for role in [
            Role::Primary,
            Role::Secondary,
            Role::Quiet,
            Role::Danger,
            Role::DangerQuiet,
        ] {
            for (floating, selected) in [(false, false), (true, false), (false, true)] {
                let clicks = Rc::new(Cell::new(0));
                let (_, window) = cx.add_window_view({
                    let clicks = clicks.clone();
                    move |_, _| Probe(role, floating, selected, clicks)
                });
                window.update(|window, cx| window.draw(cx).clear(cx));
                window.simulate_click(point(px(10.), px(10.)), Modifiers::default());
                window.update(|window, cx| {
                    window.focus_next(cx);
                    assert!(window.focused(cx).is_some());
                    window.draw(cx).clear(cx);
                });
                press_activation_keys(window);
                assert_eq!(clicks.get(), 3, "{role:?} {floating} {selected}");
            }
        }
    }

    #[gpui::test]
    fn focused_button_becomes_disabled_and_does_not_activate(cx: &mut TestAppContext) {
        let (view, cx, clicks) = harness(cx, false, false);
        let focus = cx.update(|window, cx| {
            window.focus_next(cx);
            let focus = window.focused(cx).expect("按钮应当能获得焦点");
            window.draw(cx).clear(cx);
            focus
        });
        view.update(cx, |view, cx| {
            view.disabled = true;
            cx.notify();
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert_eq!(window.focused(cx), Some(focus));
        });
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        press_activation_keys(cx);
        assert_eq!(clicks.get(), 0);
    }

    #[gpui::test]
    fn loading_button_is_skipped_by_tab_and_does_not_activate(cx: &mut TestAppContext) {
        let (_, cx, clicks) = harness(cx, false, true);
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        cx.update(|window, cx| {
            window.focus_next(cx);
            assert!(window.focused(cx).is_none(), "加载按钮不应进入 Tab 顺序");
            window.draw(cx).clear(cx);
        });
        press_activation_keys(cx);
        assert_eq!(clicks.get(), 0);
    }

    #[gpui::test]
    fn loading_does_not_activate_parent_row(cx: &mut TestAppContext) {
        struct Row(Rc<Cell<usize>>);
        impl Render for Row {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let clicks = self.0.clone();
                div()
                    .id("row")
                    .size(px(100.))
                    .on_click(move |_, _, _| clicks.set(clicks.get() + 1))
                    .child(Button::new("busy").label("Save").size_full().loading(true))
            }
        }
        cx.update(gpui_base::init);
        let clicks = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view({
            let clicks = clicks.clone();
            move |_, _| Row(clicks)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        assert_eq!(clicks.get(), 0);
    }

    #[gpui::test]
    fn focused_loading_button_retains_focus_and_recovers(cx: &mut TestAppContext) {
        let (view, cx, clicks) = harness(cx, false, false);
        let focus = cx.update(|window, cx| {
            window.focus_next(cx);
            let focus = window.focused(cx).expect("按钮应当能获得焦点");
            window.draw(cx).clear(cx);
            focus
        });
        view.update(cx, |view, cx| {
            view.loading = true;
            cx.notify();
        });
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert_eq!(window.focused(cx), Some(focus.clone()));
        });
        press_activation_keys(cx);
        cx.simulate_click(point(px(10.), px(10.)), Modifiers::default());
        assert_eq!(clicks.get(), 0);
        view.update(cx, |view, cx| {
            view.loading = false;
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        press_activation_keys(cx);
        assert_eq!(clicks.get(), 2);
    }

    #[gpui::test]
    fn loading_exposes_disabled_button_semantics(cx: &mut TestAppContext) {
        struct Probe(Arc<Mutex<Vec<accesskit::Node>>>);
        impl Render for Probe {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let nodes = self.0.clone();
                canvas(
                    move |_, window, cx| {
                        let mut result = Vec::new();
                        for loading in [false, true] {
                            let base = Button::new(if loading { "busy" } else { "ready" })
                                .label("Save")
                                .loading(loading)
                                .on_click(|_, _, _| {})
                                .build();
                            let element = base.render(window, cx).into_element();
                            let mut node = accesskit::Node::new(gpui::Role::Button);
                            element.write_a11y_info(&mut node);
                            result.push(node);
                        }
                        *nodes.lock().unwrap() = result;
                    },
                    |_, _, _, _| {},
                )
            }
        }
        cx.update(gpui_base::init);
        let nodes = Arc::new(Mutex::new(Vec::new()));
        let (_, cx) = cx.add_window_view({
            let nodes = nodes.clone();
            move |_, _| Probe(nodes)
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let nodes = nodes.lock().unwrap();
        assert_eq!(nodes[0].label(), Some("Save"));
        assert!(nodes[0].supports_action(accesskit::Action::Click));
        assert!(!nodes[0].is_disabled());
        assert_eq!(nodes[1].label(), Some("Save"));
        assert!(nodes[1].is_disabled());
        assert!(!nodes[1].supports_action(accesskit::Action::Click));
    }

    #[test]
    fn button_elevations_and_loading_inner_ring() {
        let (light_theme, dark_theme) = (builtin_elevation(true), builtin_elevation(false));
        let light = button_shadows(Role::Secondary, &light_theme, false, rgba(0x1a1a1817));
        assert_eq!(light.len(), 2);
        assert_eq!(light[0].offset.y, px(1.));
        assert_eq!(light[0].blur_radius, px(1.5));
        assert_eq!(light[1].spread_radius, px(0.5));
        assert_eq!(light[1].color, rgba(0x00000024).into());
        let dark = button_shadows(Role::Secondary, &dark_theme, false, rgba(0xfffffa14));
        assert_eq!(dark.len(), 3);
        assert_eq!(dark[1].color, rgba(0xffffff33).into());
        assert_eq!(dark[2].offset.y, px(-0.5));
        let contrast = button_shadows(Role::Primary, &light_theme, false, rgba(0x1a1a1817));
        assert_eq!(contrast.len(), 4);
        assert!(contrast[2].inset && contrast[3].inset);
        let loading = button_shadows(Role::Primary, &light_theme, true, rgba(0x1a1a1817));
        assert_eq!(loading.len(), 1);
        assert!(loading[0].inset);
        assert_eq!(loading[0].spread_radius, px(1.));
        assert_eq!(loading[0].color, rgba(0x1a1a1817).into());
        assert!(button_shadows(Role::Quiet, &light_theme, false, rgba(0x1a1a1817)).is_empty());
    }

    #[test]
    fn translucent_states_never_use_drop_shadows_even_when_focused() {
        let hair = rgba(0xabcdef40);
        for light in [false, true] {
            let theme_elevation = builtin_elevation(light);
            for role in [
                Role::Primary,
                Role::Secondary,
                Role::Quiet,
                Role::Danger,
                Role::DangerQuiet,
            ] {
                let mut palette = ui_theme::builtin_fallback(light);
                for translucent_state in 0..3 {
                    let mut fills = [rgb(0x123456); 3];
                    fills[translucent_state] = rgba(0x12345680);
                    palette.button_primary = fills;
                    palette.button_secondary = fills;
                    let colors = role.colors_from(palette, false);
                    for floating in [false, true] {
                        for selected in [false, true] {
                            let rest = if selected && matches!(role, Role::Secondary | Role::Quiet)
                            {
                                Some(rgb(palette.soft_hover))
                            } else if floating {
                                Some(rgb(palette.surface))
                            } else {
                                colors.bg
                            };
                            let safe = drop_shadows_safe(rest, colors, true, false, false);
                            let expected = rest.is_some_and(|bg| bg.a >= 1.)
                                && colors.hover_bg.a >= 1.
                                && colors.press_bg.a >= 1.;
                            assert_eq!(safe, expected, "{role:?} {floating} {selected}");
                            if !safe {
                                let shadows = translucent_button_shadows(
                                    role,
                                    &theme_elevation,
                                    floating,
                                    hair,
                                );
                                assert!(shadows.iter().all(|shadow| shadow.inset));
                                let focused =
                                    focused_button_shadows(&shadows, false, palette, rest);
                                assert!(focused.iter().all(|shadow| shadow.inset));
                                assert_eq!(focused.last().unwrap().spread_radius, px(FOCUS_RING));
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn translucent_focus_is_legible_on_estimated_rest_fill() {
        for light in [false, true] {
            let mut palette = ui_theme::builtin_fallback(light);
            palette.button_primary[0] = rgba(0x6688aa80);
            palette.button_secondary[0] = rgba(0x6688aa80);
            for role in [
                Role::Primary,
                Role::Secondary,
                Role::Quiet,
                Role::Danger,
                Role::DangerQuiet,
            ] {
                for floating in [false, true] {
                    for selected in [false, true] {
                        let rest = if selected && matches!(role, Role::Secondary | Role::Quiet) {
                            Some(rgb(palette.soft_hover))
                        } else if floating {
                            Some(rgb(palette.surface))
                        } else {
                            role.colors_from(palette, false).bg
                        };
                        let backdrop =
                            rest.map_or(palette.bg, |bg| ui_theme::flatten(bg, palette.bg));
                        let focused = focused_button_shadows(&[], false, palette, rest);
                        let actual = ui_theme::flatten(focused[1].color.to_rgb(), backdrop);
                        let inner = ui_theme::flatten(focused[0].color.to_rgb(), backdrop);
                        let ratio = ui_theme::contrast_ratio(actual, backdrop);
                        assert!(
                            ratio >= 4.5,
                            "{role:?} light={light} floating={floating} selected={selected}: {ratio}"
                        );
                        assert!(ui_theme::contrast_ratio(inner, actual) >= 4.5);
                    }
                }
            }
        }
    }

    #[test]
    fn opacity_loading_and_ring_colors_are_handled_without_changing_layout() {
        let hair = rgba(0xabcdef40);
        let palette = ui_theme::builtin_fallback(true);
        let (light_theme, dark_theme) = (builtin_elevation(true), builtin_elevation(false));
        let colors = Role::Primary.colors_from(palette, false);
        assert!(drop_shadows_safe(colors.bg, colors, true, false, false));
        assert!(!drop_shadows_safe(colors.bg, colors, true, true, false));
        assert!(!drop_shadows_safe(colors.bg, colors, true, false, true));
        // Loading 不响应 hover/pressed；它自身已有 inset hair 环。
        let mut translucent_hover = colors;
        translucent_hover.hover_bg.a = 0.5;
        assert!(drop_shadows_safe(
            colors.bg,
            translucent_hover,
            false,
            false,
            false
        ));
        let loading = button_shadows(Role::Primary, &light_theme, true, hair);
        assert_eq!(loading, vec![inset_ring(hair, 1.)]);
        for (role, theme, floating, expected, width) in [
            (Role::Secondary, &light_theme, false, rgba(0x00000024), 1.),
            (Role::Danger, &dark_theme, false, rgba(0xffffff33), 1.),
            (Role::Primary, &light_theme, false, rgba(0x3a3a3aff), 1.),
            (Role::Primary, &dark_theme, false, rgba(0xffffff66), 1.),
            (Role::Quiet, &light_theme, true, hair, 1.),
        ] {
            assert_eq!(
                translucent_button_shadows(role, theme, floating, hair),
                vec![inset_ring(expected, width)]
            );
        }
        assert!(translucent_button_shadows(Role::Quiet, &light_theme, false, hair).is_empty());
        let opaque = button_shadows(Role::Primary, &light_theme, false, hair);
        let focused = focused_button_shadows(&opaque, true, palette, colors.bg);
        assert_eq!(&focused[2..], opaque.as_slice());
    }

    // ui_theme 是进程级的；只串行本模块测试不足以隔离其它模块的主题写入。
    #[cfg(target_os = "macos")]
    fn isolated_theme_test(name: &str) -> bool {
        const CHILD: &str = "SMELT_BUTTON_THEME_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            return true;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env(CHILD, "1")
            .status()
            .expect("启动隔离的按钮主题测试");
        assert!(status.success(), "隔离测试 {name} 失败: {status}");
        false
    }

    fn init_for_test(cx: &mut App) {
        gpui_base::init(cx);
        crate::init(cx);
    }

    /// 换上「默认主题只改几个色」的一款，像第三方主题自带按钮底那样。
    fn apply_button_colors(dark: bool, colors: &[(&str, Rgba)], cx: &mut App) {
        let byte = |value: f32| (value * 255.).round() as u8;
        let mut set: serde_json::Value =
            serde_json::from_str(include_str!("../themes/smelt.json")).unwrap();
        let mode = if dark { "dark" } else { "light" };
        let mut theme = set["themes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|theme| theme["mode"] == mode)
            .unwrap()
            .clone();
        theme["name"] = "button-test".into();
        for (key, color) in colors {
            theme["colors"][*key] = format!(
                "#{:02X}{:02X}{:02X}{:02X}",
                byte(color.r),
                byte(color.g),
                byte(color.b),
                byte(color.a)
            )
            .into();
        }
        set["name"] = "button-test".into();
        set["themes"] = serde_json::Value::Array(vec![theme]);
        let source = set.to_string();
        crate::theme_catalog::register_theme_sets("button-test", &[&source], cx)
            .expect("测试主题应能登记");
        assert!(crate::theme_catalog::apply("button-test", cx));
    }

    // 用真实 Metal 绘制而不是只检查 shadow 参数：外阴影不能透过半透明按钮底。
    #[cfg(target_os = "macos")]
    #[test]
    fn translucent_secondary_composites_over_each_actual_parent() {
        if !isolated_theme_test(
            "button::tests::translucent_secondary_composites_over_each_actual_parent",
        ) {
            return;
        }
        use gpui::{HeadlessAppContext, size};

        struct Probe;
        impl Render for Probe {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                // 按钮在格子里居中：无头窗口的鼠标停在原点，贴着原点的按钮会画成悬停。
                div()
                    .flex()
                    .children([0x1e1e1d, 0x507090].map(|background| {
                        div()
                            .size(px(40.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(rgb(background))
                            .child(
                                Button::new(if background == 0x1e1e1d {
                                    "dark"
                                } else {
                                    "blue"
                                })
                                .label("")
                                .size(px(28.)),
                            )
                    }))
            }
        }

        for (theme_name, override_fill, fill) in [
            (
                smelt_model::theme_index::DEFAULT_DARK,
                None,
                rgba(0xffffff0f),
            ),
            (
                smelt_model::theme_index::DEFAULT_LIGHT,
                Some(rgba(0x20408060)),
                rgba(0x20408060),
            ),
            (
                smelt_model::theme_index::DEFAULT_DARK,
                Some(rgba(0xe0804060)),
                rgba(0xe0804060),
            ),
        ] {
            let mut cx = HeadlessAppContext::with_platform(
                Arc::new(gpui_apple::AppleTextSystem::new()),
                Arc::new(crate::assets::Assets),
                || {
                    Ok(Some(Box::new(
                        gpui_apple::metal_renderer::MetalHeadlessRenderer::new()?,
                    )))
                },
            );
            cx.update(|cx| {
                init_for_test(cx);
                match override_fill {
                    Some(fill) => apply_button_colors(
                        theme_name == smelt_model::theme_index::DEFAULT_DARK,
                        &[("bg_button_neutral", fill)],
                        cx,
                    ),
                    None => assert!(crate::theme_catalog::apply(theme_name, cx)),
                }
                let actual = ui_theme::palette().button_secondary[0];
                for (a, b) in [
                    (actual.r, fill.r),
                    (actual.g, fill.g),
                    (actual.b, fill.b),
                    (actual.a, fill.a),
                ] {
                    assert!((a - b).abs() < 0.001);
                }
            });
            let window = cx
                .open_window(size(px(80.), px(40.)), |_, cx| cx.new(|_| Probe))
                .unwrap();
            cx.run_until_parked();
            let image = cx.capture_screenshot(window.into()).unwrap();
            let scale_x = image.width() / 80;
            let scale_y = image.height() / 40;
            assert!(scale_x > 0 && scale_y > 0);
            for (x, background) in [(20, 0x1e1e1d), (60, 0x507090)] {
                let pixel = image.get_pixel(x * scale_x, 20 * scale_y).0;
                let expected = ui_theme::flatten(fill, background);
                for (actual, shift) in pixel[..3].iter().zip([16, 8, 0]) {
                    let channel = ((expected >> shift) & 0xff) as i16;
                    assert!(
                        (i16::from(*actual) - channel).abs() <= 2,
                        "{theme_name}: fill={fill:?}, parent={background:06x}, pixel={pixel:?}, expected={expected:06x}"
                    );
                }
            }
        }
    }

    // 鼠标状态会切换输入模态；这里只检查其合成，不把它称为 focus-visible。
    #[cfg(target_os = "macos")]
    #[test]
    fn translucent_mouse_states_composite_over_parent() {
        if !isolated_theme_test("button::tests::translucent_mouse_states_composite_over_parent") {
            return;
        }
        use gpui::{HeadlessAppContext, MouseDownEvent, MouseMoveEvent, PlatformInput, size};

        struct Probe {
            role: Role,
            floating: bool,
            selected: bool,
        }
        impl Render for Probe {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let mut button = Button::new("button")
                    .role(self.role)
                    .label("")
                    .size(px(28.))
                    .selected(self.selected)
                    .on_click(|_, _, _| {});
                if self.floating {
                    button = button.floating();
                }
                div()
                    .tab_group()
                    .size(px(40.))
                    .bg(rgb(0x507090))
                    .child(button)
            }
        }

        for (role, floating, selected) in [
            (Role::Secondary, false, false),
            (Role::Danger, false, false),
            (Role::Secondary, true, false),
            (Role::Secondary, false, true),
        ] {
            let mut cx = HeadlessAppContext::with_platform(
                Arc::new(gpui_apple::AppleTextSystem::new()),
                Arc::new(crate::assets::Assets),
                || {
                    Ok(Some(Box::new(
                        gpui_apple::metal_renderer::MetalHeadlessRenderer::new()?,
                    )))
                },
            );
            // 悬停、按下是叠在按钮底上的半透明层，三态的实际填充从调色板读。
            let fills = cx.update(|cx| {
                init_for_test(cx);
                apply_button_colors(
                    false,
                    &[
                        ("bg_button_neutral", rgba(0x20408060)),
                        ("hover", rgba(0xb0402060)),
                        ("pressed", rgba(0x30a05060)),
                    ],
                    cx,
                );
                ui_theme::palette().button_secondary
            });
            let rest = if selected {
                rgb(ui_theme::soft_hover())
            } else if floating {
                rgb(ui_theme::surface())
            } else {
                fills[0]
            };
            let window = cx
                .open_window(size(px(40.), px(40.)), |_, cx| {
                    cx.new(|_| Probe {
                        role,
                        floating,
                        selected,
                    })
                })
                .unwrap();
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, cx| {
                window.draw(cx).clear(cx);
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position: point(px(39.), px(39.)),
                        modifiers: Modifiers::default(),
                        pressed_button: None,
                    }),
                    cx,
                );
                let keystroke = Keystroke::parse_with_os("a", cx.platform_info().os).unwrap();
                window.dispatch_event(
                    PlatformInput::KeyDown(KeyDownEvent {
                        keystroke,
                        is_held: false,
                        prefer_character_input: false,
                    }),
                    cx,
                );
                window.focus_next(cx);
                assert!(window.focused(cx).is_some());
                assert!(window.last_input_was_keyboard());
                window.draw(cx).clear(cx);
            })
            .unwrap();
            for (state, fill) in [(0, rest), (1, fills[1]), (2, fills[2])] {
                if state != 0 {
                    cx.update_window(window.into(), |_, window, cx| {
                        let position = point(px(14.), px(14.));
                        let event = if state == 1 {
                            PlatformInput::MouseMove(MouseMoveEvent {
                                position,
                                modifiers: Modifiers::default(),
                                pressed_button: None,
                            })
                        } else {
                            PlatformInput::MouseDown(MouseDownEvent {
                                position,
                                modifiers: Modifiers::default(),
                                button: MouseButton::Left,
                                click_count: 1,
                                first_mouse: false,
                            })
                        };
                        window.dispatch_event(event, cx);
                        assert!(!window.last_input_was_keyboard());
                        window.draw(cx).clear(cx);
                    })
                    .unwrap();
                }
                let image = cx.capture_screenshot(window.into()).unwrap();
                let pixel = image
                    .get_pixel(image.width() * 14 / 40, image.height() * 14 / 40)
                    .0;
                let expected = ui_theme::flatten(fill, 0x507090);
                for (actual, shift) in pixel[..3].iter().zip([16, 8, 0]) {
                    let channel = ((expected >> shift) & 0xff) as i16;
                    assert!(
                        (i16::from(*actual) - channel).abs() <= 2,
                        "{role:?}, floating={floating}, selected={selected}, state={state}: pixel={pixel:?}, expected={expected:06x}"
                    );
                }
            }
        }
    }

    // 1x 和 2x 都检查真实直边像素；先在指针移出时测静态线，再键盘聚焦测焦点环。
    #[cfg(target_os = "macos")]
    #[test]
    fn translucent_edges_and_keyboard_focus_at_both_scales() {
        if !isolated_theme_test(
            "button::tests::translucent_edges_and_keyboard_focus_at_both_scales",
        ) {
            return;
        }
        use gpui::{HeadlessAppContext, MouseMoveEvent, PlatformInput, size};

        struct Probe {
            role: Role,
            floating: bool,
            selected: bool,
            disabled: bool,
            loading: bool,
        }
        impl Render for Probe {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let button = Button::new("edge")
                    .role(self.role)
                    .label("")
                    .size(px(28.))
                    .selected(self.selected)
                    .disabled(self.disabled)
                    .loading(self.loading);
                div()
                    .tab_group()
                    .size(px(40.))
                    .bg(rgb(0xffffff))
                    .child(if self.floating {
                        button.floating()
                    } else {
                        button
                    })
            }
        }

        let mut cx = HeadlessAppContext::with_platform(
            Arc::new(gpui_apple::AppleTextSystem::new()),
            Arc::new(crate::assets::Assets),
            || {
                Ok(Some(Box::new(
                    gpui_apple::metal_renderer::MetalHeadlessRenderer::new()?,
                )))
            },
        );
        cx.update(|cx| {
            init_for_test(cx);
            // 三态同一个半透明底：悬停、按下的叠层取全透明。主按钮的底总是压在窗口底上
            // 成实色、走外焦点环，不在这里。
            let fill = rgba(0xffffff80);
            let clear = rgba(0x00000000);
            apply_button_colors(
                false,
                &[
                    ("bg_button_neutral", fill),
                    ("hover", clear),
                    ("pressed", clear),
                ],
                cx,
            );
        });
        for scale in [1_u32, 2] {
            for (role, floating, selected, disabled, loading) in [
                (Role::Secondary, false, false, false, false),
                (Role::Quiet, false, false, false, false),
                (Role::Danger, false, false, false, false),
                (Role::DangerQuiet, false, false, false, false),
                (Role::Secondary, true, false, false, false),
                (Role::Secondary, false, true, false, false),
                (Role::Secondary, false, false, true, false),
                (Role::Secondary, false, false, false, true),
            ] {
                let window = cx
                    .open_window(size(px(40.), px(40.)), |_, cx| {
                        cx.new(|_| Probe {
                            role,
                            floating,
                            selected,
                            disabled,
                            loading,
                        })
                    })
                    .unwrap();
                cx.run_until_parked();
                cx.update_window(window.into(), |_, window, cx| {
                    // Headless 平台截图画布仍是 2x；场景几何按 Window 的缩放画，
                    // 因此下面按场景缩放取样，而不是按截图尺寸推断缩放。
                    window.set_scale_factor(scale as f32);
                    assert_eq!(window.scale_factor(), scale as f32);
                    window.draw(cx).clear(cx);
                    window.dispatch_event(
                        PlatformInput::MouseMove(MouseMoveEvent {
                            position: point(px(39.), px(39.)),
                            modifiers: Modifiers::default(),
                            pressed_button: None,
                        }),
                        cx,
                    );
                    window.draw(cx).clear(cx);
                })
                .unwrap();
                let image = cx.capture_screenshot(window.into()).unwrap();
                let edge = image.get_pixel(14 * scale, 0).0;
                let inside = image.get_pixel(14 * scale, 4 * scale).0;
                if !matches!(role, Role::Quiet | Role::DangerQuiet) {
                    assert_ne!(
                        edge, inside,
                        "missing straight border: {scale}x {role:?} floating={floating} selected={selected} disabled={disabled} loading={loading}"
                    );
                }
                if disabled || loading {
                    continue;
                }
                cx.update_window(window.into(), |_, window, cx| {
                    let keystroke = Keystroke::parse_with_os("a", cx.platform_info().os).unwrap();
                    window.dispatch_event(
                        PlatformInput::KeyDown(KeyDownEvent {
                            keystroke,
                            is_held: false,
                            prefer_character_input: false,
                        }),
                        cx,
                    );
                    window.focus_next(cx);
                    assert!(window.focused(cx).is_some());
                    assert!(window.last_input_was_keyboard());
                    window.draw(cx).clear(cx);
                })
                .unwrap();
                let focused = cx.capture_screenshot(window.into()).unwrap();
                let pixel = focused.get_pixel(14 * scale, scale).0;
                assert_ne!(
                    pixel,
                    image.get_pixel(14 * scale, scale).0,
                    "focus-visible did not change edge"
                );
                let center = focused.get_pixel(14 * scale, 4 * scale).0;
                let color =
                    |p: [u8; 4]| (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2]);
                let ratio = ui_theme::contrast_ratio(color(pixel), color(center));
                assert!(
                    ratio >= 3.,
                    "focus contrast {ratio}: {scale}x {role:?} floating={floating} selected={selected}, edge={pixel:?}, center={center:?}"
                );

                // 指针移入会切换到鼠标模态；键盘输入后 GPUI 抑制 hover，
                // 逐帧检查，不能把鼠标悬停误当成 focus-visible。
                cx.update_window(window.into(), |_, window, cx| {
                    window.dispatch_event(
                        PlatformInput::MouseMove(MouseMoveEvent {
                            position: point(px(14.), px(14.)),
                            modifiers: Modifiers::default(),
                            pressed_button: None,
                        }),
                        cx,
                    );
                    assert!(!window.last_input_was_keyboard());
                    window.draw(cx).clear(cx);
                })
                .unwrap();
                let hovered = cx.capture_screenshot(window.into()).unwrap();
                cx.update_window(window.into(), |_, window, cx| {
                    let keystroke = Keystroke::parse_with_os("a", cx.platform_info().os).unwrap();
                    window.dispatch_event(
                        PlatformInput::KeyDown(KeyDownEvent {
                            keystroke,
                            is_held: false,
                            prefer_character_input: false,
                        }),
                        cx,
                    );
                    assert!(window.focused(cx).is_some() && window.last_input_was_keyboard());
                    window.draw(cx).clear(cx);
                })
                .unwrap();
                let pointer_focus = cx.capture_screenshot(window.into()).unwrap();
                assert_ne!(
                    pointer_focus.get_pixel(14 * scale, scale),
                    hovered.get_pixel(14 * scale, scale),
                    "keyboard focus must differ from hover"
                );
                let outer = color(pointer_focus.get_pixel(14 * scale, scale).0);
                let inner = color(pointer_focus.get_pixel(14 * scale, 2 * scale).0);
                let ratio = ui_theme::contrast_ratio(outer, inner);
                assert!(
                    ratio >= 3.,
                    "keyboard focus edges contrast {ratio}: {scale}x {role:?} floating={floating} selected={selected}"
                );
            }
        }
    }

    #[test]
    fn icon_accepts_named_and_custom_assets() {
        let asset = Button::new("asset").icon(crate::IconName::Plus);
        let custom = Button::new("custom").icon_path("smelt-icons/agent-claude.svg");
        assert_eq!(asset.icon, Some(crate::IconName::Plus.path()));
        assert_eq!(custom.icon.as_deref(), Some("smelt-icons/agent-claude.svg"));
    }

    #[test]
    fn control_size_metrics() {
        let sizes: Vec<_> = [
            ControlSize::Mini,
            ControlSize::Small,
            ControlSize::Regular,
            ControlSize::Large,
            ControlSize::ExtraLarge,
        ]
        .into_iter()
        .map(|size| {
            let m = metrics(size);
            (
                m.height,
                m.padding_x,
                m.radius,
                m.icon,
                m.icon_button_size,
                m.icon_button_radius,
                m.icon_button_glyph,
            )
        })
        .collect();
        assert_eq!(
            sizes,
            vec![
                (16., 8., 4., 12., 16., 4., 12.),
                (20., 10., 4., 14., 20., 4., 16.),
                (24., 12., 6., 16., 24., 6., 16.),
                (28., 14., 8., 16., 28., 6., 16.),
                (36., 18., 10., 16., 36., 8., 18.),
            ]
        );
        assert_eq!(metrics(ControlSize::Small).text, TYPE.small_medium);
        assert_eq!(metrics(ControlSize::Regular).text, TYPE.ui_medium);
    }

    #[test]
    fn shorthand_sizes_map_to_control_sizes() {
        let text: Vec<_> = [Height::Small, Height::Medium, Height::Large]
            .into_iter()
            .map(|height| height.control_size(false))
            .collect();
        let icon: Vec<_> = [Height::Small, Height::Medium, Height::Large]
            .into_iter()
            .map(|height| height.control_size(true))
            .collect();
        use ControlSize::*;
        assert_eq!(text, vec![Regular, Large, ExtraLarge]);
        assert_eq!(icon, vec![Small, Regular, Large]);
        assert_eq!(Height::Exact(Mini).control_size(false), Mini);
    }

    #[test]
    fn disabled_opacity_per_role() {
        assert_eq!(Role::Primary.disabled_opacity(), 0.4);
        for role in [
            Role::Secondary,
            Role::Quiet,
            Role::Danger,
            Role::DangerQuiet,
        ] {
            assert_eq!(role.disabled_opacity(), 0.5);
        }
    }

    #[test]
    fn default_is_secondary_medium() {
        let button = Button::new("b");
        assert_eq!(button.role, Role::Secondary);
        // 默认文字按钮按迁移表是 Large 28/14，图标按钮是 Regular 24。
        assert_eq!(button.height, Height::Medium);
        let text = metrics(button.height.control_size(false));
        assert_eq!((text.height, text.padding_x), (28., 14.));
        let icon = metrics(button.height.control_size(true));
        assert_eq!(icon.icon_button_size, 24.);
    }

    #[test]
    fn danger_text_meets_contrast_on_builtin_window_background() {
        for light in [true, false] {
            let palette = ui_theme::builtin_fallback(light);
            for (role, backgrounds) in [
                (
                    Role::Danger,
                    palette
                        .button_secondary
                        .map(|color| ui_theme::flatten(color, palette.bg)),
                ),
                (
                    Role::DangerQuiet,
                    [
                        palette.bg,
                        ui_theme::flatten(palette.row_hover, palette.bg),
                        ui_theme::flatten(palette.row_selected, palette.bg),
                    ],
                ),
            ] {
                let colors = role.colors_from(palette, false);
                for (foreground, background) in [colors.fg, colors.hover_fg, colors.press_fg]
                    .into_iter()
                    .zip(backgrounds)
                {
                    let ratio = ui_theme::contrast_ratio(foreground, background);
                    assert!(ratio >= 4.5, "{role:?}, light={light}, contrast={ratio}");
                }
            }
        }
    }

    #[test]
    fn builtin_primary_text_and_icon_buttons_stay_legible() {
        for light in [true, false] {
            let palette = ui_theme::builtin_fallback(light);
            for icon_only in [false, true] {
                let primary = Role::Primary.colors_from(palette, icon_only);
                for background in [primary.bg.unwrap(), primary.hover_bg, primary.press_bg] {
                    let ratio = ui_theme::contrast_ratio(
                        primary.fg,
                        ui_theme::flatten(background, palette.bg),
                    );
                    assert!(ratio >= 4.5, "light={light} icon_only={icon_only}: {ratio}");
                }
            }
            let neutral = Role::Secondary.colors_from(palette, false);
            assert_eq!(neutral.bg, Some(palette.button_secondary[0]));
            assert_eq!(neutral.hover_bg, palette.button_secondary[1]);
            assert_eq!(neutral.press_bg, palette.button_secondary[2]);
        }
    }

    #[test]
    fn roles_read_their_palette_slots() {
        let palette = ui_theme::builtin_fallback(true);
        let primary = Role::Primary.colors_from(palette, false);
        assert_eq!(primary.bg, Some(palette.button_primary[0]));
        assert_eq!(primary.fg, palette.button_primary_foreground);
        assert_eq!(primary.hover_bg, palette.button_primary[1]);
        assert_eq!(primary.press_bg, palette.button_primary[2]);

        let submit = Role::Primary.colors_from(palette, true);
        assert_eq!(submit.bg, Some(palette.button_submit[0]));
        assert_eq!(submit.fg, palette.button_submit_foreground);
        assert_eq!(submit.hover_bg, palette.button_submit[1]);
        assert_eq!(submit.press_bg, palette.button_submit[2]);
        // 次要按钮只有图标时不换底。
        assert_eq!(
            Role::Secondary.colors_from(palette, true).bg,
            Some(palette.button_secondary[0])
        );

        let secondary = Role::Secondary.colors_from(palette, false);
        assert_eq!(secondary.bg, Some(palette.button_secondary[0]));
        assert_eq!(secondary.fg, palette.button_secondary_foreground);
        assert_eq!(secondary.hover_bg, palette.button_secondary[1]);
        assert_eq!(secondary.press_bg, palette.button_secondary[2]);

        let quiet = Role::Quiet.colors_from(palette, false);
        assert_eq!(quiet.bg, None);
        assert_eq!(quiet.fg, palette.muted);
        assert_eq!(quiet.hover_bg, palette.row_hover);
        assert_eq!(quiet.press_bg, palette.row_selected);

        let danger = Role::Danger.colors_from(palette, false);
        assert_eq!(danger.bg, Some(palette.button_secondary[0]));
        assert_eq!(
            danger.fg,
            ui_theme::legible(
                palette.bad,
                ui_theme::flatten(palette.button_secondary[0], palette.bg),
                4.5
            )
        );
        assert_eq!(danger.hover_bg, palette.button_secondary[1]);

        let danger_quiet = Role::DangerQuiet.colors_from(palette, false);
        assert_eq!(danger_quiet.bg, None);
        assert_eq!(
            danger_quiet.fg,
            ui_theme::legible(palette.bad, palette.bg, 4.5)
        );
        assert_eq!(danger_quiet.hover_bg, palette.row_hover);
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
        assert!(Button::new("b").icon(crate::IconName::Close).icon_only());
        assert!(!Button::new("b").label("关闭").icon_only());
        assert!(!Button::new("b").child(div()).icon_only());
    }

    #[test]
    fn focus_ring_is_gap_inside_ring() {
        let shadows = focus_ring();
        assert_eq!(shadows.len(), 2);
        assert_eq!(shadows[0].spread_radius, px(FOCUS_GAP + FOCUS_RING));
        assert_eq!(shadows[1].spread_radius, px(FOCUS_GAP));
        assert_eq!(shadows[1].color.a, 1.);
    }
}
