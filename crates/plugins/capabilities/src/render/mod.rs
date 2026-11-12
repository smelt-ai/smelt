//! 能力库正文：只画当前页签那一页。页头和页签在 [`CapabilitiesPage`] 的 render 里。

use super::*;
use gpui_component::input::{Input, InputState};
use gpui_component::tag::Tag;

mod auth;
mod mcp;
mod memory;
mod models;
mod plugins;

pub(crate) fn content(page: &CapabilitiesPage, cx: &Context<CapabilitiesPage>) -> AnyElement {
    let entity = cx.entity();
    let tab = page.tab;
    let pane = match tab {
        // 模型、MCP 页自己排版：居中限宽、表单就地展开，不走 Settings 的「标签左、值右」两栏。
        CapabilitiesTab::Models => return models::pi_model_page(entity, page, cx),
        CapabilitiesTab::Mcp => return mcp::pi_mcp_page(entity, page, cx),
        CapabilitiesTab::Plugins => plugins::pi_plugin_page(entity, page, cx),
        CapabilitiesTab::Memory => memory::pi_memory_page(entity, page),
    };
    div()
        .size_full()
        .child(
            // 借 Settings 的内容排版，关掉它自带的分组侧栏。
            Settings::new(SharedString::from(format!(
                "capabilities-pane:{}",
                tab.key()
            )))
            .sidebar_width(px(0.))
            .sidebar_size_range(px(0.)..px(0.))
            .pages(vec![pane]),
        )
        .into_any_element()
}
