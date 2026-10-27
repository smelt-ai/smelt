//! ACP（Agent Client Protocol）连接层：JSON-RPC over stdio 驱动跟子进程 agent
//! 的连接，不含任何 GPUI——本来就是 `smelt` crate 里 acp.rs 的原文搬过来的，
//! 移动的唯一理由是给 smeltd 托管 ACP 会话铺路（这个 crate 本来就是
//! GUI/守护共用层，smeltd 加个 `agent-client-protocol` 依赖就能直接复用这里
//! 的连接驱动逻辑，不用重写一遍）。
//!
//! 职责边界（原 acp.rs 的约定继续有效）：
//! - 每个 ACP 会话一条专用 OS 线程 `smol::block_on` 驱动整个连接（spawn 子进程、
//!   JSON-RPC over stdio、事件翻译）；
//! - 一般失败（找不到命令 / 握手失败 / 子进程退出）以 `ConversationEvent::Fatal` 从事件
//!   通道出来；`session/load` 的失败以 `ConversationRestoreFailure` 分类后通过
//!   `ConversationEvent::RestoreFailed` 传出，让守护端统一决定是重试、报错还是对空白会话
//!   安全地开新会话。`spawn_acp` 本身永不阻塞、永不 panic 调用方。

pub use smelt_model::acp_conn::*;

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use std::collections::{BTreeMap, BTreeSet};

use futures::{AsyncBufReadExt, AsyncWriteExt, StreamExt};

use agent_client_protocol::schema::v1::{
    AgentRequest, BooleanConfigOptionCapabilities, CancelNotification, ClientCapabilities,
    ClientSessionCapabilities, ContentBlock, CreateElicitationRequest, CreateElicitationResponse,
    CreateTerminalRequest, CreateTerminalResponse, ElicitationAcceptAction, ElicitationAction,
    ElicitationCapabilities, ElicitationContentValue, ElicitationFormCapabilities, ElicitationMode,
    ElicitationPropertySchema, ElicitationSchema, ElicitationUrlCapabilities,
    FileSystemCapabilities, ImageContent, Implementation, InitializeRequest, KillTerminalRequest,
    KillTerminalResponse, LoadSessionRequest, MultiSelectItems, NewSessionRequest, Plan,
    PlanCapabilities, PlanEntry, PlanEntryPriority, PlanEntryStatus, PlanUpdate, PlanUpdateContent,
    PromptRequest, PromptResponse, ReadTextFileRequest, ReadTextFileResponse,
    ReleaseTerminalRequest, ReleaseTerminalResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionConfigId, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigOptionValue, SessionConfigOptionsCapabilities, SessionConfigSelectOption,
    SessionConfigSelectOptions, SessionConfigValueId, SessionId, SessionModeState,
    SessionNotification, SessionUpdate, SetSessionConfigOptionRequest, StopReason,
    TerminalOutputRequest, TerminalOutputResponse, ToolCall, ToolCallId, ToolCallUpdate,
    WaitForTerminalExitRequest, WaitForTerminalExitResponse, WriteTextFileRequest,
    WriteTextFileResponse,
};
use agent_client_protocol::schema::{MaybeUndefined, ProtocolVersion};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{
    AcpAgent, AcpAgentConfig, ActiveSession, Agent, Client, ConnectionTo, Lines, Responder,
    SessionMessage,
};

pub use crate::acp_chat::AcpImage as PromptImage;
use crate::agent_kind::{
    ConversationAgentKind, ConversationLaunchSpec, SMELT_PI_AGENT_COMMAND, TerminalAgentKind,
};

/// 一次 ACP 会话的启动参数。
pub struct ConversationLaunch {
    /// 启动规格：命令字符串仍是现有的空白分词语义，环境变量单独结构化存储；
    /// legacy `VAR=value cmd` 前缀仍兼容，见 `build_agent`。
    pub launch: ConversationLaunchSpec,
    /// 仅当前进程可见的额外环境变量。controller 的远端 agent 配置可能包含凭据，
    /// 所以它不属于可持久化的 `ConversationLaunchSpec`。
    pub ephemeral_env: BTreeMap<String, String>,
    /// 会话工作目录（newSession 的 cwd）；None 用进程当前目录。
    pub cwd: Option<String>,
    /// GUI 侧会话 id，约定 `acp-` 前缀——DaemonStates 全局 map 里靠这个前缀
    /// 与 smeltd 会话共存（见 main.rs 状态转发循环的 retain）。
    pub sid: String,
    /// daemon 为该 Smelt 会话生成的能力令牌，会话里的 agent 进程凭它向守护证明身份。
    pub agent_token: String,
    /// 上一次连接的 agent 侧 session id：有就用 `session/load` 让 agent 重放
    /// 完整历史，重建 smeltd 的运行时消息投影。若 agent 明确报告历史不存在，
    /// 恢复 supervisor 只会对空白本地投影降级为新会话。
    pub resume_session_id: Option<SessionId>,
    /// Pi 原生 `--fork`：把源 session 复制成一份新文件再打开。不能和
    /// `resume_session_id` 同时用——`--session` 会接到同一份文件上。
    pub fork_session_id: Option<SessionId>,
    /// 分叉副本的切点：整份拷贝打开后、重放历史前，把活动分支切到这条
    /// 用户消息**之前**（不含它）。用于「从第 i 条回答分叉」——切点取该回答
    /// 之后的下一条用户消息，切完新会话恰好包含到该回答为止。`text` +
    /// `occurrence` 与 `ConversationCommand::Rewind` 同语义：在 agent 的可分叉
    /// 列表里按文本配对，`occurrence` 是同文本消息中的第几条（从 0 数起）。
    pub fork_cut: Option<AcpForkCut>,
}

/// 回退到一条用户消息之前以后，这条消息怎么办。
#[derive(Clone, Debug)]
pub enum AfterRewind {
    /// 编辑并重发：原文回输入框供修改。
    RestoreComposer,
    /// 重试：原样再发一次。截断已经去掉了这条消息和失败的那一轮，再发一次后对话里和
    /// agent 的上下文里都只有一条，不会像把它当新消息重发那样叠出两条。
    Resend {
        text: String,
        images: Vec<PromptImage>,
    },
}

/// 随 prompt 一起发出去的一张图（剪贴板粘进来的截图等）。
///
/// 协议要的就是 base64 + mime，所以在进这条通道前就编码好——连接线程不碰
/// GPUI 的图片类型，`acp.rs 不许引 gpui` 那条底线在这里同样成立。
/// UI → 连接线程的指令。
pub enum ConversationCommand {
    /// 发一轮 prompt。调用方负责确保同一 session 不并发发送多个 turn。
    /// `images` 空 = 纯文本那条老路径。
    Prompt {
        text: String,
        images: Vec<PromptImage>,
    },
    /// 把一条消息插进**正在跑**的 turn。只有 `ConversationHandle::supports_mid_turn_input`
    /// 为真的驱动才会收到它；调用方不得用它开新回合。
    Steer {
        text: String,
        images: Vec<PromptImage>,
    },
    /// 等当前回合完全结束后再送。只有 `supports_native_queue` 的驱动会处理。
    FollowUp {
        text: String,
        images: Vec<PromptImage>,
    },
    /// 手动压缩上下文。只有 `supports_compaction` 的驱动会处理。
    Compact { custom_instructions: Option<String> },
    /// 清空 provider 侧 steering / follow-up 队列，并把原文还回输入框。
    ClearQueue,
    /// 回到最后一条用户消息之前：agent 把活动分支切到该消息之前（Pi 的
    /// `fork(entryId)`，进程内切换、同一条连接），之后按 `then` 处理这条消息。
    /// `text` + `occurrence` 用来在 agent 的可分叉消息列表里定位 entryId（事件流
    /// 不带 entryId，只能按文本配对；`occurrence` 是同文本消息中的第几条，从 0
    /// 数起）。`truncate_from` 是发起方投影里的截断游标（entry 下标），driver 不
    /// 解释，fork 成功后连同 `then` 原样放进 `Rewound` 事件。
    /// 只有 `supports_rewind` 的驱动会收到它。
    Rewind {
        text: String,
        occurrence: usize,
        truncate_from: usize,
        then: AfterRewind,
    },
    /// 连接线程内部使用：一轮 prompt 的 JSON-RPC response 已经回来。
    ///
    /// `Ok` 带 agent 给的 StopReason；`Err` 是 agent 明确回的错误响应——那是
    /// **本轮**失败，不是连接失败，收尾后循环继续跑（会话不能死）。
    ///
    /// 走 cmd 通道而不是在回调里直接发事件，是为了让收尾发生在连接循环里：
    /// 循环拿到它时会先把 update 队列里已到的流式更新排空，保证「正文先到、
    /// 回合结束后到」的顺序。
    PromptSettled(Result<StopReason, String>),
    /// 取消当前 turn（session/cancel 通知）。支持原生队列的驱动会先 `clear_queue`
    /// 再 abort，避免队列在 abort 之后继续跑。
    Cancel,
    /// 更新一项会话配置：配置和值都由 agent 的 `config_options` 上报，不能猜。
    SetConfigOption {
        config_id: String,
        value: ConfigValue,
    },
    /// 重新拉取模型目录。
    ///
    /// 模型目录是会话启动时问 agent 要的一份快照（Pi 走 `get_available_models`）。
    /// 用户在应用里登录了新 provider、改了模型配置之后，这份快照就过期了，但会话
    /// 不会自己知道——所以由 UI 侧订阅账户状态变化后发这条命令。不支持的驱动静默
    /// 忽略：它们的模型列表本来就不随本机配置变化。
    RefreshModels,
    /// 改会话名。用户在侧栏重命名后同步给 agent，让 agent 侧的会话档也叫这个名字。
    /// 不是所有 agent 都支持，不支持的静默忽略——重命名对本地侧栏始终生效。
    SetSessionTitle(String),
    /// 用户在界面上停一个后台任务。只有会话宿主管着后台任务的驱动（Pi）会处理。
    StopBackgroundTask { id: String },
    /// 用户在界面上停一个 Pi 子智能体。只有 Pi 驱动会处理。
    StopSubagent { id: String },
    /// 主 daemon 交来的别的 Pi 对话的消息，交给会话宿主排队写进会话。只有 Pi 驱动会处理。
    DeliverPeerMessage(crate::acp_session::PeerDelivery),
    /// 关闭会话：退出连接循环，随连接 drop 杀掉子进程。
    Shutdown,
}

/// `session/load` 的恢复失败。这里把 agent 的协议错误转换成稳定的语义类型，
/// 上层不需要继续依赖某个 adapter 的错误文案。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConversationRestoreFailure {
    /// provider 已明确确认目标历史不存在。
    HistoryMissing,
    /// agent 没有声明或实现 `session/load`。
    UnsupportedLoad,
    /// 其他恢复失败，通常可以重试。
    Failed(String),
}

impl ConversationRestoreFailure {
    /// 只有 agent 明确说明「没有可加载的历史」时，空白本地投影才可以
    /// 安全地降级成新会话。超时/网络/其他协议错误必须保留为失败，不能
    /// 静默丢掉用户原本想恢复的身份。
    pub fn allows_fresh_session(&self) -> bool {
        matches!(self, Self::HistoryMissing | Self::UnsupportedLoad)
    }
}

impl std::fmt::Display for ConversationRestoreFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HistoryMissing => write!(f, "旧会话记录不存在，无法恢复"),
            Self::UnsupportedLoad => write!(f, "agent 不支持 session/load，无法恢复历史对话"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

const ACP_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

async fn wait_for_acp_request<T, F>(
    request: F,
    method: &'static str,
) -> Result<T, agent_client_protocol::Error>
where
    F: Future<Output = Result<T, agent_client_protocol::Error>>,
{
    smol::future::race(request, async move {
        smol::Timer::after(ACP_HANDSHAKE_TIMEOUT).await;
        Err(agent_client_protocol::Error::internal_error().data(format!(
            "ACP {method} 超时（{} 秒）",
            ACP_HANDSHAKE_TIMEOUT.as_secs()
        )))
    })
    .await
}

/// 连接线程 → UI 的事件。schema 类型（ToolCall 等）原样透传，不造平行模型。
pub enum ConversationEvent {
    /// 启动阶段的进度文案（下载运行时 / 拉取适配器等），Starting 横幅显示。
    Status(String),
    /// 用户该看到的一次性提示（插话、压缩、停后台任务失败等）。显示在输入框上方，
    /// 下一回合开始时清掉；不能走 `Status`，那条只在启动横幅里显示。
    Notice(String),
    /// 冷恢复即将由 agent 重放完整历史。必须先清空 daemon 中的旧投影；Ready
    /// 到达时历史可能已经同步回放完，不能再在那里清空。
    HistoryReplayStarted,
    /// 重放确实结束：草稿到齐，可以一次性换上。pi 在全量重放完 `get_messages`
    /// 后发；ACP 的 `session/load` 在响应返回、并把会话队列里响应前的重放全部
    /// 排空之后发（ACP 规范要求重放先于响应）。没有这个信号时，`replaying_history`
    /// 会一直挂着 true，把「是否有活跃回合」、「是否在执行工具」和 GUI 的列表
    /// 高度提示全部拖成失真。
    HistoryReplayFinished,
    /// `session/load` 失败。smeltd 的恢复 supervisor 根据失败类型和本地投影
    /// 决定是否重试、报错或降级为 `session/new`。
    RestoreFailed(ConversationRestoreFailure),
    /// 握手完成，可以发 prompt 了。`kind` 说明这是怎么接上的——布尔的
    /// 「resumed 与否」表达不了三种情况，会让「续接成功」被渲染成「新会话」。
    Ready {
        session_id: SessionId,
        kind: ReadyKind,
        /// agent 是否收图（`promptCapabilities.image`）。Grok 是 false——UI 据此
        /// 拦下粘贴，别让图片进了 prompt 被静默丢弃。
        supports_image: bool,
    },
    /// assistant 正文 / 思考块的流式增量（content 已文本化）。
    AgentChunk {
        thought: bool,
        text: String,
        parent_id: Option<String>,
    },
    /// client 侧 ACP 终端的输出快照。`terminal/create` 之后由读线程推过来。
    TerminalOutput {
        terminal_id: String,
        output: String,
        truncated: bool,
        exit_code: Option<i32>,
        signal: Option<String>,
    },
    ToolCall(ToolCall),
    ToolCallUpdate(ToolCallUpdate),
    /// Provider 明确暴露的工具原始元数据。与展示生命周期分开，避免从 title 反推。
    ToolDebug {
        id: String,
        name: Option<String>,
        raw_input: Option<serde_json::Value>,
    },
    /// 受管运行时明确上报的本轮 system prompt 与可用工具定义。
    RuntimeDebug(Box<crate::acp_session::RuntimeDebug>),
    /// Provider-neutral tool lifecycle used by native drivers such as Codex app-server.
    ToolStarted {
        id: String,
        title: String,
        kind: crate::acp_chat::ToolKind,
    },
    ToolOutputDelta {
        id: String,
        delta: String,
    },
    ToolFinished {
        id: String,
        status: crate::acp_chat::ToolCallStatus,
        output: Vec<crate::acp_chat::ToolOutputPart>,
    },
    /// 用完整子轨迹替换某条工具的 `children`（ACP agent 的子代理）。
    ToolChildren {
        id: String,
        children: Vec<crate::acp_chat::AcpEntry>,
        debug: BTreeMap<String, crate::acp_session::ToolCallDebug>,
    },
    /// 这次 Pi `session_send` 发给了谁、结果如何。工具开始时只有参数，结束后补上门卫的回答。
    PeerSend {
        id: String,
        send: crate::acp_chat::PeerSend,
    },
    /// 这次 Pi `subagent` 调用派出的子智能体（工具结果里的 `runs`），全量替换。
    SubagentRuns {
        id: String,
        runs: Vec<crate::acp_chat::SubagentRun>,
    },
    /// agent 的任务计划（步骤清单 + 三态进度）：每次全量覆盖，回合态不落盘。
    /// UI 渲染成消息流上方的可折叠 PLAN 条。
    Plan(Plan),
    /// Pi 后台任务的全量快照。输出是尾部，不随每个字节落盘。
    BackgroundTasks(Vec<crate::acp_session::BackgroundTaskView>),
    /// Pi 子智能体的全量快照（本次会话宿主生命期内的）。
    Subagents(Vec<crate::acp_session::SubagentView>),
    /// 别的 Pi 对话发来、宿主还记得的消息，全量快照。
    PeerInbox(Vec<crate::acp_session::PeerMessageView>),
    /// 后台任务结束。归约写成状态行，不开回合，也不当成用户说的话。
    BackgroundNotice(crate::acp_chat::TaskNote),
    /// 模型状态：当前名 + 可选列表。来自会话配置项里 category=Model 的那条
    /// select；建会话时给一次，切换或 agent 侧改动时通过 ConfigOptionUpdate 再给。
    /// 取不到就一直是 None，UI 不假装知道。
    Model(ModelState),
    /// 除模型外的可选会话配置（权限模式、协作方式、推理强度、快速模式等）。
    /// agent 不上报就为空，各 agent 共用这条 ACP 标准路径。
    ConfigOptions(Vec<SessionConfigState>),
    /// Agent 通过通用 ACP `session_info_update` 推送的会话标题。
    /// `None` 表示明确清空；未携带 title 的 update 不产生这个事件。
    SessionTitle(Option<String>),
    /// agent 请求权限：UI 渲染按钮，凭 responder 直接回 RPC。
    Permission {
        /// 请求摘要（tool call 标题，没有就用工具 id）。
        question: String,
        /// 关联的工具调用 id：UI 靠它把审批按钮内嵌进对应工具卡片，
        /// 消息流里找不到该卡片时退回独立卡片渲染。
        tool_call_id: ToolCallId,
        pub_options: Vec<crate::acp_session::PermissionOptionView>,
        responder: PermissionResponder,
        details: crate::acp_session::ApprovalDetailsView,
        /// 这条请求的原始 JSON-RPC 行文本（`with_debug` 的 `Stdout` 方向捕获的
        /// 最近一行）。审批卡挂起期间保留，供活体状态对照；GUI 直连路径不用。
        raw_request_line: Option<String>,
    },
    /// 用户消息的回显：`session/load` 重放历史时，agent 会把旧的用户提问也
    /// 当一条更新发回来（这是 entries 里 User 记录在 replay 场景下唯一的来源，
    /// 我们没有替它们手动 push 过）。正常 live 对话是否也会收到这个事件目前
    /// 没有把握确认，UI 侧用「等回声」状态机兼容两种可能，见 acp_view.rs。
    UserChunk(String),
    /// 用户消息里的图片块。会话恢复时 agent 会逐块重放，不能降级成 `[图片]`，
    /// 否则桌面端没有可渲染的数据。
    UserImage(PromptImage),
    /// 会话当前可用的斜杠命令（`/compact` 这类，不是「工具」）：(名字, 说明)。
    /// 以前只存数量——一个光秃秃的「47 条命令」既点不开也没法用，等于没有。
    AvailableCommands(Vec<(String, String)>),
    /// 上下文用量：已用 / 窗口大小（token），外加本轮缓存读取量（agent 给才有）。
    /// UI 据此显示「上下文 32%」这类指示。
    Usage {
        used: u64,
        size: u64,
        cached_read: Option<u64>,
        cost: Option<f64>,
        breakdown: Option<ContextUsageBreakdown>,
    },
    /// 握手后声明宿主可调用的会话控制。缺省为全关，旧驱动不发这条。
    SessionControls {
        compaction: bool,
        native_queue: bool,
        /// 回退到历史消息重发（Pi 的 fork）。旧驱动的事件没有这个字段——
        /// 事件是进程内的，不存在旧数据，但为了语义完整仍随声明一起下发。
        rewind: bool,
    },
    /// 上下文压缩进度。`running` 为真时 UI 显示「压缩中」；结束时 `detail`
    /// 是结果摘要，可选带压缩后的用量。
    Compaction {
        running: bool,
        detail: String,
        used: Option<u64>,
        size: Option<u64>,
    },
    /// Provider 开始（`Some`）或结束（`None`）自动重试这一轮的模型请求。
    ProviderRetry(Option<crate::acp_session::ProviderRetryView>),
    /// Provider 侧 steering / follow-up 队列快照。
    PromptQueue {
        steering: Vec<String>,
        follow_up: Vec<String>,
    },
    /// `clear_queue` 之后要把原文还回输入框。`revision` 单调增加；客户端处理后
    /// 发送 `AcknowledgeComposerRestore`，服务端清掉文本和图片，但保留 revision 水位。
    /// `images[i]` 是 `texts[i]` 的附图：「立即发送」认领其中一条时，图片要跟着它走。
    /// 只有图的那条，文字是空串。
    ComposerRestore {
        revision: u64,
        texts: Vec<String>,
        images: Vec<Vec<PromptImage>>,
    },
    /// agent 的选择题 / 表单（AskUserQuestion 类）：UI 渲染字段，凭 responder 回填。
    Elicitation {
        message: String,
        fields: Vec<ElicitField>,
        responder: ElicitationResponder,
        /// 同 `Permission::raw_request_line`。
        raw_request_line: Option<String>,
    },
    /// Pi 收下了这条 prompt（预检成功）。回显从这里开始，不再在送出时提前记入会话。
    PromptAccepted,
    /// Pi 没收下。常见原因是上一轮还在跑。这不是回合结束，也不能写成「回合失败」。
    PromptNotAccepted,
    /// Pi 现在能接受一条不带 streamingBehavior 的 prompt。
    /// 只有这条信号才打开 Pi 会话的发送闸门；界面上的回合结束不算。
    ProviderIdle,
    /// Provider 自己开了一轮，不是宿主发的 prompt。Pi 的后台任务、后台子智能体
    /// 结果带 `triggerTurn` 时就是这样。没有它，这一轮的输出落在已结束的回合上：
    /// 界面不转圈、不能停，结束也没有完成提示。
    ProviderTurnStarted,
    /// 一轮 prompt 结束（含被取消）。
    TurnEnded(StopReason),
    /// 一轮 prompt 以 JSON-RPC 错误收场（agent 明确回了 error，而不是断线）。
    ///
    /// 这跟 `Fatal` 是两回事，混为一谈过：`session/prompt` 的错误响应曾经沿着
    /// SDK 回调一路冒泡，把整条连接 return 掉，于是「没配 API key」这种一句话
    /// 就能改好的问题表现为整个会话猝死、输入框消失。agent 进程当时还活得好
    /// 好的。这里让回合失败、连接继续，用户改完配置直接重发即可。
    TurnFailed(String),
    /// 回退成功：agent 已切到目标消息之前的新分叉。`truncate_from` 和 `then` 是
    /// 发起 `Rewind` 命令时带上的，这里原样奉还；投影据 `truncate_from` 丢弃该消息
    /// 及其之后的所有条目。`then` 是 [`AfterRewind::RestoreComposer`] 时原文由紧随
    /// 其后的 `ComposerRestore` 事件还回输入框（复用现有还原管线，天然带 revision
    /// 去重）；是 [`AfterRewind::Resend`] 时由 daemon 按普通发送再发一次。
    Rewound {
        truncate_from: usize,
        then: AfterRewind,
    },
    /// 会话的 provider 侧身份变了（Pi fork 之后切换到新的 session 文件）。
    /// 恢复 / 续接必须认新 id，旧 id 对应的分支已经是被丢弃的历史。
    ProviderSessionIdChanged(SessionId),
    /// 连接不可恢复地结束：启动失败 / 协议错误 / 子进程退出。带 stderr 尾巴。
    Fatal(String),
}

impl ConversationEvent {
    /// 这条事件是否结束了界面上的当前回合。
    ///
    /// Pi 能不能再收一条 prompt 是另一件事，看 `ProviderIdle`，不要拿这里放行。
    pub fn ends_turn(&self) -> bool {
        matches!(
            self,
            Self::TurnEnded(_) | Self::TurnFailed(_) | Self::Rewound { .. }
        )
    }
}

/// 会话是怎么接上的——决定 UI 拿本地历史怎么办。
#[derive(Clone, Copy, PartialEq)]
pub enum ReadyKind {
    /// 全新会话。本地若有旧历史，UI 插一条分割线标明「以下是新对话」。
    Fresh,
    /// `session/load` 续接：agent 会重放完整历史。归约器先把草稿清空，已发布
    /// 正文留到 `HistoryReplayFinished` 再换上。Ready 本身不得修改消息。
    ResumedWithReplay,
    /// 续接时不重放历史。投影保持调用方已经有的正文。
    ResumedKeepHistory,
}

/// 权限回执守卫：UI 点按钮时消费；**被 drop（视图关闭、卡片被弃置）自动回
/// Cancelled**，保证 agent 侧永远等得到答案、不会挂起。
enum PermissionResponderInner {
    Acp(agent_client_protocol::Responder<RequestPermissionResponse>),
    External(Box<dyn FnOnce(String) + Send>),
}

pub struct PermissionResponder(Option<PermissionResponderInner>);

impl PermissionResponder {
    fn acp(responder: agent_client_protocol::Responder<RequestPermissionResponse>) -> Self {
        Self(Some(PermissionResponderInner::Acp(responder)))
    }

    pub fn external(respond: impl FnOnce(String) + Send + 'static) -> Self {
        Self(Some(PermissionResponderInner::External(Box::new(respond))))
    }

    /// 选中某个选项（allow / reject 都是「选中」，语义在 option.kind 里）。
    pub fn select(mut self, option_id: String) {
        match self.0.take() {
            Some(PermissionResponderInner::Acp(r)) => {
                let _ = r.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                        agent_client_protocol::schema::v1::PermissionOptionId::new(option_id),
                    )),
                ));
            }
            Some(PermissionResponderInner::External(respond)) => respond(option_id),
            None => {}
        }
    }
}

impl Drop for PermissionResponder {
    fn drop(&mut self) {
        match self.0.take() {
            Some(PermissionResponderInner::Acp(r)) => {
                let _ = r.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ));
            }
            Some(PermissionResponderInner::External(respond)) => respond("cancel".into()),
            None => {}
        }
    }
}

/// 外部（非 ACP）回执处理函数：接收回填的 elicitation 内容，None 表示取消。
type ExternalElicitResponder =
    Box<dyn FnOnce(Option<BTreeMap<String, ElicitationContentValue>>) + Send>;

/// 表单回执守卫：accept/decline 消费；**被 drop 自动回 Cancel**，agent 不会挂起。
enum ElicitationResponderInner {
    Acp(agent_client_protocol::Responder<CreateElicitationResponse>),
    External(ExternalElicitResponder),
}

pub struct ElicitationResponder(Option<ElicitationResponderInner>);

impl ElicitationResponder {
    pub fn accept(mut self, content: BTreeMap<String, ElicitationContentValue>) {
        match self.0.take() {
            Some(ElicitationResponderInner::Acp(r)) => {
                let _ = r.respond(CreateElicitationResponse::new(ElicitationAction::Accept(
                    ElicitationAcceptAction::new().content(content),
                )));
            }
            Some(ElicitationResponderInner::External(respond)) => respond(Some(content)),
            None => {}
        }
    }

    pub fn external(
        respond: impl FnOnce(Option<BTreeMap<String, ElicitationContentValue>>) + Send + 'static,
    ) -> Self {
        Self(Some(ElicitationResponderInner::External(Box::new(respond))))
    }
}

