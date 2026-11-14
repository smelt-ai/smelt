//! 页面里共用的小块：卡片、表单框、分段。

use crate::prelude::*;

/// 内容卡：12px 圆角加发丝描边。智能体空状态、自动化表和表单共用。
pub(crate) fn content_card_style<E: Styled>(element: E) -> E {
    element
        .rounded(ui_theme::card_radius())
        .border_1()
        .border_color(ui_theme::card_stroke())
        .bg(rgb(ui_theme::bg_card()))
}

pub(crate) fn content_card() -> Div {
    content_card_style(div())
}

pub(crate) fn agent_surface_card() -> Div {
    content_card()
}

pub(crate) fn render_agent_error(error: String) -> AnyElement {
    gpui_component::alert::Alert::error("agent-error", error).into_any_element()
}

pub(crate) fn editor_field_frame() -> Div {
    div()
        .flex()
        .items_center()
        .flex_none()
        .h(ui_theme::height::sm())
        .overflow_hidden()
        .border_1()
        .border_color(rgb(ui_theme::border_loud()))
        .rounded(px(8.))
        .bg(rgb(ui_theme::bg_column()))
}

pub(crate) fn framed_text_input(input: &Entity<InputState>, aria: &'static str) -> Div {
    editor_field_frame().child(
        Input::new(input)
            .small()
            .appearance(false)
            .focus_bordered(false)
            .focus_ring(false)
            .w_full()
            .aria_label(aria),
    )
}

pub(crate) fn framed_number_input(input: &Entity<InputState>) -> Div {
    editor_field_frame().child(
        NumberInput::new(input)
            .small()
            .appearance(false)
            .focus_ring(false)
            .w_full(),
    )
}

pub(crate) fn framed_automation_textarea(textarea: Textarea) -> AnyElement {
    div()
        .w_full()
        .px_3()
        .py_3()
        .min_h(px(120.))
        .child(textarea.appearance(false))
        .into_any_element()
}

pub(crate) fn compact_chip(id: impl Into<ElementId>) -> Button {
    Button::new(id).quiet().compact()
}

pub(crate) fn automation_form_shell() -> Div {
    content_card().w_full()
}

/// 一次运行的三行字：标题、时间或来源，失败时再加一行红字。目录和编辑器共用。
/// 标题不自己定字色，跟着行走：行被选中时换成选中字色（见 `ListRow`）。
pub(crate) fn automation_run_lines(title: String, detail: String, error: Option<String>) -> Div {
    div()
        .min_w_0()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_sm()
                .font_medium()
                .overflow_hidden()
                .child(truncated_line(title)),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(ui_theme::text_muted()))
                .overflow_hidden()
                .child(truncated_line(detail)),
        )
        .when_some(error, |column, error| {
            column.child(
                div()
                    .text_xs()
                    .text_color(rgb(ui_theme::red()))
                    .overflow_hidden()
                    .child(truncated_line(error)),
            )
        })
}

pub(crate) fn automation_form_row() -> Div {
    automation_form_shell()
        .h(ui_theme::height::xl())
        .px_3()
        .flex()
        .items_center()
}

pub(crate) fn automation_section_label(label: &'static str) -> AnyElement {
    div()
        .text_sm()
        .font_medium()
        .text_color(rgb(ui_theme::text_bright()))
        .child(label)
        .into_any_element()
}

/// 几个互斥选项的分段开关，选中块在选项之间滑动。`on_select` 拿到被点的下标。
pub(crate) fn choice_tabs(
    id: impl Into<ElementId>,
    selected: Option<usize>,
    tabs: impl IntoIterator<Item = Tab>,
    on_select: impl Fn(usize, &mut App) + 'static,
) -> TabBar {
    TabBar::new(id)
        .segmented()
        .flex_none()
        .when_some(selected, |bar, ix| bar.selected_index(ix))
        .on_click(move |ix, _, cx| on_select(*ix, cx))
        .children(tabs)
}
