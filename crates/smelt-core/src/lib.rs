//! GUI（workspace）与守护（smeltd / gateway）共用的无 UI 逻辑。
//!
//! 历史上这些模块散在根 crate 的 src/ 下、靠 `#[path]` 编进各二进制，代价是每个
//! bin 重复编译一遍、dead_code 误报、依赖边界全靠自觉。收进 lib 后由编译器守边界：
//! 本 crate 不许出现 GPUI 依赖。

pub mod acp_chat;
pub mod acp_client;
pub mod acp_conn;
pub mod acp_session;
pub mod acp_terminal;
pub mod agent_bus;
pub mod agent_definition;
pub mod agent_definition_store;
pub mod agent_event;
pub mod agent_kind;
pub mod agent_status;
pub mod antigravity_history;
pub mod app_log;
pub mod appearance_settings;
pub mod attention;
pub mod auto_model_store;
pub mod automation;
pub mod automation_store;
pub mod automation_transcript;
pub mod block_on;
pub mod claude_paths;
pub mod codex_app_server;
pub mod control_api;
pub mod control_client;
pub mod conversation;
pub mod copilot_quota;
pub mod daemon_protocol;
pub mod daemon_state;
pub mod dsh_auto_models;
pub mod fd_limit;
pub mod font_config;
pub mod fs;
pub mod isolated_workspace;
pub mod login_env;
#[cfg(unix)]
pub mod managed_runtime;
pub mod new_session;
pub mod osc;
pub mod pi_auth;
pub mod pi_auto_models;
pub mod pi_model_discovery;
pub mod pi_model_settings;
pub mod pi_plugin_catalog;
pub mod pi_rpc;
pub mod plugin_enablement;
pub mod project_catalog;
pub mod provider_api;
pub mod provider_quota;
pub mod remote_config;
#[cfg(unix)]
pub mod runtime_generation;
pub mod session_control;
pub mod session_handoff;
pub mod session_history;
pub mod session_metadata;
pub mod session_title;
pub mod sqlite_state;
pub mod subprocess;
pub mod term_text;
pub mod terminal_theme;
pub mod tty_color;
pub mod updater;
pub mod workspace_menu;
pub mod workspace_override;
pub mod worktree_inherit;
