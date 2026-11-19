//! 会话（来源 `smelt.sessions`）对外的约定：会话视图、开会话和跳到会话的命令、往眼下在看的
//! 会话里递话的命令、分屏的键盘命令，以及给会话列表用的选中、新建。JS 插件按同样的字符串和
//! JSON 调（`run`、事件）。
//!
//! 这里是权威定义。插件之间不互相依赖，插件接口也不认识具体功能：调用方各自照抄用到的
//! id 和字段，注明出处。改这里就是改线上形状，同一次改动里把照抄的各处一起改掉
//! （`grep -rn 'smelt.sessions/' crates plugins`）。
//!
//! 启动命令、环境变量、初始配置都由这边按那台机器的智能体配置现算，参数里只说开什么。
//!
//! 「跳到某段会话」都是打开会话视图（[`SESSION`]，参数 [`SessionArgs`]）。刚写进目录的会话
//! （新建、分叉、刚恢复的归档）命令来时目录可能还没推到：写目录的调用会回一个目录版本，
//! 带上它（`after`），视图就等那台机器的目录推到这一版再找；推到了还没有才算找不到。
//! 不带 `after` 的按眼下的目录找。等是随目录推送判断的，不另起定时器；后来的打开盖掉
//! 还在等的。

use serde::{Deserialize, Serialize};
use smelt_plugin_api::model::acp_conn::AcpForkCut;
use smelt_plugin_api::model::workspace_directory::{WorkspaceConversation, WorkspaceRevision};
use smelt_plugin_api::{Command, Event, RuntimeKey, machine_json};

// ---- 视图 ----

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
    /// 刚写进目录时那次调用回的目录版本：目录推到这一版之前找不到不算没有，见模块文档。
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

/// 眼下选中的是什么。列表起来时问一次，之后跟着 [`SELECTION_CHANGED`]。
pub(crate) const SELECTION: Command<(), Selected> = Command::new("smelt.sessions/selection");
/// 选中变了。又选了一次同一段会话（从通知、命令跳回来）也发：它可能折在列表里，列表据此
/// 把它露出来。
pub(crate) const SELECTION_CHANGED: Event<Selected> =
    Event::new("smelt.sessions/selection-changed");
/// 一段会话刚由这边写进目录（新建、分叉、续接、交接），还没推回来。列表等它出现时播一次
/// 落进来的动画。写失败了不另发。
pub(crate) const CONVERSATION_CREATED: Event<ConversationRef> =
    Event::new("smelt.sessions/conversation-created");
/// 把一段新会话写进那台机器的目录，再打开过去。有对话的当新对话拉起，不当成要续的旧对话。
/// 写不进去、打不开都是失败。
pub(crate) const CREATE_CONVERSATION: Command<CreateConversation, ()> =
    Command::new("smelt.sessions/create-conversation");
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

// ---- 带参数的命令（给别的插件调） ----

/// 在主窗口里露出这个会话（终端是那一格），把主窗口拿到前面来；主窗口关着就先开。等于
/// 打开 [`SESSION`]，`after` 的意思见模块文档。
pub(crate) const ACTIVATE: Command<Activate, ()> = Command::new("smelt.sessions/activate");

/// 开一段新对话：开在那台机器上、`cwd` 里，露在主窗口里。跟智能体开的按它定义的引擎、
/// 模型和启动命令起，智能体不在了、还不能对话、引擎不开放都算失败；裸引擎的按那台机器
/// 的智能体配置起（输入框 `/clear`、新建会话框都走这条）。
pub(crate) const START_CONVERSATION: Command<StartConversation, ()> =
    Command::new("smelt.sessions/start-conversation");

/// 从一段开着的对话分叉出一段新对话：同一台机器、同一个 agent 和工作目录，启动命令、配置、
/// 绑定照源对话（工作区目录里的那条），历史复制自 `provider_session_id`，按 `cut` 切。
/// 新对话露在主窗口里。agent 不支持分叉、源对话不在了都算失败。
pub(crate) const FORK: Command<Fork, ()> = Command::new("smelt.sessions/fork");

/// 用那家 agent 自己的续接（ACP `session/load`）接着一段历史会话；同一段已经开着就
/// 切过去。
pub(crate) const RESUME: Command<Resume, ()> = Command::new("smelt.sessions/resume");
/// 在终端里起那家 agent 的 CLI 恢复同一段历史会话（`--resume` 之类）。不看
/// `agent_definition_id`。
pub(crate) const RESUME_IN_TERMINAL: Command<Resume, ()> =
    Command::new("smelt.sessions/resume-in-terminal");
/// 起一段新会话，第一句话是交接说明（把别家的对话重述给它）。
pub(crate) const HANDOFF: Command<Handoff, ()> = Command::new("smelt.sessions/handoff");

