//! ACP（Agent Client Protocol）连接层：JSON-RPC over stdio 驱动跟子进程 agent
//! 的连接，不含任何 GPUI——本来就是 `smelt` crate 里 acp.rs 的原文搬过来的，
//! 移动的唯一理由是给 smeltd 托管 ACP 会话铺路（这个 crate 本来就是
//! GUI/守护共用层，smeltd 加个 `agent-client-protocol` 依赖就能直接复用这里
//! 的连接驱动逻辑，不用重写一遍）。
//!
//! 职责边界（原 acp.rs 的约定继续有效）：
//! - 每个 ACP 会话一条专用 OS 线程 `smol::block_on` 驱动整个连接（spawn 子进程、
//!   JSON-RPC over stdio、事件翻译）；
//! - 一切失败（找不到命令 / 握手失败 / 子进程退出）都以 `AcpEvent::Fatal` 从事件
//!   通道出来，`spawn_acp` 本身永不阻塞、永不 panic 调用方。

use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::sync::{Arc, Mutex, RwLock};

use std::collections::BTreeMap;

use futures::{AsyncBufReadExt, AsyncWriteExt, StreamExt};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, ClientCapabilities, ContentBlock, CreateElicitationRequest,
    CreateElicitationResponse, ElicitationAcceptAction, ElicitationAction, ElicitationCapabilities,
    ElicitationContentValue, ElicitationFormCapabilities, ElicitationMode,
    ElicitationPropertySchema, ElicitationSchema, ImageContent, InitializeRequest,
    LoadSessionRequest, MultiSelectItems, NewSessionRequest, NewSessionResponse, Plan,
    PromptRequest, PromptResponse, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionConfigId, SessionConfigKind,
    SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOptions,
    SessionConfigValueId, SessionId, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, StopReason, ToolCall, ToolCallId, ToolCallUpdate,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{
    AcpAgent, ActiveSession, Agent, Client, ConnectionTo, Lines, SessionMessage,
};

use crate::agent_kind::AcpLaunchSpec;

/// 一次 ACP 会话的启动参数。
pub struct AcpLaunch {
    /// 启动规格：命令字符串仍是现有的空白分词语义，环境变量单独结构化存储；
    /// legacy `VAR=value cmd` 前缀仍兼容，见 `build_agent`。
    pub launch: AcpLaunchSpec,
    /// 会话工作目录（newSession 的 cwd）；None 用进程当前目录。
    pub cwd: Option<String>,
    /// GUI 侧会话 id，约定 `acp-` 前缀——DaemonStates 全局 map 里靠这个前缀
    /// 与 smeltd 会话共存（见 main.rs 状态转发循环的 retain）。
    pub sid: String,
    /// 上一次连接的 agent 侧 session id：有就用 `session/load` 让 agent 重放
    /// 完整历史，重建 smeltd 的运行时消息投影。恢复失败不会静默开新会话。
    pub resume_session_id: Option<SessionId>,
    /// 旧 smeltd handoff 格式兼容字段。历史是否存在现在一律由 agent 的
    /// `session/load` 判断，Smelt 不再检查任何 agent 的私有 transcript 路径。
    pub resume_needs_transcript_check: bool,
}

/// 随 prompt 一起发出去的一张图（剪贴板粘进来的截图等）。
///
/// 协议要的就是 base64 + mime，所以在进这条通道前就编码好——连接线程不碰
/// GPUI 的图片类型，`acp.rs 不许引 gpui` 那条底线在这里同样成立。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PromptImage {
    /// `image/png` 这类 MIME。
    pub mime: String,
    /// base64 编码后的原始字节（不带 data: 前缀）。
    pub data_b64: String,
}

/// UI → 连接线程的指令。
pub enum AcpCommand {
    /// 发一轮 prompt（agent 空闲时才该发；UI 侧负责在 turn 进行中排队/禁用）。
    /// `images` 空 = 纯文本那条老路径。
    Prompt {
        text: String,
        images: Vec<PromptImage>,
    },
    /// 取消当前 turn（session/cancel 通知）。
    Cancel,
    /// 更新一项会话配置：配置和值都由 agent 的 `config_options` 上报，不能猜。
    SetConfigOption { config_id: String, value_id: String },
    /// 关闭会话：退出连接循环，随连接 drop 杀掉子进程。
    Shutdown,
}

/// 连接线程 → UI 的事件。schema 类型（ToolCall 等）原样透传，不造平行模型。
pub enum AcpEvent {
    /// 启动阶段的进度文案（下载运行时 / 拉取适配器等），Starting 横幅显示。
    Status(String),
    /// 冷恢复即将由 agent 重放完整历史。必须先清空 daemon 中的旧投影；Ready
    /// 到达时历史可能已经同步回放完，不能再在那里清空。
    HistoryReplayStarted,
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
    },
    ToolCall(ToolCall),
    ToolCallUpdate(ToolCallUpdate),
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
    /// agent 的任务计划（步骤清单 + 三态进度）：每次全量覆盖，回合态不落盘。
    /// UI 渲染成消息流上方的可折叠 PLAN 条。
    Plan(Plan),
    /// 模型状态：当前名 + 可选列表。来自会话配置项里 category=Model 的那条
    /// select；建会话时给一次，切换或 agent 侧改动时通过 ConfigOptionUpdate 再给。
    /// 取不到就一直是 None，UI 不假装知道。
    Model(ModelState),
    /// 除模型外的可选会话配置（权限模式、协作方式、推理强度、快速模式等）。
    /// agent 不上报就为空，四个 agent 共用这条 ACP 标准路径。
    ConfigOptions(Vec<SessionConfigState>),
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
        /// 最近一行）。smeltd 无缝升级时若这条会话正卡着这张审批卡，会把这行
        /// 原文一起交接过去——新进程接手继承来的 fd 后，先把这行「回放」一遍
        /// 让 SDK 重新解析出等价的 responder（绑定同一个原始请求 id），再继续
        /// 读实时字节，见 `resume_acp_from_fds`。GUI 直连路径不用这个字段。
        raw_request_line: Option<String>,
    },
    /// 用户消息的回显：`session/load` 重放历史时，agent 会把旧的用户提问也
    /// 当一条更新发回来（这是 entries 里 User 记录在 replay 场景下唯一的来源，
    /// 我们没有替它们手动 push 过）。正常 live 对话是否也会收到这个事件目前
    /// 没有把握确认，UI 侧用「等回声」状态机兼容两种可能，见 acp_view.rs。
    UserChunk(String),
    /// 会话当前可用的斜杠命令（`/compact` 这类，不是「工具」）：(名字, 说明)。
    /// 以前只存数量——一个光秃秃的「47 条命令」既点不开也没法用，等于没有。
    AvailableCommands(Vec<(String, String)>),
    /// 上下文用量：已用 / 窗口大小（token），外加本轮缓存读取量（agent 给才有）。
    /// UI 据此显示「上下文 32%」这类指示。
    Usage {
        used: u64,
        size: u64,
        cached_read: Option<u64>,
    },
    /// agent 的选择题 / 表单（AskUserQuestion 类）：UI 渲染字段，凭 responder 回填。
    Elicitation {
        message: String,
        fields: Vec<ElicitField>,
        responder: ElicitationResponder,
        /// 同 `Permission::raw_request_line`。
        raw_request_line: Option<String>,
    },
    /// 一轮 prompt 结束（含被取消）。
    TurnEnded(StopReason),
    /// 连接不可恢复地结束：启动失败 / 协议错误 / 子进程退出。带 stderr 尾巴。
    Fatal(String),
}

