//! 远程客户端的会话生命周期控制方法：新建目录、新建、删除、智能体归属。
//!
//! 新建、删除和 `open` / `kill` / `acp_kill` op 走同一套内部实现。运行时起好会话之后
//! 写进工作区目录，桌面、手机都按目录看到它。这里只负责把客户端的意图解析成要起的
//! 会话：用哪个 agent、开在哪个目录、会话 id 是什么、重试是不是同一个会话。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use smelt_core::acp_conn::AcpRuntimeDiagnostics;
use smelt_core::automation_store::AutomationStore;
use smelt_core::control_api::{
    NewSessionCatalogResult, SessionAgentBindingsResult, SessionCreateKind, SessionCreateParams,
    SessionCreateResult,
};
use smelt_core::new_session::NewSessionTarget;
use smelt_core::session_control::{
    AcpAgentOption, DaemonConversation, SessionDeleteKind, directory_session_kind,
    is_background_acp_session_id,
};
use smelt_core::workspace_directory::{
    WorkspaceConversation, WorkspaceDirectory, WorkspaceTerminal,
};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode};

use super::{AcpSessions, EventHubHandle, Sessions, WorkspaceDirectoryReader};

/// 远程新建的终端先按这个尺寸起；客户端接上后按自己的视口 resize。
const REMOTE_TERMINAL_COLS: u16 = 100;
const REMOTE_TERMINAL_ROWS: u16 = 32;

/// CLI 探测要对每家跑一次 `--version`，不能每次打开新建菜单都来一遍；同一台机器上
/// 装没装 CLI 不会在几分钟内反复变化，过期后再探一次即可。
const RUNTIME_DIAGNOSTICS_TTL: Duration = Duration::from_secs(120);

/// 最近一次 CLI 探测的结果。`agent.cli.inspect` / `agent.cli.install` 每次都现探并写回
/// 这里，新建目录读缓存——设置页刚装完的 CLI 立刻出现在新建菜单里。
struct DiagnosticsCache {
    latest: Mutex<Option<(Instant, AcpRuntimeDiagnostics)>>,
}

impl DiagnosticsCache {
    const fn new() -> Self {
        Self {
            latest: Mutex::new(None),
        }
    }

    /// 没过期就用缓存，否则现探。探测时不持锁：它要起一串进程，别让并发的请求排队等锁。
    fn get_or_probe(
        &self,
        now: Instant,
        probe: impl FnOnce() -> AcpRuntimeDiagnostics,
    ) -> AcpRuntimeDiagnostics {
        if let Some((checked_at, diagnostics)) = self
            .latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            && now.saturating_duration_since(*checked_at) < RUNTIME_DIAGNOSTICS_TTL
        {
            return diagnostics.clone();
        }
        let diagnostics = probe();
        self.store(Instant::now(), diagnostics.clone());
        diagnostics
    }

    fn store(&self, checked_at: Instant, diagnostics: AcpRuntimeDiagnostics) {
        *self.latest.lock().unwrap_or_else(|e| e.into_inner()) = Some((checked_at, diagnostics));
    }
}

static RUNTIME_DIAGNOSTICS: DiagnosticsCache = DiagnosticsCache::new();

/// 现探一次 CLI 并刷新缓存。设置页要的是此刻的真实状态，不读缓存。
pub(crate) fn inspect_runtime() -> AcpRuntimeDiagnostics {
    let diagnostics = smelt_core::acp_conn::inspect_acp_runtime();
    RUNTIME_DIAGNOSTICS.store(Instant::now(), diagnostics.clone());
    diagnostics
}

pub(crate) fn new_session_catalog() -> NewSessionCatalogResult {
    let diagnostics =
        RUNTIME_DIAGNOSTICS.get_or_probe(Instant::now(), smelt_core::acp_conn::inspect_acp_runtime);
    catalog_with(&diagnostics)
}

fn catalog_with(diagnostics: &AcpRuntimeDiagnostics) -> NewSessionCatalogResult {
    let agents = smelt_core::new_session::stored_conversation_options();
    let sections = smelt_core::new_session::stored_new_session_sections(&agents, Some(diagnostics));
    NewSessionCatalogResult { agents, sections }
}

