//! `smelt-workspace`：smelt 注册给全部插件的工作台 HostModule。
//!
//! 插件读得到主目录、工作台当前项目、机器和项目、侧栏里的智能体会话，能等它们变化，
//! 能请宿主弹系统的目录选择框、跳到或关掉某个会话、切项目。
//!
//! 宿主不能回调脚本（gpui-shell 的宿主函数不许重入引擎），所以「订阅」做成一个异步
//! 函数：`changed(topic, since)` 在这个主题的 generation 不等于 `since` 时兑现。插件在
//! 循环里 await 它，有变化才醒，空闲时没有任何定时器。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_shell::{HostError, HostModule, HostObject, HostResult, HostValue};
use smelt_model::agent_kind::TerminalAgentKind;
use smelt_model::agent_status::AgentStatus;
use smelt_model::new_session::{
    NewSessionAction, NewSessionActionKind, NewSessionSections, NewSessionTarget,
};
use tokio::sync::oneshot;

use super::WorkspaceRequest;
use super::host_fs::path_value;

/// 宿主上下文：插件读得到的工作台状态。由工作台写入（纯内存），插件经
/// `smelt-workspace.context()` / `sessions()` 读取，经 `changed()` 等变化。
#[derive(Default)]
pub(super) struct HostContext {
    /// 工作台当前项目在哪台机器、根目录是什么。不管本机远程都有。
    pub(super) project: Option<CurrentProject>,
    /// 每次 `project` 变化加一。插件拿它判断要不要重扫，不必比较路径。
    pub(super) generation: u64,
    pub(super) sessions: Vec<SessionInfo>,
    /// 每次 `sessions` 有任何一项变化加一。
    pub(super) sessions_generation: u64,
    pub(super) machines: Vec<MachineEntry>,
    /// 每次 `machines`（含其中的项目）有任何一项变化加一。
    pub(super) machines_generation: u64,
    /// 正在 `changed()` 里等着的插件。
    waiters: Vec<Waiter>,
}

/// 工作台当前项目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CurrentProject {
    pub(crate) machine: MachineInfo,
    pub(crate) root: String,
}

/// 插件能订阅的主题。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Topic {
    /// 当前项目，对应 `context()`。
    Context,
    /// 会话列表和各会话的状态，对应 `sessions()`。
    Sessions,
    /// 机器和各机器上的项目，对应 `machines()`。
    Machines,
}

impl Topic {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "context" => Some(Self::Context),
            "sessions" => Some(Self::Sessions),
            "machines" => Some(Self::Machines),
            _ => None,
        }
    }
}

struct Waiter {
    topic: Topic,
    reply: oneshot::Sender<u64>,
}

impl HostContext {
    fn generation_of(&self, topic: Topic) -> u64 {
        match topic {
            Topic::Context => self.generation,
            Topic::Sessions => self.sessions_generation,
            Topic::Machines => self.machines_generation,
        }
    }

    /// 主题变了：叫醒等它的插件。插件视图已经卸载的，接收端早没了，发送失败就丢掉。
    pub(super) fn wake(&mut self, topic: Topic) {
        let generation = self.generation_of(topic);
        let (ready, waiting) = std::mem::take(&mut self.waiters)
            .into_iter()
            .partition::<Vec<_>, _>(|waiter| waiter.topic == topic);
        self.waiters = waiting;
        for waiter in ready {
            let _ = waiter.reply.send(generation);
        }
    }

    /// 等 `topic` 的 generation 不等于 `since`。已经不等就立刻兑现。
    fn wait(&mut self, topic: Topic, since: f64) -> oneshot::Receiver<u64> {
        let (reply, receiver) = oneshot::channel();
        let generation = self.generation_of(topic);
        if generation as f64 != since {
            let _ = reply.send(generation);
        } else {
            // 视图卸载了的插件不会再来取，顺手清掉，免得一直不变的主题攒下一串。
            self.waiters.retain(|waiter| !waiter.reply.is_closed());
            self.waiters.push(Waiter { topic, reply });
        }
        receiver
    }
}

