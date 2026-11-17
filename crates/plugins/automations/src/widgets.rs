//! 页面里共用的小块：卡片、表单框、分段。

use crate::prelude::*;
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

/// 内容卡：12px 圆角加发丝描边。智能体空状态、自动化表和表单共用。
pub(crate) fn content_card_style<E: Styled>(element: E) -> E {
    element
        .rounded(ui_theme::card_radius())
        .border_1()
        .border_color(ui_theme::hair())
        .bg(rgb(ui_theme::surface()))
}

pub(crate) fn content_card() -> Div {
    content_card_style(div())
}

pub(crate) fn agent_surface_card() -> Div {
    content_card()
}

pub(crate) fn render_agent_error(error: String) -> AnyElement {
    api::ui::Alert::error("agent-error", error).into_any_element()
}

/// 嵌在表单卡里的多行框：外框是卡片，文本框自己不再画。
pub(crate) fn framed_automation_textarea(textarea: TextArea) -> AnyElement {
    div()
        .w_full()
        .px_3()
        .py_3()
        .min_h(px(120.))
        .child(textarea.bare())
        .into_any_element()
}

pub(crate) fn compact_chip(id: impl Into<ElementId>) -> Button {
    Button::new(id).quiet().small()
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
                .type_style(TYPE.ui_medium)
                .overflow_hidden()
                .child(truncated_line(title)),
        )
        .child(
            div()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(ui_theme::muted()))
                .overflow_hidden()
                .child(truncated_line(detail)),
        )
        .when_some(error, |column, error| {
            column.child(
                div()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(ui_theme::bad()))
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
        .type_style(TYPE.ui_medium)
        .text_color(rgb(ui_theme::text()))
        .child(label)
        .into_any_element()
}
