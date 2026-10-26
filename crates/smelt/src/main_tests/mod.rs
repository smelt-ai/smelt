//! GUI 工作台单测：按主题拆成子模块，从 `main.rs` 迁出。

use super::*;

mod acp_agent_tests;
mod agent_icon_tests;
mod agent_route_alignment_tests;
mod daemon_delivery_ownership_tests;
mod daemon_watch_tests;
mod daemon_state_notification_tests;
mod internal_file_url_tests;
mod key_binding_tests;
mod main_window_liveness_tests;
mod overlay_architecture_tests;
mod pane_state_tests;
mod project_tests;
mod remote_terminal_bench;
mod remote_session_catalog_global_tests;
mod resizable_split_tests;
mod session_facts_tests;
mod session_route_tests;
mod sidebar_order_tests;
mod sidebar_width_tests;
mod term_dock_tests;
mod workspace_state_tests;