/// 会话在哪台机器上，给插件看的那部分。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineInfo {
    /// `local`，或远程运行时的 id。
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) local: bool,
    /// 远程运行时连得上。本机总是 true。
    pub(crate) online: bool,
}

/// 侧栏里一台机器上的一个项目分组。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectInfo {
    /// 项目根目录，在这台机器上唯一。
    pub(crate) root: String,
    /// 侧栏显示名。末段重名时带上父目录。
    pub(crate) name: String,
    /// 打开过的项目。false 是没打开项目、按会话目录自成的一组。
    pub(crate) opened: bool,
    pub(crate) pinned: bool,
    /// 工作台当前项目。
    pub(crate) active: bool,
}

/// 一台机器和它的项目，项目按侧栏顺序。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineEntry {
    pub(crate) machine: MachineInfo,
    pub(crate) projects: Vec<ProjectInfo>,
}

/// 侧栏里的一个智能体会话，给插件看的那部分。和侧栏行用的是同一份事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionInfo {
    /// 守护进程里的稳定会话 id，`open_session` 认它。
    pub(crate) id: String,
    pub(crate) title: String,
    /// `conversation`：smelt 里的对话；`terminal`：终端会话。
    pub(crate) kind: &'static str,
    /// 智能体的 id，比如 `claude`、`codex`、`pi`。裸终端里没认出智能体时为 None。
    pub(crate) agent: Option<&'static str>,
    pub(crate) status: AgentStatus,
    /// 有没看过的提醒，但不需要处理（处理类的已经算进 `status` 的「要你」）。
    pub(crate) unread: bool,
    pub(crate) cwd: Option<String>,
    /// 挂在哪个项目分组下（`ProjectInfo::root`）。智能体定义的对话不进项目，为 None。
    pub(crate) project: Option<String>,
    pub(crate) machine: MachineInfo,
    /// 当前模型的显示名。只有对话有，而且要 agent 上报过。
    pub(crate) model: Option<String>,
    /// 上下文用量：(已用 token, 窗口大小)。只有对话有，而且要 agent 上报过。
    pub(crate) context: Option<(u64, u64)>,
    /// 最近一次活动（unix 秒）。
    pub(crate) updated_at: u64,
    /// 这一轮从什么时候开始（unix 秒）；没在一轮里是 0。
    pub(crate) turn_started_at: u64,
    /// 主舞台上正显示着它。
    pub(crate) viewing: bool,
}

