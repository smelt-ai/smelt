//! GUI（workspace）与守护（smeltd / gateway）共用的无 UI 逻辑。
//!
//! 历史上这些模块散在根 crate 的 src/ 下、靠 `#[path]` 编进各二进制，代价是每个
//! bin 重复编译一遍、dead_code 误报、依赖边界全靠自觉。收进 lib 后由编译器守边界：
//! 本 crate 不许出现 GPUI 依赖。

pub use smelt_model::acp_chat;
pub mod acp_conn;
pub mod acp_session;
pub mod acp_terminal;
pub mod agent_bus;
pub use smelt_model::agent_definition;
pub mod agent_definition_store;
pub mod agent_event;
pub mod agent_hooks;
pub mod agent_host;
pub mod agent_kind;
pub mod agent_memory;
pub use smelt_model::agent_status;
pub mod antigravity_history;
pub use smelt_sys::app_log;
pub mod appearance_settings;
pub use smelt_model::attention;
pub mod auto_model_store;
pub use smelt_model::automation;
pub mod automation_store;
pub mod automation_transcript;
pub mod background_tasks;
pub use smelt_sys::block_on;
pub mod claude_paths;
pub mod client_migration;
pub mod codex_app_server;
pub use smelt_model::control_api;
pub mod control_skill;
pub use smelt_model::conversation;
pub mod copilot_quota;
pub use smelt_sys::daemon_image;
pub mod daemon_state;
pub mod default_conversation;
pub mod dsh_auto_models;
pub mod dsh_service;
pub use smelt_model::font_config;
pub use smelt_sys::fd_limit;
pub mod fs;
pub mod history_service;
pub mod launch_settings;
pub mod login_env;
#[cfg(unix)]
pub mod managed_runtime;
pub mod memory_settings;
pub mod new_session;
pub use smelt_model::osc;
pub mod pi_auth;
pub mod pi_auth_service;
pub mod pi_auto_models;
pub mod pi_mcp;
pub mod pi_model_discovery;
pub mod pi_model_settings;
pub mod pi_plugin_catalog;
pub mod pi_quota;
pub mod pi_rpc;
pub mod pi_service;
pub use smelt_sys::process_wait;
pub mod project_catalog;
pub mod project_files;
pub use smelt_model::provider_api;
pub mod provider_quota;
pub mod quota_service;
pub mod remote_config;
#[cfg(unix)]
pub mod runtime_generation;
pub mod session_control;
pub use smelt_model::session_handoff;
pub mod session_history;
pub mod session_metadata;
pub use smelt_model::session_title;
pub mod sqlite_state;
pub mod subprocess;
pub use smelt_model::term_config;
pub mod term_text;
pub mod terminal_theme;
pub use smelt_model::theme_index;
pub use smelt_sys::tty_color;
pub mod workspace_catalog;
pub mod workspace_menu;
pub use smelt_model::workspace_override;
pub mod worktree_inherit;