/// 把文字交给眼下在看的那一格：终端打进输入（`smelt.terminal/send-text`），对话填进输入框
/// （`smelt.conversation/insert-text`），都不替用户发出。没有能接的那一格时失败。
pub(crate) const SEND_TEXT: Command<SendText, ()> = Command::new("smelt.sessions/send-text");

/// [`ACTIVATE`] 的参数：哪台机器上的哪段守护会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Activate {
    /// 会话在哪台机器上。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 守护会话 id，同 `smelt_plugin_api::Host::session_states` 里的：对话是它的 ACP 会话，
    /// 终端是其中一格。
    pub session_id: String,
    /// 刚恢复、刚写进目录时那次调用回的目录版本，见模块文档。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<WorkspaceRevision>,
}

/// [`START_CONVERSATION`] 的参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StartConversation {
    /// 智能体定义所在的机器，对话也开在那台机器上。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 那台机器上的工作目录。智能体自己的工作区用 `agent.space.ensure` 取。
    pub cwd: String,
    /// 跟谁开：JSON 里平铺，`{agent_definition_id}` 或 `{agent, profile_id}`。
    #[serde(flatten)]
    pub with: StartWith,
}

/// [`StartConversation`] 跟谁开。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum StartWith {
    /// 跟一个智能体（id 见智能体插件 `crates/plugins/agents/src/contract.rs` 的
    /// `AGENT_SETTINGS` 里的 `agents`）：启动命令、模型按它现算。
    Agent { agent_definition_id: String },
    /// 裸引擎：基础 agent（`ConversationAgentKind::id`），或它的一个自定义 Agent。
    Engine {
        agent: String,
        #[serde(default)]
        profile_id: Option<String>,
    },
}

/// [`FORK`] 的参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Fork {
    /// 源对话在哪台机器上，新对话也开在那里（agent 的会话文件在那台机器上）。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 源对话的守护会话 id，新对话记它作来历。
    pub session_id: String,
    /// 源对话在 agent 那边的会话 id：新对话复制它的历史（Pi 的 `--fork`）。
    pub provider_session_id: String,
    /// 副本切到哪条用户消息之前（不含它）；`None` 是整份。切点是第 `occurrence` 条（从 0
    /// 数）原文是 `text` 的用户消息。
    #[serde(default)]
    pub cut: Option<AcpForkCut>,
    /// 预填进新对话输入框的话，不替用户发出；空的是不预填。
    #[serde(default)]
    pub prompt: String,
}

/// [`RESUME`]、[`RESUME_IN_TERMINAL`] 的参数：续接一段历史会话。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Resume {
    /// 存档在哪台机器上，会话也开在那台机器上。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    /// 历史来源的 id（`HistorySourceKind::id`）。
    pub agent: String,
    /// 自定义 Agent 的 id；基础 agent 是 `None`。启动命令和环境变量按它取。
    pub profile_id: Option<String>,
    /// 那台机器上的工作目录。
    pub cwd: String,
    /// agent 自己认的会话 id。
    pub resume_id: String,
    /// 这段会话属于哪个智能体定义（运行时随历史列表给的）。有的话按那个智能体续接。
    #[serde(default)]
    pub agent_definition_id: Option<String>,
}

/// [`HANDOFF`] 的参数：把一段对话交给另一家 agent。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Handoff {
    /// 新会话开在哪台机器上。
    #[serde(with = "machine_json")]
    pub machine: RuntimeKey,
    pub cwd: String,
    /// 目标 agent（`ConversationAgentKind::id`）。
    pub agent: String,
    /// 目标是自定义 Agent 时它的 id。
    pub profile_id: Option<String>,
    /// 新会话的第一句话。
    pub prompt: String,
}

/// [`SEND_TEXT`] 的参数。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SendText {
    pub text: String,
}

// ---- 不带参数的命令（快捷键、命令面板） ----
//
// 用户改过的键按这些 id 存在宿主的快捷键表里，改名就是丢掉用户的设置。

