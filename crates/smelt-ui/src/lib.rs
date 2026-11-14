//! 共享 GPUI UI 基建：主 GUI 与 `smelt-acp-view` 渲染层共用。跟
//! `smelt-core` 的分工——那边不许引 GPUI，这边允许，但只放「不专属某个具体
//! 页面、被多处复用」的东西，别把整个应用的 UI 都塞进来。

pub mod acp_completion;
pub mod button;
pub mod frame_stats;
pub mod global_wait;
pub mod icon_action;
pub mod image;
pub mod list_row;
pub mod markdown_links;
mod markdown_literal_html;
pub mod markdown_mermaid;
pub mod motion;
pub mod theme_catalog;
pub mod truncated_line;
pub mod ui_theme;

pub use button::Button;
pub use icon_action::IconAction;
pub use list_row::ListRow;

/// 组件库没有垃圾桶：`Delete` 是带 X 的退格键，`CircleX` 是带圈的关。
/// 删除/移除统一用这枚 Lucide trash-2。
pub fn trash_icon() -> gpui_component::Icon {
    gpui_component::Icon::empty().path("smelt-icons/trash.svg")
}
