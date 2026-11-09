//! `smelt-workspace`：smelt 注册给全部插件的工作台 HostModule。
//!
//! 插件读得到主目录、工作台当前项目和它的 git 摘要、机器和项目、侧栏里的智能体会话、
//! 当前布局（整棵树点出来的样子、贴角的情况），
//! 能等它们变化，能请宿主弹系统的目录选择框、跳到或关掉某个会话、切项目、打开设置，
//! 能执行 smelt 的命令（含带参数的 `layout.*`）和别的插件的命令，能收到发给自己的命令。
//!
//! 宿主不能回调脚本（gpui-shell 的宿主函数不许重入引擎），所以「订阅」做成一个异步
//! 函数：`changed(topic, since)` 在这个主题的 generation 不等于 `since` 时兑现。插件在
//! 循环里 await 它，有变化才醒，空闲时没有任何定时器。命令也一样：`next_command()`
//! 在有命令发给这个插件时兑现。
//!
//! 模块每个插件各建一份（见 `plugins::plugin_policy`），命令队列是这个插件自己的。

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

use super::host_fs::path_value;
use super::{LayoutCommand, LayoutDockSide, WorkspaceRequest};
use crate::settings::SettingsSection;

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
    /// 每次重装快捷键（改键、插件重新加载）加一：`commands()` 给的列表或键可能变了。
    pub(super) commands_generation: u64,
    /// 当前布局点出来的样子，对应 `layout()`。
    pub(super) layout: LayoutSnapshot,
    /// 布局里的几块舞台和各自显示什么，对应 `layout()` 的 `stages`。换焦点、换显示的东西
    /// 不经过窗口，所以和 `layout` 分开写。
    pub(super) stages: Vec<LayoutStage>,
    /// 每次 `layout` 或 `stages` 变化加一。
    pub(super) layout_generation: u64,
    /// 当前项目的 git 摘要，对应 `git()`。
    pub(super) git: Option<GitSummary>,
    /// 每次 `git` 变化加一。
    pub(super) git_generation: u64,
    /// 正在 `changed()` 里等着的插件。
    waiters: Vec<Waiter>,
}

/// 工作台当前项目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CurrentProject {
    pub(crate) machine: MachineInfo,
    pub(crate) root: String,
}

/// 当前布局：Dock 现在的树（哪组选中哪个、放大哪组、dock 开没开），贴着窗口上沿两个角的
/// 组。贴角的是插件面板时，红黄绿按钮的让位由插件自己画。
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LayoutSnapshot {
    /// 布局 id，交给 `layout.switch`。
    pub(crate) id: String,
    /// 中间的树。装好布局之前是 None。
    pub(crate) center: Option<LayoutNodeInfo>,
    pub(crate) left: Option<LayoutDock>,
    pub(crate) right: Option<LayoutDock>,
    pub(crate) bottom: Option<LayoutDock>,
    /// 放大着的那组里的面板名。
    pub(crate) zoomed: Option<Vec<String>>,
    /// 贴着窗口左上角、右上角的那组里的面板名。
    pub(crate) top_left: Option<Vec<String>>,
    pub(crate) top_right: Option<Vec<String>>,
    /// 红黄绿按钮占的宽度；全屏、没有红黄绿按钮时是 0。
    pub(crate) lights: f32,
}

/// 布局里的一块舞台（`smelt.stage`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LayoutStage {
    pub(crate) id: String,
    /// 收哪几类：`terminal` / `conversation` / `page`。
    pub(crate) accepts: Vec<&'static str>,
    /// 有焦点：工作台的「当前」显示在它上面。
    pub(crate) focused: bool,
    /// 显示着的页面（`agents`、`automations`、`capabilities`、`desktop`）；显示会话或空着时是 None。
    pub(crate) page: Option<String>,
}

/// Dock 树里的一个节点：`row` / `column` 是拆分，`tabs` 是一组面板。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LayoutNodeInfo {
    /// `row` / `column` / `tabs`。
    pub(crate) kind: &'static str,
    /// `tabs` 里的面板，按标签的先后；拆分是空的。
    pub(crate) panels: Vec<LayoutPanelInfo>,
    /// `tabs` 选中的是第几个；拆分是 None。
    pub(crate) active: Option<usize>,
    pub(crate) children: Vec<LayoutNodeInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LayoutPanelInfo {
    /// 面板名，和布局文件里的 `panel_name` 一样。
    pub(crate) name: String,
    /// 标签上的名字。
    pub(crate) title: String,
    /// 此刻看得见。
    pub(crate) shown: bool,
}