pub(crate) fn agent_bindings() -> SessionAgentBindingsResult {
    SessionAgentBindingsResult {
        bindings: smelt_core::agent_definition_store::load_session_agent_definition_ids(),
    }
}

fn error(code: ControlErrorCode, message: impl Into<String>) -> ControlError {
    ControlError::new(code, message)
}

/// 新建什么：对话（带解析好的 agent 选项）或终端（带首条命令和它在启动项里的名字）。
enum CreateTarget {
    Acp(AcpAgentOption),
    Terminal {
        launch: Option<String>,
        label: Option<String>,
    },
}

/// 规划好的新建。重试命中工作区目录里已有的那一段时是 `Existing`，不再起。
enum CreatePlan {
    Existing(String),
    Acp(DaemonConversation),
    Terminal(DaemonTerminal),
}

impl CreatePlan {
    fn session_id(&self) -> &str {
        match self {
            Self::Existing(id) => id,
            Self::Acp(conversation) => &conversation.sid,
            Self::Terminal(terminal) => &terminal.id,
        }
    }
}

/// 运行时自己开的终端：起 PTY 用的参数，和起好之后写进工作区目录的那一段对话。
struct DaemonTerminal {
    id: String,
    cwd: String,
    launch: Option<String>,
    label: Option<String>,
}

impl DaemonTerminal {
    fn directory_entry(&self) -> WorkspaceConversation {
        WorkspaceConversation {
            id: uuid::Uuid::new_v4().to_string(),
            terminals: vec![WorkspaceTerminal {
                id: Some(self.id.clone()),
                cwd: Some(self.cwd.clone()),
                launch_label: self.label.clone(),
                launch_cmd: self.launch.clone(),
                ..WorkspaceTerminal::default()
            }],
            ..WorkspaceConversation::default()
        }
    }
}

/// `launch_key` 优先：客户端只回传 key，开对话还是哪种终端、跑什么命令都由运行时
/// 自己的目录决定。没给 key 时按 `kind`，默认是对话。
fn resolve_target(params: &SessionCreateParams) -> Result<CreateTarget, ControlError> {
    if let Some(key) = non_empty(params.launch_key.as_deref()) {
        let action = smelt_core::new_session::find_stored_action(key)
            .ok_or_else(|| error(ControlErrorCode::NotFound, format!("未知的启动项：{key}")))?;
        return match action.target {
            NewSessionTarget::Conversation {
                agent_option_id, ..
            } => find_option(&agent_option_id).map(CreateTarget::Acp),
            NewSessionTarget::Terminal { command, .. } => Ok(CreateTarget::Terminal {
                launch: Some(command),
                label: Some(action.label),
            }),
            NewSessionTarget::BlankTerminal => Ok(CreateTarget::Terminal {
                launch: None,
                label: None,
            }),
        };
    }
    match params.kind.unwrap_or_default() {
        SessionCreateKind::Terminal => Ok(CreateTarget::Terminal {
            launch: None,
            label: None,
        }),
        SessionCreateKind::Acp => {
            let id = non_empty(params.agent_option_id.as_deref()).ok_or_else(|| {
                error(
                    ControlErrorCode::InvalidParams,
                    "新建对话需要 agent_option_id",
                )
            })?;
            find_option(id).map(CreateTarget::Acp)
        }
    }
}

fn find_option(id: &str) -> Result<AcpAgentOption, ControlError> {
    smelt_core::session_control::find_agent_option(id).ok_or_else(|| {
        error(
            ControlErrorCode::NotFound,
            format!("未知的 agent 选项：{id}"),
        )
    })
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// 会话 id。带 `request_id` 时由它确定，重试才会落到同一个会话；用无连字符的 UUID，
/// 和运行时、桌面随机生成的 `acp-<带连字符 UUID>` 不在一个空间里，客户端选的 id
/// 撞不上别人的会话。
fn session_id(prefix: &str, request_id: Option<&str>) -> Result<String, ControlError> {
    let Some(request_id) = non_empty(request_id) else {
        return Ok(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    };
    let request_id = uuid::Uuid::parse_str(request_id)
        .map_err(|_| error(ControlErrorCode::InvalidParams, "request_id 必须是 UUID"))?;
    Ok(format!("{prefix}-{}", request_id.simple()))
}

fn require_project<'a>(
    directory: &'a WorkspaceDirectory,
    project_root: &str,
) -> Result<&'a str, ControlError> {
    directory
        .projects
        .iter()
        .find(|root| *root == project_root)
        .map(String::as_str)
        .ok_or_else(|| {
            error(
                ControlErrorCode::FailedPrecondition,
                format!("工作区里没有这个项目：{project_root}"),
            )
        })
}