/// 分屏里焦点移到上一格、下一格。
pub(crate) const PREV_PANE: Command<(), ()> = Command::new("smelt.sessions/prev-pane");
pub(crate) const NEXT_PANE: Command<(), ()> = Command::new("smelt.sessions/next-pane");
/// 在焦点那一格右边、下边分出一格终端。
pub(crate) const SPLIT_RIGHT: Command<(), ()> = Command::new("smelt.sessions/split-right");
pub(crate) const SPLIT_DOWN: Command<(), ()> = Command::new("smelt.sessions/split-down");
/// 关掉焦点那一格；是会话的最后一格就是关会话（要确认的先确认）。
pub(crate) const CLOSE_PANE: Command<(), ()> = Command::new("smelt.sessions/close-pane");
/// 复制眼下那一格的会话 id：agent 自己的对话 id（拿去 `--resume`、找存档用），agent 还没
/// 报上来时是守护会话 id。
pub(crate) const COPY_SESSION_ID: Command<(), ()> = Command::new("smelt.sessions/copy-session-id");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activate_and_fork_carry_the_machine() {
        let activate = Activate {
            machine: RuntimeKey::Remote("box".into()),
            session_id: "s".into(),
            after: None,
        };
        let value = serde_json::to_value(&activate).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "machine": "box", "session_id": "s" })
        );
        assert_eq!(serde_json::from_value::<Activate>(value).unwrap(), activate);

        let fork: Fork = serde_json::from_value(serde_json::json!({
            "machine": null, "session_id": "s", "provider_session_id": "p",
        }))
        .unwrap();
        assert_eq!(fork.machine, RuntimeKey::Local);
        assert_eq!((fork.cut, fork.prompt.as_str()), (None, ""));
    }

    #[test]
    fn selection_is_tagged_by_kind() {
        let selected = Selected::Conversation {
            machine: RuntimeKey::Local,
            conversation_id: "c".into(),
        };
        let value = serde_json::to_value(&selected).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "kind": "conversation", "machine": null, "conversation_id": "c" })
        );
        assert_eq!(serde_json::from_value::<Selected>(value).unwrap(), selected);
        let project = Selected::Project {
            machine: RuntimeKey::Remote("box".into()),
            root: "/repo".into(),
        };
        let value = serde_json::to_value(&project).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "kind": "project", "machine": "box", "root": "/repo" })
        );
        assert_eq!(serde_json::from_value::<Selected>(value).unwrap(), project);
        assert_eq!(
            serde_json::to_value(Selected::None).unwrap(),
            serde_json::json!({ "kind": "none" })
        );
    }

    #[test]
    fn session_args_flatten_what_to_find() {
        let by_session: SessionArgs = serde_json::from_value(serde_json::json!({
            "machine": null, "session_id": "t1",
        }))
        .unwrap();
        assert_eq!(
            by_session.at,
            SessionAt::Session {
                session_id: "t1".into()
            }
        );
        assert_eq!(by_session.after, None);

        let created = SessionArgs {
            machine: RuntimeKey::Remote("box".into()),
            at: SessionAt::Conversation {
                conversation_id: "c".into(),
            },
            after: Some(WorkspaceRevision {
                epoch: "e".into(),
                revision: 7,
            }),
        };
        let value = serde_json::to_value(&created).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "machine": "box",
                "conversation_id": "c",
                "after": { "epoch": "e", "revision": 7 },
            })
        );
        assert_eq!(
            serde_json::from_value::<SessionArgs>(value).unwrap(),
            created
        );
        // 不带参数打开（宿主建好视图时）不是要露哪段会话。
        assert!(serde_json::from_value::<SessionArgs>(serde_json::Value::Null).is_err());
    }

    #[test]
    fn resume_args_round_trip_and_tolerate_missing_definition() {
        let args = Resume {
            machine: RuntimeKey::Remote("box".into()),
            agent: "claude".into(),
            profile_id: None,
            cwd: "/repo".into(),
            resume_id: "abc".into(),
            agent_definition_id: Some("writer".into()),
        };
        let value = serde_json::to_value(&args).unwrap();
        assert_eq!(serde_json::from_value::<Resume>(value).unwrap(), args);

        let bare = serde_json::json!({
            "machine": null, "agent": "codex", "profile_id": null,
            "cwd": "/repo", "resume_id": "x",
        });
        let parsed: Resume = serde_json::from_value(bare).unwrap();
        assert_eq!(parsed.agent_definition_id, None);
        assert_eq!(parsed.machine, RuntimeKey::Local);
    }

    #[test]
    fn every_id_is_under_this_source() {
        let commands = [
            ACTIVATE.id(),
            START_CONVERSATION.id(),
            FORK.id(),
            RESUME.id(),
            RESUME_IN_TERMINAL.id(),
            HANDOFF.id(),
            SEND_TEXT.id(),
            SELECTION.id(),
            CREATE_CONVERSATION.id(),
            SELECT_PROJECT.id(),
            PREV_PANE.id(),
            NEXT_PANE.id(),
            SPLIT_RIGHT.id(),
            SPLIT_DOWN.id(),
            CLOSE_PANE.id(),
            COPY_SESSION_ID.id(),
        ];
        let others = [SESSION, SELECTION_CHANGED.id(), CONVERSATION_CREATED.id()];
        let mut seen = std::collections::HashSet::new();
        for id in commands.into_iter().chain(others) {
            assert!(id.starts_with("smelt.sessions/"), "{id}");
            assert!(seen.insert(id), "{id} 重复");
        }
    }
}
