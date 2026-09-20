//! UI-independent ACP session catalog and lifecycle requests.
//!
//! Desktop and mobile render different controls, but agent/profile resolution, transcript
//! discovery and smeltd lifecycle operations belong here. Mobile clients only send stable ids;
//! launch commands and local paths never need to be assembled on the phone.

use crate::agent_kind::{
    AcpProfile, ConversationAgentKind, ConversationLaunchSpec, HistorySourceKind,
};
use crate::control_api::{EmptyParams, SessionList};
use crate::daemon_protocol::DaemonOperation;
use crate::workspace_menu::{
    WorkspaceMenuProject, WorkspaceMenuSessionKind, WorkspaceMenuSnapshot,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug, serde::Serialize)]
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
    #[serde(skip_serializing)]
    pub launch: ConversationLaunchSpec,
    #[serde(skip_serializing)]
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
    #[serde(skip)]
    pub path: PathBuf,
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
pub fn is_agent_conversation(
    automation_id: Option<&str>,
    agent_definition_id: Option<&str>,
    cwd: Option<&str>,
) -> bool {
    if automation_id.is_some() {
        return false;
    }
    match cwd {
        Some(cwd) => is_agent_conversation_cwd(cwd),
        None => agent_definition_id.is_some(),
    }
}

/// cwd 落在 Smelt 为智能体托管的目录下：智能体自己的 space
/// （`~/.smelt/agents/<id>`）或旧版每会话一个的对话目录。两者结构上都不可能
/// 是用户项目。
pub fn is_agent_conversation_cwd(cwd: &str) -> bool {
    let path = std::path::Path::new(cwd);
    crate::agent_definition_store::is_agent_space(path)
        || crate::agent_definition_store::is_workbench_conversation_workspace(path)
}

