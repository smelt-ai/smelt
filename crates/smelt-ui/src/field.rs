//! 文本框和多行文本框。规则见 `docs/ui-design-language.md` 第 4、6 节。
//!
//! 外形取 [`crate::kit`]：触发器 28 高、圆角 `RADIUS.md`、正文 `TYPE.ui_compact`、`input`
//! 层级投影代替边框，聚焦时换成边内 1px 焦点线。`small` 用于紧凑场景，当前与默认尺寸相同。
//! 多行高度由状态（`auto_grow`）和调用方决定。
//!
//! 编辑、输入法、选区、滚动、自动高度和读屏的值都归 gpui-base 的输入状态；这里只画外框、
//! 把主题色投影给编辑器、给右键菜单（剪切、复制、粘贴、全选，用 [`crate::menu`]）。调用方的
//! Styled 样式最后覆盖默认值。
//!
//! 状态和事件类型（[`InputState`]、[`TextareaState`]、[`InputEvent`]）从这里转出：它们就是
//! 这两个控件接口的一部分，调用方（含插件）从这里拿，不另依赖组件库。键位动作（`Enter`、
//! `Paste` 之类）要拦截时直接用 `gpui_base::input` 的。

use gpui::{
    Action, AnyElement, App, BoxShadow, Edges, Entity, Focusable as _, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, RenderOnce, Role,
    SharedString, StyleRefinement, Styled, TextAlign, Window, div, prelude::FluentBuilder as _, px,
    relative, rgb,
};
use gpui_base::input::{
    Copy, Cut, Input as BaseInput, InputAccessibility, InputBaseState, InputEditorStyle,
    InputModeKind, Paste, SelectAll, Textarea as BaseTextarea,
};
pub use gpui_base::input::{InputEvent, InputState, TextareaState};
use gpui_base::{InputBase, RoleOverride, StyledExt as _};

use crate::button::{Button, DISABLED_OPACITY};
use crate::kit::{GLYPHS, RADIUS, TYPE, elevation};
use crate::menu::{Menu, MenuItem, MenuTrigger};
use crate::ui_theme;

const TRIGGER_HEIGHT: f32 = 28.;
const TEXTAREA_LINE_HEIGHT: f32 = 1.35;
/// 多行编辑区内边距：编辑器自己留，外框不再加。
const TEXTAREA_PADDING: Edges<Pixels> = Edges {
    top: px(8.),
    right: px(10.),
    bottom: px(8.),
    left: px(10.),
};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Density {
    #[default]
    Regular,
    Small,
}

impl Density {
    fn height(self) -> Pixels {
        px(TRIGGER_HEIGHT)
    }
}

/// 两种控件共用的字。
fn text<E: Styled>(element: E) -> E {
    element
        .text_size(px(TYPE.ui_compact.size))
        .text_color(rgb(ui_theme::text()))
}

/// 两种控件共用外形：主内容底 `bg`、`input` 层级（一圈细边加一点下沉），
/// 不画边框。不设高度，避免干扰多行 auto-grow 和显式高度。
fn frame<E: Styled>(element: E) -> E {
    text(element)
        .rounded(px(RADIUS.md))
        .bg(rgb(ui_theme::bg()))
        .shadow(elevation::input())
}

/// 聚焦时在边内画 1px 焦点线（outline 偏移 -1），换掉层级投影。
fn focus_ring() -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.), px(0.), ui_theme::focus().into())
            .spread_radius(px(1.))
            .inset(),
    ]
}

fn input_frame<E: Styled>(element: E, density: Density, bare: bool) -> E {
    // 没有边线，文字和前缀从外沿 8px 开始。
    let element = if bare { text(element) } else { frame(element) };
    element
        .h(density.height())
        .line_height(relative(TYPE.ui_compact.line_height / TYPE.ui_compact.size))
        .pl(px(8.))
        .pr(px(8.))
        .py(px(0.))
        .gap(px(6.))
}

fn textarea_frame<E: Styled>(element: E, bare: bool) -> E {
    // 内边距在编辑器上（TEXTAREA_PADDING），外框再加 padding 会重复计算。
    let element = if bare { text(element) } else { frame(element) };
    element.line_height(relative(TEXTAREA_LINE_HEIGHT))
}

/// 编辑器画字、占位、光标和选区用的颜色，每帧从当前主题取。
fn editor_style() -> InputEditorStyle {
    let text: Hsla = rgb(ui_theme::text()).into();
    InputEditorStyle {
        foreground: text,
        muted_foreground: rgb(ui_theme::faint()).into(),
        background: rgb(ui_theme::bg()).into(),
        border: ui_theme::hair_strong().into(),
        selection: ui_theme::selection().into(),
        caret: text,
        ..InputEditorStyle::default()
    }
}

