//! 会话插件的对话框（S4）：新建会话的菜单和项目的仓库（`project_repositories.rs`）。本目录下
//! 的文件都归 S4。
//!
//! 设置页「Agent」里的「新建菜单」不在这里：按 11-16 的决定归智能体插件。

mod agents;
mod new_session;
mod repositories;

use gpui::{App, Window};
use smelt_plugin_api::RuntimeKey;

pub(crate) use new_session::NewSessionButton;

/// 从命令打开新建会话的菜单（钉在窗口上方，带位置行）：预选眼下活动项目，没有就取本机上
/// 第一个项目。项目行、工作树行的「+」贴着按钮弹，用 [`NewSessionButton`]；行的右键菜单不再
/// 重复这一项。选好了调 `lifecycle::create_conversation`。
pub(crate) fn open_new_session(window: &mut Window, cx: &mut App) {
    new_session::open(window, cx);
}

/// 打开项目的仓库对话框（纳入、克隆、移除仓库，建工作集）。
pub(crate) fn open_project_repositories(
    machine: RuntimeKey,
    root: String,
    window: &mut Window,
    cx: &mut App,
) {
    repositories::open(machine, root, repositories::Mode::Manage, None, window, cx);
}

/// 直接打开建工作树那一步（工作集：挑几个仓库、起任务名）。建好后侧栏随目录推送出现那一行。
pub(crate) fn open_new_worktree(
    machine: RuntimeKey,
    root: String,
    window: &mut Window,
    cx: &mut App,
) {
    repositories::open(machine, root, repositories::Mode::Create, None, window, cx);
}
