//! 文本框、多行文本框和下拉框。规则见 `docs/ui-design-language.md` 第 4、6 节。
//!
//! 都是 `surface` 底、1px `hair_strong` 描边。默认 36 高、圆角 12，用在表单和对话框里；
//! `small` 28 高、圆角 8，用在工具栏里的筛选框和行内改名，跟紧凑列表行一样高。
//!
//! 建在组件库的输入框、多行输入框和下拉框上：光标、输入法、右键菜单、搜索列表都归它们，
//! 这里只定外形。聚焦沿用组件库的画法：描边换成 `focus`，外圈 3px 半透明的 `focus`。
//! 鼠标点进去也画，因为文字要往哪里打，得一直看得出来；这一点和按钮不同。
//!
//! 调用方写的样式（宽度、字体、字号）最后盖上去，所以也能改高度，比如页头里的大号名字框。

use gpui::{
    App, Entity, IntoElement, Length, Pixels, RenderOnce, SharedString, StyleRefinement, Styled,
    Window, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::input::{Input, InputState, Textarea, TextareaState};
use gpui_component::select::{Select as KitSelect, SelectDelegate, SelectItem, SelectState};
use gpui_component::{Sizable as _, Size, StyledExt as _};

use crate::button::DISABLED_OPACITY;
use crate::ui_theme;

const HEIGHT: f32 = 36.;
const RADIUS: f32 = 12.;
const SMALL_HEIGHT: f32 = 28.;
const SMALL_RADIUS: f32 = 8.;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Density {
    #[default]
    Regular,
    Small,
}

impl Density {
    fn height(self) -> Pixels {
        px(match self {
            Self::Regular => HEIGHT,
            Self::Small => SMALL_HEIGHT,
        })
    }

    fn radius(self) -> Pixels {
        px(match self {
            Self::Regular => RADIUS,
            Self::Small => SMALL_RADIUS,
        })
    }

    /// 组件库按这一档定内边距和字号：默认是正文 14，小号也不缩字。
    fn kit_size(self) -> Size {
        match self {
            Self::Regular => Size::Medium,
            Self::Small => Size::Small,
        }
    }
}

/// 三种控件共用的外形：底、描边、圆角。高度单独给，多行文本框跟着内容长。
fn frame<E: Styled>(element: E, density: Density) -> E {
    element
        .rounded(density.radius())
        .bg(rgb(ui_theme::surface()))
        .border_color(ui_theme::hair_strong())
}

/// 单行文本框。
#[derive(IntoElement)]
pub struct TextInput {
    input: Input,
    style: StyleRefinement,
    density: Density,
    disabled: bool,
}

impl TextInput {
    pub fn new(state: &Entity<InputState>) -> Self {
        Self {
            input: Input::new(state),
            style: StyleRefinement::default(),
            density: Density::Regular,
            disabled: false,
        }
    }

    /// 28 高：工具栏里的筛选框、列表行里的改名。
    pub fn small(mut self) -> Self {
        self.density = Density::Small;
        self
    }

    /// 前缀，通常是一个图标（搜索框的放大镜）。
    pub fn prefix(mut self, prefix: impl IntoElement) -> Self {
        self.input = self.input.prefix(prefix);
        self
    }

    /// 有字时末尾出一个清空按钮。筛选框用。
    pub fn cleanable(mut self, cleanable: bool) -> Self {
        self.input = self.input.cleanable(cleanable);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 给读屏的名字。不给时用占位文字；旁边没有可见标题、也没有占位文字时一定要给。
    pub fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.input = self.input.aria_label(label);
        self
    }
}

impl Styled for TextInput {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for TextInput {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let Self {
            input,
            style,
            density,
            disabled,
        } = self;
        frame(input.with_size(density.kit_size()), density)
            .h(density.height())
            .disabled(disabled)
            .when(disabled, |this| this.opacity(DISABLED_OPACITY))
            .refine_style(&style)
    }
}

/// 多行文本框：高度跟着内容长，上下限在状态上定（`auto_grow`）。
#[derive(IntoElement)]
pub struct TextArea {
    textarea: Textarea,
    style: StyleRefinement,
    disabled: bool,
}

impl TextArea {
    pub fn new(state: &Entity<TextareaState>) -> Self {
        Self {
            textarea: Textarea::new(state),
            style: StyleRefinement::default(),
            disabled: false,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.textarea = self.textarea.aria_label(label);
        self
    }
}

impl Styled for TextArea {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for TextArea {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let Self {
            textarea,
            style,
            disabled,
        } = self;
        frame(textarea, Density::Regular)
            .disabled(disabled)
            .when(disabled, |this| this.opacity(DISABLED_OPACITY))
            .refine_style(&style)
    }
}

/// 下拉框：从一组固定选项里选一项，选项多时可以搜。
#[derive(IntoElement)]
pub struct Select<D>
where
    D: SelectDelegate + 'static,
    <D::Item as SelectItem>::Value: PartialEq + Clone,
{
    select: KitSelect<D>,
    style: StyleRefinement,
}

impl<D> Select<D>
where
    D: SelectDelegate + 'static,
    <D::Item as SelectItem>::Value: PartialEq + Clone,
{
    pub fn new(state: &Entity<SelectState<D>>) -> Self {
        Self {
            select: KitSelect::new(state),
            style: StyleRefinement::default(),
        }
    }

    /// 弹出的选项列表宽度；不给时和框一样宽。
    pub fn menu_width(mut self, width: impl Into<Length>) -> Self {
        self.select = self.select.menu_width(width);
        self
    }

    /// 可搜索时搜索框里的占位文字。
    pub fn search_placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.select = self.select.search_placeholder(placeholder);
        self
    }
}

impl<D> Styled for Select<D>
where
    D: SelectDelegate + 'static,
    <D::Item as SelectItem>::Value: PartialEq + Clone,
{
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl<D> RenderOnce for Select<D>
where
    D: SelectDelegate + 'static,
    <D::Item as SelectItem>::Value: PartialEq + Clone,
{
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let Self { select, style } = self;
        frame(
            select.with_size(Density::Regular.kit_size()),
            Density::Regular,
        )
        .h(Density::Regular.height())
        .refine_style(&style)
    }
}
