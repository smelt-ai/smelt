//! 会话（来源 `smelt.sessions`）的约定：本插件用到的 id 和字段，照抄自权威定义
//! `crates/plugins/sessions/src/contract.rs`，形状必须和那边一致。选中、会话视图、写新会话
//! 都归那边，列表只画、只发请求。

use serde::{Deserialize, Serialize};
use smelt_plugin_api::model::workspace_directory::{WorkspaceConversation, WorkspaceRevision};
use smelt_plugin_api::{Command, Event, RuntimeKey, machine_json};

// ---- 视图 ----

// 约定：smelt.sessions 的 SESSION。
/// 会话（用户看到的名字「会话」）：选中的那段会话——对话加上它的终端分屏，切会话整套换。
/// 单例，自己画顶栏；是首页出口的默认内容（布局登记时的 `index`）。`OPEN_VIEW` 带
/// [`SessionArgs`] 是选中那段会话、焦点给它；不带参数只是露出来。
pub(crate) const SESSION: &str = "smelt.sessions/session";

/// 打开 [`SESSION`] 的参数：露哪段会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionArgs {
    /// 会话在哪台机器上。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 按什么找。JSON 里平铺：`{session_id}` 或 `{conversation_id}`。
    #[serde(flatten)]
    pub at: SessionAt,
    /// 刚写进目录时那次调用回的目录版本：目录推到这一版之前找不到不算没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<WorkspaceRevision>,
}

/// [`SessionArgs`] 按什么找那段会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum SessionAt {
    /// 守护会话 id，同 `smelt_plugin_api::Host::session_states` 里的：对话是它的 ACP 会话，
    /// 终端是其中一格，焦点给那一格。
    Session { session_id: String },
    /// 工作区目录里的对话 id，焦点给它眼下那一格。
    Conversation { conversation_id: String },
}

// ---- 给会话列表的：选中、新建 ----
//
// 选中归这边（会话视图露的、活动上下文按的都是它），列表只画、只发请求。

// 约定：smelt.sessions 的 SELECTION。
/// 眼下选中的是什么。列表起来时问一次，之后跟着 [`SELECTION_CHANGED`]。
pub(crate) const SELECTION: Command<(), Selected> = Command::new("smelt.sessions/selection");
// 约定：smelt.sessions 的 SELECTION_CHANGED。
/// 选中变了。又选了一次同一段会话（从通知、命令跳回来）也发：它可能折在列表里，列表据此
/// 把它露出来。
pub(crate) const SELECTION_CHANGED: Event<Selected> =
    Event::new("smelt.sessions/selection-changed");
// 约定：smelt.sessions 的 CONVERSATION_CREATED。
/// 一段会话刚由这边写进目录（新建、分叉、续接、交接），还没推回来。列表等它出现时播一次
/// 落进来的动画。写失败了不另发。
pub(crate) const CONVERSATION_CREATED: Event<ConversationRef> =
    Event::new("smelt.sessions/conversation-created");
// 约定：smelt.sessions 的 CREATE_CONVERSATION。
/// 把一段新会话写进那台机器的目录，再打开过去。有对话的当新对话拉起，不当成要续的旧对话。
/// 写不进去、打不开都是失败。
pub(crate) const CREATE_CONVERSATION: Command<CreateConversation, ()> =
    Command::new("smelt.sessions/create-conversation");
// 约定：smelt.sessions 的 SELECT_PROJECT。
/// 选中一个项目（不打开它，目录里没有就是失败）：项目里还没有会话时，文件、Git 这些也按它看。
/// 刚打开的项目带上打开那次回的目录版本（`after`），目录推到之前不算没有。
pub(crate) const SELECT_PROJECT: Command<SelectProject, ()> =
    Command::new("smelt.sessions/select-project");

/// 选中的是什么，见 [`SELECTION`]。线上是 `{"kind": "none"}`、
/// `{"kind": "conversation", "machine", "conversation_id"}`、`{"kind": "project", "machine", "root"}`。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Selected {
    #[default]
    None,
    /// 一段会话，会话视图露的就是它。
    Conversation {
        #[serde(with = "machine_json")]
        machine: RuntimeKey,
        /// 工作区目录里那条对话的 id。
        conversation_id: String,
    },
    /// 一个项目，根目录按那台机器规整过。
    Project {
        #[serde(with = "machine_json")]
        machine: RuntimeKey,
        root: String,
    },
}

/// 哪台机器的工作区目录里的哪条对话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ConversationRef {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    pub conversation_id: String,
}

/// [`CREATE_CONVERSATION`] 的参数：目录里那一条整条由调用方拼好，id 也由调用方先分好。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CreateConversation {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    pub conversation: WorkspaceConversation,
}

/// [`SELECT_PROJECT`] 的参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SelectProject {
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<WorkspaceRevision>,
}