/// 不进项目会话列表：智能体对话、自动化 Run，以及旧档里的后台自动化 session。
pub fn is_product_conversation(
    automation_id: Option<&str>,
    agent_definition_id: Option<&str>,
    acp_session_id: Option<&str>,
    cwd: Option<&str>,
) -> bool {
    is_agent_conversation(automation_id, agent_definition_id, cwd)
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

/// The daemon-owned durable catalog for remote-created sessions.
///
/// 远程 ACP / 终端会话共用一份类型化快照。只有本对象写入；损坏数据会显式报错，
/// 不能伪装成空目录。
#[derive(Debug)]
pub struct RemoteSessionCatalog {
    acp_path: Option<PathBuf>,
    terminal_path: Option<PathBuf>,
    acp_sessions: Vec<RemoteAcpSession>,
    terminal_sessions: Vec<RemoteTerminalSession>,
    revision: u64,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RemoteSessionSnapshot {
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub sessions: Vec<RemoteSessionRecord>,
}

fn smelt_path(name: &str) -> PathBuf {
    smelt_paths::smelt_home()
        .unwrap_or_else(|| PathBuf::from("/tmp/.smelt"))
        .join(name)
}

fn load_agent_ui_snapshot() -> Option<smelt_store::AgentUiSnapshot> {
    crate::sqlite_state::default_sqlite_store()
        .ok()?
        .get_agent_ui_snapshot()
        .ok()
        .flatten()
}

fn configured_command(
    snapshot: Option<&smelt_store::AgentUiSnapshot>,
    kind: ConversationAgentKind,
) -> String {
    let key = kind.descriptor().config_key;
    snapshot
        .and_then(|snapshot| {
            snapshot
                .commands
                .iter()
                .find(|command| command.command_key == key)
                .map(|command| command.command.clone())
        })
        .filter(|command| !command.trim().is_empty())
        .unwrap_or_else(|| kind.default_cmd())
}

fn configured_env(
    snapshot: Option<&smelt_store::AgentUiSnapshot>,
    kind: ConversationAgentKind,
) -> BTreeMap<String, String> {
    snapshot
        .map(|snapshot| {
            snapshot
                .env
                .iter()
                .filter(|record| record.engine_kind_id == kind.id())
                .map(|record| (record.name.clone(), record.value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn configured_launch(
    snapshot: Option<&smelt_store::AgentUiSnapshot>,
    kind: ConversationAgentKind,
) -> ConversationLaunchSpec {
    let mut launch = kind.default_launch();
    launch.command = configured_command(snapshot, kind);
    launch.env.extend(configured_env(snapshot, kind));
    launch
}

pub fn agent_options() -> Vec<AcpAgentOption> {
    let snapshot = load_agent_ui_snapshot();
    let snapshot_ref = snapshot.as_ref();
    let mut options = ConversationAgentKind::ALL
        .into_iter()
        .filter(|kind| kind.is_bare_kind())
        .map(|kind| AcpAgentOption {
            id: kind.id().to_string(),
            kind: kind.id().to_string(),
            label: kind.label().to_string(),
            profile: false,
            agent_definition_id: None,
            launch: configured_launch(snapshot_ref, kind),
            history_dir: None,
        })
        .collect::<Vec<_>>();
    let profiles = snapshot
        .as_ref()
        .map(|snapshot| {
            snapshot
                .profiles
                .iter()
                .map(|profile| AcpProfile {
                    id: profile.id.clone(),
                    kind_id: profile.kind_id.clone(),
                    label: profile.label.clone(),
                    workspace_dir: profile.workspace_dir.clone(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for profile in profiles {
        let Some(kind) = profile.kind() else {
            continue;
        };
        let Ok(mut launch) = profile.launch_spec() else {
            continue;
        };
        launch.command = configured_command(snapshot_ref, kind);
        launch.env.extend(configured_env(snapshot_ref, kind));
        // profile 的 workspace 变量定义身份，必须在通用 agent 环境之后写回。
        if let Ok(name) = profile.env_var() {
            launch
                .env
                .insert(name.into(), profile.workspace_dir.clone());
        }
        options.push(AcpAgentOption {
            id: format!("profile:{}", profile.id),
            kind: kind.id().to_string(),
            label: profile.label,
            profile: true,
            agent_definition_id: None,
            launch,
            history_dir: Some(crate::workspace_override::expand_tilde(
                &profile.workspace_dir,
            )),
        });
    }
    for profile in crate::agent_kind::native_dsh_profiles() {
        let Ok(launch) = profile.launch_spec() else {
            continue;
        };
        options.push(AcpAgentOption {
            id: format!("profile:{}", profile.id),
            kind: ConversationAgentKind::Dsh.id().to_string(),
            label: profile.label,
            profile: true,
            agent_definition_id: None,
            launch,
            history_dir: crate::agent_kind::dsh_sessions_root()
                .map(|path| path.to_string_lossy().into_owned()),
        });
    }
    options
}

/// 产品智能体的新建对话选项。与裸引擎/profile 分开产出：那两类是「用哪个 CLI」，
/// 这一类是「用哪个智能体」，混进 `agent_options()` 会让按 kind 找 profile 的
/// 调用方误命中。launch 在这里就把工作方式与插件注入好，桌面与移动端同一份。
pub fn agent_definition_options() -> Vec<AcpAgentOption> {
    let snapshot = load_agent_ui_snapshot();
    let snapshot_ref = snapshot.as_ref();
    crate::agent_definition_store::load_agent_definitions()
        .into_iter()
        .filter(|definition| definition.is_conversation_ready())
        .filter_map(|definition| {
            let kind = definition.engine_kind()?;
            let launch = crate::agent_definition_store::prepare_agent_definition_launch(
                configured_launch(snapshot_ref, kind),
                Some(&definition),
            );
            Some(AcpAgentOption {
                id: format!("agent:{}", definition.id),
                kind: kind.id().to_string(),
                label: definition.name.clone(),
                profile: false,
                agent_definition_id: Some(definition.id),
                launch,
                history_dir: None,
            })
        })
        .collect()
}

pub fn find_agent_option(id: &str) -> Option<AcpAgentOption> {
    if agent_definition_id_from_option_id(id).is_some() {
        return agent_definition_options()
            .into_iter()
            .find(|option| option.id == id);
    }
    agent_options().into_iter().find(|option| option.id == id)
}

pub fn find_agent_option_for(
    kind: ConversationAgentKind,
    profile_id: Option<&str>,
) -> Option<AcpAgentOption> {
    agent_options()
        .into_iter()
        .find(|option| option.kind == kind.id() && option.profile_id() == profile_id)
}

/// 历史自定义标题的唯一写入口。smeltd 写 overlay 并同步远程目录。
pub fn rename_history_title(
    kind: HistorySourceKind,
    profile_id: Option<&str>,
    resume_id: &str,
    title: Option<&str>,
    cwd: Option<&str>,
) -> Result<(), String> {
    let mut request = json!({
        "op": DaemonOperation::HistoryRename,
        "agent": kind.id(),
        "resume_id": resume_id,
    });
    if let Some(profile_id) = profile_id {
        request["profile_id"] = json!(profile_id);
    }
    match title {
        Some(title) => request["title"] = json!(title),
        None => request["title"] = Value::Null,
    }
    if let Some(cwd) = cwd {
        request["cwd"] = json!(cwd);
    }
    daemon_request(request)?;
    Ok(())
}

pub fn workspace_projects(menu: &WorkspaceMenuSnapshot) -> Vec<WorkspaceMenuProject> {
    let mut projects = menu.projects.clone();
    projects.sort_by_key(|project| project.order);
    projects
}

fn remote_sessions_path() -> PathBuf {
    smelt_path(smelt_store::DATABASE_FILE_NAME)
}

fn remote_terminal_sessions_path() -> PathBuf {
    smelt_path(smelt_store::DATABASE_FILE_NAME)
}

impl RemoteSessionCatalog {
    /// Open the production catalog. Only `smeltd` should call this constructor.
    pub fn load_default() -> Result<Self, String> {
        Self::load_from_paths(
            Some(remote_sessions_path()),
            Some(remote_terminal_sessions_path()),
        )
    }

    /// A non-persistent catalog for daemon protocol tests.
    pub fn in_memory() -> Self {
        Self {
            acp_path: None,
            terminal_path: None,
            acp_sessions: Vec::new(),
            terminal_sessions: Vec::new(),
            revision: 0,
        }
    }

    fn load_from_paths(
        acp_path: Option<PathBuf>,
        terminal_path: Option<PathBuf>,
    ) -> Result<Self, String> {
        let mut acp_sessions = Vec::new();
        let mut terminal_sessions = Vec::new();
        if let Some(path) = acp_path.as_ref().or(terminal_path.as_ref()) {
            match crate::sqlite_state::open_sqlite_store(path) {
                Ok(store) => {
                    if let Some(snapshot) = store.get_remote_session_snapshot()? {
                        acp_sessions = snapshot
                            .acp
                            .into_iter()
                            .map(acp_from_record)
                            .collect::<Result<Vec<_>, _>>()?;
                        terminal_sessions = snapshot
                            .terminal
                            .into_iter()
                            .map(terminal_from_record)
                            .collect::<Vec<_>>();
                    }
                }
                Err(error) if path.is_file() => {
                    return Err(format!(
                        "invalid remote session catalog {}: {error}",
                        path.display()
                    ));
                }
                Err(_) => {}
            }
        }
        validate_unique_remote_ids(&acp_sessions, &terminal_sessions)?;
        Ok(Self {
            acp_path,
            terminal_path,
            acp_sessions,
            terminal_sessions,
            revision: 0,
        })
    }

    pub fn snapshot(&self) -> RemoteSessionSnapshot {
        RemoteSessionSnapshot {
            revision: self.revision,
            sessions: self.records(),
        }
    }

    pub fn records(&self) -> Vec<RemoteSessionRecord> {
        self.acp_sessions
            .iter()
            .cloned()
            .map(RemoteSessionRecord::from)
            .chain(
                self.terminal_sessions
                    .iter()
                    .cloned()
                    .map(RemoteSessionRecord::from),
            )
            .collect()
    }

    pub fn remote_acp(&self, id: &str) -> Option<RemoteAcpSession> {
        self.acp_sessions
            .iter()
            .find(|session| session.id == id)
            .cloned()
    }

    pub fn remote_terminal(&self, id: &str) -> Option<RemoteTerminalSession> {
        self.terminal_sessions
            .iter()
            .find(|session| session.id == id)
            .cloned()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.kind_for(id).is_some()
    }

    pub fn kind_for(&self, id: &str) -> Option<RemoteSessionKind> {
        if self.acp_sessions.iter().any(|session| session.id == id) {
            return Some(RemoteSessionKind::Acp);
        }
        self.terminal_sessions
            .iter()
            .any(|session| session.id == id)
            .then_some(RemoteSessionKind::Terminal)
    }

    /// 目录只保留当前仍有 runtime 的远程会话。活着的提升为 Active；没有进程的直接删掉，
    /// 不要标 Failed 永久躺在磁盘上——客户端只投影 Active，Failed 既看不见也清不掉。
    pub fn retain_live_runtimes(
        &mut self,
        live_acp_ids: &HashSet<String>,
        live_terminal_ids: &HashSet<String>,
    ) -> Result<bool, String> {
        let acp_ids = self
            .acp_sessions
            .iter()
            .map(|session| session.id.clone())
            .collect::<Vec<_>>();
        let terminal_ids = self
            .terminal_sessions
            .iter()
            .map(|session| session.id.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for id in acp_ids {
            if live_acp_ids.contains(&id) {
                if self
                    .remote_acp(&id)
                    .is_some_and(|session| session.lifecycle != RemoteSessionLifecycle::Active)
                {
                    self.set_lifecycle_for_kind(
                        RemoteSessionKind::Acp,
                        &id,
                        RemoteSessionLifecycle::Active,
                    )?;
                    changed = true;
                }
            } else {
                self.remove_for_kind(RemoteSessionKind::Acp, &id)?;
                changed = true;
            }
        }
        for id in terminal_ids {
            if live_terminal_ids.contains(&id) {
                if self
                    .remote_terminal(&id)
                    .is_some_and(|session| session.lifecycle != RemoteSessionLifecycle::Active)
                {
                    self.set_lifecycle_for_kind(
                        RemoteSessionKind::Terminal,
                        &id,
                        RemoteSessionLifecycle::Active,
                    )?;
                    changed = true;
                }
            } else {
                self.remove_for_kind(RemoteSessionKind::Terminal, &id)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    pub fn upsert_acp(&mut self, session: RemoteAcpSession) -> Result<(), String> {
        if session.id.is_empty() {
            return Err("remote session id must not be empty".to_string());
        }
        if self
            .terminal_sessions
            .iter()
            .any(|existing| existing.id == session.id)
        {
            return Err(format!(
                "remote session id {} already belongs to a terminal",
                session.id
            ));
        }
        let mut updated = self.acp_sessions.clone();
        match updated
            .iter_mut()
            .find(|existing| existing.id == session.id)
        {
            Some(existing) => {
                if existing.cwd != session.cwd
                    || existing.agent_option_id != session.agent_option_id
                    || existing.resume_id != session.resume_id
                    || existing.agent != session.agent
                {
                    return Err("requestId is already bound to different session parameters".into());
                }
                existing.title = session.title;
                existing.launch = session.launch;
                existing.lifecycle = session.lifecycle;
                existing.hidden = session.hidden;
            }
            None => updated.push(session),
        }
        self.save_acp(&updated)?;
        self.acp_sessions = updated;
        self.bump_revision();
        Ok(())
    }

    pub fn upsert_terminal(&mut self, session: RemoteTerminalSession) -> Result<(), String> {
        if session.id.is_empty() {
            return Err("remote session id must not be empty".to_string());
        }
        if self
            .acp_sessions
            .iter()
            .any(|existing| existing.id == session.id)
        {
            return Err(format!(
                "remote session id {} already belongs to an ACP session",
                session.id
            ));
        }
        let mut updated = self.terminal_sessions.clone();
        match updated
            .iter_mut()
            .find(|existing| existing.id == session.id)
        {
            Some(existing) => {
                if existing.cwd != session.cwd {
                    return Err("requestId is already bound to different session parameters".into());
                }
                existing.title = session.title;
                existing.lifecycle = session.lifecycle;
            }
            None => updated.push(session),
        }
        self.save_terminal(&updated)?;
        self.terminal_sessions = updated;
        self.bump_revision();
        Ok(())
    }

    pub fn set_lifecycle(
        &mut self,
        id: &str,
        lifecycle: RemoteSessionLifecycle,
    ) -> Result<Option<RemoteSessionRecord>, String> {
        let Some(kind) = self.kind_for(id) else {
            return Ok(None);
        };
        self.set_lifecycle_for_kind(kind, id, lifecycle)
    }

    pub fn set_lifecycle_for_kind(
        &mut self,
        kind: RemoteSessionKind,
        id: &str,
        lifecycle: RemoteSessionLifecycle,
    ) -> Result<Option<RemoteSessionRecord>, String> {
        match kind {
            RemoteSessionKind::Acp => {
                let Some(index) = self
                    .acp_sessions
                    .iter()
                    .position(|session| session.id == id)
                else {
                    return Ok(None);
                };
                let mut updated = self.acp_sessions.clone();
                updated[index].lifecycle = lifecycle;
                self.save_acp(&updated)?;
                self.acp_sessions = updated;
                self.bump_revision();
                Ok(self.remote_acp(id).map(RemoteSessionRecord::from))
            }
            RemoteSessionKind::Terminal => {
                let Some(index) = self
                    .terminal_sessions
                    .iter()
                    .position(|session| session.id == id)
                else {
                    return Ok(None);
                };
                let mut updated = self.terminal_sessions.clone();
                updated[index].lifecycle = lifecycle;
                self.save_terminal(&updated)?;
                self.terminal_sessions = updated;
                self.bump_revision();
                Ok(self.remote_terminal(id).map(RemoteSessionRecord::from))
            }
        }
    }

    pub fn remove(&mut self, id: &str) -> Result<Option<RemoteSessionRecord>, String> {
        let Some(kind) = self.kind_for(id) else {
            return Ok(None);
        };
        self.remove_for_kind(kind, id)
    }

    pub fn remove_for_kind(
        &mut self,
        kind: RemoteSessionKind,
        id: &str,
    ) -> Result<Option<RemoteSessionRecord>, String> {
        match kind {
            RemoteSessionKind::Acp => {
                let Some(index) = self
                    .acp_sessions
                    .iter()
                    .position(|session| session.id == id)
                else {
                    return Ok(None);
                };
                let removed = self.acp_sessions[index].clone();
                let mut updated = self.acp_sessions.clone();
                updated.remove(index);
                self.save_acp(&updated)?;
                self.acp_sessions = updated;
                self.bump_revision();
                Ok(Some(removed.into()))
            }
            RemoteSessionKind::Terminal => {
                let Some(index) = self
                    .terminal_sessions
                    .iter()
                    .position(|session| session.id == id)
                else {
                    return Ok(None);
                };
                let removed = self.terminal_sessions[index].clone();
                let mut updated = self.terminal_sessions.clone();
                updated.remove(index);
                self.save_terminal(&updated)?;
                self.terminal_sessions = updated;
                self.bump_revision();
                Ok(Some(removed.into()))
            }
        }
    }

    /// Update titles for all remote ACP sessions resumed from the same provider session.
    pub fn rename_acp_by_resume_id(
        &mut self,
        agent_option_id: &str,
        resume_id: &str,
        title: &str,
    ) -> Result<bool, String> {
        let mut updated = self.acp_sessions.clone();
        let mut changed = false;
        for session in &mut updated {
            if session.agent_option_id == agent_option_id
                && session.resume_id.as_deref() == Some(resume_id)
                && session.title != title
            {
                session.title = title.to_string();
                changed = true;
            }
        }
        if changed {
            self.save_acp(&updated)?;
            self.acp_sessions = updated;
            self.bump_revision();
        }
        Ok(changed)
    }

    fn save_acp(&self, sessions: &[RemoteAcpSession]) -> Result<(), String> {
        persist_remote_catalog_parts(
            self.acp_path.as_ref(),
            self.terminal_path.as_ref(),
            sessions,
            &self.terminal_sessions,
        )
    }

    fn save_terminal(&self, sessions: &[RemoteTerminalSession]) -> Result<(), String> {
        persist_remote_catalog_parts(
            self.acp_path.as_ref(),
            self.terminal_path.as_ref(),
            &self.acp_sessions,
            sessions,
        )
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.checked_add(1).unwrap_or(1);
    }
}

/// 目录沿用两个历史逻辑文档，但 runtime 命名空间是共享的。若允许跨 kind 重名，
/// 通用的 lifecycle/remove 请求就会按内部遍历顺序修改错误记录，因此在读盘和写入
/// 两端都把“全目录唯一 ID”作为不变量。
fn validate_unique_remote_ids(
    acp_sessions: &[RemoteAcpSession],
    terminal_sessions: &[RemoteTerminalSession],
) -> Result<(), String> {
    let mut ids = HashSet::with_capacity(acp_sessions.len() + terminal_sessions.len());
    for id in acp_sessions
        .iter()
        .map(|session| session.id.as_str())
        .chain(terminal_sessions.iter().map(|session| session.id.as_str()))
    {
        if id.is_empty() {
            return Err("remote session catalog contains an empty id".to_string());
        }
        if !ids.insert(id) {
            return Err(format!("duplicate remote session id in catalog: {id}"));
        }
    }
    Ok(())
}

fn persist_remote_catalog_parts(
    acp_path: Option<&PathBuf>,
    terminal_path: Option<&PathBuf>,
    acp_sessions: &[RemoteAcpSession],
    terminal_sessions: &[RemoteTerminalSession],
) -> Result<(), String> {
    let Some(path) = acp_path.or(terminal_path) else {
        return Ok(());
    };
    let store = crate::sqlite_state::open_sqlite_store(path)?;
    store.put_remote_session_snapshot(&smelt_store::RemoteSessionCatalogSnapshot {
        acp: acp_sessions
            .iter()
            .map(acp_to_record)
            .collect::<Result<Vec<_>, _>>()?,
        terminal: terminal_sessions.iter().map(terminal_to_record).collect(),
    })?;
    Ok(())
}

fn acp_to_record(
    session: &RemoteAcpSession,
) -> Result<smelt_store::RemoteAcpSessionRecord, String> {
    Ok(smelt_store::RemoteAcpSessionRecord {
        id: session.id.clone(),
        cwd: session.cwd.clone(),
        title: session.title.clone(),
        agent_option_id: session.agent_option_id.clone(),
        agent: session.agent.clone(),
        launch_command: session.launch.command.clone(),
        launch_env_json: serde_json::to_vec(&session.launch.env)
            .map_err(|error| error.to_string())?,
        resume_id: session.resume_id.clone(),
        created_at: session.created_at,
        lifecycle: remote_lifecycle_name(session.lifecycle).to_string(),
        hidden: session.hidden,
    })
}

fn acp_from_record(
    record: smelt_store::RemoteAcpSessionRecord,
) -> Result<RemoteAcpSession, String> {
    Ok(RemoteAcpSession {
        id: record.id,
        cwd: record.cwd,
        title: record.title,
        agent_option_id: record.agent_option_id,
        agent: record.agent,
        launch: ConversationLaunchSpec {
            command: record.launch_command,
            env: serde_json::from_slice(&record.launch_env_json).unwrap_or_default(),
        },
        resume_id: record.resume_id,
        created_at: record.created_at,
        lifecycle: remote_lifecycle_from_name(&record.lifecycle)?,
        hidden: record.hidden,
    })
}

fn terminal_to_record(session: &RemoteTerminalSession) -> smelt_store::RemoteTerminalSessionRecord {
    smelt_store::RemoteTerminalSessionRecord {
        id: session.id.clone(),
        cwd: session.cwd.clone(),
        title: session.title.clone(),
        created_at: session.created_at,
        lifecycle: remote_lifecycle_name(session.lifecycle).to_string(),
    }
}

fn terminal_from_record(record: smelt_store::RemoteTerminalSessionRecord) -> RemoteTerminalSession {
    RemoteTerminalSession {
        id: record.id,
        cwd: record.cwd,
        title: record.title,
        created_at: record.created_at,
        lifecycle: remote_lifecycle_from_name(&record.lifecycle).unwrap_or_default(),
    }
}

fn remote_lifecycle_name(lifecycle: RemoteSessionLifecycle) -> &'static str {
    match lifecycle {
        RemoteSessionLifecycle::Creating => "creating",
        RemoteSessionLifecycle::Active => "active",
        RemoteSessionLifecycle::Closing => "closing",
        RemoteSessionLifecycle::Failed => "failed",
    }
}

fn remote_lifecycle_from_name(name: &str) -> Result<RemoteSessionLifecycle, String> {
    match name {
        "creating" => Ok(RemoteSessionLifecycle::Creating),
        "active" => Ok(RemoteSessionLifecycle::Active),
        "closing" => Ok(RemoteSessionLifecycle::Closing),
        "failed" => Ok(RemoteSessionLifecycle::Failed),
        other => Err(format!("未知远程会话 lifecycle: {other}")),
    }
}

/// smeltd 当前活着的会话 id（终端 + ACP，靠前缀区分）。任务对账用：判断绑定会话
/// 是否还活着，不活的任务标失败，避免「会话没了但任务永远卡 Running」。
pub fn list_sessions() -> Result<Vec<String>, String> {
    // 只读探测可以安全回退：老守护不认识 `control` 会关闭第一条连接，第二条连接仍走
    // 稳定的 legacy `list`。有副作用的调用绝不能使用这种“失败后换协议重试”。
    if let Ok(result) = crate::control_client::call::<SessionList>(&EmptyParams::default()) {
        return Ok(result.sessions);
    }
    let response = daemon_request(json!({ "op": DaemonOperation::List }))?;
    Ok(response
        .get("sessions")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default())
}

const DAEMON_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// 向 smeltd 发一个短请求并等待一行 JSON 回包。适合所有 daemon-owned 小型领域命令；
/// 长连接流式协议（terminal / ACP / subscribe）仍各自管理 socket 生命周期。
pub fn daemon_request(request: Value) -> Result<Value, String> {
    daemon_request_with_timeout(request, DAEMON_REQUEST_TIMEOUT)
}

fn daemon_request_with_timeout(request: Value, timeout: Duration) -> Result<Value, String> {
    let response = daemon_response_with_timeout(request, timeout)?;
    if response.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("session operation failed")
            .to_string());
    }
    Ok(response)
}

fn daemon_response_with_timeout(request: Value, timeout: Duration) -> Result<Value, String> {
    let mut stream = UnixStream::connect(crate::daemon_state::smeltd_sock_path())
        .map_err(|error| format!("smeltd unavailable: {error}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| format!("configure smeltd write timeout: {error}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| format!("configure smeltd read timeout: {error}"))?;
    writeln!(stream, "{request}").map_err(|error| error.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if line.trim().is_empty() {
        return Err("smeltd returned no response".into());
    }
    let response: Value = serde_json::from_str(line.trim()).map_err(|error| error.to_string())?;
    Ok(response)
}

/// 延迟 reattach 完成时是否还该插入桌面投影。目录已经撤回，或本地已有同 id，都丢弃。
pub fn accept_remote_terminal_projection(
    id: &str,
    projected_ids: &HashSet<String>,
    local_ids: &HashSet<String>,
) -> bool {
    projected_ids.contains(id) && !local_ids.contains(id)
}

/// 桌面发布侧栏菜单。smeltd 是唯一持有者；网关只消费 subscribe，不读工作区快照。
pub fn publish_workspace_menu(menu: &WorkspaceMenuSnapshot) -> Result<(), String> {
    daemon_request(serde_json::json!({
        "op": DaemonOperation::WorkspaceMenu,
        "menu": menu,
    }))?;
    Ok(())
}

/// Fetch the daemon-owned remote catalog. GUI and gateway callers only project this snapshot;
/// they must never fall back to reading the historical documents directly.
pub fn remote_session_snapshot() -> Result<RemoteSessionSnapshot, String> {
    let response = daemon_request(serde_json::json!({
        "op": DaemonOperation::RemoteSessions
    }))?;
    response
        .get("remote_sessions")
        .cloned()
        .ok_or_else(|| "smeltd returned no remote session catalog".to_string())
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
}

pub fn submit_automation_command(
    command: &crate::automation::AutomationCommand,
) -> Result<
    (
        crate::automation::AutomationCommandResult,
        crate::automation::AutomationFile,
    ),
    String,
> {
    let response = daemon_request(serde_json::json!({
        "op": DaemonOperation::AutomationCommand,
        "command": command,
    }))?;
    let result = response
        .get("result")
        .cloned()
        .ok_or_else(|| "smeltd returned no automation command result".to_string())
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))?;
    let snapshot = response
        .get("automations")
        .cloned()
        .ok_or_else(|| "smeltd returned no automation snapshot".to_string())
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))?;
    Ok((result, snapshot))
}

pub fn publish_automation_event(
    event: &crate::automation::AutomationInboundEvent,
) -> Result<serde_json::Value, String> {
    daemon_request(serde_json::json!({
        "op": DaemonOperation::EventPublish,
        "event": event,
    }))
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CreatedAgentConversation {
    pub session_id: String,
    pub agent_definition_id: String,
    pub cwd: String,
}

/// 用产品智能体定义开一场对话。会话进 daemon 远程目录，桌面会投影到「对话」栏。
pub fn create_agent_conversation(
    agent_definition_id: &str,
) -> Result<CreatedAgentConversation, String> {
    let agent_definition_id = agent_definition_id.trim();
    if agent_definition_id.is_empty() {
        return Err("智能体 id 不能为空".into());
    }
    let definition = crate::agent_definition_store::load_agent_definitions()
        .into_iter()
        .find(|definition| definition.id == agent_definition_id)
        .ok_or_else(|| format!("智能体不存在: {agent_definition_id}"))?;
    if !definition.is_conversation_ready() {
        return Err("智能体还不能开对话：需要已注册的产品引擎（目前是 Pi）".into());
    }
    let option = find_agent_option(&format!("agent:{agent_definition_id}"))
        .ok_or_else(|| "智能体不可开对话".to_string())?;
    let cwd = crate::agent_definition_store::ensure_agent_space(agent_definition_id)
        .ok_or_else(|| "无法创建智能体工作区".to_string())?;
    let session_id = format!("acp-{}", uuid::Uuid::new_v4());
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0);
    let title = if definition.name.trim().is_empty() {
        "智能体对话".to_string()
    } else {
        definition.name.clone()
    };
    create_remote_acp_session(&RemoteAcpSession {
        id: session_id.clone(),
        cwd: cwd.to_string_lossy().into_owned(),
        title,
        agent_option_id: option.id,
        agent: option.kind,
        launch: option.launch,
        resume_id: None,
        created_at,
        lifecycle: RemoteSessionLifecycle::default(),
        hidden: false,
    })?;
    Ok(CreatedAgentConversation {
        session_id,
        agent_definition_id: agent_definition_id.to_string(),
        cwd: cwd.to_string_lossy().into_owned(),
    })
}

pub fn create_remote_acp_session(session: &RemoteAcpSession) -> Result<(), String> {
    daemon_request(serde_json::json!({
        "op": DaemonOperation::AcpCreate,
        "id": session.id,
        "cwd": session.cwd,
        "launch": session.launch,
        "agent": session.agent,
        "resume_id": session.resume_id,
        "remote_session": session,
    }))?;
    Ok(())
}

/// The catalog is removed by `smeltd` only after the provider has actually exited.
pub fn delete_acp_session(id: &str) -> Result<(), String> {
    daemon_request(serde_json::json!({
        "op": DaemonOperation::AcpKill,
        "id": id
    }))?;
    Ok(())
}

/// 提交一次交互式会话输入。路由由 smeltd 保存的 ConversationBinding 决定；
/// 调用方不能在失败后自行改走 ACP，否则远端请求结果不确定时会重复执行。
///
/// `config_values` 是用户刚点、agent 还没回包确认的会话配置（尤其是模型）。
/// 必须和正文走同一条 `acp_submit_input`：切模型走 acp_open action 流、发消息
/// 走这条短 RPC，两条通道没有顺序，空闲时一切就发会让 prompt 先开回合，
/// 模型切换被排到下一轮，看起来像没生效。
pub fn submit_conversation_input(
    id: &str,
    input: &crate::conversation::ConversationInput,
    config_values: &[(String, String)],
) -> Result<crate::conversation::ConversationInputRoute, crate::conversation::ConversationSubmitError>
{
    // 插件路由可能包含上传与一次远端 HTTP 请求，必须覆盖 daemon 侧 30s
    // invocation deadline；超时后结果不明，调用方会恢复草稿但不会改路由重发。
    let mut request = serde_json::json!({
        "op": DaemonOperation::AcpSubmitInput,
        "id": id,
        "input": input,
    });
    if !config_values.is_empty() {
        request["config_values"] = serde_json::to_value(config_values)
            .expect("config_values 是 String 对，序列化不会失败");
    }
    let response = daemon_response_with_timeout(request, Duration::from_secs(35))
        .map_err(crate::conversation::ConversationSubmitError::unknown)?;
    parse_conversation_submit_response(response)
}

pub fn parse_conversation_submit_response(
    response: Value,
) -> Result<crate::conversation::ConversationInputRoute, crate::conversation::ConversationSubmitError>
{
    use crate::conversation::ConversationSubmitError;

    if response.get("ok").and_then(Value::as_bool) == Some(false) {
        let error = response.get("error").cloned().unwrap_or(Value::Null);
        return Err(serde_json::from_value(error.clone()).unwrap_or_else(|_| {
            ConversationSubmitError::unknown(
                error
                    .as_str()
                    .unwrap_or("smeltd returned an invalid conversation submit error"),
            )
        }));
    }
    response
        .get("route")
        .cloned()
        .ok_or_else(|| ConversationSubmitError::unknown("smeltd returned no input route"))
        .and_then(|value| {
            serde_json::from_value(value).map_err(|error| {
                ConversationSubmitError::unknown(format!(
                    "smeltd returned an invalid input route: {error}"
                ))
            })
        })
}

/// 新建一个终端 PTY 会话。走 smeltd 的 `open` op：会话在守护里落地（进程已
/// spawn、已存进 sessions map）发生在它回第一行 JSON 之前，所以这条请求/响应
/// 一来一回就够——不需要像交互 attach 那样占住连接进流模式，回完这行 socket
/// 直接丢掉，PTY 照样常驻（同一套"GUI 退出会话不死"的保证）。
///
/// `launch`：新建时先跑的命令（`None` = 干净的 shell）。守护只在真正新建 PTY 时用它，
/// reattach 会忽略——所以它是创建参数，不进会话目录，重试时由调用方原样再带一次。
pub fn create_remote_terminal_session(
    session: &RemoteTerminalSession,
    launch: Option<&str>,
) -> Result<(), String> {
    daemon_request(serde_json::json!({
        "op": DaemonOperation::Open,
        "id": session.id,
        "cwd": session.cwd,
        "cols": 100,
        "rows": 32,
        "initial_launch": launch,
        "remote_session": session,
    }))?;
    Ok(())
}

pub fn delete_terminal_session(id: &str) -> Result<(), String> {
    daemon_request(serde_json::json!({
        "op": DaemonOperation::Kill,
        "id": id
    }))?;
    Ok(())
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

pub fn delete_session(id: &str, kind: SessionDeleteKind) -> Result<(), String> {
    match kind {
        SessionDeleteKind::Terminal => delete_terminal_session(id),
        SessionDeleteKind::Acp => delete_acp_session(id),
    }
}

pub fn list_history(option: &AcpAgentOption, cwd: &str) -> Vec<HistorySessionSummary> {
    let Some(kind) = ConversationAgentKind::from_id(&option.kind) else {
        return Vec::new();
    };
    list_history_for(
        kind.into(),
        option.profile_id(),
        cwd,
        option.history_dir.as_deref(),
    )
}

/// 历史解析器由 `session_history` 的 Provider 注册表分发；这里仅编排可选的 ACP
/// live index，随后叠加 profile 自定义标题并转换成远程摘要模型。
fn history_sessions(
    kind: HistorySourceKind,
    profile_id: Option<&str>,
    cwd: &str,
    override_dir: Option<&str>,
) -> Vec<crate::session_history::SessionSummary> {
    let provider = crate::session_history::history_provider(kind);
    let local = provider.list_sessions(cwd, override_dir);
    let Some(acp_index) = provider.acp_session_index() else {
        return local;
    };
    // live index 要跟 agent 进程说话，没有 ACP 身份就无从谈起。
    let Some(acp_kind) = kind.acp() else {
        return local;
    };

    let Some(option) = find_agent_option_for(acp_kind, profile_id) else {
        return local;
    };
    let Ok(listed) = crate::acp_conn::list_acp_sessions(&option.launch, cwd) else {
        return local;
    };
    merge_acp_history_index(local, listed, profile_id, acp_index)
}

fn merge_acp_history_index(
    local: Vec<crate::session_history::SessionSummary>,
    listed: Vec<agent_client_protocol::schema::v1::SessionInfo>,
    profile_id: Option<&str>,
    acp_index: &crate::session_history::AcpSessionIndex,
) -> Vec<crate::session_history::SessionSummary> {
    let mut local = local
        .into_iter()
        .map(|session| (session.resume_id.clone(), session))
        .collect::<std::collections::HashMap<_, _>>();
    listed
        .into_iter()
        .map(|session| {
            let resume_id = session.session_id.to_string();
            if let Some(summary) = local.remove(&resume_id) {
                return summary;
            }
            let title = session
                .title
                .filter(|title| !title.trim().is_empty())
                .unwrap_or_else(|| resume_id.clone());
            let last_active_at = session.updated_at.as_deref().and_then(|timestamp| {
                chrono::DateTime::parse_from_rfc3339(timestamp)
                    .ok()
                    .map(|value| value.with_timezone(&chrono::Utc))
            });
            crate::session_history::SessionSummary {
                path: acp_index.path(profile_id, &resume_id),
                title: title.clone(),
                agent_title: title,
                custom_title: None,
                resume_id,
                started_at: None,
                last_active_at,
                message_count: 0,
                total_tokens: 0,
            }
        })
        .collect()
}

pub fn list_history_for(
    kind: HistorySourceKind,
    profile_id: Option<&str>,
    cwd: &str,
    override_dir: Option<&str>,
) -> Vec<HistorySessionSummary> {
    let mut sessions = history_sessions(kind, profile_id, cwd, override_dir)
        .into_iter()
        .map(|session| HistorySessionSummary {
            path: session.path,
            resume_id: session.resume_id,
            title: session.agent_title,
            custom_title: None,
            started_at: session.started_at,
            last_active_at: session.last_active_at,
            message_count: session.message_count,
            total_tokens: session.total_tokens,
        })
        .collect::<Vec<_>>();
    // 用户改过的名字在这一层贴上去，桌面端和移动端就拿到同一份展示标题。
    let mut custom_titles = crate::session_metadata::custom_titles(kind, profile_id);
    if !custom_titles.is_empty() {
        for session in &mut sessions {
            session.custom_title = custom_titles.remove(&session.resume_id);
        }
    }
    sessions.sort_by_key(|b| std::cmp::Reverse(b.last_active_at));
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_agent_conversation_rejects_a_missing_definition() {
        let error = create_agent_conversation("missing-agent").unwrap_err();
        assert!(error.contains("不存在"), "{error}");
    }

    #[test]
    fn product_conversations_stay_out_of_the_project_session_list() {
        assert!(is_product_conversation(Some("auto-1"), None, None, None));
        assert!(is_product_conversation(None, Some("agent-1"), None, None));
        assert!(is_product_conversation(
            None,
            None,
            Some("acp-automation-abc"),
            None
        ));
        assert!(!is_product_conversation(None, None, Some("acp-abc"), None));
        assert!(!is_product_conversation(None, None, None, None));
        assert!(is_background_acp_session_id("acp-automation-abc"));
        assert!(!is_background_acp_session_id("acp-abc"));
        assert!(is_agent_conversation(None, Some("agent-1"), None));
        assert!(!is_agent_conversation(
            Some("auto-1"),
            Some("agent-1"),
            None
        ));
        assert!(!is_agent_conversation(Some("auto-1"), None, None));
        assert!(!is_agent_conversation(None, None, Some("/repo")));
    }

    /// 项目里新建对话即使选了某个智能体，cwd 仍是用户仓库——那是项目会话，
    /// 不能因为绑了定义就把行从项目列表拽到「对话」栏。
    #[test]
    fn project_cwd_keeps_an_agent_backed_session_in_the_project_list() {
        assert!(!is_agent_conversation(
            None,
            Some("agent-1"),
            Some("/Users/me/dev/smelt")
        ));
        assert!(!is_product_conversation(
            None,
            Some("agent-1"),
            None,
            Some("/Users/me/dev/smelt")
        ));
    }

    /// 归属字段缺失（旧库没落盘）时，托管对话目录本身就足以认出智能体对话，
    /// 不会退化成按 cwd 成组的「项目」。智能体的 space 目录同样是 Smelt 托管
    /// 的，结构上不可能是用户项目；少了它，换成 space 当工作目录之后对话会重新
    /// 掉进「项目」分组。
    #[test]
    fn agent_space_cwd_alone_marks_an_agent_conversation() {
        let Some(root) = crate::agent_definition_store::agent_space_root("space-agent") else {
            return;
        };
        let cwd = root.to_string_lossy().to_string();
        assert!(is_agent_conversation(None, None, Some(cwd.as_str())));
        assert!(is_agent_conversation(
            None,
            Some("space-agent"),
            Some(cwd.as_str())
        ));
        assert!(!is_agent_conversation(
            None,
            None,
            Some("/Users/me/Desktop/project")
        ));
    }

    #[test]
    fn workbench_conversation_cwd_alone_marks_an_agent_conversation() {
        let Some(root) = crate::agent_definition_store::workbench_conversation_workspace_root()
        else {
            return;
        };
        let cwd = root.join("92560dd2-23d6-4bc5-8d19-a12c3fc83340");
        let cwd = cwd.to_string_lossy().to_string();
        assert!(is_agent_conversation(None, None, Some(cwd.as_str())));
        assert!(is_product_conversation(
            None,
            None,
            None,
            Some(cwd.as_str())
        ));
        // 自动化 Run 借用同一目录时仍然归自动化，不进「对话」。
        assert!(!is_agent_conversation(
            Some("auto-1"),
            None,
            Some(cwd.as_str())
        ));
    }

    #[test]
    fn conversation_submit_error_kind_survives_the_daemon_response() {
        let rejected = parse_conversation_submit_response(serde_json::json!({
            "ok": false,
            "error": {"kind": "rejected", "message": "not accepted"}
        }))
        .unwrap_err();
        assert_eq!(
            rejected.kind,
            crate::conversation::ConversationSubmitErrorKind::Rejected
        );

        let unknown = parse_conversation_submit_response(serde_json::json!({
            "ok": false,
            "error": {"kind": "unknown", "message": "connection closed"}
        }))
        .unwrap_err();
        assert_eq!(
            unknown.kind,
            crate::conversation::ConversationSubmitErrorKind::Unknown
        );
    }

    fn catalog_test_paths(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "smelt-remote-session-catalog-{name}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let database = root.join(smelt_store::DATABASE_FILE_NAME);
        (root, database.clone(), database)
    }

    fn test_remote_acp(id: &str) -> RemoteAcpSession {
        RemoteAcpSession {
            id: id.to_string(),
            cwd: "/work/project".to_string(),
            title: String::new(),
            agent_option_id: "codex".to_string(),
            agent: "codex".to_string(),
            launch: ConversationLaunchSpec::from_command("codex".to_string()),
            resume_id: None,
            created_at: 1,
            lifecycle: RemoteSessionLifecycle::Active,
            hidden: false,
        }
    }

    fn test_remote_terminal(id: &str) -> RemoteTerminalSession {
        RemoteTerminalSession {
            id: id.to_string(),
            cwd: "/work/project".to_string(),
            title: String::new(),
            created_at: 1,
            lifecycle: RemoteSessionLifecycle::Active,
        }
    }

    #[test]
    fn remote_catalog_rejects_malformed_existing_storage() {
        let (root, acp_path, terminal_path) = catalog_test_paths("malformed");
        std::fs::write(&acp_path, "{ not valid json").unwrap();

        let error = RemoteSessionCatalog::load_from_paths(Some(acp_path), Some(terminal_path))
            .expect_err("损坏目录不能被当成空目录");
        assert!(error.contains("invalid remote session catalog"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remote_catalog_rejects_cross_kind_duplicate_ids_on_load_and_upsert() {
        let (root, acp_path, terminal_path) = catalog_test_paths("duplicate-id");
        let store = crate::sqlite_state::open_sqlite_store(&acp_path).unwrap();
        store
            .put_remote_session_snapshot(&smelt_store::RemoteSessionCatalogSnapshot {
                acp: vec![acp_to_record(&test_remote_acp("shared")).unwrap()],
                terminal: vec![terminal_to_record(&test_remote_terminal("shared"))],
            })
            .unwrap();
        let error = RemoteSessionCatalog::load_from_paths(Some(acp_path), Some(terminal_path))
            .expect_err("跨 kind 重名必须作为损坏目录拒绝加载");
        assert!(error.contains("duplicate remote session id"));

        let mut catalog = RemoteSessionCatalog::in_memory();
        catalog
            .upsert_terminal(test_remote_terminal("shared"))
            .unwrap();
        let error = catalog
            .upsert_acp(test_remote_acp("shared"))
            .expect_err("写入时也必须阻止跨 kind 覆盖");
        assert!(error.contains("already belongs to a terminal"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upsert_rejects_conflicting_request_binding() {
        let mut catalog = RemoteSessionCatalog::in_memory();
        catalog.upsert_acp(test_remote_acp("acp-mobile-1")).unwrap();
        let mut conflict = test_remote_acp("acp-mobile-1");
        conflict.cwd = "/other".into();
        let error = catalog
            .upsert_acp(conflict)
            .expect_err("不同参数不能覆盖已有绑定");
        assert!(error.contains("already bound"));

        let mut same = test_remote_acp("acp-mobile-1");
        same.title = "新标题".into();
        same.lifecycle = RemoteSessionLifecycle::Creating;
        catalog.upsert_acp(same).expect("同参应幂等");
        assert_eq!(catalog.remote_acp("acp-mobile-1").unwrap().title, "新标题");

        catalog
            .upsert_terminal(test_remote_terminal("term-1"))
            .unwrap();
        let mut term_conflict = test_remote_terminal("term-1");
        term_conflict.cwd = "/other".into();
        assert!(catalog.upsert_terminal(term_conflict).is_err());
    }

    #[test]
    fn closing_records_are_visible_but_not_active() {
        let mut record = RemoteSessionRecord::from(test_remote_acp("acp-1"));
        assert!(record.is_active());
        assert!(record.is_visible());
        record.lifecycle = RemoteSessionLifecycle::Closing;
        assert!(!record.is_active());
        assert!(record.is_visible());
        record.lifecycle = RemoteSessionLifecycle::Creating;
        assert!(!record.is_visible());
        record.lifecycle = RemoteSessionLifecycle::Active;
        record.hidden = true;
        assert!(!record.is_visible());
        assert!(record.is_present());
    }

    #[test]
    fn remote_catalog_ignores_leftover_json_when_snapshot_exists() {
        let (root, acp_path, terminal_path) = catalog_test_paths("ignore-json");
        let store = crate::sqlite_state::open_sqlite_store(&acp_path).unwrap();
        store
            .put_remote_session_snapshot(&smelt_store::RemoteSessionCatalogSnapshot {
                acp: vec![smelt_store::RemoteAcpSessionRecord {
                    id: "acp-1".into(),
                    cwd: "/work/project".into(),
                    title: String::new(),
                    agent_option_id: "codex".into(),
                    agent: "codex".into(),
                    launch_command: "codex".into(),
                    launch_env_json: b"{}".to_vec(),
                    resume_id: None,
                    created_at: 1,
                    lifecycle: "active".into(),
                    hidden: false,
                }],
                terminal: Vec::new(),
            })
            .unwrap();
        std::fs::write(
            root.join("remote_terminal_sessions.json"),
            r#"{"sessions":[{"id":"term-1","cwd":"/work/project","created_at":1}]}"#,
        )
        .unwrap();

        let catalog = RemoteSessionCatalog::load_from_paths(
            Some(acp_path.clone()),
            Some(terminal_path.clone()),
        )
        .unwrap();
        assert_eq!(catalog.acp_sessions.len(), 1);
        assert!(catalog.terminal_sessions.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remote_catalog_persists_lifecycle_before_advancing_snapshot() {
        let (root, acp_path, terminal_path) = catalog_test_paths("lifecycle");
        let mut catalog =
            RemoteSessionCatalog::load_from_paths(Some(acp_path), Some(terminal_path.clone()))
                .unwrap();
        catalog
            .upsert_terminal(RemoteTerminalSession {
                id: "term-1".to_string(),
                cwd: "/work/project".to_string(),
                title: "Terminal".to_string(),
                created_at: 42,
                lifecycle: RemoteSessionLifecycle::Creating,
            })
            .unwrap();
        assert_eq!(catalog.snapshot().revision, 1);

        catalog
            .set_lifecycle("term-1", RemoteSessionLifecycle::Active)
            .unwrap();
        let snapshot = catalog.snapshot();
        assert_eq!(snapshot.revision, 2);
        assert_eq!(snapshot.sessions.len(), 1);
        assert!(snapshot.sessions[0].is_active());

        let store = crate::sqlite_state::store_beside_json(&terminal_path).unwrap();
        let persisted = store.get_remote_session_snapshot().unwrap().unwrap();
        assert_eq!(persisted.terminal.len(), 1);
        assert_eq!(persisted.terminal[0].lifecycle, "active");

        catalog.remove("term-1").unwrap();
        assert_eq!(catalog.snapshot().revision, 3);
        assert!(catalog.snapshot().sessions.is_empty());
        let persisted = store.get_remote_session_snapshot().unwrap();
        assert!(persisted.is_none() || persisted.unwrap().terminal.is_empty());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn default_agent_options_use_shared_agent_definitions() {
        let options = agent_options();
        for kind in ConversationAgentKind::ALL
            .into_iter()
            .filter(|kind| kind.is_bare_kind())
        {
            let option = options
                .iter()
                .find(|option| option.id == kind.id())
                .unwrap();
            assert_eq!(option.kind, kind.id());
            assert!(!option.launch.command.trim().is_empty());
        }
    }

    #[test]
    fn remote_opencode_launch_keeps_full_access_and_allows_explicit_override() {
        let default = configured_launch(None, ConversationAgentKind::OpenCode);
        assert_eq!(
            default
                .env
                .get("OPENCODE_CONFIG_CONTENT")
                .map(String::as_str),
            Some(r#"{"permission":"allow"}"#)
        );

        let snapshot = smelt_store::Store::agent_ui_snapshot_from_json(&serde_json::json!({
            "acp_env": {
                "opencode": {
                    "OPENCODE_CONFIG_CONTENT": "{\"permission\":\"ask\"}"
                }
            }
        }))
        .unwrap();
        let configured = configured_launch(Some(&snapshot), ConversationAgentKind::OpenCode);
        assert_eq!(
            configured
                .env
                .get("OPENCODE_CONFIG_CONTENT")
                .map(String::as_str),
            Some(r#"{"permission":"ask"}"#)
        );
    }

    /// option id 与 `session_metadata` 的 profile id 之间的换算：默认 agent 没有
    /// profile，手动加的 workspace profile 要把 `profile:` 前缀剥掉——两端重命名
    /// 落到同一条记录，全靠这一步。
    #[test]
    fn profile_id_strips_the_option_prefix() {
        let options = agent_options();
        for kind in ConversationAgentKind::ALL
            .into_iter()
            .filter(|kind| kind.is_bare_kind())
        {
            let option = options
                .iter()
                .find(|option| option.id == kind.id())
                .unwrap();
            assert_eq!(option.profile_id(), None);
        }

        let profile = AcpAgentOption {
            id: "profile:quant".to_string(),
            kind: ConversationAgentKind::Codex.id().to_string(),
            label: "Quant".to_string(),
            profile: true,
            agent_definition_id: None,
            launch: ConversationLaunchSpec::from_command("codex".to_string()),
            history_dir: None,
        };
        assert_eq!(profile.profile_id(), Some("quant"));
        assert_eq!(
            find_agent_option_for(ConversationAgentKind::Codex, None).map(|option| option.id),
            Some(ConversationAgentKind::Codex.id().to_string())
        );
    }

    #[test]
    fn the_agent_prefix_is_the_only_marker_of_a_product_conversation() {
        let option = AcpAgentOption {
            id: "agent:writer".to_string(),
            kind: ConversationAgentKind::Pi.id().to_string(),
            label: "写作助手".to_string(),
            profile: false,
            agent_definition_id: Some("writer".to_string()),
            launch: ConversationLaunchSpec::from_command("pi".to_string()),
            history_dir: None,
        };
        assert_eq!(option.agent_definition_id(), Some("writer"));
        // 智能体不是 profile：按 kind 找 profile 的调用方不能把它当账号。
        assert_eq!(option.profile_id(), None);

        assert_eq!(agent_definition_id_from_option_id("pi"), None);
        assert_eq!(agent_definition_id_from_option_id("profile:quant"), None);
        assert_eq!(agent_definition_id_from_option_id("agent:"), None);
        assert_eq!(agent_definition_id_from_option_id("agent:   "), None);
    }

    #[test]
    fn a_remote_session_carries_its_agent_ownership_in_the_option_id() {
        let mut session = RemoteAcpSession {
            id: "acp-mobile-1".to_string(),
            cwd: "/tmp".to_string(),
            title: String::new(),
            agent_option_id: "agent:writer".to_string(),
            agent: ConversationAgentKind::Pi.id().to_string(),
            launch: ConversationLaunchSpec::from_command("pi".to_string()),
            resume_id: None,
            created_at: 0,
            lifecycle: Default::default(),
            hidden: false,
        };
        assert_eq!(session.agent_definition_id(), Some("writer"));

        session.agent_option_id = ConversationAgentKind::Pi.id().to_string();
        assert_eq!(session.agent_definition_id(), None);
    }

    #[test]
    fn display_title_prefers_the_user_name() {
        let mut session = HistorySessionSummary {
            path: PathBuf::from("/tmp/session.jsonl"),
            resume_id: "session-1".to_string(),
            title: "Fix the flaky test".to_string(),
            custom_title: None,
            started_at: None,
            last_active_at: None,
            message_count: 0,
            total_tokens: 0,
        };
        assert_eq!(session.display_title(), "Fix the flaky test");
        session.custom_title = Some("夜里那次排查".to_string());
        assert_eq!(session.display_title(), "夜里那次排查");
    }

    #[test]
    fn acp_history_index_preserves_list_order_and_matching_local_metadata() {
        let local = vec![
            crate::session_history::SessionSummary {
                path: PathBuf::from("/archive/existing.jsonl"),
                title: "本地标题".to_string(),
                agent_title: "本地标题".to_string(),
                custom_title: None,
                resume_id: "existing".to_string(),
                started_at: None,
                last_active_at: None,
                message_count: 12,
                total_tokens: 34,
            },
            crate::session_history::SessionSummary {
                path: PathBuf::from("/archive/local-only.jsonl"),
                title: "仅本地".to_string(),
                agent_title: "仅本地".to_string(),
                custom_title: None,
                resume_id: "local-only".to_string(),
                started_at: None,
                last_active_at: None,
                message_count: 1,
                total_tokens: 2,
            },
        ];
        let mut indexed =
            agent_client_protocol::schema::v1::SessionInfo::new("indexed/a", "/work/project");
        indexed.title = Some("远端标题".to_string());
        indexed.updated_at = Some("2026-08-23T10:20:30Z".to_string());
        let listed = vec![
            indexed,
            agent_client_protocol::schema::v1::SessionInfo::new("existing", "/work/project"),
        ];
        let index = crate::session_history::history_provider(ConversationAgentKind::Dsh.into())
            .acp_session_index()
            .unwrap();

        let merged = merge_acp_history_index(local, listed, Some("team"), index);

        assert_eq!(
            merged
                .iter()
                .map(|session| session.resume_id.as_str())
                .collect::<Vec<_>>(),
            vec!["indexed/a", "existing"]
        );
        assert_eq!(merged[0].title, "远端标题");
        assert_eq!(
            merged[0].path.file_name().and_then(|name| name.to_str()),
            Some("indexed_a")
        );
        assert_eq!(merged[0].message_count, 0);
        assert!(merged[0].last_active_at.is_some());
        assert_eq!(merged[1].path, PathBuf::from("/archive/existing.jsonl"));
        assert_eq!(merged[1].message_count, 12);
        assert_eq!(merged[1].total_tokens, 34);
    }

    #[test]
    fn truncates_history_titles_without_breaking_unicode() {
        let input = "你".repeat(81);
        assert_eq!(
            crate::session_history::truncate_title(&input)
                .chars()
                .count(),
            81
        );
        assert!(crate::session_history::truncate_title(&input).ends_with('…'));
    }

    #[test]
    fn delete_kind_prefers_catalog_over_menu() {
        assert_eq!(
            resolve_session_delete_kind(
                Some(RemoteSessionKind::Terminal),
                Some(WorkspaceMenuSessionKind::Acp)
            )
            .unwrap(),
            SessionDeleteKind::Terminal
        );
        assert_eq!(
            resolve_session_delete_kind(None, Some(WorkspaceMenuSessionKind::Terminal)).unwrap(),
            SessionDeleteKind::Terminal
        );
        assert_eq!(
            resolve_session_delete_kind(None, Some(WorkspaceMenuSessionKind::Acp)).unwrap(),
            SessionDeleteKind::Acp
        );
        assert!(resolve_session_delete_kind(None, None).is_err());
    }

    #[test]
    fn remote_terminal_projection_rejects_stale_reattach() {
        let projected = HashSet::from(["term-1".to_string()]);
        let local = HashSet::new();
        assert!(accept_remote_terminal_projection(
            "term-1", &projected, &local
        ));

        let withdrawn = HashSet::new();
        assert!(
            !accept_remote_terminal_projection("term-1", &withdrawn, &local),
            "目录撤回后不能再把延迟 reattach 的结果插进侧栏"
        );

        let already_local = HashSet::from(["term-1".to_string()]);
        assert!(!accept_remote_terminal_projection(
            "term-1",
            &projected,
            &already_local
        ));
    }

    #[test]
    fn catalog_drops_records_without_runtime() {
        let mut catalog = RemoteSessionCatalog::in_memory();
        catalog
            .upsert_terminal(RemoteTerminalSession {
                id: "dead-active".to_string(),
                cwd: "/work".to_string(),
                title: String::new(),
                created_at: 1,
                lifecycle: RemoteSessionLifecycle::Active,
            })
            .unwrap();
        catalog
            .upsert_terminal(RemoteTerminalSession {
                id: "dead-creating".to_string(),
                cwd: "/work".to_string(),
                title: String::new(),
                created_at: 2,
                lifecycle: RemoteSessionLifecycle::Creating,
            })
            .unwrap();
        catalog
            .upsert_acp(RemoteAcpSession {
                id: "dead-failed".to_string(),
                cwd: "/work".to_string(),
                title: String::new(),
                agent_option_id: "claude".to_string(),
                agent: "claude".to_string(),
                launch: ConversationLaunchSpec::from_command("claude".to_string()),
                resume_id: None,
                created_at: 3,
                lifecycle: RemoteSessionLifecycle::Failed,
                hidden: false,
            })
            .unwrap();

        assert!(
            catalog
                .retain_live_runtimes(&HashSet::new(), &HashSet::new())
                .unwrap()
        );
        assert!(
            catalog.snapshot().sessions.is_empty(),
            "冷启动没有 runtime 时目录应清空，不要把 Failed 留下当脏数据"
        );
    }

    #[test]
    fn catalog_promotes_live_creating_and_keeps_active() {
        let mut catalog = RemoteSessionCatalog::in_memory();
        catalog
            .upsert_terminal(RemoteTerminalSession {
                id: "live-creating".to_string(),
                cwd: "/work".to_string(),
                title: String::new(),
                created_at: 1,
                lifecycle: RemoteSessionLifecycle::Creating,
            })
            .unwrap();
        catalog
            .upsert_terminal(RemoteTerminalSession {
                id: "live-active".to_string(),
                cwd: "/work".to_string(),
                title: String::new(),
                created_at: 2,
                lifecycle: RemoteSessionLifecycle::Active,
            })
            .unwrap();

        let live = HashSet::from(["live-creating".to_string(), "live-active".to_string()]);
        assert!(
            catalog
                .retain_live_runtimes(&HashSet::new(), &live)
                .unwrap()
        );
        let snapshot = catalog.snapshot();
        assert_eq!(snapshot.sessions.len(), 2);
        assert!(
            snapshot
                .sessions
                .iter()
                .all(|session| session.lifecycle == RemoteSessionLifecycle::Active)
        );
    }
}