/// 对话开在哪。项目对话必须是工作区里已有的项目；智能体对话可以不带项目，开在
/// 智能体自己的 space——从「智能体」页开对话时根本没有项目可选。
fn resolve_acp_cwd(
    project_root: &str,
    option: &AcpAgentOption,
    directory: &WorkspaceDirectory,
) -> Result<String, ControlError> {
    if project_root.is_empty() {
        let definition_id = option.agent_definition_id().ok_or_else(|| {
            error(
                ControlErrorCode::InvalidParams,
                "新建对话需要 project_root（只有智能体对话可以不带项目）",
            )
        })?;
        return smelt_core::agent_definition_store::ensure_agent_space(definition_id)
            .map(|dir| dir.to_string_lossy().into_owned())
            .ok_or_else(|| error(ControlErrorCode::OperationFailed, "无法创建智能体工作区"));
    }
    require_project(directory, project_root).map(str::to_string)
}

fn already_bound(message: &str) -> ControlError {
    error(
        ControlErrorCode::Conflict,
        format!("request_id 已用于{message}"),
    )
}

/// 把请求解析成要起的会话。重试看工作区目录：同一个 `request_id` 推出来的会话 id 已经
/// 在目录里，参数也一样，就是同一次新建；`runtime_taken` 看这个 id 是否被不在目录里的
/// runtime 占着。
fn plan_create(
    params: SessionCreateParams,
    target: CreateTarget,
    directory: &WorkspaceDirectory,
    runtime_taken: impl FnOnce(&str) -> bool,
) -> Result<CreatePlan, ControlError> {
    let project_root = params.project_root.trim();
    match target {
        CreateTarget::Terminal { launch, label } => {
            let cwd = require_project(directory, project_root)?.to_string();
            let id = session_id("term", params.request_id.as_deref())?;
            if let Some(conversation) = conversation_of(directory, &id) {
                let terminal = conversation
                    .terminals
                    .iter()
                    .find(|terminal| terminal.id.as_deref() == Some(id.as_str()))
                    .ok_or_else(|| already_bound("另一种会话"))?;
                if terminal.cwd.as_deref() != Some(cwd.as_str()) {
                    return Err(already_bound("另一个项目的会话"));
                }
                return Ok(CreatePlan::Existing(id));
            }
            if runtime_taken(&id) {
                return Err(already_bound("一个本地会话"));
            }
            Ok(CreatePlan::Terminal(DaemonTerminal {
                id,
                cwd,
                launch,
                label,
            }))
        }
        CreateTarget::Acp(option) => {
            let cwd = resolve_acp_cwd(project_root, &option, directory)?;
            let resume_id = non_empty(params.resume_id.as_deref()).map(str::to_string);
            let id = session_id("acp", params.request_id.as_deref())?;
            if let Some(conversation) = conversation_of(directory, &id) {
                let acp = conversation
                    .acp
                    .as_ref()
                    .ok_or_else(|| already_bound("另一种会话"))?;
                if acp.cwd.as_deref() != Some(cwd.as_str())
                    || acp.agent != option.kind
                    || acp.profile_id.as_deref() != option.profile_id()
                    || acp.agent_definition_id.as_deref() != option.agent_definition_id()
                    || acp.history_session_id != resume_id
                {
                    return Err(already_bound("参数不同的另一次新建"));
                }
                return Ok(CreatePlan::Existing(id));
            }
            if runtime_taken(&id) {
                return Err(already_bound("一个本地会话"));
            }
            Ok(CreatePlan::Acp(DaemonConversation {
                sid: id,
                cwd,
                option,
                resume_id,
            }))
        }
    }
}

