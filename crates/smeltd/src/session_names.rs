//! 会话的展示名：会话状态里的 `conversation_name`、`name` 由这里维护。
//!
//! 名字由三样拼出来：工作区目录（这段对话的改名、每一格的改名和启动项）、会话自己的状态
//! （会话名、那段 provider 对话的改名、工作目录）、智能体配置（定义和自定义 Agent 的名字）。
//! 三样都在运行时，所以
//! 在这里算一次推给各端；桌面侧栏、菜单栏、资源页、手机直接显示，不各自再拼一份。
//!
//! 记账都在一个线程里，跟 [`crate::session_branch`] 一个做法：广播状态、删会话、推目录、
//! 改智能体配置只往通道里发消息。名字变了就改会话状态再广播；广播是在提交门里调的，这里
//! 不能在门里同步改。没有运行时的留下的会话改总线上发布着的那份（[`crate::republish_dormant`]）。
//!
//! 只认工作区目录里的会话：自动化跑的、命令行直接起的会话不在目录里，没有名字，各端显示
//! 通用名。

mod rules;

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender};

use smelt_core::agent_kind::ConversationAgentKind;
use smelt_core::workspace_directory::{
    WorkspaceConversation, WorkspaceDirectory, WorkspaceTerminal,
};

use crate::Sessions;
use crate::acp_host::AcpSessions;
use crate::event_hub::EventHubHandle;
use crate::session_state::{SessionState, bump_state_revision};
use rules::{TerminalTitle, acp_conversation_name, pane_name};

enum Message {
    /// 一份会话状态刚发布出去。
    Observe { id: String, observed: Observed },
    /// 会话从发布的状态里删掉了。
    Forget { id: String },
    /// 工作区目录刚推出去。
    Directory(WorkspaceDirectory),
    /// 智能体配置（定义、自定义 Agent）改过。
    AgentsChanged,
}

static CHANNEL: OnceLock<Sender<Message>> = OnceLock::new();

/// 起记账线程。守护启动、留下的会话都发布进总线之后调一次；测试不调，下面几个入口就什么
/// 都不做。
pub(crate) fn spawn(sessions: Sessions, acp_sessions: AcpSessions, event_hub: EventHubHandle) {
    let (tx, rx) = std::sync::mpsc::channel();
    if CHANNEL.set(tx).is_err() {
        return;
    }
    let worker = Worker {
        sessions,
        acp_sessions,
        event_hub,
        book: Book::default(),
    };
    if let Err(error) = std::thread::Builder::new()
        .name("smeltd-session-names".into())
        .spawn(move || worker.run(rx))
    {
        eprintln!("[session-names] 起线程失败，会话不带展示名：{error}");
    }
}

/// 会话状态发布出去了（[`crate::broadcast_state`] 调）。
pub(crate) fn observe(state: &SessionState) {
    send(Message::Observe {
        id: state.id.clone(),
        observed: Observed::of(state),
    });
}

/// 会话从发布的状态里删掉了。
pub(crate) fn forget(id: &str) {
    send(Message::Forget { id: id.to_string() });
}

/// 工作区目录推出去了（`DaemonEventHub::publish_workspace` 调）。
pub(crate) fn directory(directory: &WorkspaceDirectory) {
    send(Message::Directory(directory.clone()));
}

/// 智能体配置改过了：定义、自定义 Agent 的名字可能变了，也可能删了。
pub(crate) fn agents_changed() {
    send(Message::AgentsChanged);
}

fn send(message: Message) {
    if let Some(tx) = CHANNEL.get() {
        let _ = tx.send(message);
    }
}

/// 一份发布出去的会话状态里，跟名字有关的部分。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Observed {
    title: Option<String>,
    custom_title: Option<String>,
    cwd: Option<String>,
    names: Names,
}