impl Drop for ElicitationResponder {
    fn drop(&mut self) {
        match self.0.take() {
            Some(ElicitationResponderInner::Acp(r)) => {
                let _ = r.respond(CreateElicitationResponse::new(ElicitationAction::Cancel));
            }
            Some(ElicitationResponderInner::External(respond)) => respond(None),
            None => {}
        }
    }
}

fn elicitation_fields(request: &CreateElicitationRequest) -> Option<Vec<ElicitField>> {
    match &request.mode {
        ElicitationMode::Form(form) => parse_elicit_fields(&form.requested_schema),
        ElicitationMode::Url(url) => {
            let title = if request.message.trim().is_empty() {
                "在浏览器完成".to_string()
            } else {
                request.message.clone()
            };
            Some(vec![ElicitField {
                key: "url".into(),
                title,
                required: false,
                allow_custom_input: false,
                kind: ElicitFieldKind::ExternalUrl(url.url.clone()),
            }])
        }
        _ => None,
    }
}

/// schema → 简化字段模型。宽容策略：
/// - 按钮化不了的**可选**字段（自由文本、数字等——如 AskUserQuestion 给每题附带的
///   "Other" 自由回答框）直接跳过，不提交即等于没填；
/// - 按钮化不了的**必填**字段 → 返回 None，调用方整表 Decline，agent 退回纯文本问
///   （不能提交一份缺必填项的表单）；
/// - 一个可按钮化字段都没有 → None。
fn parse_elicit_fields(schema: &ElicitationSchema) -> Option<Vec<ElicitField>> {
    let required = schema.required.clone().unwrap_or_default();
    let mut fields = Vec::new();
    for (key, prop) in &schema.properties {
        let is_required = required.iter().any(|required_key| required_key == key);
        let buttonized = match prop {
            ElicitationPropertySchema::String(s) => {
                let options: Vec<ElicitOption> = if let Some(one_of) = &s.one_of {
                    one_of
                        .iter()
                        .map(|o| ElicitOption {
                            value: ElicitationContentValue::String(o.value.clone()),
                            label: o.title.clone(),
                        })
                        .collect()
                } else if let Some(values) = &s.enum_values {
                    values
                        .iter()
                        .map(|v| ElicitOption {
                            value: ElicitationContentValue::String(v.clone()),
                            label: v.clone(),
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                Some(ElicitField {
                    key: key.clone(),
                    title: s.title.clone().unwrap_or_else(|| key.clone()),
                    required: is_required,
                    allow_custom_input: false,
                    kind: if options.is_empty() {
                        ElicitFieldKind::Text { secret: false }
                    } else {
                        ElicitFieldKind::Select(options)
                    },
                })
            }
            ElicitationPropertySchema::Boolean(b) => Some(ElicitField {
                key: key.clone(),
                title: b.title.clone().unwrap_or_else(|| key.clone()),
                required: is_required,
                allow_custom_input: false,
                kind: ElicitFieldKind::Select(vec![
                    ElicitOption {
                        value: ElicitationContentValue::Boolean(true),
                        label: "是".into(),
                    },
                    ElicitOption {
                        value: ElicitationContentValue::Boolean(false),
                        label: "否".into(),
                    },
                ]),
            }),
            ElicitationPropertySchema::Array(a) => {
                let options: Vec<ElicitOption> = match &a.items {
                    MultiSelectItems::String(items) => items
                        .values
                        .iter()
                        .map(|v| ElicitOption {
                            value: ElicitationContentValue::String(v.clone()),
                            label: v.clone(),
                        })
                        .collect(),
                    MultiSelectItems::Titled(items) => items
                        .options
                        .iter()
                        .map(|o| ElicitOption {
                            value: ElicitationContentValue::String(o.value.clone()),
                            label: o.title.clone(),
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                (!options.is_empty()).then(|| ElicitField {
                    key: key.clone(),
                    title: a.title.clone().unwrap_or_else(|| key.clone()),
                    required: is_required,
                    allow_custom_input: false,
                    kind: ElicitFieldKind::MultiSelect(options),
                })
            }
            _ => None, // Number/Integer/未知：MVP 不按钮化
        };
        match buttonized {
            Some(field) => fields.push(field),
            None if is_required => return None,
            None => {} // 可选且按钮化不了：跳过
        }
    }
    if fields.is_empty() {
        None
    } else {
        Some(fields)
    }
}

/// UI 侧持有的会话句柄。drop cmd_tx（整个句柄）即请求连接收摊。
pub struct ConversationHandle {
    pub cmd_tx: smol::channel::Sender<ConversationCommand>,
    pub event_rx: smol::channel::Receiver<ConversationEvent>,
    /// 子进程 pid，spawn 成功后才会填（Fatal 前的极短窗口是 None）。
    /// 停机和杀进程组用这个 pid。
    pub stdio: Arc<Mutex<Option<AcpStdio>>>,
    /// 已发出但尚未收到最终 JSON-RPC response 的请求数。
    pub in_flight_rpc: Arc<AtomicUsize>,
    /// 生命周期层直接设置的停止标记。连接线程在真正 spawn 子进程的同一把
    /// `stdio` 锁内检查它，保证“请求停止时尚未 spawn”的连接以后也不会漏跑。
    pub shutdown_requested: Arc<AtomicBool>,
    /// 驱动能不能把消息插进正在跑的 turn（Pi 的 `steer`）。为假时 daemon 继续
    /// 走「排到回合结束后再发」的老队列，不按引擎名写死判断。
    pub supports_mid_turn_input: bool,
    /// 驱动能不能手动压缩上下文（Pi 的 `compact`）。
    pub supports_compaction: bool,
    /// 驱动能不能发 follow-up、以及取消前先 `clear_queue`（Pi 原生队列）。
    pub supports_native_queue: bool,
    /// 驱动能不能编辑并重发最后一条用户消息（Pi 的 fork）。GUI 据此决定是否显示
    /// 最后一条用户消息上的编辑按钮；daemon 侧动作据此拒绝不支持的 agent。
    pub supports_rewind: bool,
}

/// 子进程 pid。spawn 成功后由连接线程写入，停机时用来杀进程组。
#[derive(Clone, Copy)]
pub struct AcpStdio {
    pub pid: i32,
}

/// SIGKILL 之后等内核收尸的上限。这不是「再猜一次进程死了没」，而是 `waitpid`
/// 的超时：D 状态等无法收的进程在这里失败，由调用方拒绝复用。
const ACP_REAP_AFTER_KILL: Duration = Duration::from_secs(2);

async fn wait_for_event_senders_closed(
    event_rx: &smol::channel::Receiver<ConversationEvent>,
    timeout: Duration,
) -> bool {
    smol::future::race(
        async {
            while event_rx.recv().await.is_ok() {}
            true
        },
        async {
            smol::Timer::after(timeout).await;
            false
        },
    )
    .await
}

/// 统一停止一个 ACP runtime：先阻止尚未发生的 spawn，再请求连接线程正常退出；
/// 宽限过后 SIGKILL 进程组，并且 **waitpid 收到子进程** 才返回 true。
///
/// SIGKILL 不是 Stopped。返回 false 表示无法证明旧进程已退出，调用方不得
/// spawn 替换 runtime，也不得释放 provider session 占用。
pub fn shutdown_and_wait(handle: ConversationHandle, timeout: std::time::Duration) -> bool {
    handle.shutdown_requested.store(true, Ordering::SeqCst);
    let _ = handle.cmd_tx.try_send(ConversationCommand::Shutdown);
    let pid = handle.stdio.lock().unwrap().map(|process| process.pid);
    let event_rx = handle.event_rx;
    // 尚无子进程：停止标记与 spawn 共用 stdio 锁，连接线程不会再创建 provider。
    // 这里不等 event_rx——测试桩会故意留着 sender，那不是一个可收尸的 child。
    let Some(pid) = pid else {
        return true;
    };
    let _ = smol::block_on(wait_for_event_senders_closed(&event_rx, timeout));
    kill_and_reap_process_group(pid, ACP_REAP_AFTER_KILL)
}

/// 证明一个 provider 进程组已经死了。本函数是纯验证，不发任何信号。
///
/// 证明方式取决于亲缘关系（调用方不用选，运行时按 `waitpid` 结果自动分流——
/// 同一个会话交接前是亲生、交接后是收养，关系是动态的）：
/// - 直接子进程：`waitpid`（精确，还能收走僵尸）+ 组缺席双重确认；
/// - 收养进程（handoff 后前任已退）：`waitpid` 永报 ECHILD，改用信号探针，
///   组员退出用内核事件等（macOS `kqueue`，Linux `pidfd`）。调用方应在 prove
///   之前先发过 SIGKILL：刚杀完就地等，pid 复用需要整组先死再重建同号组，
///   不可能在该窗口内完成，探得缺席即原组已死。D-state 等杀不死的只会探得
///   仍在，超时返回 false（与 waitpid 路径同语义）。
///
/// 组缺席之外还查 leader 本人：若该 pid 活着却已不在自编号组里（重设过 pgid
/// 的非 leader），光看组会误判已死。双条件缺一不可。
pub fn prove_process_group_dead(pid: i32, timeout: Duration) -> bool {
    prove_process_group_dead_inner(pid, timeout, true)
}

/// 和 [`prove_process_group_dead`] 一样查组是否已经空了，但绝不 `waitpid`。
///
/// 给「上一次等待还占着这个 pid」的重试用。僵尸对 `kill(pid, 0)` 仍算活着，
/// 组长还没被原来的 wait 收走时，这里返回 false。原来的 wait 返回之后，
/// 下一次才能看到这个号已经不在。
pub fn prove_process_group_dead_without_reaping(pid: i32, timeout: Duration) -> bool {
    prove_process_group_dead_inner(pid, timeout, false)
}

fn prove_process_group_dead_inner(pid: i32, timeout: Duration, reap_child: bool) -> bool {
    if pid <= 1 {
        return true;
    }
    #[cfg(windows)]
    {
        let _ = reap_child;
        prove_windows_process_dead(pid, timeout)
    }
    #[cfg(unix)]
    {
    prove_unix_process_group_dead(pid, timeout, reap_child)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (timeout, reap_child);
        false
    }
}

/// Windows 没有交接过来的进程组。这个 pid 是我们拉起来的，等它退出即可。
#[cfg(windows)]
fn prove_windows_process_dead(pid: i32, timeout: Duration) -> bool {
    // 等进程句柄变成有信号。句柄打不开时 `wait_until_exit` 也报「已退出」，
    // 所以返回前再查一次：没权限打开（活着）不能当成已死。
    crate::process_wait::wait_until_exit(pid, timeout)
        && u32::try_from(pid).is_ok_and(|pid| !smelt_sys::process::pid_is_alive(pid))
}

#[cfg(unix)]
fn prove_unix_process_group_dead(pid: i32, timeout: Duration, reap_child: bool) -> bool {
    let deadline = Instant::now() + timeout;
    // 允许收尸时才做亲缘判定：ECHILD=收养/已收走。此后不再复核。
    // 不允许收尸时，连这一次 WNOHANG 也不发，避免和还堵在 wait 上的那条线程抢。
    let ours = if reap_child {
        loop {
            match waitpid_child(pid, libc::WNOHANG) {
                Waitpid::Interrupted => {
                    if Instant::now() >= deadline {
                        return false;
                    }
                }
                Waitpid::NotOurChild => break false,
                Waitpid::Reaped | Waitpid::StillRunning => break true,
            }
        }
    } else {
        false
    };
    let mut reported_dead = BTreeSet::new();
    let mut blind_wait_used = false;
    loop {
        let child_gone = if ours {
            match waitpid_child(pid, libc::WNOHANG) {
                Waitpid::Reaped | Waitpid::NotOurChild => true,
                Waitpid::StillRunning | Waitpid::Interrupted => false,
            }
        } else {
            true
        };
        if child_gone && process_group_absent(pid) && process_absent(pid) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let remaining = deadline - now;
        let mut watch = Vec::new();
        if ours && !child_gone {
            watch.push(pid);
        }
        for member in process_group_members(pid) {
            if member > 1 && !reported_dead.contains(&member) {
                watch.push(member);
            }
        }
        watch.sort_unstable();
        watch.dedup();
        if watch.is_empty() {
            // 能看的 pid 都已经报过退出，组探针却还说人在（多半是还没被收走的僵尸）。
            // 把剩下的截止时间一次睡完再查，不再隔几毫秒问。
            if blind_wait_used {
                return false;
            }
            blind_wait_used = true;
            // 轮询例外：僵尸由别的父进程收走时没有我们能挂的事件（pid 已报过退出，组探针
            // 要等收尸才变）；只睡一次，周期即剩余时间，上限是调用方的 timeout。
            std::thread::park_timeout(remaining);
            continue;
        }
        match crate::process_wait::wait_until_any_exit(&watch, remaining) {
            Some(exited) => {
                reported_dead.insert(exited);
                if ours && exited == pid {
                    let _ = waitpid_child(pid, libc::WNOHANG);
                }
            }
            None => return false,
        }
    }
}

/// pid 本人是否已不存在。EPERM（活着但无权限）按存在处理。
#[cfg(unix)]
fn process_absent(pid: i32) -> bool {
    if pid <= 1 {
        return true;
    }
    let sent = unsafe { libc::kill(pid, 0) };
    sent != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// 终止一个 provider 进程组并证明它死了：发 SIGKILL + [`prove_process_group_dead`]。
///
/// SIGKILL 发一次：它不可捕获，重发只对“杀完又 fork 出逃”的组员有意义——而那
/// 个竞态在最后一次确认之后依然存在，多发一次关不上。确认失败（D-state 等）
/// 返回 false，调用方不得 spawn 替代 runtime（见 `unreaped_pid` 语义）。
/// pid 必须是调用方刚确认过归属的（活句柄、或刚验过启动时间）；陌生 pid 先验
/// 身份再杀，见 smeltd 交接清理的启动时间门。
pub fn kill_and_reap_process_group(pid: i32, timeout: Duration) -> bool {
    if pid <= 1 {
        return true;
    }
    kill_process_group(pid);
    let dead = prove_process_group_dead(pid, timeout);
    if dead && let Ok(pid) = u32::try_from(pid) {
        smelt_sys::process::release_tree(pid);
    }
    dead
}

/// 进程启动时刻（wall clock）。给“按 pid 动刀前验明正身”用：handoff 后 successor
/// 与收养进程无亲缘关系，裸 pid 可能已被复用；启动早于快照时刻的才是原进程
/// （复用只能发生在原进程死后，即快照之后——因果律，不是时钟信任；仅极端的
/// NTP 大步回拨能打破，见调用方注释）。
/// 读不到（进程已死、无权限、不支持的平台）返回 None，调用方必须按“验不出”处理。
pub fn process_start_time(pid: i32) -> Option<std::time::SystemTime> {
    if pid <= 1 {
        return None;
    }
    process_start_time_inner(pid)
}

#[cfg(target_os = "macos")]
fn process_start_time_inner(pid: i32) -> Option<std::time::SystemTime> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    std::time::UNIX_EPOCH.checked_add(std::time::Duration::new(
        info.pbi_start_tvsec,
        (info.pbi_start_tvusec * 1000) as u32,
    ))
}

/// 启动时刻：开机时刻 + 启动 tick。精度是一个 tick（通常 10ms）。
///
/// 开机时刻不用 `/proc/stat` 的 `btime`：它只到整秒，会把所有进程的启动时刻往前挪
/// 最多一秒，快照之后才复用这个 pid 的进程就可能被当成快照前出生的原进程杀掉。
/// 改用 `CLOCK_REALTIME - CLOCK_BOOTTIME`，进程内只算一次。
#[cfg(target_os = "linux")]
fn process_start_time_inner(pid: i32) -> Option<std::time::SystemTime> {
    static BOOTED_AT: std::sync::OnceLock<Option<std::time::SystemTime>> =
        std::sync::OnceLock::new();
    let booted_at = (*BOOTED_AT.get_or_init(|| {
        let mut boottime: libc::timespec = unsafe { std::mem::zeroed() };
        if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut boottime) } != 0 {
            return None;
        }
        let since_boot = std::time::Duration::new(boottime.tv_sec as u64, boottime.tv_nsec as u32);
        std::time::SystemTime::now().checked_sub(since_boot)
    }))?;
    let ticks_per_sec = linux_ticks_per_sec()?;
    let start_ticks = linux_start_ticks(pid)?;
    booted_at.checked_add(std::time::Duration::from_nanos(
        (u128::from(start_ticks) * 1_000_000_000 / u128::from(ticks_per_sec)) as u64,
    ))
}

/// `/proc/<pid>/stat` 第 22 字段：自开机起的启动 tick。同一次开机内对一个进程恒定。
#[cfg(target_os = "linux")]
fn linux_start_ticks(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(target_os = "linux")]
fn linux_ticks_per_sec() -> Option<u64> {
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    (ticks > 0).then_some(ticks as u64)
}

/// 进程的出生标识：同一个进程任何时候、任何进程读都一样，pid 被复用后一定不同。
/// 跨进程记下来以后比对身份用；要和墙钟比先后用 [`process_start_time`]。
pub fn process_birth(pid: i32) -> Option<String> {
    if pid <= 1 {
        return None;
    }
    process_birth_inner(pid)
}

#[cfg(target_os = "linux")]
fn process_birth_inner(pid: i32) -> Option<String> {
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(format!("{}:{}", boot_id.trim(), linux_start_ticks(pid)?))
}

#[cfg(not(target_os = "linux"))]
fn process_birth_inner(pid: i32) -> Option<String> {
    let started = process_start_time_inner(pid)?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(format!(
        "{}.{:09}",
        started.as_secs(),
        started.subsec_nanos()
    ))
}

#[cfg(windows)]
fn process_start_time_inner(pid: i32) -> Option<std::time::SystemTime> {
    let Ok(pid) = u32::try_from(pid) else {
        return None;
    };
    smelt_sys::process::process_started_at(pid)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn process_start_time_inner(_pid: i32) -> Option<std::time::SystemTime> {
    None
}

#[cfg(unix)]
enum Waitpid {
    Reaped,
    StillRunning,
    NotOurChild,
    Interrupted,
}

#[cfg(unix)]
fn waitpid_child(pid: i32, options: i32) -> Waitpid {
    let mut status = 0;
    let waited = unsafe { libc::waitpid(pid, &mut status, options) };
    if waited == pid {
        Waitpid::Reaped
    } else if waited == 0 {
        Waitpid::StillRunning
    } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD) {
        Waitpid::NotOurChild
    } else {
        Waitpid::Interrupted
    }
}

#[cfg(unix)]
fn process_group_absent(pgid: i32) -> bool {
    if pgid <= 1 {
        return true;
    }
    let sent = unsafe { libc::kill(-pgid, 0) };
    sent != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// 这个进程组里现在还挂着哪些 pid。用来等组员退出，而不是隔几毫秒问组还在不在。
/// 枚举失败就返回空：调用方会把剩余截止时间一次睡完再查。
#[cfg(unix)]
fn process_group_members(pgid: i32) -> Vec<i32> {
    if pgid <= 1 {
        return Vec::new();
    }
    process_group_members_os(pgid)
}

#[cfg(target_os = "macos")]
fn process_group_members_os(pgid: i32) -> Vec<i32> {
    let mut count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }
    count = count.saturating_add(64);
    let mut pids = vec![0i32; count as usize];
    let bytes = (pids.len() * std::mem::size_of::<i32>()) as libc::c_int;
    let read = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    if read <= 0 {
        return Vec::new();
    }
    let read = (read as usize).min(pids.len());
    let mut members = Vec::new();
    for &pid in &pids[..read] {
        if pid > 1 && process_group_id(pid) == Some(pgid) {
            members.push(pid);
        }
    }
    members
}

#[cfg(target_os = "macos")]
fn process_group_id(pid: i32) -> Option<i32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (read == size).then_some(info.pbi_pgid as i32)
}

#[cfg(target_os = "linux")]
fn process_group_members_os(pgid: i32) -> Vec<i32> {
    let mut members = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return members;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        if pid > 1 && linux_process_group(pid) == Some(pgid) {
            members.push(pid);
        }
    }
    members
}

#[cfg(target_os = "linux")]
fn linux_process_group(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = stat.rsplit_once(')')?.1;
    after_comm.split_whitespace().nth(2)?.parse().ok()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn process_group_members_os(_pgid: i32) -> Vec<i32> {
    Vec::new()
}

/// 根据启动规格选择 provider 原生驱动。Pi 走自己的 JSONL RPC；其余 agent 才
/// 进入 ACP。返回的统一句柄只承载 Smelt 内部命令/事件，不代表底层 wire 协议。
pub fn spawn_agent_runtime(
    launch: ConversationLaunch,
    spawn_gate: Option<Arc<RwLock<()>>>,
) -> ConversationHandle {
    if crate::pi_rpc::is_smelt_pi_launch(&launch.launch) {
        crate::pi_rpc::spawn_pi_rpc(launch, spawn_gate)
    } else {
        spawn_acp(launch, spawn_gate)
    }
}

/// 起一条专用线程跑 ACP 连接，立即返回句柄。`spawn_gate` 只包住实际创建
/// 子进程到发布 `ConversationHandle.stdio` 的区间；运行时解析/下载在拿读锁之前完成。
/// 不需要与外部升级流程协调的调用方可传 `None`。
pub fn spawn_acp(
    launch: ConversationLaunch,
    spawn_gate: Option<Arc<RwLock<()>>>,
) -> ConversationHandle {
    let (cmd_tx, cmd_rx) = smol::channel::unbounded::<ConversationCommand>();
    let (event_tx, event_rx) = smol::channel::unbounded::<ConversationEvent>();
    let cmd_tx_for_thread = cmd_tx.clone();
    let stdio: Arc<Mutex<Option<AcpStdio>>> = Arc::new(Mutex::new(None));
    let in_flight_rpc = Arc::new(AtomicUsize::new(0));
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    let in_flight_rpc_for_thread = Arc::clone(&in_flight_rpc);
    let stdio_for_thread = Arc::clone(&stdio);
    let shutdown_requested_for_thread = Arc::clone(&shutdown_requested);
    let thread_name = format!("smelt-acp-{}", &launch.sid[..launch.sid.len().min(12)]);
    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            // stderr 尾巴：环形保尾部若干行，Fatal 时拼进诊断（npx 找不到包/装包
            // 失败的真实原因都在 stderr 里，别让用户猜）。
            let stderr_tail: Arc<Mutex<Vec<String>>> = Arc::default();
            // 先把 bunx / bun 指到应用里的 Bun，再进连接循环。
            let (cmd, managed_bun) = {
                let tx = event_tx.clone();
                match resolve_runtime_command_with_bun(&launch.launch.command, &|msg| {
                    let _ = tx.try_send(ConversationEvent::Status(msg.to_string()));
                }) {
                    Ok(cmd) => cmd,
                    Err(e) => {
                        let _ = event_tx.try_send(ConversationEvent::Fatal(e));
                        return;
                    }
                }
            };
            let ConversationLaunch {
                launch: launch_spec,
                ephemeral_env,
                cwd,
                sid,
                agent_token,
                resume_session_id,
                fork_session_id,
                fork_cut,
            } = launch;
            let launch = ConversationLaunch {
                launch: ConversationLaunchSpec {
                    command: cmd,
                    env: launch_spec.env,
                },
                ephemeral_env,
                cwd,
                sid,
                agent_token,
                resume_session_id,
                fork_session_id,
                fork_cut,
            };
            let result = smol::block_on(run_connection(
                &launch,
                managed_bun,
                AcpChannels {
                    cmd_rx,
                    cmd_tx: cmd_tx_for_thread,
                    event_tx: event_tx.clone(),
                },
                ConnectionSync {
                    stderr_tail: stderr_tail.clone(),
                    stdio_out: stdio_for_thread,
                    spawn_gate,
                    in_flight_rpc: in_flight_rpc_for_thread,
                    shutdown_requested: shutdown_requested_for_thread,
                },
            ));
            if let Err(e) = result {
                let tail = stderr_tail.lock().unwrap().join("\n");
                let msg = if tail.is_empty() {
                    format!("{e}")
                } else {
                    format!("{e}\n--- agent stderr ---\n{tail}")
                };
                crate::app_log::error("acp", &format!("会话 {} 连接异常终止：{msg}", launch.sid));
                let _ = event_tx.try_send(ConversationEvent::Fatal(msg));
            }
            // Ok 结束（Shutdown）不发 Fatal——UI 主动关的，没必要再报。
        })
        .expect("spawn smelt-acp thread");
    ConversationHandle {
        cmd_tx,
        event_rx,
        stdio,
        in_flight_rpc,
        shutdown_requested,
        // 通用 ACP 没有中途插话的协议动作，运行中的消息仍由 daemon 排队。
        supports_mid_turn_input: false,
        supports_compaction: false,
        supports_native_queue: false,
        supports_rewind: false,
    }
}

fn with_spawn_gate<T>(spawn_gate: Option<&Arc<RwLock<()>>>, spawn: impl FnOnce() -> T) -> T {
    let _permit = spawn_gate.map(|gate| gate.read().unwrap());
    spawn()
}

/// 写一行（带换行）到异步 writer——`agent_client_protocol` 自己的同款 helper
/// 是 `pub(crate)`，这边够简单，直接写一份。
async fn write_line<W>(w: &mut W, line: String) -> std::io::Result<()>
where
    W: futures::AsyncWrite + Unpin,
{
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await
}

/// 把一个异步 writer 包成 `Lines` transport 要的 outgoing `Sink<String>`。
fn make_outgoing_sink<W>(writer: W) -> impl futures::Sink<String, Error = std::io::Error>
where
    W: futures::AsyncWrite + Send + Unpin + 'static,
{
    futures::sink::unfold(writer, |mut w, line: String| async move {
        write_line(&mut w, line).await?;
        Ok::<_, std::io::Error>(w)
    })
}

/// 把一个异步 reader 包成 `Lines` transport 要的 incoming `Stream<Item =
/// io::Result<String>>`，顺带把每一行原文写进 `last_stdout_line`——跟旧版
/// `AcpAgent::with_debug` 的 `Stdout` 方向是同一件事，只是现在自己接管
/// spawn 之后，`with_debug` 钩子不会被 SDK 调用了，得自己包一层
/// （`futures::StreamExt::inspect` 原理跟 SDK 内部一样：逐行 `.lines()` 之后
/// 挂一个旁路回调，不影响往下游转发的内容）。
fn make_incoming_lines<R>(
    reader: R,
    last_stdout_line: Arc<Mutex<Option<String>>>,
    config_tx: Option<smol::channel::Sender<ConversationEvent>>,
) -> impl futures::Stream<Item = std::io::Result<String>> + Send
where
    R: futures::AsyncRead + Send + Unpin + 'static,
{
    futures::io::BufReader::new(reader)
        .lines()
        .inspect(move |res| {
            if let Ok(line) = res {
                *last_stdout_line.lock().unwrap() = Some(line.clone());
                if let Some(event_tx) = &config_tx {
                    publish_raw_session_config_line(line, event_tx);
                }
            }
        })
}

/// 子进程 stderr 逐行收进尾巴（原来挂在 `AcpAgent::with_debug` 的
/// `Stderr` 分支，自己接管 spawn 之后要自己起一条任务做）。
fn spawn_stderr_drain(stderr: async_process::ChildStderr, stderr_tail: Arc<Mutex<Vec<String>>>) {
    smol::spawn(async move {
        let mut lines = futures::io::BufReader::new(stderr).lines();
        while let Some(Ok(line)) = lines.next().await {
            let mut tail = stderr_tail.lock().unwrap();
            if tail.len() >= 30 {
                tail.remove(0);
            }
            tail.push(line);
        }
    })
    .detach();
}

/// Unix 下 agent 子进程 spawn 时已经 `process_group(0)` 成了自己那组的组长
/// （见 `AcpAgent::spawn_process` 文档：常见的 `npx …`/`uvx …` 包装启动器要
/// 连它派生出的真身一起杀，只杀直接子进程会留孤儿）。正常走
/// `Client::connect_with(agent, ..)` 时 SDK 自己的 `ChildGuard` 负责这个；
/// 这里改成自己调 `spawn_process()`，没有那份内部 guard，得自己补——
/// Drop 时对整个进程组发 SIGKILL，跟 smeltd 杀终端会话用的是同一个系统调用。
struct AcpChildGuard {
    pid: i32,
    child: async_process::Child,
    reaped: bool,
}

impl AcpChildGuard {
    fn new(child: async_process::Child) -> Self {
        Self {
            pid: child.id() as i32,
            child,
            reaped: false,
        }
    }

    fn kill_tree(&mut self) {
        kill_process_group(self.pid);
        let _ = self.child.kill();
    }

    /// 用 `waitpid` 确认直属子进程已经退出。`Child::try_status()` 只是问一句
    /// 「现在死了没有」；信号被掩码挡住时，隔 10ms 再杀一次也等不来结果。
    /// 先标上已收，避免这个 Child 析构时再进来一次。杀一次，然后堵住等到
    /// `waitpid` 把直属子进程收回。
    fn reap_now(&mut self) {
        if self.reaped {
            return;
        }
        self.reaped = true;
        self.kill_tree();
        let _ = crate::process_wait::reap_direct_child(self.pid);
        if let Ok(pid) = u32::try_from(self.pid) {
            smelt_sys::process::release_tree(pid);
        }
    }

    fn kill_and_reap(&mut self) {
        self.reap_now();
    }
}

impl Drop for AcpChildGuard {
    fn drop(&mut self) {
        self.reap_now();
    }
}

fn kill_process_group(pid: i32) {
    if pid <= 1 {
        return;
    }
    let Ok(pid) = u32::try_from(pid) else {
        return;
    };
    let _ = smelt_sys::process::signal_tree(pid, smelt_sys::process::TreeSignal::Kill);
}

/// `connect_with` 的 SDK future 可能卡在 transport 或 agent 自己的启动阶段，
/// 这时 `wait_for_acp_request` 只能包住已经发出去的 JSON-RPC 请求，管不到更外层。
/// 这里用一个独立 OS 线程守住「spawn 到 session attach」这段生命周期：握手超时就
/// 杀整个进程组，让 stdio EOF 唤醒连接 future，最终由 `spawn_acp` 发出 Fatal。
struct AcpHandshakeWatchdog {
    completed: Arc<(Mutex<bool>, Condvar)>,
    timed_out: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AcpHandshakeWatchdog {
    fn new(pid: i32) -> Self {
        Self::with_timeout(pid, ACP_HANDSHAKE_TIMEOUT)
    }

    fn with_timeout(pid: i32, timeout: Duration) -> Self {
        let completed = Arc::new((Mutex::new(false), Condvar::new()));
        let timed_out = Arc::new(AtomicBool::new(false));
        let completed_for_thread = Arc::clone(&completed);
        let timed_out_for_thread = Arc::clone(&timed_out);
        let thread = std::thread::Builder::new()
            .name(format!("smelt-acp-watchdog-{pid}"))
            .spawn(move || {
                if wait_for_acp_handshake(&completed_for_thread, timeout) {
                    return;
                }
                timed_out_for_thread.store(true, Ordering::Release);
                kill_process_group(pid);
            })
            .expect("spawn ACP handshake watchdog");
        Self {
            completed,
            timed_out,
            thread: Some(thread),
        }
    }

    fn mark_complete(&self) {
        let (completed, wake) = &*self.completed;
        *completed.lock().unwrap() = true;
        wake.notify_one();
    }

    fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::Acquire)
    }
}