const WORKSPACE_DECLARATIONS: &str = r#"
export interface HostContext {
  home: string;
  /** 当前项目在本机时是它的根目录，能直接读写磁盘；没有项目或项目在别的机器上时是 null。 */
  project_root: string | null;
  /** 当前项目，不管在哪台机器上。没有选项目时是 null。 */
  project: { machine: Machine; root: string } | null;
  generation: number;
}
/** 主目录和工作台当前项目。generation 在项目变化时加一。 */
export function context(): HostContext;
/** 弹出系统的目录选择框。取消时为 null。 */
export function pick_directory(prompt?: string): Promise<string | null>;
/** needs_you：等批准、等输入或出错；running：正在干活；idle：空闲、刚完成或已断开。 */
export type SessionStatus = "needs_you" | "running" | "idle";
export interface Machine {
  /** "local"，或远程运行时的 id。 */
  id: string;
  name: string;
  local: boolean;
  /** 远程运行时连得上。本机总是 true。 */
  online: boolean;
}
export interface ContextUsage { used: number; size: number }
export interface AgentSession {
  /** 稳定的会话 id，交给 open_session。 */
  id: string;
  title: string;
  /** conversation：smelt 里的对话；terminal：终端会话。 */
  kind: "conversation" | "terminal";
  /** 智能体 id，比如 "claude"、"codex"、"pi"；裸终端里没认出来时为 null。 */
  agent: string | null;
  status: SessionStatus;
  /** 有没看过的提醒，但不需要处理。需要处理的已经算进 status 的 needs_you。 */
  unread: boolean;
  cwd: string | null;
  /** 挂在哪个项目下（Project.root，同一台机器上）。不进项目的对话为 null。 */
  project: string | null;
  machine: Machine;
  /** 当前模型的显示名。只有对话有，而且要 agent 上报过。 */
  model: string | null;
  /** 上下文用量（token）。只有对话有，而且要 agent 上报过。 */
  context: ContextUsage | null;
  /** 最近一次活动，unix 秒。 */
  updated_at: number;
  /** 这一轮从什么时候开始，unix 秒；没在一轮里是 0。 */
  turn_started_at: number;
  /** 主舞台上正显示着它。 */
  viewing: boolean;
}
export interface Project {
  /** 项目根目录，在这台机器上唯一。 */
  root: string;
  /** 侧栏显示名。 */
  name: string;
  /** 打开过的项目。false 是没打开项目、按会话目录自成的一组。 */
  opened: boolean;
  pinned: boolean;
  /** 工作台当前项目。 */
  active: boolean;
}
export interface MachineProjects extends Machine { projects: Project[] }
export interface Machines { machines: MachineProjects[]; generation: number }
/** 侧栏里的机器（本机在前）和各自的项目，按侧栏顺序。只读内存。 */
export function machines(): Machines;
export interface Sessions { sessions: AgentSession[]; generation: number }
/** 工作台侧栏里的全部会话，按侧栏顺序。只读内存。generation 在任何一项变化时加一。 */
export function sessions(): Sessions;
/** context：当前项目；sessions：会话列表和各会话的状态；machines：机器和项目。 */
export type Topic = "context" | "sessions" | "machines";
/**
 * 等这个主题变化：它的 generation 不等于 since 时兑现，给出新的 generation。
 * since 传 -1 立刻拿到当前值。在循环里 await 它代替定时检查，空闲时不会醒：
 *
 *   let seen = -1;
 *   for (;;) { seen = await changed("sessions", seen); this.refresh(); cx.notify(); }
 */
export function changed(topic: Topic, since: number): Promise<number>;
/** 跳到这个会话，并把主窗口提到前面。会话已经关掉时什么也不做。 */
export function open_session(id: string): null;
/** 关掉这个会话，和点 × 一样：正在跑的先请用户确认。会话已经关掉时什么也不做。 */
export function close_session(id: string): null;
/** 把工作台当前项目切到这台机器上的这个项目。不在侧栏里时什么也不做。 */
export function select_project(machine: string, root: string): null;
export interface NewSessionOption {
  /** 交给 new_session 的标识。 */
  key: string;
  /** 不带「对话 / 终端」后缀的名字。 */
  label: string;
  kind: "conversation" | "terminal";
  /** 对话是引擎 id（claude、pi…），终端是命令对应的智能体；空白终端是 null。 */
  agent: string | null;
}
/** 分组和侧栏「+」一致：常用（用户置顶的）、终端、对话。 */
export interface NewSessionOptions {
  common: NewSessionOption[];
  terminal: NewSessionOption[];
  conversation: NewSessionOption[];
}
/**
 * 这台机器上能新建什么，和侧栏「+」同一份。远程机器会先去那台机器读一次装了什么，
 * 读不到就用上次读到的（没读过就只有空白终端）。每次要显示时现取，别缓存太久。
 */
export function new_session_options(machine: string): Promise<NewSessionOptions>;
/**
 * 在这台机器的这个目录下新建会话并切过去。root 传 "" 不带目录。
 * 选项已经不在了（设置里删了）时什么也不做。
 */
export function new_session(machine: string, root: string, key: string): null;
"#;