impl Observed {
    fn of(state: &SessionState) -> Self {
        Self {
            title: state.title.clone(),
            custom_title: state.custom_title.clone(),
            cwd: state.cwd.clone(),
            names: Names {
                conversation: state.conversation_name.clone(),
                pane: state.name.clone(),
            },
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Names {
    conversation: Option<String>,
    pane: Option<String>,
}

/// 算名字要的全部输入。只记账、只算，不碰会话。
#[derive(Default)]
struct Book {
    conversations: Vec<WorkspaceConversation>,
    /// 会话 id → 它在哪段对话（`conversations` 的下标）。
    owner: HashMap<String, usize>,
    agents: Agents,
    /// 发布着的会话。
    sessions: HashMap<String, Observed>,
}

impl Book {
    fn set_directory(&mut self, directory: WorkspaceDirectory) {
        self.owner.clear();
        for (ix, conversation) in directory.conversations.iter().enumerate() {
            if let Some(acp) = &conversation.acp {
                self.owner.insert(acp.sid.clone(), ix);
            }
            for terminal in &conversation.terminals {
                if let Some(id) = &terminal.id {
                    self.owner.insert(id.clone(), ix);
                }
            }
        }
        self.conversations = directory.conversations;
    }

    /// `id` 所在那段对话里的全部会话：一格的输入变了，同一段里别的格的对话名可能跟着变。
    /// 不在目录里的只有它自己。
    fn conversation_of(&self, id: &str) -> Vec<String> {
        let Some(&ix) = self.owner.get(id) else {
            return vec![id.to_string()];
        };
        let conversation = &self.conversations[ix];
        conversation
            .acp
            .iter()
            .map(|acp| acp.sid.clone())
            .chain(conversation.terminals.iter().filter_map(|t| t.id.clone()))
            .collect()
    }

    /// `ids` 里发布着、名字跟该叫的不一样的会话，和它们该叫的名字。
    fn stale(&self, ids: impl IntoIterator<Item = String>) -> Vec<(String, Names)> {
        ids.into_iter()
            .filter_map(|id| {
                let observed = self.sessions.get(&id)?;
                let names = self.names_of(&id);
                (names != observed.names).then_some((id, names))
            })
            .collect()
    }

    /// 一段会话现在该叫什么。
    fn names_of(&self, id: &str) -> Names {
        let Some(&ix) = self.owner.get(id) else {
            return Names::default();
        };
        let conversation = &self.conversations[ix];
        if let Some(acp) = &conversation.acp {
            let state = self.sessions.get(&acp.sid);
            // 自定义 Agent 用它自己的名字，其余用种类短名。
            let label = acp
                .profile_id
                .as_ref()
                .and_then(|id| self.agents.profiles.get(id))
                .map(String::as_str)
                .or_else(|| {
                    ConversationAgentKind::from_id(&acp.agent)
                        .map(ConversationAgentKind::short_label)
                })
                .unwrap_or(acp.agent.as_str());
            // 会话名以运行时为准；还没起来、没报过的，用目录里记下的上一次。
            let session_title = state
                .and_then(|state| state.title.as_deref())
                .filter(|title| !title.trim().is_empty())
                .or(acp.session_title.as_deref());
            let definition = acp
                .agent_definition_id
                .as_ref()
                .and_then(|id| self.agents.definitions.get(id))
                .map(String::as_str);
            let cwd = acp
                .cwd
                .as_deref()
                .or(state.and_then(|state| state.cwd.as_deref()));
            let name = acp_conversation_name(
                conversation.custom_title.as_deref(),
                session_title,
                definition,
                label,
                last_segment(cwd),
            );
            return Names {
                conversation: Some(name.clone()),
                pane: Some(name),
            };
        }
        // 对话名取第一格：侧栏、菜单栏、手机看到的是同一个名字，不跟着哪格是活动的变。
        let Some(first) = conversation.terminals.iter().find(|t| t.id.is_some()) else {
            return Names::default();
        };
        let first_state = first.id.as_deref().and_then(|id| self.sessions.get(id));
        let conversation_name = TerminalTitle {
            conversation_custom: first_state.and_then(|state| state.custom_title.as_deref()),
            session_custom: conversation.custom_title.as_deref(),
            pane_count: conversation.terminals.len(),
            pane_custom: first.custom_title.as_deref(),
            session_title: first_state.and_then(|state| state.title.as_deref()),
            launch_label: first.launch_label.as_deref(),
            fallback: terminal_dir(first, first_state),
        }
        .display();
        let pane = if conversation.terminals.len() == 1 {
            conversation_name.clone()
        } else {
            let terminal = conversation
                .terminals
                .iter()
                .find(|t| t.id.as_deref() == Some(id))
                .expect("owner 只记目录里有 id 的终端");
            let state = self.sessions.get(id);
            pane_name(
                state.and_then(|state| state.custom_title.as_deref()),
                terminal.custom_title.as_deref(),
                state.and_then(|state| state.title.as_deref()),
                terminal.launch_label.as_deref(),
                terminal_dir(terminal, state),
            )
        };
        Names {
            conversation: Some(conversation_name),
            pane: Some(pane),
        }
    }
}

/// 终端的工作目录末段：目录里记的优先，没记的用会话报的。
fn terminal_dir<'a>(
    terminal: &'a WorkspaceTerminal,
    state: Option<&'a Observed>,
) -> Option<&'a str> {
    last_segment(
        terminal
            .cwd
            .as_deref()
            .or(state.and_then(|state| state.cwd.as_deref())),
    )
}

/// 路径末段。路径是运行时这台机器上的，按本机的系统切。
fn last_segment(path: Option<&str>) -> Option<&str> {
    Path::new(path?).file_name()?.to_str()
}

/// 名字里用到的智能体配置：定义和自定义 Agent 的名字（去掉空白，空的不记）。
#[derive(Default)]
struct Agents {
    /// 智能体定义 id → 名字。
    definitions: HashMap<String, String>,
    /// 自定义 Agent（profile）id → 名字。
    profiles: HashMap<String, String>,
}

impl Agents {
    fn load() -> Self {
        let settings = match smelt_core::agent_host::load() {
            Ok(settings) => settings,
            Err(error) => {
                crate::dlog(&format!(
                    "[session-names] 读取智能体配置失败，名字里先不带智能体名：{error}"
                ));
                return Self::default();
            }
        };
        let named = |id: String, name: &str| {
            let name = name.trim();
            (!name.is_empty()).then(|| (id, name.to_string()))
        };
        Self {
            definitions: settings
                .agents
                .into_iter()
                .filter_map(|agent| named(agent.id, &agent.name))
                .collect(),
            profiles: settings
                .profiles
                .into_iter()
                .filter_map(|profile| named(profile.id, &profile.label))
                .collect(),
        }
    }
}

struct Worker {
    sessions: Sessions,
    acp_sessions: AcpSessions,
    event_hub: EventHubHandle,
    book: Book,
}

impl Worker {
    fn run(mut self, rx: Receiver<Message>) {
        self.seed();
        while let Ok(message) = rx.recv() {
            let ids = match message {
                Message::Observe { id, observed } => {
                    self.book.sessions.insert(id.clone(), observed);
                    self.book.conversation_of(&id)
                }
                Message::Forget { id } => {
                    self.book.sessions.remove(&id);
                    self.book.conversation_of(&id)
                }
                Message::Directory(directory) => {
                    self.book.set_directory(directory);
                    self.book.sessions.keys().cloned().collect()
                }
                Message::AgentsChanged => {
                    self.book.agents = Agents::load();
                    self.book.sessions.keys().cloned().collect()
                }
            };
            self.apply(self.book.stale(ids));
        }
    }

