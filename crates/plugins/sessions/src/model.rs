//! 会话插件的状态和从工作区目录算东西的纯函数。
//!
//! 权威在哪：
//! - 有哪些会话、每段会话的终端和对话、项目、工作集：那台机器的工作区目录
//!   （`Host::workspace_directory`），运行时是唯一写入方。这里不另存一份，用的时候现取。
//! - 每一格在干什么、叫什么：守护推来的会话状态（`Host::session_states`），同样现取。
//! - 分屏布局、上次选中：本插件的库（[`crate::store`]）。
//! - 选中哪段会话或哪个项目：只在内存里（[`Sessions::selection`]），选中的会话另存一份进库，
//!   下次启动恢复。会话列表插件经 `contract::SELECTION`、`SELECTION_CHANGED` 跟着它。
//! - 建好的会话（视图实例、分屏）：归会话视图（`session_view::SessionView`），它在窗口里建、
//!   在窗口里扔，不放在这里。选中会话眼下那一格由它推来一份只读缓存
//!   （[`Sessions::active_pane`]）。
//!
//! 整个插件只有一份 [`Sessions`]（全局实体，[`Sessions::global`]），会话视图、命令都读它；
//! 变了 `cx.notify()`，看它的视图 `observe` 着重画。这里只放数据，不放绑着窗口的东西。

use rust_i18n::t;
use std::collections::{HashMap, HashSet};

use gpui::{App, AppContext as _, Entity, Global};
use smelt_plugin_api::RuntimeKey;
use smelt_plugin_api::model::daemon_state::DaemonSessionState;
use smelt_plugin_api::model::runtime_path::PathStyle;
use smelt_plugin_api::model::session_control::is_background_acp_session_id;
use smelt_plugin_api::model::workspace_directory::{
    WorkspaceConversation, WorkspaceDirectory, WorkspaceRevision, project_root_of,
};

use crate::contract::{Selected, SessionAt};
use crate::lifecycle;
use crate::peers::conversation::ConversationStart;
use crate::session_view::Pane;
use crate::store::SessionsStore;

/// 一段会话：哪台机器的工作区目录里的哪条对话。对话 id 是 uuid，跨机器实际不会撞，但
/// 机器是会话身份的一部分（库里的行、选中、跳转都带着），不靠「不会撞」省掉它。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ConversationKey {
    pub(crate) machine: RuntimeKey,
    /// 工作区目录里那条对话的 `id`（不是守护会话 id）。
    pub(crate) id: String,
}

/// 会话插件的全部内存状态。
pub(crate) struct Sessions {
    pub(crate) store: SessionsStore,
    /// 选中的会话或项目，见 [`Selection`]。只由 lifecycle 改（`select`、`select_project`，
    /// 没了、机器移除时清掉），改了发 `contract::SELECTION_CHANGED`。
    pub(crate) selection: Selection,
    /// 选中的会话眼下那一格：会话视图从分屏推来的只读缓存（权威在分屏），带着是哪段会话，
    /// 和 [`Self::selection`] 对不上就不算。活动上下文、`SEND_TEXT` 按它。只由会话视图改，
    /// 改了不通知（没有谁画它）。
    pub(crate) active_pane: Option<(ConversationKey, Pane)>,
    /// 新建、分叉刚写进目录、还没建过视图的对话：建对话视图时带上，让它当新对话拉起
    /// （见 `peers::conversation::ConversationArgs::start`）。建视图时取走。
    pub(crate) starts: HashMap<ConversationKey, ConversationStart>,
    /// 生命周期（S3）自己的状态：订阅、看过哪些机器的目录、对话翻没翻在最新处等。
    pub(crate) lifecycle: lifecycle::State,
}

struct SessionsGlobal(Entity<Sessions>);

impl Global for SessionsGlobal {}