/// 目录里用着这个会话 id（ACP 会话或某一格终端）的那段对话。
fn conversation_of<'a>(
    directory: &'a WorkspaceDirectory,
    session_id: &str,
) -> Option<&'a WorkspaceConversation> {
    directory.conversations.iter().find(|conversation| {
        conversation
            .acp
            .as_ref()
            .is_some_and(|acp| acp.sid == session_id)
            || conversation
                .terminals
                .iter()
                .any(|terminal| terminal.id.as_deref() == Some(session_id))
    })
}

pub(crate) fn create_session(
    params: SessionCreateParams,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    workspace_directory: &WorkspaceDirectoryReader,
    event_hub: &EventHubHandle,
) -> Result<SessionCreateResult, ControlError> {
    let target = resolve_target(&params)?;
    let directory = workspace_directory
        .read()
        .map_err(|message| error(ControlErrorCode::TemporarilyUnavailable, message))?;
    let plan = plan_create(params, target, &directory, |id| {
        sessions.get(id).is_some() || acp_sessions.get(id).is_some()
    })?;
    let session_id = plan.session_id().to_string();
    match plan {
        CreatePlan::Existing(_) => {}
        CreatePlan::Acp(conversation) => {
            super::acp_host::open_daemon_conversation(
                &conversation,
                sessions,
                acp_sessions,
                event_hub,
            )
            .map_err(|message| error(ControlErrorCode::OperationFailed, message))?;
        }
        CreatePlan::Terminal(terminal) => {
            open_daemon_terminal(&terminal, sessions, acp_sessions, event_hub)?;
        }
    }
    Ok(SessionCreateResult { session_id })
}

/// 起一个运行时自己开的终端，起好写进工作区目录；写不进就收掉。
fn open_daemon_terminal(
    terminal: &DaemonTerminal,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    event_hub: &EventHubHandle,
) -> Result<(), ControlError> {
    super::open_terminal_runtime(
        super::TerminalOpenRequest {
            id: terminal.id.clone(),
            cols: REMOTE_TERMINAL_COLS,
            rows: REMOTE_TERMINAL_ROWS,
            cwd: Some(terminal.cwd.clone()),
            launch: terminal.launch.clone(),
            create_if_missing: true,
        },
        sessions,
        acp_sessions,
        event_hub,
        |_, _| (),
    )
    .map_err(|open_error| match open_error {
        // 守护在停机：换了守护再建就行，不是这次请求本身有错。
        super::TerminalOpenError::DaemonStopping => error(
            ControlErrorCode::TemporarilyUnavailable,
            super::DaemonStopping.to_string(),
        ),
        super::TerminalOpenError::Failed(message) => {
            error(ControlErrorCode::OperationFailed, message)
        }
    })?;
    let entered = super::workspace_directory::default_store().and_then(|store| {
        super::workspace_directory::enter_conversation(
            &store,
            terminal.directory_entry(),
            event_hub,
        )
    });
    if let Err(failure) = entered {
        let _ = super::kill_terminal_session(&terminal.id, sessions, event_hub);
        return Err(error(
            ControlErrorCode::OperationFailed,
            format!("终端没能写进工作区目录：{}", failure.message),
        ));
    }
    Ok(())
}

/// 这个 id 是终端还是对话。先看运行时自己的会话表（在跑的），再看工作区目录（眼下没在
/// 跑的）。两边都没有就是不存在，不要默认当 ACP 杀。
fn session_delete_kind(
    id: &str,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    workspace_directory: &WorkspaceDirectoryReader,
) -> Option<SessionDeleteKind> {
    if sessions.get(id).is_some() {
        return Some(SessionDeleteKind::Terminal);
    }
    if acp_sessions.get(id).is_some() {
        return Some(SessionDeleteKind::Acp);
    }
    workspace_directory
        .read()
        .ok()
        .and_then(|directory| directory_session_kind(&directory, id))
}