impl Drop for AcpHandshakeWatchdog {
    fn drop(&mut self) {
        self.mark_complete();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 等待握手完成；处理虚假唤醒，并以 deadline 为准避免反复延长超时时间。
fn wait_for_acp_handshake(completed: &Arc<(Mutex<bool>, Condvar)>, timeout: Duration) -> bool {
    let (completed, wake) = &**completed;
    let deadline = Instant::now() + timeout;
    let mut completed_guard = completed.lock().unwrap();
    loop {
        if *completed_guard {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let (next_guard, wait_result) = wake.wait_timeout(completed_guard, remaining).unwrap();
        completed_guard = next_guard;
        if wait_result.timed_out() && !*completed_guard {
            return false;
        }
    }
}

fn signal_acp_handshake_complete(completed: &Arc<(Mutex<bool>, Condvar)>) {
    let (completed, wake) = &**completed;
    *completed.lock().unwrap() = true;
    wake.notify_one();
}

fn classify_restore_failure(error: &agent_client_protocol::Error) -> ConversationRestoreFailure {
    if is_missing_history_error(error) {
        return ConversationRestoreFailure::HistoryMissing;
    }
    match error.code {
        agent_client_protocol::ErrorCode::MethodNotFound => {
            ConversationRestoreFailure::UnsupportedLoad
        }
        _ => ConversationRestoreFailure::Failed(format!("恢复历史对话失败，可重试：{error}")),
    }
}

/// 不同 ACP adapter 对「历史 rollout 不存在」使用了不同的错误形态：有的返回
/// `ResourceNotFound`，Codex ACP 则把 app-server 的错误包装成 `InternalError`，
/// 并把真实原因放进 `data.details`。这里只识别明确的 missing 文案，不能把所有
/// `InternalError` 都降级成新会话。
fn is_missing_history_error(error: &agent_client_protocol::Error) -> bool {
    if matches!(
        error.code,
        agent_client_protocol::ErrorCode::ResourceNotFound
    ) {
        return true;
    }

    let contains_marker = |value: Option<&str>| {
        value.is_some_and(|value| value.to_ascii_lowercase().contains("no rollout found"))
    };
    contains_marker(Some(&error.message))
        || error.data.as_ref().is_some_and(|data| {
            contains_marker(data.as_str())
                || contains_marker(data.get("details").and_then(serde_json::Value::as_str))
                || contains_marker(data.get("message").and_then(serde_json::Value::as_str))
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionStart {
    New,
    Load,
    UnsupportedLoad,
}

fn select_session_start(has_session_id: bool, load_supported: bool) -> SessionStart {
    match (has_session_id, load_supported) {
        (false, _) => SessionStart::New,
        (true, true) => SessionStart::Load,
        (true, false) => SessionStart::UnsupportedLoad,
    }
}

/// 连接线程与驱动循环共享的通道组：UI 指令下行、事件上行。
#[derive(Clone)]
struct AcpChannels {
    cmd_rx: smol::channel::Receiver<ConversationCommand>,
    cmd_tx: smol::channel::Sender<ConversationCommand>,
    event_tx: smol::channel::Sender<ConversationEvent>,
}

fn acp_err(message: impl Into<String>) -> agent_client_protocol::Error {
    agent_client_protocol::Error::internal_error().data(message.into())
}

fn spawn_terminal_event_forwarder(
    event_tx: smol::channel::Sender<ConversationEvent>,
) -> smol::channel::Sender<(String, crate::acp_terminal::TerminalSnapshot)> {
    let (term_tx, term_rx) =
        smol::channel::unbounded::<(String, crate::acp_terminal::TerminalSnapshot)>();
    smol::spawn(async move {
        while let Ok((terminal_id, snapshot)) = term_rx.recv().await {
            let _ = event_tx.try_send(ConversationEvent::TerminalOutput {
                terminal_id,
                output: snapshot.output,
                truncated: snapshot.truncated,
                exit_code: snapshot.exit_code,
                signal: snapshot.signal,
            });
        }
    })
    .detach();
    term_tx
}

fn resolve_terminal_command(command: &str) -> String {
    if command.contains('/') {
        return command.to_string();
    }
    resolve_in_path(command, &extended_search_path()).unwrap_or_else(|| command.to_string())
}

struct ConnectionSync {
    stderr_tail: Arc<Mutex<Vec<String>>>,
    stdio_out: Arc<Mutex<Option<AcpStdio>>>,
    spawn_gate: Option<Arc<RwLock<()>>>,
    in_flight_rpc: Arc<AtomicUsize>,
    shutdown_requested: Arc<AtomicBool>,
}

/// 连接主体：spawn agent 子进程 → initialize → newSession → 双源 loop
/// （UI 指令 / agent 更新流）。返回 Ok 表示用户主动 Shutdown。
async fn run_connection(
    launch: &ConversationLaunch,
    managed_bun: Option<crate::managed_runtime::ManagedBunRuntime>,
    channels: AcpChannels,
    sync: ConnectionSync,
) -> Result<(), agent_client_protocol::Error> {
    let ConnectionSync {
        stderr_tail,
        stdio_out,
        spawn_gate,
        in_flight_rpc,
        shutdown_requested,
    } = sync;
    let agent = build_agent(&launch.launch, &launch.ephemeral_env)?;
    let spawned = with_spawn_gate(spawn_gate.as_ref(), || {
        let mut stdio = stdio_out.lock().unwrap();
        if shutdown_requested.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let (child_stdin, child_stdout, child_stderr, child) =
            spawn_agent_process(&agent, launch.cwd.as_deref(), managed_bun.as_ref())?;
        *stdio = Some(AcpStdio {
            pid: child.id() as i32,
        });
        Ok::<_, agent_client_protocol::Error>(Some((
            child_stdin,
            child_stdout,
            child_stderr,
            child,
        )))
    })?;
    let Some((child_stdin, child_stdout, child_stderr, child)) = spawned else {
        return Ok(());
    };
    let pid = child.id() as i32;
    let mut child_guard = AcpChildGuard::new(child);
    let handshake_watchdog = AcpHandshakeWatchdog::new(pid);
    spawn_stderr_drain(child_stderr, stderr_tail);

    let last_stdout_line: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let outgoing = make_outgoing_sink(child_stdin);
    let incoming = make_incoming_lines(
        child_stdout,
        Arc::clone(&last_stdout_line),
        Some(channels.event_tx.clone()),
    );
    let transport = Lines::new(outgoing, incoming);

    let cwd = launch
        .cwd
        .clone()
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "/".to_string());
    let perm_tx = channels.event_tx.clone();
    let elicit_tx = channels.event_tx.clone();
    let perm_last_line = Arc::clone(&last_stdout_line);
    let elicit_last_line = Arc::clone(&last_stdout_line);
    let handshake_complete = Arc::clone(&handshake_watchdog.completed);
    let terminals = Arc::new(crate::acp_terminal::AcpTerminals::new());
    let term_tx = spawn_terminal_event_forwarder(channels.event_tx.clone());
    let session_cwd = Some(std::path::PathBuf::from(&cwd));
    let result =
        Client
        .builder()
        .name("smelt")
        // 权限请求：Responder 打包进事件甩给 UI，handler 立即返回不堵事件循环；
        // UI 弃置卡片时 PermissionResponder 的 Drop 兜底回 Cancelled。
        .on_receive_request(
            move |request: RequestPermissionRequest, responder, _connection| {
                let perm_tx = perm_tx.clone();
                let raw_request_line = perm_last_line.lock().unwrap().clone();
                async move {
                    let (question, details) = permission_from_request(&request);
                    let _ = perm_tx.try_send(ConversationEvent::Permission {
                        question,
                        tool_call_id: request.tool_call.tool_call_id.clone(),
                        pub_options: request
                            .options
                            .into_iter()
                            .map(|option| crate::acp_session::PermissionOptionView {
                                option_id: option.option_id.to_string(),
                                name: option.name,
                                kind: crate::acp_session::PermissionOptionKindView::from_acp(
                                    option.kind,
                                ),
                            })
                            .collect(),
                        responder: PermissionResponder::acp(responder),
                        details,
                        raw_request_line,
                    });
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        // 选择题 / 表单：能按钮化的甩给 UI，按钮化不了的立即 Decline——agent 会退回
        // 纯文本问，跟不支持该能力时的行为一致，绝不让请求悬着。
        .on_receive_request(
            move |request: CreateElicitationRequest, responder, _connection| {
                let elicit_tx = elicit_tx.clone();
                let raw_request_line = elicit_last_line.lock().unwrap().clone();
                async move {
                    let fields = elicitation_fields(&request);
                    match fields {
                        Some(fields) => {
                            let _ = elicit_tx.try_send(ConversationEvent::Elicitation {
                                message: request.message,
                                fields,
                                responder: ElicitationResponder(Some(
                                    ElicitationResponderInner::Acp(responder),
                                )),
                                raw_request_line,
                            });
                            Ok(())
                        }
                        None => responder
                            .respond(CreateElicitationResponse::new(ElicitationAction::Decline)),
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            move |request: ReadTextFileRequest,
                  responder: Responder<ReadTextFileResponse>,
                  _connection| async move {
                match crate::acp_terminal::read_text_file(
                    &request.path,
                    request.line,
                    request.limit,
                ) {
                    Ok(content) => {
                        let _ = responder.respond(ReadTextFileResponse::new(content));
                        Ok(())
                    }
                    Err(msg) => Err(acp_err(msg)),
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            move |request: WriteTextFileRequest,
                  responder: Responder<WriteTextFileResponse>,
                  _connection| async move {
                crate::acp_terminal::write_text_file(&request.path, &request.content)
                    .map_err(acp_err)?;
                let _ = responder.respond(WriteTextFileResponse::new());
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let terminals = Arc::clone(&terminals);
                let term_tx = term_tx.clone();
                let session_cwd = session_cwd.clone();
                move |request: CreateTerminalRequest,
                      responder: Responder<CreateTerminalResponse>,
                      _connection| {
                    let terminals = Arc::clone(&terminals);
                    let term_tx = term_tx.clone();
                    let session_cwd = session_cwd.clone();
                    async move {
                        let command = resolve_terminal_command(&request.command);
                        let env = request
                            .env
                            .into_iter()
                            .map(|item| (item.name, item.value))
                            .collect();
                        let id = terminals
                            .create(
                                command,
                                request.args,
                                env,
                                request.cwd.or(session_cwd),
                                request.output_byte_limit,
                                term_tx,
                            )
                            .map_err(acp_err)?;
                        let _ = responder.respond(CreateTerminalResponse::new(id));
                        Ok(())
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let terminals = Arc::clone(&terminals);
                move |request: TerminalOutputRequest,
                      responder: Responder<TerminalOutputResponse>,
                      _connection| {
                    let terminals = Arc::clone(&terminals);
                    async move {
                        let id = request.terminal_id.to_string();
                        let snapshot = terminals
                            .snapshot(&id)
                            .ok_or_else(|| acp_err(format!("终端不存在：{id}")))?;
                        let mut response =
                            TerminalOutputResponse::new(snapshot.output, snapshot.truncated);
                        if let Some(exit) = terminals.proto_exit(&id) {
                            response = response.exit_status(exit);
                        }
                        let _ = responder.respond(response);
                        Ok(())
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let terminals = Arc::clone(&terminals);
                move |request: WaitForTerminalExitRequest,
                      responder: Responder<WaitForTerminalExitResponse>,
                      _connection| {
                    let terminals = Arc::clone(&terminals);
                    async move {
                        let id = request.terminal_id.to_string();
                        if terminals.snapshot(&id).is_none() {
                            return Err(acp_err(format!("终端不存在：{id}")));
                        }
                        std::thread::Builder::new()
                            .name("smelt-acp-term-wait-rpc".into())
                            .spawn(move || {
                                let status = terminals.wait(&id).unwrap_or_else(|_| {
                                    agent_client_protocol::schema::v1::TerminalExitStatus::new()
                                });
                                let _ = responder
                                    .respond(WaitForTerminalExitResponse::new(status));
                            })
                            .map_err(|err| acp_err(format!("等待线程启动失败：{err}")))?;
                        Ok(())
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let terminals = Arc::clone(&terminals);
                move |request: KillTerminalRequest,
                      responder: Responder<KillTerminalResponse>,
                      _connection| {
                    let terminals = Arc::clone(&terminals);
                    async move {
                        terminals
                            .kill(&request.terminal_id.to_string())
                            .map_err(acp_err)?;
                        let _ = responder.respond(KillTerminalResponse::new());
                        Ok(())
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            {
                let terminals = Arc::clone(&terminals);
                move |request: ReleaseTerminalRequest,
                      responder: Responder<ReleaseTerminalResponse>,
                      _connection| {
                    let terminals = Arc::clone(&terminals);
                    async move {
                        terminals
                            .release(&request.terminal_id.to_string())
                            .map_err(acp_err)?;
                        let _ = responder.respond(ReleaseTerminalResponse::new());
                        Ok(())
                    }
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            move |request: AgentRequest,
                  responder: Responder<serde_json::Value>,
                  _connection| async move {
                respond_vendor_ext_request(request, responder).await
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(transport, |connection: ConnectionTo<Agent>| async move {
            let init =
                wait_for_acp_request(
                    connection
                        .send_request(
                            InitializeRequest::new(ProtocolVersion::V1)
                                .client_info(Implementation::new(
                                    "smelt",
                                    smelt_version::VERSION,
                                ))
                                .client_capabilities(smelt_client_capabilities()),
                        )
                        .block_task(),
                    "initialize",
                )
                .await?;
            // 收图能力：三条恢复路径共用（bool 是 Copy，多次读没问题）。
            let supports_image = init.agent_capabilities.prompt_capabilities.image;

            // 冷恢复只有一条路径：`session/load`。agent 的 session store 是历史
            // 唯一持久化来源，load 重放的更新负责重建 smeltd 的 entries 投影。
            // `session/resume` 不重放历史，不能拿来冷恢复；smeltd 仍持有完整活体
            // 状态时根本不会进这里，而是在 `acp_open` 里直接 attach。
            //
            // 任何恢复失败都必须显式分类报告：连接层不在这里静默 session/new，
            // 由上层 supervisor 根据本地投影决定是否安全地降级。
            match select_session_start(
                launch.resume_session_id.is_some(),
                init.agent_capabilities.load_session,
            ) {
                SessionStart::UnsupportedLoad => {
                    signal_acp_handshake_complete(&handshake_complete);
                    let _ = channels
                        .event_tx
                        .try_send(ConversationEvent::RestoreFailed(ConversationRestoreFailure::UnsupportedLoad));
                    return Ok(());
                }
                SessionStart::New => {}
                SessionStart::Load => {
                    let sid = launch
                        .resume_session_id
                        .clone()
                        .expect("Load path requires a session id");
                    // 2.1 的 load_session_from 会在发出 session/load 之前装好
                    // session 路由，Claude 在响应前同步重放的历史不会被丢掉。
                    // 清空事件必须排在请求之前，不能等到 Ready 再清。
                    let _ = channels.event_tx.try_send(ConversationEvent::HistoryReplayStarted);
                    let mut load_request = LoadSessionRequest::new(sid.clone(), cwd.clone());
                    if let Some(meta) = claude_raw_sdk_meta(&launch.launch.command) {
                        load_request = load_request.meta(meta);
                    }
                    match connection
                        .load_session_from(load_request)
                        .block_task()
                        .start_session()
                        .await
                    {
                        Ok(restored) => {
                            let (mut session, loaded) = restored.into_parts();
                            publish_session_surface(
                                loaded.config_options.as_deref(),
                                loaded.modes.as_ref(),
                                &channels.event_tx,
                            );
                            // 响应前的重放在会话自己的队列里，还没进事件通道。先排空，
                            // 再收尾：草稿要等正文全部到齐才一次性换上；先发收尾会让
                            // 草稿以空正文提交，随后的历史被当成实时消息。
                            drain_loaded_replay(
                                &mut session,
                                &channels.event_tx,
                                &mut ReplayCursor::default(),
                            )
                            .await?;
                            let _ = channels
                                .event_tx
                                .try_send(ConversationEvent::HistoryReplayFinished);
                            signal_acp_handshake_complete(&handshake_complete);
                            return drive_session(
                                session,
                                channels,
                                ReadyKind::ResumedWithReplay,
                                supports_image,
                                in_flight_rpc,
                            )
                            .await;
                        }
                        Err(error) => {
                            signal_acp_handshake_complete(&handshake_complete);
                            let _ = channels.event_tx.try_send(ConversationEvent::RestoreFailed(
                                classify_restore_failure(&error),
                            ));
                            return Ok(());
                        }
                    }
                }
            }

            // 2.1 的 ActiveSession 已保留 config_options，走官方 builder
            // 就能同时拿到模型档位和 session 路由。
            let session = connection
                .build_session_from(NewSessionRequest::new(std::path::Path::new(&cwd)))
                .block_task()
                .start_session()
                .await?;
            publish_session_surface(
                session.config_options(),
                session.modes(),
                &channels.event_tx,
            );
            signal_acp_handshake_complete(&handshake_complete);
            drive_session(
                session,
                channels,
                ReadyKind::Fresh,
                supports_image,
                in_flight_rpc,
            )
            .await
        })
        .await;
    child_guard.kill_and_reap();
    drop(managed_bun);
    if handshake_watchdog.timed_out() {
        return Err(agent_client_protocol::Error::internal_error().data(format!(
            "ACP 启动握手超时（{} 秒），agent 进程已终止",
            ACP_HANDSHAKE_TIMEOUT.as_secs()
        )));
    }
    result
}

/// 把 `session/load` 响应前已经入队的重放翻译进投影，并记下当前这一轮的形状。
///
/// 只取已经就绪的更新。`poll_once` 拿不到就停，不会把还没到的消息从队列里丢掉。
async fn drain_loaded_replay<S: SessionDrive>(
    session: &mut S,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    replay: &mut ReplayCursor,
) -> Result<(), agent_client_protocol::Error> {
    loop {
        let ready = {
            let read = session.read_update();
            futures::pin_mut!(read);
            smol::future::poll_once(read).await
        };
        match ready {
            Some(update) => {
                let update = update?;
                observe_replay(&update, replay);
                translate_update(update, event_tx).await?;
            }
            None => return Ok(()),
        }
    }
}

/// `session/load` 重放里，最后一条用户消息之后的内容。用来判断交接里的
/// 未完成一轮有哪些已经在会话文件里，避免再贴一份。
#[derive(Default)]
struct ReplayCursor {
    saw_user: bool,
    in_user: bool,
    last_user: String,
    after_user: Vec<ReplaySegment>,
    tool_ids: BTreeSet<String>,
}

enum ReplaySegment {
    Assistant { thought: bool, text: String },
    Tool,
}

impl ReplayCursor {
    fn note_user(&mut self, text: &str) {
        if !self.in_user {
            self.last_user.clear();
            self.after_user.clear();
            self.in_user = true;
            self.saw_user = true;
        }
        self.last_user.push_str(text);
    }

    fn note_assistant(&mut self, thought: bool, text: &str) {
        self.in_user = false;
        if text.is_empty() {
            return;
        }
        match self.after_user.last_mut() {
            Some(ReplaySegment::Assistant {
                thought: same,
                text: buf,
            }) if *same == thought => buf.push_str(text),
            _ => self.after_user.push(ReplaySegment::Assistant {
                thought,
                text: text.to_string(),
            }),
        }
    }

    fn note_tool(&mut self, id: &str) {
        self.in_user = false;
        if id.is_empty() || !self.tool_ids.insert(id.to_string()) {
            return;
        }
        self.after_user.push(ReplaySegment::Tool);
    }
}

fn observe_replay(message: &SessionMessage, replay: &mut ReplayCursor) {
    let SessionMessage::SessionMessage(dispatch) = message else {
        return;
    };
    let Ok(untyped) = dispatch.to_untyped_message() else {
        return;
    };
    if untyped.method() != "session/update" {
        return;
    }
    let params = untyped.params();
    let Some(update) = params.get("update") else {
        return;
    };
    if replay_update_is_nested(params, update) {
        return;
    }
    match update
        .get("sessionUpdate")
        .and_then(serde_json::Value::as_str)
    {
        Some("user_message_chunk") => replay.note_user(update_text(update)),
        Some("agent_message_chunk") => replay.note_assistant(false, update_text(update)),
        Some("agent_thought_chunk") => replay.note_assistant(true, update_text(update)),
        Some("tool_call" | "tool_call_update") => replay.note_tool(update_tool_id(update)),
        _ => {}
    }
}

fn replay_update_is_nested(params: &serde_json::Value, update: &serde_json::Value) -> bool {
    meta_has_parent(params.get("_meta")) || meta_has_parent(update.get("_meta"))
}

fn meta_has_parent(meta: Option<&serde_json::Value>) -> bool {
    parent_tool_id_from_meta(meta.and_then(serde_json::Value::as_object)).is_some()
}

fn update_text(update: &serde_json::Value) -> &str {
    update
        .get("content")
        .and_then(|content| content.get("text"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

fn update_tool_id(update: &serde_json::Value) -> &str {
    update
        .get("toolCallId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
}

/// 已建立会话的最小读写面：官方 `ActiveSession` 和无缝升级的自建路由共用。
trait SessionDrive {
    fn session_id(&self) -> &SessionId;
    fn connection(&self) -> &ConnectionTo<Agent>;
    fn read_update(
        &mut self,
    ) -> impl Future<Output = Result<SessionMessage, agent_client_protocol::Error>> + Send;
}

impl SessionDrive for ActiveSession<'_, Agent> {
    fn session_id(&self) -> &SessionId {
        ActiveSession::session_id(self)
    }

    fn connection(&self) -> &ConnectionTo<Agent> {
        ActiveSession::connection(self)
    }

    fn read_update(
        &mut self,
    ) -> impl Future<Output = Result<SessionMessage, agent_client_protocol::Error>> + Send {
        ActiveSession::read_update(self)
    }
}

/// 驱动一个已建立的会话：发 Ready → 双源 loop（UI 指令 / agent 更新流）。
/// `session/load` 与 `session/new` 共用。
async fn drive_session<S: SessionDrive>(
    mut session: S,
    channels: AcpChannels,
    ready_kind: ReadyKind,
    // 握手时 agent 声明的收图能力（promptCapabilities.image），随 Ready 转给 UI。
    supports_image: bool,
    in_flight_rpc: Arc<AtomicUsize>,
) -> Result<(), agent_client_protocol::Error> {
    let _ = channels.event_tx.try_send(ConversationEvent::Ready {
        session_id: session.session_id().clone(),
        kind: ready_kind,
        supports_image,
    });
    // ACP 的 session config（尤其权限模式）不能可靠地在一个 prompt 进行中
    // 改变：不少 adapter 会把它留到当前 turn 结束，或用 turn 末尾的旧快照
    // 覆盖刚写入的值。这里在协议驱动层排队，保证桌面、移动端和任务入口都
    // 走同一套「本轮结束 → 配置确认 → 下一轮 prompt」顺序。
    let mut turn_active = false;
    let mut pending_config_options: Vec<(String, ConfigValue)> = Vec::new();

    loop {
        // 两个等待源合一：先构造 read_update future，race 决议后它
        // 即被 drop（消息未出队不会丢），借用随之结束——绕开
        // 「cmd 分支也要 &mut session」的借用冲突。
        enum Next {
            Cmd(Option<ConversationCommand>),
            Update(Box<Result<SessionMessage, agent_client_protocol::Error>>),
        }
        let next = {
            let read = session.read_update();
            smol::future::race(
                async { Next::Cmd(channels.cmd_rx.recv().await.ok()) },
                async move { Next::Update(Box::new(read.await)) },
            )
            .await
        };
        match next {
            // 通道关闭（UI 句柄 drop）等同 Shutdown。
            Next::Cmd(None) | Next::Cmd(Some(ConversationCommand::Shutdown)) => {
                return Ok(());
            }
            Next::Cmd(Some(ConversationCommand::PromptSettled(settled))) => {
                // 响应回来时，本轮更早的通知早已按线序进了 SDK 的 update 队列
                // （dispatch handler 是同步 push 的），但循环不一定读完了。
                // 先排空再收尾，否则「回合结束」可能抢在正文前面到 UI。
                //
                // 改用自建请求前，纯文本的 StopReason 是走 update 流的，顺序
                // 由队列天然保证；这次排空是把那份保证还回来，不是新加的保险。
                // 实测（假 LLM 端点跑真回合）抖 5 次都没能触发乱序——窗口很
                // 窄，但正文落到「已结束」回合之后是用户可见且极难查的故障，
                // 不值得用「测不出来」当作不存在。
                drain_ready_updates(&mut session, &channels.event_tx).await?;
                let event = match settled {
                    Ok(stop_reason) => ConversationEvent::TurnEnded(stop_reason),
                    Err(msg) => ConversationEvent::TurnFailed(msg),
                };
                let _ = channels.event_tx.try_send(event);
                turn_active = false;
                flush_pending_config_options(
                    &mut pending_config_options,
                    session.connection().clone(),
                    session.session_id().clone(),
                    &channels.event_tx,
                    &in_flight_rpc,
                )
                .await;
            }
            Next::Cmd(Some(ConversationCommand::Prompt { text, images })) => {
                // 文本和带图走同一条自己拼请求的路径。曾经纯文本走 SDK 的
                // `send_prompt`，但它的回调对错误响应做 `?`：agent 回一个
                // JSON-RPC error（比如「没配 API key」）会让回调任务失败，
                // 进而拖垮整条连接——用户看到的是会话猝死，而 agent 进程还
                // 活着。自己拼请求才能把「本轮失败」和「连接失败」分开。
                //
                // 这里**不能**改成 `block_task().await`：那会卡住整个连接
                // 循环，流式更新一条都收不到。
                let mut blocks: Vec<ContentBlock> = Vec::new();
                if !text.is_empty() {
                    blocks.push(text.into());
                }
                for im in images {
                    blocks.push(ContentBlock::Image(ImageContent::new(im.data_b64, im.mime)));
                }
                let rpc = Arc::clone(&in_flight_rpc);
                let settled_tx = channels.cmd_tx.clone();
                session
                    .connection()
                    .send_request(PromptRequest::new(session.session_id().clone(), blocks))
                    .on_receiving_result(async move |result| {
                        let _guard = RpcCompletionGuard(rpc);
                        let settled = match result {
                            Ok(PromptResponse { stop_reason, .. }) => Ok(stop_reason),
                            Err(e) => Err(describe_prompt_error(&e)),
                        };
                        let _ = settled_tx.try_send(ConversationCommand::PromptSettled(settled));
                        Ok(())
                    })?;
                turn_active = true;
            }
            // 通用 ACP 不该收到 Steer（daemon 按 `supports_mid_turn_input` 过滤）。
            // 万一漏进来，当成新一轮 prompt 会破坏回合闸门，这里只出个提示。
            Next::Cmd(Some(ConversationCommand::Steer { .. })) => {
                let _ = channels.event_tx.try_send(ConversationEvent::Status(
                    "当前智能体不支持运行中插入消息".to_string(),
                ));
            }
            Next::Cmd(Some(
                ConversationCommand::FollowUp { .. } | ConversationCommand::ClearQueue,
            )) => {
                let _ = channels.event_tx.try_send(ConversationEvent::Status(
                    "当前智能体不支持原生消息队列".to_string(),
                ));
            }
            Next::Cmd(Some(ConversationCommand::Compact { .. })) => {
                let _ = channels.event_tx.try_send(ConversationEvent::Status(
                    "当前智能体不支持压缩上下文".to_string(),
                ));
            }
            // daemon 按 `supports_rewind` 过滤，这里只防漏进来。
            Next::Cmd(Some(ConversationCommand::Rewind { .. })) => {
                let _ = channels.event_tx.try_send(ConversationEvent::Status(
                    "当前智能体不支持回退消息".to_string(),
                ));
            }
            Next::Cmd(Some(ConversationCommand::Cancel)) => {
                session
                    .connection()
                    .send_notification(CancelNotification::new(session.session_id().clone()))?;
            }
            // 通用 ACP 没有改会话名的方法；本地标题已经改好，这里无声跳过。
            Next::Cmd(Some(ConversationCommand::SetSessionTitle(_))) => {}
            // 通用 ACP 没有宿主里的后台任务，界面也不会给它出停止按钮。daemon 只把
            // 别的对话的消息投给 Pi 对话。
            Next::Cmd(Some(
                ConversationCommand::StopBackgroundTask { .. }
                | ConversationCommand::StopSubagent { .. }
                | ConversationCommand::DeliverPeerMessage(_),
            )) => {}
            // 通用 ACP 的模型列表由 agent 在 `session/update` 里自己推，没有「重拉」
            // 这个动作；这条命令是给 Pi 那条路径的。
            Next::Cmd(Some(ConversationCommand::RefreshModels)) => {}
            Next::Cmd(Some(ConversationCommand::SetConfigOption { config_id, value })) => {
                if turn_active {
                    queue_config_option(
                        &mut pending_config_options,
                        config_id,
                        value,
                        &in_flight_rpc,
                    );
                } else {
                    send_config_option(
                        session.connection().clone(),
                        session.session_id().clone(),
                        config_id,
                        value,
                        &channels.event_tx,
                        Arc::clone(&in_flight_rpc),
                    )
                    .await;
                }
            }
            Next::Update(update) => {
                let update = (*update)?;
                let completes_prompt = matches!(&update, SessionMessage::StopReason(_));
                translate_update(update, &channels.event_tx).await?;
                if completes_prompt {
                    turn_active = false;
                    finish_rpc(&in_flight_rpc);
                    flush_pending_config_options(
                        &mut pending_config_options,
                        session.connection().clone(),
                        session.session_id().clone(),
                        &channels.event_tx,
                        &in_flight_rpc,
                    )
                    .await;
                }
            }
        }
    }
}

/// 把 update 队列里**已经就绪**的更新全部翻译掉，不等新的。
///
/// 用在回合收尾前：保证已到达的流式正文排在「回合结束」之前发给 UI。
async fn drain_ready_updates<S: SessionDrive>(
    session: &mut S,
    event_tx: &smol::channel::Sender<ConversationEvent>,
) -> Result<(), agent_client_protocol::Error> {
    loop {
        let ready = {
            let read = session.read_update();
            futures::pin_mut!(read);
            smol::future::poll_once(read).await
        };
        match ready {
            Some(update) => translate_update(update?, event_tx).await?,
            None => return Ok(()),
        }
    }
}

/// 回合失败时可以给出的下一步建议。
///
/// 表驱动而不是一串 if：每家 agent 的措辞不同，新增一家只是加一行数据，
/// 不用动匹配逻辑。`needles` 全部命中才算数（都按小写比对），所以可以用
/// 两个词把「没配 key」和「key 不对」这类相邻情况分开。
///
/// 只放**确定能给出动作**的条目。猜错了比不猜更坏：它会把人支到错的地方。
const PROMPT_ERROR_HINTS: &[(&[&str], &str)] = &[
    (
        &["no api key"],
        "还没配 API key：打开 设置 → Agent 对话 → 对应 agent 的「环境变量」，按 `KEY=值` 每行一条填好后新建会话即可（比如 DEEPSEEK_API_KEY=sk-...）。",
    ),
    (
        &["api key", "invalid"],
        "API key 被服务端拒绝了：到 设置 → Agent 对话 → 「环境变量」里核对一遍，注意别把前后空格或引号一起粘进去。",
    ),
    (
        &["econnrefused"],
        "连不上模型服务：如果配了自定义 base URL（如 DEEPSEEK_BASE_URL），到 设置 → Agent 对话 → 「环境变量」确认地址和端口，以及本机能否访问。",
    ),
    (
        &["rate limit"],
        "被服务端限流了，等一会儿再发；持续出现就要看账号配额。",
    ),
];

/// 给错误文本配一条可操作的建议，配不上就不配。
fn prompt_error_hint(text: &str) -> Option<&'static str> {
    let hay = text.to_lowercase();
    PROMPT_ERROR_HINTS
        .iter()
        .find(|(needles, _)| needles.iter().all(|n| hay.contains(n)))
        .map(|(_, hint)| *hint)
}

/// agent 明确回错时给用户看的文本。ACP 的 `Error` 把有用信息分散在 message 和
/// data 两处，只打印其一常常只剩「Internal error」这种废话。
///
/// 认得出来的失败还会附一句下一步做什么——用户报「一发消息就断开」时，
/// 屏幕上一个字都没有，光有英文堆栈也只是从「不知道为什么」变成「不知道该
/// 干什么」。
fn describe_prompt_error(e: &agent_client_protocol::Error) -> String {
    let mut out = e.message.trim().to_string();
    if let Some(data) = &e.data {
        let detail = match data {
            serde_json::Value::String(s) => s.trim().to_string(),
            other => other.to_string(),
        };
        // data 常常是 message 的超集（原样重复一遍），重复了就别显示两次。
        if !detail.is_empty() && !out.contains(&detail) && !detail.contains(&out) {
            out = format!("{out}：{detail}");
        } else if detail.len() > out.len() {
            out = detail;
        }
    }
    if out.is_empty() {
        out = format!("agent 返回错误（{:?}）", e.code);
    }
    match prompt_error_hint(&out) {
        Some(hint) => format!("{out}\n\n{hint}"),
        None => out,
    }
}

struct RpcCompletionGuard(Arc<AtomicUsize>);

impl Drop for RpcCompletionGuard {
    fn drop(&mut self) {
        finish_rpc(&self.0);
    }
}

fn finish_rpc(in_flight_rpc: &AtomicUsize) {
    let _ = in_flight_rpc.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
}

/// 当前 turn 还在运行时只保留同一 config id 的最后一次选择。被覆盖的请求
/// 没有发给 agent，必须同步归还它占用的 in-flight 计数，否则无缝升级会误以为
/// 连接一直有未完成 RPC。
fn queue_config_option(
    pending: &mut Vec<(String, ConfigValue)>,
    config_id: String,
    value: ConfigValue,
    in_flight_rpc: &AtomicUsize,
) {
    if let Some((_, pending_value)) = pending
        .iter_mut()
        .find(|(pending_id, _)| pending_id == &config_id)
    {
        *pending_value = value;
        finish_rpc(in_flight_rpc);
    } else {
        pending.push((config_id, value));
    }
}

fn config_option_wire_value(value: ConfigValue) -> SessionConfigOptionValue {
    match value {
        ConfigValue::Boolean(flag) => SessionConfigOptionValue::boolean(flag),
        ConfigValue::Select(id) => SessionConfigValueId::new(id).into(),
    }
}

/// 发送一个已经到安全边界的配置变更。响应里的全量 config options 是唯一可信
/// 的确认，不能在客户端收到点击时直接把 current value 当成已生效。
async fn send_config_option(
    connection: ConnectionTo<Agent>,
    session_id: SessionId,
    config_id: String,
    value: ConfigValue,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    in_flight_rpc: Arc<AtomicUsize>,
) {
    let _guard = RpcCompletionGuard(in_flight_rpc);
    let req = SetSessionConfigOptionRequest::new(
        session_id,
        SessionConfigId::new(config_id),
        config_option_wire_value(value),
    );
    match connection.send_request(req).block_task().await {
        Ok(resp) => {
            publish_config_options(Some(&resp.config_options), event_tx);
        }
        Err(e) => {
            let _ = event_tx.try_send(ConversationEvent::Status(format!("更新会话配置失败：{e}")));
        }
    }
}

async fn flush_pending_config_options(
    pending: &mut Vec<(String, ConfigValue)>,
    connection: ConnectionTo<Agent>,
    session_id: SessionId,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    in_flight_rpc: &Arc<AtomicUsize>,
) {
    let pending = std::mem::take(pending);
    for (config_id, value) in pending {
        send_config_option(
            connection.clone(),
            session_id.clone(),
            config_id,
            value,
            event_tx,
            Arc::clone(in_flight_rpc),
        )
        .await;
    }
}

/// 握手时声明的 client 能力。声明了就必须实现对应方法，否则 agent 一调用就会挂起。
pub fn smelt_client_capabilities() -> ClientCapabilities {
    let mut meta = serde_json::Map::new();
    meta.insert(
        "subagent-transcript".to_string(),
        serde_json::Value::Bool(true),
    );
    ClientCapabilities::default()
        .fs(FileSystemCapabilities::new()
            .read_text_file(true)
            .write_text_file(true))
        .terminal(true)
        .session(
            ClientSessionCapabilities::default().config_options(
                SessionConfigOptionsCapabilities::default()
                    .boolean(BooleanConfigOptionCapabilities::new()),
            ),
        )
        .elicitation(
            ElicitationCapabilities::default()
                .form(ElicitationFormCapabilities::default())
                .url(ElicitationUrlCapabilities::new()),
        )
        .plan(PlanCapabilities::new())
        .meta(meta)
}

fn vendor_ext_method(method: &str) -> &str {
    method.strip_prefix('_').unwrap_or(method)
}

fn detect_shell_type() -> String {
    std::env::var("SHELL")
        .ok()
        .as_deref()
        .and_then(|shell| std::path::Path::new(shell).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("zsh")
        .to_string()
}

/// `kiro-cli chat _ get-kas-token` 的 stdout 是 `{kind,data}` 信封；ACP 扩展要的是
/// data 里那份 token 对象。测试用假信封，绝不把真实 token 写进断言。
pub(crate) fn kiro_token_from_cli_output(stdout: &str) -> Result<serde_json::Value, String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim())
        .map_err(|err| format!("解析 kiro token 输出失败：{err}"))?;
    match value.get("kind").and_then(|kind| kind.as_str()) {
        Some("error") => {
            let message = value
                .get("data")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Err(format!("kiro 取 token 失败：{message}"))
        }
        Some("getKasToken") => value
            .get("data")
            .cloned()
            .ok_or_else(|| "kiro token 输出缺少 data".to_string()),
        _ if value.get("accessToken").is_some() => Ok(value),
        _ => Err("kiro token 输出无法识别".to_string()),
    }
}

fn kiro_cli_get_kas_token(force_refresh: bool) -> Result<serde_json::Value, String> {
    let search_path = extended_search_path();
    let program = resolve_in_path("kiro-cli", &search_path).unwrap_or_else(|| "kiro-cli".into());
    let mut command = std::process::Command::new(program);
    crate::login_env::apply_login_environment(&mut command);
    command.args(["chat", "_", "get-kas-token"]);
    if force_refresh {
        command.arg("--force-refresh");
    }
    let output = command
        .output()
        .map_err(|err| format!("启动 kiro-cli get-kas-token 失败：{err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "kiro-cli get-kas-token 退出 {}：{}",
            output.status,
            stderr.trim()
        ));
    }
    kiro_token_from_cli_output(&String::from_utf8_lossy(&output.stdout))
}

pub(crate) fn vendor_ext_result(method: &str, params: &str) -> Result<serde_json::Value, String> {
    match vendor_ext_method(method) {
        "kiro/auth/getAccessToken" => {
            let parsed: serde_json::Value =
                serde_json::from_str(params).unwrap_or(serde_json::json!({}));
            let force_refresh = parsed
                .get("forceRefresh")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            kiro_cli_get_kas_token(force_refresh)
        }
        "kiro/terminal/shell_type" => Ok(serde_json::json!({
            "shellType": detect_shell_type()
        })),
        _ => Ok(serde_json::json!({})),
    }
}

async fn respond_vendor_ext_request(
    request: AgentRequest,
    responder: Responder<serde_json::Value>,
) -> Result<(), agent_client_protocol::Error> {
    let AgentRequest::ExtMethodRequest(request) = request else {
        return Err(acp_err("未处理的 ACP 扩展请求"));
    };
    let method = request.method.to_string();
    let params = request.params.get().to_string();
    let value = smol::unblock(move || vendor_ext_result(&method, &params))
        .await
        .map_err(acp_err)?;
    let _ = responder.respond(value);
    Ok(())
}

/// Claude ACP 把子代理事件挂在 `_meta.claudeCode.parentToolUseId`；其它 adapter
/// 也可能把同样的键放在 `_meta` 顶层。找不到就当作顶层事件。
pub fn parent_tool_id_from_meta(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<String> {
    let meta = meta?;
    if let Some(id) = json_string_path(meta, &["claudeCode", "parentToolUseId"]) {
        return Some(id);
    }
    if let Some(id) = json_string_path(meta, &["parentToolUseId"]) {
        return Some(id);
    }
    json_string_path(meta, &["parent_tool_use_id"])
}

fn plan_from_update(update: PlanUpdate) -> Plan {
    match update.plan {
        PlanUpdateContent::Items(items) => Plan::new(items.entries),
        PlanUpdateContent::Markdown(markdown) => Plan::new(vec![PlanEntry::new(
            markdown.content,
            PlanEntryPriority::Medium,
            PlanEntryStatus::Pending,
        )]),
        PlanUpdateContent::File(file) => Plan::new(vec![PlanEntry::new(
            file.uri,
            PlanEntryPriority::Medium,
            PlanEntryStatus::Pending,
        )]),
        _ => Plan::new(Vec::new()),
    }
}

fn json_string_path(
    object: &serde_json::Map<String, serde_json::Value>,
    path: &[&str],
) -> Option<String> {
    let mut current = object;
    for (ix, key) in path.iter().enumerate() {
        let value = current.get(*key)?;
        if ix + 1 == path.len() {
            return value.as_str().filter(|s| !s.is_empty()).map(str::to_string);
        }
        current = value.as_object()?;
    }
    None
}

/// ACP 的 title 是三态：字段缺失表示“不更新”，null 表示“清空”，字符串表示
/// “替换”。空白字符串没有展示价值，按清空处理。
fn session_title_update(title: MaybeUndefined<String>) -> Option<Option<String>> {
    match title {
        MaybeUndefined::Undefined => None,
        MaybeUndefined::Null => Some(None),
        MaybeUndefined::Value(title) => {
            let title = title.trim();
            Some((!title.is_empty()).then(|| title.to_string()))
        }
    }
}

/// 把 agent 的一条更新翻译成 ConversationEvent（不认识的一律忽略——协议会长新枝）。
async fn translate_update(
    message: SessionMessage,
    event_tx: &smol::channel::Sender<ConversationEvent>,
) -> Result<(), agent_client_protocol::Error> {
    match message {
        SessionMessage::SessionMessage(dispatch) => {
            MatchDispatch::new(dispatch)
                .if_notification(async |notif: SessionNotification| {
                    let notif_parent = parent_tool_id_from_meta(notif.meta.as_ref());
                    let event = match notif.update {
                        SessionUpdate::AgentMessageChunk(chunk) => {
                            Some(ConversationEvent::AgentChunk {
                                thought: false,
                                text: content_text(&chunk.content),
                                parent_id: parent_tool_id_from_meta(chunk.meta.as_ref())
                                    .or(notif_parent),
                            })
                        }
                        SessionUpdate::AgentThoughtChunk(chunk) => {
                            Some(ConversationEvent::AgentChunk {
                                thought: true,
                                text: content_text(&chunk.content),
                                parent_id: parent_tool_id_from_meta(chunk.meta.as_ref())
                                    .or(notif_parent),
                            })
                        }
                        SessionUpdate::ToolCall(tc) => Some(ConversationEvent::ToolCall(tc)),
                        SessionUpdate::ToolCallUpdate(u) => {
                            Some(ConversationEvent::ToolCallUpdate(u))
                        }
                        SessionUpdate::UserMessageChunk(chunk) => match chunk.content {
                            ContentBlock::Image(image) => Some(ConversationEvent::UserImage(
                                PromptImage::new(image.mime_type, image.data),
                            )),
                            content => Some(ConversationEvent::UserChunk(content_text(&content))),
                        },
                        SessionUpdate::AvailableCommandsUpdate(u) => {
                            Some(ConversationEvent::AvailableCommands(
                                u.available_commands
                                    .into_iter()
                                    .map(|c| (c.name, c.description))
                                    .collect(),
                            ))
                        }
                        // 上下文用量：used/size 是 token 数，UI 换算成百分比。
                        SessionUpdate::UsageUpdate(u) => Some(ConversationEvent::Usage {
                            used: u.used,
                            size: u.size,
                            cached_read: None,
                            cost: None,
                            breakdown: None,
                        }),
                        // 计划（步骤清单）：透传给 UI 渲染 PLAN 条。
                        SessionUpdate::Plan(p) => Some(ConversationEvent::Plan(p)),
                        SessionUpdate::PlanUpdate(update) => {
                            Some(ConversationEvent::Plan(plan_from_update(update)))
                        }
                        SessionUpdate::PlanRemoved(_) => {
                            Some(ConversationEvent::Plan(Plan::new(Vec::new())))
                        }
                        // 会话配置变了（用户在 agent 侧换了模型、模式等）：全量刷新。
                        SessionUpdate::ConfigOptionUpdate(u) => {
                            publish_config_options(Some(&u.config_options), event_tx);
                            None
                        }
                        SessionUpdate::SessionInfoUpdate(update) => {
                            session_title_update(update.title).map(ConversationEvent::SessionTitle)
                        }
                        _ => None,
                    };
                    if let Some(ev) = event {
                        let _ = event_tx.try_send(ev);
                    }
                    Ok(())
                })
                .await
                .otherwise_ignore()?;
        }
        SessionMessage::StopReason(reason) => {
            let _ = event_tx.try_send(ConversationEvent::TurnEnded(reason));
        }
        _ => {} // SessionMessage #[non_exhaustive]
    }
    Ok(())
}

/// 组装 ACP agent 的 stdio 配置（不 spawn）：命令按空白分词，注入 login shell 的 PATH
/// （Finder 启动的 GUI 进程 PATH 不含 nvm/homebrew，直接 spawn `npx` 会
/// ENOENT）。先借 SDK 解析成标准 stdio 配置，再由本文件自己 spawn；这样能拿到
/// agent 子进程的 pid，也能让 `run_connection` 统一接管 stderr 尾巴和原始行捕获。
/// 网页 agent 的临时环境变量（ephemeral_env）在组装参数时叠加进子进程环境，但不会写回可持久化的 launch。
fn build_agent(
    launch: &ConversationLaunchSpec,
    ephemeral_env: &BTreeMap<String, String>,
) -> Result<AcpAgentConfig, agent_client_protocol::Error> {
    // 查找命令用 login PATH + 一批常见 CLI 安装目录兜底。为什么要兜底：
    // login_env 的探测走 `-ilc`（交互 login，能读到 ~/.zshrc 里的 PATH），但
    // 3 秒超时熔断后可能拿不到（慢配置/挂起配置），且各家 CLI 的安装脚本未必
    // 把 PATH 写进 shell 配置。这里补搜标准安装位（尤其 grok 的 ~/.grok/bin），
    // 装了就一定找得到；真没装的才落到下面的友好提示。子进程 PATH 也用这份
    // 扩展，免得 agent 起来后找它自己的子工具又缺路径。
    let search_path = extended_search_path();
    // 命令字符串允许开头带 shell 风格的 `VAR=value` 前缀（比如
    // `CLAUDE_CONFIG_DIR=~/.claude-quant claude --dangerously-skip-permissions`，
    // 让同一家 agent 的多个 workspace 各开一条「设置 → Agent 集成」里的独立
    // 启动命令）——`AcpAgent::from_args` 本来就认这个语法（内部 parse_env_var
    // 逐个 token 解析，遇到第一个不是 `VAR=value` 形状的 token 才当作程序名），
    // 这里的 PATH 注入用的正是同一条路。真正要做的是"先把这些前缀跳过去找到
    // 真正的程序名"，不然会把 `CLAUDE_CONFIG_DIR=...` 整个当成程序名去查 PATH，
    // 报"未找到命令"（这是之前真实的行为，不是假设）。
    let args = build_agent_args_with_ephemeral_env(launch, ephemeral_env, &search_path)?;
    Ok(AcpAgent::from_args(args.iter().map(String::as_str))?.into_config())
}

/// 在指定 worktree 中启动 ACP agent，并保留低层 stdio / child 句柄给连接线程。
///
/// `AcpAgent::spawn_process()` 没有 current_dir 参数，直接调用会让 agent 继承
/// smeltd 的工作目录；controller 创建的隔离 worktree 因此只对协议可见、对 CLI 自己的
/// 相对路径不可见。这里复刻 SDK 的安全 spawn 约定，并额外设置 current_dir。
fn spawn_agent_process(
    config: &AcpAgentConfig,
    cwd: Option<&str>,
    managed_bun: Option<&crate::managed_runtime::ManagedBunRuntime>,
) -> Result<
    (
        async_process::ChildStdin,
        async_process::ChildStdout,
        async_process::ChildStderr,
        async_process::Child,
    ),
    agent_client_protocol::Error,
> {
    let mut std_cmd = std::process::Command::new(config.command());
    std_cmd.args(config.arguments());
    // 先铺登录 shell 的完整环境作为基底。只传 PATH 不够：用户的 node 可能来自
    // volta/asdf/mise 这类 shim 式版本管理器，PATH 里那个 `node` 只是转发器，
    // 真正的版本决策靠 `VOLTA_HOME` 一类的环境变量；代理设置同理。GUI 是
    // Finder 拉起的，这些变量一个都没有，于是"终端里跑得动、点图标跑不动"。
    // 放在 config.env 之前，让启动规格里显式写的 `VAR=value` 前缀（含 PATH）
    // 始终覆盖它。
    crate::login_env::apply_login_environment(&mut std_cmd);
    for (name, value) in config.environment() {
        std_cmd.env(name, value);
    }
    if let Some(cwd) = cwd.filter(|cwd| !cwd.trim().is_empty()) {
        std_cmd.current_dir(cwd);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;

        // 与 agent-client-protocol 的 AcpAgent::spawn_process 一致：wrapper
        // （npx/uvx）以及它派生的真实 agent 必须属于同一进程组，结束会话时不留孤儿。
        std_cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std_cmd.creation_flags(
            smelt_sys::process::CREATE_SUSPENDED | smelt_sys::process::CREATE_NO_WINDOW,
        );
    }
    if let Some(runtime) = managed_bun {
        runtime.inherit_into(&mut std_cmd);
    }
    let mut cmd = async_process::Command::from(std_cmd);
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| agent_client_protocol::Error::internal_error().data(e.to_string()))?;
    #[cfg(windows)]
    if let Err(error) = smelt_sys::process::attach_suspended(child.id()) {
        drop(child);
        return Err(agent_client_protocol::Error::internal_error().data(format!(
            "无法把 agent 放进进程树：{error}"
        )));
    }
    let child_stdin = child.stdin.take().ok_or_else(|| {
        agent_client_protocol::Error::internal_error().data("无法打开 agent stdin")
    })?;
    let child_stdout = child.stdout.take().ok_or_else(|| {
        agent_client_protocol::Error::internal_error().data("无法打开 agent stdout")
    })?;
    let child_stderr = child.stderr.take().ok_or_else(|| {
        agent_client_protocol::Error::internal_error().data("无法打开 agent stderr")
    })?;
    Ok((child_stdin, child_stdout, child_stderr, child))
}

#[cfg(test)]
fn build_agent_args(
    launch: &ConversationLaunchSpec,
    search_path: &str,
) -> Result<Vec<String>, agent_client_protocol::Error> {
    build_agent_args_with_ephemeral_env(launch, &BTreeMap::new(), search_path)
}

fn build_agent_args_with_ephemeral_env(
    launch: &ConversationLaunchSpec,
    ephemeral_env: &BTreeMap<String, String>,
    search_path: &str,
) -> Result<Vec<String>, agent_client_protocol::Error> {
    let mut tokens = launch.command.split_whitespace();
    let mut user_env = BTreeMap::<String, String>::new();
    let mut prog_token = None;
    for tok in tokens.by_ref() {
        match crate::workspace_override::split_env_assignment(tok) {
            Some((name, value)) => {
                user_env.insert(
                    name.to_string(),
                    crate::workspace_override::expand_tilde(value),
                );
            }
            None => {
                prog_token = Some(tok);
                break;
            }
        }
    }
    for (name, value) in &launch.env {
        user_env.insert(name.clone(), crate::workspace_override::expand_tilde(value));
    }
    // controller agent 配置在最后覆盖本地 profile / 启动命令中的同名变量，和
    // daemon 的 custom_env overlay 语义一致；它只随着这一次启动走，不会写回
    // `launch.env` 或任何会话存档。
    for (name, value) in ephemeral_env {
        user_env.insert(name.clone(), value.clone());
    }
    inject_local_adapter_cli_path(
        &launch.command,
        "@agentclientprotocol/codex-acp",
        "CODEX_PATH",
        "codex",
        &mut user_env,
        search_path,
    );
    inject_local_adapter_cli_path(
        &launch.command,
        "@agentclientprotocol/claude-agent-acp",
        "CLAUDE_CODE_EXECUTABLE",
        "claude",
        &mut user_env,
        search_path,
    );
    let resolved: Vec<String> = match prog_token {
        Some(prog) => {
            let prog = if prog.contains('/') {
                prog.to_string()
            } else {
                resolve_in_path(prog, search_path).ok_or_else(|| {
                    agent_client_protocol::Error::internal_error().data(format!(
                        "未找到命令 `{prog}`。这类对话 agent 是各自独立的 CLI，需要先自行\
                         安装并登录。如果确定装了，多半是它的目录没进登录 shell 的 PATH——\
                         可在「设置 → Agent 集成」把启动命令换成绝对路径（如 \
                         `~/.grok/bin/{prog} …`）。"
                    ))
                })?
            };
            std::iter::once(prog)
                .chain(tokens.map(String::from))
                .collect()
        }
        None => Vec::new(),
    };
    let mut args = vec![format!("PATH={search_path}")];
    args.extend(
        user_env
            .into_iter()
            .map(|(name, value)| format!("{name}={value}")),
    );
    args.extend(resolved);
    Ok(args)
}

/// 官方 ACP 适配器有时会携带自己的 agent 运行时；优先使用扩展 PATH 中找到的
/// 本机 CLI，避免模型、能力或登录态落在另一份过期运行时上。用户通过启动规格或
/// 环境显式指定时保持原样，找不到本机 CLI 则让适配器自行回退。
fn inject_local_adapter_cli_path(
    command: &str,
    adapter_package: &str,
    env_var: &str,
    program: &str,
    user_env: &mut BTreeMap<String, String>,
    search_path: &str,
) {
    if !command.contains(adapter_package)
        || user_env.contains_key(env_var)
        || std::env::var_os(env_var).is_some()
    {
        return;
    }
    if let Some(path) = resolve_in_path(program, search_path) {
        user_env.insert(env_var.to_string(), path);
    }
}

/// Ask Claude's adapter to include raw SDK messages so usage/cache-token
/// details remain available when history is reconstructed by session/load.
fn claude_raw_sdk_meta(cmd: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    if !cmd.contains("claude") {
        return None;
    }
    let mut inner = serde_json::Map::new();
    inner.insert(
        "emitRawSDKMessages".to_string(),
        serde_json::Value::Bool(true),
    );
    let mut meta = serde_json::Map::new();
    meta.insert("claudeCode".to_string(), serde_json::Value::Object(inner));
    Some(meta)
}

/// 从会话配置项里挑出「当前模型」的人类可读名。
///
/// 协议把模型建模成一条 `category = Model` 的 select 配置项：`current_value`
/// 是值 id，`options` 里同 id 那条的 `name` 才是给人看的名字（如
/// `Claude Sonnet 4.5`）。找不到对应选项就退回值 id 本身——显示 `sonnet-4.5`
/// 也比显示适配器包名强。
pub(crate) fn model_from_config(options: &[SessionConfigOption]) -> Option<ModelState> {
    let opt = options
        .iter()
        .find(|o| matches!(o.category, Some(SessionConfigOptionCategory::Model)))?;
    let SessionConfigKind::Select(sel) = &opt.kind else {
        return None;
    };
    let cur = &sel.current_value;
    // 选项可能是平铺的，也可能按厂商/档位分组，两种都要翻。
    let (flat, provider_groups): (
        Vec<&agent_client_protocol::schema::v1::SessionConfigSelectOption>,
        Vec<ModelProviderGroup>,
    ) = match &sel.options {
        SessionConfigSelectOptions::Ungrouped(v) => (v.iter().collect(), Vec::new()),
        SessionConfigSelectOptions::Grouped(groups) => (
            groups
                .iter()
                .flat_map(|group| group.options.iter())
                .collect(),
            groups
                .iter()
                .map(|group| ModelProviderGroup {
                    id: group.group.to_string(),
                    name: group.name.clone(),
                    options: group
                        .options
                        .iter()
                        .map(|option| (option.value.to_string(), option.name.clone()))
                        .collect(),
                })
                .collect(),
        ),
        _ => (Vec::new(), Vec::new()), // schema #[non_exhaustive]，协议会长新枝
    };
    let name = flat
        .iter()
        .find(|o| &o.value == cur)
        .map(|o| o.name.clone())
        .unwrap_or_else(|| cur.to_string());
    if name.trim().is_empty() {
        return None;
    }
    let options = flat
        .iter()
        .map(|o| (o.value.to_string(), o.name.clone()))
        .collect();
    Some(ModelState {
        config_id: opt.id.to_string(),
        current_value: cur.to_string(),
        current_name: name,
        options,
        provider_groups,
    })
}

/// 将 agent 声明的会话配置收敛成视图状态。select 保留选项列表；boolean 投影成
/// 「开 / 关」两项，写入时走 `type: boolean`。
pub(crate) fn session_configs_from_config(
    options: &[SessionConfigOption],
) -> Vec<SessionConfigState> {
    options
        .iter()
        .filter(|opt| !matches!(opt.category, Some(SessionConfigOptionCategory::Model)))
        .filter_map(|opt| match &opt.kind {
            SessionConfigKind::Select(sel) => {
                let flat: Vec<&agent_client_protocol::schema::v1::SessionConfigSelectOption> =
                    match &sel.options {
                        SessionConfigSelectOptions::Ungrouped(v) => v.iter().collect(),
                        SessionConfigSelectOptions::Grouped(gs) => {
                            gs.iter().flat_map(|g| g.options.iter()).collect()
                        }
                        _ => Vec::new(),
                    };
                let current_name = flat
                    .iter()
                    .find(|o| o.value == sel.current_value)
                    .map(|o| o.name.clone())
                    .unwrap_or_else(|| sel.current_value.to_string());
                (!current_name.trim().is_empty()).then(|| SessionConfigState {
                    config_id: opt.id.to_string(),
                    name: opt.name.clone(),
                    description: opt.description.clone(),
                    current_name,
                    options: flat
                        .iter()
                        .map(|o| (o.value.to_string(), o.name.clone()))
                        .collect(),
                    boolean: None,
                })
            }
            SessionConfigKind::Boolean(flag) => Some(SessionConfigState {
                config_id: opt.id.to_string(),
                name: opt.name.clone(),
                description: opt.description.clone(),
                current_name: if flag.current_value {
                    "开".to_string()
                } else {
                    "关".to_string()
                },
                options: vec![("true".into(), "开".into()), ("false".into(), "关".into())],
                boolean: Some(flag.current_value),
            }),
            _ => None,
        })
        .collect()
}

fn publish_config_options(
    options: Option<&[SessionConfigOption]>,
    event_tx: &smol::channel::Sender<ConversationEvent>,
) {
    let Some(options) = options else { return };
    // 空列表多半是解析失败被 DefaultOnError 吞成 []。不能用它覆盖已经画上
    // 的 Mode/Model：Kiro 会先在 session/new 给 mode，再异步补 model。
    if options.is_empty() {
        return;
    }
    if let Some(model) = model_from_config(options) {
        let _ = event_tx.try_send(ConversationEvent::Model(model));
    }
    let _ = event_tx.try_send(ConversationEvent::ConfigOptions(
        session_configs_from_config(options),
    ));
}

fn config_has_id_or_category(
    options: &[SessionConfigOption],
    id: &str,
    category: SessionConfigOptionCategory,
) -> bool {
    options
        .iter()
        .any(|opt| opt.id.to_string() == id || opt.category.as_ref() == Some(&category))
}

fn session_mode_config(modes: &SessionModeState) -> Option<SessionConfigOption> {
    let options: Vec<SessionConfigSelectOption> = modes
        .available_modes
        .iter()
        .map(|mode| {
            let mut option = SessionConfigSelectOption::new(
                SessionConfigValueId::new(mode.id.to_string()),
                mode.name.clone(),
            );
            if let Some(description) = mode.description.clone() {
                option = option.description(description);
            }
            option
        })
        .collect();
    (options.len() > 1).then(|| {
        SessionConfigOption::select(
            SessionConfigId::new("mode".to_string()),
            "Mode".to_string(),
            SessionConfigValueId::new(modes.current_mode_id.to_string()),
            SessionConfigSelectOptions::Ungrouped(options),
        )
        .category(SessionConfigOptionCategory::Mode)
    })
}

fn select_option_values(option: &SessionConfigOption) -> Option<Vec<String>> {
    let SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };
    Some(match &select.options {
        SessionConfigSelectOptions::Ungrouped(options) => options
            .iter()
            .map(|option| option.value.to_string())
            .collect(),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter())
            .map(|option| option.value.to_string())
            .collect(),
        _ => Vec::new(),
    })
}

/// Some adapters expose the same selector twice: once through the ACP `modes` field and
/// once as a `configOptions` entry (Pi currently calls the latter `thought_level`). Keep
/// the explicit config option, which has the correct provider-specific id for writes, and
/// only synthesize `mode` when the two surfaces are genuinely different.
fn config_option_matches_modes(option: &SessionConfigOption, modes: &SessionModeState) -> bool {
    // Only a thought-level selector can be the duplicate Pi surface. A model or
    // vendor-specific selector with coincidentally similar values must not hide real modes.
    if option.id.to_string() != "thought_level"
        && option.category.as_ref() != Some(&SessionConfigOptionCategory::ThoughtLevel)
    {
        return false;
    }
    let SessionConfigKind::Select(select) = &option.kind else {
        return false;
    };
    if select.current_value.to_string() != modes.current_mode_id.to_string() {
        return false;
    }
    let values = select_option_values(option).unwrap_or_default();
    values.len() == modes.available_modes.len()
        && modes
            .available_modes
            .iter()
            .all(|mode| values.iter().any(|value| value == &mode.id.to_string()))
}

fn mode_surface_present(options: &[SessionConfigOption], modes: Option<&SessionModeState>) -> bool {
    config_has_id_or_category(options, "mode", SessionConfigOptionCategory::Mode)
        || modes.is_some_and(|modes| {
            options
                .iter()
                .any(|option| config_option_matches_modes(option, modes))
        })
}

fn merge_session_config_options(
    config_options: Option<&[SessionConfigOption]>,
    modes: Option<&SessionModeState>,
) -> Vec<SessionConfigOption> {
    let mut options = config_options.unwrap_or(&[]).to_vec();
    if !mode_surface_present(&options, modes)
        && let Some(mode) = modes.and_then(session_mode_config)
    {
        options.insert(0, mode);
    }
    options
}

fn publish_session_surface(
    config_options: Option<&[SessionConfigOption]>,
    modes: Option<&SessionModeState>,
    event_tx: &smol::channel::Sender<ConversationEvent>,
) {
    let options = merge_session_config_options(config_options, modes);
    publish_config_options(
        (!options.is_empty()).then_some(options.as_slice()),
        event_tx,
    );
}

fn raw_config_options(value: &serde_json::Value) -> Vec<SessionConfigOption> {
    value
        .get("configOptions")
        .or_else(|| value.get("config_options"))
        .and_then(|raw| serde_json::from_value::<Vec<SessionConfigOption>>(raw.clone()).ok())
        .unwrap_or_default()
}

fn raw_model_config(value: &serde_json::Value) -> Option<SessionConfigOption> {
    let models = value.get("models")?;
    let current = models
        .get("currentModelId")
        .or_else(|| models.get("current_model_id"))?
        .as_str()?;
    let available = models
        .get("availableModels")
        .or_else(|| models.get("available_models"))?
        .as_array()?;
    let options: Vec<SessionConfigSelectOption> = available
        .iter()
        .filter_map(|model| {
            let id = model
                .get("modelId")
                .or_else(|| model.get("model_id"))
                .or_else(|| model.get("id"))?
                .as_str()?;
            let name = model
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap_or(id);
            Some(SessionConfigSelectOption::new(
                SessionConfigValueId::new(id.to_string()),
                name.to_string(),
            ))
        })
        .collect();
    (options.len() > 1).then(|| {
        SessionConfigOption::select(
            SessionConfigId::new("model".to_string()),
            "Model".to_string(),
            SessionConfigValueId::new(current.to_string()),
            SessionConfigSelectOptions::Ungrouped(options),
        )
        .category(SessionConfigOptionCategory::Model)
    })
}

fn options_from_raw_session_result(result: &serde_json::Value) -> Vec<SessionConfigOption> {
    let mut options = raw_config_options(result);
    let modes = result
        .get("modes")
        .and_then(|value| serde_json::from_value::<SessionModeState>(value.clone()).ok());
    if !mode_surface_present(&options, modes.as_ref())
        && let Some(mode) = modes.as_ref().and_then(session_mode_config)
    {
        options.insert(0, mode);
    }
    if !config_has_id_or_category(&options, "model", SessionConfigOptionCategory::Model)
        && let Some(model) = raw_model_config(result)
    {
        options.insert(0, model);
    }
    options
}

pub(crate) fn session_config_options_from_rpc_line(line: &str) -> Option<Vec<SessionConfigOption>> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if let Some(result) = value.get("result")
        && result
            .get("sessionId")
            .or_else(|| result.get("session_id"))
            .is_some()
    {
        let options = options_from_raw_session_result(result);
        return (!options.is_empty()).then_some(options);
    }
    let update = value.pointer("/params/update")?;
    if update.get("sessionUpdate").and_then(|kind| kind.as_str()) != Some("config_option_update") {
        return None;
    }
    let options = raw_config_options(update);
    (!options.is_empty()).then_some(options)
}

fn publish_raw_session_config_line(
    line: &str,
    event_tx: &smol::channel::Sender<ConversationEvent>,
) {
    if let Some(options) = session_config_options_from_rpc_line(line) {
        publish_config_options(Some(&options), event_tx);
    }
}

/// ACP 审批请求的问题和详情。
fn permission_from_request(
    request: &RequestPermissionRequest,
) -> (String, crate::acp_session::ApprovalDetailsView) {
    let fields = &request.tool_call.fields;
    let paths: Vec<String> = fields
        .locations
        .iter()
        .flatten()
        .map(|location| location.path.to_string_lossy().into_owned())
        .collect();
    permission_view(
        fields.title.as_deref(),
        &request.tool_call.tool_call_id.to_string(),
        fields.kind,
        fields.raw_input.as_ref(),
        &paths,
    )
}

/// 审批卡要让人看得出批的是什么：工具标题（「Fetch URL」「Run shell」）之外，带上这次
/// 调用的对象——命令、网址、文件。能认出是执行命令或改文件的，给专门的详情样式；其余的
/// 把对象接在标题后面。请求里的字段是稀疏的，没带参数时调用方用之前那条 tool_call 的
/// `raw_input` 再算一次（见 `acp_session::apply` 的审批分支）。
pub(crate) fn permission_view(
    title: Option<&str>,
    tool_call_id: &str,
    kind: Option<agent_client_protocol::schema::v1::ToolKind>,
    raw_input: Option<&serde_json::Value>,
    paths: &[String],
) -> (String, crate::acp_session::ApprovalDetailsView) {
    use crate::acp_session::ApprovalDetailsView;
    use agent_client_protocol::schema::v1::ToolKind as Acp;

    /// 网址、命令过长时只留开头，卡片和系统通知都放得下。
    const TARGET_MAX_CHARS: usize = 300;

    let title = title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("工具调用 {tool_call_id}"));
    let input = raw_input.and_then(serde_json::Value::as_object);
    let text = |keys: &[&str]| {
        let input = input?;
        keys.iter()
            .find_map(|key| input.get(*key).and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let command = input
        .and_then(|input| input.get("command"))
        .and_then(|command| match command {
            serde_json::Value::String(command) => {
                Some(command.trim().to_string()).filter(|command| !command.is_empty())
            }
            serde_json::Value::Array(words) => {
                let words: Vec<&str> = words.iter().filter_map(serde_json::Value::as_str).collect();
                (!words.is_empty()).then(|| words.join(" "))
            }
            _ => None,
        });
    let path = text(&["file_path", "path", "notebook_path"]).or_else(|| paths.first().cloned());

    let details = match kind {
        Some(Acp::Execute) if command.is_some() => ApprovalDetailsView::Command {
            command: command.clone().unwrap_or_default(),
            cwd: text(&["cwd", "workdir"]),
            reason: None,
        },
        Some(Acp::Edit | Acp::Delete | Acp::Move) if path.is_some() => {
            ApprovalDetailsView::FileChange {
                reason: None,
                grant_root: path.clone(),
            }
        }
        _ => ApprovalDetailsView::Generic,
    };
    let target = command
        .or_else(|| text(&["url", "uri"]))
        .or(path)
        .or_else(|| text(&["pattern", "query"]));
    let question = match target {
        Some(target) if !title.contains(&target) => {
            let mut clipped: String = target.chars().take(TARGET_MAX_CHARS).collect();
            if clipped.len() < target.len() {
                clipped.push('…');
            }
            format!("{title}：{clipped}")
        }
        _ => title,
    };
    (question, details)
}

/// ContentBlock 文本化：MVP 只取文本，资源/图片降级为占位（方案「已知不做」）。
pub fn content_text(content: &ContentBlock) -> String {
    match content {
        ContentBlock::Text(t) => t.text.clone(),
        ContentBlock::Image(_) => "[图片]".to_string(),
        ContentBlock::Audio(_) => "[音频]".to_string(),
        ContentBlock::ResourceLink(l) => format!("[资源 {}]", l.uri),
        ContentBlock::Resource(_) => "[内嵌资源]".to_string(),
        _ => "[未知内容]".to_string(), // schema #[non_exhaustive]，协议会长新枝
    }
}

/// agent_client_protocol 的协议类型 → `acp_chat` 共享模型类型。原来住在
/// acp_view.rs（GUI 层），跟着 `apply_event` 归约逻辑一起搬进 acp_session——
/// 谁跑归约谁就要做这层翻译，现在是 smeltd 不是 GUI。
pub fn tool_kind_from_acp(
    k: agent_client_protocol::schema::v1::ToolKind,
) -> crate::acp_chat::ToolKind {
    use crate::acp_chat::ToolKind;
    use agent_client_protocol::schema::v1::ToolKind as Acp;
    match k {
        Acp::Read => ToolKind::Read,
        Acp::Edit => ToolKind::Edit,
        Acp::Delete => ToolKind::Delete,
        Acp::Move => ToolKind::Move,
        Acp::Search => ToolKind::Search,
        Acp::Execute => ToolKind::Execute,
        Acp::Think => ToolKind::Think,
        Acp::Fetch => ToolKind::Fetch,
        Acp::SwitchMode => ToolKind::SwitchMode,
        _ => ToolKind::Other, // #[non_exhaustive]：协议以后加的新分类先归到这
    }
}

type AcpToolKindRecognizer = fn(
    &str,
    Option<&serde_json::Value>,
    Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<crate::acp_chat::ToolKind>;

/// ACP v1 没有 subagent/collaboration 的标准 ToolKind。各 agent 服务端只能把它
/// 投影成 `other`，再通过 `_meta`、rawInput 或标题携带语义。识别器集中注册在这里，
/// 新增适配器方言时加一个 recognizer，不把 provider 判断散落到会话归约和 UI。
const ACP_TOOL_KIND_RECOGNIZERS: &[AcpToolKindRecognizer] = &[
    recognize_collaboration_meta,
    recognize_collaboration_input,
    recognize_collaboration_title,
];

/// 一条完整 ACP tool_call 的最终语义类别。标准类别优先；只有协议退化成 Other
/// 时才运行扩展识别器，避免厂商元数据覆盖 Read/Edit 等明确语义。
pub fn tool_kind_from_acp_call(
    call: &agent_client_protocol::schema::v1::ToolCall,
) -> crate::acp_chat::ToolKind {
    tool_kind_from_acp_fields(
        Some(call.kind),
        Some(&call.title),
        call.raw_input.as_ref(),
        call.meta.as_ref(),
    )
    .unwrap_or(crate::acp_chat::ToolKind::Other)
}

/// tool_call_update 的字段是稀疏的，因此没有足够信息时返回 None，调用方保留原类别。
pub fn tool_kind_from_acp_fields(
    explicit: Option<agent_client_protocol::schema::v1::ToolKind>,
    title: Option<&str>,
    raw_input: Option<&serde_json::Value>,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<crate::acp_chat::ToolKind> {
    let explicit = explicit.map(tool_kind_from_acp);
    if explicit.is_some_and(|kind| kind != crate::acp_chat::ToolKind::Other) {
        return explicit;
    }
    ACP_TOOL_KIND_RECOGNIZERS
        .iter()
        .find_map(|recognize| recognize(title.unwrap_or_default(), raw_input, meta))
        .or(explicit)
}

fn normalize_semantic_token(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn json_has_normalized_key(value: &serde_json::Value, expected: &[&str]) -> bool {
    match value {
        serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
            expected.contains(&normalize_semantic_token(key).as_str())
                || json_has_normalized_key(value, expected)
        }),
        serde_json::Value::Array(items) => items
            .iter()
            .any(|item| json_has_normalized_key(item, expected)),
        _ => false,
    }
}

fn object_has_semantic_marker(object: &serde_json::Map<String, serde_json::Value>) -> bool {
    object.iter().any(|(key, value)| {
        let key = normalize_semantic_token(key);
        let direct = matches!(
            key.as_str(),
            "subagent" | "subagentactivity" | "collaboration" | "collabagent" | "delegation"
        );
        let declared_kind = matches!(key.as_str(), "kind" | "type" | "category" | "toolkind")
            && value.as_str().is_some_and(|value| {
                matches!(
                    normalize_semantic_token(value).as_str(),
                    "subagent" | "collaborate" | "collaboration" | "delegate" | "delegation"
                )
            });
        direct
            || declared_kind
            || value.as_object().is_some_and(object_has_semantic_marker)
            || value.as_array().is_some_and(|items| {
                items
                    .iter()
                    .any(|item| item.as_object().is_some_and(object_has_semantic_marker))
            })
    })
}

fn recognize_collaboration_meta(
    _title: &str,
    _raw_input: Option<&serde_json::Value>,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<crate::acp_chat::ToolKind> {
    meta.filter(|meta| object_has_semantic_marker(meta))
        .map(|_| crate::acp_chat::ToolKind::Collaborate)
}

fn recognize_collaboration_input(
    _title: &str,
    raw_input: Option<&serde_json::Value>,
    _meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<crate::acp_chat::ToolKind> {
    let raw_input = raw_input?;
    let strong_keys = [
        "subagent",
        "subagenttype",
        "agentpath",
        "agentthreadid",
        "receiverthreadids",
        "delegationdepth",
    ];
    let strong = json_has_normalized_key(raw_input, &strong_keys);
    let typed_agent = json_has_normalized_key(raw_input, &["agenttype"])
        && json_has_normalized_key(raw_input, &["prompt", "description", "task"]);
    (strong || typed_agent).then_some(crate::acp_chat::ToolKind::Collaborate)
}

fn recognize_collaboration_title(
    title: &str,
    _raw_input: Option<&serde_json::Value>,
    _meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<crate::acp_chat::ToolKind> {
    let normalized = normalize_semantic_token(title);
    let explicit_name = [
        "spawnagent",
        "spawnsubagent",
        "delegateagent",
        "delegatetask",
        "subagentactivity",
    ]
    .contains(&normalized.as_str());
    let descriptive =
        normalized.contains("subagent") || title.contains("子代理") || title.contains("协作代理");
    (explicit_name || descriptive).then_some(crate::acp_chat::ToolKind::Collaborate)
}

pub fn tool_status_from_acp(
    s: agent_client_protocol::schema::v1::ToolCallStatus,
) -> crate::acp_chat::ToolCallStatus {
    use crate::acp_chat::ToolCallStatus;
    use agent_client_protocol::schema::v1::ToolCallStatus as Acp;
    match s {
        Acp::Pending => ToolCallStatus::Pending,
        Acp::InProgress => ToolCallStatus::InProgress,
        Acp::Completed => ToolCallStatus::Completed,
        Acp::Failed => ToolCallStatus::Failed,
        _ => ToolCallStatus::Pending, // #[non_exhaustive]：协议以后加的新状态先当待定
    }
}

pub fn tool_content_parts(
    content: &[agent_client_protocol::schema::v1::ToolCallContent],
) -> Vec<crate::acp_chat::ToolOutputPart> {
    use crate::acp_chat::ToolOutputPart;
    use agent_client_protocol::schema::v1::ToolCallContent;
    content
        .iter()
        .filter_map(|c| match c {
            ToolCallContent::Content(inner) => {
                if let agent_client_protocol::schema::v1::ContentBlock::Image(image) =
                    &inner.content
                {
                    return Some(ToolOutputPart::Image(crate::acp_chat::AcpImage::new(
                        image.mime_type.clone(),
                        image.data.clone(),
                    )));
                }
                let text = content_text(&inner.content);
                (!text.trim().is_empty()).then_some(ToolOutputPart::Text(text))
            }
            ToolCallContent::Diff(d) => Some(ToolOutputPart::Diff {
                path: d.path.display().to_string(),
                old_text: d.old_text.clone(),
                new_text: d.new_text.clone(),
            }),
            ToolCallContent::Terminal(term) => Some(ToolOutputPart::Terminal {
                id: term.terminal_id.to_string(),
                output: String::new(),
                truncated: false,
                exit_code: None,
                signal: None,
            }),
            _ => None,
        })
        .collect()
}

/// Managed Bun/Pi provisioning, immutable generations and leases live in `managed_runtime`.
/// This module only owns ACP command rewriting, adapter-cache cleanup and diagnostics./// Smelt 启动的 Bun 实际使用的包缓存。登录 shell 里显式配置的绝对路径优先；
/// 相对路径会随会话 cwd 漂移，无法安全判断删除目标，因此不做自动清理。
fn managed_bun_cache_dir() -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    let configured = crate::login_env::login_environment()
        .get("BUN_INSTALL_CACHE_DIR")
        .filter(|value| !value.trim().is_empty())
        .map(|value| crate::workspace_override::expand_tilde(value));
    let path = configured
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join(".bun/install/cache"));
    // 即便用户把缓存变量误设成根目录或 home，也不能让自动回收触碰这种宽目标。
    (path.is_absolute() && path.parent().is_some() && path != home).then_some(path)
}

fn safe_bun_cache_component(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\'])
}

/// 返回 Bun 缓存中一个 npm 包的父目录与无 scope 包名。
fn bun_cache_package_location(
    cache_root: &std::path::Path,
    package: &str,
) -> Option<(std::path::PathBuf, String)> {
    if let Some(scoped) = package.strip_prefix('@') {
        let (scope, name) = scoped.split_once('/')?;
        if !safe_bun_cache_component(scope) || !safe_bun_cache_component(name) {
            return None;
        }
        return Some((cache_root.join(format!("@{scope}")), name.to_string()));
    }
    safe_bun_cache_component(package).then(|| (cache_root.to_path_buf(), package.to_string()))
}

fn bun_cache_entry_matches(name: &str, stem: &str) -> bool {
    name == stem
        || name
            .strip_prefix(stem)
            .is_some_and(|rest| rest.starts_with("@@@"))
}

fn remove_bun_cache_entry(path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        std::fs::remove_file(path).is_ok()
    } else if metadata.is_dir() {
        std::fs::remove_dir_all(path).is_ok()
    } else {
        false
    }
}

fn cleanup_managed_adapter_release(
    cache_root: &std::path::Path,
    release: crate::agent_kind::ManagedAcpAdapterRelease,
) -> usize {
    if !safe_bun_cache_component(release.version) {
        return 0;
    }
    let Some((package_parent, package_name)) =
        bun_cache_package_location(cache_root, release.package)
    else {
        return 0;
    };

    let mut removed = 0;
    let actual_stem = format!("{package_name}@{}", release.version);
    if let Ok(entries) = std::fs::read_dir(&package_parent) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name
                .to_str()
                .is_some_and(|name| bun_cache_entry_matches(name, &actual_stem))
                && remove_bun_cache_entry(&entry.path())
            {
                removed += 1;
            }
        }
    }

    // Bun 还会维护 `<scope>/<package>/<version>@@@N` 别名符号链接。只遍历真正
    // 的目录，拒绝跟随异常符号链接，避免缓存损坏时越过 cache_root。
    let aliases = package_parent.join(&package_name);
    if std::fs::symlink_metadata(&aliases).is_ok_and(|metadata| metadata.is_dir()) {
        if let Ok(entries) = std::fs::read_dir(&aliases) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name
                    .to_str()
                    .is_some_and(|name| bun_cache_entry_matches(name, release.version))
                    && remove_bun_cache_entry(&entry.path())
                {
                    removed += 1;
                }
            }
        }
        let _ = std::fs::remove_dir(&aliases); // 仅空目录会成功；当前版本别名不会受影响。
    }
    removed
}

/// 清掉 Smelt 历史默认值对应的适配器包，不碰当前版、未发布版本或共享依赖。
/// 用户若显式继续运行某个已知旧版，Bun 会按普通缓存缺失语义重新下载；命令配置
/// 本身由上层精确匹配迁移保护，不会在这里改写。
fn cleanup_stale_managed_adapter_cache(cache_root: &std::path::Path) -> usize {
    let mut removed = 0;
    for agent in ConversationAgentKind::ALL {
        let Some(adapter) = agent.managed_adapter() else {
            continue;
        };
        for release in adapter.previous_releases() {
            removed += cleanup_managed_adapter_release(cache_root, *release);
        }
        // 当默认命令已经迁离一个受管适配器（目前是 Pi）时，它最后发布的版本也
        // 变成可回收项；Claude/Codex 当前默认仍等于 current command，必须保留。
        if adapter.current_command() != agent.default_cmd() {
            removed += cleanup_managed_adapter_release(cache_root, adapter.current_release());
        }
    }
    removed
}

fn cleanup_stale_managed_adapters(status: &dyn Fn(&str)) {
    let Some(cache_root) = managed_bun_cache_dir() else {
        return;
    };
    let removed = cleanup_stale_managed_adapter_cache(&cache_root);
    if removed > 0 {
        status(&format!("已清理 {removed} 个旧 ACP 适配器缓存项"));
    }
}

/// 探测 Smelt 实际会用到的 Agent CLI。
///
/// 调用方应在后台执行本函数：每个已找到的可执行文件都会运行一次固定的
/// `--version`，但不创建会话或访问网络。
pub fn inspect_acp_runtime() -> AcpRuntimeDiagnostics {
    let search_path = extended_search_path();
    let inspect =
        |kind: TerminalAgentKind| inspect_runtime_program(kind.cli_program(), &search_path);
    let mut diagnostics = AcpRuntimeDiagnostics::default();
    for agent in TerminalAgentKind::ALL {
        diagnostics.record_terminal(agent, inspect(agent));
    }
    diagnostics.record_agent(ConversationAgentKind::Pi, inspect_smelt_pi_runtime());
    diagnostics
}

fn inspect_smelt_pi_runtime() -> RuntimeExecutable {
    let program = SMELT_PI_AGENT_COMMAND.to_string();
    match crate::managed_runtime::managed_pi_diagnostic() {
        Ok((entry, ready)) => RuntimeExecutable {
            program,
            path: Some(entry.to_string_lossy().into_owned()),
            version: Some(if ready {
                format!(
                    "Smelt Pi Runtime {} · 已就绪",
                    crate::managed_runtime::pi_runtime_version()
                )
            } else {
                format!(
                    "Smelt Pi Runtime {} · 缺少 photon_rs_bg.wasm",
                    crate::managed_runtime::pi_runtime_version()
                )
            }),
            error: None,
        },
        Err(error) => RuntimeExecutable {
            program,
            error: Some(error),
            ..Default::default()
        },
    }
}

fn inspect_runtime_program(program: &str, search_path: &str) -> RuntimeExecutable {
    inspect_runtime_path(
        program,
        resolve_in_path(program, search_path).map(std::path::PathBuf::from),
    )
}

fn inspect_runtime_path(program: &str, path: Option<std::path::PathBuf>) -> RuntimeExecutable {
    let mut status = RuntimeExecutable {
        program: program.to_string(),
        ..Default::default()
    };
    let Some(path) = path else {
        return status;
    };

    status.path = Some(path.to_string_lossy().into_owned());
    match run_runtime_version(&path) {
        Ok(output) if output.status.success() => {
            status.version = runtime_output_line(&output.stdout, &output.stderr);
            if status.version.is_none() {
                status.error = Some("`--version` 未输出版本信息".to_string());
            }
        }
        Ok(output) => {
            let detail = runtime_output_line(&output.stderr, &output.stdout)
                .unwrap_or_else(|| "没有输出错误信息".to_string());
            status.error = Some(format!(
                "`--version` 执行失败（{}）：{detail}",
                output.status
            ));
        }
        Err(error) => status.error = Some(error),
    }
    status
}

/// 版本查询必须有上限：用户自己的 shell shim 或损坏的 CLI 不能让设置页永久处于
/// 「检测中」。stdout / stderr 各由一条线程边跑边读，输出再多也不会把子进程卡在写
/// 管道上；进程退出由 `wait` 告知，超过上限便杀掉并收回。
fn run_runtime_version(path: &std::path::Path) -> Result<std::process::Output, String> {
    const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
    let mut command = std::process::Command::new(path);
    command
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .env("NO_COLOR", "1");
    // 版本探测也要完整环境：volta 一类的 shim 缺 `VOLTA_HOME` 时连 `--version`
    // 都会失败，设置页于是把"装好了但环境没带全"误报成"未安装"，把用户引向
    // 重装这条完全错误的路。
    crate::login_env::apply_login_environment(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法执行 `--version`：{error}"))?;
    let deadline = Instant::now() + VERSION_PROBE_TIMEOUT;
    let stdout = read_pipe_in_background(child.stdout.take());
    let stderr = read_pipe_in_background(child.stderr.take());
    let timed_out = Arc::new(AtomicBool::new(false));
    let status = {
        let timed_out = timed_out.clone();
        crate::process_wait::wait_child_for(child, VERSION_PROBE_TIMEOUT, move |pid| {
            timed_out.store(true, Ordering::SeqCst);
            let _ = smelt_sys::process::terminate_pid(pid);
        })
    }
    .map_err(|error| format!("等待 `--version` 失败：{error}"))?;
    if timed_out.load(Ordering::SeqCst) {
        return Err("`--version` 检测超时（5 秒）".to_string());
    }
    // 子进程已经退出。它派生的后代可能还握着管道，读的一侧也只等到同一个截止。
    let collect = |pipe: std::sync::mpsc::Receiver<Vec<u8>>| {
        pipe.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_default()
    };
    Ok(std::process::Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

/// 在后台线程里把管道读到 EOF，读完从返回的 channel 交出。没有管道或线程起不来时
/// channel 直接断开，接收方拿到空输出。
fn read_pipe_in_background(
    pipe: Option<impl std::io::Read + Send + 'static>,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(mut pipe) = pipe {
        let _ = std::thread::Builder::new()
            .name("smelt-pipe-read".to_string())
            .spawn(move || {
                let mut buf = Vec::new();
                let _ = pipe.read_to_end(&mut buf);
                let _ = tx.send(buf);
            });
    }
    rx
}

/// 从命令输出中提取首个非空行，并限制长度，避免异常 CLI 输出撑爆设置页。
fn runtime_output_line(primary: &[u8], fallback: &[u8]) -> Option<String> {
    [primary, fallback].into_iter().find_map(|bytes| {
        String::from_utf8_lossy(bytes)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(compact_runtime_output)
    })
}

fn compact_runtime_output(line: &str) -> String {
    const MAX_CHARS: usize = 180;
    let mut output: String = line.chars().take(MAX_CHARS).collect();
    if line.chars().nth(MAX_CHARS).is_some() {
        output.push_str("...");
    }
    output
}

/// 设置页完成一次 Agent CLI 安装后返回的执行摘要。
///
/// `command` 只包含 Smelt 内置的固定包管理器参数，便于在失败提示中让用户复现；
/// 不接受任何来自设置文本框或环境变量的命令片段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcpCliInstallReport {
    pub installer: &'static str,
    pub command: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AcpCliInstallPlan {
    installer: &'static str,
    executable: String,
    display_executable: &'static str,
    args: Vec<String>,
}

impl AcpCliInstallPlan {
    fn report(&self) -> AcpCliInstallReport {
        AcpCliInstallReport {
            installer: self.installer,
            command: std::iter::once(self.display_executable.to_string())
                .chain(self.args.iter().cloned())
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

/// 通过本机已有的 Homebrew 或 npm 安装指定的 Agent CLI。
///
/// 不执行各厂商的 `curl | sh` 安装器，也不调用 shell 或 sudo；只有用户点设置页
/// 的「安装」按钮时才会进入这里。Claude、Codex、Copilot 优先使用官方 Homebrew
/// cask，Grok 使用 xAI 官方 npm 包。包管理器不存在或命令失败时会返回可展示错误。
pub fn install_acp_cli(agent: ConversationAgentKind) -> Result<AcpCliInstallReport, String> {
    let search_path = extended_search_path();
    let plan = acp_cli_install_plan(
        agent,
        resolve_in_path("brew", &search_path),
        resolve_in_path("npm", &search_path),
    )?;
    let report = plan.report();
    run_acp_cli_install(&plan, &search_path)?;
    Ok(report)
}

fn acp_cli_install_plan(
    agent: ConversationAgentKind,
    brew: Option<String>,
    npm: Option<String>,
) -> Result<AcpCliInstallPlan, String> {
    if let (Some(executable), Some(cask)) = (brew, agent.homebrew_cask()) {
        return Ok(AcpCliInstallPlan {
            installer: "Homebrew",
            executable,
            display_executable: "brew",
            args: ["install", "--cask", cask]
                .into_iter()
                .map(str::to_string)
                .collect(),
        });
    }

    if let (Some(executable), Some(package)) = (npm, agent.npm_package()) {
        return Ok(AcpCliInstallPlan {
            installer: "npm",
            executable,
            display_executable: "npm",
            args: ["install", "--global", package]
                .into_iter()
                .map(str::to_string)
                .collect(),
        });
    }

    let help = match (agent.homebrew_cask(), agent.npm_package()) {
        (None, None) => format!(
            "{} CLI 请用官方安装器：curl https://cursor.com/install -fsS | bash",
            agent.label()
        ),
        (None, Some(package)) => format!("{} CLI 需要 npm（官方包：{package}）", agent.label()),
        _ => "需要 Homebrew 或 npm".to_string(),
    };
    Err(format!(
        "无法安装 {}：未找到可用的包管理器（{help}）",
        agent.label()
    ))
}

/// 包安装可能拉取数百 MB，但不能无限占着设置页的任务状态。stdout/stderr 刻意
/// 丢弃，避免 npm 或 brew 的大量进度输出塞满 pipe 导致子进程卡死；失败会返回固定
/// 命令，用户可在终端重跑获得完整日志。进程退出由 `wait` 告知，到上限便杀掉并收回。
fn run_acp_cli_install(plan: &AcpCliInstallPlan, search_path: &str) -> Result<(), String> {
    const INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
    let report = plan.report();
    let mut command = std::process::Command::new(&plan.executable);
    command
        .args(&plan.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env("NO_COLOR", "1");
    // 完整登录环境 + 扩展 PATH：npm 走 registry，缺代理设置时不会报错而是重试到
    // 超时；brew 同理。PATH 放在最后，覆盖快照里那份。
    crate::login_env::apply_login_environment(&mut command);
    command.env("PATH", search_path);
    let child = command
        .spawn()
        .map_err(|error| format!("无法启动 {}：{error}", report.installer))?;
    let timed_out = Arc::new(AtomicBool::new(false));
    let status = {
        let timed_out = timed_out.clone();
        crate::process_wait::wait_child_for(child, INSTALL_TIMEOUT, move |pid| {
            timed_out.store(true, Ordering::SeqCst);
            let _ = smelt_sys::process::terminate_pid(pid);
        })
    }
    .map_err(|error| format!("等待 {} 安装进程失败：{error}", report.installer))?;
    if timed_out.load(Ordering::SeqCst) {
        return Err(format!(
            "{} 安装超时（15 分钟）。可在终端执行 `{}` 重试",
            report.installer, report.command
        ));
    }
    if status.success() {
        return Ok(());
    }
    Err(format!(
        "{} 安装命令退出（{status}）。可在终端执行 `{}` 查看完整错误",
        report.installer, report.command
    ))
}

/// 命令首词是 `bunx`/`bun` 时解析到应用里的 Bun。Pi 的逻辑入口由
/// `pi_rpc` 驱动在进入 ACP 解析前接管；这里不把它伪装成 ACP 命令。应用里没有
/// Bun、但系统 PATH 里有同名可执行则原样放行；其他命令一律不动。
fn resolve_runtime_command_with_bun(
    cmd: &str,
    status: &dyn Fn(&str),
) -> Result<(String, Option<crate::managed_runtime::ManagedBunRuntime>), String> {
    let head = command_head_after_env_prefixes(cmd).unwrap_or_default();
    if head != "bunx" && head != "bun" {
        return Ok((cmd.to_string(), None));
    }
    let runtime_result = crate::managed_runtime::sync_managed_bun(status);
    cleanup_stale_managed_adapters(status);
    match runtime_result {
        Ok(runtime) => {
            let bun = runtime.path.to_string_lossy().into_owned();
            let command =
                rewrite_runtime_command_with_bun(cmd, &bun).unwrap_or_else(|| cmd.to_string());
            Ok((command, Some(runtime)))
        }
        Err(error) => {
            // Explicit third-party ACP commands retain their existing PATH fallback policy.
            let system_has = std::env::split_paths(crate::login_env::login_path())
                .any(|path| path.join(head).is_file());
            if system_has {
                Ok((cmd.to_string(), None))
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(test)]
fn resolve_runtime_command(cmd: &str, status: &dyn Fn(&str)) -> Result<String, String> {
    resolve_runtime_command_with_bun(cmd, status).map(|(command, _runtime)| command)
}

fn command_head_after_env_prefixes(cmd: &str) -> Option<&str> {
    cmd.split_whitespace()
        .find(|tok| crate::workspace_override::split_env_assignment(tok).is_none())
}

fn rewrite_runtime_command_with_bun(cmd: &str, bun: &str) -> Option<String> {
    let mut words = cmd.split_whitespace();
    let mut prefix = Vec::new();
    let head = loop {
        match words.next() {
            Some(tok) if crate::workspace_override::split_env_assignment(tok).is_some() => {
                prefix.push(tok);
            }
            Some(tok) => break tok,
            None => return None,
        }
    };
    if head != "bunx" && head != "bun" {
        return None;
    }
    let mut parts: Vec<String> = prefix.into_iter().map(str::to_string).collect();
    parts.push(bun.to_string());
    if head == "bunx" {
        parts.push("x".to_string());
    }
    parts.extend(words.map(str::to_string));
    Some(parts.join(" "))
}

/// login shell 的 PATH（跑一次缓存）。GUI 进程从 Finder 启动时 PATH 只有系统
/// 目录，nvm/homebrew 里的 npx 找不到；跟终端会话不同（那边 shell 由 smeltd 起，
/// 自带 login 环境），ACP 子进程是 GUI 直接 spawn 的，得自己补。
/// login PATH + 一批常见 CLI 安装目录，用来兜底查找命令（见 build_agent 的
/// 注释）。顺序：login PATH 在前（用户显式配的优先），标准安装位在后兜底；
/// 重复目录无所谓，resolve 取第一个命中即止。
pub(crate) fn extended_search_path() -> String {
    extend_search_path(crate::login_env::login_path(), dirs::home_dir().as_deref())
}

fn extend_search_path(login: &str, home: Option<&std::path::Path>) -> String {
    // 空的一段在 PATH 里等于当前目录，不带进来。
    let mut entries: Vec<std::path::PathBuf> = std::env::split_paths(login)
        .filter(|entry| !entry.as_os_str().is_empty())
        .collect();
    if let Some(home) = home {
        // grok 装在 ~/.grok/bin（软链常在 ~/.local/bin）；其余是 pip/npm/cargo
        // 之类常把 CLI 放的用户级目录。
        for sub in [
            ".grok/bin",
            ".opencode/bin",
            ".local/bin",
            "bin",
            ".cargo/bin",
            ".volta/bin",
        ] {
            entries.push(home.join(sub));
        }
    }
    #[cfg(unix)]
    for dir in ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"] {
        entries.push(dir.into());
    }
    // 分隔符按平台：Windows 是 `;`。拼不起来（某一项本身带分隔符）就只用登录 PATH。
    std::env::join_paths(entries)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| login.to_string())
}

/// 在 PATH 里把命令名解析成绝对路径（找第一个可执行文件）。
/// 已经带 `/` 或 `\` 的（绝对路径、受管 bun 的全路径）原样返回，不查。找不到返回
/// None——调用方保留原名，让 spawn 照常失败并把真实错误报出来。
///
/// Windows 上没有扩展名的名字按 `.exe`、`.cmd`、`.bat`、原名这个顺序找。
/// `CreateProcess` 只补 `.exe`，`claude` / `npx` 这类实际是 `.cmd`。
fn resolve_in_path(program: &str, path: &str) -> Option<String> {
    if program.contains('/') || program.contains('\\') {
        return Some(program.to_string());
    }
    let names = command_file_names(program);
    for dir in std::env::split_paths(path).filter(|d| !d.as_os_str().is_empty()) {
        for name in &names {
            let full = dir.join(name);
            // 是文件且可执行（软链会被 metadata 跟随到目标）。Windows 没有执行位。
            if let Ok(meta) = std::fs::metadata(&full) {
                if !meta.is_file() {
                    continue;
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if meta.permissions().mode() & 0o111 == 0 {
                        continue;
                    }
                }
                return full.to_str().map(String::from);
            }
        }
    }
    None
}

fn command_file_names(program: &str) -> Vec<String> {
    #[cfg(windows)]
    if !program.as_bytes().contains(&b'.') {
        return vec![
            format!("{program}.exe"),
            format!("{program}.cmd"),
            format!("{program}.bat"),
            program.to_string(),
        ];
    }
    vec![program.to_string()]
}

// login shell 的 PATH 探测（连同各家 agent 的自定义 workspace 目录变量一起）
// 挪进了 `crate::login_env`——不止 PATH 这一个变量要用同一套"起交互式 shell
// 才能读到 .zshrc export"的机制，claude_paths.rs 的 CLAUDE_CONFIG_DIR 判断
// 也要用它，不能各起一次慢 shell。

#[cfg(test)]
mod elicit_parse_tests {
    use super::*;
    use agent_client_protocol::schema::v1::{
        ElicitationScope, ElicitationSessionScope, ElicitationUrlMode,
    };

    /// claude-agent-acp 对 AskUserQuestion 的真实 wire 形状：单选 `oneOf`+`const`，
    /// 每题附带一个**可选**自由文本 "Other" 字段；现在两者都应进入统一表单。
    #[test]
    fn ask_user_question_shape_with_optional_custom_field_parses() {
        let schema: ElicitationSchema = serde_json::from_value(serde_json::json!({
            "type": "object",
            "properties": {
                "question_0": {
                    "type": "string",
                    "title": "水果",
                    "oneOf": [
                        { "const": "苹果", "title": "苹果", "description": "脆甜多汁" },
                        { "const": "香蕉", "title": "香蕉" }
                    ]
                },
                "question_0_custom": {
                    "type": "string",
                    "title": "Other",
                    "description": "Type your own answer (optional)."
                }
            },
            "required": ["question_0"]
        }))
        .expect("schema 反序列化");
        let fields = parse_elicit_fields(&schema).expect("单选和自由文本都应解析");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].key, "question_0");
        assert!(fields[0].required);
        let ElicitFieldKind::Select(options) = &fields[0].kind else {
            panic!("单选题应解析为 Select");
        };
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].label, "苹果");
        assert!(matches!(&options[0].value, ElicitationContentValue::String(s) if s == "苹果"));
        assert!(
            fields
                .iter()
                .any(|field| matches!(field.kind, ElicitFieldKind::Text { .. }))
        );
        assert!(!fields[1].required);
    }

    /// 必填自由文本由通用输入框承接，不再迫使 agent 回退纯文本追问。
    #[test]
    fn required_free_text_field_maps_to_text_input() {
        let schema: ElicitationSchema = serde_json::from_value(serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "title": "你的名字" }
            },
            "required": ["name"]
        }))
        .expect("schema 反序列化");
        let fields = parse_elicit_fields(&schema).expect("自由文本应可显示");
        assert!(fields[0].required);
        assert!(matches!(
            fields[0].kind,
            ElicitFieldKind::Text { secret: false }
        ));
    }

    /// 多选题：`type: "array"` + `items.anyOf`（titled 枚举）。
    #[test]
    fn multi_select_anyof_shape_parses() {
        let schema: ElicitationSchema = serde_json::from_value(serde_json::json!({
            "type": "object",
            "properties": {
                "question_0": {
                    "type": "array",
                    "title": "运动",
                    "items": { "anyOf": [
                        { "const": "跑步", "title": "跑步" },
                        { "const": "游泳", "title": "游泳" }
                    ] }
                }
            }
        }))
        .expect("schema 反序列化");
        let fields = parse_elicit_fields(&schema).expect("anyOf 多选应可解析");
        assert!(matches!(&fields[0].kind, ElicitFieldKind::MultiSelect(o) if o.len() == 2));
    }

    #[test]
    fn url_elicitation_becomes_an_external_link_field() {
        let request = CreateElicitationRequest::new(
            ElicitationUrlMode::new(
                ElicitationScope::Session(ElicitationSessionScope::new(SessionId::new("s"))),
                "elicit-1",
                "https://example.com/device",
            ),
            "请在浏览器完成登录",
        );
        let fields = elicitation_fields(&request).expect("URL 模式应显示");
        assert_eq!(fields.len(), 1);
        assert!(!fields[0].required);
        assert!(matches!(
            &fields[0].kind,
            ElicitFieldKind::ExternalUrl(url) if url == "https://example.com/device"
        ));
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    #[test]
    fn runtime_output_prefers_stdout_and_trims_whitespace() {
        assert_eq!(
            runtime_output_line(b"\n  codex-cli 1.2.3  \nmore\n", b"fallback"),
            Some("codex-cli 1.2.3".to_string())
        );
    }

    #[test]
    fn runtime_output_falls_back_to_stderr_and_limits_length() {
        let long = "x".repeat(181);
        let output = runtime_output_line(b"\n", long.as_bytes()).expect("stderr fallback");
        assert_eq!(output.chars().count(), 183);
        assert!(output.ends_with("..."));
    }

    #[test]
    fn runtime_version_probe_collects_a_short_lived_process() {
        // where.exe 会把 `--version` 当文件名逐个搜 PATH，CI 上耗时 1.7～5.3 秒，
        // 压线超时；whoami.exe 遇到未知参数立刻报错退出。
        let program = if cfg!(windows) {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
            std::path::PathBuf::from(root).join(r"System32\whoami.exe")
        } else {
            std::path::PathBuf::from("/usr/bin/true")
        };
        let output = run_runtime_version(&program);
        assert!(output.is_ok(), "短命令的版本探测应正常收尾：{output:?}");
    }

    /// 输出超过管道容量时，退出前没人读就会卡在写上，只能等到超时。
    #[cfg(unix)]
    #[test]
    fn runtime_version_probe_drains_large_output_while_running() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("noisy-cli");
        std::fs::write(
            &program,
            "#!/bin/sh\ndd if=/dev/zero bs=1024 count=256 1>&2 2>/dev/null\necho noisy-cli 1.2.3\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&program, permissions).unwrap();

        let output = run_runtime_version(&program).expect("大量输出不该让版本探测超时");
        assert!(output.status.success());
        assert_eq!(output.stderr.len(), 256 * 1024);
        assert_eq!(
            runtime_output_line(&output.stdout, &output.stderr),
            Some("noisy-cli 1.2.3".to_string())
        );
    }

    #[test]
    fn preferred_installers_match_the_official_packages() {
        let brew = Some("/opt/homebrew/bin/brew".to_string());
        let npm = Some("/opt/homebrew/bin/npm".to_string());

        let claude = acp_cli_install_plan(ConversationAgentKind::Claude, brew.clone(), npm.clone())
            .expect("Claude 应有安装方案");
        assert_eq!(claude.report().command, "brew install --cask claude-code");

        let codex = acp_cli_install_plan(ConversationAgentKind::Codex, brew.clone(), npm.clone())
            .expect("Codex 应有安装方案");
        assert_eq!(codex.report().command, "brew install --cask codex");

        let copilot =
            acp_cli_install_plan(ConversationAgentKind::Copilot, brew.clone(), npm.clone())
                .expect("Copilot 应有安装方案");
        assert_eq!(copilot.report().command, "brew install --cask copilot-cli");

        let grok = acp_cli_install_plan(ConversationAgentKind::Grok, brew.clone(), npm.clone())
            .expect("Grok 应有安装方案");
        assert_eq!(
            grok.report().command,
            "npm install --global @xai-official/grok"
        );

        let cursor_err =
            acp_cli_install_plan(ConversationAgentKind::Cursor, brew.clone(), npm.clone())
                .expect_err("Cursor 没有 brew/npm 包");
        assert!(cursor_err.contains("cursor.com/install"));

        let opencode =
            acp_cli_install_plan(ConversationAgentKind::OpenCode, brew.clone(), npm.clone())
                .expect("OpenCode 应有安装方案");
        assert_eq!(
            opencode.report().command,
            "npm install --global opencode-ai"
        );

        let pi = acp_cli_install_plan(ConversationAgentKind::Pi, brew, npm)
            .expect("Pi 应有 npm 安装方案");
        assert_eq!(
            pi.report().command,
            "npm install --global @earendil-works/pi-coding-agent"
        );
    }

    #[test]
    fn npm_is_used_when_homebrew_is_unavailable() {
        let plan = acp_cli_install_plan(
            ConversationAgentKind::Codex,
            None,
            Some("/usr/local/bin/npm".to_string()),
        )
        .expect("npm 应作为回退安装器");

        assert_eq!(plan.installer, "npm");
        assert_eq!(plan.report().command, "npm install --global @openai/codex");
    }

    #[test]
    fn missing_package_managers_produces_actionable_error() {
        let error = acp_cli_install_plan(ConversationAgentKind::Claude, None, None)
            .expect_err("没有安装器时应失败");

        assert!(error.contains("Homebrew 或 npm"));
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_stale_managed_adapter_cache_is_targeted() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "smelt-adapter-cache-cleanup-{}",
            uuid::Uuid::new_v4()
        ));
        let scope = root.join("@agentclientprotocol");
        let old = scope.join("claude-agent-acp@0.59.0@@@1");
        let retired = scope.join("claude-agent-acp@0.70.0@@@1");
        let current = scope.join("claude-agent-acp@0.78.0@@@1");
        let unknown = scope.join("claude-agent-acp@0.58.0@@@1");
        let shared_sdk = scope.join("sdk@1.2.1@@@1");
        let aliases = scope.join("claude-agent-acp");
        let old_alias = aliases.join("0.59.0@@@1");
        let retired_alias = aliases.join("0.70.0@@@1");
        let current_alias = aliases.join("0.78.0@@@1");
        let retired_pi = root.join("pi-acp@0.0.33@@@1");
        let pi_aliases = root.join("pi-acp");
        let retired_pi_alias = pi_aliases.join("0.0.33@@@1");
        for dir in [&old, &retired, &current, &unknown, &shared_sdk, &aliases] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::create_dir_all(&retired_pi).unwrap();
        std::fs::create_dir_all(&pi_aliases).unwrap();
        symlink(&old, &old_alias).unwrap();
        symlink(&retired, &retired_alias).unwrap();
        symlink(&current, &current_alias).unwrap();
        symlink(&retired_pi, &retired_pi_alias).unwrap();

        let removed = cleanup_stale_managed_adapter_cache(&root);

        assert!(removed >= 6, "旧实体目录和版本别名都应被清掉");
        assert!(!old.exists());
        assert!(std::fs::symlink_metadata(&old_alias).is_err());
        assert!(!retired.exists(), "刚卸任的 0.70.0 缓存应清掉");
        assert!(std::fs::symlink_metadata(&retired_alias).is_err());
        assert!(!retired_pi.exists(), "已退役的 pi-acp 缓存应清掉");
        assert!(std::fs::symlink_metadata(&retired_pi_alias).is_err());
        assert!(current.is_dir(), "当前适配器版本必须保留");
        assert!(current_alias.is_symlink(), "当前版本别名必须保留");
        assert!(
            unknown.is_dir(),
            "没有作为 Smelt 默认值发布过的版本不应删除"
        );
        assert!(shared_sdk.is_dir(), "共享依赖缓存不属于适配器清理范围");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 非 bun 前缀的命令一律原样放行（npx / 绝对路径等逃生口不被劫持）。
    #[test]
    fn non_bun_commands_pass_through() {
        let noop = |_: &str| {};
        for cmd in [
            "npx -y foo@1",
            "/usr/local/bin/some-acp --flag",
            "node adapter.js",
        ] {
            assert_eq!(resolve_runtime_command(cmd, &noop).unwrap(), cmd);
        }
    }

    #[test]
    fn bun_and_bunx_rewrites_are_deterministic_and_preserve_argument_order() {
        assert_eq!(
            rewrite_runtime_command_with_bun("bunx pkg@1 --flag", "/managed/bun").as_deref(),
            Some("/managed/bun x pkg@1 --flag")
        );
        assert_eq!(
            rewrite_runtime_command_with_bun("bunx --bun pkg@1 --flag", "/managed/bun").as_deref(),
            Some("/managed/bun x --bun pkg@1 --flag")
        );
        assert_eq!(
            rewrite_runtime_command_with_bun("bun --version", "/managed/bun").as_deref(),
            Some("/managed/bun --version")
        );
        assert_eq!(
            rewrite_runtime_command_with_bun("node app.js", "/managed/bun"),
            None
        );
    }

    #[test]
    fn runtime_rewrite_preserves_legacy_env_prefixes_before_bunx() {
        let out = rewrite_runtime_command_with_bun(
            "CLAUDE_CONFIG_DIR=~/.claude bunx --bun @agentclientprotocol/claude-agent-acp@0.78.0",
            "/managed/bun",
        )
        .expect("bunx should rewrite");
        assert_eq!(
            out,
            "CLAUDE_CONFIG_DIR=~/.claude /managed/bun x --bun @agentclientprotocol/claude-agent-acp@0.78.0"
        );
    }
}

#[cfg(test)]
mod client_capability_tests {
    use super::{parent_tool_id_from_meta, smelt_client_capabilities};

    #[test]
    fn handshake_declares_fs_terminal_and_nested_subagent() {
        let caps = smelt_client_capabilities();
        assert!(caps.fs.read_text_file);
        assert!(caps.fs.write_text_file);
        assert!(caps.terminal);
        assert_eq!(
            caps.meta
                .as_ref()
                .and_then(|meta| meta.get("subagent-transcript")),
            Some(&serde_json::Value::Bool(true))
        );
        assert!(
            caps.session
                .as_ref()
                .and_then(|session| session.config_options.as_ref())
                .and_then(|options| options.boolean.as_ref())
                .is_some()
        );
        let elicitation = caps.elicitation.as_ref().expect("elicitation");
        assert!(elicitation.form.is_some());
        assert!(elicitation.url.is_some());
        assert!(caps.plan.is_some());
    }

    #[test]
    fn parent_tool_id_reads_claude_nested_meta() {
        let mut claude = serde_json::Map::new();
        claude.insert(
            "parentToolUseId".into(),
            serde_json::Value::String("agent-1".into()),
        );
        let mut meta = serde_json::Map::new();
        meta.insert("claudeCode".into(), serde_json::Value::Object(claude));
        assert_eq!(
            parent_tool_id_from_meta(Some(&meta)).as_deref(),
            Some("agent-1")
        );
        assert_eq!(parent_tool_id_from_meta(None), None);
    }
}

#[cfg(test)]
mod kiro_ext_tests {
    #[test]
    fn unwraps_get_kas_token_envelope_without_keeping_kind() {
        let stdout = r#"{"kind":"getKasToken","data":{"accessToken":"tok","expiresAt":"t","profileArn":"arn","provider":"Github"}}"#;
        let value = super::kiro_token_from_cli_output(stdout).expect("信封应解开");
        assert_eq!(value["accessToken"], "tok");
        assert_eq!(value["provider"], "Github");
        assert!(value.get("kind").is_none());
    }

    #[test]
    fn get_kas_token_error_envelope_becomes_err() {
        let stdout = r#"{"kind":"error","data":{"message":"not logged in"}}"#;
        let err = super::kiro_token_from_cli_output(stdout).expect_err("错误信封");
        assert!(err.contains("not logged in"));
    }

    #[test]
    fn shell_type_ext_does_not_need_kiro_cli() {
        let value = super::vendor_ext_result("kiro/terminal/shell_type", "{}").unwrap();
        assert!(value["shellType"].as_str().is_some());
        let prefixed = super::vendor_ext_result("_kiro/terminal/shell_type", "{}").unwrap();
        assert_eq!(value, prefixed);
    }

    #[test]
    fn unknown_vendor_ext_returns_empty_object_so_handshake_does_not_hang() {
        let value =
            super::vendor_ext_result("_kiro/openExternalUrl", r#"{"url":"https://example"}"#)
                .unwrap();
        assert_eq!(value, serde_json::json!({}));
    }
}

#[cfg(test)]
mod session_info_update_tests {
    use super::session_title_update;
    use agent_client_protocol::schema::MaybeUndefined;

    #[test]
    fn title_update_preserves_undefined_clear_and_value() {
        assert_eq!(session_title_update(MaybeUndefined::Undefined), None);
        assert_eq!(session_title_update(MaybeUndefined::Null), Some(None));
        assert_eq!(
            session_title_update(MaybeUndefined::Value("  修复登录超时  ".into())),
            Some(Some("修复登录超时".into()))
        );
        assert_eq!(
            session_title_update(MaybeUndefined::Value("   ".into())),
            Some(None),
            "空白 ACP 标题按清空处理，不能让侧栏出现空行"
        );
    }
}

#[cfg(test)]
mod restore_failure_tests {
    use super::{
        ConversationRestoreFailure, SessionStart, classify_restore_failure, select_session_start,
    };
    use agent_client_protocol::Error;

    #[test]
    fn cold_restore_selects_load_and_never_resume() {
        assert_eq!(select_session_start(true, true), SessionStart::Load);
    }

    #[test]
    fn fresh_session_selects_new_without_requiring_load_capability() {
        assert_eq!(select_session_start(false, false), SessionStart::New);
        assert_eq!(select_session_start(false, true), SessionStart::New);
    }

    #[test]
    fn cold_restore_without_load_capability_is_explicitly_unsupported() {
        assert_eq!(
            select_session_start(true, false),
            SessionStart::UnsupportedLoad
        );
    }

    #[test]
    fn missing_history_is_an_explicit_restore_failure() {
        assert_eq!(
            classify_restore_failure(&Error::resource_not_found(None)),
            ConversationRestoreFailure::HistoryMissing
        );
    }

    #[test]
    fn wrapped_missing_rollout_is_an_explicit_restore_failure() {
        let error = Error::internal_error().data(serde_json::json!({
            "details": "no rollout found for thread id deadbeef"
        }));
        assert_eq!(
            classify_restore_failure(&error),
            ConversationRestoreFailure::HistoryMissing
        );
    }

    #[test]
    fn unsupported_load_is_an_explicit_restore_failure() {
        assert_eq!(
            classify_restore_failure(&Error::method_not_found()),
            ConversationRestoreFailure::UnsupportedLoad
        );
    }

    #[test]
    fn transient_load_failure_remains_retryable_and_does_not_start_fresh() {
        let error = Error::internal_error();
        let failure = classify_restore_failure(&error);
        assert_eq!(
            failure,
            ConversationRestoreFailure::Failed(format!("恢复历史对话失败，可重试：{error}"))
        );
        assert!(!matches!(
            failure,
            ConversationRestoreFailure::HistoryMissing
        ));
    }
}

#[cfg(test)]
mod handshake_watchdog_tests {
    use super::{AcpHandshakeWatchdog, wait_for_acp_handshake};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    #[test]
    fn completed_handshake_cancels_watchdog() {
        let watchdog = AcpHandshakeWatchdog::with_timeout(999_999, Duration::from_millis(50));
        watchdog.mark_complete();
        std::thread::sleep(Duration::from_millis(5));
        assert!(!watchdog.timed_out());
    }

    #[test]
    fn handshake_wait_obeys_deadline() {
        let completed = Arc::new((Mutex::new(false), Condvar::new()));
        assert!(!wait_for_acp_handshake(
            &completed,
            Duration::from_millis(5)
        ));
    }
}

#[cfg(test)]
mod spawn_gate_tests {
    use super::with_spawn_gate;
    use std::sync::{Arc, RwLock, mpsc};
    use std::time::Duration;

    #[test]
    fn write_guard_blocks_gated_spawn_section_and_permit_releases_afterward() {
        let gate = Arc::new(RwLock::new(()));
        let write_guard = gate.write().unwrap();
        let gate_for_thread = Arc::clone(&gate);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();

        let worker = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            with_spawn_gate(Some(&gate_for_thread), || {
                entered_tx.send(()).unwrap();
            });
        });

        ready_rx.recv().unwrap();
        let entered_while_locked = entered_rx.recv_timeout(Duration::from_millis(50)).is_ok();
        drop(write_guard);
        if !entered_while_locked {
            entered_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("spawn section should proceed after upgrade releases the write guard");
        }
        worker.join().unwrap();
        assert!(
            !entered_while_locked,
            "write guard must block entry into the actual spawn section"
        );
        assert!(
            gate.try_write().is_ok(),
            "spawn read permit must be released after the gated section"
        );
    }
}

#[cfg(test)]
mod path_resolve_tests {
    use super::{
        build_agent_args, build_agent_args_with_ephemeral_env, extend_search_path,
        inject_local_adapter_cli_path, resolve_in_path,
    };
    use crate::agent_kind::ConversationLaunchSpec;
    use std::collections::BTreeMap;

    #[test]
    fn absolute_or_slashed_returned_asis() {
        // 带斜杠的（绝对路径 / 受管 bun 全路径）不查 PATH，原样返回。
        assert_eq!(
            resolve_in_path("/usr/bin/env", "/nonexistent").as_deref(),
            Some("/usr/bin/env")
        );
        assert_eq!(resolve_in_path("./x", "/bin").as_deref(), Some("./x"));
    }

    fn temp_root(label: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "smelt-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// 写成当前系统会认的可执行文件：Unix 要执行位，Windows 无扩展名时是 `.exe`。
    fn write_executable(dir: &std::path::Path, bare: &str) -> std::path::PathBuf {
        let file_name = if cfg!(windows) && !bare.contains('.') {
            format!("{bare}.exe")
        } else {
            bare.to_string()
        };
        let exe = dir.join(file_name);
        std::fs::write(&exe, b"x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&exe).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&exe, permissions).unwrap();
        }
        exe
    }

    fn path_list(dirs: &[std::path::PathBuf]) -> String {
        let sep = if cfg!(windows) { ";" } else { ":" };
        dirs.iter()
            .map(|dir| dir.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(sep)
    }

    #[test]
    fn finds_executable_across_path_dirs() {
        // 命令放在最后一段，前面两段是空目录，验证会逐段找下去。
        let root = temp_root("path");
        let missing_a = root.join("no");
        let missing_b = root.join("also");
        let bin = root.join("bin");
        std::fs::create_dir_all(&missing_a).unwrap();
        std::fs::create_dir_all(&missing_b).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let exe = write_executable(&bin, "sh");
        let path = path_list(&[missing_a, missing_b, bin]);
        assert_eq!(
            resolve_in_path("sh", &path).as_deref(),
            exe.to_str(),
            "前面的空目录跳过，命中后面目录里的命令"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_bare_name_prefers_exe_then_cmd() {
        let root = std::env::temp_dir().join(format!(
            "smelt-cmd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.to_string_lossy().into_owned();
        std::fs::write(root.join("claude.cmd"), "@echo off\r\n").unwrap();
        let found = resolve_in_path("claude", &path).expect("cmd");
        assert!(
            found.to_ascii_lowercase().ends_with("claude.cmd"),
            "{found}"
        );
        std::fs::write(root.join("claude.exe"), "MZ").unwrap();
        let found = resolve_in_path("claude", &path).expect("exe");
        assert!(
            found.to_ascii_lowercase().ends_with("claude.exe"),
            "{found}"
        );
        std::fs::write(root.join("grok"), "bare").unwrap();
        let found = resolve_in_path("grok", &path).expect("bare");
        assert!(found.to_ascii_lowercase().ends_with("grok"), "{found}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 追加目录要按平台的分隔符拼。Windows 上用 `:` 拼会把登录 PATH 的最后一段
    /// 和追加的目录粘成一个不存在的目录。
    #[test]
    fn extended_search_path_keeps_every_entry_separable() {
        let root = temp_root("extend");
        let login_dirs = [root.join("a"), root.join("b")];
        let home = root.join("home");
        let extended = extend_search_path(&path_list(&login_dirs), Some(&home));
        let entries: Vec<_> = std::env::split_paths(&extended).collect();
        assert_eq!(&entries[..2], &login_dirs[..]);
        assert!(entries.contains(&home.join(".cargo/bin")), "{extended}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_command_returns_none() {
        // 找不到就返回 None，让调用方保留原名、把真实 spawn 错误报出来。
        assert!(resolve_in_path("definitely-not-a-real-cmd-xyz", "/bin:/usr/bin").is_none());
    }

    #[test]
    fn build_agent_args_preserves_plain_launch_specs() {
        let args = build_agent_args(
            &ConversationLaunchSpec::from_command("/bin/sh -lc echo"),
            "/bin:/usr/bin",
        )
        .expect("plain launch spec should build");

        assert_eq!(
            args,
            vec![
                "PATH=/bin:/usr/bin".to_string(),
                "/bin/sh".to_string(),
                "-lc".to_string(),
                "echo".to_string(),
            ]
        );
    }

    #[test]
    fn build_agent_args_overlays_structured_env_on_legacy_prefixes() {
        let launch =
            ConversationLaunchSpec::from_command("CLAUDE_CONFIG_DIR=/legacy/path /bin/sh --version")
                .with_env(
                    "CLAUDE_CONFIG_DIR",
                    "~/Library/Application Support/Claude Quant",
                )
                .with_env("XDG_CONFIG_HOME", "~/.config");

        let args = build_agent_args(&launch, "/bin:/usr/bin").expect("launch args");
        let home = dirs::home_dir().unwrap();
        let structured_config = format!(
            "CLAUDE_CONFIG_DIR={}/Library/Application Support/Claude Quant",
            home.display()
        );
        let structured_xdg = format!("XDG_CONFIG_HOME={}/.config", home.display());

        assert_eq!(args.first().map(String::as_str), Some("PATH=/bin:/usr/bin"));
        assert!(args[1..3].contains(&structured_config));
        assert!(args[1..3].contains(&structured_xdg));
        assert_eq!(args[3], "/bin/sh");
        assert_eq!(args[4], "--version");
    }

    #[test]
    fn ephemeral_env_overlays_launch_env_without_mutating_the_saved_spec() {
        let launch = ConversationLaunchSpec::from_command("/bin/sh --version")
            .with_env("PLUGIN_TOKEN", "stored-value")
            .with_env("UNCHANGED", "yes");
        let ephemeral =
            BTreeMap::from([("PLUGIN_TOKEN".to_string(), "remote-task-value".to_string())]);

        let args = build_agent_args_with_ephemeral_env(&launch, &ephemeral, "/bin:/usr/bin")
            .expect("launch args");

        assert!(args.contains(&"PLUGIN_TOKEN=remote-task-value".to_string()));
        assert!(args.contains(&"UNCHANGED=yes".to_string()));
        assert_eq!(launch.env["PLUGIN_TOKEN"], "stored-value");
    }

    #[cfg(unix)]
    #[test]
    fn official_codex_adapter_prefers_local_codex_cli() {
        use std::os::unix::fs::PermissionsExt;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("smelt-codex-path-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let codex = dir.join("codex");
        std::fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&codex).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&codex, permissions).unwrap();

        let mut env = BTreeMap::new();
        inject_local_adapter_cli_path(
            "bunx --bun @agentclientprotocol/codex-acp@1.12.0",
            "@agentclientprotocol/codex-acp",
            "SMELT_TEST_CODEX_PATH",
            "codex",
            &mut env,
            dir.to_str().unwrap(),
        );

        assert_eq!(
            env.get("SMELT_TEST_CODEX_PATH"),
            Some(&codex.to_string_lossy().into_owned())
        );
        std::fs::remove_file(codex).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn explicit_codex_path_is_not_overridden() {
        let mut env = BTreeMap::from([("CODEX_PATH".to_string(), "/custom/codex".to_string())]);

        inject_local_adapter_cli_path(
            "bunx --bun @agentclientprotocol/codex-acp@1.12.0",
            "@agentclientprotocol/codex-acp",
            "CODEX_PATH",
            "codex",
            &mut env,
            "/opt/homebrew/bin",
        );

        assert_eq!(
            env.get("CODEX_PATH").map(String::as_str),
            Some("/custom/codex")
        );
    }

    #[test]
    fn official_claude_adapter_prefers_local_claude_cli() {
        let root = temp_root("claude");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = write_executable(&bin, "sh");
        let mut env = BTreeMap::new();

        inject_local_adapter_cli_path(
            "bunx --bun @agentclientprotocol/claude-agent-acp@0.78.0",
            "@agentclientprotocol/claude-agent-acp",
            "CLAUDE_CODE_EXECUTABLE",
            "sh",
            &mut env,
            bin.to_str().unwrap(),
        );

        assert_eq!(
            env.get("CLAUDE_CODE_EXECUTABLE").map(String::as_str),
            exe.to_str()
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod image_block_tests {
    use super::{ContentBlock, ImageContent};

    /// 图片 block 的 wire 形状：`{"type":"image","data":<b64>,"mimeType":...}`。
    /// 实测这个形状 Copilot 能正确读图（发纯红图问颜色，答「红色」）——序列化
    /// 一旦偏了（比如 mimeType 变 mime_type），agent 收到的就是废数据，
    /// 而且不会报错，只会答得驴唇不对马嘴。
    #[test]
    fn image_block_wire_shape() {
        let block = ContentBlock::Image(ImageContent::new("QUJD", "image/png"));
        let v = serde_json::to_value(&block).expect("序列化");
        assert_eq!(v["type"], "image");
        assert_eq!(v["data"], "QUJD");
        assert_eq!(v["mimeType"], "image/png");
    }
}

#[cfg(test)]
mod config_queue_tests {
    use super::*;

    #[test]
    fn keeps_only_the_latest_value_for_a_config_id() {
        let mut pending = Vec::new();
        let in_flight = AtomicUsize::new(3);

        queue_config_option(
            &mut pending,
            "mode".to_string(),
            ConfigValue::Select("ask".to_string()),
            &in_flight,
        );
        queue_config_option(
            &mut pending,
            "mode".to_string(),
            ConfigValue::Select("full".to_string()),
            &in_flight,
        );
        queue_config_option(
            &mut pending,
            "model".to_string(),
            ConfigValue::Select("sonnet".to_string()),
            &in_flight,
        );

        assert_eq!(
            pending,
            vec![
                ("mode".to_string(), ConfigValue::Select("full".to_string())),
                (
                    "model".to_string(),
                    ConfigValue::Select("sonnet".to_string())
                ),
            ]
        );
        assert_eq!(in_flight.load(Ordering::SeqCst), pending.len());
    }
}

#[cfg(test)]
mod model_tests {
    use agent_client_protocol::schema::v1::{
        NewSessionResponse, SessionConfigId, SessionConfigKind, SessionConfigOption,
        SessionConfigOptionCategory, SessionConfigSelect, SessionConfigSelectOption,
        SessionConfigSelectOptions,
    };

    use agent_client_protocol::schema::v1::SessionConfigValueId;

    fn opt(value: &str, name: &str) -> SessionConfigSelectOption {
        SessionConfigSelectOption::new(
            SessionConfigValueId::new(value.to_string()),
            name.to_string(),
        )
    }

    fn model_option(current: &str, options: SessionConfigSelectOptions) -> SessionConfigOption {
        SessionConfigOption::new(
            SessionConfigId::new("model".to_string()),
            "Model".to_string(),
            SessionConfigKind::Select(SessionConfigSelect::new(
                SessionConfigValueId::new(current.to_string()),
                options,
            )),
        )
        .category(SessionConfigOptionCategory::Model)
    }

    /// 取的是给人看的 name，不是值 id。
    #[test]
    fn picks_human_readable_name_of_current_value() {
        let opts = vec![model_option(
            "sonnet-4-5",
            SessionConfigSelectOptions::Ungrouped(vec![
                opt("opus-4-8", "Claude Opus 4.8"),
                opt("sonnet-4-5", "Claude Sonnet 4.5"),
            ]),
        )];
        let state = super::model_from_config(&opts).expect("应解析出模型项");
        assert_eq!(state.config_id, "model");
        assert_eq!(state.current_value, "sonnet-4-5");
        assert_eq!(state.current_name, "Claude Sonnet 4.5");
        // 候选要带全，UI 靠它渲染下拉
        assert_eq!(state.options.len(), 2);
        assert!(
            state
                .options
                .iter()
                .any(|(v, n)| v == "opus-4-8" && n == "Claude Opus 4.8")
        );
    }

    /// 选项按厂商/档位分组时同样要能翻出来。
    #[test]
    fn looks_inside_grouped_options() {
        use agent_client_protocol::schema::v1::{SessionConfigGroupId, SessionConfigSelectGroup};
        let group = SessionConfigSelectGroup::new(
            SessionConfigGroupId::new("anthropic".to_string()),
            "Anthropic".to_string(),
            vec![opt("haiku-4-5", "Claude Haiku 4.5")],
        );
        let opts = vec![model_option(
            "haiku-4-5",
            SessionConfigSelectOptions::Grouped(vec![group]),
        )];
        let state = super::model_from_config(&opts).expect("分组里也该翻得出来");
        assert_eq!(state.current_name, "Claude Haiku 4.5");
        assert_eq!(state.options.len(), 1);
        assert_eq!(state.provider_groups.len(), 1);
        assert_eq!(state.provider_groups[0].id, "anthropic");
        assert_eq!(state.provider_groups[0].name, "Anthropic");
        assert_eq!(
            state.provider_groups[0].options,
            vec![("haiku-4-5".to_string(), "Claude Haiku 4.5".to_string())]
        );
    }

    /// 没有 Model 分类的配置项 → None，UI 就不显示模型胶囊（不瞎猜）。
    #[test]
    fn returns_none_without_model_category() {
        let other = SessionConfigOption::new(
            SessionConfigId::new("mode".to_string()),
            "Mode".to_string(),
            SessionConfigKind::Select(SessionConfigSelect::new(
                SessionConfigValueId::new("ask".to_string()),
                SessionConfigSelectOptions::Ungrouped(vec![opt("ask", "Ask")]),
            )),
        )
        .category(SessionConfigOptionCategory::Mode);
        assert!(super::model_from_config(&[other]).is_none());
    }

    #[test]
    fn exposes_non_model_select_configs_for_all_adapters() {
        let mode = SessionConfigOption::new(
            SessionConfigId::new("mode".to_string()),
            "Mode".to_string(),
            SessionConfigKind::Select(SessionConfigSelect::new(
                SessionConfigValueId::new("agent".to_string()),
                SessionConfigSelectOptions::Ungrouped(vec![
                    opt("agent", "Agent"),
                    opt("agent-full-access", "Agent (full access)"),
                ]),
            )),
        )
        .description("Approval and sandboxing preset".to_string())
        .category(SessionConfigOptionCategory::Mode);
        let configs = super::session_configs_from_config(&[mode]);
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].config_id, "mode");
        assert_eq!(configs[0].current_name, "Agent");
        assert_eq!(configs[0].options.len(), 2);
        assert_eq!(configs[0].boolean, None);
    }

    #[test]
    fn exposes_boolean_configs_as_on_off_choices() {
        let fast = SessionConfigOption::boolean(
            SessionConfigId::new("fast".to_string()),
            "Fast mode".to_string(),
            true,
        );
        let configs = super::session_configs_from_config(&[fast]);
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].boolean, Some(true));
        assert_eq!(configs[0].current_name, "开");
        assert_eq!(
            configs[0].options,
            vec![
                ("true".to_string(), "开".to_string()),
                ("false".to_string(), "关".to_string())
            ]
        );
    }

    /// Kiro 2.20 v3 ACP 的真实 session/new 形状：select 项带 description/_meta，
    /// 没有 models 字段，配置在 configOptions 里。解析失败就会让输入栏只剩
    /// 「Kiro」静态标签、看不到 Mode/Autopilot。
    #[test]
    fn kiro_session_new_config_options_round_trip_to_composer_state() {
        let json = serde_json::json!({
            "sessionId": "sess_test",
            "modes": {
                "currentModeId": "vibe",
                "availableModes": [
                    {"id": "vibe", "name": "Default", "description": "General coding assistance"}
                ]
            },
            "configOptions": [
                {
                    "type": "select",
                    "id": "mode",
                    "name": "Mode",
                    "category": "mode",
                    "currentValue": "vibe",
                    "options": [
                        {
                            "value": "vibe",
                            "name": "Default",
                            "description": "General coding assistance",
                            "_meta": {"kiro": {"source": "bundled"}}
                        },
                        {
                            "value": "plan",
                            "name": "Plan",
                            "description": "Plan-only mode"
                        }
                    ]
                },
                {
                    "type": "select",
                    "id": "autopilot",
                    "name": "Autopilot",
                    "currentValue": "on",
                    "options": [
                        {"value": "on", "name": "Autopilot"},
                        {"value": "off", "name": "Supervised"}
                    ]
                }
            ]
        });

        let parsed: Result<NewSessionResponse, _> = serde_json::from_value(json);
        let parsed = parsed.expect("Kiro session/new 应能反序列化");
        let options = parsed
            .config_options
            .as_deref()
            .expect("configOptions 不能丢成 None");
        assert_eq!(options.len(), 2, "select 项不能被 VecSkipError 整段丢掉");
        assert!(super::model_from_config(options).is_none());
        let configs = super::session_configs_from_config(options);
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].config_id, "mode");
        assert_eq!(configs[0].current_name, "Default");
        assert_eq!(configs[0].options.len(), 2);
        assert_eq!(configs[1].config_id, "autopilot");
        assert_eq!(configs[1].options.len(), 2);
    }

    #[test]
    fn raw_kiro_session_new_line_exposes_mode_and_keeps_selects() {
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "sessionId": "sess_test",
                "modes": {
                    "currentModeId": "vibe",
                    "availableModes": [
                        {"id": "vibe", "name": "Default"},
                        {"id": "plan", "name": "Plan"}
                    ]
                },
                "configOptions": [
                    {
                        "type": "select",
                        "id": "mode",
                        "name": "Mode",
                        "category": "mode",
                        "currentValue": "vibe",
                        "options": [
                            {"value": "vibe", "name": "Default"},
                            {"value": "plan", "name": "Plan"}
                        ]
                    },
                    {
                        "type": "select",
                        "id": "autopilot",
                        "name": "Autopilot",
                        "currentValue": "on",
                        "options": [
                            {"value": "on", "name": "Autopilot"},
                            {"value": "off", "name": "Supervised"}
                        ]
                    }
                ]
            }
        })
        .to_string();

        let options = super::session_config_options_from_rpc_line(&line).expect("应解析出配置");
        let configs = super::session_configs_from_config(&options);
        assert!(
            configs
                .iter()
                .any(|c| c.config_id == "mode" && c.options.len() > 1)
        );
        assert!(
            configs
                .iter()
                .any(|c| c.config_id == "autopilot" && c.options.len() == 2)
        );
    }

    #[test]
    fn raw_kiro_config_option_update_exposes_model_after_async_list() {
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess_test",
                "update": {
                    "sessionUpdate": "config_option_update",
                    "configOptions": [
                        {
                            "type": "select",
                            "id": "mode",
                            "name": "Mode",
                            "category": "mode",
                            "currentValue": "vibe",
                            "options": [
                                {"value": "vibe", "name": "Default"},
                                {"value": "plan", "name": "Plan"}
                            ]
                        },
                        {
                            "type": "select",
                            "id": "model",
                            "name": "Model",
                            "category": "model",
                            "currentValue": "auto",
                            "options": [
                                {
                                    "value": "auto",
                                    "name": "Auto",
                                    "_meta": {"kiro": {"rateMultiplier": 1, "hasEffort": false}}
                                },
                                {"value": "claude-sonnet-4.5", "name": "Claude Sonnet 4.5"}
                            ]
                        }
                    ]
                }
            }
        })
        .to_string();

        let options = super::session_config_options_from_rpc_line(&line).expect("应解析出后续模型");
        let model = super::model_from_config(&options).expect("model 分类应进输入栏");
        assert_eq!(model.current_name, "Auto");
        assert_eq!(model.options.len(), 2);
    }

    #[test]
    fn pi_thought_level_option_is_not_duplicated_by_the_modes_surface() {
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "sessionId": "pi-session",
                "modes": {
                    "currentModeId": "medium",
                    "availableModes": [
                        {"id": "off", "name": "Thinking: off"},
                        {"id": "minimal", "name": "Thinking: minimal"},
                        {"id": "low", "name": "Thinking: low"},
                        {"id": "medium", "name": "Thinking: medium"},
                        {"id": "high", "name": "Thinking: high"},
                        {"id": "xhigh", "name": "Thinking: xhigh"}
                    ]
                },
                "configOptions": [{
                    "type": "select",
                    "id": "thought_level",
                    "name": "Thinking",
                    "category": "thought_level",
                    "currentValue": "medium",
                    "options": [
                        {"value": "off", "name": "Thinking: off"},
                        {"value": "minimal", "name": "Thinking: minimal"},
                        {"value": "low", "name": "Thinking: low"},
                        {"value": "medium", "name": "Thinking: medium"},
                        {"value": "high", "name": "Thinking: high"},
                        {"value": "xhigh", "name": "Thinking: xhigh"}
                    ]
                }]
            }
        })
        .to_string();

        let options = super::session_config_options_from_rpc_line(&line)
            .expect("Pi session/new 应保留 Thinking 配置");
        let configs = super::session_configs_from_config(&options);
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].config_id, "thought_level");
        assert_eq!(configs[0].name, "Thinking");
        assert_eq!(configs[0].options.len(), 6);
    }

    #[test]
    fn distinct_thought_and_mode_surfaces_are_both_kept() {
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "sessionId": "mixed-session",
                "modes": {
                    "currentModeId": "agent",
                    "availableModes": [
                        {"id": "agent", "name": "Agent"},
                        {"id": "plan", "name": "Plan"}
                    ]
                },
                "configOptions": [{
                    "type": "select",
                    "id": "thought_level",
                    "name": "Thinking",
                    "category": "thought_level",
                    "currentValue": "medium",
                    "options": [
                        {"value": "low", "name": "Low"},
                        {"value": "medium", "name": "Medium"}
                    ]
                }]
            }
        })
        .to_string();

        let options = super::session_config_options_from_rpc_line(&line)
            .expect("不同的 modes 和 thought_level 都应保留");
        let configs = super::session_configs_from_config(&options);
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].config_id, "mode");
        assert_eq!(configs[1].config_id, "thought_level");
    }

    #[test]
    fn modes_fill_in_when_config_options_are_missing() {
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "sessionId": "sess_test",
                "modes": {
                    "currentModeId": "vibe",
                    "availableModes": [
                        {"id": "vibe", "name": "Default"},
                        {"id": "plan", "name": "Plan"}
                    ]
                }
            }
        })
        .to_string();

        let options =
            super::session_config_options_from_rpc_line(&line).expect("modes 应兜底成选项");
        let configs = super::session_configs_from_config(&options);
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].config_id, "mode");
        assert_eq!(configs[0].current_name, "Default");
        assert_eq!(configs[0].options.len(), 2);
    }

    #[test]
    fn empty_config_option_update_is_ignored_instead_of_wiping_ui() {
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "sess_test",
                "update": {
                    "sessionUpdate": "config_option_update",
                    "configOptions": []
                }
            }
        })
        .to_string();
        assert!(super::session_config_options_from_rpc_line(&line).is_none());
    }
}

#[cfg(all(test, unix))]
mod process_reap_tests {
    use super::{
        Waitpid, kill_and_reap_process_group, process_birth, process_start_time,
        prove_process_group_dead, waitpid_child,
    };
    use std::os::unix::process::CommandExt as _;
    use std::process::Command;
    use std::time::Duration;