/// 在访达里打开用官方的 `cx.open_path`，不在这里重复提供。
pub(super) fn workspace_module(home: PathBuf, context: Rc<RefCell<HostContext>>) -> HostModule {
    let sessions = context.clone();
    let machines = context.clone();
    let waits = context.clone();
    HostModule::new("smelt-workspace")
        .declarations(WORKSPACE_DECLARATIONS)
        .function("context", move |_| {
            Ok(context_value(&home, &context.borrow()))
        })
        .function("sessions", move |_| Ok(sessions_value(&sessions.borrow())))
        .function("machines", move |_| Ok(machines_value(&machines.borrow())))
        .async_function("changed", move |arguments| {
            let name = arguments.string(0)?;
            let topic = Topic::parse(name)
                .ok_or_else(|| HostError::new(format!("没有这个主题：{name}")))?;
            let since = arguments.number(1)?;
            let receiver = waits.borrow_mut().wait(topic, since);
            Ok(async move {
                receiver
                    .await
                    .map(HostValue::from)
                    .map_err(|_| HostError::new("宿主已经关闭"))
            })
        })
        .function("open_session", |arguments| {
            let id = arguments.string(0)?.to_owned();
            send("open_session", WorkspaceRequest::OpenSession(id))
        })
        .function("close_session", |arguments| {
            let id = arguments.string(0)?.to_owned();
            send("close_session", WorkspaceRequest::CloseSession(id))
        })
        .function("select_project", |arguments| {
            let machine = arguments.string(0)?.to_owned();
            let root = arguments.string(1)?.to_owned();
            send(
                "select_project",
                WorkspaceRequest::SelectProject { machine, root },
            )
        })
        .async_function("new_session_options", |arguments| {
            let machine = arguments.string(0)?;
            let receiver = gpui_shell::with_current_app(|cx| {
                crate::workspace_plugins::new_session_options(machine, cx)
            })
            .ok_or_else(|| HostError::new("new_session_options 只能在事件或任务里调用"))?
            .ok_or_else(|| HostError::new(format!("没有这台机器：{machine}")))?;
            Ok(async move {
                receiver
                    .await
                    .map(|sections| new_session_options_value(&sections))
                    .map_err(|_| HostError::new("宿主已经关闭"))
            })
        })
        .function("new_session", |arguments| {
            let machine = arguments.string(0)?.to_owned();
            let root = arguments.string(1)?.to_owned();
            let key = arguments.string(2)?.to_owned();
            send(
                "new_session",
                WorkspaceRequest::NewSession { machine, root, key },
            )
        })
        .async_function("pick_directory", |arguments| {
            let prompt = match arguments.get(0).filter(|value| !value.is_null()) {
                Some(_) => Some(arguments.string(0)?.to_owned()),
                None => None,
            };
            pick_directory(prompt)
        })
}

/// 把操作交给工作台。工作台在这一轮更新结束后才做，所以插件这里拿不到结果。
fn send(name: &str, request: WorkspaceRequest) -> HostResult {
    gpui_shell::with_current_app(|cx| super::request(request, cx))
        .ok_or_else(|| HostError::new(format!("{name} 只能在事件或任务里调用")))?;
    Ok(HostValue::Null)
}

fn context_value(home: &Path, context: &HostContext) -> HostValue {
    HostObject::new()
        .field("home", home.to_string_lossy().into_owned())
        .field(
            "project_root",
            context
                .project
                .as_ref()
                .filter(|project| project.machine.local)
                .map(|project| project.root.clone()),
        )
        .field(
            "project",
            context.project.as_ref().map(|project| {
                HostObject::new()
                    .field("machine", machine_value(&project.machine))
                    .field("root", project.root.clone())
            }),
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
                .field("kind", session.kind)
                .field("agent", session.agent)
                .field("status", status_name(session.status))
                .field("unread", session.unread)
                .field("cwd", session.cwd.clone())
                .field("project", session.project.clone())
                .field("machine", machine_value(&session.machine))
                .field("model", session.model.clone())
                .field(
                    "context",
                    session.context.map(|(used, size)| {
                        HostObject::new().field("used", used).field("size", size)
                    }),
                )
                .field("updated_at", session.updated_at)
                .field("turn_started_at", session.turn_started_at)
                .field("viewing", session.viewing)
                .into()
        })
        .collect();
    HostObject::new()
        .field("sessions", sessions)
        .field("generation", context.sessions_generation)
        .into()
}