/// 把展示属性投影到状态上。状态的设置函数值不变时不通知，可以每帧调。
fn project<M: InputModeKind>(
    state: &Entity<InputBaseState<M>>,
    disabled: bool,
    align: Option<TextAlign>,
    accessibility: InputAccessibility,
    cx: &mut App,
) {
    state.update(cx, |state, cx| {
        state.set_editor_style(editor_style());
        state.set_disabled(disabled, cx);
        if let Some(align) = align {
            state.set_text_align(align, cx);
        }
        state.set_accessibility(accessibility);
    });
}

/// 右键菜单。打开时按状态当下的能力决定哪些项可用；点了先把焦点还给输入框再派发动作。
pub(crate) fn edit_menu<M: InputModeKind>(
    state: &Entity<InputBaseState<M>>,
) -> impl Fn(&mut Window, &mut App) -> Menu + 'static {
    let state = state.downgrade();
    move |_, cx| {
        let Some(state) = state.upgrade() else {
            return Menu::new();
        };
        let capabilities = state.read(cx).context_menu_capabilities();
        let focus = state.focus_handle(cx);
        let item = |label: &'static str, enabled: bool, action: fn() -> Box<dyn Action>| {
            let focus = focus.clone();
            MenuItem::new(label)
                .disabled(!enabled)
                .on_click(move |_, window, cx| {
                    focus.focus(window, cx);
                    window.dispatch_action(action(), cx);
                })
        };
        let editable = capabilities.is_editable();
        let copyable = capabilities.is_copyable();
        Menu::new()
            .item(item("剪切", editable && copyable, || Box::new(Cut)))
            .item(item("复制", copyable, || Box::new(Copy)))
            .item(item("粘贴", editable, || Box::new(Paste)))
            .separator()
            .item(item("全选", true, || Box::new(SelectAll)))
    }
}

/// 外框：输入语义归编辑器，外框只是展示；禁用时吞掉鼠标，聚焦时投影换成焦点线（`bare`
/// 没有外框，焦点由外面的容器画）。
fn input_base<M: InputModeKind>(
    state: &Entity<InputBaseState<M>>,
    disabled: bool,
    bare: bool,
    window: &Window,
    cx: &App,
) -> InputBase {
    let focused = !disabled && state.read(cx).has_selection_focus(window, cx);
    InputBase::new(("smelt-input", state.entity_id()))
        .role(RoleOverride::Presentational)
        .semantic_owner(state.entity_id())
        .focused(focused)
        .disabled(disabled)
        .when(!bare, |this| {
            this.styles(|styles| styles.focused(|style| style.shadow(focus_ring())))
        })
        .when(disabled, |this| {
            this.opacity(DISABLED_OPACITY)
                .capture_any_mouse_down(|_, _, cx| cx.stop_propagation())
        })
}

/// 编辑区：可用时包一层右键菜单。
fn editor_area<M: InputModeKind>(
    state: &Entity<InputBaseState<M>>,
    editor: AnyElement,
    disabled: bool,
) -> AnyElement {
    if disabled {
        return div().flex_1().min_w_0().child(editor).into_any_element();
    }
    MenuTrigger::context(
        ("smelt-input-menu", state.entity_id()),
        editor,
        edit_menu(state),
    )
    .flex_1()
    .min_w_0()
    .into_any_element()
}

/// 单行文本框。
#[derive(IntoElement)]
pub struct TextInput {
    state: Entity<InputState>,
    style: StyleRefinement,
    density: Density,
    prefix: Option<AnyElement>,
    cleanable: bool,
    disabled: bool,
    bare: bool,
    label: Option<SharedString>,
}

impl TextInput {
    pub fn new(state: &Entity<InputState>) -> Self {
        Self {
            state: state.clone(),
            style: StyleRefinement::default(),
            density: Density::Regular,
            prefix: None,
            cleanable: false,
            disabled: false,
            bare: false,
            label: None,
        }
    }

    /// 嵌进自己就是外框的浮层（下拉里的搜索框）：不画底、投影和焦点线，
    /// 光标就是焦点。高度、内边距、字照旧。
    pub fn bare(mut self) -> Self {
        self.bare = true;
        self
    }

    /// 紧凑场景：工具栏里的筛选框、列表行里的改名。
    pub fn small(mut self) -> Self {
        self.density = Density::Small;
        self
    }