    /// 线程起来之前已经发布的会话（升级交接接过来的、留下的）和当前目录、智能体配置。之后的变化
    /// 都从通道来。
    fn seed(&mut self) {
        for state in
            crate::session_catalog::published_session_states(&self.sessions, &self.acp_sessions)
        {
            self.book
                .sessions
                .insert(state.id.clone(), Observed::of(&state));
        }
        match smelt_core::workspace_catalog::directory() {
            Ok(directory) => self.book.set_directory(directory),
            Err(error) => crate::dlog(&format!(
                "[session-names] 读取工作区目录失败，等下一次目录推送再算名字：{error}"
            )),
        }
        self.book.agents = Agents::load();
        let ids: Vec<_> = self.book.sessions.keys().cloned().collect();
        self.apply(self.book.stale(ids));
    }

    fn apply(&mut self, stale: Vec<(String, Names)>) {
        for (id, names) in stale {
            self.write(&id, &names);
            // 广播回来的状态会再确认一次；先记下，同一批消息里不重复改。
            if let Some(observed) = self.book.sessions.get_mut(&id) {
                observed.names = names;
            }
        }
    }

    /// 改活的会话状态再广播；没有运行时的改总线上发布着的那份。
    fn write(&self, id: &str, names: &Names) {
        let live = |state: &mut SessionState| {
            set_names(state, names).then(|| {
                bump_state_revision(state);
                state.clone()
            })
        };
        let snapshot = if let Some(snapshot) = self
            .sessions
            .with_live(id, |session| live(&mut session.state.lock().unwrap()))
        {
            snapshot
        } else if let Some(slot) = self.acp_sessions.get(id)
            && let Some(snapshot) = self
                .acp_sessions
                .with_current(id, &slot, |session| live(&mut session.state.lock()))
        {
            snapshot
        } else {
            crate::republish_dormant(&self.event_hub, id, |state| set_names(state, names));
            None
        };
        if let Some(snapshot) = snapshot {
            crate::broadcast_state(&self.event_hub, &snapshot);
        }
    }
}

/// 名字不一样才改，返回改没改。
fn set_names(state: &mut SessionState, names: &Names) -> bool {
    if state.conversation_name == names.conversation && state.name == names.pane {
        return false;
    }
    state.conversation_name = names.conversation.clone();
    state.name = names.pane.clone();
    true
}

#[cfg(test)]
mod tests {
    use smelt_core::workspace_directory::WorkspaceAcp;

    use super::*;