/// 会话是怎么接上的——决定 UI 拿本地历史怎么办。
#[derive(Clone, Copy, PartialEq)]
pub enum ReadyKind {
    /// 全新会话。本地若有旧历史，UI 插一条分割线标明「以下是新对话」。
    Fresh,
    /// `session/load` 续接：agent 会重放完整历史。不同连接实现可能在 Ready
    /// 前后投递回放通知；投影由 `HistoryReplayStarted` 提前清空，Ready 本身
    /// 不得再修改消息。
    ResumedWithReplay,
    /// smeltd 无缝升级继承 agent stdio fd：连接和完整内存快照都还在，不重放
    /// 历史。普通冷恢复不走这条，只能通过 `session/load` 重建投影。
    ResumedKeepHistory,
}

/// 模型选择状态：UI 拿它渲染「当前模型」胶囊和下拉候选。全是纯 String/Vec
/// 字段，没有 agent_client_protocol 的 schema 类型，直接可以序列化进
/// acp_session 的 wire 快照，不用另造一份 View 类型。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelState {
    /// ACP 配置项 id。写回时必须用它，不能假定各 adapter 都叫 `model`。
    pub config_id: String,
    /// 当前模型的人类可读名（`Claude Sonnet 4.5`）。
    pub current_name: String,
    /// 可选模型：(值 id, 人类可读名)。空 = agent 没给候选，UI 就只显示不给切。
    pub options: Vec<(String, String)>,
}

/// 一项由 ACP agent 声明的可选会话配置。模型单独显示，避免和输入栏的模型入口重复。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionConfigState {
    pub config_id: String,
    pub name: String,
    pub description: Option<String>,
    pub current_name: String,
    pub options: Vec<(String, String)>,
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

/// 表单字段的 UI 无关简化模型（schema 细节收在本模块，视图只见这个）。
pub struct ElicitField {
    /// accept 回填时的 key（schema properties 的键名）。
    pub key: String,
    pub title: String,
    pub kind: ElicitFieldKind,
}

pub enum ElicitFieldKind {
    /// 单选：点一个按钮。布尔字段也翻译成 是/否 两个选项。
    Select(Vec<ElicitOption>),
    /// 多选：可切换多个再提交。
    MultiSelect(Vec<ElicitOption>),
    /// 自由文本。secret 只影响客户端显示，协议回填仍是字符串。
    Text { secret: bool },
    /// 需要用户在浏览器完成的外部步骤。
    ExternalUrl(String),
}

pub struct ElicitOption {
    pub value: ElicitationContentValue,
    pub label: String,
}

/// 表单回执守卫：accept/decline 消费；**被 drop 自动回 Cancel**，agent 不会挂起。
enum ElicitationResponderInner {
    Acp(agent_client_protocol::Responder<CreateElicitationResponse>),
    External(Box<dyn FnOnce(Option<BTreeMap<String, ElicitationContentValue>>) + Send>),
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
                    kind: ElicitFieldKind::MultiSelect(options),
                })
            }
            _ => None, // Number/Integer/未知：MVP 不按钮化
        };
        match buttonized {
            Some(field) => fields.push(field),
            None if required.iter().any(|r| r == key) => return None,
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
pub struct AcpHandle {
    pub cmd_tx: smol::channel::Sender<AcpCommand>,
    pub event_rx: smol::channel::Receiver<AcpEvent>,
    /// 子进程 pid + 原始 stdin/stdout fd，spawn 成功后才会填（Fatal 前的极短
    /// 窗口是 None）。smeltd 无缝升级要用它把这两个 fd 也裸传过 exec()（跟
    /// PTY master fd 同一招），见 `resume_acp_from_fds`。GUI 直连路径用不上，
    /// 多存三个整数不加负担。
    pub stdio: Arc<Mutex<Option<AcpStdio>>>,
}

#[derive(Clone, Copy)]
pub struct AcpStdio {
    pub pid: i32,
    pub stdin_fd: std::os::unix::io::RawFd,
    pub stdout_fd: std::os::unix::io::RawFd,
}

/// App 退出前等一个已发过 Shutdown 的连接线程真正收尾。`event_rx` 所有
/// sender 都掉光（channel 关闭）只会发生在 `spawn_acp` 起的那条线程完整跑完
/// 之后——这时候线程内部持有的 agent 子进程句柄（含杀进程的 Drop）早就已经
/// 执行过了，可以放心确认子进程不会变孤儿。等不到就按 `timeout` 放行，宁可
/// 漏杀一个不听话的 agent，也不能让整个 App 退出卡死在它上面。
pub async fn wait_for_shutdown(handle: AcpHandle, timeout: std::time::Duration) {
    let AcpHandle { event_rx, .. } = handle;
    smol::future::race(async { while event_rx.recv().await.is_ok() {} }, async {
        smol::Timer::after(timeout).await;
    })
    .await;
}

/// 起一条专用线程跑 ACP 连接，立即返回句柄。`spawn_gate` 只包住实际创建
/// 子进程到发布 `AcpHandle.stdio` 的区间；运行时解析/下载在拿读锁之前完成。
/// 不需要与外部升级流程协调的调用方可传 `None`。
pub fn spawn_acp(launch: AcpLaunch, spawn_gate: Option<Arc<RwLock<()>>>) -> AcpHandle {
    let (cmd_tx, cmd_rx) = smol::channel::unbounded::<AcpCommand>();
    let (event_tx, event_rx) = smol::channel::unbounded::<AcpEvent>();
    let stdio: Arc<Mutex<Option<AcpStdio>>> = Arc::new(Mutex::new(None));
    let stdio_for_thread = Arc::clone(&stdio);
    let thread_name = format!("smelt-acp-{}", &launch.sid[..launch.sid.len().min(12)]);
    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            // stderr 尾巴：环形保尾部若干行，Fatal 时拼进诊断（npx 找不到包/装包
            // 失败的真实原因都在 stderr 里，别让用户猜）。
            let stderr_tail: Arc<Mutex<Vec<String>>> = Arc::default();
            // 先解析运行时（bunx → 受管 bun，可能触发首次下载），再进连接循环。
            let cmd = {
                let tx = event_tx.clone();
                match resolve_runtime_command(&launch.launch.command, &|msg| {
                    let _ = tx.try_send(AcpEvent::Status(msg.to_string()));
                }) {
                    Ok(cmd) => cmd,
                    Err(e) => {
                        let _ = event_tx.try_send(AcpEvent::Fatal(e));
                        return;
                    }
                }
            };
            let AcpLaunch {
                launch: launch_spec,
                cwd,
                sid,
                resume_session_id,
                resume_needs_transcript_check,
            } = launch;
            let launch = AcpLaunch {
                launch: AcpLaunchSpec {
                    command: cmd,
                    env: launch_spec.env,
                },
                cwd,
                sid,
                resume_session_id,
                resume_needs_transcript_check,
            };
            let result = smol::block_on(run_connection(
                &launch,
                cmd_rx,
                event_tx.clone(),
                stderr_tail.clone(),
                stdio_for_thread,
                spawn_gate,
            ));
            if let Err(e) = result {
                let tail = stderr_tail.lock().unwrap().join("\n");
                let msg = if tail.is_empty() {
                    format!("{e}")
                } else {
                    format!("{e}\n--- agent stderr ---\n{tail}")
                };
                let _ = event_tx.try_send(AcpEvent::Fatal(msg));
            }
            // Ok 结束（Shutdown）不发 Fatal——UI 主动关的，没必要再报。
        })
        .expect("spawn smelt-acp thread");
    AcpHandle {
        cmd_tx,
        event_rx,
        stdio,
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
) -> impl futures::Stream<Item = std::io::Result<String>> + Send
where
    R: futures::AsyncRead + Send + Unpin + 'static,
{
    futures::io::BufReader::new(reader)
        .lines()
        .inspect(move |res| {
            if let Ok(line) = res {
                *last_stdout_line.lock().unwrap() = Some(line.clone());
            }
        })
}