pub(crate) fn delete_session(
    id: &str,
    sessions: &Sessions,
    acp_sessions: &AcpSessions,
    workspace_directory: &WorkspaceDirectoryReader,
    automations: &AutomationStore,
    event_hub: &EventHubHandle,
) -> Result<(), ControlError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(error(
            ControlErrorCode::InvalidParams,
            "session_id 不能为空",
        ));
    }
    // Run 的会话归运行时管（claim / 投递 / 对账 / 回收）。从旁边删掉执行现场等于把
    // 自动化的账本抽走，所以直接拒绝，而不是删一半再让自动化发现会话没了。
    if is_background_acp_session_id(id) || automations.lock().unwrap().is_run_session(id) {
        return Err(error(
            ControlErrorCode::FailedPrecondition,
            "自动化运行的会话由运行时管理，不能删除",
        ));
    }
    let kind = session_delete_kind(id, sessions, acp_sessions, workspace_directory)
        .ok_or_else(|| error(ControlErrorCode::NotFound, format!("会话不存在：{id}")))?;
    match kind {
        SessionDeleteKind::Terminal => super::kill_terminal_session(id, sessions, event_hub),
        SessionDeleteKind::Acp => super::kill_acp_session(id, acp_sessions, event_hub),
    }
    .map_err(|message| error(ControlErrorCode::OperationFailed, message))?;
    // 工作区目录里也摘掉：列表跟着目录走，桌面重启也按目录恢复。进程已经收掉了，摘不掉
    // 只记日志，不把这次删除报成失败。
    if let Err(failure) = super::workspace_directory::default_store().and_then(|store| {
        super::workspace_directory::forget_session(&store, id, event_hub)
    }) {
        super::dlog(&format!("删掉会话 {id} 后没能从工作区目录摘掉：{}", failure.message));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use smelt_core::acp_conn::RuntimeExecutable;
    use smelt_core::agent_kind::{
        ConversationAgentKind, ConversationLaunchSpec, TerminalAgentKind,
    };
    use smelt_core::workspace_directory::{
        WorkspaceAcp, WorkspaceConversation, WorkspaceTerminal,
    };

    use super::*;

    fn directory_with_projects(roots: &[&str]) -> WorkspaceDirectory {
        WorkspaceDirectory {
            projects: roots.iter().map(|root| root.to_string()).collect(),
            ..WorkspaceDirectory::default()
        }
    }

    fn claude_option() -> AcpAgentOption {
        AcpAgentOption {
            id: "claude".into(),
            kind: "claude".into(),
            label: "Claude Code".into(),
            profile: false,
            agent_definition_id: None,
            launch: ConversationLaunchSpec::from_command("claude-agent-acp"),
            history_dir: None,
        }
    }

    fn acp_params(project_root: &str, request_id: &str) -> SessionCreateParams {
        SessionCreateParams {
            project_root: project_root.into(),
            agent_option_id: Some("claude".into()),
            request_id: Some(request_id.into()),
            ..Default::default()
        }
    }

    const REQUEST: &str = "6f1c2b8e-4d5a-4f3b-9c2d-1e0f9a8b7c6d";

    fn plan_acp(
        params: SessionCreateParams,
        directory: &WorkspaceDirectory,
    ) -> Result<CreatePlan, ControlError> {
        plan_create(params, CreateTarget::Acp(claude_option()), directory, |_| false)
    }

    fn blank_terminal() -> CreateTarget {
        CreateTarget::Terminal {
            launch: None,
            label: None,
        }
    }

    /// 把规划好的对话写进目录，就像它已经起好了。
    fn entered(directory: &WorkspaceDirectory, plan: CreatePlan) -> WorkspaceDirectory {
        let CreatePlan::Acp(conversation) = plan else {
            panic!("应规划成对话");
        };
        let mut directory = directory.clone();
        directory
            .conversations
            .push(conversation.directory_entry("conv-1".into()));
        directory
    }

    #[test]
    fn request_id_maps_retries_onto_the_same_session_id() {
        let first = session_id("acp", Some(REQUEST)).unwrap();
        let retry = session_id("acp", Some(&REQUEST.to_uppercase())).unwrap();
        assert_eq!(first, retry);
        assert_eq!(first, "acp-6f1c2b8e4d5a4f3b9c2d1e0f9a8b7c6d");
        assert!(!first.contains("mobile"));
        assert_ne!(
            session_id("term", None).unwrap(),
            session_id("term", None).unwrap()
        );
        let error = session_id("acp", Some("not-a-uuid")).unwrap_err();
        assert_eq!(error.code, ControlErrorCode::InvalidParams);
    }

    /// 同一个 request_id 的重试：工作区目录里已经有它，就是同一次新建，不再起。
    #[test]
    fn retry_with_the_same_request_reuses_the_conversation_in_the_directory() {
        let directory = directory_with_projects(&["/repo"]);
        let first = plan_acp(acp_params("/repo", REQUEST), &directory).unwrap();
        let first_id = first.session_id().to_string();
        let directory = entered(&directory, first);

        let retry = plan_acp(acp_params("/repo", REQUEST), &directory).unwrap();
        assert!(matches!(&retry, CreatePlan::Existing(id) if *id == first_id));
    }

    #[test]
    fn retry_with_different_parameters_is_a_conflict() {
        let directory = directory_with_projects(&["/repo", "/other"]);
        let first = plan_acp(acp_params("/repo", REQUEST), &directory).unwrap();
        let directory = entered(&directory, first);

        let other_project = plan_acp(acp_params("/other", REQUEST), &directory);
        assert_eq!(
            other_project.err().unwrap().code,
            ControlErrorCode::Conflict
        );

        let resumed = SessionCreateParams {
            resume_id: Some("resume-1".into()),
            ..acp_params("/repo", REQUEST)
        };
        assert_eq!(
            plan_acp(resumed, &directory).err().unwrap().code,
            ControlErrorCode::Conflict
        );

        // 同一个 request_id 换成终端：推出来的终端 id 不在目录里，但对话那条用的是
        // `acp-` 前缀，两者不撞，所以终端照常规划。
        let terminal = plan_create(
            SessionCreateParams {
                kind: Some(SessionCreateKind::Terminal),
                ..acp_params("/repo", REQUEST)
            },
            blank_terminal(),
            &directory,
            |_| false,
        );
        assert!(matches!(terminal.unwrap(), CreatePlan::Terminal(_)));
    }

    #[test]
    fn a_request_id_cannot_adopt_a_local_runtime() {
        let directory = directory_with_projects(&["/repo"]);
        let plan = plan_create(
            acp_params("/repo", REQUEST),
            CreateTarget::Acp(claude_option()),
            &directory,
            |_| true,
        );
        assert_eq!(plan.err().unwrap().code, ControlErrorCode::Conflict);
    }

    #[test]
    fn projects_outside_the_workspace_directory_are_rejected() {
        let directory = directory_with_projects(&["/repo"]);
        let acp = plan_acp(acp_params("/etc", REQUEST), &directory);
        assert_eq!(
            acp.err().unwrap().code,
            ControlErrorCode::FailedPrecondition
        );

        let terminal = plan_create(
            SessionCreateParams {
                project_root: "/etc".into(),
                ..Default::default()
            },
            blank_terminal(),
            &directory,
            |_| false,
        );
        assert_eq!(
            terminal.err().unwrap().code,
            ControlErrorCode::FailedPrecondition
        );

        // 只有智能体对话可以不带项目；普通对话不带项目是参数错误。
        let missing = plan_acp(acp_params("", REQUEST), &directory);
        assert_eq!(missing.err().unwrap().code, ControlErrorCode::InvalidParams);
    }

    /// 终端开在项目里，带着首条命令和它在启动项里的名字；进目录时这两样都记上，守护
    /// 重启后桌面按它们恢复。
    #[test]
    fn terminal_plan_uses_the_project_and_keeps_the_launch_command() {
        let directory = directory_with_projects(&["/repo"]);
        let CreatePlan::Terminal(terminal) = plan_create(
            SessionCreateParams {
                project_root: "/repo".into(),
                request_id: Some(REQUEST.into()),
                ..Default::default()
            },
            CreateTarget::Terminal {
                launch: Some("claude".into()),
                label: Some("Claude Code".into()),
            },
            &directory,
            |_| false,
        )
        .unwrap() else {
            panic!("应规划成终端");
        };
        assert_eq!(terminal.id, "term-6f1c2b8e4d5a4f3b9c2d1e0f9a8b7c6d");
        assert_eq!(terminal.cwd, "/repo");
        let entry = terminal.directory_entry();
        let entry = &entry.terminals[0];
        assert_eq!(entry.id.as_deref(), Some(terminal.id.as_str()));
        assert_eq!(entry.cwd.as_deref(), Some("/repo"));
        assert_eq!(entry.launch_cmd.as_deref(), Some("claude"));
        assert_eq!(entry.launch_label.as_deref(), Some("Claude Code"));

        // 重试：目录里已经有这个终端，就是同一次新建。
        let directory = WorkspaceDirectory {
            conversations: vec![terminal.directory_entry()],
            ..directory
        };
        let retry = plan_create(
            SessionCreateParams {
                project_root: "/repo".into(),
                request_id: Some(REQUEST.into()),
                ..Default::default()
            },
            blank_terminal(),
            &directory,
            |_| false,
        )
        .unwrap();
        assert!(matches!(retry, CreatePlan::Existing(_)));
    }

    #[test]
    fn blank_terminal_launch_key_resolves_on_the_runtime() {
        let target = resolve_target(&SessionCreateParams {
            launch_key: Some(smelt_core::new_session::BLANK_TERMINAL_KEY.into()),
            kind: Some(SessionCreateKind::Acp),
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(
            target,
            CreateTarget::Terminal {
                launch: None,
                label: None
            }
        ));

        let unknown = resolve_target(&SessionCreateParams {
            launch_key: Some("terminal:nope:rm -rf /".into()),
            ..Default::default()
        });
        assert_eq!(unknown.err().unwrap().code, ControlErrorCode::NotFound);

        let missing_option = resolve_target(&SessionCreateParams::default());
        assert_eq!(
            missing_option.err().unwrap().code,
            ControlErrorCode::InvalidParams
        );
    }

    #[test]
    fn diagnostics_are_probed_once_per_ttl() {
        let cache = DiagnosticsCache::new();
        let probes = Cell::new(0);
        let probe = || {
            probes.set(probes.get() + 1);
            AcpRuntimeDiagnostics::default()
        };
        let start = Instant::now();
        cache.get_or_probe(start, probe);
        cache.get_or_probe(start + Duration::from_secs(60), probe);
        assert_eq!(probes.get(), 1);
        cache.get_or_probe(Instant::now() + RUNTIME_DIAGNOSTICS_TTL, probe);
        assert_eq!(probes.get(), 2);
    }

    #[test]
    fn catalog_lists_agent_options_and_grouped_actions() {
        let mut diagnostics = AcpRuntimeDiagnostics::default();
        diagnostics.record_terminal(
            TerminalAgentKind::Claude,
            RuntimeExecutable {
                program: "claude".into(),
                path: Some("/usr/bin/claude".into()),
                version: Some("1.0.0".into()),
                error: None,
            },
        );
        let catalog = catalog_with(&diagnostics);
        let value = serde_json::to_value(&catalog).unwrap();

        let agents = value["agents"].as_array().unwrap();
        let claude = agents
            .iter()
            .find(|agent| agent["id"] == ConversationAgentKind::Claude.id())
            .expect("裸引擎选项应在目录里");
        assert_eq!(claude["kind"], "claude");
        assert!(claude.get("launch").is_none(), "启动规格不出线");
        for section in ["common", "terminal", "conversation"] {
            assert!(value["sections"][section].is_array(), "缺分组 {section}");
        }
        let terminal = value["sections"]["terminal"].as_array().unwrap();
        assert!(terminal.iter().any(|action| action["key"]
            == smelt_core::new_session::BLANK_TERMINAL_KEY
            && action["target"] == "blankTerminal"));
        assert!(
            terminal
                .iter()
                .any(|action| action["target"] == "terminal" && action["provider"] == "claude"),
            "探测到的 CLI 出现在终端分组"
        );

        let decoded: NewSessionCatalogResult = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.sections, catalog.sections);
        assert_eq!(decoded.agents.len(), catalog.agents.len());
    }

    fn acp_conversation(sid: &str) -> WorkspaceConversation {
        WorkspaceConversation {
            id: format!("c-{sid}"),
            acp: Some(WorkspaceAcp {
                sid: sid.into(),
                ..WorkspaceAcp::default()
            }),
            ..WorkspaceConversation::default()
        }
    }

    fn terminal_conversation(terminal_id: &str) -> WorkspaceConversation {
        WorkspaceConversation {
            id: format!("c-{terminal_id}"),
            terminals: vec![WorkspaceTerminal {
                id: Some(terminal_id.into()),
                ..WorkspaceTerminal::default()
            }],
            ..WorkspaceConversation::default()
        }
    }

    #[test]
    fn delete_kind_comes_from_the_runtime_then_the_directory() {
        let sessions = super::super::new_sessions();
        let acp_sessions = super::super::new_test_acp_sessions();
        acp_sessions.reserve_with("acp-live", || {
            super::super::make_acp_session("acp-live", None, None, None)
        });
        let workspace_directory = WorkspaceDirectoryReader::fixed(WorkspaceDirectory {
            conversations: vec![
                terminal_conversation("acp-live"),
                acp_conversation("dormant"),
                terminal_conversation("dormant-term"),
            ],
            ..WorkspaceDirectory::default()
        });
        let kind = |id| {
            session_delete_kind(
                id,
                &sessions,
                &acp_sessions,
                &workspace_directory,
            )
        };

        assert_eq!(
            kind("acp-live"),
            Some(SessionDeleteKind::Acp),
            "在跑的 runtime 说了算"
        );
        assert_eq!(kind("dormant"), Some(SessionDeleteKind::Acp));
        assert_eq!(kind("dormant-term"), Some(SessionDeleteKind::Terminal));
        assert_eq!(kind("missing"), None);
    }

    fn delete(
        id: &str,
        acp_sessions: &AcpSessions,
        automations: &AutomationStore,
    ) -> Result<(), ControlError> {
        delete_session(
            id,
            &super::super::new_sessions(),
            acp_sessions,
            &super::super::new_test_workspace_directory(),
            automations,
            &super::super::new_event_hub(),
        )
    }

    #[test]
    fn delete_removes_the_runtime_it_resolved() {
        let acp_sessions = super::super::new_test_acp_sessions();
        let automations = super::super::new_test_automation_store();
        acp_sessions.reserve_with("acp-live", || {
            super::super::make_acp_session("acp-live", None, None, None)
        });

        delete("acp-live", &acp_sessions, &automations).unwrap();
        assert!(acp_sessions.get("acp-live").is_none());

        let missing = delete("nope", &acp_sessions, &automations);
        assert_eq!(missing.err().unwrap().code, ControlErrorCode::NotFound);
    }

    #[test]
    fn automation_run_sessions_cannot_be_deleted() {
        use smelt_core::automation::{
            Automation, AutomationAction, AutomationCommand, AutomationSchedule, AutomationTrigger,
        };

        let acp_sessions = super::super::new_test_acp_sessions();
        let automations = super::super::new_test_automation_store();
        {
            let mut owner = automations.lock().unwrap();
            owner
                .apply(
                    AutomationCommand::Upsert {
                        automation: Box::new(Automation {
                            id: "automation-1".into(),
                            name: "Morning report".into(),
                            enabled: true,
                            workspace_dir: Some(
                                std::env::temp_dir().to_string_lossy().into_owned(),
                            ),
                            trigger: AutomationTrigger::schedule(
                                AutomationSchedule::EveryMinutes { minutes: 30 },
                            ),
                            action: AutomationAction::Agent {
                                agent_definition_id: "agent-1".into(),
                                prompt: Some("Summarize".into()),
                            },
                            sinks: Vec::new(),
                        }),
                    },
                    chrono::Utc::now(),
                )
                .unwrap();
            let execution = smelt_core::agent_definition_store::AgentExecutionDefinition {
                definition: smelt_core::agent_definition::AgentDefinition {
                    id: "agent-1".into(),
                    name: "Reporter".into(),
                    engine_kind_id: ConversationAgentKind::Pi.id().into(),
                    ..Default::default()
                },
                kind: ConversationAgentKind::Pi,
                launch: ConversationAgentKind::Pi.default_launch(),
            };
            let run = owner
                .run_once_with_execution("automation-1".into(), chrono::Utc::now(), &execution)
                .unwrap()
                .result
                .run()
                .unwrap();
            owner
                .bind_run_session(&run.id, "acp-run-bound".into(), 0)
                .unwrap();
        }
        acp_sessions.reserve_with("acp-run-bound", || {
            super::super::make_acp_session("acp-run-bound", None, None, None)
        });

        for id in ["acp-run-bound", "acp-automation-legacy"] {
            let error = delete(id, &acp_sessions, &automations).unwrap_err();
            assert_eq!(error.code, ControlErrorCode::FailedPrecondition, "{id}");
        }
        assert!(
            acp_sessions.get("acp-run-bound").is_some(),
            "Run 的会话没被动"
        );
    }
}
