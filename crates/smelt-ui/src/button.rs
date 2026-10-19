//! 产品按钮。调用方选语义角色和三档尺寸，颜色只来自主题 token。
//!
//! 不把 gpui-component 的 variant / size / `focus_ring` 再暴露出去。那些方法
//! 留在库按钮上，这里在 `render` 里按角色应用一次。后写的角色覆盖先写的，
//! 跟库的 `with_variant` 一样。

use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, Styled, Window,
};
use gpui_component::button::{
    Button as KitButton, ButtonCustomVariant, ButtonRounded, ButtonVariants as _,
};
use gpui_component::menu::DropdownMenu;
use gpui_component::{ActiveTheme as _, Disableable, Icon, Selectable, Sizable as _, Size};

/// 这一块界面里按钮扮演的角色。
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Role {
    /// 工具条、行内图标。库的 ghost。
    Quiet,
    /// 普通命令，包括「新增」。库的 default。
    #[default]
    Normal,
    /// 有边框，但不是这块的主提交。
    Outline,
    /// 这块界面里唯一的默认提交：发送、创建、允许。库的 primary。
    Commit,
    /// 不可逆的确认。可撤销的行内操作仍用 [`Role::Quiet`]。
    Danger,
    /// 危险，但用描边而不是实心，留给和主提交并排的次级危险操作。
    DangerOutline,
    /// 分段选择。配合 [`.selected()`](Button::selected)，不要用「✓」前缀。
    Choice,
    /// 浮在内容上的表面，例如「回到最新」。用 popover token，不在调用点铺底色。
    Surface,
    /// 离开当前应用的地址。库的 link。
    Link,
}

