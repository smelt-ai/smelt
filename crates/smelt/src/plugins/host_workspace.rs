//! `smelt-workspace`：smelt 注册给全部插件的工作台 HostModule。
//!
//! 插件读得到侧栏里的智能体会话，能请宿主跳到某个会话。主目录用 `os.homedir()`；
//! 当前项目、选路径、宿主事件在 `smelt-plugin-api`（见 `smelt_plugin_api::js`）。

use std::cell::RefCell;
use std::rc::Rc;

use gpui_shell::{HostError, HostModule, HostObject, HostValue};
use smelt_model::agent_status::AgentStatus;

/// 宿主上下文：插件读得到的工作台状态。由工作台写入（纯内存），插件经
/// `smelt-workspace.sessions()` 读取。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct HostContext {
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
pub(super) fn workspace_module(context: Rc<RefCell<HostContext>>) -> HostModule {
    HostModule::new("smelt-workspace")
        .declarations(WORKSPACE_DECLARATIONS)
        .function("sessions", move |_| Ok(sessions_value(&context.borrow())))
        .function("open_session", |arguments| {
            let id = arguments.string(0)?.to_owned();
            gpui_shell::with_current_app(|cx| super::open_session(id, cx))
                .ok_or_else(|| HostError::new("open_session 只能在事件或任务里调用"))?;
            Ok(HostValue::Null)
        })
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