    fn spawn_sleep() -> (std::process::Child, i32) {
        let child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        (child, pid)
    }

    fn process_exists(pid: i32) -> bool {
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// 造一个“收养”进程组：double-fork 后重定父到 launchd，调用方永远不是
    /// 其父进程（`waitpid` 永 ECHILD），只能走信号探针路径——模拟 handoff 后
    /// successor 看 adopted provider 的视角。只用 async-signal-safe 调用，
    /// 在多线程测试进程里 fork 是安全的。
    fn spawn_orphaned_sleep() -> i32 {
        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let first = unsafe { libc::fork() };
        assert!(first >= 0, "fork 失败");
        if first == 0 {
            // 中间进程：只 fork + _exit，不碰任何锁。
            let second = unsafe { libc::fork() };
            if second == 0 {
                unsafe {
                    libc::setsid();
                    libc::close(pipe[0]);
                    let pid = libc::getpid();
                    let _ = libc::write(
                        pipe[1],
                        (&pid as *const libc::pid_t).cast(),
                        std::mem::size_of::<libc::pid_t>(),
                    );
                    libc::close(pipe[1]);
                    // 绝对路径 execv：execlp 的 PATH 搜索要调 malloc，在多线程
                    // 进程 fork 出来的子进程里不安全。失败就 _exit（父端读到
                    // pid 但进程已死——测试会因此失败而非误过）。
                    let sleep = c"/bin/sleep".as_ptr();
                    let arg = c"30".as_ptr();
                    let argv = [sleep, arg, std::ptr::null::<libc::c_char>()];
                    libc::execv(sleep, argv.as_ptr());
                    libc::_exit(127);
                }
            }
            unsafe { libc::_exit(0) };
        }
        unsafe { libc::close(pipe[1]) };
        let mut pid: libc::pid_t = 0;
        let mut read_bytes = 0;
        while read_bytes < std::mem::size_of::<libc::pid_t>() {
            let n = unsafe {
                libc::read(
                    pipe[0],
                    (&mut pid as *mut libc::pid_t)
                        .cast::<libc::c_void>()
                        .byte_add(read_bytes),
                    std::mem::size_of::<libc::pid_t>() - read_bytes,
                )
            };
            assert!(n > 0, "读孙进程 pid 失败");
            read_bytes += n as usize;
        }
        unsafe { libc::close(pipe[0]) };
        let mut status = 0;
        unsafe { libc::waitpid(first, &mut status, 0) };
        assert!(pid > 1, "孙进程 pid 非法");
        // 确认真是“收养”关系：不是我们的 child。
        assert!(
            matches!(waitpid_child(pid, libc::WNOHANG), Waitpid::NotOurChild),
            "孤儿必须已重定父，waitpid 应 ECHILD"
        );
        pid
    }

    /// SIGKILL 本身不是收尸：不 waitpid 的话子进程会变成僵尸，kill(pid,0) 仍成功。
    #[test]
    fn sigkill_without_waitpid_leaves_a_zombie() {
        let (mut child, pid) = spawn_sleep();
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            process_exists(pid),
            "僵尸仍占着 pid，kill(pid,0) 不能当 Stopped"
        );
        assert!(
            matches!(waitpid_child(pid, libc::WNOHANG), Waitpid::Reaped),
            "waitpid 才能收掉这个 child"
        );
        let _ = child.try_wait();
        assert!(!process_exists(pid));
    }

