//! 能力库正文：只画当前页签那一页。页头和页签在 [`CapabilitiesPage`] 的 render 里。

use super::*;
use api::model::pi_plugin_catalog::PiPluginKind;
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};
use smelt_plugin_api::ui::{Badge, TextInput};

mod auth;
mod mcp;
mod memory;
mod models;
mod plugins;
mod uses;

pub(crate) fn content(
    page: &CapabilitiesPage,
    window: &mut Window,
    cx: &mut Context<CapabilitiesPage>,
) -> AnyElement {
    let entity = cx.entity();
    match page.tab {
        // 模型、MCP 页自己排版：表单就地展开，不走设置页「名字左、控件右」的行。
        CapabilitiesTab::Models => models::pi_model_page(entity, page, cx),
        CapabilitiesTab::Mcp => mcp::pi_mcp_page(entity, page, cx),
        CapabilitiesTab::Skills => {
            plugins::pi_plugin_page(entity, page, PiPluginKind::Skill, cx).into_any_element()
        }
        CapabilitiesTab::Extensions => {
            plugins::pi_plugin_page(entity, page, PiPluginKind::Extension, cx).into_any_element()
        }
        CapabilitiesTab::Memory => {
            memory::pi_memory_page(entity, page, window, cx).into_any_element()
        }
    }
}

/// 页首一段说明：整页是干什么的、眼下什么情况。不进卡片。
fn page_intro(text: impl Into<SharedString>) -> Div {
    div()
        .px_4()
        .type_style(TYPE.ui)
        .text_color(rgb(ui_theme::muted()))
        .child(text.into())
}