    /// 前缀，通常是一个图标（搜索框的放大镜）。
    pub fn prefix(mut self, prefix: impl IntoElement) -> Self {
        self.prefix = Some(prefix.into_any_element());
        self
    }

    /// 有字时末尾出一个清空按钮。筛选框用。
    pub fn cleanable(mut self, cleanable: bool) -> Self {
        self.cleanable = cleanable;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 给读屏的名字。不给时用占位文字；旁边没有可见标题、也没有占位文字时一定要给。
    pub fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Styled for TextInput {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for TextInput {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            state,
            style,
            density,
            prefix,
            cleanable,
            disabled,
            bare,
            label,
        } = self;
        let presentation = state.read(cx).presentation();
        let masked = presentation.is_masked();
        let placeholder = Some(presentation.placeholder().clone()).filter(|p| !p.is_empty());
        // 掩码格式生成的占位（如 "(___)___"）不当名字念。
        let label = label.or_else(|| {
            placeholder
                .clone()
                .filter(|p| presentation.mask_placeholder() != Some(p.as_ref()))
        });
        let role = if masked {
            Role::PasswordInput
        } else {
            Role::TextInput
        };
        project(
            &state,
            disabled,
            style.text.text_align,
            InputAccessibility::new(Some(role))
                .with_label(label)
                .with_placeholder(placeholder)
                .with_value_exposed(!masked),
            cx,
        );
        let show_clear = cleanable
            && presentation.is_editable()
            && !presentation.is_loading()
            && state.read(cx).text().len() > 0;
        let editor = BaseInput::new(&state).into_any_element();
        input_frame(
            input_base(&state, disabled, bare, window, cx),
            density,
            bare,
        )
        .flex()
        .items_center()
        .children(prefix)
        .child(editor_area(&state, editor, disabled))
        .when(show_clear, |this| {
            let state = state.clone();
            this.child(
                Button::new("clear")
                    .quiet()
                    .small()
                    .icon(GLYPHS.close)
                    .accessibility_label("清空")
                    .on_click(move |_, window, cx| {
                        state.update(cx, |state, cx| {
                            state.clean(window, cx);
                            state.focus(window, cx);
                        })
                    }),
            )
        })
        .refine_style(&style)
    }
}

/// 多行文本框：高度跟着内容长，上下限在状态上定（`auto_grow`）。
#[derive(IntoElement)]
pub struct TextArea {
    state: Entity<TextareaState>,
    style: StyleRefinement,
    disabled: bool,
    bare: bool,
    label: Option<SharedString>,
}

impl TextArea {
    pub fn new(state: &Entity<TextareaState>) -> Self {
        Self {
            state: state.clone(),
            style: StyleRefinement::default(),
            disabled: false,
            bare: false,
            label: None,
        }
    }

