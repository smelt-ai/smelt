//! 会话目录的数据：运行时远程目录里的会话记录、可开对话的 agent 选项，以及判断一段
//! 会话该归到哪一栏的纯函数。读写目录、向守护发请求的部分分别在运行时（smelt-core）
//! 和桌面客户端。

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::agent_kind::ConversationLaunchSpec;
use crate::workspace_menu::{
    WorkspaceMenuProject, WorkspaceMenuSessionKind, WorkspaceMenuSnapshot,
};

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteSessionLifecycle {
    /// 目录已占坑，runtime 还没就绪。客户端不投影；提交失败或启动对账会删掉。
    Creating,
    #[default]
    Active,
    /// A close request is durably recorded while daemon tears down the runtime.
    Closing,
    /// 当次拆卸没完成（例如 ACP 未能证明进程已退出）。冷启动对账会删掉无 runtime 的记录。
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteSessionKind {
    Acp,
    Terminal,
}

/// A transport-safe, unified projection of a remote-owned session. The daemon publishes these
/// through `subscribe`; clients never use it as a write model.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RemoteSessionRecord {
    pub kind: RemoteSessionKind,
    pub id: String,
    pub cwd: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub agent_option_id: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub launch: Option<ConversationLaunchSpec>,
    #[serde(default)]
    pub resume_id: Option<String>,
    pub created_at: i64,
    #[serde(default)]
    pub lifecycle: RemoteSessionLifecycle,
    /// daemon 后台执行（自动化 Run）不进侧栏；用户显式打开后再投影。
    #[serde(default)]
    pub hidden: bool,
}

impl RemoteSessionRecord {
    pub fn is_active(&self) -> bool {
        self.lifecycle == RemoteSessionLifecycle::Active
    }

    /// Active 正常投影；Closing 也要可见，否则目录删除失败后用户没法从 UI 再 kill。
    /// hidden 的后台会话始终不进侧栏。
    pub fn is_visible(&self) -> bool {
        !self.hidden
            && matches!(
                self.lifecycle,
                RemoteSessionLifecycle::Active | RemoteSessionLifecycle::Closing
            )
    }

    pub fn is_present(&self) -> bool {
        matches!(
            self.lifecycle,
            RemoteSessionLifecycle::Active | RemoteSessionLifecycle::Closing
        )
    }

    pub fn as_acp(&self) -> Option<RemoteAcpSession> {
        if self.kind != RemoteSessionKind::Acp {
            return None;
        }
        Some(RemoteAcpSession {
            id: self.id.clone(),
            cwd: self.cwd.clone(),
            title: self.title.clone(),
            agent_option_id: self.agent_option_id.clone()?,
            agent: self.agent.clone()?,
            launch: self.launch.clone()?,
            resume_id: self.resume_id.clone(),
            created_at: self.created_at,
            lifecycle: self.lifecycle,
            hidden: self.hidden,
        })
    }

    pub fn as_terminal(&self) -> Option<RemoteTerminalSession> {
        (self.kind == RemoteSessionKind::Terminal).then(|| RemoteTerminalSession {
            id: self.id.clone(),
            cwd: self.cwd.clone(),
            title: self.title.clone(),
            created_at: self.created_at,
            lifecycle: self.lifecycle,
        })
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RemoteAcpSession {
    pub id: String,
    pub cwd: String,
    #[serde(default)]
    pub title: String,
    pub agent_option_id: String,
    pub agent: String,
    pub launch: ConversationLaunchSpec,
    #[serde(default)]
    pub resume_id: Option<String>,
    pub created_at: i64,
    #[serde(default)]
    pub lifecycle: RemoteSessionLifecycle,
    #[serde(default)]
    pub hidden: bool,
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

impl RemoteAcpSession {
    /// 移动端建的智能体对话：归属编码在 `agent_option_id` 里，桌面接管时靠它
    /// 把会话放回「对话」栏，而不是当成一场普通项目会话。
    pub fn agent_definition_id(&self) -> Option<&str> {
        agent_definition_id_from_option_id(&self.agent_option_id)
    }

    pub fn is_visible(&self) -> bool {
        !self.hidden
            && matches!(
                self.lifecycle,
                RemoteSessionLifecycle::Active | RemoteSessionLifecycle::Closing
            )
    }

    pub fn is_present(&self) -> bool {
        matches!(
            self.lifecycle,
            RemoteSessionLifecycle::Active | RemoteSessionLifecycle::Closing
        )
    }
}

impl From<RemoteAcpSession> for RemoteSessionRecord {
    fn from(session: RemoteAcpSession) -> Self {
        Self {
            kind: RemoteSessionKind::Acp,
            id: session.id,
            cwd: session.cwd,
            title: session.title,
            agent_option_id: Some(session.agent_option_id),
            agent: Some(session.agent),
            launch: Some(session.launch),
            resume_id: session.resume_id,
            created_at: session.created_at,
            lifecycle: session.lifecycle,
            hidden: session.hidden,
        }
    }
}

/// 手机端远程新建的终端会话（不依赖 PC GUI 写工作区快照才能显示/管理）。
/// 跟 `RemoteAcpSession` 是同一套思路，见文件顶部注释。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RemoteTerminalSession {
    pub id: String,
    pub cwd: String,
    #[serde(default)]
    pub title: String,
    pub created_at: i64,
    #[serde(default)]
    pub lifecycle: RemoteSessionLifecycle,
}

impl From<RemoteTerminalSession> for RemoteSessionRecord {
    fn from(session: RemoteTerminalSession) -> Self {
        Self {
            kind: RemoteSessionKind::Terminal,
            id: session.id,
            cwd: session.cwd,
            title: session.title,
            agent_option_id: None,
            agent: None,
            launch: None,
            resume_id: None,
            created_at: session.created_at,
            lifecycle: session.lifecycle,
            hidden: false,
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RemoteSessionSnapshot {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub sessions: Vec<RemoteSessionRecord>,
}

pub fn workspace_projects(menu: &WorkspaceMenuSnapshot) -> Vec<WorkspaceMenuProject> {
    let mut projects = menu.projects.clone();
    projects.sort_by_key(|project| project.order);
    projects
}

/// 延迟 reattach 完成时是否还该插入桌面投影。目录已经撤回，或本地已有同 id，都丢弃。
pub fn accept_remote_terminal_projection(
    id: &str,
    projected_ids: &HashSet<String>,
    local_ids: &HashSet<String>,
) -> bool {
    projected_ids.contains(id) && !local_ids.contains(id)
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

/// 远程目录里的 kind 是所有权真相；菜单只覆盖未进目录的本地会话。
/// 两边都没有就失败，不要默认当 ACP 杀——会把远程终端的目录摘掉却留下 PTY。
pub fn resolve_session_delete_kind(
    catalog_kind: Option<RemoteSessionKind>,
    menu_kind: Option<WorkspaceMenuSessionKind>,
) -> Result<SessionDeleteKind, String> {
    match catalog_kind {
        Some(RemoteSessionKind::Terminal) => Ok(SessionDeleteKind::Terminal),
        Some(RemoteSessionKind::Acp) => Ok(SessionDeleteKind::Acp),
        None => match menu_kind {
            Some(WorkspaceMenuSessionKind::Terminal) => Ok(SessionDeleteKind::Terminal),
            Some(WorkspaceMenuSessionKind::Acp) => Ok(SessionDeleteKind::Acp),
            None => Err("session not found".to_string()),
        },
    }
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
