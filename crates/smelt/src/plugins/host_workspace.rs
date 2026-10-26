//! `smelt-workspace`：smelt 注册给全部插件的工作台 HostModule。
//!
//! 插件读得到主目录、工作台当前项目和侧栏里的智能体会话，能请宿主弹系统的目录
//! 选择框、跳到某个会话。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_shell::{HostError, HostModule, HostObject, HostResult, HostValue};
use smelt_model::agent_status::AgentStatus;

use super::host_fs::path_value;

/// 宿主上下文：插件读得到的工作台状态。由工作台写入（纯内存），插件经
/// `smelt-workspace.context()` / `sessions()` 读取。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct HostContext {
    pub(super) project_root: Option<PathBuf>,
    /// 每次 `project_root` 变化加一。插件拿它判断要不要重扫，不必比较路径。
    pub(super) generation: u64,
    pub(super) sessions: Vec<SessionInfo>,
    /// 每次 `sessions` 有任何一项变化加一。
    pub(super) sessions_generation: u64,
}

/// 侧栏里的一个智能体会话，给插件看的那部分。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionInfo {
    /// 守护进程里的稳定会话 id，`open_session` 认它。
    pub(crate) id: String,
    pub(crate) title: String,
    /// 智能体的 id，比如 `claude`、`codex`、`pi`。裸终端里没认出智能体时为 None。
    pub(crate) agent: Option<&'static str>,
    pub(crate) status: AgentStatus,
    pub(crate) cwd: Option<String>,
    /// 主舞台上正显示着它。
    pub(crate) viewing: bool,
}

const WORKSPACE_DECLARATIONS: &str = r#"
export interface HostContext { home: string; project_root: string | null; generation: number }
/** 主目录和工作台当前项目。generation 在项目变化时加一。 */
export function context(): HostContext;
/** 弹出系统的目录选择框。取消时为 null。 */
export function pick_directory(prompt?: string): Promise<string | null>;
/** needs_you：等批准、等输入或出错；running：正在干活；idle：空闲、刚完成或已断开。 */
export type SessionStatus = "needs_you" | "running" | "idle";
export interface AgentSession {
  /** 稳定的会话 id，交给 open_session。 */
  id: string;
  title: string;
  /** 智能体 id，比如 "claude"、"codex"、"pi"；裸终端里没认出来时为 null。 */
  agent: string | null;
  status: SessionStatus;
  cwd: string | null;
  /** 主舞台上正显示着它。 */
  viewing: boolean;
}
export interface Sessions { sessions: AgentSession[]; generation: number }
/** 工作台侧栏里的全部会话，按侧栏顺序。只读内存，可以频繁轮询；generation 在任何一项变化时加一。 */
export function sessions(): Sessions;
/** 跳到这个会话，并把主窗口提到前面。会话已经关掉时什么也不做。 */
export function open_session(id: string): null;
"#;

/// 在访达里打开用官方的 `cx.open_path`，不在这里重复提供。
pub(super) fn workspace_module(home: PathBuf, context: Rc<RefCell<HostContext>>) -> HostModule {
    let sessions = context.clone();
    HostModule::new("smelt-workspace")
        .declarations(WORKSPACE_DECLARATIONS)
        .function("context", move |_| {
            Ok(context_value(&home, &context.borrow()))
        })
        .function("sessions", move |_| Ok(sessions_value(&sessions.borrow())))
        .function("open_session", |arguments| {
            let id = arguments.string(0)?.to_owned();
            gpui_shell::with_current_app(|cx| super::open_session(id, cx))
                .ok_or_else(|| HostError::new("open_session 只能在事件或任务里调用"))?;
            Ok(HostValue::Null)
        })
        .async_function("pick_directory", |arguments| {
            let prompt = match arguments.get(0).filter(|value| !value.is_null()) {
                Some(_) => Some(arguments.string(0)?.to_owned()),
                None => None,
            };
            pick_directory(prompt)
        })
}

fn context_value(home: &Path, context: &HostContext) -> HostValue {
    HostObject::new()
        .field("home", home.to_string_lossy().into_owned())
        .field(
            "project_root",
            context
                .project_root
                .as_ref()
                .map(|root| root.to_string_lossy().into_owned()),
        )
        .field("generation", context.generation)
        .into()
}

fn sessions_value(context: &HostContext) -> HostValue {
    let sessions: Vec<HostValue> = context
        .sessions
        .iter()
        .map(|session| {
            HostObject::new()
                .field("id", session.id.clone())
                .field("title", session.title.clone())
                .field("agent", session.agent)
                .field("status", status_name(session.status))
                .field("cwd", session.cwd.clone())
                .field("viewing", session.viewing)
                .into()
        })
        .collect();
    HostObject::new()
        .field("sessions", sessions)
        .field("generation", context.sessions_generation)
        .into()
}

fn status_name(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::NeedsYou => "needs_you",
        AgentStatus::Running => "running",
        AgentStatus::Idle => "idle",
    }
}

/// 选目录的同步半截在主线程上打开系统选择框；等用户选完是在后台。
fn pick_directory(
    prompt: Option<String>,
) -> Result<impl std::future::Future<Output = HostResult> + Send + 'static, HostError> {
    let receiver = gpui_shell::with_current_app(|cx| {
        cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: prompt.map(Into::into),
        })
    })
    .ok_or_else(|| HostError::new("选目录只能在事件或任务里调用"))?;
    Ok(async move {
        match receiver.await {
            Ok(Ok(Some(paths))) => Ok(paths
                .into_iter()
                .next()
                .map_or(HostValue::Null, |path| path_value(&path))),
            Ok(Ok(None)) | Err(_) => Ok(HostValue::Null),
            Ok(Err(error)) => Err(HostError::new(format!("无法打开目录选择框：{error}"))),
        }
    })
}