    /// 嵌进自己画外框的容器（对话输入栏、表单卡）：不画底、描边和圆角，聚焦时的样子也归
    /// 容器。编辑区内边距、字和右键菜单照旧。
    pub fn bare(mut self) -> Self {
        self.bare = true;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Styled for TextArea {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for TextArea {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            state,
            style,
            disabled,
            bare,
            label,
        } = self;
        let placeholder =
            Some(state.read(cx).presentation().placeholder().clone()).filter(|p| !p.is_empty());
        project(
            &state,
            disabled,
            None,
            InputAccessibility::new(Some(Role::MultilineTextInput))
                .with_label(label.or_else(|| placeholder.clone()))
                .with_placeholder(placeholder),
            cx,
        );
        state.update(cx, |state, _| state.set_editor_paddings(TEXTAREA_PADDING));
        let editor = div()
            .relative()
            .size_full()
            .child(BaseTextarea::new(&state))
            .into_any_element();
        textarea_frame(input_base(&state, disabled, bare, window, cx), bare)
            .flex()
            .w_full()
            .h_auto()
            .child(editor_area(&state, editor, disabled))
            .refine_style(&style)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Refineable as _, TextStyle, div};

    fn resolved_line_height(style: &StyleRefinement) -> Pixels {
        let mut text = TextStyle::default();
        text.refine(&style.text);
        text.line_height_in_pixels(px(16.))
    }

    #[test]
    fn line_heights_follow_the_final_font_size() {
        let mut input = input_frame(div(), Density::Regular, false);
        let mut small_input = input_frame(div(), Density::Small, false);
        let mut area = textarea_frame(div(), false);
        let mut custom = div().text_size(px(20.));
        let custom = custom.style().clone();

        for (style, default_height, custom_height, ratio) in [
            (input.style(), 16., 25., 16. / 13.),
            (small_input.style(), 16., 25., 16. / 13.),
            (area.style(), 18., 27., 1.35),
        ] {
            assert_eq!(style.text.line_height, Some(relative(ratio)));
            assert_eq!(resolved_line_height(style), px(default_height));
            let mut styled = div().refine_style(style).refine_style(&custom);
            assert_eq!(resolved_line_height(styled.style()), px(custom_height));
        }
    }

    #[test]
    fn caller_line_height_overrides_defaults() {
        let mut fixed = div().text_size(px(20.)).line_height(px(32.));
        let mut relative_override = div().text_size(px(20.)).line_height(relative(1.5));
        let overrides = [
            (fixed.style().clone(), px(32.)),
            (relative_override.style().clone(), px(30.)),
        ];

        for (custom, expected) in overrides {
            let mut input = input_frame(div(), Density::Regular, false).refine_style(&custom);
            let mut small_input = input_frame(div(), Density::Small, false).refine_style(&custom);
            let mut area = textarea_frame(div(), false).refine_style(&custom);
            for style in [input.style(), small_input.style(), area.style()] {
                assert_eq!(style.text.line_height, custom.text.line_height);
                assert_eq!(resolved_line_height(style), expected);
            }
        }
    }

    #[test]
    fn trigger_metrics_apply_to_regular_and_small() {
        let mut expected = div()
            .h(px(28.))
            .rounded(px(6.))
            .text_size(px(13.))
            .line_height(relative(16. / 13.));
        let expected = expected.style().clone();
        for density in [Density::Regular, Density::Small] {
            let mut input = input_frame(div(), density, false);
            let style = input.style();
            assert_eq!(style.size.height, expected.size.height);
            assert_eq!(style.corner_radii, expected.corner_radii);
            assert_eq!(style.text.font_size, expected.text.font_size);
            assert_eq!(style.text.line_height, expected.text.line_height);
            assert_eq!(style.padding.left, Some(px(8.).into()));
            assert_eq!(style.padding.right, Some(px(8.).into()));
            // 外框靠层级投影，不画边框。
            assert!(style.border_widths.left.is_none());
            assert!(style.box_shadow.as_ref().is_some_and(|s| !s.is_empty()));
        }
        let mut bare = input_frame(div(), Density::Regular, true);
        let bare = bare.style();
        assert_eq!(bare.size.height, expected.size.height);
        assert!(bare.background.is_none() && bare.box_shadow.is_none());
    }

    #[test]
    fn focus_ring_is_one_inset_pixel() {
        let ring = focus_ring();
        assert_eq!(ring.len(), 1);
        assert!(ring[0].inset);
        assert_eq!(ring[0].spread_radius, px(1.));
    }

    #[test]
    fn textarea_keeps_height_and_padding_under_upstream_control() {
        let mut area = textarea_frame(div(), false);
        assert!(area.style().size.height.is_none());
        assert!(area.style().min_size.height.is_none());
        assert!(area.style().padding.left.is_none());
        assert!(area.style().padding.top.is_none());

        // git 提交框可以小于多行输入 80px 的设计最小高度。
        let mut custom = div().h(px(52.)).text_size(px(15.)).px(px(12.));
        let custom = custom.style().clone();
        let mut area = textarea_frame(div(), false).refine_style(&custom);
        assert_eq!(area.style().size.height, custom.size.height);
        assert_eq!(area.style().min_size.height, None);
        assert_eq!(area.style().text.font_size, custom.text.font_size);
        assert_eq!(area.style().padding.left, custom.padding.left);
    }

    #[test]
    fn bare_textarea_keeps_text_but_drops_the_frame() {
        let mut framed = textarea_frame(div(), false);
        let mut bare = textarea_frame(div(), true);
        let (framed, bare) = (framed.style(), bare.style());
        assert_eq!(bare.text.font_size, framed.text.font_size);
        assert_eq!(bare.text.line_height, framed.text.line_height);
        assert_eq!(bare.text.color, framed.text.color);
        assert!(bare.background.is_none());
        assert!(bare.box_shadow.is_none());
        assert!(framed.box_shadow.is_some());
        assert!(bare.corner_radii.top_left.is_none());
    }

    #[test]
    fn trigger_caller_style_wins() {
        let mut custom = div().h(px(40.)).rounded(px(10.)).pl(px(12.));
        let custom = custom.style().clone();
        let mut input = input_frame(div(), Density::Small, false).refine_style(&custom);
        let style = input.style();
        assert_eq!(style.size.height, custom.size.height);
        assert_eq!(style.corner_radii, custom.corner_radii);
        assert_eq!(style.padding.left, custom.padding.left);
    }
}