impl Sessions {
    /// 打开库、读回侧栏状态和上次的选中，建好全局实体。`register` 一开头调一次。
    ///
    /// 上次的选中直接放回 [`Self::selection`]：那段会话可能在 app 关着时被关掉了，那台机器
    /// 的目录推到时里面没有它，lifecycle 照常清掉（库里那行一起）。会话视图等目录里有了它
    /// 再建。
    pub(crate) fn install(cx: &mut App) -> Entity<Self> {
        let store = SessionsStore::open();
        let selection = store
            .selection()
            .map_or(Selection::None, Selection::Session);
        let sessions = cx.new(|_| Self {
            store,
            selection,
            active_pane: None,
            starts: HashMap::new(),
            lifecycle: lifecycle::State::default(),
        });
        cx.set_global(SessionsGlobal(sessions.clone()));
        sessions
    }

    /// 插件的那份状态。`register` 之后才有；视图、命令都在那之后。
    pub(crate) fn global(cx: &App) -> Entity<Self> {
        cx.global::<SessionsGlobal>().0.clone()
    }

    /// 那台机器的目录推到了：清掉库里指着目录里已经没有的对话的行。不碰选中和建好的会话
    /// （那归 lifecycle）。
    pub(crate) fn prune(&self, machine: &RuntimeKey, directory: &WorkspaceDirectory) {
        let known: HashSet<String> = directory
            .conversations
            .iter()
            .map(|conversation| conversation.id.clone())
            .collect();
        self.store.prune(machine, &known);
    }

    /// 机器移除了：它的分屏、选中全删。只对真的从机器表里消失的调，不对离线的调。
    pub(crate) fn forget_machine(&self, machine: &RuntimeKey) {
        self.store.forget_machine(machine);
    }
}

/// 侧栏里有行的项目根：打开的项目，加上不在任何项目里的会话按工作目录自成的组。
pub(crate) fn listed_roots(style: PathStyle, directory: &WorkspaceDirectory) -> HashSet<String> {
    let mut roots: HashSet<String> = directory.projects.iter().cloned().collect();
    for conversation in &directory.conversations {
        if let Placement::Project { root, .. } = placement(style, directory, conversation) {
            roots.insert(root);
        }
    }
    roots
}

// ---- 选中、要露的会话在哪 ----

/// 选中的是什么。会话和项目只能选一个：选中会话时项目跟着它算（活动上下文），选项目是
/// 项目里还没有会话、或想先看项目时。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Selection {
    #[default]
    None,
    /// 一段会话，会话视图露的就是它。存库，下次启动恢复。
    Session(ConversationKey),
    /// 一个项目（按那台机器规整过的根目录）：点了项目行、刚打开的项目，文件、Git 这些按
    /// 它看。只在内存里，不存库。
    Project {
        machine: RuntimeKey,
        root: String,
        /// 刚打开它时那次调用回的目录版本：目录还没推到这一版时，目录里没有它不算项目
        /// 没了。点项目行选的（目录里本来就有）是 `None`。
        since: Option<WorkspaceRevision>,
    },
}

impl Selection {
    /// 选中的会话。
    pub(crate) fn session(&self) -> Option<&ConversationKey> {
        match self {
            Self::Session(key) => Some(key),
            Self::None | Self::Project { .. } => None,
        }
    }

    /// 线上的形状（`contract::SELECTION`）。
    pub(crate) fn to_selected(&self) -> Selected {
        match self {
            Self::None => Selected::None,
            Self::Session(key) => Selected::Conversation {
                machine: key.machine.clone(),
                conversation_id: key.id.clone(),
            },
            Self::Project { machine, root, .. } => Selected::Project {
                machine: machine.clone(),
                root: root.clone(),
            },
        }
    }

    /// 选中的东西在这台机器上。
    pub(crate) fn on(&self, machine: &RuntimeKey) -> bool {
        match self {
            Self::None => false,
            Self::Session(key) => key.machine == *machine,
            Self::Project { machine: on, .. } => on == machine,
        }
    }
}

/// [`locate`] 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Located {
    /// 在，露这段会话。
    Found(ConversationKey),
    /// 目录还没推到给的版本，接着等下一次推送。
    Wait,
    /// 不在（或不在侧栏：归档了、自动化运行），也不用再等。
    Missing,
}