    fn terminal(id: &str, cwd: &str) -> WorkspaceTerminal {
        WorkspaceTerminal {
            id: Some(id.to_string()),
            cwd: Some(cwd.to_string()),
            ..Default::default()
        }
    }

    fn book(conversations: Vec<WorkspaceConversation>) -> Book {
        let mut book = Book::default();
        book.set_directory(WorkspaceDirectory {
            conversations,
            ..Default::default()
        });
        book
    }

    fn observe(book: &mut Book, id: &str, title: Option<&str>) {
        book.sessions.insert(
            id.to_string(),
            Observed {
                title: title.map(str::to_string),
                ..Default::default()
            },
        );
    }

    fn names(conversation: &str, pane: &str) -> Names {
        Names {
            conversation: Some(conversation.to_string()),
            pane: Some(pane.to_string()),
        }
    }

    #[test]
    fn a_single_terminal_uses_its_session_name_then_its_directory() {
        let mut book = book(vec![WorkspaceConversation {
            id: "c".into(),
            terminals: vec![terminal("t1", "/work/smelt")],
            ..Default::default()
        }]);
        observe(&mut book, "t1", None);
        assert_eq!(book.names_of("t1"), names("smelt", "smelt"));
        observe(&mut book, "t1", Some("修侧栏"));
        assert_eq!(book.names_of("t1"), names("修侧栏", "修侧栏"));
    }

    /// 分屏时对话名跟第一格走，每一格还有自己的名字；第一格改了名，别的格的对话名也要改。
    #[test]
    fn split_panes_share_the_first_panes_name_and_keep_their_own() {
        let mut book = book(vec![WorkspaceConversation {
            id: "c".into(),
            terminals: vec![
                terminal("t1", "/work/smelt"),
                WorkspaceTerminal {
                    launch_label: Some("Claude".into()),
                    ..terminal("t2", "/work/smelt")
                },
            ],
            ..Default::default()
        }]);
        observe(&mut book, "t1", Some("修侧栏"));
        observe(&mut book, "t2", None);
        assert_eq!(book.names_of("t1"), names("修侧栏", "修侧栏"));
        assert_eq!(book.names_of("t2"), names("修侧栏", "Claude"));

        for (id, names) in book.stale(["t1".to_string(), "t2".to_string()]) {
            book.sessions.get_mut(&id).unwrap().names = names;
        }
        observe(&mut book, "t1", Some("改存储"));
        let stale: HashMap<_, _> = book.stale(book.conversation_of("t1")).into_iter().collect();
        assert_eq!(stale["t1"], names("改存储", "改存储"));
        assert_eq!(stale["t2"], names("改存储", "Claude"));
    }

    #[test]
    fn acp_prefers_user_name_then_session_then_definition_then_agent_and_directory() {
        let conversation = |custom: Option<&str>| WorkspaceConversation {
            id: "c".into(),
            custom_title: custom.map(str::to_string),
            acp: Some(WorkspaceAcp {
                sid: "a1".into(),
                agent: "claude".into(),
                cwd: Some("/work/smelt".into()),
                agent_definition_id: Some("til".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut book = book(vec![conversation(None)]);
        observe(&mut book, "a1", None);
        let label = ConversationAgentKind::from_id("claude")
            .map(ConversationAgentKind::short_label)
            .unwrap();
        let generic = format!("{label} 对话 · smelt");
        assert_eq!(book.names_of("a1"), names(&generic, &generic));
        book.agents.definitions.insert("til".into(), "Til".into());
        assert_eq!(book.names_of("a1"), names("Til", "Til"));
        observe(&mut book, "a1", Some("会话名"));
        assert_eq!(book.names_of("a1"), names("会话名", "会话名"));
        book.set_directory(WorkspaceDirectory {
            conversations: vec![conversation(Some("改过"))],
            ..Default::default()
        });
        assert_eq!(book.names_of("a1"), names("改过", "改过"));
    }

    /// 不在目录里的会话没有名字；从目录里删掉了，名字也要清掉。
    #[test]
    fn sessions_outside_the_directory_have_no_name() {
        let mut book = book(vec![WorkspaceConversation {
            id: "c".into(),
            terminals: vec![terminal("t1", "/work/smelt")],
            ..Default::default()
        }]);
        observe(&mut book, "t1", None);
        observe(&mut book, "loose", Some("命令行起的"));
        assert_eq!(book.names_of("loose"), Names::default());
        assert!(book.stale(["loose".to_string()]).is_empty());

        book.sessions.get_mut("t1").unwrap().names = names("smelt", "smelt");
        book.set_directory(WorkspaceDirectory::default());
        assert_eq!(
            book.stale(["t1".to_string()]),
            vec![("t1".to_string(), Names::default())]
        );
    }
}