/// 只有三档，对齐库的 20 / 24 / 32 rem 步长。没有 large：库的 large 并不比 medium 更高。
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tier {
    /// 20 见方，图标 16，圆角用主题 `sm`。
    Icon,
    /// 24。工具条和设置行。
    Compact,
    /// 32。默认。
    #[default]
    Default,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Shape {
    #[default]
    Standard,
    /// 高度的一半，对话里的主操作胶囊共用一个轮廓。
    Capsule,
    /// 正圆。主题半径为 0 时跟着变成直角。
    Circle,
}

/// 桌面按钮。尺寸和角色在渲染时落到库按钮上，调用点不再写 px 高度或 variant。
#[derive(IntoElement)]
pub struct Button {
    inner: KitButton,
    role: Role,
    tier: Tier,
    shape: Shape,
    /// 关闭页签：quiet 的图标钮，悬停时字色改成 danger。
    dismiss: bool,
    labeled: bool,
    has_children: bool,
}

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            inner: KitButton::new(id),
            role: Role::Normal,
            tier: Tier::Default,
            shape: Shape::Standard,
            dismiss: false,
            labeled: false,
            has_children: false,
        }
    }

    pub fn quiet(mut self) -> Self {
        self.role = Role::Quiet;
        self.dismiss = false;
        self
    }

    pub fn normal(mut self) -> Self {
        self.role = Role::Normal;
        self.dismiss = false;
        self
    }

    pub fn outline(mut self) -> Self {
        self.role = Role::Outline;
        self.dismiss = false;
        self
    }

    pub fn commit(mut self) -> Self {
        self.role = Role::Commit;
        self.dismiss = false;
        self
    }

    pub fn danger(mut self) -> Self {
        self.role = Role::Danger;
        self.dismiss = false;
        self
    }

    pub fn danger_outline(mut self) -> Self {
        self.role = Role::DangerOutline;
        self.dismiss = false;
        self
    }

    pub fn choice(mut self) -> Self {
        self.role = Role::Choice;
        self.dismiss = false;
        self
    }

    /// 浮层表面。底色、字色、悬停都在渲染时从主题 popover / button 读，调用点不要再 `.bg()`。
    pub fn surface(mut self) -> Self {
        self.role = Role::Surface;
        self.dismiss = false;
        self
    }

    /// 外部地址。下划线和手型光标由库的 link 变体提供。
    pub fn link(mut self) -> Self {
        self.role = Role::Link;
        self.dismiss = false;
        self
    }

    /// 关闭控件：20 图标钮，悬停字色为 danger。焦点和禁用跟普通按钮一样。
    pub fn dismiss(mut self) -> Self {
        self.role = Role::Quiet;
        self.tier = Tier::Icon;
        self.dismiss = true;
        self
    }

    /// 20 见方，字形 16。
    pub fn icon_size(mut self) -> Self {
        self.tier = Tier::Icon;
        self
    }

    /// 24。工具条和设置行。
    pub fn compact(mut self) -> Self {
        self.tier = Tier::Compact;
        self
    }

    /// 高度的一半做圆角。对话主操作胶囊用这一档，和发送圆钮是同一个 32 高的轮廓。
    pub fn capsule(mut self) -> Self {
        self.shape = Shape::Capsule;
        self
    }

    /// 正圆。发送、停止、添加附件。
    pub fn circle(mut self) -> Self {
        self.shape = Shape::Circle;
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.labeled = true;
        self.inner = self.inner.label(label);
        self
    }

    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.inner = self.inner.icon(icon);
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.inner = self.inner.on_click(handler);
        self
    }

    pub fn on_hover(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.inner = self.inner.on_hover(handler);
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.inner = self.inner.tooltip(tooltip);
        self
    }

    pub fn tooltip_placement(mut self, placement: gpui_component::Placement) -> Self {
        self.inner = self.inner.tooltip_placement(placement);
        self
    }

    pub fn tooltip_with_action(
        mut self,
        tooltip: impl Into<SharedString>,
        action: &dyn gpui::Action,
        context: Option<&str>,
    ) -> Self {
        self.inner = self.inner.tooltip_with_action(tooltip, action, context);
        self
    }

    pub fn accessibility_label(mut self, label: impl Into<SharedString>) -> Self {
        self.inner = self.inner.accessibility_label(label);
        self
    }

    pub fn accessibility_id(mut self, id: impl Into<SharedString>) -> Self {
        self.inner = self.inner.accessibility_id(id);
        self
    }

    pub fn loading(mut self, loading: bool) -> Self {
        self.inner = self.inner.loading(loading);
        self
    }

    pub fn loading_icon(mut self, icon: impl Into<Icon>) -> Self {
        self.inner = self.inner.loading_icon(icon);
        self
    }

    pub fn dropdown_caret(mut self, dropdown_caret: bool) -> Self {
        self.inner = self.inner.dropdown_caret(dropdown_caret);
        self
    }

    pub fn tab_index(mut self, tab_index: isize) -> Self {
        self.inner = self.inner.tab_index(tab_index);
        self
    }

    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.inner = self.inner.tab_stop(tab_stop);
        self
    }

    pub fn toggled(mut self, toggled: bool) -> Self {
        self.inner = self.inner.toggled(toggled);
        self
    }

    pub fn disabled(self, disabled: bool) -> Self {
        Disableable::disabled(self, disabled)
    }

    pub fn selected(self, selected: bool) -> Self {
        Selectable::selected(self, selected)
    }

    pub fn open(self, open: bool) -> Self {
        Selectable::open(self, open)
    }

    fn apply_role(self_role: Role, button: KitButton, cx: &mut App) -> KitButton {
        match self_role {
            Role::Quiet => button.ghost(),
            Role::Normal => button,
            Role::Outline => button.outline(),
            Role::Commit => button.primary(),
            Role::Danger => button.danger(),
            Role::DangerOutline => button.danger().outline(),
            Role::Choice => button.secondary(),
            Role::Link => button.link(),
            Role::Surface => {
                let theme = cx.theme();
                let popover = theme.popover;
                let foreground = theme.popover_foreground;
                let hover = theme.button_hover;
                let active = theme.button_active;
                // 库把 custom 底色和透明色按 0.2 混合。这里用 popover 实色盖回去，
                // 调用点不用自己再写 `.bg()`。
                button
                    .custom(
                        ButtonCustomVariant::new(cx)
                            .color(popover)
                            .foreground(foreground)
                            .hover(hover)
                            .active(active)
                            .shadow(true),
                    )
                    .bg(popover)
            }
        }
    }

    fn apply_tier(button: KitButton, tier: Tier, shape: Shape, icon_only: bool) -> KitButton {
        // 图标尺寸是库的私有字段，只能跟着 size 档走。图标档要 16px 字形
        // （Medium）塞进 20px 盒子（size_5）；refine 会盖过库按 Medium 画出的 32px 盒子。
        let icon_box = icon_only || shape == Shape::Circle;
        let button = match tier {
            Tier::Icon if icon_box => button.with_size(Size::Medium),
            Tier::Icon => button.with_size(Size::XSmall),
            Tier::Compact => button.with_size(Size::Small),
            Tier::Default => button.with_size(Size::Medium),
        };
        if !icon_box {
            return button;
        }
        match tier {
            Tier::Icon => button.p_0().size_5(),
            Tier::Compact => button.p_0().size_6(),
            Tier::Default => button.p_0().size_8(),
        }
    }

    fn apply_shape(
        button: KitButton,
        tier: Tier,
        shape: Shape,
        window: &Window,
        cx: &App,
    ) -> KitButton {
        match shape {
            Shape::Circle => button.rounded(ButtonRounded::Size(cx.theme().radius_full())),
            Shape::Capsule => {
                let rem = window.rem_size();
                let radius = match tier {
                    Tier::Icon => rem * 0.625,
                    Tier::Compact => rem * 0.75,
                    Tier::Default => rem * 1.0,
                };
                button.rounded(ButtonRounded::Size(radius))
            }
            Shape::Standard if tier == Tier::Icon => button.rounded(ButtonRounded::Small),
            Shape::Standard => button,
        }
    }
}

impl Disableable for Button {
    fn disabled(mut self, disabled: bool) -> Self {
        self.inner = self.inner.disabled(disabled);
        self
    }
}

impl Selectable for Button {
    fn selected(mut self, selected: bool) -> Self {
        self.inner = self.inner.selected(selected);
        self
    }

    fn is_selected(&self) -> bool {
        self.inner.is_selected()
    }

    fn open(mut self, open: bool) -> Self {
        self.inner = self.inner.open(open);
        self
    }

    fn is_open(&self) -> bool {
        self.inner.is_open()
    }
}

impl Styled for Button {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        self.inner.style()
    }
}

impl ParentElement for Button {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.has_children = true;
        self.inner.extend(elements);
    }
}

impl InteractiveElement for Button {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.inner.interactivity()
    }
}

impl DropdownMenu for Button {}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Button {
            inner,
            role,
            tier,
            shape,
            dismiss,
            labeled,
            has_children,
        } = self;
        let icon_only = !labeled && !has_children;
        let mut button = Self::apply_role(role, inner, cx);
        button = Self::apply_tier(button, tier, shape, icon_only);
        button = Self::apply_shape(button, tier, shape, window, cx);
        if dismiss {
            let danger = cx.theme().danger;
            button = button.hover(move |style| style.text_color(danger));
        }
        button
    }
}

impl From<Button> for AnyElement {
    fn from(button: Button) -> Self {
        button.into_any_element()
    }
}
