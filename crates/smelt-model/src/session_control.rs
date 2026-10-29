//! 会话目录的数据：运行时远程目录里的会话记录、可开对话的 agent 选项，以及判断一段
//! 会话该归到哪一栏的纯函数。读写目录、向守护发请求的部分分别在运行时（smelt-core）
//! 和桌面客户端。


use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::agent_kind::ConversationLaunchSpec;
use crate::workspace_directory::{WorkspaceAcp, WorkspaceConversation, WorkspaceDirectory};

/// 可开对话的 agent 选项。出线（`new_session.catalog`）只带展示与回传用的字段；
/// 启动规格和历史目录只在运行时那台机器上有意义，不出线，客户端解出来是空的。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpAgentOption {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub profile: bool,
    /// 产品智能体选项：会话归属的定义 id。由 option id 的 `agent:` 前缀派生，
    /// 见 [`agent_definition_id_from_option_id`]。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_definition_id: Option<String>,
    #[serde(skip)]
    pub launch: ConversationLaunchSpec,
    #[serde(skip)]
    pub history_dir: Option<String>,
}

impl AcpAgentOption {
    /// 手动添加的 workspace profile 在 option id 里带 `profile:` 前缀，而
    /// `session_metadata` 的身份三元组要的是裸 profile id——两边不是同一个字符串，
    /// 转换只此一处，别在调用方各切各的。
    pub fn profile_id(&self) -> Option<&str> {
        self.id.strip_prefix("profile:")
    }

    /// 产品智能体的选项在 id 里带 `agent:` 前缀。归属只编码在 id 里，不另存一份：
    /// 会话落盘时 `agent_option_id` 就够还原「这是哪个智能体的对话」。
    pub fn agent_definition_id(&self) -> Option<&str> {
        agent_definition_id_from_option_id(&self.id)
    }
}

