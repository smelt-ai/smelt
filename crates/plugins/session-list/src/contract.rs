//! 会话列表（来源 `smelt.session-list`）对外的约定：列表视图、每行行首的插槽、列表里右键
//! 菜单的 id 和上下文，以及列表的命令（找出某段会话、新建会话、打开项目、上一段 / 下一段）。
//! JS 插件按同样的字符串和 JSON 调（`run`、`register_context_menu_items`）。
//!
//! 这里是权威定义。插件之间不互相依赖，插件接口也不认识具体功能：调用方各自照抄用到的
//! id 和字段，注明出处。改这里就是改线上形状，同一次改动里把照抄的各处一起改掉
//! （`grep -rn 'smelt.session-list/' crates plugins`）。
//!
//! 会话本身（会话视图、选中、开会话的命令）归会话插件 `smelt.sessions`，照抄在 [`crate::core`]。

use serde::{Deserialize, Serialize};
use smelt_plugin_api::{Command, RuntimeKey, machine_json};

// ---- 视图 ----

/// 会话列表（用户看到的名字「会话列表」）：按机器、项目分组列出会话。单例，摆在布局里。
pub(crate) const LIST: &str = "smelt.session-list/main";

/// 打开 [`LIST`] 的参数。不带参数只是露出来。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ListArgs {
    /// 键盘焦点交给列表。
    #[serde(default)]
    pub focus: bool,
}

// ---- 插槽 ----

/// 会话列表每行行首的头像（插槽，见 `smelt_plugin_api` 视图注册表的「插槽」）。填它的是按实例
/// 建的视图，每行一份，参数是 [`SessionAvatarArgs`]，第一次 `open` 给，之后不换会话。只用
/// 一个：好几个填进来时由用户在设置「会话列表 → 行首头像」里选，没选过用第一个（按登记
/// 顺序）；没人填、选了不要时行首是图标、行照常高。
///
/// 画在 36pt 见方的格子里，零件可以伸出去一点（举起来的道具、头顶的特效），这边不裁。右下角
/// 盖着标明哪家 CLI、按状态上色的小圆标，归这边画。会话在干什么、露没露着、跟哪个智能体，
/// 填的一方自己看宿主的目录、会话状态。
pub(crate) const SESSION_AVATAR: &str = "smelt.session-list/session-avatar";

/// [`SESSION_AVATAR`] 的参数。线上是 `{"machine": null | "<id>", "conversation": "<id>"}`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionAvatarArgs {
    /// 本机是 `null`。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 工作区目录里那段会话的 id。
    pub conversation: String,
}

// ---- 右键菜单 ----

/// 会话列表里项目行的右键菜单（见 `smelt_plugin_api::register_context_menu_items`）。点了
/// 交给命令的上下文是 [`ProjectMenuContext`]。
pub(crate) const PROJECT_MENU: &str = "smelt.session-list/project";
/// 会话列表里机器行的右键菜单。点了交给命令的上下文是 [`MachineMenuContext`]。
pub(crate) const MACHINE_MENU: &str = "smelt.session-list/machine";
/// 会话列表里对话行的右键菜单，别的插件登记的项排在自带的「置顶」「改名」后面。点了交给命令的
/// 上下文是 [`SessionMenuContext`]。
pub(crate) const SESSION_MENU: &str = "smelt.session-list/conversation";

/// [`PROJECT_MENU`] 的上下文：右键的是哪个项目。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectMenuContext {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 项目根目录，按那台机器的路径写。
    pub path: String,
}

/// [`MACHINE_MENU`] 的上下文：右键的是哪台机器。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MachineMenuContext {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
}

/// [`SESSION_MENU`] 的上下文：右键的是哪台机器上的哪条对话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionMenuContext {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 工作区目录里那条对话的 id（`workspace_directory` 里的那个，不是守护会话 id）。
    pub conversation: String,
    /// 领头那一格的守护会话 id（对话的，或第一格终端的）；还没起过是 `None`。
    pub session_id: Option<String>,
}

// ---- 带参数的命令（给别的插件调） ----

/// 在会话列表里找到这段会话：侧栏收着就打开，展开它所在的组，滚过去并闪一下。不换选中、
/// 不动焦点。会话不在列表里（归档了、自动化运行、目录里没有）是失败。
pub(crate) const REVEAL_IN_LIST: Command<RevealInList, ()> =
    Command::new("smelt.session-list/reveal-in-list");

/// [`REVEAL_IN_LIST`] 的参数：哪台机器上的哪段守护会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RevealInList {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 守护会话 id，同 `smelt_plugin_api::Host::session_states` 里的：对话是它的 ACP 会话，
    /// 终端是其中一格。
    pub session_id: String,
}

// ---- 不带参数的命令（快捷键、命令面板） ----
//
// 用户改过的键按这些 id 存在宿主的快捷键表里，改名就是丢掉用户的设置。

/// 新建会话（打开新建会话的菜单）。
pub(crate) const NEW_SESSION: Command<(), ()> = Command::new("smelt.session-list/new-session");
/// 打开项目（选一个目录）。
pub(crate) const OPEN_PROJECT: Command<(), ()> = Command::new("smelt.session-list/open-project");
/// 焦点移到会话列表。
pub(crate) const FOCUS_LIST: Command<(), ()> = Command::new("smelt.session-list/focus-list");
/// 选中列表里的上一段、下一段会话。
pub(crate) const PREV_SESSION: Command<(), ()> = Command::new("smelt.session-list/prev");
pub(crate) const NEXT_SESSION: Command<(), ()> = Command::new("smelt.session-list/next");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_avatar_args_shape() {
        let args = SessionAvatarArgs {
            machine: RuntimeKey::Local,
            conversation: "c".into(),
        };
        let value = serde_json::to_value(&args).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "machine": null, "conversation": "c" })
        );
        assert_eq!(
            serde_json::from_value::<SessionAvatarArgs>(value).unwrap(),
            args
        );
    }

    #[test]
    fn every_id_is_under_this_source() {
        let commands = [
            REVEAL_IN_LIST.id(),
            NEW_SESSION.id(),
            OPEN_PROJECT.id(),
            FOCUS_LIST.id(),
            PREV_SESSION.id(),
            NEXT_SESSION.id(),
        ];
        let others = [
            LIST,
            SESSION_AVATAR,
            PROJECT_MENU,
            MACHINE_MENU,
            SESSION_MENU,
        ];
        let mut seen = std::collections::HashSet::new();
        for id in commands.into_iter().chain(others) {
            assert!(id.starts_with("smelt.session-list/"), "{id}");
            assert!(seen.insert(id), "{id} 重复");
        }
    }
}
