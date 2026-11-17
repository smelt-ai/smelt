//! 会话插件的对话框（S4）：新建会话（旧宿主 `session_list/new_session.rs`）和项目的仓库
//! （`project_repositories.rs`）。本目录下的文件都归 S4。
//!
//! 设置页「Agent」里的「新建菜单」不在这里：按 11-16 的决定归智能体插件。

mod agents;
mod new_session;
mod repositories;

use gpui::{App, Window};
use smelt_plugin_api::RuntimeKey;

/// 打开新建会话的对话框。选好了调 `lifecycle::create_conversation`。
///
/// - `machine`：预选的机器；`None` 按眼下活动项目所在的机器，没有就是本机。
/// - `project`：预选的位置路径（那台机器上的路径）。是项目根就预选那个项目；落在某个工作树
///   （工作集，`<项目根>/worktrees/<任务名>`）里就预选那个工作树，S1 的工作树行传工作树
///   路径。目录里还对不上的按项目根用。`None` 按活动项目，再不行取那台机器上第一个项目。
pub(crate) fn open_new_session(
    machine: Option<RuntimeKey>,
    project: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    new_session::open(machine, project, window, cx);
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