/// 「这个 agent option 属于哪个产品智能体」的唯一判据。选项、落盘的会话记录、
/// 桌面接管移动端会话时都走这一条，别在调用方各切各的前缀。
pub fn agent_definition_id_from_option_id(option_id: &str) -> Option<&str> {
    option_id
        .strip_prefix("agent:")
        .map(str::trim)
        .filter(|id| !id.is_empty())
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySessionSummary {
    /// 运行时那台机器上的存档路径。
    #[serde(skip)]
    pub path: String,
    pub resume_id: String,
    /// agent transcript / summary 自己生成的标题，永远是原始值。
    pub title: String,
    /// 用户在 Smelt 里手动改的名字（`session_metadata` 那份覆盖层）。PC 和移动端
    /// 读的是同一份存储，所以列表要展示哪个名字必须在这里就算好，不能各端各判。
    pub custom_title: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub last_active_at: Option<DateTime<Utc>>,
    pub message_count: usize,
    #[serde(skip)]
    pub total_tokens: u64,
}

impl HistorySessionSummary {
    /// 实际展示的标题：用户改过名就用用户的，否则用 agent 原始标题。
    pub fn display_title(&self) -> &str {
        self.custom_title.as_deref().unwrap_or(&self.title)
    }
}

/// 运行时自己开的一段 ACP 对话（手机、命令行、Pi 派生、`conversation.create_for_agent`）：
/// 起进程用的启动规格，和起好之后写进工作区目录的那一条。之后它跟桌面建的对话一样，
/// 只存在工作区目录里。
#[derive(Clone, Debug)]
pub struct DaemonConversation {
    /// 守护里的 ACP 会话 id。
    pub sid: String,
    pub cwd: String,
    pub option: AcpAgentOption,
    /// 接着哪段 provider 对话聊。
    pub resume_id: Option<String>,
}

impl DaemonConversation {
    /// 起进程的请求，`acp_host::parse_acp_open_request` 认的形状。
    pub fn open_request(&self) -> Value {
        serde_json::json!({
            "id": self.sid,
            "cwd": self.cwd,
            "launch": self.option.launch,
            "agent": self.option.kind,
            "resume_id": self.resume_id,
        })
    }

    /// 进工作区目录的那一条。不存名字：没人起过名的对话按智能体名、agent 显示。
    pub fn directory_entry(&self, conversation_id: String) -> WorkspaceConversation {
        WorkspaceConversation {
            id: conversation_id,
            acp: Some(WorkspaceAcp {
                cwd: Some(self.cwd.clone()),
                sid: self.sid.clone(),
                agent: self.option.kind.clone(),
                profile_id: self.option.profile_id().map(str::to_string),
                history_session_id: self.resume_id.clone(),
                launch: self.option.launch.clone(),
                agent_definition_id: self.option.agent_definition_id().map(str::to_string),
                ..WorkspaceAcp::default()
            }),
            ..WorkspaceConversation::default()
        }
    }
}

pub fn is_background_acp_session_id(id: &str) -> bool {
    id.starts_with("acp-automation-")
}

/// 产品级智能体对话：侧栏「对话」栏，不进项目会话列表。
///
/// 分组跟工作目录走，不跟「用了哪个智能体定义」走。项目里点 + 选一个智能体，
/// 只是让它来做这件项目上的事，对话仍属于那个项目；从智能体页开的对话 cwd
/// 才是托管 space / 旧版工作台目录，那才进「对话」栏。
///
/// `agent_definition_id` 只在还没有 cwd 时当归属（冷恢复占位、测试夹具）。
/// 有 cwd 时，托管目录本身就是判据：归属字段缺失（旧库没落盘）也不会掉进「项目」。
///
/// 托管目录按本机的 `SMELT_HOME` 算，只用来判断本机运行时上的会话；别的机器上的会话用
/// [`is_agent_conversation_on`]。
pub fn is_agent_conversation(
    automation_id: Option<&str>,
    agent_definition_id: Option<&str>,
    cwd: Option<&str>,
) -> bool {
    let home = smelt_paths::smelt_home();
    is_agent_conversation_on(
        home.as_deref().and_then(|path| path.to_str()),
        automation_id,
        agent_definition_id,
        cwd,
    )
}

/// 同 [`is_agent_conversation`]，托管目录按 `smelt_home`（会话所在那台机器的
/// `SMELT_HOME`）算。那台机器的 `SMELT_HOME` 还不知道（运行时早于上报它、从没连上过）
/// 时，有 cwd 的一律算项目：宁可放进项目列表，也不把项目对话藏进「最近对话」。
pub fn is_agent_conversation_on(
    smelt_home: Option<&str>,
    automation_id: Option<&str>,
    agent_definition_id: Option<&str>,
    cwd: Option<&str>,
) -> bool {
    if automation_id.is_some() {
        return false;
    }
    match cwd {
        Some(cwd) => smelt_home.is_some_and(|home| is_agent_conversation_cwd_in(home, cwd)),
        None => agent_definition_id.is_some(),
    }
}

/// cwd 落在 Smelt 为智能体托管的目录下：智能体自己的 space
/// （`~/.smelt/agents/<id>`）或旧版每会话一个的对话目录。两者结构上都不可能
/// 是用户项目。按本机的 `SMELT_HOME` 算。
pub fn is_agent_conversation_cwd(cwd: &str) -> bool {
    smelt_paths::smelt_home().is_some_and(|home| {
        home.to_str()
            .is_some_and(|home| is_agent_conversation_cwd_in(home, cwd))
    })
}

/// 同 [`is_agent_conversation_cwd`]，按 cwd 所在那台机器的 `SMELT_HOME` 算。
pub fn is_agent_conversation_cwd_in(smelt_home: &str, cwd: &str) -> bool {
    crate::agent_paths::is_agent_space_in(smelt_home, cwd)
        || crate::agent_paths::is_workbench_conversation_workspace_in(smelt_home, cwd)
}

/// 不进项目会话列表：智能体对话、自动化 Run，以及旧档里的后台自动化 session。按本机的
/// `SMELT_HOME` 算，别的机器上的会话用 [`is_product_conversation_on`]。
pub fn is_product_conversation(
    automation_id: Option<&str>,
    agent_definition_id: Option<&str>,
    acp_session_id: Option<&str>,
    cwd: Option<&str>,
) -> bool {
    let home = smelt_paths::smelt_home();
    is_product_conversation_on(
        home.as_deref().and_then(|path| path.to_str()),
        automation_id,
        agent_definition_id,
        acp_session_id,
        cwd,
    )
}

/// 同 [`is_product_conversation`]，按会话所在那台机器的 `SMELT_HOME` 算。
pub fn is_product_conversation_on(
    smelt_home: Option<&str>,
    automation_id: Option<&str>,
    agent_definition_id: Option<&str>,
    acp_session_id: Option<&str>,
    cwd: Option<&str>,
) -> bool {
    is_agent_conversation_on(smelt_home, automation_id, agent_definition_id, cwd)
        || automation_id.is_some()
        || acp_session_id.is_some_and(is_background_acp_session_id)
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CreatedAgentConversation {
    pub session_id: String,
    pub agent_definition_id: String,
    pub cwd: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AgentConversationSummary {
    pub session_id: String,
    pub title: String,
    pub agent_definition_id: Option<String>,
    pub cwd: String,
}

pub fn parse_conversation_submit_response(
    response: Value,
) -> Result<(), crate::conversation::ConversationSubmitError> {
    use crate::conversation::ConversationSubmitError;

    match response.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(()),
        Some(false) => {
            let error = response.get("error").cloned().unwrap_or(Value::Null);
            Err(serde_json::from_value(error.clone()).unwrap_or_else(|_| {
                ConversationSubmitError::unknown(
                    error
                        .as_str()
                        .unwrap_or("smeltd returned an invalid conversation submit error"),
                )
            }))
        }
        None => Err(ConversationSubmitError::unknown(
            "smeltd returned no conversation submit result",
        )),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionDeleteKind {
    Terminal,
    Acp,
}

/// 工作区目录里这个会话 id 是终端还是 ACP 会话。不在目录里是 `None`。
pub fn directory_session_kind(
    directory: &WorkspaceDirectory,
    session_id: &str,
) -> Option<SessionDeleteKind> {
    directory.conversations.iter().find_map(|conversation| {
        if conversation
            .acp
            .as_ref()
            .is_some_and(|acp| acp.sid == session_id)
        {
            Some(SessionDeleteKind::Acp)
        } else if conversation
            .terminals
            .iter()
            .any(|terminal| terminal.id.as_deref() == Some(session_id))
        {
            Some(SessionDeleteKind::Terminal)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod machine_home_tests {
    use super::*;

    /// 远程机器上的 space 按那台机器的 SMELT_HOME 认，跟本机的主目录无关。
    #[test]
    fn agent_conversations_are_recognised_by_their_own_machine_home() {
        let remote = "/home/someone/.smelt";
        let space = "/home/someone/.smelt/agents/agent-1";
        assert!(is_agent_conversation_on(
            Some(remote),
            None,
            Some("agent-1"),
            Some(space)
        ));
        assert!(is_agent_conversation_on(
            Some(remote),
            None,
            None,
            Some("/home/someone/.smelt/workspaces/conversations/c1"),
        ));
        // 换一台机器的主目录就不是它的 space。
        assert!(!is_agent_conversation_on(
            Some("/Users/me/.smelt"),
            None,
            Some("agent-1"),
            Some(space),
        ));
    }

    /// 项目里点 + 选智能体开的对话属于项目；主目录还不知道时有 cwd 的也一律算项目。
    #[test]
    fn project_conversations_and_unknown_homes_stay_in_projects() {
        let remote = "/home/someone/.smelt";
        assert!(!is_agent_conversation_on(
            Some(remote),
            None,
            Some("agent-1"),
            Some("/home/someone/code/quant"),
        ));
        assert!(!is_agent_conversation_on(
            None,
            None,
            Some("agent-1"),
            Some("/home/someone/.smelt/agents/agent-1"),
        ));
        // 还没有 cwd（冷恢复占位）时按归属算，跟主目录无关。
        assert!(is_agent_conversation_on(None, None, Some("agent-1"), None));
        assert!(!is_agent_conversation_on(
            Some(remote),
            Some("auto-1"),
            None,
            None
        ));
        assert!(is_product_conversation_on(
            Some(remote),
            Some("auto-1"),
            None,
            None,
            None
        ));
    }
}

#[cfg(test)]
mod daemon_conversation_tests {
    use super::*;

    /// 运行时自己开的对话进工作区目录时带着启动规格、续接身份和智能体归属，不带名字：
    /// 初始名不能当成用户起的名字存下来，不然 agent 后来报的标题永远显示不出来。
    #[test]
    fn a_daemon_conversation_enters_the_directory_without_a_title() {
        let conversation = DaemonConversation {
            sid: "acp-1".to_string(),
            cwd: "/repo".to_string(),
            option: AcpAgentOption {
                id: "agent:writer".to_string(),
                kind: "pi".to_string(),
                label: "写作".to_string(),
                profile: false,
                agent_definition_id: Some("writer".to_string()),
                launch: ConversationLaunchSpec::from_command("smelt-pi".to_string()),
                history_dir: None,
            },
            resume_id: Some("r1".to_string()),
        };
        let entry = conversation.directory_entry("conv-1".to_string());
        assert_eq!(entry.id, "conv-1");
        assert_eq!(entry.custom_title, None);
        let acp = entry.acp.unwrap();
        assert_eq!(acp.sid, "acp-1");
        assert_eq!(acp.agent, "pi");
        assert_eq!(acp.cwd.as_deref(), Some("/repo"));
        assert_eq!(acp.history_session_id.as_deref(), Some("r1"));
        assert_eq!(acp.agent_definition_id.as_deref(), Some("writer"));
        assert_eq!(acp.launch.command, "smelt-pi");
        assert_eq!(acp.session_title, None);
        assert_eq!(conversation.open_request()["id"], "acp-1");
    }
}