/// 要露的那段会话在 `machine` 的目录里找不找得到。`after` 是写目录那次回的版本：目录比它
/// 旧时找不到（或还是归档的）就接着等；没给、或目录已经推到了，找不到就是没有。还没收到
/// 目录的，给了 `after` 等，没给算没有。
pub(crate) fn locate(
    machine: &RuntimeKey,
    at: &SessionAt,
    after: Option<&WorkspaceRevision>,
    style: PathStyle,
    directory: Option<&WorkspaceDirectory>,
) -> Located {
    let Some(directory) = directory else {
        return if after.is_some() {
            Located::Wait
        } else {
            Located::Missing
        };
    };
    let found = match at {
        SessionAt::Session { session_id } => directory.conversation_of_session(session_id),
        SessionAt::Conversation { conversation_id } => directory
            .conversations
            .iter()
            .find(|conversation| conversation.id == *conversation_id),
    };
    if let Some(conversation) =
        found.filter(|conversation| placement(style, directory, conversation) != Placement::Hidden)
    {
        return Located::Found(ConversationKey {
            machine: machine.clone(),
            id: conversation.id.clone(),
        });
    }
    if after.is_some_and(|after| directory.version().is_older_than(after)) {
        Located::Wait
    } else {
        Located::Missing
    }
}

/// 侧栏里看得见的那条（归档的、自动化运行的不能选、不能露）。
pub(crate) fn shown_conversation<'a>(
    style: PathStyle,
    directory: &'a WorkspaceDirectory,
    id: &str,
) -> Option<&'a WorkspaceConversation> {
    directory.conversations.iter().find(|conversation| {
        conversation.id == id && placement(style, directory, conversation) != Placement::Hidden
    })
}

// ---- 从目录和会话状态算的 ----

/// 给人看的名字：运行时算好的展示名（对话的、或第一格终端的），没有就用用户起的名字，
/// 再没有就是通用名。
pub(crate) fn title(conversation: &WorkspaceConversation, states: &[DaemonSessionState]) -> String {
    let lead = conversation
        .acp
        .as_ref()
        .map(|acp| acp.sid.as_str())
        .or_else(|| {
            conversation
                .terminals
                .first()
                .and_then(|terminal| terminal.id.as_deref())
        });
    let named = lead
        .and_then(|id| states.iter().find(|state| state.id == id))
        .and_then(|state| state.conversation_name.as_deref());
    let custom = conversation.custom_title.as_deref();
    match [named, custom]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|name| !name.is_empty())
    {
        Some(name) => name.to_string(),
        None if conversation.acp.is_some() => t!("kind.conversation").into_owned(),
        None => t!("kind.terminal").into_owned(),
    }
}

/// 这段会话的工作目录：对话的，或第一格终端起的时候的。是起的时候定的，不随终端里
/// `cd` 变（那是会话状态里的 `cwd`），所以分组稳定。
pub(crate) fn cwd_of(conversation: &WorkspaceConversation) -> Option<&str> {
    let cwd = match &conversation.acp {
        Some(acp) => acp.cwd.as_deref(),
        None => conversation
            .terminals
            .first()
            .and_then(|terminal| terminal.cwd.as_deref()),
    };
    cwd.filter(|cwd| !cwd.is_empty())
}

/// 会话摆在侧栏哪儿。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// 在一个项目组里。`root` 是打开的项目，或者不在任何项目里时它自己的工作目录（自成
    /// 一组，没有工作目录的是 `""`）。`workset` 是它绑的工作集。
    Project {
        root: String,
        workset: Option<String>,
    },
    /// 「对话」那一段：跟智能体的对话，工作目录不在任何打开的项目里。
    Conversations,
    /// 不在侧栏：已归档（去归档视图看），或自动化的运行（去自动化看）。
    Hidden,
}

pub(crate) fn placement(
    style: PathStyle,
    directory: &WorkspaceDirectory,
    conversation: &WorkspaceConversation,
) -> Placement {
    if conversation.archived_at.is_some() || is_automation_run(conversation) {
        return Placement::Hidden;
    }
    if let Some(workset) = conversation
        .workset_id
        .as_ref()
        .and_then(|id| directory.worksets.iter().find(|workset| &workset.id == id))
    {
        return Placement::Project {
            root: workset.project_root.clone(),
            workset: Some(workset.id.clone()),
        };
    }
    let cwd = cwd_of(conversation);
    if let Some(root) = cwd.and_then(|cwd| project_root_of(style, &directory.projects, cwd)) {
        return Placement::Project {
            root: root.to_string(),
            workset: None,
        };
    }
    if conversation.agent_definition().is_some() {
        return Placement::Conversations;
    }
    Placement::Project {
        root: cwd.unwrap_or_default().to_string(),
        workset: None,
    }
}