/// 官方的一块 dock（左、右、下）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LayoutDock {
    pub(crate) open: bool,
    pub(crate) root: LayoutNodeInfo,
}

/// 当前项目根目录的 git 摘要。目录不是仓库、还没读回来时没有。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitSummary {
    /// 是哪个项目的（`context()` 的 `project.root`）。
    pub(crate) root: String,
    pub(crate) branch: String,
    pub(crate) upstream: Option<String>,
    pub(crate) ahead: u32,
    pub(crate) behind: u32,
    /// 有改动的文件数，含未跟踪的。
    pub(crate) changed: u32,
    pub(crate) stashes: u32,
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
    /// 能执行的命令和它们绑的键，对应 `commands()`。
    Commands,
    /// 当前布局，对应 `layout()`。
    Layout,
    /// 当前项目的 git 摘要，对应 `git()`。
    Git,
}

impl Topic {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "context" => Some(Self::Context),
            "sessions" => Some(Self::Sessions),
            "machines" => Some(Self::Machines),
            "commands" => Some(Self::Commands),
            "layout" => Some(Self::Layout),
            "git" => Some(Self::Git),
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
            Topic::Commands => self.commands_generation,
            Topic::Layout => self.layout_generation,
            Topic::Git => self.git_generation,
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

/// 发给一个插件、还没被它取走的命令。
///
/// 插件没在等的时候命令先排着，等它下一次 `next_command()` 再给；排太多（插件不取）
/// 就丢最早的，命令是用户按出来的，过时的不值得留。
#[derive(Default)]
pub(crate) struct CommandQueue {
    pending: std::collections::VecDeque<String>,
    waiter: Option<oneshot::Sender<String>>,
}

/// 插件不取时最多替它排几条。
const COMMAND_QUEUE_LIMIT: usize = 16;

impl CommandQueue {
    /// 发一条命令：插件正等着就直接给它，否则排队。
    pub(super) fn push(&mut self, command: String) {
        let command = match self.waiter.take() {
            Some(waiter) => match waiter.send(command) {
                Ok(()) => return,
                // 等的那个视图已经卸载了。
                Err(command) => command,
            },
            None => command,
        };
        if self.pending.len() == COMMAND_QUEUE_LIMIT {
            self.pending.pop_front();
        }
        self.pending.push_back(command);
    }