fn machines_value(context: &HostContext) -> HostValue {
    let machines: Vec<HostValue> = context
        .machines
        .iter()
        .map(|entry| {
            let projects: Vec<HostValue> = entry
                .projects
                .iter()
                .map(|project| {
                    HostObject::new()
                        .field("root", project.root.clone())
                        .field("name", project.name.clone())
                        .field("opened", project.opened)
                        .field("pinned", project.pinned)
                        .field("active", project.active)
                        .into()
                })
                .collect();
            machine_value(&entry.machine)
                .field("projects", projects)
                .into()
        })
        .collect();
    HostObject::new()
        .field("machines", machines)
        .field("generation", context.machines_generation)
        .into()
}

fn machine_value(machine: &MachineInfo) -> HostObject {
    HostObject::new()
        .field("id", machine.id.clone())
        .field("name", machine.name.clone())
        .field("local", machine.local)
        .field("online", machine.online)
}

fn new_session_options_value(sections: &NewSessionSections) -> HostValue {
    let options = |actions: &[NewSessionAction]| -> Vec<HostValue> {
        actions
            .iter()
            .map(|action| {
                let agent = match &action.target {
                    NewSessionTarget::Conversation { agent_kind, .. } => Some(agent_kind.clone()),
                    NewSessionTarget::Terminal { command, provider } => {
                        provider.clone().or_else(|| {
                            TerminalAgentKind::from_command_prefix(command)
                                .map(|kind| kind.id().to_owned())
                        })
                    }
                    NewSessionTarget::BlankTerminal => None,
                };
                HostObject::new()
                    .field("key", action.key.clone())
                    .field("label", action.label.clone())
                    .field(
                        "kind",
                        match action.kind {
                            NewSessionActionKind::Conversation => "conversation",
                            NewSessionActionKind::Terminal => "terminal",
                        },
                    )
                    .field("agent", agent)
                    .into()
            })
            .collect()
    };
    HostObject::new()
        .field("common", options(&sections.common))
        .field("terminal", options(&sections.terminal))
        .field("conversation", options(&sections.conversation))
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

#[cfg(test)]
mod tests {
    use super::{HostContext, Topic};

    #[test]
    fn changed_resolves_at_once_when_generation_already_differs() {
        let mut context = HostContext {
            generation: 3,
            ..HostContext::default()
        };
        let mut receiver = context.wait(Topic::Context, -1.0);
        assert_eq!(receiver.try_recv(), Ok(3));
        assert!(context.waiters.is_empty());
    }

    #[test]
    fn changed_waits_for_its_own_topic_only() {
        let mut context = HostContext::default();
        let mut on_context = context.wait(Topic::Context, 0.0);
        let mut on_sessions = context.wait(Topic::Sessions, 0.0);
        assert!(on_context.try_recv().is_err());

        context.sessions_generation += 1;
        context.wake(Topic::Sessions);
        assert_eq!(on_sessions.try_recv(), Ok(1));
        assert!(on_context.try_recv().is_err());

        context.generation += 1;
        context.wake(Topic::Context);
        assert_eq!(on_context.try_recv(), Ok(1));
        assert!(context.waiters.is_empty());
    }

    #[test]
    fn waiters_of_unloaded_plugins_are_dropped() {
        let mut context = HostContext::default();
        drop(context.wait(Topic::Context, 0.0));
        let _alive = context.wait(Topic::Context, 0.0);
        assert_eq!(context.waiters.len(), 1);
    }
}