    #[test]
    fn kill_and_reap_reaps_the_direct_child() {
        let (mut child, pid) = spawn_sleep();
        assert!(kill_and_reap_process_group(pid, Duration::from_secs(2)));
        assert!(!process_exists(pid));
        assert!(matches!(
            waitpid_child(pid, libc::WNOHANG),
            Waitpid::NotOurChild
        ));
        let _ = child.try_wait();
    }

    #[test]
    fn kill_and_reap_foreign_pid_is_already_gone() {
        assert!(kill_and_reap_process_group(
            i32::MAX,
            Duration::from_millis(10)
        ));
    }

    /// 陌生但活着的 pid 判失败（fail closed），且绝不能杀到自己。
    /// 组号 N 存在当且仅当 pid N 是组长：cargo 下测试进程不是组长，
    /// kill(-self) 必 ESRCH 伤不到任何人；但 leader 本人活着，prove 按
    /// “活着即未证伪”返回 false——调用方不得 spawn（与 regrouped-leader
    /// 边同形，保守即正确）。直接跑二进制时自己是组长，组探针同样判 false。
    #[test]
    fn kill_and_reap_live_foreign_pid_fails_closed() {
        let me = std::process::id() as i32;
        if unsafe { libc::getpgrp() } == me {
            // 直接跑测试二进制时自己是组长：kill(-me) 等于自杀，跳过
            // （cargo test 下永不命中——cargo 才是组长）。
            return;
        }
        assert!(!kill_and_reap_process_group(me, Duration::from_millis(50)));
        assert!(process_exists(me));
    }

