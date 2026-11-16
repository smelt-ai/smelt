//! 终端：内嵌终端的状态机（[`terminal`]）和视图（[`terminal_view`]）。PTY 和 shell 在
//! smeltd 里，这里连守护拿字节流，alacritty_terminal 解析成网格，GPUI 画出来。
//!
//! 内置插件（来源 id [`OWNER`]），只依赖 [`smelt_plugin_api`] 和第三方库。
//!
//! 开连接、运行时在线、等上线、会话还在不在都用插件接口的宿主服务（[`smelt_plugin_api::Host`]
//! 的 `connector`、`machine_online`、`session_presence` 和 [`smelt_plugin_api::wait_online`]）。
//!
//! 过渡：眼下宿主的工作台、侧栏、终端停靠区还直接用这里的类型；会话插件搬走后改为经宿主
//! 的视图实例机制和 `smelt.terminal` 命令使用。

pub mod terminal;
mod terminal_images;
pub mod terminal_view;

/// 来源 id。
pub const OWNER: &str = "smelt.terminal";
