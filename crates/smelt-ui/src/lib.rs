//! 共享 GPUI UI 基建：主 GUI 与 `smelt-acp-view` 渲染层共用。跟
//! `smelt-core` 的分工——那边不许引 GPUI，这边允许，但只放「不专属某个具体
//! 页面、被多处复用」的东西，别把整个应用的 UI 都塞进来。

pub mod acp_completion;
pub mod agent_host_state;
pub mod automation;
pub mod button;
pub mod daemon_states_global;
pub mod frame_stats;
pub mod global_wait;
pub mod history_titles_global;
pub mod icon_action;
pub mod image;
pub mod list_row;
pub mod markdown_links;
mod markdown_literal_html;
pub mod markdown_mermaid;
pub mod motion;
pub mod provider_status_global;
pub mod theme_catalog;
pub mod theme_files;
pub mod ui_theme;

pub use button::Button;
pub use icon_action::IconAction;
pub use list_row::ListRow;