/// 自动化跑出来的会话：智能体对话绑着自动化，或者是后台起的 ACP 会话。
fn is_automation_run(conversation: &WorkspaceConversation) -> bool {
    conversation
        .acp
        .as_ref()
        .is_some_and(|acp| acp.automation_id.is_some() || is_background_acp_session_id(&acp.sid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_plugin_api::model::daemon_state::DaemonPhase;
    use smelt_plugin_api::model::workspace_directory::{
        WorkspaceAcp, WorkspaceTerminal, WorkspaceWorkset,
    };

    fn terminal_session(id: &str, terminal: &str, cwd: &str) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            terminals: vec![WorkspaceTerminal {
                id: Some(terminal.into()),
                cwd: Some(cwd.into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn acp_session(id: &str, sid: &str, cwd: Option<&str>) -> WorkspaceConversation {
        WorkspaceConversation {
            id: id.into(),
            acp: Some(WorkspaceAcp {
                sid: sid.into(),
                cwd: cwd.map(Into::into),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn state(id: &str, phase: DaemonPhase, since: u64) -> DaemonSessionState {
        DaemonSessionState {
            id: id.into(),
            phase,
            phase_since: since,
            turn_events: true,
            ..Default::default()
        }
    }

    fn key(machine: RuntimeKey, id: &str) -> ConversationKey {
        ConversationKey {
            machine,
            id: id.into(),
        }
    }

    #[test]
    fn title_prefers_runtime_name_then_custom_then_generic() {
        let mut conversation = acp_session("c", "a1", None);
        assert_eq!(title(&conversation, &[]), "Conversation");
        conversation.custom_title = Some("我起的".into());
        assert_eq!(title(&conversation, &[]), "我起的");
        let mut named = state("a1", DaemonPhase::Idle, 0);
        named.conversation_name = Some("运行时的".into());
        assert_eq!(title(&conversation, &[named]), "运行时的");
        assert_eq!(title(&terminal_session("t", "t1", "/"), &[]), "Terminal");
    }

    #[test]
    fn placement_groups_by_workset_project_agent_then_cwd() {
        let style = PathStyle::Posix;
        let mut in_workset = terminal_session("w", "t0", "/elsewhere");
        in_workset.workset_id = Some("ws".into());
        let in_project = terminal_session("p", "t1", "/repo/sub");
        let mut agent_in_project = acp_session("ap", "a0", Some("/repo"));
        agent_in_project.acp.as_mut().unwrap().agent_definition_id = Some("writer".into());
        let mut agent_space = acp_session("a", "a1", Some("/home/u/.smelt/agents/writer"));
        agent_space.acp.as_mut().unwrap().agent_definition_id = Some("writer".into());
        let loose = terminal_session("l", "t2", "/tmp/x");
        let mut no_cwd = terminal_session("n", "t3", "");
        no_cwd.terminals[0].cwd = None;
        let mut archived = terminal_session("old", "t4", "/repo");
        archived.archived_at = Some(1);
        let mut automation = acp_session("auto", "a2", Some("/repo"));
        automation.acp.as_mut().unwrap().automation_id = Some("nightly".into());
        let background = acp_session("bg", "acp-automation-1", Some("/repo"));
        let directory = WorkspaceDirectory {
            projects: vec!["/repo".into()],
            worksets: vec![WorkspaceWorkset {
                id: "ws".into(),
                project_root: "/repo".into(),
                ..Default::default()
            }],
            conversations: vec![
                in_workset,
                in_project,
                agent_in_project,
                agent_space,
                loose,
                no_cwd,
                archived,
                automation,
                background,
            ],
            ..Default::default()
        };
        let placed: Vec<Placement> = directory
            .conversations
            .iter()
            .map(|conversation| placement(style, &directory, conversation))
            .collect();
        let project = |root: &str, workset: Option<&str>| Placement::Project {
            root: root.into(),
            workset: workset.map(Into::into),
        };
        assert_eq!(
            placed,
            [
                project("/repo", Some("ws")),
                project("/repo", None),
                project("/repo", None),
                Placement::Conversations,
                project("/tmp/x", None),
                project("", None),
                Placement::Hidden,
                Placement::Hidden,
                Placement::Hidden,
            ]
        );

        let roots = listed_roots(style, &directory);
        assert!(roots.contains("/repo") && roots.contains("/tmp/x"));
    }

    #[test]
    fn locating_waits_only_until_the_written_revision() {
        let remote = RuntimeKey::Remote("box".into());
        let style = PathStyle::Posix;
        let revision = |revision| WorkspaceRevision {
            epoch: "e".into(),
            revision,
        };
        let mut directory = WorkspaceDirectory {
            epoch: "e".into(),
            revision: 3,
            conversations: vec![terminal_session("c", "t1", "/repo")],
            ..Default::default()
        };
        let by_session = SessionAt::Session {
            session_id: "t1".into(),
        };
        let by_id = |id: &str| SessionAt::Conversation {
            conversation_id: id.into(),
        };
        let found = Located::Found(key(remote.clone(), "c"));

        assert_eq!(
            locate(&remote, &by_session, None, style, Some(&directory)),
            found
        );
        assert_eq!(
            locate(
                &remote,
                &by_id("c"),
                Some(&revision(3)),
                style,
                Some(&directory)
            ),
            found
        );
        // 刚写进去的：目录推到那一版之前接着等，推到了还没有就是没有。
        assert_eq!(
            locate(
                &remote,
                &by_id("new"),
                Some(&revision(4)),
                style,
                Some(&directory)
            ),
            Located::Wait
        );
        assert_eq!(
            locate(
                &remote,
                &by_id("new"),
                Some(&revision(3)),
                style,
                Some(&directory)
            ),
            Located::Missing
        );
        assert_eq!(
            locate(&remote, &by_id("new"), None, style, Some(&directory)),
            Located::Missing
        );
        // 运行时的库换过（epoch 不同）谈不上新旧，不等。
        let other = WorkspaceRevision {
            epoch: "f".into(),
            revision: 9,
        };
        assert_eq!(
            locate(
                &remote,
                &by_id("new"),
                Some(&other),
                style,
                Some(&directory)
            ),
            Located::Missing
        );
        // 还没收到目录。
        assert_eq!(
            locate(&remote, &by_id("c"), Some(&revision(1)), style, None),
            Located::Wait
        );
        assert_eq!(
            locate(&remote, &by_id("c"), None, style, None),
            Located::Missing
        );

        // 归档的不在侧栏：刚恢复、目录还没推到时等，推到了还是归档的就是没有。
        directory.conversations[0].archived_at = Some(1);
        assert_eq!(
            locate(
                &remote,
                &by_session,
                Some(&revision(4)),
                style,
                Some(&directory)
            ),
            Located::Wait
        );
        assert_eq!(
            locate(&remote, &by_session, None, style, Some(&directory)),
            Located::Missing
        );
        assert_eq!(shown_conversation(style, &directory, "c"), None);
    }

    #[test]
    fn selection_is_a_session_or_a_project() {
        let remote = RuntimeKey::Remote("box".into());
        let session = Selection::Session(key(remote.clone(), "c"));
        assert_eq!(session.session(), Some(&key(remote.clone(), "c")));
        assert_eq!(
            session.to_selected(),
            Selected::Conversation {
                machine: remote.clone(),
                conversation_id: "c".into(),
            }
        );
        assert!(session.on(&remote) && !session.on(&RuntimeKey::Local));

        let project = Selection::Project {
            machine: RuntimeKey::Local,
            root: "/repo".into(),
            since: None,
        };
        assert_eq!(project.session(), None);
        assert_eq!(
            project.to_selected(),
            Selected::Project {
                machine: RuntimeKey::Local,
                root: "/repo".into(),
            }
        );
        assert!(project.on(&RuntimeKey::Local));
        assert!(!Selection::None.on(&RuntimeKey::Local));
    }
}