    /// 收养组照杀照证：非亲生（ECHILD）不再直接 true，必须真把组杀掉。
    /// 这是 handoff 后 successor 杀 adopted provider 的同一条路。
    #[test]
    fn kill_and_reap_kills_adopted_group() {
        let pid = spawn_orphaned_sleep();
        assert!(process_exists(pid));
        assert!(
            kill_and_reap_process_group(pid, Duration::from_secs(5)),
            "收养组应被杀掉并探得缺席"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while process_exists(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "孤儿 sleep 应已死（launchd 收尸可能慢半拍）"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// prove 是纯验证：活着的陌生组返回 false，且绝不发信号。
    /// 用测试进程自己的组当“活陌生组”（waitpid 对它必 ECHILD）。
    #[test]
    fn prove_finds_live_foreign_group_without_signaling() {
        let pgid = unsafe { libc::getpgrp() };
        assert!(!prove_process_group_dead(pgid, Duration::from_millis(100)));
        // 还活着（废话）——重点是上面那行没杀任何东西：100ms 超时立刻返回，
        // 测试进程与同组兄弟都安然无恙。
        assert!(process_exists(std::process::id() as i32));
    }

    /// prove 对已死的陌生号立刻 true（不傻等超时）。
    #[test]
    fn prove_dead_foreign_pid_returns_immediately() {
        let start = std::time::Instant::now();
        assert!(prove_process_group_dead(i32::MAX, Duration::from_secs(5)));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "ESRCH 应立刻返回，不应等满超时"
        );
    }

    /// 亲生进程自己退出时，要在长截止之前被等到，并收走。
    #[test]
    fn own_child_exit_is_proven_before_a_long_deadline() {
        let child = Command::new("sh")
            .args(["-c", "sleep 0.2"])
            .process_group(0)
            .spawn()
            .expect("spawn");
        let pid = child.id() as i32;
        let started = std::time::Instant::now();
        assert!(
            prove_process_group_dead(pid, Duration::from_secs(5)),
            "进程退出后应马上证明组已空"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "不应干等到截止，实际 {:?}",
            started.elapsed()
        );
        assert!(!process_exists(pid));
        let _ = child.wait_with_output();
    }

    /// 还在跑的亲生进程，短截止内不能被说成已死，也不能被误杀。
    #[test]
    fn running_child_is_not_proven_dead_before_the_deadline() {
        let (child, pid) = spawn_sleep();
        let started = std::time::Instant::now();
        assert!(!prove_process_group_dead(pid, Duration::from_millis(200)));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(process_exists(pid));
        assert!(kill_and_reap_process_group(pid, Duration::from_secs(2)));
        let _ = child.wait_with_output();
    }

    /// 观察用的证明不能把还没被原来的 wait 收走的僵尸抢走。
    /// 抢走的话，这里会返回 true，随后 `child.wait()` 得到 ECHILD。
    #[test]
    fn prove_without_reaping_leaves_the_zombie_for_its_waiter() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 0.2"])
            .process_group(0)
            .spawn()
            .expect("spawn");
        let pid = child.id() as i32;
        assert!(
            crate::process_wait::wait_until_exit(pid, Duration::from_secs(2)),
            "应先看到退出，并且不收尸"
        );
        let started = std::time::Instant::now();
        assert!(
            !super::prove_process_group_dead_without_reaping(pid, Duration::from_millis(200)),
            "僵尸对 kill 仍算在，不能当成已经收走"
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        let status = child.wait().expect("原来的 wait 必须还能收走这个僵尸");
        assert!(status.success());
    }

    /// 出生标识：同一进程反复读不变，不同进程不同，死进程读不出。
    #[test]
    fn process_birth_is_stable_per_process_and_distinct_across_processes() {
        let (_a, a) = spawn_sleep();
        std::thread::sleep(Duration::from_millis(50));
        let (_b, b) = spawn_sleep();
        let first = process_birth(a).expect("活进程应读得出");
        assert_eq!(process_birth(a).as_deref(), Some(first.as_str()));
        assert_ne!(process_birth(b), Some(first));
        assert_eq!(process_birth(i32::MAX), None);
    }

    /// 启动时刻贴着墙钟：刚 spawn 的进程，启动时刻离现在不超过一个 tick 量级。
    #[test]
    fn process_start_time_tracks_the_wall_clock() {
        let before = std::time::SystemTime::now();
        let (_a, a) = spawn_sleep();
        let started = process_start_time(a).unwrap();
        let slack = Duration::from_millis(50);
        assert!(
            started + slack >= before,
            "{started:?} 早于 spawn 前的 {before:?}"
        );
        assert!(started <= std::time::SystemTime::now() + slack);
    }

    /// 启动时间单调：后 spawn 的更大；死进程读不出。
    #[test]
    fn process_start_time_orders_births_and_rejects_the_dead() {
        let (_a, a) = spawn_sleep();
        std::thread::sleep(Duration::from_millis(50));
        let (_b, b) = spawn_sleep();
        let (ta, tb) = (process_start_time(a), process_start_time(b));
        assert!(ta.is_some() && tb.is_some());
        assert!(ta.unwrap() < tb.unwrap(), "后生的启动时间必须更大");
        assert!(ta.unwrap() <= std::time::SystemTime::now());
        assert_eq!(process_start_time(i32::MAX), None);
        assert_eq!(process_start_time(0), None);
        assert_eq!(process_start_time(-5), None);
        // 清理：两个 sleep 都是亲生，直接杀。
        assert!(kill_and_reap_process_group(a, Duration::from_secs(2)));
        assert!(kill_and_reap_process_group(b, Duration::from_secs(2)));
    }
}

#[cfg(test)]
mod prompt_failure_tests {
    use super::*;

    fn err(
        code: i32,
        message: &str,
        data: Option<serde_json::Value>,
    ) -> agent_client_protocol::Error {
        let mut e = agent_client_protocol::Error::internal_error();
        e.code = code.into();
        e.message = message.to_string();
        e.data = data;
        e
    }

    /// agent 常把真正有用的话（缺哪个环境变量、怎么补）放在 message 里，
    /// 而 code 只是个 -32603。只报 code 等于什么都没说。
    #[test]
    fn prompt_error_keeps_the_actionable_sentence() {
        let e = err(
            -32603,
            "Internal error: turn failed: llm-deepseek: no API key for provider route \"deepseek-official\"; export DEEPSEEK_API_KEY in the launching environment",
            None,
        );
        let text = describe_prompt_error(&e);
        assert!(text.contains("DEEPSEEK_API_KEY"), "{text}");
        assert!(text.contains("no API key"), "{text}");
    }

    /// 细节在 data 里的 agent 也有；两处都要能取到。
    #[test]
    fn prompt_error_picks_up_detail_from_data() {
        let e = err(
            -32603,
            "Internal error",
            Some(serde_json::json!("rate limit exceeded, retry in 30s")),
        );
        let text = describe_prompt_error(&e);
        assert!(text.contains("rate limit exceeded"), "{text}");
    }

    /// data 只是 message 的复读时不要显示两遍。
    #[test]
    fn prompt_error_does_not_duplicate_message_and_data() {
        let e = err(-32603, "boom", Some(serde_json::json!("boom")));
        assert_eq!(describe_prompt_error(&e), "boom");
    }

    /// agent 什么都不说时也得给出一句能搜的话，不能是空字符串。
    #[test]
    fn prompt_error_never_renders_empty() {
        let e = err(-32000, "  ", None);
        let text = describe_prompt_error(&e);
        assert!(
            !text.trim().is_empty(),
            "空错误会渲染成一张什么都没有的卡片"
        );
        // ErrorCode 的 Debug 认得的码打名字（比裸数字有用），不认得的打数字。
        assert!(text.contains("Authentication required"), "{text}");
        assert!(text.contains("-32000"), "码要能直接搜：{text}");
        let unknown = err(-31999, "", None);
        assert!(describe_prompt_error(&unknown).contains("-31999"));
    }

    /// 用户看到的第一句必须是「我现在该做什么」。agent 的原文常是英文长句，
    /// 还可能指向 smelt 根本没有的页面（如「web Models page」）——照抄等于把人
    /// 支到不存在的地方。
    #[test]
    fn missing_key_error_points_at_smelt_settings() {
        let e = err(
            -32603,
            "Internal error: turn failed: llm-deepseek: no API key for provider route \"deepseek-official\"; store DEEPSEEK_API_KEY through the credentials service (the web Models page writes it)",
            None,
        );
        let text = describe_prompt_error(&e);
        assert!(text.contains("设置"), "要指到设置页：{text}");
        assert!(text.contains("环境变量"), "要指到具体那个输入框：{text}");
        // 原文仍然保留：诊断信息不能因为「友好」就被吞掉。
        assert!(text.contains("no API key"), "{text}");
    }

    /// 认不出来的错误就老老实实只给原文，别硬编一条可能是错的建议。
    #[test]
    fn unrecognised_error_gets_no_invented_advice() {
        let e = err(-32603, "tool `bash` exited with code 3", None);
        let text = describe_prompt_error(&e);
        assert_eq!(text, "tool `bash` exited with code 3");
    }

    /// 「没配 key」和「key 不对」是两件事，建议不能串。
    #[test]
    fn invalid_key_and_missing_key_get_different_advice() {
        let missing = describe_prompt_error(&err(-32603, "no api key configured", None));
        let invalid = describe_prompt_error(&err(-32000, "invalid api key provided", None));
        assert!(missing.contains("还没配"), "{missing}");
        assert!(invalid.contains("被服务端拒绝"), "{invalid}");
    }

    /// 回合结束的判定只有一份：新增结束方式时别再让每个消费点各自 `matches!`。
    #[test]
    fn both_turn_terminators_end_the_turn() {
        assert!(ConversationEvent::TurnEnded(StopReason::EndTurn).ends_turn());
        assert!(ConversationEvent::TurnFailed("x".into()).ends_turn());
        assert!(!ConversationEvent::Fatal("x".into()).ends_turn());
        assert!(!ConversationEvent::Status("x".into()).ends_turn());
        assert!(!ConversationEvent::PromptAccepted.ends_turn());
        assert!(!ConversationEvent::PromptNotAccepted.ends_turn());
        assert!(!ConversationEvent::ProviderIdle.ends_turn());
    }
}

#[cfg(test)]
mod fake_agent_tests;

#[cfg(test)]
mod runtime_diagnostics_wire_tests {
    use super::*;

    /// 诊断从运行时传到客户端后，按种类查到的还是同一份结果；对端新加的 agent
    /// 本端认不出，丢掉而不是整份解析失败。
    #[test]
    fn diagnostics_survive_the_wire_and_skip_unknown_agents() {
        let mut diagnostics = AcpRuntimeDiagnostics::default();
        let claude = RuntimeExecutable {
            program: "claude".into(),
            path: Some("/usr/local/bin/claude".into()),
            version: Some("2.0.0".into()),
            error: None,
        };
        diagnostics.record_terminal(TerminalAgentKind::Claude, claude.clone());
        diagnostics.record_agent(ConversationAgentKind::Pi, RuntimeExecutable::default());

        let mut wire = serde_json::to_value(&diagnostics).unwrap();
        wire["acp"]["future-agent"] = serde_json::json!({"program": "x"});
        let decoded: AcpRuntimeDiagnostics = serde_json::from_value(wire).unwrap();

        assert_eq!(decoded, diagnostics);
        assert_eq!(
            decoded.for_terminal(TerminalAgentKind::Claude),
            Some(&claude)
        );
    }
}

#[cfg(test)]
mod permission_view_tests {
    use super::permission_view;
    use crate::acp_session::ApprovalDetailsView;
    use agent_client_protocol::schema::v1::ToolKind;
    use serde_json::json;

    #[test]
    fn a_fetch_names_its_url() {
        let (question, details) = permission_view(
            Some("Fetch URL"),
            "t1",
            Some(ToolKind::Fetch),
            Some(&json!({"url": "https://example.com/a", "prompt": "summarize"})),
            &[],
        );
        assert_eq!(question, "Fetch URL：https://example.com/a");
        assert!(matches!(details, ApprovalDetailsView::Generic));
    }

    #[test]
    fn a_command_gets_the_command_card() {
        let (question, details) = permission_view(
            Some("Run shell"),
            "t1",
            Some(ToolKind::Execute),
            Some(&json!({"command": ["git", "push"], "cwd": "/repo"})),
            &[],
        );
        assert_eq!(question, "Run shell：git push");
        assert!(matches!(
            details,
            ApprovalDetailsView::Command { command, cwd, .. }
                if command == "git push" && cwd.as_deref() == Some("/repo")
        ));
    }

    #[test]
    fn an_edit_takes_its_file_from_the_locations() {
        let (_, details) = permission_view(
            Some("Edit"),
            "t1",
            Some(ToolKind::Edit),
            None,
            &["/repo/src/main.rs".to_string()],
        );
        assert!(matches!(
            details,
            ApprovalDetailsView::FileChange { grant_root: Some(root), .. } if root == "/repo/src/main.rs"
        ));
    }

    #[test]
    fn the_target_is_not_repeated_when_the_title_already_has_it() {
        let (question, _) = permission_view(
            Some("`git status`"),
            "t1",
            Some(ToolKind::Execute),
            Some(&json!({"command": "git status"})),
            &[],
        );
        assert_eq!(question, "`git status`");
    }

    #[test]
    fn without_a_title_or_input_it_falls_back_to_the_tool_id() {
        let (question, details) = permission_view(None, "t9", None, None, &[]);
        assert_eq!(question, "工具调用 t9");
        assert!(matches!(details, ApprovalDetailsView::Generic));
    }

    #[test]
    fn a_long_target_is_clipped() {
        let url = format!("https://example.com/{}", "a".repeat(400));
        let (question, _) = permission_view(
            Some("Fetch URL"),
            "t1",
            None,
            Some(&json!({ "url": url })),
            &[],
        );
        assert!(question.ends_with('…'));
        assert!(question.chars().count() < 320);
    }
}