    /// 取下一条命令。排着的立刻给；没有就等。同一个插件只能有一处在等：两处都等，
    /// 命令该给谁说不清。
    fn next(&mut self) -> Result<oneshot::Receiver<String>, HostError> {
        let (reply, receiver) = oneshot::channel();
        if let Some(command) = self.pending.pop_front() {
            let _ = reply.send(command);
            return Ok(receiver);
        }
        if self
            .waiter
            .as_ref()
            .is_some_and(|waiter| !waiter.is_closed())
        {
            return Err(HostError::new(
                "已经有一处在等 next_command()：一个插件只在一个地方取命令",
            ));
        }
        self.waiter = Some(reply);
        Ok(receiver)
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
    /// 推理强度：(档位 id, 显示名)。只有对话有，而且要这家 agent 有这一项、上报过。
    pub(crate) effort: Option<(String, String)>,
    /// 最近一次活动（unix 秒）。
    pub(crate) updated_at: u64,
    /// 这一轮从什么时候开始（unix 秒）；没在一轮里是 0。
    pub(crate) turn_started_at: u64,
    /// 显示着它、此刻看得见的舞台 id。没显示、被挡着是 None。
    pub(crate) stage: Option<String>,
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
/** value 是这家 agent 自己的档位 id（比如 "high"），各家的档位不一样；name 是显示名。 */
export interface Effort { value: string; name: string }
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
  /** 推理强度。只有对话有，而且要这家 agent 有这一项（Claude、Codex、Pi）并上报过；终端会话总是 null。 */
  effort: Effort | null;
  /** 最近一次活动，unix 秒。 */
  updated_at: number;
  /** 这一轮从什么时候开始，unix 秒；没在一轮里是 0。 */
  turn_started_at: number;
  /** 显示着它、此刻看得见的舞台 id（见 layout().stages）。没显示或被挡着时为 null。 */
  stage: string | null;
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
export interface LayoutPanel {
  /** 面板名，和布局文件里的 panel_name 一样：内置的是 "smelt.stage" 这样，插件的是 "shell:<插件 id>/<面板>"。交给 run("layout.select", { panel })。 */
  name: string;
  /** 标签上的名字：内置面板的名字（「文件」「变更」…），插件面板的标题。 */
  title: string;
  /** 此刻看得见：是所在组选中的那个，没被别的组的放大挡住，也不在收起的 dock 里。 */
  shown: boolean;
}
export interface LayoutNode {
  /** row / column 是拆分，tabs 是一组面板。 */
  type: "row" | "column" | "tabs";
  /** tabs 里的面板，按标签先后；row / column 是 []。 */
  panels: LayoutPanel[];
  /** tabs 选中的是第几个；row / column 是 null。 */
  active: number | null;
  children: LayoutNode[];
}
export interface LayoutDock {
  /** 开着。用 run("layout.toggle_dock", { dock }) 开合。 */
  open: boolean;
  root: LayoutNode;
}
export interface LayoutStage {
  /** 舞台的 id，交给 run("layout.focus", { id })。运行时给的，换布局、重启会变。 */
  id: string;
  /** 收哪几类。 */
  accepts: ("terminal" | "conversation" | "page")[];
  /** 有焦点：标题、文件、变更、上一个 / 下一个会话都跟着它显示的东西。有舞台时正好一块是 true。 */
  focused: boolean;
  /** 显示着的页面；显示会话（看 sessions() 的 stage）或空着时是 null。 */
  page: "agents" | "automations" | "capabilities" | "desktop" | null;
}
export interface Layout {
  /** 布局 id，交给 run("layout.switch", { layout })。 */
  id: string;
  /** 中间的树。还没装好布局时是 null。 */
  center: LayoutNode | null;
  /** 官方的 dock 区域。布局没写的是 null。 */
  docks: { left: LayoutDock | null; right: LayoutDock | null; bottom: LayoutDock | null };
  /** 放大着的那组里的面板名。没放大是 null。 */
  zoomed: string[] | null;
  /** 贴着窗口左上角的那组里的面板名。它开头要给红黄绿按钮让出 lights 那么宽。 */
  top_left: string[] | null;
  /** 贴着窗口右上角的那组里的面板名。 */
  top_right: string[] | null;
  /** 红黄绿按钮占的宽度（像素）；全屏、没有红黄绿按钮时是 0。 */
  lights: number;
  /** 几块舞台，按树里的先后。什么东西放哪块由各块的 accepts 和用户的操作定，插件不指定。 */
  stages: LayoutStage[];
  generation: number;
}
/**
 * 当前布局：Dock 的树（哪组选中哪个、放大、dock 开合），贴角的是哪组。只读内存。
 * generation 在任何一项变化时加一。
 */
export function layout(): Layout;
export interface GitRepo {
  /** 是哪个项目的，同 context().project.root。 */
  root: string;
  /** 当前分支；分离头指针时是运行时给的写法。 */
  branch: string;
  /** 跟踪的远端分支，比如 "origin/main"；没设时是 null。 */
  upstream: string | null;
  /** 比 upstream 多几个提交（未推送）。 */
  ahead: number;
  /** 比 upstream 少几个提交。 */
  behind: number;
  /** 有改动的文件数，含未跟踪的（未提交）。 */
  changed: number;
  /** stash 里存着几份。 */
  stashes: number;
}
export interface Git { repo: GitRepo | null; generation: number }
/**
 * 当前项目根目录的 git 摘要。目录不是 git 仓库、还没读回来时 repo 是 null。
 * 文件一变 smelt 就会重读，用 changed("git", since) 等，不用自己跑 git。只读内存。
 */
export function git(): Git;
/**
 * context：当前项目；sessions：会话列表和各会话的状态；machines：机器和项目；
 * commands：命令和键；layout：当前布局；git：当前项目的 git 摘要。
 */
export type Topic = "context" | "sessions" | "machines" | "commands" | "layout" | "git";
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
/** 设置窗口的页。加机器在 remote。 */
export type SettingsPage =
  | "appearance" | "notifications" | "shortcuts" | "panel-plugins"
  | "agents" | "remote" | "resources" | "maintenance" | "about";
/**
 * 打开设置窗口。不给 page 和按 ⌘, 一样：已经开着就只提到前面，新开时停在有事要处理的那一页。
 * 给了 page 就切到那一页。
 */
export function open_settings(page?: SettingsPage): null;
export interface Command {
  /**
   * 交给 run 的 id。smelt 自己的命令是快捷键设置里那些（比如 "split_beside"）；插件的是 "<插件 id>/<命令名>"。
   */
  id: string;
  title: string;
  /**
   * 分组名，现成能显示的：smelt 自己的命令按快捷键设置页分（「全局」「会话与面板」
   * 「编辑与导航」），插件的命令是插件名。
   */
  group: string;
  /** 眼下绑的键，gpui 写法（比如 "cmd-shift-j"）；没绑是 null。 */
  key: string | null;
}
/**
 * 能执行的命令：先 smelt 自己的，再各插件声明的。只读内存。改键、插件重新加载后会变，
 * 用 changed("commands", since) 等。
 */
export function commands(): Command[];
/**
 * 执行一条命令。smelt 自己的命令和按它的快捷键一样，按当前焦点派发：比如 save_file
 * 只在焦点在文件里时有用。插件的命令交给那个插件。没有这条命令、参数不对时报错。
 *
 * 布局命令带参数，面板按面板名找（见 layout() 里各面板的 name）：
 * - layout.select { panel }：切到这个面板：选中它的标签，被放大挡住先还原，在收起的 dock 里就打开；
 * - layout.zoom { panel }：放大它所在的组，已经放大的是这组就还原；
 * - layout.add { panel, to? }：把布局里没有的面板加进来，给了 to（布局里的面板名）就进它那一组，
 *   不然在最右边另起一组；已经有了就和 layout.select 一样；
 * - layout.focus { id }：焦点换到这块舞台，它显示的变成当前（和在里面点一下一样）。空着的不换；
 * - layout.toggle_dock { dock }：开合官方的 dock 区域（"left" / "right" / "bottom"）；
 * - layout.switch { layout }：切到这套布局（布局 id 是文件名去掉 .json）；
 * - layout.reset：丢掉这套布局的存档，重新读文件；
 * - layout.copy { layout? }：把这套布局（不给是当前这套）复制到用户布局目录再切过去。
 * 面板、布局不在了在工作台里提示，这里不报错。
 */
export function run(id: "layout.select" | "layout.zoom", args: { panel: string }): null;
export function run(id: "layout.add", args: { panel: string; to?: string }): null;
export function run(id: "layout.focus", args: { id: string }): null;
export function run(id: "layout.toggle_dock", args: { dock: "left" | "right" | "bottom" }): null;
export function run(id: "layout.switch", args: { layout: string }): null;
export function run(id: "layout.copy", args?: { layout?: string }): null;
export function run(id: string): null;
/**
 * 等下一条发给这个插件的命令（smelt.json 里声明的，用户从命令面板或快捷键发出），
 * 兑现成命令名。插件没在等时命令先排着。一个插件只在一处等，通常是根视图的 init：
 *
 *   for (;;) { const name = await next_command(); if (name === "jump") this.jump(); }
 */
export function next_command(): Promise<string>;
"#;

/// 在访达里打开用官方的 `cx.open_path`，不在这里重复提供。
pub(super) fn workspace_module(
    home: PathBuf,
    context: Rc<RefCell<HostContext>>,
    commands: Rc<RefCell<CommandQueue>>,
) -> HostModule {
    let sessions = context.clone();
    let machines = context.clone();
    let layout = context.clone();
    let git = context.clone();
    let waits = context.clone();
    HostModule::new("smelt-workspace")
        .declarations(WORKSPACE_DECLARATIONS)
        .function("context", move |_| {
            Ok(context_value(&home, &context.borrow()))
        })
        .function("sessions", move |_| Ok(sessions_value(&sessions.borrow())))
        .function("machines", move |_| Ok(machines_value(&machines.borrow())))
        .function("layout", move |_| Ok(layout_value(&layout.borrow())))
        .function("git", move |_| Ok(git_value(&git.borrow())))
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
        .function("open_settings", |arguments| {
            let section = match arguments.get(0).filter(|value| !value.is_null()) {
                Some(_) => {
                    let page = arguments.string(0)?;
                    Some(
                        SettingsSection::from_key(page)
                            .ok_or_else(|| HostError::new(format!("没有这一页设置：{page}")))?,
                    )
                }
                None => None,
            };
            send("open_settings", WorkspaceRequest::OpenSettings(section))
        })
        .async_function("pick_directory", |arguments| {
            let prompt = match arguments.get(0).filter(|value| !value.is_null()) {
                Some(_) => Some(arguments.string(0)?.to_owned()),
                None => None,
            };
            pick_directory(prompt)
        })
        .function("commands", |_| {
            gpui_shell::with_current_app(|cx| commands_value(cx))
                .ok_or_else(|| HostError::new("commands 只能在事件或任务里调用"))
        })
        .function("run", |arguments| {
            let id = arguments.string(0)?;
            if let Some(shortcut) = crate::keymap::shortcut(id) {
                return send("run", WorkspaceRequest::Run(shortcut.id));
            }
            if let Some(name) = id
                .strip_prefix("layout.")
                .filter(|name| !name.contains('/'))
            {
                let args = arguments.get(1).filter(|value| !value.is_null());
                let command = layout_command(name, args)?;
                return send("run", WorkspaceRequest::Layout(command));
            }
            // 命令名不带 `/`，最后一个 `/` 前面就是插件 id。
            let invoked = id
                .rsplit_once('/')
                .is_some_and(|(plugin, command)| super::invoke(plugin, command));
            if invoked {
                Ok(HostValue::Null)
            } else {
                Err(HostError::new(format!("没有这个命令：{id}")))
            }
        })
        .async_function("next_command", move |_| {
            let receiver = commands.borrow_mut().next()?;
            Ok(async move {
                receiver
                    .await
                    .map(HostValue::from)
                    .map_err(|_| HostError::new("宿主已经关闭"))
            })
        })
}

/// `run("layout.<name>", args)` 的参数。只看形状，面板、布局在不在由工作台判断。
fn layout_command(name: &str, args: Option<&HostValue>) -> Result<LayoutCommand, HostError> {
    let field = |key: &str| -> Result<String, HostError> {
        args.and_then(|args| args.get(key))
            .and_then(HostValue::as_str)
            .map(str::to_owned)
            .ok_or_else(|| HostError::new(format!("layout.{name} 要参数 {{ {key}: string }}")))
    };
    // 可省的参数：没给、给 null 是 None，给了就要是字符串。
    let optional = |key: &str| -> Result<Option<String>, HostError> {
        match args
            .and_then(|args| args.get(key))
            .filter(|value| !value.is_null())
        {
            None => Ok(None),
            Some(_) => field(key).map(Some),
        }
    };
    if let Some(args) = args
        && args.as_object().is_none()
    {
        return Err(HostError::new(format!(
            "layout.{name} 的参数要是对象，给的是 {}",
            args.describe()
        )));
    }
    Ok(match name {
        "select" => LayoutCommand::Select {
            panel: field("panel")?,
        },
        "zoom" => LayoutCommand::Zoom {
            panel: field("panel")?,
        },
        "add" => LayoutCommand::Add {
            panel: field("panel")?,
            to: optional("to")?,
        },
        "focus" => LayoutCommand::Focus { id: field("id")? },
        "toggle_dock" => {
            let dock = field("dock")?;
            LayoutCommand::ToggleDock {
                dock: LayoutDockSide::parse(&dock).ok_or_else(|| {
                    HostError::new(format!(
                        "layout.toggle_dock 的 dock 只能是 \"left\"、\"right\"、\"bottom\"，给的是 \"{dock}\""
                    ))
                })?,
            }
        }
        "switch" => LayoutCommand::Switch {
            layout: field("layout")?,
        },
        "reset" => LayoutCommand::Reset,
        "copy" => LayoutCommand::Copy {
            layout: optional("layout")?,
        },
        _ => return Err(HostError::new(format!("没有这个命令：layout.{name}"))),
    })
}

/// 不带参数就能执行的布局命令，列进 `commands()`。带参数的（select、zoom…）要面板名，
/// 放进命令列表点了也只会报错，只写在 `run` 的说明里。
const LAYOUT_COMMANDS: [(&str, &str); 2] = [
    ("layout.reset", "重置当前布局"),
    ("layout.copy", "复制当前布局"),
];

/// smelt 自己能改键的那些在前，然后是布局命令，插件声明的在后。键是眼下真绑着的。
fn commands_value(cx: &gpui::App) -> HostValue {
    let overrides = cx.global::<crate::keymap::KeymapState>().overrides();
    let builtin = crate::keymap::SHORTCUTS.iter().map(|shortcut| {
        HostObject::new()
            .field("id", shortcut.id)
            .field("title", shortcut.label)
            .field("group", shortcut.group.label())
            .field("key", shortcut.keystroke(overrides).map(str::to_owned))
            .into()
    });
    let layout = LAYOUT_COMMANDS.iter().map(|(id, title)| {
        HostObject::new()
            .field("id", *id)
            .field("title", *title)
            .field("group", "布局")
            .field("key", None::<String>)
            .into()
    });
    let plugins = super::commands().into_iter().map(|entry| {
        let key = super::bound_key(&entry.plugin, &entry.command.name);
        HostObject::new()
            .field("id", format!("{}/{}", entry.plugin, entry.command.name))
            .field("title", entry.command.title)
            .field("group", entry.plugin_name)
            .field("key", key)
            .into()
    });
    HostValue::from(
        builtin
            .chain(layout)
            .chain(plugins)
            .collect::<Vec<HostValue>>(),
    )
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

fn layout_value(context: &HostContext) -> HostValue {
    let layout = &context.layout;
    let stages: Vec<HostValue> = context
        .stages
        .iter()
        .map(|stage| {
            let accepts: Vec<HostValue> = stage.accepts.iter().map(|&kind| kind.into()).collect();
            HostObject::new()
                .field("id", stage.id.clone())
                .field("accepts", accepts)
                .field("focused", stage.focused)
                .field("page", stage.page.clone())
                .into()
        })
        .collect();
    let dock = |dock: &Option<LayoutDock>| {
        dock.as_ref().map(|dock| {
            HostObject::new()
                .field("open", dock.open)
                .field("root", layout_node_value(&dock.root))
        })
    };
    HostObject::new()
        .field("id", layout.id.clone())
        .field("center", layout.center.as_ref().map(layout_node_value))
        .field(
            "docks",
            HostObject::new()
                .field("left", dock(&layout.left))
                .field("right", dock(&layout.right))
                .field("bottom", dock(&layout.bottom)),
        )
        .field("zoomed", layout.zoomed.clone())
        .field("top_left", layout.top_left.clone())
        .field("top_right", layout.top_right.clone())
        .field("lights", layout.lights)
        .field("stages", stages)
        .field("generation", context.layout_generation)
        .into()
}

fn layout_node_value(node: &LayoutNodeInfo) -> HostValue {
    let panels: Vec<HostValue> = node
        .panels
        .iter()
        .map(|panel| {
            HostObject::new()
                .field("name", panel.name.clone())
                .field("title", panel.title.clone())
                .field("shown", panel.shown)
                .into()
        })
        .collect();
    let children: Vec<HostValue> = node.children.iter().map(layout_node_value).collect();
    HostObject::new()
        .field("type", node.kind)
        .field("panels", panels)
        .field("active", node.active)
        .field("children", children)
        .into()
}

fn git_value(context: &HostContext) -> HostValue {
    let repo = context.git.as_ref().map(|git| {
        HostObject::new()
            .field("root", git.root.clone())
            .field("branch", git.branch.clone())
            .field("upstream", git.upstream.clone())
            .field("ahead", git.ahead)
            .field("behind", git.behind)
            .field("changed", git.changed)
            .field("stashes", git.stashes)
    });
    HostObject::new()
        .field("repo", repo)
        .field("generation", context.git_generation)
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
                .field(
                    "effort",
                    session.effort.as_ref().map(|(value, name)| {
                        HostObject::new()
                            .field("value", value.clone())
                            .field("name", name.clone())
                    }),
                )
                .field("updated_at", session.updated_at)
                .field("turn_started_at", session.turn_started_at)
                .field("stage", session.stage.clone())
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
    use super::{
        COMMAND_QUEUE_LIMIT, CommandQueue, HostContext, HostObject, HostValue, LayoutCommand,
        Topic, layout_command,
    };

    #[test]
    fn layout_commands_find_panels_by_name() {
        let args: HostValue = HostObject::new().field("panel", "smelt.git").into();
        assert!(matches!(
            layout_command("select", Some(&args)),
            Ok(LayoutCommand::Select { panel }) if panel == "smelt.git"
        ));
        assert!(matches!(
            layout_command("add", Some(&args)),
            Ok(LayoutCommand::Add { panel, to: None }) if panel == "smelt.git"
        ));
        let into: HostValue = HostObject::new()
            .field("panel", "shell:demo/topbar")
            .field("to", "smelt.files")
            .into();
        assert!(matches!(
            layout_command("add", Some(&into)),
            Ok(LayoutCommand::Add { to: Some(to), .. }) if to == "smelt.files"
        ));
        assert!(layout_command("zoom", None).is_err());
        let dock: HostValue = HostObject::new().field("dock", "bottom").into();
        assert!(matches!(
            layout_command("toggle_dock", Some(&dock)),
            Ok(LayoutCommand::ToggleDock {
                dock: LayoutDockSide::Bottom
            })
        ));
        let sideways: HostValue = HostObject::new().field("dock", "center").into();
        assert!(layout_command("toggle_dock", Some(&sideways)).is_err());
        assert!(
            layout_command("toggle", Some(&args)).is_err(),
            "开合节点没有了"
        );
    }

    #[test]
    fn layout_focus_takes_a_stage_id() {
        let args: HostValue = HostObject::new().field("id", "chat").into();
        assert!(matches!(
            layout_command("focus", Some(&args)),
            Ok(LayoutCommand::Focus { id }) if id == "chat"
        ));
        assert!(layout_command("focus", None).is_err());
    }

    #[test]
    fn commands_reach_a_waiting_plugin_at_once() {
        let mut queue = CommandQueue::default();
        let mut receiver = queue.next().unwrap();
        assert!(receiver.try_recv().is_err());
        queue.push("jump".to_string());
        assert_eq!(receiver.try_recv().as_deref(), Ok("jump"));
        assert!(queue.pending.is_empty());
    }

    #[test]
    fn commands_wait_in_order_until_taken() {
        let mut queue = CommandQueue::default();
        queue.push("a".to_string());
        queue.push("b".to_string());
        assert_eq!(queue.next().unwrap().try_recv().as_deref(), Ok("a"));
        assert_eq!(queue.next().unwrap().try_recv().as_deref(), Ok("b"));
    }

    #[test]
    fn a_plugin_that_never_takes_keeps_only_the_latest_commands() {
        let mut queue = CommandQueue::default();
        for index in 0..COMMAND_QUEUE_LIMIT + 3 {
            queue.push(index.to_string());
        }
        assert_eq!(queue.pending.len(), COMMAND_QUEUE_LIMIT);
        assert_eq!(queue.next().unwrap().try_recv().as_deref(), Ok("3"));
    }

    #[test]
    fn only_one_place_may_wait_for_commands() {
        let mut queue = CommandQueue::default();
        let _first = queue.next().unwrap();
        assert!(queue.next().is_err());
    }

    #[test]
    fn a_waiter_whose_view_is_gone_neither_blocks_nor_eats_commands() {
        let mut queue = CommandQueue::default();
        drop(queue.next().unwrap());
        queue.push("kept".to_string());
        assert_eq!(queue.next().unwrap().try_recv().as_deref(), Ok("kept"));
        drop(queue.next().unwrap());
        assert!(queue.next().is_ok(), "等的视图没了，换一处等不算两处");
    }

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
    fn reinstalling_keys_wakes_whoever_waits_for_commands() {
        let context = super::super::REGISTRY.with(|registry| registry.borrow().context.clone());
        let mut on_commands = context.borrow_mut().wait(Topic::Commands, 0.0);
        let mut on_sessions = context.borrow_mut().wait(Topic::Sessions, 0.0);

        super::super::set_keys(super::super::PluginKeys::default());
        assert_eq!(on_commands.try_recv(), Ok(1));
        assert!(on_sessions.try_recv().is_err(), "别的主题不跟着醒");
    }

    #[test]
    fn waiters_of_unloaded_plugins_are_dropped() {
        let mut context = HostContext::default();
        drop(context.wait(Topic::Context, 0.0));
        let _alive = context.wait(Topic::Context, 0.0);
        assert_eq!(context.waiters.len(), 1);
    }
}