/// 无缝升级续接专用：先「回放」升级前捕获到的那行原始请求（如果有——对应
/// 一张正卡着的权限/选择题卡片），再无缝接上继承来的 fd 往后实时读。SDK 的
/// 请求分发器看到的字节序列跟"从来没断过"完全一样，会重新解析出一个等价的
/// responder（绑定同一个原始 JSON-RPC 请求 id），不会丢这张卡。
///
/// `last_stdout_line` 回放行 + 后续实时行都要写——不写的话，这条恢复出来的
/// 连接活着期间如果 agent 又发一次新的权限/选择题请求，`raw_request_line`
/// 会一直是 None：等真撑到*下一次*升级，这条请求就没有原文可回放，agent
/// 会永远卡在等一个不会来的回复上，审批卡在 GUI 上直接消失（真实教训：
/// 早期版本这里只在 `make_incoming_lines`——也就是首次 spawn 那条路——接了
/// 这根线，`make_resume_incoming_lines` 漏接，连续两次升级期间会复现）。
fn make_resume_incoming_lines<R>(
    reader: R,
    pending_raw_line: Option<String>,
    last_stdout_line: Arc<Mutex<Option<String>>>,
) -> std::pin::Pin<Box<dyn futures::Stream<Item = std::io::Result<String>> + Send>>
where
    R: futures::AsyncRead + Send + Unpin + 'static,
{
    let last_stdout_line_for_replay = Arc::clone(&last_stdout_line);
    let live = futures::io::BufReader::new(reader)
        .lines()
        .inspect(move |res| {
            if let Ok(line) = res {
                *last_stdout_line.lock().unwrap() = Some(line.clone());
            }
        });
    match pending_raw_line {
        Some(line) => {
            *last_stdout_line_for_replay.lock().unwrap() = Some(line.clone());
            Box::pin(futures::stream::once(async move { Ok(line) }).chain(live))
        }
        None => Box::pin(live),
    }
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
struct KillProcessGroupOnDrop(i32);

impl Drop for KillProcessGroupOnDrop {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

fn load_restore_failure_message(error: &agent_client_protocol::Error) -> String {
    match error.code {
        agent_client_protocol::ErrorCode::ResourceNotFound => "旧会话记录不存在，无法恢复".into(),
        agent_client_protocol::ErrorCode::MethodNotFound => {
            "agent 不支持 session/load，无法恢复历史对话".into()
        }
        _ => format!("恢复历史对话失败，可重试：{error}"),
    }
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

/// 连接主体：spawn agent 子进程 → initialize → newSession → 双源 loop
/// （UI 指令 / agent 更新流）。返回 Ok 表示用户主动 Shutdown。
async fn run_connection(
    launch: &AcpLaunch,
    cmd_rx: smol::channel::Receiver<AcpCommand>,
    event_tx: smol::channel::Sender<AcpEvent>,
    stderr_tail: Arc<Mutex<Vec<String>>>,
    stdio_out: Arc<Mutex<Option<AcpStdio>>>,
    spawn_gate: Option<Arc<RwLock<()>>>,
) -> Result<(), agent_client_protocol::Error> {
    let agent = build_agent(&launch.launch)?;
    let (child_stdin, child_stdout, child_stderr, child) =
        with_spawn_gate(spawn_gate.as_ref(), || {
            let (child_stdin, child_stdout, child_stderr, child) = agent
                .spawn_process()
                .map_err(|e| agent_client_protocol::Error::internal_error().data(e.to_string()))?;
            *stdio_out.lock().unwrap() = Some(AcpStdio {
                pid: child.id() as i32,
                stdin_fd: child_stdin.as_raw_fd(),
                stdout_fd: child_stdout.as_raw_fd(),
            });
            Ok::<_, agent_client_protocol::Error>((child_stdin, child_stdout, child_stderr, child))
        })?;
    let pid = child.id() as i32;
    // `child` 本身不能就地 drop：它是子进程唯一的活体句柄（drop async_process
    // 的 Child 不会杀进程，跟 std 一样），得撑到整个连接结束——用不着它的
    // 任何方法，只借它的存在期，_guard 才是真正负责杀的那个。
    let _child_keep_alive = child;
    let _guard = KillProcessGroupOnDrop(pid);
    spawn_stderr_drain(child_stderr, stderr_tail);

    let last_stdout_line: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let outgoing = make_outgoing_sink(child_stdin);
    let incoming = make_incoming_lines(child_stdout, Arc::clone(&last_stdout_line));
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

    let perm_tx = event_tx.clone();
    let elicit_tx = event_tx.clone();
    let perm_last_line = Arc::clone(&last_stdout_line);
    let elicit_last_line = Arc::clone(&last_stdout_line);
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
                    let question = permission_question(&request);
                    let _ = perm_tx.try_send(AcpEvent::Permission {
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
                        details: crate::acp_session::ApprovalDetailsView::Generic,
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
                    let fields = match &request.mode {
                        ElicitationMode::Form(form) => parse_elicit_fields(&form.requested_schema),
                        _ => None, // Url / 未知模式不支持
                    };
                    match fields {
                        Some(fields) => {
                            let _ = elicit_tx.try_send(AcpEvent::Elicitation {
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
        .connect_with(transport, |connection: ConnectionTo<Agent>| async move {
            let init = connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1).client_capabilities(
                        ClientCapabilities::default().elicitation(
                            ElicitationCapabilities::default()
                                .form(ElicitationFormCapabilities::default()),
                        ),
                    ),
                )
                .block_task()
                .await?;
            // 收图能力：三条恢复路径共用（bool 是 Copy，多次读没问题）。
            let supports_image = init.agent_capabilities.prompt_capabilities.image;

            // 冷恢复只有一条路径：`session/load`。agent 的 session store 是历史
            // 唯一持久化来源，load 重放的更新负责重建 smeltd 的 entries 投影。
            // `session/resume` 不重放历史，不能拿来冷恢复；smeltd 仍持有完整活体
            // 状态时根本不会进这里，而是在 `acp_open` 里直接 attach。无缝升级继承
            // fd 则走 `resume_acp_from_fds`，同样不进入这条 spawn 路径。
            //
            // 任何恢复失败都必须显式结束：静默 session/new 会让用户以为恢复成功，
            // 实际却在一条丢失上下文的新对话里继续工作。
            match select_session_start(
                launch.resume_session_id.is_some(),
                init.agent_capabilities.load_session,
            ) {
                SessionStart::UnsupportedLoad => {
                    let _ = event_tx.try_send(AcpEvent::Fatal(
                        "agent 未声明 loadSession 能力，无法恢复历史对话".to_string(),
                    ));
                    return Ok(());
                }
                SessionStart::New => {}
                SessionStart::Load => {
                    let sid = launch
                        .resume_session_id
                        .clone()
                        .expect("Load path requires a session id");
                    // Claude replays history synchronously *before* returning the
                    // session/load response. Register the session handler first or
                    // those notifications are unhandled and the SDK drops them.
                    // The id is already known, so a placeholder response is enough;
                    // modes/config are published from the real load response below.
                    let session = connection
                        .attach_session(NewSessionResponse::new(sid.clone()), Default::default())?;
                    // Claude 等 ACP agent 会在 session/load 响应返回前同步推送历史，
                    // SDK 会先缓冲这些通知，drive_session 再于 Ready 后逐条读出。
                    // 因而清空事件必须排在请求之前，不能等到 Ready 再清。
                    let _ = event_tx.try_send(AcpEvent::HistoryReplayStarted);
                    let mut load_request = LoadSessionRequest::new(sid.clone(), cwd.clone());
                    if let Some(meta) = claude_raw_sdk_meta(&launch.launch.command) {
                        load_request = load_request.meta(meta);
                    }
                    match connection.send_request(load_request).block_task().await {
                        Ok(loaded) => {
                            publish_config_options(loaded.config_options.as_deref(), &event_tx);
                            return drive_session(
                                session,
                                cmd_rx,
                                event_tx,
                                ReadyKind::ResumedWithReplay,
                                supports_image,
                            )
                            .await;
                        }
                        Err(error) => {
                            let _ = event_tx
                                .try_send(AcpEvent::Fatal(load_restore_failure_message(&error)));
                            return Ok(());
                        }
                    }
                }
            }

            // 手动 session/new 而不是 build_session：SDK 的 ActiveSession 只留
            // session_id/modes/meta，会把 config_options（模型等）丢掉，而那正是
            // 「当前用的哪个模型」的唯一来源。session/new 不会像 session/load
            // 那样在 response 前同步重放历史，因此可以在收到 response 后 attach。
            let created = connection
                .send_request(NewSessionRequest::new(std::path::Path::new(&cwd)))
                .block_task()
                .await?;
            publish_config_options(created.config_options.as_deref(), &event_tx);
            let session = connection.attach_session(created, Default::default())?;
            drive_session(session, cmd_rx, event_tx, ReadyKind::Fresh, supports_image).await
        })
        .await
}

/// smeltd 无缝升级续接：不 spawn 新子进程，直接接上继承来的 fd（`exec()` 前
/// 清了 CLOEXEC、活过整个交接）继续跑，起一条跟 `spawn_acp` 同款的专用线程，
/// 立即返回句柄。`AcpHandle.stdio` 一开始就是 `Some`——这几个 fd 本来就是
/// 调用方（smeltd）传进来的，不用等 spawn。
pub fn resume_acp_from_fds(
    sid: String,
    stdin_fd: RawFd,
    stdout_fd: RawFd,
    pid: i32,
    acp_session_id: String,
    supports_image: bool,
    pending_raw_line: Option<String>,
) -> AcpHandle {
    let (cmd_tx, cmd_rx) = smol::channel::unbounded::<AcpCommand>();
    let (event_tx, event_rx) = smol::channel::unbounded::<AcpEvent>();
    let stdio = Arc::new(Mutex::new(Some(AcpStdio {
        pid,
        stdin_fd,
        stdout_fd,
    })));
    let thread_name = format!("smelt-acp-r-{}", &sid[..sid.len().min(10)]);
    std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let result = smol::block_on(run_resumed_connection(
                stdin_fd,
                stdout_fd,
                pid,
                acp_session_id,
                supports_image,
                pending_raw_line,
                cmd_rx,
                event_tx.clone(),
            ));
            if let Err(e) = result {
                let _ = event_tx.try_send(AcpEvent::Fatal(format!("{e}")));
            }
            // Ok 结束（Shutdown）不发 Fatal——跟 run_connection 一致。
        })
        .expect("spawn smelt-acp resume thread");
    AcpHandle {
        cmd_tx,
        event_rx,
        stdio,
    }
}

/// `resume_acp_from_fds` 的连接主体：接上继承来的 fd → 跳过握手直接
/// attach_session（agent 早跟上一个进程做过 initialize/newSession 了）→
/// 双源 loop。`.on_receive_request` 那两段处理逻辑跟 `run_connection`里的
/// 完全一样——本想抽成共用，但 `Client.builder()` 链式调用之后的类型是个
/// 展开不动的匿名泛型，硬拆共用函数会把签名搞得比这点重复代码还难读，
/// protocol 这层的粘合代码本来就不常变，可以接受这份重复。
async fn run_resumed_connection(
    stdin_fd: RawFd,
    stdout_fd: RawFd,
    pid: i32,
    acp_session_id: String,
    supports_image: bool,
    pending_raw_line: Option<String>,
    cmd_rx: smol::channel::Receiver<AcpCommand>,
    event_tx: smol::channel::Sender<AcpEvent>,
) -> Result<(), agent_client_protocol::Error> {
    // `unsafe`：这两个 fd 是 smeltd 从上一代进程 dup 过来、清了 CLOEXEC 活过
    // exec() 的，调用方保证此刻整个进程里没有别的代码持有/关闭过它们
    // （smeltd 那边交接完立刻转手，见 resume_handoff 的用法）。
    let stdin_file = unsafe { std::fs::File::from_raw_fd(stdin_fd) };
    let stdout_file = unsafe { std::fs::File::from_raw_fd(stdout_fd) };
    let stdin_async = smol::Async::new(stdin_file)
        .map_err(|e| agent_client_protocol::Error::internal_error().data(e.to_string()))?;
    let stdout_async = smol::Async::new(stdout_file)
        .map_err(|e| agent_client_protocol::Error::internal_error().data(e.to_string()))?;

    let _guard = KillProcessGroupOnDrop(pid);

    let last_stdout_line: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let outgoing = make_outgoing_sink(stdin_async);
    let incoming = make_resume_incoming_lines(
        stdout_async,
        pending_raw_line,
        Arc::clone(&last_stdout_line),
    );
    let transport = Lines::new(outgoing, incoming);

    let session_id = SessionId::new(acp_session_id);

    let perm_tx = event_tx.clone();
    let elicit_tx = event_tx.clone();
    let perm_last_line = Arc::clone(&last_stdout_line);
    let elicit_last_line = Arc::clone(&last_stdout_line);
    Client
        .builder()
        .name("smelt")
        .on_receive_request(
            move |request: RequestPermissionRequest, responder, _connection| {
                let perm_tx = perm_tx.clone();
                let raw_request_line = perm_last_line.lock().unwrap().clone();
                async move {
                    let question = permission_question(&request);
                    let _ = perm_tx.try_send(AcpEvent::Permission {
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
                        details: crate::acp_session::ApprovalDetailsView::Generic,
                        raw_request_line,
                    });
                    Ok(())
                }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            move |request: CreateElicitationRequest, responder, _connection| {
                let elicit_tx = elicit_tx.clone();
                let raw_request_line = elicit_last_line.lock().unwrap().clone();
                async move {
                    let fields = match &request.mode {
                        ElicitationMode::Form(form) => parse_elicit_fields(&form.requested_schema),
                        _ => None,
                    };
                    match fields {
                        Some(fields) => {
                            let _ = elicit_tx.try_send(AcpEvent::Elicitation {
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
        .connect_with(transport, |connection: ConnectionTo<Agent>| async move {
            // 跳过 initialize/newSession/resume/load：agent 早就跟上一个进程
            // 完成过握手了，这里只是换了个"读它输出的人"。modes/config_options
            // /meta 留空——只影响"切模型"下拉暂时是空的，下次 agent 发 Model
            // 更新（ConfigOptionUpdate）会自动补上，不影响对话本身。
            let resp = NewSessionResponse::new(session_id.clone());
            let session = connection.attach_session(resp, Default::default())?;
            drive_session(
                session,
                cmd_rx,
                event_tx,
                ReadyKind::ResumedKeepHistory,
                supports_image,
            )
            .await
        })
        .await
}

/// 驱动一个已建立的会话：发 Ready → 双源 loop（UI 指令 / agent 更新流）。
/// `session/load` / `session/new` 与继承 fd 的路径共用。
async fn drive_session<'r>(
    mut session: ActiveSession<'r, Agent>,
    cmd_rx: smol::channel::Receiver<AcpCommand>,
    event_tx: smol::channel::Sender<AcpEvent>,
    ready_kind: ReadyKind,
    // 握手时 agent 声明的收图能力（promptCapabilities.image），随 Ready 转给 UI。
    supports_image: bool,
) -> Result<(), agent_client_protocol::Error> {
    let _ = event_tx.try_send(AcpEvent::Ready {
        session_id: session.session_id().clone(),
        kind: ready_kind,
        supports_image,
    });
    loop {
        // 两个等待源合一：先构造 read_update future，race 决议后它
        // 即被 drop（消息未出队不会丢），借用随之结束——绕开
        // 「cmd 分支也要 &mut session」的借用冲突。
        enum Next {
            Cmd(Option<AcpCommand>),
            Update(Result<SessionMessage, agent_client_protocol::Error>),
        }
        let next = {
            let read = session.read_update();
            smol::future::race(async { Next::Cmd(cmd_rx.recv().await.ok()) }, async move {
                Next::Update(read.await)
            })
            .await
        };
        match next {
            // 通道关闭（UI 句柄 drop）等同 Shutdown。
            Next::Cmd(None) | Next::Cmd(Some(AcpCommand::Shutdown)) => {
                return Ok(());
            }
            Next::Cmd(Some(AcpCommand::Prompt { text, images })) => {
                if images.is_empty() {
                    // 纯文本走 SDK 的 send_prompt：它顺带把 StopReason 塞回
                    // read_update 流，TurnEnded 由 translate_update 发。
                    session.send_prompt(text)?;
                } else {
                    // 带图就得自己拼 ContentBlock——SDK 的 send_prompt 只收
                    // 一个 ToString，塞不进 Image block。代价是 StopReason 不
                    // 再流经 read_update，得在响应回调里自己发 TurnEnded
                    // （所以这里**不能**改成 block_task().await：那会把整个
                    // 连接循环卡住，流式更新全部收不到）。
                    let mut blocks: Vec<ContentBlock> = Vec::new();
                    if !text.is_empty() {
                        blocks.push(text.into());
                    }
                    for im in images {
                        blocks.push(ContentBlock::Image(ImageContent::new(im.data_b64, im.mime)));
                    }
                    let tx = event_tx.clone();
                    session
                        .connection()
                        .send_request(PromptRequest::new(session.session_id().clone(), blocks))
                        .on_receiving_result(async move |result| {
                            let PromptResponse { stop_reason, .. } = result?;
                            let _ = tx.try_send(AcpEvent::TurnEnded(stop_reason));
                            Ok(())
                        })?;
                }
            }
            Next::Cmd(Some(AcpCommand::Cancel)) => {
                session
                    .connection()
                    .send_notification(CancelNotification::new(session.session_id().clone()))?;
            }
            Next::Cmd(Some(AcpCommand::SetConfigOption {
                config_id,
                value_id,
            })) => {
                let req = SetSessionConfigOptionRequest::new(
                    session.session_id().clone(),
                    SessionConfigId::new(config_id),
                    SessionConfigValueId::new(value_id),
                );
                match session.connection().send_request(req).block_task().await {
                    // 响应带回全量配置项：直接据此刷新，不猜 agent 是否接受了修改。
                    Ok(resp) => {
                        publish_config_options(Some(&resp.config_options), &event_tx);
                    }
                    Err(e) => {
                        let _ =
                            event_tx.try_send(AcpEvent::Status(format!("更新会话配置失败：{e}")));
                    }
                }
            }
            Next::Update(update) => {
                translate_update(update?, &event_tx).await?;
            }
        }
    }
}

/// 把 agent 的一条更新翻译成 AcpEvent（不认识的一律忽略——协议会长新枝）。
async fn translate_update(
    message: SessionMessage,
    event_tx: &smol::channel::Sender<AcpEvent>,
) -> Result<(), agent_client_protocol::Error> {
    match message {
        SessionMessage::SessionMessage(dispatch) => {
            MatchDispatch::new(dispatch)
                .if_notification(async |notif: SessionNotification| {
                    let event = match notif.update {
                        SessionUpdate::AgentMessageChunk(chunk) => Some(AcpEvent::AgentChunk {
                            thought: false,
                            text: content_text(&chunk.content),
                        }),
                        SessionUpdate::AgentThoughtChunk(chunk) => Some(AcpEvent::AgentChunk {
                            thought: true,
                            text: content_text(&chunk.content),
                        }),
                        SessionUpdate::ToolCall(tc) => Some(AcpEvent::ToolCall(tc)),
                        SessionUpdate::ToolCallUpdate(u) => Some(AcpEvent::ToolCallUpdate(u)),
                        SessionUpdate::UserMessageChunk(chunk) => {
                            Some(AcpEvent::UserChunk(content_text(&chunk.content)))
                        }
                        SessionUpdate::AvailableCommandsUpdate(u) => {
                            Some(AcpEvent::AvailableCommands(
                                u.available_commands
                                    .into_iter()
                                    .map(|c| (c.name, c.description))
                                    .collect(),
                            ))
                        }
                        // 上下文用量：used/size 是 token 数，UI 换算成百分比。
                        SessionUpdate::UsageUpdate(u) => Some(AcpEvent::Usage {
                            used: u.used,
                            size: u.size,
                            cached_read: None,
                        }),
                        // 计划（步骤清单）：透传给 UI 渲染 PLAN 条。
                        SessionUpdate::Plan(p) => Some(AcpEvent::Plan(p)),
                        // 会话配置变了（用户在 agent 侧换了模型、模式等）：全量刷新。
                        SessionUpdate::ConfigOptionUpdate(u) => {
                            publish_config_options(Some(&u.config_options), &event_tx);
                            None
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
            let _ = event_tx.try_send(AcpEvent::TurnEnded(reason));
        }
        _ => {} // SessionMessage #[non_exhaustive]
    }
    Ok(())
}

/// 组装 AcpAgent 配置（不 spawn）：命令按空白分词，注入 login shell 的 PATH
/// （Finder 启动的 GUI 进程 PATH 不含 nvm/homebrew，直接 spawn `npx` 会
/// ENOENT）。真正的 spawn 由调用方通过 `AcpAgent::spawn_process()`（SDK 公开
/// 的"低层逃生口"，见其文档）自己发起——这样才能拿到子进程 pid + 原始
/// stdin/stdout fd（smeltd 无缝升级要用），不能再走 `connect_with(agent, ..)`
/// 那条把 spawn 过程整个封在 SDK 内部、什么都拿不到的路。stderr 尾巴 / 原始
/// 行捕获也因此改成调用方（`run_connection`）自己接管，不再挂在这个 AcpAgent
/// 对象上的 `with_debug` 钩子。
fn build_agent(launch: &AcpLaunchSpec) -> Result<AcpAgent, agent_client_protocol::Error> {
    // 查找命令用 login PATH + 一批常见 CLI 安装目录兜底。为什么要兜底：
    // login_shell_path 走的是 `zsh -lc`（非交互 login），只读 ~/.zprofile；很多
    // CLI 的安装脚本把 PATH 加在 ~/.zshrc（交互式）里，非交互读不到 → 明明装了
    // 却「未找到」。这里补搜标准安装位（尤其 grok 的 ~/.grok/bin），装了就一定
    // 找得到；真没装的才落到下面的友好提示。子进程 PATH 也用这份扩展，免得
    // agent 起来后找它自己的子工具又缺路径。
    let search_path = extended_search_path();
    // 命令字符串允许开头带 shell 风格的 `VAR=value` 前缀（比如
    // `CLAUDE_CONFIG_DIR=~/.claude-quant claude --dangerously-skip-permissions`，
    // 让同一家 agent 的多个 workspace 各开一条「设置 → Agent 集成」里的独立
    // 启动命令）——`AcpAgent::from_args` 本来就认这个语法（内部 parse_env_var
    // 逐个 token 解析，遇到第一个不是 `VAR=value` 形状的 token 才当作程序名），
    // 这里的 PATH 注入用的正是同一条路。真正要做的是"先把这些前缀跳过去找到
    // 真正的程序名"，不然会把 `CLAUDE_CONFIG_DIR=...` 整个当成程序名去查 PATH，
    // 报"未找到命令"（这是之前真实的行为，不是假设）。
    let args = build_agent_args(launch, &search_path)?;
    Ok(AcpAgent::from_args(args.iter().map(String::as_str))?)
}

fn build_agent_args(
    launch: &AcpLaunchSpec,
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
    let flat: Vec<&agent_client_protocol::schema::v1::SessionConfigSelectOption> =
        match &sel.options {
            SessionConfigSelectOptions::Ungrouped(v) => v.iter().collect(),
            SessionConfigSelectOptions::Grouped(gs) => {
                gs.iter().flat_map(|g| g.options.iter()).collect()
            }
            _ => Vec::new(), // schema #[non_exhaustive]，协议会长新枝
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
        current_name: name,
        options,
    })
}

/// 将 agent 声明的全部 select 配置收敛成视图状态。Boolean 尚未在初始化能力中
/// 声明，因此兼容 ACP 1.3 的 adapter 会回退为 select；未来声明 bool 能力后可在
/// 这里自然扩展成开关，不会影响已有 agent。
pub(crate) fn session_configs_from_config(
    options: &[SessionConfigOption],
) -> Vec<SessionConfigState> {
    options
        .iter()
        .filter(|opt| !matches!(opt.category, Some(SessionConfigOptionCategory::Model)))
        .filter_map(|opt| {
            let SessionConfigKind::Select(sel) = &opt.kind else {
                return None;
            };
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
            })
        })
        .collect()
}

fn publish_config_options(
    options: Option<&[SessionConfigOption]>,
    event_tx: &smol::channel::Sender<AcpEvent>,
) {
    let Some(options) = options else { return };
    if let Some(model) = model_from_config(options) {
        let _ = event_tx.try_send(AcpEvent::Model(model));
    }
    let _ = event_tx.try_send(AcpEvent::ConfigOptions(session_configs_from_config(
        options,
    )));
}

/// 权限卡片的问题摘要：tool call 有标题用标题，否则退回工具 id。
fn permission_question(request: &RequestPermissionRequest) -> String {
    request
        .tool_call
        .fields
        .title
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| format!("工具调用 {}", request.tool_call.tool_call_id))
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
                let text = content_text(&inner.content);
                (!text.trim().is_empty()).then(|| ToolOutputPart::Text(text))
            }
            ToolCallContent::Diff(d) => Some(ToolOutputPart::Diff {
                path: d.path.display().to_string(),
                old_text: d.old_text.clone(),
                new_text: d.new_text.clone(),
            }),
            _ => None, // Terminal 等 MVP 不渲染
        })
        .collect()
}

/// —— 受管 bun 运行时（Zed 式按需下载）——————————————————————————
///
/// 适配器是 npm 包，需要 JS 运行时；不依赖用户装 node/bun，smelt 自己按需下载
/// 一份锁定版本的 bun（单文件）到 ~/.smelt/runtime/bun-v<版本>/。升级 = 改下面
/// 常量（URL 与 sha256 成对锁死），旧版本目录留着不碍事。
/// agent 主体是 SDK 自带的原生 claude 二进制，bun 只跑适配器那层薄翻译。
const BUN_VERSION: &str = "1.3.14";
#[cfg(target_arch = "aarch64")]
const BUN_DOWNLOAD: (&str, &str) = (
    "https://github.com/oven-sh/bun/releases/download/bun-v1.3.14/bun-darwin-aarch64.zip",
    "d8b96221828ad6f97ac7ac0ab7e95872341af763001e8803e8267652c2652620",
);
#[cfg(target_arch = "x86_64")]
const BUN_DOWNLOAD: (&str, &str) = (
    "https://github.com/oven-sh/bun/releases/download/bun-v1.3.14/bun-darwin-x64.zip",
    "4183df3374623e5bab315c547cfa0974533cd457d86b73b639f7a87974cd6633",
);
#[cfg(target_arch = "aarch64")]
const BUN_ZIP_DIR: &str = "bun-darwin-aarch64";
#[cfg(target_arch = "x86_64")]
const BUN_ZIP_DIR: &str = "bun-darwin-x64";

fn managed_bun_path() -> Option<std::path::PathBuf> {
    Some(
        dirs::home_dir()?
            .join(".smelt/runtime")
            .join(format!("bun-v{BUN_VERSION}"))
            .join("bun"),
    )
}

/// 确保受管 bun 就位（不在则下载 + sha256 校验 + 冒烟），返回可执行路径。
fn ensure_bun(status: &dyn Fn(&str)) -> Result<std::path::PathBuf, String> {
    let bun = managed_bun_path().ok_or("找不到 home 目录")?;
    if bun.is_file() {
        return Ok(bun);
    }
    let dir = bun.parent().unwrap();
    std::fs::create_dir_all(dir).map_err(|e| format!("建目录 {} 失败：{e}", dir.display()))?;
    let (url, want_sha) = BUN_DOWNLOAD;
    status("正在下载 Bun 运行时（约 22MB，仅首次）…");
    let zip = dir.join(".download.zip");
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--retry", "2", "-o"])
        .arg(&zip)
        .arg(url)
        .output()
        .map_err(|e| format!("无法执行 curl：{e}"))?;
    if !out.status.success() {
        return Err(format!(
            "下载 Bun 失败（可离线安装：brew install bun 后把命令改成系统 bunx）：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    status("校验并解压运行时…");
    let sum = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .arg(&zip)
        .output()
        .map_err(|e| format!("无法执行 shasum：{e}"))?;
    let got = String::from_utf8_lossy(&sum.stdout);
    let got = got.split_whitespace().next().unwrap_or("");
    if got != want_sha {
        let _ = std::fs::remove_file(&zip);
        return Err(format!(
            "Bun 下载校验失败（期望 {want_sha}，实际 {got}），已丢弃"
        ));
    }
    let unzip = std::process::Command::new("unzip")
        .args(["-o", "-q"])
        .arg(&zip)
        .arg("-d")
        .arg(dir)
        .output()
        .map_err(|e| format!("无法执行 unzip：{e}"))?;
    if !unzip.status.success() {
        return Err(format!(
            "解压 Bun 失败：{}",
            String::from_utf8_lossy(&unzip.stderr).trim()
        ));
    }
    let _ = std::fs::remove_file(&zip);
    std::fs::rename(dir.join(BUN_ZIP_DIR).join("bun"), &bun)
        .map_err(|e| format!("安放 bun 失败：{e}"))?;
    let _ = std::fs::remove_dir_all(dir.join(BUN_ZIP_DIR));
    // 冒烟：能报版本才算装好（顺带触发 macOS 首次执行检查）。
    let ver = std::process::Command::new(&bun)
        .arg("--version")
        .output()
        .map_err(|e| format!("bun 无法执行：{e}"))?;
    if !ver.status.success() {
        return Err("bun 下载后无法运行".to_string());
    }
    Ok(bun)
}

/// 命令首词是 `bunx`/`bun` 时解析到受管 bun（必要时下载）；受管失败但系统 PATH
/// 里有同名可执行则原样放行（用户自己装的）；其他命令一律不动（npx / 绝对路径
/// 等逃生口）。
fn resolve_runtime_command(cmd: &str, status: &dyn Fn(&str)) -> Result<String, String> {
    let head = command_head_after_env_prefixes(cmd).unwrap_or_default();
    if head != "bunx" && head != "bun" {
        return Ok(cmd.to_string());
    }
    match ensure_bun(status) {
        Ok(bun) => {
            let bun = bun.to_string_lossy().into_owned();
            Ok(rewrite_runtime_command_with_bun(cmd, &bun).unwrap_or_else(|| cmd.to_string()))
        }
        Err(e) => {
            // 受管失败：系统里用户自己装过 bun 就用系统的。
            let sys_has = std::env::split_paths(crate::login_env::login_path())
                .any(|p| p.join(head).is_file());
            if sys_has { Ok(cmd.to_string()) } else { Err(e) }
        }
    }
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
fn extended_search_path() -> String {
    let mut path = crate::login_env::login_path().to_string();
    let mut push = |p: String| {
        path.push(':');
        path.push_str(&p);
    };
    if let Some(home) = dirs::home_dir() {
        // grok 装在 ~/.grok/bin（软链常在 ~/.local/bin）；其余是 pip/npm/cargo
        // 之类常把 CLI 放的用户级目录。
        for sub in [".grok/bin", ".local/bin", "bin", ".cargo/bin", ".volta/bin"] {
            push(home.join(sub).to_string_lossy().into_owned());
        }
    }
    for d in ["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin"] {
        push(d.to_string());
    }
    path
}

/// 在 `:` 分隔的 PATH 里把命令名解析成绝对路径（找第一个可执行文件）。
/// 已经带 `/` 的（绝对路径、受管 bun 的全路径）原样返回，不查。找不到返回
/// None——调用方保留原名，让 spawn 照常失败并把真实错误报出来。
fn resolve_in_path(program: &str, path: &str) -> Option<String> {
    if program.contains('/') {
        return Some(program.to_string());
    }
    for dir in path.split(':').filter(|d| !d.is_empty()) {
        let full = std::path::Path::new(dir).join(program);
        // 是文件且可执行（软链会被 metadata 跟随到目标）。
        if let Ok(meta) = std::fs::metadata(&full) {
            use std::os::unix::fs::PermissionsExt;
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return full.to_str().map(String::from);
            }
        }
    }
    None
}

// login shell 的 PATH 探测（连同各家 agent 的自定义 workspace 目录变量一起）
// 挪进了 `crate::login_env`——不止 PATH 这一个变量要用同一套"起交互式 shell
// 才能读到 .zshrc export"的机制，claude_paths.rs 的 CLAUDE_CONFIG_DIR 判断
// 也要用它，不能各起一次慢 shell。

#[cfg(test)]
mod elicit_parse_tests {
    use super::*;

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
            }
        }))
        .expect("schema 反序列化");
        let fields = parse_elicit_fields(&schema).expect("单选和自由文本都应解析");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].key, "question_0");
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
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

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

    /// 受管 bun 已就位时，bunx 前缀改写为 `<managed-bun> x …`。
    #[test]
    fn bunx_rewrites_to_managed_bun_when_present() {
        let Some(bun) = managed_bun_path() else {
            return;
        };
        if !bun.is_file() {
            return; // 受管 bun 未安装的机器上跳过（真实下载见 manual_ensure_bun）
        }
        let noop = |_: &str| {};
        let out = resolve_runtime_command("bunx pkg@1 --flag", &noop).unwrap();
        assert_eq!(out, format!("{} x pkg@1 --flag", bun.to_string_lossy()));
    }

    /// 默认命令带 `--bun`（强制不 fallback 到系统 Node，见 default_acp_cmd 的
    /// 注释）：这个 flag 只是 `rest` 里的又一个词，改写时要原样透传、落在
    /// `x` 后面、包名前面——`bun x --bun pkg@version`，不能被误吞或挪位置。
    #[test]
    fn bunx_dash_dash_bun_flag_passes_through_in_order() {
        let Some(bun) = managed_bun_path() else {
            return;
        };
        if !bun.is_file() {
            return;
        }
        let noop = |_: &str| {};
        let out = resolve_runtime_command(
            "bunx --bun @agentclientprotocol/claude-agent-acp@0.59.0",
            &noop,
        )
        .unwrap();
        assert_eq!(
            out,
            format!(
                "{} x --bun @agentclientprotocol/claude-agent-acp@0.59.0",
                bun.to_string_lossy()
            )
        );
    }

    #[test]
    fn runtime_rewrite_preserves_legacy_env_prefixes_before_bunx() {
        let out = rewrite_runtime_command_with_bun(
            "CLAUDE_CONFIG_DIR=~/.claude bunx --bun @agentclientprotocol/claude-agent-acp@0.59.0",
            "/managed/bun",
        )
        .expect("bunx should rewrite");
        assert_eq!(
            out,
            "CLAUDE_CONFIG_DIR=~/.claude /managed/bun x --bun @agentclientprotocol/claude-agent-acp@0.59.0"
        );
    }

    /// 真实下载验证 + 预热（22MB，网络依赖）：`cargo test -- --ignored manual_ensure_bun`
    #[test]
    #[ignore]
    fn manual_ensure_bun() {
        let path = ensure_bun(&|msg| eprintln!("[status] {msg}")).expect("ensure_bun");
        assert!(path.is_file());
        let out = std::process::Command::new(&path)
            .arg("--version")
            .output()
            .unwrap();
        assert!(out.status.success());
        eprintln!(
            "bun @ {} → {}",
            path.display(),
            String::from_utf8_lossy(&out.stdout).trim()
        );
    }
}

#[cfg(test)]
mod restore_failure_tests {
    use super::{SessionStart, load_restore_failure_message, select_session_start};
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
            load_restore_failure_message(&Error::resource_not_found(None)),
            "旧会话记录不存在，无法恢复"
        );
    }

    #[test]
    fn unsupported_load_is_an_explicit_restore_failure() {
        assert_eq!(
            load_restore_failure_message(&Error::method_not_found()),
            "agent 不支持 session/load，无法恢复历史对话"
        );
    }

    #[test]
    fn transient_load_failure_remains_retryable_and_does_not_start_fresh() {
        let error = Error::internal_error();
        let message = load_restore_failure_message(&error);
        assert_eq!(message, format!("恢复历史对话失败，可重试：{error}"));
        assert!(!message.contains("新对话"));
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
    use super::{build_agent_args, inject_local_adapter_cli_path, resolve_in_path};
    use crate::agent_kind::AcpLaunchSpec;
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

    #[test]
    fn finds_executable_across_path_dirs() {
        // `sh` 一定在 /bin；把它藏在几个假目录后面，验证会逐段找下去。
        let path = "/no/such/dir:/also/fake:/bin:/usr/bin";
        assert_eq!(resolve_in_path("sh", path).as_deref(), Some("/bin/sh"));
    }

    #[test]
    fn missing_command_returns_none() {
        // 找不到就返回 None，让调用方保留原名、把真实 spawn 错误报出来。
        assert!(resolve_in_path("definitely-not-a-real-cmd-xyz", "/bin:/usr/bin").is_none());
    }

    #[test]
    fn build_agent_args_preserves_plain_launch_specs() {
        let args = build_agent_args(&AcpLaunchSpec::from_command("sh -lc echo"), "/bin:/usr/bin")
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
        let launch = AcpLaunchSpec::from_command("CLAUDE_CONFIG_DIR=/legacy/path sh --version")
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
            "bunx --bun @agentclientprotocol/codex-acp@1.1.7",
            "@agentclientprotocol/codex-acp",
            "CODEX_PATH",
            "codex",
            &mut env,
            dir.to_str().unwrap(),
        );

        assert_eq!(
            env.get("CODEX_PATH"),
            Some(&codex.to_string_lossy().into_owned())
        );
        std::fs::remove_file(codex).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn explicit_codex_path_is_not_overridden() {
        let mut env = BTreeMap::from([("CODEX_PATH".to_string(), "/custom/codex".to_string())]);

        inject_local_adapter_cli_path(
            "bunx --bun @agentclientprotocol/codex-acp@1.1.7",
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
        let mut env = BTreeMap::new();

        inject_local_adapter_cli_path(
            "bunx --bun @agentclientprotocol/claude-agent-acp@0.59.0",
            "@agentclientprotocol/claude-agent-acp",
            "CLAUDE_CODE_EXECUTABLE",
            "sh",
            &mut env,
            "/bin:/usr/bin",
        );

        assert_eq!(
            env.get("CLAUDE_CODE_EXECUTABLE").map(String::as_str),
            Some("/bin/sh")
        );
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
mod resume_incoming_lines_tests {
    use super::make_resume_incoming_lines;
    use crate::acp_session::{AcpSessionState, LivePermission};
    use futures::StreamExt;
    use std::sync::{Arc, Mutex};

    /// 复现 code review 发现的 bug：早期版本只有 `make_incoming_lines`（首次
    /// spawn 路径）接了 `last_stdout_line`，`make_resume_incoming_lines`（续接
    /// 路径）漏接——连续两次升级期间，第二次升级前如果 agent 又发一次权限/
    /// 选择题请求，`pending_raw_request_line()` 读到的永远是 None，回放不出
    /// 那行原文，审批卡直接消失，agent 永久卡死。这里验证回放行和后续实时行
    /// 都会写回 `last_stdout_line`，保证下一次升级能读到最新的待处理请求。
    #[test]
    fn replay_and_live_lines_both_update_last_stdout_line() {
        let reader = futures::io::Cursor::new(b"live-request-line\n".to_vec());
        let last_stdout_line: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let mut stream = make_resume_incoming_lines(
            reader,
            Some("replayed-request-line".to_string()),
            Arc::clone(&last_stdout_line),
        );

        smol::block_on(async {
            let first = stream.next().await.unwrap().unwrap();
            assert_eq!(first, "replayed-request-line");
            assert_eq!(
                last_stdout_line.lock().unwrap().as_deref(),
                Some("replayed-request-line"),
                "回放行也要立刻计入 last_stdout_line：万一 agent 紧跟着又发一次新\
                 请求，下一次升级得从这行往后看，不能还停在 None"
            );

            let second = stream.next().await.unwrap().unwrap();
            assert_eq!(second, "live-request-line");
            assert_eq!(
                last_stdout_line.lock().unwrap().as_deref(),
                Some("live-request-line"),
                "续接连接活着期间读到的实时行（模拟第二次升级前新来的权限/选择题\
                 请求）也必须覆盖 last_stdout_line，否则下一次升级回放不出这行"
            );
        });
    }

    #[test]
    fn no_pending_line_still_tracks_live_lines() {
        let reader = futures::io::Cursor::new(b"only-live-line\n".to_vec());
        let last_stdout_line: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let mut stream = make_resume_incoming_lines(reader, None, Arc::clone(&last_stdout_line));

        smol::block_on(async {
            let line = stream.next().await.unwrap().unwrap();
            assert_eq!(line, "only-live-line");
            assert_eq!(
                last_stdout_line.lock().unwrap().as_deref(),
                Some("only-live-line")
            );
        });
    }

    #[test]
    fn pending_request_survives_two_resume_handoff_cycles() {
        let replayed = r#"{"jsonrpc":"2.0","id":41,"method":"session/request_permission"}"#;
        let live = r#"{"jsonrpc":"2.0","id":42,"method":"session/request_permission"}"#;
        let reader = futures::io::Cursor::new(format!("{live}\n").into_bytes());
        let last_stdout_line: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let mut first_resume = make_resume_incoming_lines(
            reader,
            Some(replayed.to_string()),
            Arc::clone(&last_stdout_line),
        );

        let pending_raw_line = smol::block_on(async {
            assert_eq!(first_resume.next().await.unwrap().unwrap(), replayed);
            assert_eq!(last_stdout_line.lock().unwrap().as_deref(), Some(replayed));

            assert_eq!(first_resume.next().await.unwrap().unwrap(), live);
            assert_eq!(last_stdout_line.lock().unwrap().as_deref(), Some(live));

            let mut state = AcpSessionState::default();
            state.permissions.push(LivePermission {
                question: "Allow?".to_string(),
                tool_call_id: "tool-1".to_string(),
                options: Vec::new(),
                details: crate::acp_session::ApprovalDetailsView::Generic,
                responder: None,
                raw_request_line: last_stdout_line.lock().unwrap().clone(),
            });
            state
                .pending_raw_request_line()
                .expect("handoff must capture the live raw request")
                .to_string()
        });

        let mut second_resume = make_resume_incoming_lines(
            futures::io::Cursor::new(Vec::<u8>::new()),
            Some(pending_raw_line.clone()),
            Arc::new(Mutex::new(None)),
        );
        let replayed_again = smol::block_on(async { second_resume.next().await.unwrap().unwrap() });

        assert_eq!(replayed_again, pending_raw_line);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&replayed_again).unwrap()["id"],
            42
        );
    }
}

#[cfg(test)]
mod model_tests {
    use agent_client_protocol::schema::v1::{
        SessionConfigId, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
        SessionConfigSelect, SessionConfigSelectOption, SessionConfigSelectOptions,
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
    }
}
