//! Pi 原生 RPC 驱动。
//!
//! Pi 不实现 ACP；它为 IDE/桌面应用提供 `--mode rpc` JSONL 协议。本模块直接
//! 驱动该协议，再把 Pi 事件归一化到 Smelt 现有的会话状态机。`ConversationEvent` 等类型名
//! 是历史 wire 兼容名，不代表这里向 Pi 发送过任何 ACP 消息。

use std::collections::{BTreeMap, HashMap, HashSet};
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    ElicitationContentValue, SessionId, StopReason, ToolCallId,
};
use futures::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, Stream, StreamExt};

use crate::acp_chat::{AcpEntry, ToolCallStatus, ToolKind, ToolOutputPart};
use crate::acp_conn::{
    AcpForkCut, AcpStdio, ConfigValue, ContextUsageBreakdown, ConversationCommand,
    ConversationEvent, ConversationHandle, ConversationLaunch, ConversationRestoreFailure,
    ElicitField, ElicitFieldKind, ElicitOption, ElicitationResponder, ModelProviderGroup,
    ModelState, PermissionResponder, ReadyKind, SessionConfigState,
};
use crate::acp_session::{
    ApprovalDetailsView, PermissionOptionKindView, PermissionOptionView, RuntimeDebug,
    RuntimeDebugModelCall,
};
use crate::agent_kind::{ConversationLaunchSpec, SMELT_PI_AGENT_COMMAND};
use crate::pi_queue::QueueLane;

const PI_RPC_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// 关会话时等 Pi 自己退出（关 MCP 服务器、杀它跟踪的子进程）最多这么久。
const PI_EXIT_GRACE: Duration = Duration::from_secs(3);
const SMELT_PERMISSION_TITLE: &str = "smelt.permission.v1";
const SMELT_CONTEXT_USAGE_WIDGET: &str = "smelt-context-usage";
const SMELT_RUNTIME_DEBUG_WIDGET: &str = "smelt-runtime-debug";
const SMELT_BACKGROUND_TASK_WIDGET: &str = "smelt-background-tasks";
const SMELT_CODEMODE_COMMAND: &str = "smelt-codemode";
/// 让会话重读 models.json 的扩展命令（`packages/pi-agent/src/model-refresh.ts`）。
const SMELT_REFRESH_MODELS_COMMAND: &str = "smelt-refresh-models";
const SMELT_CODEMODE_WIDGET: &str = "smelt-codemode";

#[derive(Clone, Debug, PartialEq)]
struct PiModel {
    provider: String,
    id: String,
    name: String,
    context_window: Option<u64>,
}

#[derive(Clone, Debug)]
enum PendingConfig {
    Model(String),
    Thinking(String),
    /// 请求的 `off` / `on`。回执只表示命令被处理，勾选以扩展回报的活动工具为准。
    Codemode(String),
}

#[derive(Default)]
struct PiState {
    session_id: String,
    session_name: Option<String>,
    model: Option<PiModel>,
    models: Vec<PiModel>,
    thinking_level: String,
    thinking_levels: Vec<String>,
    permission_mode: String,
    /// 脚本工具是否已在活动工具集里。启动默认关，只信扩展回报。
    codemode_enabled: bool,
    available_commands: Vec<(String, String)>,
    active_turn: bool,
    /// Pi 已经收下当前这条 prompt。没收下之前，`agent_settled` 属于上一轮，
    /// 不能把这一条的本地回合标志清掉。
    prompt_accepted: bool,
    /// Pi 此刻会拒绝不带 streamingBehavior 的 prompt。和界面回合是否结束无关。
    provider_busy: bool,
    cancel_requested: bool,
    last_stop_reason: Option<String>,
    last_error: Option<String>,
    prompt_request_id: Option<String>,
    prompt_state_request_id: Option<String>,
    /// prompt 被拒绝后补问的 `get_state`。和成功路径的状态询问分开。
    refusal_state_request_id: Option<String>,
    pending_refusal_error: Option<String>,
    /// 用户停止后补问的 `get_state`。界面先停，这条回来才知道能不能再发。
    cancel_state_request_id: Option<String>,
    /// 用户在应用里登录/改配置后补拉的 `get_available_models`。会话启动时那一次走
    /// `rpc_call` 同步等，不走这里。
    models_request_id: Option<String>,
    /// 已发出、等回执的重读 models.json：（request id，回执到了要切的模型）。
    models_refresh_request: Option<(String, Option<String>)>,
    /// 压缩已经结束。拒绝用的快照可能仍写着 `isCompacting`，不能再把它当成还在忙。
    compaction_idle_pending: bool,
    /// 已发出、等 Pi 回执的 steer 请求。只用来把 `in_flight_rpc` 记账还回去；
    /// steer 不开新回合，不能碰 `prompt_request_id`。
    steer_request_ids: HashSet<String>,
    follow_up_request_ids: HashSet<String>,
    compact_request_ids: HashSet<String>,

    /// `clear_queue` 回执后是否要把 in_flight 还回去（ClearQueue 动作才要）。
    pending_clear: Option<(String, bool)>,
    /// 送进 Pi 队列的输入。停止时附图要跟着文字还回输入框。
    queue: crate::pi_queue::PiQueueMirror,
    /// 会话宿主里的后台任务。`clear_queue` 会把排在 Pi 队列里的结果一起清掉，要告诉
    /// 宿主重发；用户在界面上停任务也走它。
    background: Option<crate::background_tasks::BackgroundControl>,
    composer_restore_seq: u64,
    pending_configs: HashMap<String, PendingConfig>,
    partial_tool_output: HashMap<String, String>,
    /// toolcall_delta 拼出来的参数 JSON。id → 原文。
    partial_tool_args: HashMap<String, String>,
    /// 已解析的工具参数，结束时用来出 diff / 标题。
    tool_args: HashMap<String, serde_json::Value>,
    tool_names: HashMap<String, String>,
    /// 只接受最后一次 `get_session_stats` 的回包。Pi 会并发处理 stdin 命令，
    /// 旧请求可能晚于压缩后的新请求返回，不能让旧上下文覆盖新估算。
    latest_stats_request_id: Option<String>,
    /// 进行中的回退：先等 `get_fork_messages` 回执定位 entryId，再等 `fork` 回执。
    /// 两段用同一份 pending 而不是两个 id 集，因为它们的语义是连续的，
    /// 中途任何一段失败都要把整次回退作废。
    pending_rewind: Option<PendingRewind>,
    /// fork 成功后补发 `get_state` 的请求 id：Pi 切到了新 session 文件，
    /// 本地 session id 必须跟着换，否则后续断线重连会恢复到被丢弃的旧分支。
    rewind_state_request_id: Option<String>,
    /// 最近一条 `prompt` 写出的时刻，以及本会话已发出的条数。Pi 的 response 才算
    /// 「收下」，而它前面压着整个 `before_agent_start`（扩展在那里做慢活就直接计
    /// 在这段时延里），所以这是首轮卡顿唯一的可观测点。
    prompt_sent_at: Option<std::time::Instant>,
    prompts_sent: u64,
    /// Smelt 侧会话 id，只用于日志可读（Pi 自己的 id 在 `session_id`）。
    sid: String,
    next_request_id: u64,
}

/// 一永在途的回退请求。
struct PendingRewind {
    /// 发起方投影里的截断游标，fork 成功后原样带回 `Rewound` 事件。
    truncate_from: usize,
    /// 同文本匹配里取第几条（从 0 数起），对应发起方投影里的同文本序号。
    occurrence: usize,
    /// 要回退到的用户消息原文。
    text: String,
    /// 回退成功后这条消息怎么办，原样放进 `Rewound`。
    then: crate::acp_conn::AfterRewind,
    /// 正在等回执的 `get_fork_messages` 请求 id。
    list_request_id: String,
    /// 匹配到 entryId 之后发出的 `fork` 请求 id；None = 还在第一阶段。
    fork_request_id: Option<String>,
}

impl PiState {
    fn new() -> Self {
        Self {
            thinking_level: "medium".to_string(),
            permission_mode: "default".to_string(),
            ..Default::default()
        }
    }

    fn request_id(&mut self, purpose: &str) -> String {
        self.next_request_id += 1;
        format!("smelt-{purpose}-{}", self.next_request_id)
    }

    fn context_window(&self) -> Option<u64> {
        self.model.as_ref().and_then(|model| model.context_window)
    }
}

/// 只有 Smelt 发布的逻辑入口进入 Pi RPC 驱动。用户显式配置的其他命令仍按 ACP
/// 处理，不能因为字符串里碰巧出现 `pi` 就改变其协议语义。
pub fn is_smelt_pi_launch(launch: &ConversationLaunchSpec) -> bool {
    launch
        .command
        .split_whitespace()
        .find(|token| crate::workspace_override::split_env_assignment(token).is_none())
        == Some(SMELT_PI_AGENT_COMMAND)
}

/// 自动化 Run 不进 Pi 的交互 session 库。`--no-session` 与 `--session` 互斥，
/// 已经要续跑的启动命令原样返回。
pub fn with_unpersisted_pi_session(mut launch: ConversationLaunchSpec) -> ConversationLaunchSpec {
    if !is_smelt_pi_launch(&launch) {
        return launch;
    }
    let mut tokens = launch
        .command
        .split_whitespace()
        .filter(|token| crate::workspace_override::split_env_assignment(token).is_none());
    let _program = tokens.next();
    let rest = tokens.collect::<Vec<_>>();
    if rest.contains(&"--no-session")
        || rest
            .windows(2)
            .any(|pair| matches!(pair[0], "--session" | "--session-id" | "--fork"))
    {
        return launch;
    }
    if !launch.command.is_empty() && !launch.command.ends_with(' ') {
        launch.command.push(' ');
    }
    launch.command.push_str("--no-session");
    launch
}

/// `--session` 续接同一份文件；`--fork` 复制成新 session。两者和 `--no-session`
/// 互斥。命令行里已经写过这些参数时，不再二次追加。
fn apply_pi_session_cli_args(
    trailing: &mut Vec<String>,
    resume_session_id: Option<&str>,
    fork_session_id: Option<&str>,
) {
    let already_bound = trailing.iter().any(|token| token == "--no-session")
        || trailing
            .windows(2)
            .any(|pair| matches!(pair[0].as_str(), "--session" | "--session-id" | "--fork"));
    if already_bound {
        return;
    }
    if let Some(id) = fork_session_id.map(str::trim).filter(|id| !id.is_empty()) {
        trailing.push("--fork".into());
        trailing.push(id.to_string());
        return;
    }
    if let Some(id) = resume_session_id.map(str::trim).filter(|id| !id.is_empty()) {
        trailing.push("--session".into());
        trailing.push(id.to_string());
    }
}

/// 启动一个 Pi RPC 会话。句柄沿用现有 Smelt daemon wire，进程侧协议完全是 Pi
/// JSONL；这样 GUI/移动端不需要知道 provider 使用 ACP 还是原生驱动。
pub fn spawn_pi_rpc(
    launch: ConversationLaunch,
    spawn_gate: Option<Arc<RwLock<()>>>,
) -> ConversationHandle {
    let (cmd_tx, cmd_rx) = smol::channel::unbounded::<ConversationCommand>();
    let (event_tx, event_rx) = smol::channel::unbounded::<ConversationEvent>();
    let stdio: Arc<Mutex<Option<AcpStdio>>> = Arc::new(Mutex::new(None));
    let in_flight_rpc = Arc::new(AtomicUsize::new(0));
    let shutdown_requested = Arc::new(AtomicBool::new(false));

    let stdio_for_thread = Arc::clone(&stdio);
    let in_flight_for_thread = Arc::clone(&in_flight_rpc);
    let shutdown_for_thread = Arc::clone(&shutdown_requested);
    let sid = launch.sid.clone();
    std::thread::Builder::new()
        .name(format!("smelt-pi-rpc-{}", &sid[..sid.len().min(12)]))
        .spawn(move || {
            let stderr_tail: Arc<Mutex<Vec<String>>> = Arc::default();
            let stderr_drain: Arc<Mutex<Option<smol::Task<()>>>> = Arc::default();
            let ready = Arc::new(AtomicBool::new(false));
            let runtime = crate::managed_runtime::sync_managed_pi_agent(&|message| {
                let _ = event_tx.try_send(ConversationEvent::Status(message.to_string()));
            });
            let runtime = match runtime {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = event_tx.try_send(ConversationEvent::Fatal(error));
                    return;
                }
            };
            // 要连的 MCP 里有用受管 Bun 启动的（预设都是），先把 Bun 备好，会话期间持有它的
            // 租约。Pi 自己已经不跑在 Bun 上，没人会为它下载；新装的机器上那条路径不在，
            // MCP 起不来（spawn … ENOENT）。备不好也照常起 Pi，MCP 那边会如实报错。
            let _mcp_bun = crate::pi_mcp::uses_managed_bun(
                &crate::pi_mcp::loaded_mcp_servers_for_launch(&launch.launch),
            )
            .then(|| {
                crate::managed_runtime::sync_managed_bun(&|message| {
                    let _ = event_tx.try_send(ConversationEvent::Status(message.to_string()));
                })
                .inspect_err(|error| {
                    crate::app_log::error("pi-rpc", &format!("准备 MCP 用的 Bun 失败：{error}"));
                })
                .ok()
            });
            let had_resume = launch.resume_session_id.is_some() || launch.fork_session_id.is_some();
            let result = smol::block_on(async {
                let result = run_connection(
                    &launch,
                    runtime,
                    cmd_rx,
                    event_tx.clone(),
                    Arc::clone(&stderr_tail),
                    Arc::clone(&stderr_drain),
                    stdio_for_thread,
                    spawn_gate,
                    in_flight_for_thread,
                    shutdown_for_thread,
                    Arc::clone(&ready),
                )
                .await;
                // run_connection 返回后进程组已 SIGKILL。stderr drain 必须仍在
                // 同一个 smol executor 上被 poll，否则 EOF 永远到不了。
                let drain = if result.is_err() {
                    stderr_drain.lock().unwrap().take()
                } else {
                    None
                };
                if let Some(drain) = drain {
                    drain.await;
                }
                result
            });
            if let Err(error) = result {
                let tail = stderr_tail.lock().unwrap().join("\n");
                let message = if tail.is_empty() {
                    error
                } else {
                    format!("{error}\n--- Pi stderr ---\n{tail}")
                };
                crate::app_log::error("pi-rpc", &format!("会话 {sid} 异常终止：{message}"));
                if had_resume && !ready.load(Ordering::Acquire) {
                    let failure = if tail.to_ascii_lowercase().contains("no session found") {
                        ConversationRestoreFailure::HistoryMissing
                    } else {
                        ConversationRestoreFailure::Failed(format!(
                            "恢复 Pi 历史对话失败：{message}"
                        ))
                    };
                    let _ = event_tx.try_send(ConversationEvent::RestoreFailed(failure));
                } else {
                    let _ = event_tx.try_send(ConversationEvent::Fatal(message));
                }
            }
        })
        .expect("spawn smelt Pi RPC thread");

    ConversationHandle {
        cmd_tx,
        event_rx,
        stdio,
        in_flight_rpc,
        shutdown_requested,
        // Pi 原生支持 steer：运行中的消息可以插进当前回合。
        supports_mid_turn_input: true,
        supports_compaction: true,
        supports_native_queue: true,
        // Pi 的 fork(entryId) 可以把活动分支切回任意历史用户消息之前。
        supports_rewind: true,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_connection(
    launch: &ConversationLaunch,
    runtime: crate::managed_runtime::ManagedPiRuntime,
    cmd_rx: smol::channel::Receiver<ConversationCommand>,
    event_tx: smol::channel::Sender<ConversationEvent>,
    stderr_tail: Arc<Mutex<Vec<String>>>,
    stderr_drain: Arc<Mutex<Option<smol::Task<()>>>>,
    stdio_out: Arc<Mutex<Option<AcpStdio>>>,
    spawn_gate: Option<Arc<RwLock<()>>>,
    in_flight_rpc: Arc<AtomicUsize>,
    shutdown_requested: Arc<AtomicBool>,
    ready: Arc<AtomicBool>,
) -> Result<(), String> {
    let (inline_env, mut trailing) = parse_logical_launch(&launch.launch.command)?;
    let resume_id = launch.resume_session_id.as_ref().map(|id| id.to_string());
    let fork_id = launch.fork_session_id.as_ref().map(|id| id.to_string());
    apply_pi_session_cli_args(&mut trailing, resume_id.as_deref(), fork_id.as_deref());
    let process_args = runtime.process_args(trailing);
    let (outbound_tx, outbound_rx) = smol::channel::unbounded::<serde_json::Value>();
    let background = crate::background_tasks::BackgroundSupervisor::start(
        &launch.sid,
        launch.cwd.clone(),
        event_tx.clone(),
        outbound_tx.clone(),
    )?;
    let mut trace = HandshakeTrace {
        started: std::time::Instant::now(),
        published_model: None,
    };
    let session_seed_rx = start_session_preload(launch);
    let (child_stdin, child_stdout, child_stderr, child) = {
        let _spawn_permit = spawn_gate.as_ref().map(|gate| gate.read().unwrap());
        let mut stdio = stdio_out.lock().unwrap();
        if shutdown_requested.load(Ordering::SeqCst) {
            return Ok(());
        }
        let spawn_started = std::time::Instant::now();
        let spawned = spawn_process(
            launch,
            inline_env,
            process_args,
            Some(background.socket_path()),
        )?;
        crate::app_log::info(
            "pi-rpc",
            &format!(
                "会话 {} 进程已拉起，用时 {:?}",
                launch.sid,
                spawn_started.elapsed()
            ),
        );
        *stdio = Some(AcpStdio {
            pid: spawned.3.id() as i32,
            #[cfg(unix)]
            stdin_fd: spawned.0.as_raw_fd(),
            #[cfg(unix)]
            stdout_fd: spawned.1.as_raw_fd(),
        });
        spawned
    };
    let session_seed = session_seed_rx.and_then(|rx| rx.recv().ok()).flatten();
    let mut process = PiProcessGuard::new(child);
    *stderr_drain.lock().unwrap() = Some(spawn_stderr_drain(child_stderr, stderr_tail));

    let result = async {
        let mut writer = child_stdin;
        let mut lines = futures::io::BufReader::new(child_stdout).lines();
        let mut state = PiState::new();
        state.sid = launch.sid.clone();
        state.background = Some(background.control());
        let initialize = initialize_session(
            &mut lines,
            &mut writer,
            &outbound_tx,
            &outbound_rx,
            &event_tx,
            &mut state,
            launch,
            session_seed,
            &mut trace,
        );
        smol::future::race(initialize, async {
            smol::Timer::after(PI_RPC_HANDSHAKE_TIMEOUT).await;
            Err(format!(
                "Pi RPC 启动握手超时（{} 秒）",
                PI_RPC_HANDSHAKE_TIMEOUT.as_secs()
            ))
        })
        .await?;
        ready.store(true, Ordering::Release);

        enum Next {
            Command(Result<ConversationCommand, smol::channel::RecvError>),
            Outbound(Result<serde_json::Value, smol::channel::RecvError>),
            Line(Option<std::io::Result<String>>),
        }

        loop {
            let next = smol::future::race(
                async { Next::Command(cmd_rx.recv().await) },
                smol::future::race(async { Next::Outbound(outbound_rx.recv().await) }, async {
                    Next::Line(lines.next().await)
                }),
            )
            .await;
            match next {
                Next::Command(Ok(ConversationCommand::Shutdown)) | Next::Command(Err(_)) => {
                    return Ok(());
                }
                Next::Command(Ok(command)) => {
                    handle_command(command, &mut writer, &event_tx, &mut state, &in_flight_rpc)
                        .await?;
                }
                Next::Outbound(Ok(message)) => {
                    if message.get("type").and_then(serde_json::Value::as_str)
                        == Some("smelt_background_notify")
                    {
                        // 状态行留给归约。不要写进 Pi，也不要开一条用户 prompt。
                        if let Some(event) = background_notice_from_outbound(&message) {
                            let _ = event_tx.try_send(event);
                        }
                    } else {
                        write_rpc(&mut writer, &message).await?;
                    }
                }
                Next::Outbound(Err(_)) => return Err("Pi RPC 回执通道已关闭".to_string()),
                Next::Line(Some(Ok(line))) => {
                    let value = parse_rpc_line(&line)?;
                    if value.get("type").and_then(serde_json::Value::as_str) == Some("response") {
                        handle_response(value, &mut writer, &event_tx, &mut state, &in_flight_rpc)
                            .await?;
                    } else {
                        handle_event(value, &event_tx, &outbound_tx, &mut state, launch);
                    }
                }
                Next::Line(Some(Err(error))) => {
                    return Err(format!("读取 Pi RPC 输出失败：{error}"));
                }
                Next::Line(None) => return Err("Pi RPC 进程已关闭 stdout".to_string()),
            }
        }
    }
    .await;
    // 仅发送 SIGKILL 不等于进程已经消失。先收割，再让函数返回。
    process.kill_and_reap();
    drop(background);
    result
}

type SpawnedPi = (
    async_process::ChildStdin,
    async_process::ChildStdout,
    async_process::ChildStderr,
    async_process::Child,
);

fn runtime_env_value(name: &str, value: &str) -> String {
    if name == crate::agent_kind::SMELT_AGENT_INSTRUCTIONS_ENV
        || name == crate::agent_kind::SMELT_AGENT_MODEL_PROVIDER_ENV
        || name == crate::agent_kind::SMELT_AGENT_MODEL_ID_ENV
    {
        // 自然语言 / 模型 id，不是路径。通用 launch env 为兼容 workspace/profile
        // 会展开 `~/`，在这里展开会悄悄篡改值。
        value.to_string()
    } else {
        crate::workspace_override::expand_tilde(value)
    }
}

fn agent_model_from_launch(launch: &ConversationLaunch) -> Option<(String, String)> {
    let provider = launch
        .launch
        .env
        .get(crate::agent_kind::SMELT_AGENT_MODEL_PROVIDER_ENV)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())?;
    let model = launch
        .launch
        .env
        .get(crate::agent_kind::SMELT_AGENT_MODEL_ID_ENV)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())?;
    Some((provider.to_string(), model.to_string()))
}

/// 交给子进程的 `SMELT_SOCK`。Unix 是 sock 文件。Windows 上 Bun 的
/// `net.connect` 只认命名管道，名字和 smeltd 监听的 `daemon_pipe_path` 相同。
fn daemon_sock_for_child() -> String {
    #[cfg(windows)]
    {
        if let Ok(name) = smelt_sys::named_pipe::daemon_pipe_path(
            &crate::daemon_state::smeltd_sock_path(),
        ) {
            return name;
        }
    }
    crate::daemon_state::smeltd_sock_path()
        .to_string_lossy()
        .into_owned()
}

fn spawn_process(
    launch: &ConversationLaunch,
    inline_env: BTreeMap<String, String>,
    mut process_args: Vec<String>,
    background_sock: Option<&std::path::Path>,
) -> Result<SpawnedPi, String> {
    if process_args.is_empty() {
        return Err("Pi 启动参数不完整".to_string());
    }
    let program = process_args.remove(0);
    let mut command = std::process::Command::new(program);
    command.args(process_args);
    crate::login_env::apply_login_environment(&mut command);

    let mut environment = inline_env;
    for (name, value) in &launch.launch.env {
        environment.insert(name.clone(), runtime_env_value(name, value));
    }
    for (name, value) in &launch.ephemeral_env {
        environment.insert(name.clone(), value.clone());
    }
    let path = environment
        .remove("PATH")
        .unwrap_or_else(crate::acp_conn::extended_search_path);
    command.env("PATH", path);
    command.envs(environment);
    // 会话身份与 capability token 必须由 daemon 决定；用户在旧式命令
    // 前缀或配置 env 中写了同名变量也不能覆盖。
    command.env("SMELT_SESSION_ID", &launch.sid);
    command.env("SMELT_AGENT_TOKEN", &launch.agent_token);
    command.env(
        crate::managed_runtime::PI_PROTOCOL_ENV,
        crate::managed_runtime::pi_protocol(),
    );
    command.env("SMELT_SOCK", daemon_sock_for_child());
    if let Some(path) = background_sock {
        command.env("SMELT_BACKGROUND_TASK_SOCK", path);
    }
    if let Some(cwd) = launch.cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()) {
        command.current_dir(cwd);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(
            smelt_sys::process::CREATE_SUSPENDED | smelt_sys::process::CREATE_NO_WINDOW,
        );
    }
    let mut command = async_process::Command::from(command);
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法启动 Pi RPC：{error}"))?;
    #[cfg(windows)]
    if let Err(error) = smelt_sys::process::attach_suspended(child.id()) {
        drop(child);
        return Err(format!("无法把 Pi 放进进程树：{error}"));
    }
    let stdin = child.stdin.take().ok_or("无法打开 Pi RPC stdin")?;
    let stdout = child.stdout.take().ok_or("无法打开 Pi RPC stdout")?;
    let stderr = child.stderr.take().ok_or("无法打开 Pi RPC stderr")?;
    Ok((stdin, stdout, stderr, child))
}

fn parse_logical_launch(command: &str) -> Result<(BTreeMap<String, String>, Vec<String>), String> {
    let mut environment = BTreeMap::new();
    let mut words = command.split_whitespace();
    let program = loop {
        let Some(word) = words.next() else {
            return Err("Pi 启动命令为空".to_string());
        };
        if let Some((name, value)) = crate::workspace_override::split_env_assignment(word) {
            environment.insert(
                name.to_string(),
                crate::workspace_override::expand_tilde(value),
            );
        } else {
            break word;
        }
    };
    if program != SMELT_PI_AGENT_COMMAND {
        return Err(format!("不是 Smelt Pi 逻辑入口：{program}"));
    }
    let trailing = words.map(str::to_string).collect::<Vec<_>>();
    if trailing
        .windows(2)
        .any(|pair| pair[0] == "--mode" && pair[1] != "rpc")
    {
        return Err("smelt-pi-agent 的传输固定为 Pi RPC，不能通过 --mode 切换".to_string());
    }
    Ok((environment, trailing))
}

fn spawn_stderr_drain(
    stderr: async_process::ChildStderr,
    stderr_tail: Arc<Mutex<Vec<String>>>,
) -> smol::Task<()> {
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
}

struct PiProcessGuard {
    pid: i32,
    child: async_process::Child,
    reaped: bool,
}

impl PiProcessGuard {
    fn new(child: async_process::Child) -> Self {
        Self {
            pid: child.id() as i32,
            child,
            reaped: false,
        }
    }

    /// 用 `waitpid` 确认直属子进程已经退出。`Child::try_status()` 只是问一句
    /// 「现在死了没有」；信号被掩码挡住时，隔 10ms 再杀一次也等不来结果。
    /// 先标上已收，避免这个 Child 析构时再进来一次。
    ///
    /// 先 SIGTERM，让 Pi 自己收尾：它起的 stdio MCP 服务器各在自己的进程组里，
    /// 不在 Pi 的组里，只有 Pi 的退出流程会关掉它们。直接 SIGKILL 会把它们留在
    /// launchd 下面。到时限还没退才 SIGKILL 整组。
    fn reap_now(&mut self) {
        if self.reaped {
            return;
        }
        self.reaped = true;
        let pid = self.pid;
        // 组长收尸之后组号可能被复用，所以只在等的时候按组发信号，收完不再动这个组。
        let reaped = crate::process_wait::wait_for_pid(
            pid,
            PI_EXIT_GRACE,
            move || {
                let _ = smelt_sys::process::signal_tree(
                    pid as u32,
                    smelt_sys::process::TreeSignal::Interrupt,
                );
            },
            move || {
                let _ = smelt_sys::process::signal_tree(
                    pid as u32,
                    smelt_sys::process::TreeSignal::Kill,
                );
            },
            PI_EXIT_GRACE,
        );
        if !reaped {
            // 子进程理论上仍是组长；即便它异常改了进程组，也要保证直属 child 会死。
            let _ = self.child.kill();
            let _ = crate::process_wait::reap_direct_child(pid);
        }
        if let Ok(pid) = u32::try_from(pid) {
            smelt_sys::process::release_tree(pid);
        }
    }

    fn kill_and_reap(&mut self) {
        self.reap_now();
    }
}

impl Drop for PiProcessGuard {
    fn drop(&mut self) {
        self.reap_now();
    }
}

struct HandshakeTrace {
    started: std::time::Instant,
    /// 已经发给界面的 provider / id / 显示名。用来判断这次是不是一次新的对齐。
    published_model: Option<(String, String, String)>,
}

/// 模型名在握手之前用启动前读到的会话文件发布。转录在 `Ready` 之前重放：
/// 先按 cwd 目录和会话 id 读文件；文件不存在或解析失败时再向 Pi 要 `get_entries`。
/// 两次都失败则握手失败，不把空转录当成成功。
#[allow(clippy::too_many_arguments)]
async fn initialize_session<R, W>(
    lines: &mut R,
    writer: &mut W,
    outbound_tx: &smol::channel::Sender<serde_json::Value>,
    outbound_rx: &smol::channel::Receiver<serde_json::Value>,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    launch: &ConversationLaunch,
    seed: Option<crate::session_history::PiSessionBranch>,
    trace: &mut HandshakeTrace,
) -> Result<(), String>
where
    R: Stream<Item = std::io::Result<String>> + Unpin,
    W: AsyncWrite + Unpin,
{
    let resumed = launch.resume_session_id.is_some() || launch.fork_session_id.is_some();
    if let Some(model) = optimistic_pi_model(launch, seed.as_ref(), resumed) {
        note_model(state, event_tx, launch, trace, model, "启动前");
    }

    let round_started = std::time::Instant::now();
    let responses = rpc_round(
        lines,
        writer,
        outbound_tx,
        outbound_rx,
        event_tx,
        state,
        launch,
        vec![
            ("smelt-init-state", serde_json::json!({"type": "get_state"})),
            (
                "smelt-init-models",
                serde_json::json!({"type": "get_available_models"}),
            ),
            (
                "smelt-init-thinking",
                serde_json::json!({"type": "get_available_thinking_levels"}),
            ),
            (
                "smelt-init-commands",
                serde_json::json!({"type": "get_commands"}),
            ),
        ],
    )
    .await?;
    crate::app_log::info(
        "pi-rpc",
        &format!(
            "会话 {} 独立握手 RPC 收齐，用时 {:?}",
            launch.sid,
            round_started.elapsed()
        ),
    );
    apply_state_response(
        state,
        response_data(responses.get("smelt-init-state").unwrap())?,
    )?;
    if let Some(model) = state.model.clone() {
        note_model(state, event_tx, launch, trace, model, "get_state");
    }
    state.models =
        parse_available_models(response_data(responses.get("smelt-init-models").unwrap())?);
    let model_before_set = state
        .model
        .as_ref()
        .map(|model| (model.provider.clone(), model.id.clone()));

    // 分叉切点要先落在 Pi 里，用户才能对切完的会话发消息。它不读整棵 entry 树。
    if let Some(cut) = launch.fork_cut.as_ref() {
        apply_fork_cut(
            lines,
            writer,
            outbound_tx,
            outbound_rx,
            event_tx,
            state,
            launch,
            cut,
        )
        .await?;
        let response = rpc_call(
            lines,
            writer,
            outbound_tx,
            outbound_rx,
            event_tx,
            state,
            launch,
            "smelt-init-state-fork",
            serde_json::json!({"type": "get_state"}),
        )
        .await?;
        apply_state_response(state, response_data(&response)?)?;
        if let Some(model) = state.model.clone() {
            note_model(state, event_tx, launch, trace, model, "fork 后的 get_state");
        }
    }

    let force_agent_model = agent_model_from_launch(launch);
    if force_agent_model.is_some()
        || state
            .model
            .as_ref()
            .is_none_or(|model| model.provider == "unknown" || model.id == "unknown")
    {
        let default_target = force_agent_model
            .or_else(|| settings_default_model(launch))
            .or_else(|| {
                state
                    .models
                    .first()
                    .map(|model| (model.provider.clone(), model.id.clone()))
            });
        if let Some((provider, model_id)) = default_target {
            let set_res = rpc_call(
                lines,
                writer,
                outbound_tx,
                outbound_rx,
                event_tx,
                state,
                launch,
                "smelt-init-set-model",
                serde_json::json!({
                    "type": "set_model",
                    "provider": provider,
                    "modelId": model_id,
                }),
            )
            .await;
            if let Ok(res) = set_res
                && let Some(model) = res.get("data").and_then(parse_model)
            {
                state.model = Some(model);
            }
            if state.model.is_none() {
                if let Some(model) = state
                    .models
                    .iter()
                    .find(|model| model.provider == provider && model.id == model_id)
                {
                    state.model = Some(model.clone());
                } else {
                    state.model = Some(PiModel {
                        provider: provider.clone(),
                        id: model_id.clone(),
                        name: model_id,
                        context_window: None,
                    });
                }
            }
            if let Some(model) = state.model.clone() {
                note_model(state, event_tx, launch, trace, model, "set_model");
            }
        }
    }

    let model_after_set = state
        .model
        .as_ref()
        .map(|model| (model.provider.clone(), model.id.clone()));
    let thinking = if model_before_set != model_after_set && model_after_set.is_some() {
        rpc_call(
            lines,
            writer,
            outbound_tx,
            outbound_rx,
            event_tx,
            state,
            launch,
            "smelt-init-thinking-after-model",
            serde_json::json!({"type": "get_available_thinking_levels"}),
        )
        .await?
    } else {
        responses
            .get("smelt-init-thinking")
            .cloned()
            .ok_or_else(|| "缺少 thinking level 回执".to_string())?
    };
    state.thinking_levels = response_data(&thinking)?["levels"]
        .as_array()
        .map(|levels| {
            levels
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    state.available_commands = parse_pi_commands(response_data(
        responses.get("smelt-init-commands").unwrap(),
    )?);

    publish_configuration(state, event_tx);
    let _ = event_tx.try_send(ConversationEvent::SessionControls {
        compaction: true,
        native_queue: true,
        rewind: true,
    });
    let _ = event_tx.try_send(ConversationEvent::AvailableCommands(publishable_commands(
        &state.available_commands,
    )));
    if let Some(title) = state.session_name.clone() {
        let _ = event_tx.try_send(ConversationEvent::SessionTitle(Some(title)));
    }
    if resumed {
        let session_id = if state.session_id.is_empty() {
            resume_source_id(launch).unwrap_or_default()
        } else {
            state.session_id.clone()
        };
        let messages = resume_transcript(
            lines,
            writer,
            outbound_tx,
            outbound_rx,
            event_tx,
            state,
            launch,
            &session_id,
        )
        .await?;
        crate::app_log::info(
            "pi-rpc",
            &format!(
                "会话 {} 开始重放 {} 条当前分支历史消息",
                launch.sid,
                messages.len()
            ),
        );
        // 空的也要发开始和结束：`ResumedWithReplay` 不清投影，靠这一对换掉守护和窗口里
        // 留着的旧消息。不发的话，Pi 没有的历史（比如没写进文件的那条用户消息）还挂在屏上。
        let _ = event_tx.try_send(ConversationEvent::HistoryReplayStarted);
        replay_history(&messages, event_tx, state);
    }
    let _ = event_tx.try_send(ConversationEvent::Ready {
        session_id: SessionId::new(state.session_id.clone()),
        kind: if resumed {
            ReadyKind::ResumedWithReplay
        } else {
            ReadyKind::Fresh
        },
        supports_image: true,
    });
    crate::app_log::info(
        "pi-rpc",
        &format!(
            "会话 {} 握手完成，已发出 Ready（resumed={resumed}，距启动 {:?}）",
            launch.sid,
            trace.started.elapsed()
        ),
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn resume_transcript<R, W>(
    lines: &mut R,
    writer: &mut W,
    outbound_tx: &smol::channel::Sender<serde_json::Value>,
    outbound_rx: &smol::channel::Receiver<serde_json::Value>,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    launch: &ConversationLaunch,
    session_id: &str,
) -> Result<Vec<serde_json::Value>, String>
where
    R: Stream<Item = std::io::Result<String>> + Unpin,
    W: AsyncWrite + Unpin,
{
    match read_launch_session(launch, session_id) {
        Ok(branch) => Ok(branch.entries.iter().filter_map(entry_message).collect()),
        Err(error) => {
            crate::app_log::info(
                "pi-rpc",
                &format!(
                    "会话 {} 读会话文件失败（{error}），改向 Pi 请求 get_entries",
                    launch.sid
                ),
            );
            let response = rpc_call(
                lines,
                writer,
                outbound_tx,
                outbound_rx,
                event_tx,
                state,
                launch,
                "smelt-init-entries",
                serde_json::json!({"type": "get_entries"}),
            )
            .await?;
            messages_from_get_entries(response_data(&response)?)
        }
    }
}

fn read_launch_session(
    launch: &ConversationLaunch,
    session_id: &str,
) -> Result<crate::session_history::PiSessionBranch, String> {
    let agent = launch
        .launch
        .env
        .get("PI_CODING_AGENT_DIR")
        .or_else(|| launch.ephemeral_env.get("PI_CODING_AGENT_DIR"))
        .map(String::as_str);
    let session = launch
        .launch
        .env
        .get("PI_CODING_AGENT_SESSION_DIR")
        .or_else(|| launch.ephemeral_env.get("PI_CODING_AGENT_SESSION_DIR"))
        .map(String::as_str);
    crate::session_history::read_pi_session_branch(
        agent,
        session,
        launch.cwd.as_deref(),
        session_id,
    )
}

fn resume_source_id(launch: &ConversationLaunch) -> Option<String> {
    launch
        .fork_session_id
        .as_ref()
        .or(launch.resume_session_id.as_ref())
        .map(|id| id.to_string())
        .filter(|id| !id.is_empty())
}

fn optimistic_pi_model(
    launch: &ConversationLaunch,
    seed: Option<&crate::session_history::PiSessionBranch>,
    resumed: bool,
) -> Option<PiModel> {
    if resumed
        && let Some(branch) = seed
        && let Some((provider, id)) = crate::session_history::pi_branch_model(&branch.entries)
    {
        return Some(named_pi_model(provider, id));
    }
    agent_model_from_launch(launch)
        .map(|(provider, id)| named_pi_model(provider, id))
        .or_else(|| {
            settings_default_model(launch).map(|(provider, id)| named_pi_model(provider, id))
        })
}

fn named_pi_model(provider: String, id: String) -> PiModel {
    let name = id.clone();
    PiModel {
        provider,
        id,
        name,
        context_window: None,
    }
}

fn settings_default_model(launch: &ConversationLaunch) -> Option<(String, String)> {
    let from_dir = launch
        .launch
        .env
        .get("PI_CODING_AGENT_DIR")
        .or_else(|| launch.ephemeral_env.get("PI_CODING_AGENT_DIR"))
        .map(std::path::PathBuf::from);
    let settings = if let Some(dir) = from_dir {
        crate::pi_model_settings::load_pi_model_settings_at(&dir).ok()
    } else if smelt_paths::running_under_test() {
        None
    } else {
        crate::pi_model_settings::load_pi_model_settings().ok()
    }?;
    let provider = settings.default_model.provider.trim().to_string();
    let model = settings.default_model.model.trim().to_string();
    if provider.is_empty() || model.is_empty() {
        None
    } else {
        Some((provider, model))
    }
}

fn note_model(
    state: &mut PiState,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    launch: &ConversationLaunch,
    trace: &mut HandshakeTrace,
    model: PiModel,
    reason: &str,
) {
    let next = (model.provider.clone(), model.id.clone(), model.name.clone());
    state.model = Some(model);
    if trace.published_model.as_ref() == Some(&next) {
        return;
    }
    publish_model_event(state, event_tx);
    let shown = state.model.as_ref().expect("刚刚写上");
    crate::app_log::info(
        "pi-rpc",
        &format!(
            "会话 {} 发布模型 {}/{}（{reason}），距启动 {:?}",
            launch.sid,
            shown.provider,
            shown.id,
            trace.started.elapsed()
        ),
    );
    trace.published_model = Some(next);
}

fn start_session_preload(
    launch: &ConversationLaunch,
) -> Option<std::sync::mpsc::Receiver<Option<crate::session_history::PiSessionBranch>>> {
    let id = resume_source_id(launch)?;
    let agent = launch
        .launch
        .env
        .get("PI_CODING_AGENT_DIR")
        .or_else(|| launch.ephemeral_env.get("PI_CODING_AGENT_DIR"))
        .cloned();
    let session = launch
        .launch
        .env
        .get("PI_CODING_AGENT_SESSION_DIR")
        .or_else(|| launch.ephemeral_env.get("PI_CODING_AGENT_SESSION_DIR"))
        .cloned();
    let cwd = launch.cwd.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("smelt-pi-session-file".into())
        .spawn(move || {
            let _ = tx.send(
                crate::session_history::read_pi_session_branch(
                    agent.as_deref(),
                    session.as_deref(),
                    cwd.as_deref(),
                    &id,
                )
                .ok(),
            );
        })
        .ok()?;
    Some(rx)
}

#[allow(clippy::too_many_arguments)]
async fn rpc_round<R, W>(
    lines: &mut R,
    writer: &mut W,
    outbound_tx: &smol::channel::Sender<serde_json::Value>,
    outbound_rx: &smol::channel::Receiver<serde_json::Value>,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    launch: &ConversationLaunch,
    requests: Vec<(&str, serde_json::Value)>,
) -> Result<HashMap<String, serde_json::Value>, String>
where
    R: Stream<Item = std::io::Result<String>> + Unpin,
    W: AsyncWrite + Unpin,
{
    let started = std::time::Instant::now();
    let mut pending: HashSet<String> = HashSet::new();
    for (id, mut command) in requests {
        command["id"] = serde_json::Value::String(id.to_string());
        write_rpc(writer, &command).await?;
        pending.insert(id.to_string());
    }
    let mut responses = HashMap::new();
    let mut events = 0usize;
    while !pending.is_empty() {
        enum Wait {
            Line(Option<std::io::Result<String>>),
            Outbound(Result<serde_json::Value, smol::channel::RecvError>),
        }
        match smol::future::race(async { Wait::Line(lines.next().await) }, async {
            Wait::Outbound(outbound_rx.recv().await)
        })
        .await
        {
            Wait::Outbound(Ok(message)) => {
                if message.get("type").and_then(serde_json::Value::as_str)
                    == Some("smelt_background_notify")
                {
                    if let Some(event) = background_notice_from_outbound(&message) {
                        let _ = event_tx.try_send(event);
                    }
                    continue;
                }
                write_rpc(writer, &message).await?;
            }
            Wait::Outbound(Err(_)) => return Err("Pi RPC 回执通道已关闭".to_string()),
            Wait::Line(Some(Ok(line))) => {
                let value = parse_rpc_line(&line)?;
                if value.get("type").and_then(serde_json::Value::as_str) == Some("response") {
                    let id = value
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if pending.remove(&id) {
                        ensure_response_success(&value)?;
                        crate::app_log::info(
                            "pi-rpc",
                            &format!(
                                "会话 {} 握手 {id} 用时 {:?}，响应 {} 字节（其间 {events} 条事件）",
                                launch.sid,
                                started.elapsed(),
                                line.len()
                            ),
                        );
                        responses.insert(id, value);
                    }
                    continue;
                }
                events += 1;
                handle_event(value, event_tx, outbound_tx, state, launch);
            }
            Wait::Line(Some(Err(error))) => {
                return Err(format!("读取 Pi RPC 握手响应失败：{error}"));
            }
            Wait::Line(None) => return Err("Pi RPC 在握手完成前退出".to_string()),
        }
    }
    Ok(responses)
}

#[allow(clippy::too_many_arguments)]
async fn rpc_call<R, W>(
    lines: &mut R,
    writer: &mut W,
    outbound_tx: &smol::channel::Sender<serde_json::Value>,
    outbound_rx: &smol::channel::Receiver<serde_json::Value>,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    launch: &ConversationLaunch,
    id: &str,
    mut command: serde_json::Value,
) -> Result<serde_json::Value, String>
where
    R: Stream<Item = std::io::Result<String>> + Unpin,
    W: AsyncWrite + Unpin,
{
    command["id"] = serde_json::Value::String(id.to_string());
    write_rpc(writer, &command).await?;
    let started = std::time::Instant::now();
    // 握手每一步都留痕。超时由外层的 `PI_RPC_HANDSHAKE_TIMEOUT` 统一看着，这里不再
    // 叠一层；但得能从日志里看出「哪一步、多久、其间收了多少事件」，否则一旦
    // 会话停在 Connecting，现场除了「卡着」之外没有任何可观测信息。
    let mut events = 0usize;
    loop {
        enum Wait {
            Line(Option<std::io::Result<String>>),
            Outbound(Result<serde_json::Value, smol::channel::RecvError>),
        }
        match smol::future::race(async { Wait::Line(lines.next().await) }, async {
            Wait::Outbound(outbound_rx.recv().await)
        })
        .await
        {
            Wait::Outbound(Ok(message)) => {
                if message.get("type").and_then(serde_json::Value::as_str)
                    == Some("smelt_background_notify")
                {
                    // 握手期间也留下状态行。这条不是给 Pi 的 RPC。
                    if let Some(event) = background_notice_from_outbound(&message) {
                        let _ = event_tx.try_send(event);
                    }
                    continue;
                }
                write_rpc(writer, &message).await?;
            }
            Wait::Outbound(Err(_)) => return Err("Pi RPC 回执通道已关闭".to_string()),
            Wait::Line(Some(Ok(line))) => {
                let value = parse_rpc_line(&line)?;
                if value.get("type").and_then(serde_json::Value::as_str) == Some("response") {
                    if value.get("id").and_then(serde_json::Value::as_str) == Some(id) {
                        ensure_response_success(&value)?;
                        crate::app_log::info(
                            "pi-rpc",
                            &format!(
                                "会话 {} 握手 {id} 用时 {:?}，响应 {} 字节（其间 {events} 条事件）",
                                launch.sid,
                                started.elapsed(),
                                line.len()
                            ),
                        );
                        return Ok(value);
                    }
                } else {
                    events += 1;
                    handle_event(value, event_tx, outbound_tx, state, launch);
                }
            }
            Wait::Line(Some(Err(error))) => {
                return Err(format!("读取 Pi RPC 握手响应失败：{error}"));
            }
            Wait::Line(None) => return Err("Pi RPC 在握手完成前退出".to_string()),
        }
    }
}

async fn write_rpc<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &serde_json::Value,
) -> Result<(), String> {
    let mut bytes =
        serde_json::to_vec(value).map_err(|error| format!("编码 Pi RPC 失败：{error}"))?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .await
        .map_err(|error| format!("写入 Pi RPC 失败：{error}"))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("刷新 Pi RPC stdin 失败：{error}"))
}

fn parse_rpc_line(line: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(line).map_err(|error| {
        let preview: String = line.chars().take(200).collect();
        format!("Pi RPC 输出不是有效 JSON：{error}；内容：{preview}")
    })
}

fn ensure_response_success(response: &serde_json::Value) -> Result<(), String> {
    if response.get("success").and_then(serde_json::Value::as_bool) == Some(true) {
        return Ok(());
    }
    Err(response
        .get("error")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Pi RPC 请求失败")
        .to_string())
}

fn response_data(response: &serde_json::Value) -> Result<&serde_json::Value, String> {
    ensure_response_success(response)?;
    response
        .get("data")
        .ok_or_else(|| "Pi RPC 响应缺少 data".to_string())
}

fn apply_state_response(state: &mut PiState, data: &serde_json::Value) -> Result<(), String> {
    state.session_id = data
        .get("sessionId")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or("Pi RPC 状态缺少 sessionId")?
        .to_string();
    state.session_name = data
        .get("sessionName")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    state.model = data.get("model").and_then(parse_model);
    if let Some(level) = data
        .get("thinkingLevel")
        .and_then(serde_json::Value::as_str)
    {
        state.thinking_level = level.to_string();
    }
    Ok(())
}

/// 把 `get_available_models` 的响应拼成完整模型目录。
///
/// Pi 只报它自己知道的 provider；`models.json` 里的自定义 provider 是我们这边的
/// 配置，Pi 不认，所以要在这里并进去。会话初始化和运行中刷新目录走同一条路径，
/// 否则刷新回来的列表会悄悄少掉自定义 provider。
fn parse_available_models(data: &serde_json::Value) -> Vec<PiModel> {
    let mut models: Vec<PiModel> = data["models"]
        .as_array()
        .map(|models| models.iter().filter_map(parse_model).collect())
        .unwrap_or_default();

    if let Ok(settings) = crate::pi_model_settings::load_pi_model_settings() {
        for cp in settings.custom_providers {
            for m in cp.models {
                if !models
                    .iter()
                    .any(|existing| existing.provider == cp.id && existing.id == m.id)
                {
                    models.push(PiModel {
                        provider: cp.id.clone(),
                        id: m.id.clone(),
                        name: if m.name.is_empty() {
                            m.id.clone()
                        } else {
                            m.name
                        },
                        context_window: m.context_window,
                    });
                }
            }
        }
    }
    models
}

fn parse_model(value: &serde_json::Value) -> Option<PiModel> {
    Some(PiModel {
        provider: value.get("provider")?.as_str()?.to_string(),
        id: value.get("id")?.as_str()?.to_string(),
        name: value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|n| !n.trim().is_empty())
            .or_else(|| value.get("id").and_then(serde_json::Value::as_str))?
            .to_string(),
        context_window: value
            .get("contextWindow")
            .and_then(serde_json::Value::as_u64),
    })
}

fn model_value(model: &PiModel) -> String {
    format!("{}/{}", model.provider, model.id)
}

/// 这次 `set_model` 成功后，会话当前模型就是请求里的 `provider/id`。
/// 回包只提供显示名和上下文长度，不另立一个 id。
fn apply_requested_pi_model(state: &mut PiState, value: &str, data: Option<&serde_json::Value>) {
    let from_data = data.and_then(parse_model);
    let from_list = state
        .models
        .iter()
        .find(|model| model_value(model) == value)
        .cloned();
    let Some((provider, id)) = value.split_once('/') else {
        if let Some(model) = from_data.or(from_list) {
            state.model = Some(model);
        }
        return;
    };
    let named = from_data.as_ref().or(from_list.as_ref());
    state.model = Some(PiModel {
        provider: provider.to_string(),
        id: id.to_string(),
        name: named
            .map(|model| model.name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| id.to_string()),
        context_window: named.and_then(|model| model.context_window),
    });
}

fn config_update_failure_status(detail: impl std::fmt::Display) -> String {
    format!("更新会话配置失败：{detail}")
}

fn publish_model_event(state: &PiState, event_tx: &smol::channel::Sender<ConversationEvent>) {
    let effective_model = state
        .model
        .as_ref()
        .filter(|model| model.provider != "unknown" && model.id != "unknown")
        .or_else(|| state.models.first());
    let Some(current) = effective_model else {
        return;
    };
    let mut providers = BTreeMap::<String, Vec<(String, String)>>::new();
    for model in &state.models {
        providers.entry(model.provider.clone()).or_default().push((
            format!("{}/{}", model.provider, model.id),
            model.name.clone(),
        ));
    }

    if let Ok(settings) = crate::pi_model_settings::load_pi_model_settings() {
        for cp in settings.custom_providers {
            for m in cp.models {
                let val = format!("{}/{}", cp.id, m.id);
                let list = providers.entry(cp.id.clone()).or_default();
                if !list.iter().any(|(v, _)| v == &val) {
                    let display = if m.name.is_empty() {
                        m.id.clone()
                    } else {
                        m.name
                    };
                    list.push((val, display));
                }
            }
        }
    }

    if !state
        .models
        .iter()
        .any(|m| m.provider == current.provider && m.id == current.id)
        && current.provider != "unknown"
    {
        providers
            .entry(current.provider.clone())
            .or_default()
            .push((
                format!("{}/{}", current.provider, current.id),
                current.name.clone(),
            ));
    }

    let provider_groups = providers
        .iter()
        .map(|(provider, options)| ModelProviderGroup {
            id: provider.clone(),
            name: provider.clone(),
            options: options.clone(),
        })
        .collect();
    let options = providers.into_values().flatten().collect();
    let _ = event_tx.try_send(ConversationEvent::Model(ModelState {
        config_id: "model".to_string(),
        current_value: format!("{}/{}", current.provider, current.id),
        current_name: current.name.clone(),
        options,
        provider_groups,
    }));
}

fn publish_configuration(state: &PiState, event_tx: &smol::channel::Sender<ConversationEvent>) {
    publish_model_event(state, event_tx);

    let mut levels = state.thinking_levels.clone();
    if !levels.iter().any(|level| level == &state.thinking_level) {
        levels.push(state.thinking_level.clone());
    }
    let permission_name = if state.permission_mode == "bypassPermissions" {
        "完全访问"
    } else {
        "逐次审批"
    };
    let mut configs = vec![
        SessionConfigState {
            config_id: "mode".to_string(),
            name: "权限模式".to_string(),
            description: Some("写入、执行命令和自定义工具的审批策略".to_string()),
            current_name: permission_name.to_string(),
            options: vec![
                ("default".to_string(), "逐次审批".to_string()),
                ("bypassPermissions".to_string(), "完全访问".to_string()),
            ],
            boolean: None,
        },
        SessionConfigState {
            config_id: "thought_level".to_string(),
            name: "推理强度".to_string(),
            description: Some("Pi 当前模型使用的 thinking level".to_string()),
            current_name: thinking_name(&state.thinking_level).to_string(),
            options: levels
                .into_iter()
                .map(|level| {
                    let name = thinking_name(&level).to_string();
                    (level, name)
                })
                .collect(),
            boolean: None,
        },
    ];
    if codemode_command_available(state) {
        configs.push(codemode_config(state));
    }
    let _ = event_tx.try_send(ConversationEvent::ConfigOptions(configs));
}

fn refresh_models_command_available(state: &PiState) -> bool {
    state
        .available_commands
        .iter()
        .any(|(name, _)| name == SMELT_REFRESH_MODELS_COMMAND)
}

/// 让 Pi 重读 models.json。Pi 的可用模型是会话启动时的快照，之后在 Smelt 里加的 provider
/// 它不知道：`get_available_models` 拉回来还是旧的，`set_model` 也回 Model not found。
/// 这条 prompt 只走扩展命令，回执是 handled，不开模型回合，回答进行中也立即执行。
async fn request_models_refresh<W: AsyncWrite + Unpin>(
    writer: &mut W,
    state: &mut PiState,
    then_set_model: Option<String>,
    in_flight_rpc: &AtomicUsize,
) -> Result<(), String> {
    if let Some((_, pending_model)) = &mut state.models_refresh_request {
        // 已经在重读，等那次回执。要切的模型以最后一次为准，被顶掉的那次直接还账。
        if let Some(value) = then_set_model
            && pending_model.replace(value).is_some()
        {
            finish_in_flight(in_flight_rpc);
        }
        return Ok(());
    }
    let id = state.request_id("reload-models");
    state.models_refresh_request = Some((id.clone(), then_set_model));
    write_rpc(
        writer,
        &serde_json::json!({
            "id": id,
            "type": "prompt",
            "message": format!("/{SMELT_REFRESH_MODELS_COMMAND}"),
        }),
    )
    .await
}

/// 补拉一次可用模型。响应回来时按 request id 认领（见 `models_request_id`）。
async fn request_available_models<W: AsyncWrite + Unpin>(writer: &mut W, state: &mut PiState) {
    let id = state.request_id("refresh-models");
    state.models_request_id = Some(id.clone());
    if write_rpc(
        writer,
        &serde_json::json!({"id": id, "type": "get_available_models"}),
    )
    .await
    .is_err()
    {
        state.models_request_id = None;
    }
}

async fn send_set_model<W: AsyncWrite + Unpin>(
    writer: &mut W,
    state: &mut PiState,
    value: String,
) -> Result<(), String> {
    let Some((provider, model_id)) = value.split_once('/') else {
        return Err(format!("Pi 模型值格式无效：{value}"));
    };
    let (provider, model_id) = (provider.to_string(), model_id.to_string());
    let id = state.request_id("config-model");
    state
        .pending_configs
        .insert(id.clone(), PendingConfig::Model(value));
    write_rpc(
        writer,
        &serde_json::json!({
            "id": id,
            "type": "set_model",
            "provider": provider,
            "modelId": model_id,
        }),
    )
    .await
}

fn codemode_command_available(state: &PiState) -> bool {
    state
        .available_commands
        .iter()
        .any(|(name, _)| name == SMELT_CODEMODE_COMMAND)
}

fn codemode_config(state: &PiState) -> SessionConfigState {
    SessionConfigState {
        config_id: "codemode".to_string(),
        name: "脚本工具".to_string(),
        description: Some("模型写脚本调用其它工具，下一轮生效".to_string()),
        current_name: if state.codemode_enabled { "开" } else { "关" }.to_string(),
        options: vec![
            ("off".to_string(), "关".to_string()),
            ("on".to_string(), "开".to_string()),
        ],
        boolean: None,
    }
}

/// 内部开关命令不进 slash 菜单。缺了它，会话配置行也不发布。
fn publishable_commands(commands: &[(String, String)]) -> Vec<(String, String)> {
    commands
        .iter()
        .filter(|(name, _)| name != SMELT_CODEMODE_COMMAND && name != SMELT_REFRESH_MODELS_COMMAND)
        .cloned()
        .collect()
}

fn thinking_name(level: &str) -> &str {
    match level {
        "off" => "关闭",
        "minimal" => "极简",
        "low" => "低",
        "medium" => "中",
        "high" => "高",
        "xhigh" => "很高",
        "max" => "最高",
        other => other,
    }
}

fn parse_pi_commands(data: &serde_json::Value) -> Vec<(String, String)> {
    data.get("commands")
        .and_then(serde_json::Value::as_array)
        .map(|commands| {
            commands
                .iter()
                .filter_map(|command| {
                    Some((
                        command.get("name")?.as_str()?.to_string(),
                        command
                            .get("description")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `/reload`。后面再跟字就不是这条指令，避免吞掉用户本来想发给模型的话。
pub(crate) fn parse_reload_slash(text: &str) -> bool {
    text.trim() == "/reload"
}

/// `/compact` 或 `/compact 自定义说明`。其它以 `/compact` 为前缀的词不算。
pub(crate) fn parse_compact_slash(text: &str) -> Option<Option<String>> {
    let trimmed = text.trim();
    let rest = trimmed.strip_prefix("/compact")?;
    if rest.is_empty() {
        return Some(None);
    }
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let instructions = rest.trim();
    Some(if instructions.is_empty() {
        None
    } else {
        Some(instructions.to_string())
    })
}

/// `/handoff` 或 `/handoff 自定义说明`。其它以 `/handoff` 为前缀的词不算。
pub(crate) fn parse_handoff_slash(text: &str) -> Option<Option<String>> {
    let trimmed = text.trim();
    let rest = trimmed.strip_prefix("/handoff")?;
    if rest.is_empty() {
        return Some(None);
    }
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let instructions = rest.trim();
    Some(if instructions.is_empty() {
        None
    } else {
        Some(instructions.to_string())
    })
}

/// `/shake`。后面再跟字就不是这条指令，避免吞掉用户本来想发给模型的话。
pub(crate) fn parse_shake_slash(text: &str) -> bool {
    text.trim() == "/shake"
}

/// 会话命令，不是模型输入。宿主在选 Prompt / Steer / FollowUp 之前就要认出来，
/// 驱动在写给 Pi 之前再认一次：回合中的用户文本会被宿主改写成 `Steer`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PiSessionSlash {
    Reload,
    Shake,
    Compact { custom_instructions: Option<String> },
}

pub fn parse_session_slash(text: &str) -> Option<PiSessionSlash> {
    if parse_reload_slash(text) {
        return Some(PiSessionSlash::Reload);
    }
    if parse_shake_slash(text) {
        return Some(PiSessionSlash::Shake);
    }
    if let Some(custom_instructions) = parse_compact_slash(text) {
        return Some(PiSessionSlash::Compact {
            custom_instructions,
        });
    }
    if let Some(custom_instructions) = parse_handoff_slash(text) {
        return Some(PiSessionSlash::Compact {
            custom_instructions,
        });
    }
    None
}

/// 已识别的会话命令不再交给模型。`/reload` 官方 RPC 没有对应方法，不能用 prompt
/// 模拟：那会占住 stdin 读循环，并可能停在「已用 0s」。直接拒绝。
async fn consume_session_slash<W: AsyncWrite + Unpin>(
    text: &str,
    writer: &mut W,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    in_flight_rpc: &AtomicUsize,
    turn_is_active: bool,
) -> Result<bool, String> {
    let Some(slash) = parse_session_slash(text) else {
        return Ok(false);
    };
    match slash {
        PiSessionSlash::Reload => {
            finish_in_flight(in_flight_rpc);
            let _ = event_tx.try_send(ConversationEvent::Notice(
                "当前协议不支持 /reload，不会发给模型".to_string(),
            ));
            if !turn_is_active {
                mark_provider_idle(state, event_tx);
            }
            Ok(true)
        }
        PiSessionSlash::Shake if turn_is_active => {
            finish_in_flight(in_flight_rpc);
            let _ = event_tx.try_send(ConversationEvent::Notice(
                "回合进行中，结束后再 /shake".to_string(),
            ));
            Ok(true)
        }
        PiSessionSlash::Shake => Ok(false),
        PiSessionSlash::Compact {
            custom_instructions,
        } => {
            send_compact(custom_instructions, writer, state).await?;
            Ok(true)
        }
    }
}

/// Pi 报的队列原文。不 trim、不丢空串：只有图的插话文字就是空串，丢了它，
/// 投影会以为它已经交给模型，停止时它的图也还不回来。要和发出时的原文逐字对得上。
fn json_string_list(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect()
}

fn encode_pi_images(images: Vec<crate::acp_conn::PromptImage>) -> Vec<serde_json::Value> {
    images
        .into_iter()
        .map(|image| {
            serde_json::json!({
                "type": "image",
                "data": image.data_b64,
                "mimeType": image.mime,
            })
        })
        .collect()
}

/// 后台任务完成。缺任务 id 的旧句子丢掉，不能写进 Pi，也不能当成用户 prompt。
fn background_notice_from_outbound(message: &serde_json::Value) -> Option<ConversationEvent> {
    if message.get("type").and_then(serde_json::Value::as_str) != Some("smelt_background_notify") {
        return None;
    }
    let id = message
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())?;
    Some(ConversationEvent::BackgroundNotice(
        crate::acp_chat::TaskNote {
            kind: crate::acp_chat::TaskNoteKind::BackgroundTask,
            id: id.to_string(),
            title: message
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            status: message
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            exit_code: message
                .get("exitCode")
                .and_then(serde_json::Value::as_i64)
                .and_then(|code| i32::try_from(code).ok()),
            output_tail: message
                .get("outputTail")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            runs: Vec::new(),
            peer: None,
        },
    ))
}

async fn handle_command<W: AsyncWrite + Unpin>(
    command: ConversationCommand,
    writer: &mut W,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    in_flight_rpc: &AtomicUsize,
) -> Result<(), String> {
    match command {
        ConversationCommand::Prompt { text, images } => {
            if consume_session_slash(
                &text,
                writer,
                event_tx,
                state,
                in_flight_rpc,
                state.active_turn,
            )
            .await?
            {
                return Ok(());
            }
            if state.active_turn {
                // 回合还在跑时回车必须插进当前回合。丢掉这条 prompt 会让输入
                // 框被清空、Stop 一直亮着，看起来像卡死。
                return Box::pin(handle_command(
                    ConversationCommand::Steer { text, images },
                    writer,
                    event_tx,
                    state,
                    in_flight_rpc,
                ))
                .await;
            }
            state.active_turn = true;
            state.prompt_accepted = false;
            state.provider_busy = true;
            state.cancel_requested = false;
            state.last_stop_reason = None;
            state.last_error = None;
            let id = state.request_id("prompt");
            state.prompt_request_id = Some(id.clone());
            let images = encode_pi_images(images);
            let mut request = serde_json::json!({
                "id": id,
                "type": "prompt",
                "message": text,
            });
            if !images.is_empty() {
                request["images"] = serde_json::Value::Array(images);
            }
            write_rpc(writer, &request).await?;
            state.prompts_sent += 1;
            state.prompt_sent_at = Some(std::time::Instant::now());
        }
        ConversationCommand::Steer { text, images } => {
            // steer 只对正在跑的回合有意义。界面和 Pi 都空了就降级成普通 prompt，
            // 否则这条消息会被 Pi 丢掉（用户看得到回显却永远等不到回复）。
            // Pi 还忙着时不能降级：裸 prompt 会被直接拒绝。
            if !state.active_turn && !state.provider_busy {
                return Box::pin(handle_command(
                    ConversationCommand::Prompt { text, images },
                    writer,
                    event_tx,
                    state,
                    in_flight_rpc,
                ))
                .await;
            }
            if consume_session_slash(&text, writer, event_tx, state, in_flight_rpc, true).await? {
                return Ok(());
            }
            let id = state.request_id("steer");
            state.steer_request_ids.insert(id.clone());
            state.queue.sent(
                QueueLane::Steering,
                id.clone(),
                text.clone(),
                images.clone(),
            );
            let mut request = serde_json::json!({
                "id": id,
                "type": "steer",
                "message": text,
            });
            let images = encode_pi_images(images);
            if !images.is_empty() {
                request["images"] = serde_json::Value::Array(images);
            }
            write_rpc(writer, &request).await?;
        }
        ConversationCommand::FollowUp { text, images } => {
            if !state.active_turn && !state.provider_busy {
                return Box::pin(handle_command(
                    ConversationCommand::Prompt { text, images },
                    writer,
                    event_tx,
                    state,
                    in_flight_rpc,
                ))
                .await;
            }
            if consume_session_slash(&text, writer, event_tx, state, in_flight_rpc, true).await? {
                return Ok(());
            }
            let id = state.request_id("follow-up");
            state.follow_up_request_ids.insert(id.clone());
            state.queue.sent(
                QueueLane::FollowUp,
                id.clone(),
                text.clone(),
                images.clone(),
            );
            let mut request = serde_json::json!({
                "id": id,
                "type": "follow_up",
                "message": text,
            });
            let images = encode_pi_images(images);
            if !images.is_empty() {
                request["images"] = serde_json::Value::Array(images);
            }
            write_rpc(writer, &request).await?;
        }
        ConversationCommand::Compact {
            custom_instructions,
        } => {
            send_compact(custom_instructions, writer, state).await?;
        }
        ConversationCommand::ClearQueue => {
            send_clear_queue(writer, state, true).await?;
        }
        ConversationCommand::Rewind {
            text,
            occurrence,
            truncate_from,
            then,
        } => {
            // daemon 发命令前记了一笔在途 RPC，拒绝也要销账，否则热升级永远等不到它。
            if state.active_turn {
                abort_rewind(
                    event_tx,
                    in_flight_rpc,
                    "回退失败：回合还在运行，请先停止".to_string(),
                );
                return Ok(());
            }
            if state.pending_rewind.is_some() {
                abort_rewind(
                    event_tx,
                    in_flight_rpc,
                    "回退失败：已有一次回退在进行中".to_string(),
                );
                return Ok(());
            }
            let list_id = state.request_id("rewind-list");
            state.pending_rewind = Some(PendingRewind {
                truncate_from,
                occurrence,
                text,
                then,
                list_request_id: list_id.clone(),
                fork_request_id: None,
            });
            write_rpc(
                writer,
                &serde_json::json!({"id": list_id, "type": "get_fork_messages"}),
            )
            .await?;
        }
        ConversationCommand::Cancel => {
            // 界面立刻停。Pi 还在不在跑，要等 get_state 或 agent_settled，不能在这里放行。
            state.cancel_requested = true;
            if state.active_turn {
                finish_turn(state, event_tx, None);
            } else {
                state.cancel_requested = false;
                let _ = event_tx.try_send(ConversationEvent::TurnEnded(StopReason::Cancelled));
            }
            send_clear_queue(writer, state, false).await?;
            let abort_id = state.request_id("abort");
            write_rpc(
                writer,
                &serde_json::json!({"id": abort_id, "type": "abort"}),
            )
            .await?;
            let state_id = state.request_id("cancel-state");
            state.cancel_state_request_id = Some(state_id.clone());
            if write_rpc(
                writer,
                &serde_json::json!({"id": state_id, "type": "get_state"}),
            )
            .await
            .is_err()
            {
                state.cancel_state_request_id = None;
                mark_provider_idle(state, event_tx);
            }
        }
        ConversationCommand::SetConfigOption { config_id, value } => {
            handle_config_command(config_id, value, writer, event_tx, state, in_flight_rpc).await?;
        }
        ConversationCommand::RefreshModels => {
            // 发出去就不管了，回执按 request id 认领。这里不能同步等——命令循环在这条
            // 路上没有 reader，等下去会把整条会话卡死。先让 Pi 重读 models.json，
            // 回执到了再拉列表，不然拉回来的还是会话启动时的快照。
            if refresh_models_command_available(state) {
                request_models_refresh(writer, state, None, in_flight_rpc).await?;
            } else {
                request_available_models(writer, state).await;
            }
        }
        ConversationCommand::StopBackgroundTask { id } => {
            let result = match &state.background {
                Some(background) => background.stop(&id),
                None => Err("后台任务服务还没就绪".to_string()),
            };
            if let Err(error) = result {
                let _ = event_tx.try_send(ConversationEvent::Notice(format!(
                    "停止后台任务失败：{error}"
                )));
            }
        }
        ConversationCommand::DeliverPeerMessage(delivery) => match &state.background {
            Some(background) => background.deliver_peer(delivery),
            None => {
                let _ = event_tx.try_send(ConversationEvent::Notice(
                    "会话宿主还没就绪，丢掉了一条别的对话发来的消息".to_string(),
                ));
            }
        },
        ConversationCommand::StopSubagent { id } => {
            let result = match &state.background {
                Some(background) => background.stop_subagent(&id),
                None => Err("子智能体服务还没就绪".to_string()),
            };
            if let Err(error) = result {
                let _ = event_tx.try_send(ConversationEvent::Notice(format!(
                    "停止子智能体失败：{error}"
                )));
            }
        }
        ConversationCommand::SetSessionTitle(title) => {
            let title = title.trim().to_string();
            // Pi 的 set_session_name 拒绝空名字；清空标题只在本地生效。
            if !title.is_empty() {
                state.session_name = Some(title.clone());
                let id = state.request_id("session-name");
                write_rpc(
                    writer,
                    &serde_json::json!({"id": id, "type": "set_session_name", "name": title}),
                )
                .await?;
            }
        }
        // 这两种命令只供 ACP driver 的异步 request callback 回送结果。Pi RPC 的
        // response 直接在本模块解析，不会生成它们。
        ConversationCommand::PromptSettled(_)
        | ConversationCommand::TurnCompleted
        | ConversationCommand::Shutdown => {}
    }
    Ok(())
}

async fn send_compact<W: AsyncWrite + Unpin>(
    custom_instructions: Option<String>,
    writer: &mut W,
    state: &mut PiState,
) -> Result<(), String> {
    let id = state.request_id("compact");
    state.compact_request_ids.insert(id.clone());
    let mut request = serde_json::json!({"id": id, "type": "compact"});
    if let Some(instructions) = custom_instructions.filter(|text| !text.is_empty()) {
        request["customInstructions"] = serde_json::Value::String(instructions);
    }
    write_rpc(writer, &request).await
}

async fn send_clear_queue<W: AsyncWrite + Unpin>(
    writer: &mut W,
    state: &mut PiState,
    then_abort: bool,
) -> Result<(), String> {
    let id = state.request_id("clear-queue");
    state.pending_clear = Some((id.clone(), then_abort));
    write_rpc(
        writer,
        &serde_json::json!({"id": id, "type": "clear_queue"}),
    )
    .await
}

/// 是 Pi 自己确认收进队列的 steer / follow_up。被输入处理器接走的不算。
fn queued_by_pi(response: &serde_json::Value) -> bool {
    ensure_response_success(response).is_ok()
        && response
            .get("data")
            .and_then(|data| data.get("disposition"))
            .and_then(serde_json::Value::as_str)
            != Some("handled")
}

fn emit_queue_restore(
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    restore: Vec<(String, Vec<crate::acp_conn::PromptImage>)>,
) {
    let (texts, images): (Vec<_>, Vec<_>) = restore.into_iter().unzip();
    state.composer_restore_seq = state.composer_restore_seq.saturating_add(1);
    // 只发还原，不清空队列当「已插入」。否则 apply 会把撤回的 steering
    // 写成会话气泡，和还回输入框叠在一起。
    let _ = event_tx.try_send(ConversationEvent::ComposerRestore {
        revision: state.composer_restore_seq,
        texts,
        images,
    });
}

async fn handle_config_command<W: AsyncWrite + Unpin>(
    config_id: String,
    value: ConfigValue,
    writer: &mut W,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    in_flight_rpc: &AtomicUsize,
) -> Result<(), String> {
    let ConfigValue::Select(value) = value else {
        finish_in_flight(in_flight_rpc);
        let _ = event_tx.try_send(ConversationEvent::Status(config_update_failure_status(
            format!("Pi 配置 `{config_id}` 不接受布尔值"),
        )));
        return Ok(());
    };
    match config_id.as_str() {
        "mode" if matches!(value.as_str(), "default" | "bypassPermissions") => {
            state.permission_mode = value;
            publish_configuration(state, event_tx);
            finish_in_flight(in_flight_rpc);
        }
        "thought_level" => {
            let id = state.request_id("config-thinking");
            state
                .pending_configs
                .insert(id.clone(), PendingConfig::Thinking(value.clone()));
            write_rpc(
                writer,
                &serde_json::json!({"id": id, "type": "set_thinking_level", "level": value}),
            )
            .await?;
        }
        "codemode" if matches!(value.as_str(), "off" | "on") => {
            if !codemode_command_available(state) {
                finish_in_flight(in_flight_rpc);
                let _ = event_tx.try_send(ConversationEvent::Status(config_update_failure_status(
                    "当前 Pi 没有脚本工具",
                )));
                return Ok(());
            }
            // Pi 没有 set_active_tools。这条 prompt 只走扩展命令，回执是 handled，
            // 不开模型回合，也不能记成用户消息。
            let id = state.request_id("config-codemode");
            state
                .pending_configs
                .insert(id.clone(), PendingConfig::Codemode(value.clone()));
            write_rpc(
                writer,
                &serde_json::json!({
                    "id": id,
                    "type": "prompt",
                    "message": format!("/{SMELT_CODEMODE_COMMAND} {value}"),
                }),
            )
            .await?;
        }
        "model" => {
            if !value.contains('/') {
                finish_in_flight(in_flight_rpc);
                let _ = event_tx.try_send(ConversationEvent::Status(config_update_failure_status(
                    "Pi 模型值格式无效",
                )));
                return Ok(());
            }
            // 菜单按磁盘上的 models.json 列模型，这个会话的 Pi 可能还不认识（会话启动后才加的
            // provider）。先让它重读，回执到了再切。
            let known = state.models.iter().any(|model| model_value(model) == value);
            if !known && refresh_models_command_available(state) {
                request_models_refresh(writer, state, Some(value), in_flight_rpc).await?;
            } else {
                // 直接切的这次是最新选择：还在等重读的那次作废，不然回执到了会把它切回去。
                if let Some((_, pending_model)) = &mut state.models_refresh_request
                    && pending_model.take().is_some()
                {
                    finish_in_flight(in_flight_rpc);
                }
                send_set_model(writer, state, value).await?;
            }
        }
        _ => {
            finish_in_flight(in_flight_rpc);
            let _ = event_tx.try_send(ConversationEvent::Status(config_update_failure_status(
                format!("Pi 不支持会话配置 `{config_id}` = `{value}`"),
            )));
        }
    }
    Ok(())
}

async fn handle_response<W: AsyncWrite + Unpin>(
    response: serde_json::Value,
    writer: &mut W,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    in_flight_rpc: &AtomicUsize,
) -> Result<(), String> {
    let id = response
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    if state.prompt_request_id.as_deref() == Some(&id) {
        state.prompt_request_id = None;
        // Pi 回这条 response 之前，这一轮还没真正开跑。首条通常最慢（扩展、MCP、
        // skills 都在第一轮 run 里初始化），没有这行日志就只能靠体感。
        if let Some(sent_at) = state.prompt_sent_at.take() {
            crate::app_log::info(
                "pi-rpc",
                &format!(
                    "会话 {} 第 {} 条 prompt 被 Pi 收下用时 {:?}（气泡在提交时已落地）",
                    state.sid,
                    state.prompts_sent,
                    sent_at.elapsed()
                ),
            );
        }
        if let Err(error) = ensure_response_success(&response) {
            // 先问 Pi 还在不在跑。忙着就不是这一条的失败，空了才是真的预检错误。
            state.pending_refusal_error = Some(error);
            let check_id = state.request_id("refusal-state");
            state.refusal_state_request_id = Some(check_id.clone());
            if write_rpc(
                writer,
                &serde_json::json!({"id": check_id, "type": "get_state"}),
            )
            .await
            .is_err()
            {
                state.refusal_state_request_id = None;
                let error = state
                    .pending_refusal_error
                    .take()
                    .unwrap_or_else(|| "Pi RPC 请求失败".to_string());
                resolve_prompt_refusal(state, event_tx, error, None);
            }
        } else {
            if state.active_turn {
                state.prompt_accepted = true;
                state.compaction_idle_pending = false;
            }
            let _ = event_tx.try_send(ConversationEvent::PromptAccepted);
            if state.active_turn {
                // Extension command/input handler 可以不启动模型回合。询问权威状态，避免
                // 只等 agent_settled 导致这种 prompt 永远卡在 Running。
                let check_id = state.request_id("prompt-state");
                state.prompt_state_request_id = Some(check_id.clone());
                write_rpc(
                    writer,
                    &serde_json::json!({"id": check_id, "type": "get_state"}),
                )
                .await?;
            }
        }
        return Ok(());
    }
    if state.prompt_state_request_id.as_deref() == Some(&id) {
        state.prompt_state_request_id = None;
        match ensure_response_success(&response) {
            Ok(()) => {
                let data = response.get("data").cloned().unwrap_or_default();
                // 缺 isStreaming 时仍当作还在跑，避免扩展命令还没真正启动就被提前收尾。
                let streaming = data
                    .get("isStreaming")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                if !streaming {
                    if state.active_turn {
                        finish_turn(state, event_tx, None);
                    }
                    mark_provider_idle(state, event_tx);
                } else {
                    state.provider_busy = true;
                }
            }
            Err(error) => {
                if refusal_is_provider_busy(&error) {
                    state.provider_busy = true;
                } else if state.active_turn {
                    finish_turn(state, event_tx, Some(error));
                    mark_provider_idle(state, event_tx);
                } else {
                    mark_provider_idle(state, event_tx);
                }
            }
        }
        return Ok(());
    }
    if state.refusal_state_request_id.as_deref() == Some(&id) {
        state.refusal_state_request_id = None;
        let error = state
            .pending_refusal_error
            .take()
            .unwrap_or_else(|| "Pi RPC 请求失败".to_string());
        match ensure_response_success(&response) {
            Ok(()) => {
                let data = response.get("data").cloned().unwrap_or_default();
                resolve_prompt_refusal(state, event_tx, error, Some(&data));
            }
            Err(state_error) => {
                if refusal_is_provider_busy(&state_error) {
                    resolve_prompt_refusal(
                        state,
                        event_tx,
                        error,
                        Some(&serde_json::json!({"isStreaming": true})),
                    );
                } else {
                    resolve_prompt_refusal(state, event_tx, error, None);
                }
            }
        }
        return Ok(());
    }
    if state.models_request_id.as_deref() == Some(&id) {
        state.models_request_id = None;
        // 拉失败就维持旧目录：模型列表突然变空比列表过期严重得多。
        if let Ok(()) = ensure_response_success(&response) {
            let data = response.get("data").cloned().unwrap_or_default();
            let models = parse_available_models(&data);
            if !models.is_empty() && models != state.models {
                state.models = models;
                publish_configuration(state, event_tx);
            }
        }
        return Ok(());
    }
    if state.cancel_state_request_id.as_deref() == Some(&id) {
        state.cancel_state_request_id = None;
        match ensure_response_success(&response) {
            Ok(()) => {
                let data = response.get("data").cloned().unwrap_or_default();
                if provider_snapshot_busy(&data) {
                    state.provider_busy = true;
                } else {
                    mark_provider_idle(state, event_tx);
                }
            }
            Err(error) => {
                if refusal_is_provider_busy(&error) {
                    state.provider_busy = true;
                } else {
                    mark_provider_idle(state, event_tx);
                }
            }
        }
        return Ok(());
    }
    if state.steer_request_ids.remove(&id) {
        let result = ensure_response_success(&response);
        state.queue.acknowledged(&id, queued_by_pi(&response));
        if let Err(error) = result {
            let _ = event_tx.try_send(ConversationEvent::Notice(format!("插入消息失败：{error}")));
        }
        finish_in_flight(in_flight_rpc);
        return Ok(());
    }
    if state.follow_up_request_ids.remove(&id) {
        let result = ensure_response_success(&response);
        state.queue.acknowledged(&id, queued_by_pi(&response));
        if let Err(error) = result {
            let _ = event_tx.try_send(ConversationEvent::Notice(format!(
                "排队后续消息失败：{error}"
            )));
        }
        finish_in_flight(in_flight_rpc);
        return Ok(());
    }
    if state.compact_request_ids.remove(&id) {
        if let Err(error) = ensure_response_success(&response) {
            let _ = event_tx.try_send(ConversationEvent::Notice(format!(
                "压缩上下文失败：{error}"
            )));
        }
        finish_in_flight(in_flight_rpc);
        return Ok(());
    }
    if state
        .pending_clear
        .as_ref()
        .is_some_and(|(pending, _)| pending == &id)
    {
        let then_abort = state.pending_clear.take().is_some_and(|(_, abort)| abort);
        match ensure_response_success(&response) {
            Ok(()) => {
                if let Some(background) = &state.background {
                    background.queue_cleared();
                }
                let data = response.get("data").cloned().unwrap_or_default();
                let restore = state.queue.cleared(
                    json_string_list(&data, "steering"),
                    json_string_list(&data, "followUp"),
                );
                emit_queue_restore(event_tx, state, restore);
            }
            Err(error) => {
                let _ =
                    event_tx.try_send(ConversationEvent::Notice(format!("清空队列失败：{error}")));
            }
        }
        if then_abort {
            finish_in_flight(in_flight_rpc);
        }
        return Ok(());
    }
    if state.latest_stats_request_id.as_deref() == Some(&id) {
        state.latest_stats_request_id = None;
        if ensure_response_success(&response).is_ok() {
            publish_session_stats(
                response.get("data").unwrap_or(&serde_json::Value::Null),
                state.context_window(),
                event_tx,
            );
        }
        return Ok(());
    }
    if state
        .rewind_state_request_id
        .as_deref()
        .is_some_and(|pending| pending == id)
    {
        state.rewind_state_request_id = None;
        // fork 已经切到新 session 文件，这里只补齐身份；失败不致命——旧 id
        // 只影响下一次断线重连恢复到哪条分支，不值得为此打断会话。
        if let Ok(data) = response_data(&response) {
            let _ = apply_state_response(state, data);
            let _ = event_tx.try_send(ConversationEvent::ProviderSessionIdChanged(SessionId::new(
                state.session_id.clone(),
            )));
        }
        finish_in_flight(in_flight_rpc);
        return Ok(());
    }
    if let Some(pending) = state.pending_rewind.take_if(|pending| {
        pending.list_request_id == id || pending.fork_request_id.as_deref() == Some(&id)
    }) {
        if pending.fork_request_id.as_deref() == Some(&id) {
            handle_rewind_fork_response(pending, &response, writer, event_tx, state, in_flight_rpc)
                .await?;
        } else {
            handle_rewind_list_response(pending, &response, writer, event_tx, state, in_flight_rpc)
                .await?;
        }
        return Ok(());
    }
    if let Some((_, then_set_model)) = state
        .models_refresh_request
        .take_if(|(pending, _)| *pending == id)
    {
        // 重读失败也往下走：列表维持旧的，模型找不到由 set_model 自己报。
        request_available_models(writer, state).await;
        if let Some(value) = then_set_model {
            // in_flight 记在这次切模型上，等 set_model 的回执再还。
            send_set_model(writer, state, value).await?;
        }
        return Ok(());
    }
    if let Some(pending) = state.pending_configs.remove(&id) {
        match ensure_response_success(&response) {
            Ok(()) => match pending {
                PendingConfig::Model(value) => {
                    apply_requested_pi_model(state, &value, response.get("data"));
                }
                PendingConfig::Thinking(level) => state.thinking_level = level,
                PendingConfig::Codemode(requested) => {
                    let enabled = requested == "on";
                    let handled = response
                        .get("data")
                        .and_then(|data| data.get("disposition"))
                        .and_then(serde_json::Value::as_str)
                        == Some("handled");
                    if !handled || state.codemode_enabled != enabled {
                        let _ = event_tx.try_send(ConversationEvent::Status(
                            config_update_failure_status("脚本工具没有在下一轮生效"),
                        ));
                    }
                }
            },
            Err(error) => {
                let _ = event_tx.try_send(ConversationEvent::Status(config_update_failure_status(
                    error,
                )));
            }
        }
        publish_configuration(state, event_tx);
        finish_in_flight(in_flight_rpc);
    }
    Ok(())
}

fn finish_in_flight(counter: &AtomicUsize) {
    let _ = counter.try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
        value.checked_sub(1)
    });
}

/// 在 agent 的可分叉消息列表里按「文本精确匹配 + 同文本序号」定位 entryId。
/// 回退与分叉切点共用：事件流不带 entryId，只能按文本配对，`occurrence`
/// 是同文本消息中的第几条（从 0 数起）。
fn match_fork_entry_id(
    messages: &[serde_json::Value],
    text: &str,
    occurrence: usize,
) -> Option<String> {
    messages
        .iter()
        .filter(|message| message.get("text").and_then(serde_json::Value::as_str) == Some(text))
        .filter_map(|message| message.get("entryId").and_then(serde_json::Value::as_str))
        .nth(occurrence)
        .map(str::to_string)
}

/// 分叉副本的启动期切点：`--fork` 整拷文件打开后、重放历史前，把活动分支
/// 切到 `fork_cut` 指定的用户消息之前（不含它）。此时连接刚建立，RPC 都是
/// 串行问答（`rpc_call`），没有用户输入与回合状态可被破坏，失败只需把错误
/// 抛给握手——切点配不上意味着 agent 历史与发起方投影分叉，重放错误的历史
/// 比失败史糟。
#[allow(clippy::too_many_arguments)]
async fn apply_fork_cut<R, W>(
    lines: &mut R,
    writer: &mut W,
    outbound_tx: &smol::channel::Sender<serde_json::Value>,
    outbound_rx: &smol::channel::Receiver<serde_json::Value>,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    launch: &ConversationLaunch,
    cut: &AcpForkCut,
) -> Result<(), String>
where
    R: Stream<Item = std::io::Result<String>> + Unpin,
    W: AsyncWrite + Unpin,
{
    let response = rpc_call(
        lines,
        writer,
        outbound_tx,
        outbound_rx,
        event_tx,
        state,
        launch,
        "smelt-fork-cut-list",
        serde_json::json!({"type": "get_fork_messages"}),
    )
    .await?;
    let messages = response_data(&response)?["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let Some(entry_id) = match_fork_entry_id(&messages, &cut.text, cut.occurrence) else {
        return Err(format!(
            "分叉切点在 agent 历史里找不到（可能已被压缩）：{}",
            cut.text.chars().take(50).collect::<String>()
        ));
    };
    let response = rpc_call(
        lines,
        writer,
        outbound_tx,
        outbound_rx,
        event_tx,
        state,
        launch,
        "smelt-fork-cut",
        serde_json::json!({"type": "fork", "entryId": entry_id}),
    )
    .await?;
    if response_data(&response)?
        .get("cancelled")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return Err("分叉切点被扩展取消".to_string());
    }
    Ok(())
}

/// 回退第一阶段回执：拿 `get_fork_messages` 的消息列表按文本配对出 entryId，
/// 再发 `fork`。配不上说明 agent 侧分支和本地投影已分叉（比如 /compact
/// 只在本地显示、没进 agent 历史），如实报错，不猜最近似。
#[allow(clippy::too_many_arguments)]
async fn handle_rewind_list_response<W: AsyncWrite + Unpin>(
    mut pending: PendingRewind,
    response: &serde_json::Value,
    writer: &mut W,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    in_flight_rpc: &AtomicUsize,
) -> Result<(), String> {
    let messages = match response_data(response) {
        Ok(data) => data.get("messages"),
        Err(error) => {
            abort_rewind(event_tx, in_flight_rpc, format!("回退失败：{error}"));
            return Ok(());
        }
    };
    let entry_id = messages
        .and_then(serde_json::Value::as_array)
        .and_then(|messages| match_fork_entry_id(messages, &pending.text, pending.occurrence));
    let Some(entry_id) = entry_id else {
        abort_rewind(
            event_tx,
            in_flight_rpc,
            "回退失败：agent 分支上找不到这条消息（可能已被压缩或属于另一条分支）".to_string(),
        );
        return Ok(());
    };
    let fork_id = state.request_id("rewind-fork");
    pending.fork_request_id = Some(fork_id.clone());
    let result = write_rpc(
        writer,
        &serde_json::json!({"id": fork_id, "type": "fork", "entryId": entry_id}),
    )
    .await;
    match result {
        Ok(()) => {
            state.pending_rewind = Some(pending);
        }
        Err(error) => abort_rewind(event_tx, in_flight_rpc, format!("回退失败：{error}")),
    }
    Ok(())
}

/// 回退第二阶段回执：fork 成功后切到新分叉。被扩展取消（cancelled）或失败
/// 时保持原分支不动；成功时广播 `Rewound`（截断游标和 `then`），编辑重发再加
/// `ComposerRestore`（原文回输入框），然后补一发 `get_state` 换 session id。
/// 重试不还原输入框：daemon 收到 `Rewound` 后按普通发送再发一次。
#[allow(clippy::too_many_arguments)]
/// fork 按切点那条分支重建会话，模型和推理强度也回到当时的设置。出错后换的模型记在被丢掉
/// 的那一段里，不补就跟着没了，重试还是用出错的那个模型。按回退前的设置再设一次：Pi 按顺序
/// 处理请求，这两条排在重发的消息和随后的 get_state 前面。回执不认领，结果由 get_state 带回。
async fn restore_settings_after_fork<W: AsyncWrite + Unpin>(writer: &mut W, state: &mut PiState) {
    if let Some(model) = state
        .model
        .clone()
        .filter(|model| model.provider != "unknown" && model.id != "unknown")
    {
        let id = state.request_id("rewind-model");
        let _ = write_rpc(
            writer,
            &serde_json::json!({
                "id": id,
                "type": "set_model",
                "provider": model.provider,
                "modelId": model.id,
            }),
        )
        .await;
    }
    if !state.thinking_level.is_empty() {
        let id = state.request_id("rewind-thinking");
        let level = state.thinking_level.clone();
        let _ = write_rpc(
            writer,
            &serde_json::json!({"id": id, "type": "set_thinking_level", "level": level}),
        )
        .await;
    }
}

async fn handle_rewind_fork_response<W: AsyncWrite + Unpin>(
    pending: PendingRewind,
    response: &serde_json::Value,
    writer: &mut W,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    in_flight_rpc: &AtomicUsize,
) -> Result<(), String> {
    let data = match response_data(response) {
        Ok(data) => data,
        Err(error) => {
            abort_rewind(event_tx, in_flight_rpc, format!("回退失败：{error}"));
            return Ok(());
        }
    };
    if data.get("cancelled").and_then(serde_json::Value::as_bool) == Some(true) {
        abort_rewind(event_tx, in_flight_rpc, "回退被扩展取消".to_string());
        return Ok(());
    }
    // fork 会中止运行中的 turn；正常路径下回退只在 Idle 时发起，这里只是
    // 纯防御，避免极端时序下卡在 Running。
    if state.active_turn {
        state.active_turn = false;
        state.cancel_requested = false;
    }
    // 换到了新分叉，旧会话的队列跟着作废。
    state.queue.reset();
    let restore =
        matches!(pending.then, crate::acp_conn::AfterRewind::RestoreComposer).then(|| {
            data.get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&pending.text)
                .to_string()
        });
    restore_settings_after_fork(writer, state).await;
    let _ = event_tx.try_send(ConversationEvent::Rewound {
        truncate_from: pending.truncate_from,
        then: pending.then,
    });
    // in_flight 记账挂到 get_state 回执上：fork 已成功，但身份刷新还没落地，
    // 热升级仍不得跨越。
    let state_id = state.request_id("rewind-state");
    state.rewind_state_request_id = Some(state_id.clone());
    if write_rpc(
        writer,
        &serde_json::json!({"id": state_id, "type": "get_state"}),
    )
    .await
    .is_err()
    {
        // 身份刷不上新 id 只是“下次重连恢复到旧分支”的退化，回退本身已成功。
        state.rewind_state_request_id = None;
        if let Some(prefill) = restore {
            emit_queue_restore(event_tx, state, vec![(prefill, Vec::new())]);
        }
        finish_in_flight(in_flight_rpc);
    } else if let Some(prefill) = restore {
        emit_queue_restore(event_tx, state, vec![(prefill, Vec::new())]);
    }
    Ok(())
}

fn abort_rewind(
    event_tx: &smol::channel::Sender<ConversationEvent>,
    in_flight_rpc: &AtomicUsize,
    message: String,
) {
    let _ = event_tx.try_send(ConversationEvent::Notice(message));
    finish_in_flight(in_flight_rpc);
}

/// 快照里缺字段不算在跑。成功路径另有「缺 isStreaming 就当作还在跑」，
/// 那是为了别把还没启动的扩展命令提前收尾，不用于放行下一条 prompt。
fn provider_snapshot_busy(data: &serde_json::Value) -> bool {
    data.get("isStreaming").and_then(serde_json::Value::as_bool) == Some(true)
        || data
            .get("isCompacting")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

fn refusal_is_provider_busy(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("already processing") || error.contains("compaction is in progress")
}

fn mark_provider_idle(state: &mut PiState, event_tx: &smol::channel::Sender<ConversationEvent>) {
    state.provider_busy = false;
    let _ = event_tx.try_send(ConversationEvent::ProviderIdle);
}

/// 这条 prompt 没被收下，不能再占着本地回合。留下 `active_turn` 会把随后放行的
/// 裸 prompt 改写成 steer。
fn release_unaccepted_admission(
    state: &mut PiState,
    event_tx: &smol::channel::Sender<ConversationEvent>,
) {
    state.active_turn = false;
    state.prompt_accepted = false;
    state.provider_busy = false;
    state.compaction_idle_pending = false;
    let _ = event_tx.try_send(ConversationEvent::ProviderIdle);
}

fn prompt_admission_unresolved(state: &PiState) -> bool {
    state.prompt_request_id.is_some()
        || state.prompt_state_request_id.is_some()
        || state.refusal_state_request_id.is_some()
        || state.pending_refusal_error.is_some()
}

/// 快照说还在跑，就继续关着进门。压缩已经结束后，过期的 `isCompacting` 不再算忙。
fn refusal_snapshot_busy(state: &PiState, data: &serde_json::Value) -> bool {
    if data.get("isStreaming").and_then(serde_json::Value::as_bool) == Some(true) {
        return true;
    }
    data.get("isCompacting")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        && !state.compaction_idle_pending
}

/// 用 Pi 的当前快照决定这次拒绝是「还在跑」还是「这条 prompt 自己失败了」。
fn resolve_prompt_refusal(
    state: &mut PiState,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    error: String,
    snapshot: Option<&serde_json::Value>,
) {
    let streaming = snapshot
        .and_then(|data| data.get("isStreaming"))
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let busy = match snapshot {
        Some(data) => refusal_snapshot_busy(state, data),
        None => refusal_is_provider_busy(&error) && !state.compaction_idle_pending,
    };
    if busy {
        if streaming {
            state.compaction_idle_pending = false;
        }
        state.provider_busy = true;
        let _ = event_tx.try_send(ConversationEvent::PromptNotAccepted);
        return;
    }
    state.compaction_idle_pending = false;
    state.provider_busy = false;
    if refusal_is_provider_busy(&error) {
        // 拒绝和快照之间这轮已经结束。消息退回队列，不记成回合失败。
        state.active_turn = false;
        state.prompt_accepted = false;
        let _ = event_tx.try_send(ConversationEvent::PromptNotAccepted);
        let _ = event_tx.try_send(ConversationEvent::ProviderIdle);
        return;
    }
    if state.active_turn {
        finish_turn(state, event_tx, Some(error));
    }
    mark_provider_idle(state, event_tx);
}

fn finish_turn(
    state: &mut PiState,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    explicit_error: Option<String>,
) {
    if !state.active_turn {
        return;
    }
    state.active_turn = false;
    state.prompt_accepted = false;
    // 不在这里清 provider_busy，也不清还在等的请求 id。
    // 停止可以先结束界面，Pi 的 prompt 回执和空闲信号还要靠这些 id 对上。
    let reason = state.last_stop_reason.take();
    let error = explicit_error.or_else(|| state.last_error.take());
    let event = if state.cancel_requested || reason.as_deref() == Some("aborted") {
        ConversationEvent::TurnEnded(StopReason::Cancelled)
    } else if reason.as_deref() == Some("error") {
        ConversationEvent::TurnFailed(error.unwrap_or_else(|| "Pi 模型请求失败".to_string()))
    } else if reason.as_deref() == Some("length") {
        ConversationEvent::TurnEnded(StopReason::MaxTokens)
    } else if let Some(error) = error {
        ConversationEvent::TurnFailed(error)
    } else {
        ConversationEvent::TurnEnded(StopReason::EndTurn)
    };
    state.cancel_requested = false;
    state.partial_tool_output.clear();
    state.partial_tool_args.clear();
    state.tool_args.clear();
    state.tool_names.clear();
    let _ = event_tx.try_send(event);
}

fn handle_event(
    event: serde_json::Value,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    outbound_tx: &smol::channel::Sender<serde_json::Value>,
    state: &mut PiState,
    launch: &ConversationLaunch,
) {
    match event.get("type").and_then(serde_json::Value::as_str) {
        Some("message_update") => {
            let Some(update) = event.get("assistantMessageEvent") else {
                return;
            };
            match update.get("type").and_then(serde_json::Value::as_str) {
                Some("text_delta") => {
                    if let Some(delta) = update.get("delta").and_then(serde_json::Value::as_str) {
                        let _ = event_tx.try_send(ConversationEvent::AgentChunk {
                            thought: false,
                            text: delta.to_string(),
                            parent_id: None,
                        });
                    }
                }
                Some("thinking_delta") => {
                    if let Some(delta) = update.get("delta").and_then(serde_json::Value::as_str) {
                        let _ = event_tx.try_send(ConversationEvent::AgentChunk {
                            thought: true,
                            text: delta.to_string(),
                            parent_id: None,
                        });
                    }
                }
                Some("toolcall_start") => {
                    let Some(id) = update.get("id").and_then(serde_json::Value::as_str) else {
                        return;
                    };
                    let name = update
                        .get("toolName")
                        .or_else(|| update.get("name"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("tool");
                    state
                        .partial_tool_args
                        .insert(id.to_string(), String::new());
                    state.tool_names.insert(id.to_string(), name.to_string());
                    emit_tool_started(event_tx, id, name, None);
                }
                Some("toolcall_delta") => {
                    let Some(id) = update.get("id").and_then(serde_json::Value::as_str) else {
                        return;
                    };
                    if let Some(delta) = update.get("delta").and_then(serde_json::Value::as_str) {
                        let raw = {
                            let buffer = state.partial_tool_args.entry(id.to_string()).or_default();
                            buffer.push_str(delta);
                            buffer.clone()
                        };
                        if let Ok(args) = serde_json::from_str::<serde_json::Value>(&raw) {
                            state.tool_args.insert(id.to_string(), args.clone());
                            let name = update
                                .get("toolName")
                                .or_else(|| update.get("name"))
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                                .or_else(|| state.tool_names.get(id).cloned())
                                .unwrap_or_else(|| "tool".to_string());
                            emit_tool_started(event_tx, id, &name, Some(&args));
                        }
                    }
                }
                Some("toolcall_end") => {
                    if let Some(call) = update.get("toolCall") {
                        apply_completed_tool_call(call, event_tx, state);
                    }
                }
                _ => {}
            }
        }
        Some("agent_start") => {
            // Smelt 发的 prompt 发出时就开了本地回合，Pi 也一直忙到 settled。两样都不是，
            // 就是 Pi 自己起的一轮：后台任务、后台子智能体的结果带 triggerTurn 时会这样。
            // 停止后 Pi 还没 settled 时 provider_busy 仍为真，那时的迟到运行不算新回合。
            if state.active_turn || state.provider_busy || prompt_admission_unresolved(state) {
                return;
            }
            state.active_turn = true;
            state.prompt_accepted = true;
            state.provider_busy = true;
            state.cancel_requested = false;
            state.last_stop_reason = None;
            state.last_error = None;
            let _ = event_tx.try_send(ConversationEvent::ProviderTurnStarted);
        }
        Some("message_start") => {
            let Some(message) = event.get("message") else {
                return;
            };
            if message.get("role").and_then(serde_json::Value::as_str) != Some("user") {
                return;
            }
            let content = message.get("content");
            let has_image = content
                .and_then(serde_json::Value::as_array)
                .is_some_and(|blocks| {
                    blocks.iter().any(|block| {
                        block.get("type").and_then(serde_json::Value::as_str) == Some("image")
                    })
                });
            let Some((steering, follow_up)) = state
                .queue
                .user_message_started(&content_text(content), has_image)
            else {
                return;
            };
            if state.pending_clear.is_none() {
                let _ = event_tx.try_send(ConversationEvent::PromptQueue {
                    steering,
                    follow_up,
                });
            }
        }
        Some("message_end") => {
            // 后台子智能体的结果、别的对话发来的消息是自定义消息（`display: false`），
            // 实时到达时只有这里能看到。后台任务的状态行由守护进程自己发
            // （`smelt_background_notify`），这里再发一次会重复。
            if let Some(message) = event.get("message")
                && message.get("role").and_then(serde_json::Value::as_str) == Some("custom")
                && let Some(note) = crate::acp_chat::task_note_from_pi_custom(message)
                && matches!(
                    note.kind,
                    crate::acp_chat::TaskNoteKind::Subagent
                        | crate::acp_chat::TaskNoteKind::PeerMessage
                )
            {
                let _ = event_tx.try_send(ConversationEvent::BackgroundNotice(note));
                return;
            }
            if let Some(message) = event.get("message")
                && message.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
            {
                state.last_stop_reason = message
                    .get("stopReason")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                state.last_error = message
                    .get("errorMessage")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                publish_usage(message, state.context_window(), event_tx);
            }
        }
        Some("tool_execution_start") => {
            let Some(id) = event.get("toolCallId").and_then(serde_json::Value::as_str) else {
                return;
            };
            let name = event
                .get("toolName")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| state.tool_names.get(id).cloned())
                .unwrap_or_else(|| "tool".to_string());
            state.tool_names.insert(id.to_string(), name.clone());
            state.partial_tool_output.remove(id);
            let args = event
                .get("args")
                .cloned()
                .or_else(|| state.tool_args.get(id).cloned());
            if let Some(args) = args.as_ref() {
                state.tool_args.insert(id.to_string(), args.clone());
            }
            emit_tool_started(event_tx, id, &name, args.as_ref());
            emit_peer_send(event_tx, id, &name, args.as_ref(), None, None);
        }
        Some("tool_execution_update") => {
            let Some(id) = event.get("toolCallId").and_then(serde_json::Value::as_str) else {
                return;
            };
            let name = state.tool_names.get(id).cloned().unwrap_or_default();
            if let Some(runs) = event
                .get("partialResult")
                .and_then(|value| value.get("details"))
                .and_then(|details| subagent_runs_from_details(&name, details))
            {
                let _ = event_tx.try_send(ConversationEvent::SubagentRuns {
                    id: id.to_string(),
                    runs,
                });
                let text = content_text(
                    event
                        .get("partialResult")
                        .and_then(|value| value.get("content")),
                );
                state.partial_tool_output.insert(id.to_string(), text);
                return;
            }
            let text = content_text(
                event
                    .get("partialResult")
                    .and_then(|value| value.get("content")),
            );
            let previous = state.partial_tool_output.entry(id.to_string()).or_default();
            if let Some(delta) = text.strip_prefix(previous.as_str())
                && !delta.is_empty()
            {
                let _ = event_tx.try_send(ConversationEvent::ToolOutputDelta {
                    id: id.to_string(),
                    delta: delta.to_string(),
                });
            }
            *previous = text;
        }
        Some("tool_execution_end") => {
            let Some(id) = event.get("toolCallId").and_then(serde_json::Value::as_str) else {
                return;
            };
            state.partial_tool_output.remove(id);
            let is_error = event
                .get("isError")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let name = event
                .get("toolName")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| state.tool_names.remove(id))
                .unwrap_or_else(|| "tool".to_string());
            let args = event
                .get("args")
                .cloned()
                .or_else(|| state.tool_args.remove(id));
            state.partial_tool_args.remove(id);
            if let Some(runs) = event
                .get("result")
                .and_then(|value| value.get("details"))
                .and_then(|details| subagent_runs_from_details(&name, details))
            {
                let _ = event_tx.try_send(ConversationEvent::SubagentRuns {
                    id: id.to_string(),
                    runs,
                });
            }
            let failure = is_error
                .then(|| content_text(event.get("result").and_then(|value| value.get("content"))));
            emit_peer_send(
                event_tx,
                id,
                &name,
                args.as_ref(),
                event.get("result").and_then(|value| value.get("details")),
                failure.as_deref(),
            );
            let output = event
                .get("result")
                .map(|result| tool_output_parts(result, &name, args.as_ref()))
                .unwrap_or_default();
            let _ = event_tx.try_send(ConversationEvent::ToolFinished {
                id: id.to_string(),
                status: if is_error {
                    ToolCallStatus::Failed
                } else {
                    ToolCallStatus::Completed
                },
                output,
            });
        }
        Some("agent_settled") => {
            let prompt_rpc_open = state.prompt_request_id.is_some()
                || state.prompt_state_request_id.is_some()
                || state.refusal_state_request_id.is_some()
                || state.pending_refusal_error.is_some();
            state.provider_busy = false;
            // 还没被 Pi 收下的那条不属于这一轮。清掉它会把随后的用户输入又变成裸 prompt。
            if state.prompt_accepted || !prompt_rpc_open {
                finish_turn(state, event_tx, None);
            }
            let _ = event_tx.try_send(ConversationEvent::ProviderIdle);
            request_session_stats(state, outbound_tx);
        }
        Some("session_info_changed") => {
            state.session_name = event
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let _ = event_tx.try_send(ConversationEvent::SessionTitle(state.session_name.clone()));
        }
        Some("thinking_level_changed") => {
            if let Some(level) = event.get("level").and_then(serde_json::Value::as_str) {
                state.thinking_level = level.to_string();
                publish_configuration(state, event_tx);
            }
        }
        Some("extension_ui_request") => {
            if !apply_codemode_widget(&event, state, event_tx) {
                handle_extension_ui(&event, event_tx, outbound_tx, state, launch)
            }
        }
        Some("extension_error") => {
            let message = event
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Pi Extension 执行失败");
            let _ = event_tx.try_send(ConversationEvent::Notice(format!(
                "Pi Extension：{message}"
            )));
        }
        Some("auto_retry_start") => {
            let number = |key: &str| event.get(key).and_then(serde_json::Value::as_u64);
            let _ = event_tx.try_send(ConversationEvent::ProviderRetry(Some(
                crate::acp_session::ProviderRetryView {
                    attempt: number("attempt").unwrap_or(1) as u32,
                    max_attempts: number("maxAttempts").unwrap_or(0) as u32,
                    delay_ms: number("delayMs").unwrap_or(0),
                    started_at_ms: crate::acp_session::unix_time_ms(),
                    error: event
                        .get("errorMessage")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                },
            )));
        }
        Some("auto_retry_end") => {
            let _ = event_tx.try_send(ConversationEvent::ProviderRetry(None));
        }
        Some("queue_update") => {
            if state.pending_clear.is_some() {
                // clear_queue 进行中：接下来会 ComposerRestore。空队列不能当成已插入。
                return;
            }
            let (steering, follow_up) = state.queue.reported(
                json_string_list(&event, "steering"),
                json_string_list(&event, "followUp"),
            );
            let _ = event_tx.try_send(ConversationEvent::PromptQueue {
                steering,
                follow_up,
            });
        }
        Some("compaction_start") => {
            state.compaction_idle_pending = false;
            let _ = event_tx.try_send(ConversationEvent::Compaction {
                running: true,
                detail: "正在压缩上下文…".to_string(),
                used: None,
                size: None,
            });
        }
        Some("compaction_end") => {
            let aborted = event
                .get("aborted")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let result = event.get("result");
            let error = event
                .get("errorMessage")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty());
            let (detail, used) = if aborted {
                ("上下文压缩已取消".to_string(), None)
            } else if let Some(error) = error {
                (format!("上下文压缩失败：{error}"), None)
            } else if let Some(result) = result {
                let before = result
                    .get("tokensBefore")
                    .and_then(serde_json::Value::as_u64);
                let after = result
                    .get("estimatedTokensAfter")
                    .and_then(serde_json::Value::as_u64);
                let detail = match (before, after) {
                    (Some(before), Some(after)) => {
                        format!("已压缩上下文：{before} → {after} tokens")
                    }
                    _ => "上下文压缩完成".to_string(),
                };
                (detail, after)
            } else {
                ("上下文压缩完成".to_string(), None)
            };
            let _ = event_tx.try_send(ConversationEvent::Compaction {
                running: false,
                detail,
                used,
                size: state.context_window(),
            });
            // 压缩本身会拒绝裸 prompt。已被收下的回合等 agent_settled。
            // 没被收下的那条不能占着 active_turn，否则压缩结束后进门仍关着，
            // 或者下一条被改写成 steer。快照还没回来时只记账，由拒绝结果一起放行。
            if !state.prompt_accepted {
                state.compaction_idle_pending = true;
                if !prompt_admission_unresolved(state) {
                    release_unaccepted_admission(state, event_tx);
                }
            }
            request_session_stats(state, outbound_tx);
        }
        _ => {}
    }
}

fn publish_usage(
    message: &serde_json::Value,
    context_window: Option<u64>,
    event_tx: &smol::channel::Sender<ConversationEvent>,
) {
    let Some(size) = context_window else { return };
    let Some(usage) = message.get("usage") else {
        return;
    };
    let cached_read = usage.get("cacheRead").and_then(serde_json::Value::as_u64);
    let _ = event_tx.try_send(ConversationEvent::Usage {
        // Provider usage is billing data. In particular, totalTokens may include
        // cache/lifetime totals and is not Pi's active context size.
        used: 0,
        size,
        cached_read,
        cost: None,
        breakdown: None,
    });
}

fn request_session_stats(
    state: &mut PiState,
    outbound_tx: &smol::channel::Sender<serde_json::Value>,
) {
    let id = state.request_id("stats");
    if outbound_tx
        .try_send(serde_json::json!({"id": id, "type": "get_session_stats"}))
        .is_ok()
    {
        state.latest_stats_request_id = Some(id);
    }
}

fn publish_session_stats(
    data: &serde_json::Value,
    fallback_window: Option<u64>,
    event_tx: &smol::channel::Sender<ConversationEvent>,
) {
    let tokens = data.get("tokens");
    let context = data.get("contextUsage");
    // Pi deliberately returns contextUsage.tokens = null immediately after
    // compaction until a later model response makes the active size reliable.
    // Session tokens.total is lifetime billing usage, never a context fallback.
    let used = context
        .and_then(|value| value.get("tokens"))
        .and_then(serde_json::Value::as_u64);
    let size = context
        .and_then(|value| value.get("contextWindow"))
        .and_then(serde_json::Value::as_u64)
        .or(fallback_window);
    let Some(size) = size else {
        return;
    };
    let cached_read = tokens
        .and_then(|value| value.get("cacheRead"))
        .and_then(serde_json::Value::as_u64);
    let cost = data
        .get("cost")
        .and_then(serde_json::Value::as_f64)
        .or_else(|| {
            data.get("cost")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value as f64)
        });
    let _ = event_tx.try_send(ConversationEvent::Usage {
        used: used.unwrap_or(0),
        size,
        cached_read,
        cost,
        breakdown: None,
    });
}

fn emit_tool_started(
    event_tx: &smol::channel::Sender<ConversationEvent>,
    id: &str,
    name: &str,
    args: Option<&serde_json::Value>,
) {
    // 保持既有生命周期顺序：先让卡片出现，再补仅供审计的原始元数据。
    let _ = event_tx.try_send(ConversationEvent::ToolStarted {
        id: id.to_string(),
        title: tool_title(name, args),
        kind: tool_kind(name),
    });
    let _ = event_tx.try_send(ConversationEvent::ToolDebug {
        id: id.to_string(),
        name: Some(name.to_string()),
        raw_input: args.cloned(),
    });
}

fn apply_completed_tool_call(
    call: &serde_json::Value,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
) {
    let Some(id) = call
        .get("id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| call.get("toolCallId").and_then(serde_json::Value::as_str))
    else {
        return;
    };
    let name = call
        .get("name")
        .or_else(|| call.get("toolName"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| state.tool_names.get(id).cloned())
        .unwrap_or_else(|| "tool".to_string());
    state.tool_names.insert(id.to_string(), name.clone());
    let args = call
        .get("arguments")
        .or_else(|| call.get("args"))
        .cloned()
        .or_else(|| {
            state
                .partial_tool_args
                .get(id)
                .and_then(|raw| serde_json::from_str(raw).ok())
        });
    state.partial_tool_args.remove(id);
    if let Some(args) = args.as_ref() {
        state.tool_args.insert(id.to_string(), args.clone());
    }
    emit_tool_started(event_tx, id, &name, args.as_ref());
}

fn json_string<'a>(args: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| args.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn tool_title(name: &str, args: Option<&serde_json::Value>) -> String {
    let Some(args) = args else {
        return name.to_string();
    };
    let path = json_string(args, &["path", "file", "filePath", "target"]);
    let command = json_string(args, &["command", "cmd"]);
    let pattern = json_string(args, &["pattern", "query", "glob"]);
    match name {
        "subagent" => crate::acp_chat::subagent_display_title("subagent", Some(args)),
        "bash" | "powershell" => command
            .map(str::to_string)
            .unwrap_or_else(|| name.to_string()),
        "grep" | "find" => match (pattern, path) {
            (Some(pattern), Some(path)) => format!("{pattern} · {path}"),
            (Some(pattern), None) => pattern.to_string(),
            (None, Some(path)) => path.to_string(),
            _ => name.to_string(),
        },
        _ => path
            .or(command)
            .or(pattern)
            .map(str::to_string)
            .unwrap_or_else(|| name.to_string()),
    }
}

fn path_from_unified_diff(diff: &str) -> Option<String> {
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            let path = rest.trim();
            if !path.is_empty() && path != "/dev/null" {
                return Some(path.to_string());
            }
        }
        if let Some(rest) = line.strip_prefix("--- a/") {
            let path = rest.trim();
            if !path.is_empty() && path != "/dev/null" {
                return Some(path.to_string());
            }
        }
    }
    None
}

fn unified_diff_to_old_new(diff: &str) -> Option<(String, String)> {
    let mut old = String::new();
    let mut new = String::new();
    let mut saw_change = false;
    for line in diff.lines() {
        if line.starts_with("@@")
            || line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("---")
            || line.starts_with("+++")
        {
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            new.push_str(rest);
            new.push('\n');
            saw_change = true;
        } else if let Some(rest) = line.strip_prefix('-') {
            old.push_str(rest);
            old.push('\n');
            saw_change = true;
        } else {
            let rest = line.strip_prefix(' ').unwrap_or(line);
            old.push_str(rest);
            old.push('\n');
            new.push_str(rest);
            new.push('\n');
        }
    }
    saw_change.then_some((old, new))
}

fn edit_args_to_texts(args: &serde_json::Value) -> Option<(String, String)> {
    if let Some(edits) = args.get("edits").and_then(serde_json::Value::as_array) {
        let mut old = String::new();
        let mut new = String::new();
        for edit in edits {
            if let Some(old_text) = json_string(edit, &["oldText"]) {
                if !old.is_empty() {
                    old.push('\n');
                }
                old.push_str(old_text);
            }
            if let Some(new_text) = json_string(edit, &["newText"]) {
                if !new.is_empty() {
                    new.push('\n');
                }
                new.push_str(new_text);
            }
        }
        return (!old.is_empty() || !new.is_empty()).then_some((old, new));
    }
    let old = json_string(args, &["oldText"])?;
    let new = json_string(args, &["newText"]).unwrap_or("");
    Some((old.to_string(), new.to_string()))
}

/// smelt 的 Pi 扩展登记的子智能体派发工具。
const PI_SUBAGENT_TOOL: &str = "subagent";

fn tool_kind(name: &str) -> ToolKind {
    match name {
        "read" => ToolKind::Read,
        "write" | "edit" => ToolKind::Edit,
        "grep" | "find" | "ls" => ToolKind::Search,
        "bash" | "powershell" => ToolKind::Execute,
        "subagent" => ToolKind::Collaborate,
        _ => ToolKind::Other,
    }
}

/// Pi `subagent` 工具结果里记下的子智能体（`details.runs`）。过程在各自的子会话里，
/// 父会话只认这份名单；别的工具没有。
/// `session_send` 发给了谁：开始时只有参数，结束后带上门卫回的标题、消息 id 或拒绝原因。
fn emit_peer_send(
    event_tx: &smol::channel::Sender<ConversationEvent>,
    id: &str,
    name: &str,
    args: Option<&serde_json::Value>,
    details: Option<&serde_json::Value>,
    failure: Option<&str>,
) {
    if let Some(send) = crate::acp_chat::peer_send_from_tool(name, args, details, failure) {
        let _ = event_tx.try_send(ConversationEvent::PeerSend {
            id: id.to_string(),
            send,
        });
    }
}

fn subagent_runs_from_details(
    name: &str,
    details: &serde_json::Value,
) -> Option<Vec<crate::acp_chat::SubagentRun>> {
    (name == PI_SUBAGENT_TOOL)
        .then(|| crate::acp_chat::subagent_runs_from_value(details.get("runs")))
}

fn content_text(content: Option<&serde_json::Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| {
            (block.get("type").and_then(serde_json::Value::as_str) == Some("text"))
                .then(|| block.get("text").and_then(serde_json::Value::as_str))
                .flatten()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 工具结果里的图片块。pi 的 `read` 读图时会同时给一段文本提示和
/// `{type:"image", data, mimeType}`，两者都要保留。
fn content_images(content: Option<&serde_json::Value>) -> Vec<crate::acp_chat::AcpImage> {
    content
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("image"))
        .filter_map(|block| {
            let data = block.get("data").and_then(serde_json::Value::as_str)?;
            let mime = block
                .get("mimeType")
                .or_else(|| block.get("mime_type"))
                .and_then(serde_json::Value::as_str)?;
            (!data.is_empty())
                .then(|| crate::acp_chat::AcpImage::new(mime.to_string(), data.to_string()))
        })
        .collect()
}

fn tool_output_parts(
    result: &serde_json::Value,
    name: &str,
    args: Option<&serde_json::Value>,
) -> Vec<ToolOutputPart> {
    let text = content_text(result.get("content"));
    let details = result.get("details");
    let mut path = args
        .and_then(|value| json_string(value, &["path", "file", "filePath"]))
        .or_else(|| {
            details
                .and_then(|value| value.get("path"))
                .and_then(serde_json::Value::as_str)
        })
        .unwrap_or("")
        .to_string();
    let patch = details
        .and_then(|value| {
            value
                .get("patch")
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.get("diff").and_then(serde_json::Value::as_str))
        })
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let mut output = Vec::new();
    let mut has_diff = false;
    if let Some(patch) = patch
        && let Some((old, new)) = unified_diff_to_old_new(patch)
    {
        if path.is_empty() {
            path = path_from_unified_diff(patch).unwrap_or_default();
        }
        output.push(ToolOutputPart::Diff {
            path,
            old_text: (!old.is_empty()).then_some(old),
            new_text: new,
        });
        has_diff = true;
    } else if name == "write" {
        if let Some(content) = args.and_then(|value| json_string(value, &["content"])) {
            output.push(ToolOutputPart::Diff {
                path,
                old_text: None,
                new_text: content.to_string(),
            });
            has_diff = true;
        }
    } else if name == "edit"
        && let Some(args) = args
        && let Some((old, new)) = edit_args_to_texts(args)
    {
        output.push(ToolOutputPart::Diff {
            path,
            old_text: (!old.is_empty()).then_some(old),
            new_text: new,
        });
        has_diff = true;
    }

    if !text.is_empty() && !(has_diff && text_looks_like_diff(&text)) {
        output.insert(0, ToolOutputPart::Text(text));
    }
    output.extend(
        content_images(result.get("content"))
            .into_iter()
            .map(ToolOutputPart::Image),
    );
    output
}

fn text_looks_like_diff(text: &str) -> bool {
    text.contains("@@")
        || text
            .lines()
            .any(|line| line.starts_with("+++ ") || line.starts_with("--- "))
}

/// 从 Pi `get_entries` 的 `leafId` 沿 parent 链取出当前分支上的消息。
/// 文件恢复用的是「文件最后一条是叶子」；这里的叶子以响应为准，两者不是同一个走法。
fn messages_from_get_entries(data: &serde_json::Value) -> Result<Vec<serde_json::Value>, String> {
    let entries = data
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Pi get_entries 响应缺少 entries".to_string())?;
    let leaf = match data.get("leafId") {
        Some(serde_json::Value::Null) => return Ok(Vec::new()),
        Some(serde_json::Value::String(id)) if !id.is_empty() => id.as_str(),
        Some(_) => return Err("Pi get_entries 响应的 leafId 无效".to_string()),
        None => return Err("Pi get_entries 响应缺少 leafId".to_string()),
    };

    let mut by_id = HashMap::with_capacity(entries.len());
    for entry in entries {
        let id = entry
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| "Pi get_entries 返回了没有有效 id 的 entry".to_string())?;
        if by_id.insert(id, entry).is_some() {
            return Err(format!("Pi get_entries 返回了重复 entry id：{id}"));
        }
    }

    let mut branch = Vec::new();
    let mut visited = HashSet::new();
    let mut cursor = leaf;
    loop {
        if !visited.insert(cursor) {
            return Err(format!("Pi 当前分支 parent 链成环：{cursor}"));
        }
        let entry = by_id
            .get(cursor)
            .copied()
            .ok_or_else(|| format!("Pi 当前分支缺少 entry：{cursor}"))?;
        branch.push(entry);
        match entry.get("parentId") {
            Some(serde_json::Value::Null) => break,
            Some(serde_json::Value::String(parent)) if !parent.is_empty() => cursor = parent,
            Some(_) => return Err(format!("Pi entry {cursor} 的 parentId 无效")),
            None => return Err(format!("Pi entry {cursor} 缺少 parentId")),
        }
    }
    branch.reverse();

    Ok(branch.into_iter().filter_map(entry_message).collect())
}

/// Pi 会话 entry 里能回放的那条消息：普通消息原样，自定义消息还原成 `role: custom`。
fn entry_message(entry: &serde_json::Value) -> Option<serde_json::Value> {
    match entry.get("type").and_then(serde_json::Value::as_str) {
        Some("message") => entry.get("message").cloned(),
        Some("custom_message") => Some(serde_json::json!({
            "role": "custom",
            "customType": entry.get("customType").cloned().unwrap_or(serde_json::Value::Null),
            "content": entry.get("content").cloned().unwrap_or_default(),
            "display": entry.get("display").cloned().unwrap_or(serde_json::Value::Bool(true)),
            "details": entry.get("details").cloned().unwrap_or_default(),
        })),
        _ => None,
    }
}

/// 不起 Pi 进程，把一份 Pi 会话文件读成对话条目：取 Pi 自己认的活动分支，按实时回放
/// 同一套规则归约。子智能体的子会话就这样给界面看。
pub fn transcript_entries_from_session_file(text: &str) -> Vec<AcpEntry> {
    let (version, entries) = crate::session_history::pi_session_entries(text);
    let messages: Vec<serde_json::Value> =
        crate::session_history::pi_active_branch(version, &entries)
            .into_iter()
            .filter_map(entry_message)
            .collect();
    let (event_tx, event_rx) = smol::channel::unbounded();
    let mut state = PiState::new();
    // 和恢复会话时同一个顺序：开始、整段回放、结束。
    let _ = event_tx.try_send(ConversationEvent::HistoryReplayStarted);
    replay_history(&messages, &event_tx, &mut state);
    drop(event_tx);
    let mut reduced = crate::acp_session::AcpSessionState::default();
    while let Ok(event) = event_rx.try_recv() {
        let _ = crate::acp_session::apply_event(&mut reduced, event);
    }
    reduced.entries().to_vec()
}

/// 全量重放 + 显式收尾。Pi 先把当前分支的原始 message entries 重放完再握手，
/// 边界在这里就确定了；不发结束信号的话，恢复后只要用户不再发消息，
/// `replaying_history` 就一直挂着。
fn replay_history(
    messages: &[serde_json::Value],
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
) {
    replay_messages(messages, event_tx, state);
    let _ = event_tx.try_send(ConversationEvent::HistoryReplayFinished);
}

fn replay_messages(
    messages: &[serde_json::Value],
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
) {
    for message in messages {
        match message.get("role").and_then(serde_json::Value::as_str) {
            Some("user") => replay_user_content(message.get("content"), event_tx),
            Some("assistant") => replay_assistant_content(message.get("content"), event_tx, state),
            Some("toolResult") => {
                let Some(id) = message
                    .get("toolCallId")
                    .and_then(serde_json::Value::as_str)
                else {
                    continue;
                };
                let result = serde_json::json!({
                    "content": message.get("content").cloned().unwrap_or_default(),
                    "details": message.get("details").cloned().unwrap_or_default(),
                });
                let name = state
                    .tool_names
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| "tool".to_string());
                if let Some(runs) = message
                    .get("details")
                    .and_then(|details| subagent_runs_from_details(&name, details))
                {
                    let _ = event_tx.try_send(ConversationEvent::SubagentRuns {
                        id: id.to_string(),
                        runs,
                    });
                }
                let args = state.tool_args.get(id).cloned();
                let failure = message
                    .get("isError")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                    .then(|| content_text(message.get("content")));
                emit_peer_send(
                    event_tx,
                    id,
                    &name,
                    args.as_ref(),
                    message.get("details"),
                    failure.as_deref(),
                );
                let _ = event_tx.try_send(ConversationEvent::ToolFinished {
                    id: id.to_string(),
                    status: if message
                        .get("isError")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    {
                        ToolCallStatus::Failed
                    } else {
                        ToolCallStatus::Completed
                    },
                    output: tool_output_parts(&result, &name, args.as_ref()),
                });
            }
            Some("custom") => {
                if let Some(note) = crate::acp_chat::task_note_from_pi_custom(message) {
                    let _ = event_tx.try_send(ConversationEvent::BackgroundNotice(note));
                    continue;
                }
                if message.get("display").and_then(serde_json::Value::as_bool) == Some(false) {
                    continue;
                }
                let text = content_text(message.get("content"));
                if !text.is_empty() {
                    let _ = event_tx.try_send(ConversationEvent::AgentChunk {
                        thought: false,
                        text,
                        parent_id: None,
                    });
                }
            }
            _ => {}
        }
    }
}

fn replay_user_content(
    content: Option<&serde_json::Value>,
    event_tx: &smol::channel::Sender<ConversationEvent>,
) {
    let Some(content) = content else { return };
    if let Some(text) = content.as_str() {
        let _ = event_tx.try_send(ConversationEvent::UserChunk(text.to_string()));
        return;
    }
    for block in content.as_array().into_iter().flatten() {
        match block.get("type").and_then(serde_json::Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(serde_json::Value::as_str) {
                    let _ = event_tx.try_send(ConversationEvent::UserChunk(text.to_string()));
                }
            }
            Some("image") => {
                if let (Some(data), Some(mime)) = (
                    block.get("data").and_then(serde_json::Value::as_str),
                    block.get("mimeType").and_then(serde_json::Value::as_str),
                ) {
                    let _ = event_tx.try_send(ConversationEvent::UserImage(
                        crate::acp_chat::AcpImage::new(mime.to_string(), data.to_string()),
                    ));
                }
            }
            _ => {}
        }
    }
}

fn replay_assistant_content(
    content: Option<&serde_json::Value>,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
) {
    for block in content
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(serde_json::Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(serde_json::Value::as_str) {
                    let _ = event_tx.try_send(ConversationEvent::AgentChunk {
                        thought: false,
                        text: text.to_string(),
                        parent_id: None,
                    });
                }
            }
            Some("thinking") => {
                if let Some(text) = block.get("thinking").and_then(serde_json::Value::as_str) {
                    let _ = event_tx.try_send(ConversationEvent::AgentChunk {
                        thought: true,
                        text: text.to_string(),
                        parent_id: None,
                    });
                }
            }
            Some("toolCall") => apply_completed_tool_call(block, event_tx, state),
            _ => {}
        }
    }
}

fn redact_runtime_payload(value: &mut serde_json::Value, path: &str, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) if text.to_ascii_lowercase().starts_with("data:image/") => {
            *value = serde_json::Value::String("[IMAGE DATA OMITTED]".to_string());
            if !paths.contains(&path.to_string()) {
                paths.push(path.to_string());
            }
        }
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                redact_runtime_payload(item, &format!("{path}[{index}]"), paths);
            }
        }
        serde_json::Value::Object(fields) => {
            let image_like = fields
                .get("type")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|kind| matches!(kind, "image" | "input_image" | "image_url"));
            for (key, child) in fields {
                let child_path = format!("{path}.{key}");
                if smelt_model::redact::is_sensitive_key(key) {
                    *child = serde_json::Value::String("[REDACTED]".to_string());
                    if !paths.contains(&child_path) {
                        paths.push(child_path);
                    }
                } else if image_like
                    && matches!(
                        smelt_model::redact::normalized_sensitive_key(key).as_str(),
                        "data" | "base64" | "bytes"
                    )
                {
                    *child = serde_json::Value::String("[IMAGE DATA OMITTED]".to_string());
                    if !paths.contains(&child_path) {
                        paths.push(child_path);
                    }
                } else {
                    redact_runtime_payload(child, &child_path, paths);
                }
            }
        }
        _ => {}
    }
}

/// 与 Pi 侧 `MAX_RETAINED_RUNTIME_DEBUG_MODEL_CALLS` 一致。每次调用都带着完整请求，
/// 只留最近几次，更早的计入 `model_calls_omitted`。
const MAX_RETAINED_RUNTIME_DEBUG_MODEL_CALLS: usize = 4;
/// 与 Pi 侧 `MAX_RETAINED_RUNTIME_DEBUG_COMPACTIONS` 一致。
const MAX_RETAINED_RUNTIME_DEBUG_COMPACTIONS: usize = 4;

fn retain_debug_tail<T>(items: &mut Vec<T>, omitted: &mut u64, max: usize) {
    let len = items.len();
    if len <= max {
        return;
    }
    let drop_count = len - max;
    items.drain(..drop_count);
    *omitted = omitted.saturating_add(drop_count as u64);
}

fn validate_runtime_debug(debug: &mut RuntimeDebug) -> bool {
    let has_system_prompt = debug
        .system_prompt
        .as_deref()
        .is_some_and(|prompt| !prompt.is_empty());
    if !has_system_prompt && debug.version != 3 {
        return false;
    }
    match (debug.version, debug.source.as_str()) {
        (1, "pi_before_agent_start") => {
            debug.model_call.is_none()
                && debug.model_calls.is_empty()
                && debug.compactions.is_empty()
        }
        (2, "pi_runtime_debug") => {
            if !debug.model_calls.is_empty() || !debug.compactions.is_empty() {
                return false;
            }
            if let Some(mut call) = debug.model_call.take() {
                if !validate_runtime_debug_model_call(&mut call) {
                    return false;
                }
                debug.model_calls.push(call);
            }
            debug.version = 3;
            true
        }
        (3, "pi_runtime_debug") => {
            if debug.model_call.is_some() {
                return false;
            }
            let mut previous_sequence = 0;
            for call in &mut debug.model_calls {
                if !validate_runtime_debug_model_call(call) || call.sequence <= previous_sequence {
                    return false;
                }
                previous_sequence = call.sequence;
            }
            let mut previous_compaction_sequence = 0;
            if !has_system_prompt && debug.model_calls.is_empty() && debug.compactions.is_empty() {
                return false;
            }
            for compaction in &mut debug.compactions {
                if compaction.sequence == 0
                    || compaction.sequence <= previous_compaction_sequence
                    || !matches!(
                        compaction.status.as_str(),
                        "started" | "completed" | "failed" | "aborted"
                    )
                    || !matches!(
                        compaction.reason.as_str(),
                        "manual" | "threshold" | "overflow"
                    )
                {
                    return false;
                }
                previous_compaction_sequence = compaction.sequence;
                if let Some(usage) = &mut compaction.usage {
                    let mut redacted_paths = Vec::new();
                    redact_runtime_payload(
                        usage,
                        &format!("$.compactions[{}].usage", compaction.sequence),
                        &mut redacted_paths,
                    );
                }
                for (index, headers) in compaction.request_headers.iter_mut().enumerate() {
                    redact_runtime_payload(
                        &mut headers.headers,
                        &format!(
                            "$.compactions[{}].requestHeaders[{index}].headers",
                            compaction.sequence
                        ),
                        &mut headers.redacted_paths,
                    );
                }
                for (index, response) in compaction.response_metadata.iter_mut().enumerate() {
                    redact_runtime_payload(
                        &mut response.headers,
                        &format!(
                            "$.compactions[{}].responseMetadata[{index}].headers",
                            compaction.sequence
                        ),
                        &mut response.redacted_paths,
                    );
                }
            }
            retain_debug_tail(
                &mut debug.model_calls,
                &mut debug.model_calls_omitted,
                MAX_RETAINED_RUNTIME_DEBUG_MODEL_CALLS,
            );
            retain_debug_tail(
                &mut debug.compactions,
                &mut debug.compactions_omitted,
                MAX_RETAINED_RUNTIME_DEBUG_COMPACTIONS,
            );
            true
        }
        _ => false,
    }
}

fn validate_runtime_debug_model_call(call: &mut RuntimeDebugModelCall) -> bool {
    if call.sequence == 0
        || !matches!(
            call.source.as_str(),
            "pi_context_with_system" | "pi_before_provider_request"
        )
        || call
            .payload_source
            .as_deref()
            .is_some_and(|source| source != "pi_before_provider_request")
    {
        return false;
    }
    redact_runtime_payload(
        &mut call.pi_context,
        "$.piContext",
        &mut call.pi_context_redacted_paths,
    );
    for (index, headers) in call.request_headers.iter_mut().enumerate() {
        redact_runtime_payload(
            &mut headers.headers,
            &format!("$.requestHeaders[{index}].headers"),
            &mut headers.redacted_paths,
        );
    }
    redact_runtime_payload(&mut call.payload, "$.payload", &mut call.redacted_paths);
    for (index, metadata) in call.response_metadata.iter_mut().enumerate() {
        redact_runtime_payload(
            &mut metadata.headers,
            &format!("$.responseMetadata[{index}].headers"),
            &mut metadata.redacted_paths,
        );
    }
    if let Some(response) = &mut call.response {
        let mut paths = call.response_redacted_paths.take().unwrap_or_default();
        redact_runtime_payload(response, "$.response", &mut paths);
        call.response_redacted_paths = Some(paths);
    }
    true
}

fn publish_extension_widget(
    request: &serde_json::Value,
    event_tx: &smol::channel::Sender<ConversationEvent>,
) {
    let Some(key) = request.get("widgetKey").and_then(serde_json::Value::as_str) else {
        return;
    };
    let Some(line) = request
        .get("widgetLines")
        .and_then(serde_json::Value::as_array)
        .and_then(|lines| lines.first())
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };
    match key {
        SMELT_CONTEXT_USAGE_WIDGET => {
            let Ok(breakdown) = serde_json::from_str::<ContextUsageBreakdown>(line) else {
                return;
            };
            let _ = event_tx.try_send(ConversationEvent::Usage {
                used: 0,
                size: 0,
                cached_read: None,
                cost: None,
                breakdown: Some(breakdown),
            });
        }
        SMELT_RUNTIME_DEBUG_WIDGET => {
            let Ok(mut debug) = serde_json::from_str::<RuntimeDebug>(line) else {
                return;
            };
            if !validate_runtime_debug(&mut debug) {
                return;
            }
            let _ = event_tx.try_send(ConversationEvent::RuntimeDebug(Box::new(debug)));
        }
        SMELT_BACKGROUND_TASK_WIDGET => {
            let Some(tasks) = crate::acp_session::parse_background_task_widget(line) else {
                return;
            };
            let _ = event_tx.try_send(ConversationEvent::BackgroundTasks(tasks));
        }
        _ => {}
    }
}

/// 扩展用 widget 回报真实活动工具。菜单勾选只跟这个走。
fn apply_codemode_widget(
    request: &serde_json::Value,
    state: &mut PiState,
    event_tx: &smol::channel::Sender<ConversationEvent>,
) -> bool {
    if request.get("method").and_then(serde_json::Value::as_str) != Some("setWidget") {
        return false;
    }
    if request.get("widgetKey").and_then(serde_json::Value::as_str) != Some(SMELT_CODEMODE_WIDGET) {
        return false;
    }
    let Some(enabled) = request
        .get("widgetLines")
        .and_then(serde_json::Value::as_array)
        .and_then(|lines| lines.first())
        .and_then(serde_json::Value::as_str)
        .and_then(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .and_then(|payload| payload.get("enabled").and_then(serde_json::Value::as_bool))
    else {
        return true;
    };
    if state.codemode_enabled != enabled {
        state.codemode_enabled = enabled;
        publish_configuration(state, event_tx);
    }
    true
}

fn handle_extension_ui(
    request: &serde_json::Value,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    outbound_tx: &smol::channel::Sender<serde_json::Value>,
    state: &PiState,
    launch: &ConversationLaunch,
) {
    let Some(id) = request.get("id").and_then(serde_json::Value::as_str) else {
        return;
    };
    let Some(method) = request.get("method").and_then(serde_json::Value::as_str) else {
        return;
    };
    if method == "confirm"
        && request.get("title").and_then(serde_json::Value::as_str) == Some(SMELT_PERMISSION_TITLE)
    {
        handle_permission_request(request, id, event_tx, outbound_tx, state, launch);
        return;
    }
    match method {
        "select" | "confirm" | "input" | "editor" => {
            handle_generic_elicitation(request, id, method, event_tx, outbound_tx, state)
        }
        "notify" => {
            // 扩展发给用户看的通知，走对话里的提示。Pi 的通知会带上连接错误（如 MCP
            // 服务器连不上时的 HTTP 错误体），先打码。
            if let Some(message) = request.get("message").and_then(serde_json::Value::as_str) {
                let _ = event_tx.try_send(ConversationEvent::Notice(
                    smelt_model::redact::redact_secrets(message),
                ));
            }
        }
        "setStatus" => {
            if let Some(message) = request
                .get("statusText")
                .and_then(serde_json::Value::as_str)
            {
                let _ = event_tx.try_send(ConversationEvent::Status(message.to_string()));
            }
        }
        "setWidget" => publish_extension_widget(request, event_tx),
        "setTitle" => {
            if let Some(title) = request.get("title").and_then(serde_json::Value::as_str) {
                let _ = event_tx.try_send(ConversationEvent::SessionTitle(Some(title.to_string())));
            }
        }
        _ => {}
    }
}

fn handle_permission_request(
    request: &serde_json::Value,
    request_id: &str,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    outbound_tx: &smol::channel::Sender<serde_json::Value>,
    state: &PiState,
    launch: &ConversationLaunch,
) {
    let Some(payload) = request
        .get("message")
        .and_then(serde_json::Value::as_str)
        .and_then(|message| serde_json::from_str::<serde_json::Value>(message).ok())
    else {
        send_extension_cancel(outbound_tx, request_id);
        return;
    };
    if payload.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        send_extension_cancel(outbound_tx, request_id);
        return;
    }
    let Some(tool_call_id) = payload
        .get("toolCallId")
        .and_then(serde_json::Value::as_str)
    else {
        send_extension_cancel(outbound_tx, request_id);
        return;
    };
    let tool_name = payload
        .get("toolName")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("tool")
        .to_string();
    let input = payload.get("input").cloned().unwrap_or_default();
    if state.permission_mode == "bypassPermissions" {
        let _ = outbound_tx.try_send(serde_json::json!({
            "type": "extension_ui_response",
            "id": request_id,
            "confirmed": true,
        }));
        return;
    }

    let response_id = request_id.to_string();
    let response_tx = outbound_tx.clone();
    let responder = PermissionResponder::external(move |option| {
        let confirmed = option == "allow_once";
        let _ = response_tx.try_send(serde_json::json!({
            "type": "extension_ui_response",
            "id": response_id,
            "confirmed": confirmed,
        }));
    });
    let question = permission_question(&tool_name, &input);
    let details = permission_details(&tool_name, &input, launch.cwd.as_deref());
    let _ = event_tx.try_send(ConversationEvent::Permission {
        question,
        tool_call_id: ToolCallId::new(tool_call_id.to_string()),
        pub_options: vec![
            PermissionOptionView {
                option_id: "allow_once".to_string(),
                name: "允许一次".to_string(),
                kind: PermissionOptionKindView::AllowOnce,
            },
            PermissionOptionView {
                option_id: "reject_once".to_string(),
                name: "拒绝".to_string(),
                kind: PermissionOptionKindView::RejectOnce,
            },
        ],
        responder,
        details,
        raw_request_line: None,
    });
}

fn permission_question(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "bash" | "powershell" => input
            .get("command")
            .and_then(serde_json::Value::as_str)
            .map(|command| format!("允许执行：{command}"))
            .unwrap_or_else(|| "允许执行命令？".to_string()),
        "write" => "允许写入文件？".to_string(),
        "edit" => "允许修改文件？".to_string(),
        _ => format!("允许 Pi 调用工具 `{tool_name}`？"),
    }
}

fn permission_details(
    tool_name: &str,
    input: &serde_json::Value,
    cwd: Option<&str>,
) -> ApprovalDetailsView {
    match tool_name {
        "bash" | "powershell" => ApprovalDetailsView::Command {
            command: input
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            cwd: cwd.map(str::to_string),
            reason: None,
        },
        "write" | "edit" => ApprovalDetailsView::FileChange {
            reason: None,
            grant_root: input
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(|path| absolute_tool_path(path, cwd)),
        },
        _ => ApprovalDetailsView::Permissions {
            summary: format!("Pi Tool: {tool_name}"),
        },
    }
}

fn absolute_tool_path(path: &str, cwd: Option<&str>) -> String {
    let path = std::path::Path::new(path);
    if path.is_absolute() {
        return path.to_string_lossy().into_owned();
    }
    cwd.map(|cwd| std::path::Path::new(cwd).join(path))
        .unwrap_or_else(|| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// 与 `packages/pi-agent/src/elicitation.ts` 的 `MULTI_SELECT_TITLE_MARK` 同一份。
const MULTI_SELECT_TITLE_MARK: &str = "\u{200B}\u{200C}\u{200B}";

fn strip_multi_select_title(title: &str) -> (bool, String) {
    title
        .strip_prefix(MULTI_SELECT_TITLE_MARK)
        .map(|rest| (true, rest.to_string()))
        .unwrap_or_else(|| (false, title.to_string()))
}

fn question_text(record: &serde_json::Value) -> Option<&str> {
    ["question", "header", "prompt"]
        .iter()
        .find_map(|key| record.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn is_multi_select_flag(record: &serde_json::Value) -> bool {
    record
        .get("multiSelect")
        .or_else(|| record.get("multi_select"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// 在飞的题目工具参数里找与 select 标题同题面的那一条。题目工具不限注册者
/// （宿主四个名字、用户自装的同类扩展都算），匹配只看参数形状与题面相等。
/// 返回的记录可直接用 [`is_multi_select_flag`] 判多选。
fn matching_question<'a>(state: &'a PiState, title: &str) -> Option<&'a serde_json::Value> {
    state.tool_args.values().find_map(|args| {
        if let Some(questions) = args.get("questions").and_then(serde_json::Value::as_array) {
            return questions
                .iter()
                .find(|question| question_text(question) == Some(title));
        }
        (question_text(args) == Some(title)).then_some(args)
    })
}

fn handle_generic_elicitation(
    request: &serde_json::Value,
    request_id: &str,
    method: &str,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    outbound_tx: &smol::channel::Sender<serde_json::Value>,
    state: &PiState,
) {
    let raw_title = request
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Pi 需要你的输入");
    let (marked_multi, title) = strip_multi_select_title(raw_title);
    let (key, field) = match method {
        "select" => {
            let options: Vec<ElicitOption> = request
                .get("options")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(|option| ElicitOption {
                    value: ElicitationContentValue::String(option.to_string()),
                    label: option.to_string(),
                })
                .collect();
            if options.is_empty() {
                send_extension_cancel(outbound_tx, request_id);
                return;
            }
            // 源头是题目工具：多选按其 multiSelect 标记画勾选卡，并且一律允许
            // 用户在选项之外自己写答案——题目的合同是答案来自人类，选项只是
            // 便利不是约束。其他扩展的 select 不受影响。
            let matched_question = matching_question(state, &title);
            let kind = if marked_multi || matched_question.is_some_and(is_multi_select_flag) {
                ElicitFieldKind::MultiSelect(options)
            } else {
                ElicitFieldKind::Select(options)
            };
            (
                "value",
                ElicitField {
                    key: "value".to_string(),
                    title: title.clone(),
                    required: true,
                    allow_custom_input: marked_multi || matched_question.is_some(),
                    kind,
                },
            )
        }
        "confirm" => (
            "confirmed",
            ElicitField {
                key: "confirmed".to_string(),
                title: request
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&title)
                    .to_string(),
                required: true,
                allow_custom_input: false,
                kind: ElicitFieldKind::Select(vec![
                    ElicitOption {
                        value: ElicitationContentValue::Boolean(true),
                        label: "是".to_string(),
                    },
                    ElicitOption {
                        value: ElicitationContentValue::Boolean(false),
                        label: "否".to_string(),
                    },
                ]),
            },
        ),
        "input" | "editor" => (
            "value",
            ElicitField {
                key: "value".to_string(),
                title: title.clone(),
                required: true,
                allow_custom_input: false,
                kind: ElicitFieldKind::Text { secret: false },
            },
        ),
        _ => return,
    };
    let response_id = request_id.to_string();
    let response_method = method.to_string();
    let response_key = key.to_string();
    let response_tx = outbound_tx.clone();
    let responder = ElicitationResponder::external(move |content| {
        let Some(mut content) = content else {
            send_extension_cancel(&response_tx, &response_id);
            return;
        };
        let Some(value) = content.remove(&response_key) else {
            send_extension_cancel(&response_tx, &response_id);
            return;
        };
        let response = match (response_method.as_str(), value) {
            ("confirm", ElicitationContentValue::Boolean(confirmed)) => serde_json::json!({
                "type": "extension_ui_response", "id": response_id, "confirmed": confirmed,
            }),
            (_, ElicitationContentValue::String(value)) => serde_json::json!({
                "type": "extension_ui_response", "id": response_id, "value": value,
            }),
            (_, ElicitationContentValue::StringArray(values)) => serde_json::json!({
                "type": "extension_ui_response",
                "id": response_id,
                "value": serde_json::to_string(&values).unwrap_or_else(|_| "[]".to_string()),
            }),
            _ => serde_json::json!({
                "type": "extension_ui_response", "id": response_id, "cancelled": true,
            }),
        };
        let _ = response_tx.try_send(response);
    });
    let _ = event_tx.try_send(ConversationEvent::Elicitation {
        message: title,
        fields: vec![field],
        responder,
        raw_request_line: None,
    });
}

fn send_extension_cancel(outbound_tx: &smol::channel::Sender<serde_json::Value>, request_id: &str) {
    let _ = outbound_tx.try_send(serde_json::json!({
        "type": "extension_ui_response",
        "id": request_id,
        "cancelled": true,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_launch(command: &str) -> ConversationLaunch {
        ConversationLaunch {
            launch: ConversationLaunchSpec::from_command(command),
            ephemeral_env: BTreeMap::new(),
            cwd: Some("/workspace".to_string()),
            sid: "acp-pi-test".to_string(),
            agent_token: String::new(),
            resume_session_id: None,
            fork_session_id: None,
            fork_cut: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn pi_process_guard_reaps_the_direct_child_before_returning() {
        use std::os::unix::process::CommandExt as _;

        let mut command = std::process::Command::new("sh");
        command
            .args(["-c", "sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let child = async_process::Command::from(command)
            .spawn()
            .expect("spawn guarded process");
        let pid = child.id() as i32;
        let mut guard = PiProcessGuard::new(child);
        guard.kill_and_reap();

        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "guard 返回前必须已回收直属子进程"
        );
    }

    /// Pi 的 stdio MCP 服务器不在它的进程组里，只有 Pi 自己的退出流程会关掉它们。
    /// 关会话必须先让它收尾，不能上来就 SIGKILL。
    #[cfg(unix)]
    #[test]
    fn pi_process_guard_lets_pi_clean_up_before_killing() {
        use std::os::unix::process::CommandExt as _;

        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("cleaned");
        let mut command = std::process::Command::new("sh");
        command
            .args([
                "-c",
                "trap 'echo done > \"$MARKER\"; exit 0' TERM; while :; do sleep 1; done",
            ])
            .env("MARKER", &marker)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let child = async_process::Command::from(command)
            .spawn()
            .expect("spawn guarded process");
        let pid = child.id() as i32;
        // 等 trap 装好。
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut guard = PiProcessGuard::new(child);
        guard.kill_and_reap();

        assert!(marker.exists(), "退出前的收尾没有跑");
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }

    #[cfg(unix)]
    #[test]
    fn pi_process_guard_kills_a_process_that_ignores_term() {
        use std::os::unix::process::CommandExt as _;

        let mut command = std::process::Command::new("sh");
        command
            .args(["-c", "trap '' TERM; while :; do sleep 1; done"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let child = async_process::Command::from(command)
            .spawn()
            .expect("spawn guarded process");
        let pid = child.id() as i32;
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut guard = PiProcessGuard::new(child);
        guard.kill_and_reap();

        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "到时限必须强杀");
    }

    /// 被拉起来跑探针的那个测试进程认这个变量；平时跑 `cargo test` 时探针直接返回。
    #[cfg(unix)]
    const REAP_PROBE_ENV: &str = "SMELT_TEST_REAP_PROBE";

    /// 场景：没有任何线程能接住 `SIGCHLD`（守护进程 fork 后只剩调用线程时就是这样），
    /// `Child::status()` 会一直停着。`PiProcessGuard` 必须照样把子进程收掉。
    ///
    /// 不能在测试进程里直接 `fork`：libtest 是多线程的，fork 出来的子进程只剩一个线程，
    /// 别的测试线程那一刻若拿着 malloc 之类的锁，子进程一分配内存、一 spawn 就永远卡住——
    /// 这是它在并行跑时偶发超时的原因。改成重新 exec 一个干净的测试进程，只跑下面的探针。
    /// exec 前在子进程里挡住 `SIGCHLD`：信号掩码跨 exec 保留，新进程从第一个线程起就挡着，
    /// 之后建的线程（包括 async-process 的回收线程）都继承，比原来只挡一个线程更严格。
    #[cfg(unix)]
    #[test]
    fn pi_process_guard_reaps_while_sigchld_is_blocked() {
        use std::os::unix::process::CommandExt as _;

        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "pi_rpc::tests::reap_probe_with_sigchld_blocked",
                "--ignored",
                "--test-threads=1",
                "--quiet",
            ])
            .env(REAP_PROBE_ENV, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // SAFETY: 只调用异步信号安全的 sigemptyset / sigaddset / sigprocmask。
        unsafe {
            command.pre_exec(|| {
                let mut blocked = std::mem::zeroed();
                if libc::sigemptyset(&mut blocked) != 0
                    || libc::sigaddset(&mut blocked, libc::SIGCHLD) != 0
                    || libc::sigprocmask(libc::SIG_BLOCK, &blocked, std::ptr::null_mut()) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut probe = command.spawn().expect("拉起探针进程失败");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let status = loop {
            if let Some(status) = probe.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = probe.kill();
                let _ = probe.wait();
                panic!("SIGCHLD 被挡住时收尸没有返回");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let read_all = |pipe: Option<&mut dyn std::io::Read>| {
            let mut text = String::new();
            if let Some(pipe) = pipe {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        };
        let stdout = read_all(
            probe
                .stdout
                .as_mut()
                .map(|pipe| pipe as &mut dyn std::io::Read),
        );
        let stderr = read_all(
            probe
                .stderr
                .as_mut()
                .map(|pipe| pipe as &mut dyn std::io::Read),
        );
        assert!(status.success(), "探针失败：{status}\n{stdout}\n{stderr}");
        assert!(
            stdout.contains("1 passed"),
            "探针必须真的跑过（改名后 --exact 匹配不到也会返回成功）：{stdout}"
        );
    }

    /// 只在 `pi_process_guard_reaps_while_sigchld_is_blocked` 拉起的独立进程里有意义。
    #[cfg(unix)]
    #[test]
    #[ignore = "由 pi_process_guard_reaps_while_sigchld_is_blocked 在独立进程里启动"]
    fn reap_probe_with_sigchld_blocked() {
        if std::env::var_os(REAP_PROBE_ENV).is_none() {
            return;
        }
        // 先确认场景成立：掩码确实跨 exec 带进来了，本线程（继承自主线程）挡着 SIGCHLD。
        let inherited = unsafe {
            let mut current = std::mem::zeroed();
            libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &mut current) == 0
                && libc::sigismember(&current, libc::SIGCHLD) == 1
        };
        assert!(inherited, "探针进程没有继承到挡住 SIGCHLD 的掩码");
        assert_eq!(reap_child_with_sigchld_blocked(), 0);
    }

    #[cfg(unix)]
    fn reap_child_with_sigchld_blocked() -> i32 {
        use std::os::unix::process::CommandExt as _;

        unsafe {
            let mut blocked = std::mem::zeroed();
            if libc::sigemptyset(&mut blocked) != 0
                || libc::sigaddset(&mut blocked, libc::SIGCHLD) != 0
                || libc::sigprocmask(libc::SIG_BLOCK, &blocked, std::ptr::null_mut()) != 0
            {
                return 2;
            }
        }
        let mut command = std::process::Command::new("sh");
        command
            .args(["-c", "sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let Ok(child) = async_process::Command::from(command).spawn() else {
            return 3;
        };
        let pid = child.id() as i32;
        let mut guard = PiProcessGuard::new(child);
        guard.kill_and_reap();
        if unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            0
        } else {
            1
        }
    }

    #[test]
    fn only_exact_logical_command_selects_native_pi_rpc() {
        assert!(is_smelt_pi_launch(&ConversationLaunchSpec::from_command(
            "smelt-pi-agent --offline"
        )));
        assert!(is_smelt_pi_launch(&ConversationLaunchSpec::from_command(
            "PI_OFFLINE=1 smelt-pi-agent"
        )));
        assert!(!is_smelt_pi_launch(&ConversationLaunchSpec::from_command(
            "pi --mode rpc"
        )));
        assert!(!is_smelt_pi_launch(&ConversationLaunchSpec::from_command(
            "my-smelt-pi-agent"
        )));
    }

    #[test]
    fn automation_launch_adds_no_session_without_touching_other_commands() {
        let launch = with_unpersisted_pi_session(ConversationLaunchSpec::from_command(
            "PI_OFFLINE=1 smelt-pi-agent --offline",
        ));
        assert_eq!(
            launch.command,
            "PI_OFFLINE=1 smelt-pi-agent --offline --no-session"
        );
        let (_, trailing) = parse_logical_launch(&launch.command).unwrap();
        assert_eq!(trailing, ["--offline", "--no-session"].map(str::to_string));

        let already = with_unpersisted_pi_session(ConversationLaunchSpec::from_command(
            "smelt-pi-agent --no-session",
        ));
        assert_eq!(already.command, "smelt-pi-agent --no-session");

        let resume = with_unpersisted_pi_session(ConversationLaunchSpec::from_command(
            "smelt-pi-agent --session abc",
        ));
        assert_eq!(resume.command, "smelt-pi-agent --session abc");

        let forked = with_unpersisted_pi_session(ConversationLaunchSpec::from_command(
            "smelt-pi-agent --fork abc",
        ));
        assert_eq!(forked.command, "smelt-pi-agent --fork abc");

        let other =
            with_unpersisted_pi_session(ConversationLaunchSpec::from_command("pi --mode rpc"));
        assert_eq!(other.command, "pi --mode rpc");
    }

    #[test]
    fn session_cli_args_prefer_native_fork_over_resume() {
        let mut trailing = vec!["--offline".to_string()];
        apply_pi_session_cli_args(&mut trailing, Some("old"), Some("src-1"));
        assert_eq!(
            trailing,
            ["--offline", "--fork", "src-1"].map(str::to_string)
        );

        let mut resume_only = Vec::new();
        apply_pi_session_cli_args(&mut resume_only, Some("old"), None);
        assert_eq!(resume_only, ["--session", "old"].map(str::to_string));

        let mut already = vec!["--fork".to_string(), "keep".to_string()];
        apply_pi_session_cli_args(&mut already, Some("old"), Some("src-1"));
        assert_eq!(already, ["--fork", "keep"].map(str::to_string));
    }

    #[test]
    fn logical_launch_preserves_env_and_native_pi_flags() {
        let (env, args) =
            parse_logical_launch("PI_OFFLINE=1 smelt-pi-agent --thinking high --no-approve")
                .unwrap();
        assert_eq!(env.get("PI_OFFLINE"), Some(&"1".to_string()));
        assert_eq!(
            args,
            ["--thinking", "high", "--no-approve"].map(str::to_string)
        );
    }

    #[test]
    fn product_agent_instructions_are_not_treated_as_a_path() {
        assert_eq!(
            runtime_env_value(
                crate::agent_kind::SMELT_AGENT_INSTRUCTIONS_ENV,
                "~/不是路径；请原样遵守"
            ),
            "~/不是路径；请原样遵守"
        );
    }

    #[test]
    fn logical_launch_rejects_transport_override() {
        let error = parse_logical_launch("smelt-pi-agent --mode text").unwrap_err();
        assert!(error.contains("传输固定为 Pi RPC"));
        assert!(parse_logical_launch("smelt-pi-agent --mode rpc").is_ok());
    }

    #[test]
    fn native_thinking_change_refreshes_smelt_configuration() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.thinking_levels = vec!["medium".to_string(), "high".to_string()];

        handle_event(
            serde_json::json!({"type": "thinking_level_changed", "level": "high"}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );

        assert_eq!(state.thinking_level, "high");
        let ConversationEvent::ConfigOptions(options) = event_rx.try_recv().unwrap() else {
            panic!("应刷新 Smelt 会话配置");
        };
        assert!(
            options.iter().any(|option| {
                option.config_id == "thought_level" && option.current_name == "高"
            })
        );
        assert!(options.iter().all(|option| option.config_id != "codemode"));
    }

    fn with_codemode_command(state: &mut PiState) {
        state
            .available_commands
            .push((SMELT_CODEMODE_COMMAND.to_string(), "toggle".to_string()));
    }

    fn codemode_option(options: &[SessionConfigState]) -> &SessionConfigState {
        options
            .iter()
            .find(|option| option.config_id == "codemode")
            .expect("脚本工具应单独成行")
    }

    #[test]
    fn codemode_row_is_omitted_until_the_command_exists() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.thinking_levels = vec!["medium".to_string()];
        publish_configuration(&state, &event_tx);
        let ConversationEvent::ConfigOptions(options) = event_rx.try_recv().unwrap() else {
            panic!("应发布会话配置");
        };
        assert!(options.iter().all(|option| option.config_id != "codemode"));

        with_codemode_command(&mut state);
        publish_configuration(&state, &event_tx);
        let ConversationEvent::ConfigOptions(options) = event_rx.try_recv().unwrap() else {
            panic!("命令出现后应重新发布");
        };
        let codemode = codemode_option(&options);
        assert_eq!(codemode.name, "脚本工具");
        assert_eq!(codemode.current_name, "关");
        assert_eq!(
            codemode.options,
            vec![
                ("off".to_string(), "关".to_string()),
                ("on".to_string(), "开".to_string())
            ]
        );
        assert_eq!(
            publishable_commands(&state.available_commands),
            Vec::<(String, String)>::new()
        );
    }

    #[test]
    fn codemode_widget_updates_the_checkmark_without_guessing() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.thinking_levels = vec!["medium".to_string()];
        with_codemode_command(&mut state);

        handle_event(
            serde_json::json!({
                "type": "extension_ui_request",
                "id": "widget-1",
                "method": "setWidget",
                "widgetKey": SMELT_CODEMODE_WIDGET,
                "widgetLines": ["{\"enabled\":true}"]
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );

        assert!(state.codemode_enabled);
        let ConversationEvent::ConfigOptions(options) = event_rx.try_recv().unwrap() else {
            panic!("回报开之后应刷新勾选");
        };
        assert_eq!(codemode_option(&options).current_name, "开");
    }

    #[test]
    fn codemode_toggle_sends_a_handled_command_not_a_user_turn() {
        let (event_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        with_codemode_command(&mut state);
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::SetConfigOption {
                config_id: "codemode".to_string(),
                value: ConfigValue::Select("on".to_string()),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        let request = rpc_lines(writer).pop().unwrap();
        assert_eq!(request["type"], "prompt");
        assert_eq!(request["message"], "/smelt-codemode on");
        assert!(!state.active_turn);
        assert!(state.prompt_request_id.is_none());
        assert!(!state.codemode_enabled);
        assert_eq!(in_flight.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn missing_codemode_command_does_not_start_a_prompt() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::SetConfigOption {
                config_id: "codemode".to_string(),
                value: ConfigValue::Select("on".to_string()),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        assert!(rpc_lines(writer).is_empty());
        assert!(!state.active_turn);
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);
        let ConversationEvent::Status(status) = event_rx.try_recv().unwrap() else {
            panic!("缺命令时应给出不支持状态");
        };
        assert!(status.starts_with("更新会话配置失败"));
        assert!(status.contains("没有脚本工具"));
    }

    #[test]
    fn codemode_response_without_widget_confirmation_is_a_failure() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.pending_configs.insert(
            "smelt-config-codemode-1".into(),
            PendingConfig::Codemode("on".into()),
        );
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_response(
            serde_json::json!({
                "id": "smelt-config-codemode-1",
                "type": "response",
                "command": "prompt",
                "success": true,
                "data": {"disposition": "handled"}
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        assert!(!state.codemode_enabled);
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);
        let ConversationEvent::Status(status) = event_rx.try_recv().unwrap() else {
            panic!("没有扩展回报时不能把勾选改成开");
        };
        assert!(status.contains("没有在下一轮生效"));
    }

    #[test]
    fn confirmed_codemode_response_keeps_the_checkmark() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.codemode_enabled = true;
        state.thinking_levels = vec!["medium".to_string()];
        with_codemode_command(&mut state);
        state.pending_configs.insert(
            "smelt-config-codemode-2".into(),
            PendingConfig::Codemode("on".into()),
        );
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_response(
            serde_json::json!({
                "id": "smelt-config-codemode-2",
                "type": "response",
                "command": "prompt",
                "success": true,
                "data": {"disposition": "handled"}
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        assert!(state.codemode_enabled);
        assert!(event_rx.try_recv().is_ok_and(|event| {
            matches!(event, ConversationEvent::ConfigOptions(options)
                if codemode_option(&options).current_name == "开")
        }));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn steer_injects_into_the_running_turn_without_starting_a_new_one() {
        let (event_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        state.prompt_request_id = Some("smelt-prompt-1".to_string());
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::Steer {
                text: "换个方向".to_string(),
                images: Vec::new(),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        let line = String::from_utf8(writer.into_inner()).unwrap();
        let request: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(request["type"], "steer");
        assert_eq!(request["message"], "换个方向");
        // 回合归属不能被插入改写，否则 prompt 的 response 会认不出自己。
        assert_eq!(state.prompt_request_id.as_deref(), Some("smelt-prompt-1"));
        assert!(state.active_turn);
        assert_eq!(state.steer_request_ids.len(), 1);
    }

    /// daemon 判定「回合在跑」到命令抵达驱动之间，回合可能刚好结束。此时
    /// steer 会被 Pi 收进队列直到下一轮，用户等不到回复；降级成 prompt 才不丢。
    #[test]
    fn steer_after_the_turn_ended_falls_back_to_a_normal_prompt() {
        let (event_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = false;
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::Steer {
                text: "补一句".to_string(),
                images: Vec::new(),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        let line = String::from_utf8(writer.into_inner()).unwrap();
        let request: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(request["type"], "prompt");
        assert_eq!(request["message"], "补一句");
        assert!(state.active_turn);
        assert!(state.steer_request_ids.is_empty());
    }

    #[test]
    fn prompt_uses_native_pi_image_shape() {
        let (event_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::Prompt {
                text: "describe".to_string(),
                images: vec![crate::acp_chat::AcpImage::new(
                    "image/png".to_string(),
                    "aGVsbG8=".to_string(),
                )],
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        let line = String::from_utf8(writer.into_inner()).unwrap();
        let request: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(request["type"], "prompt");
        assert_eq!(request["message"], "describe");
        assert_eq!(request["images"][0]["type"], "image");
        assert_eq!(request["images"][0]["mimeType"], "image/png");
        assert_eq!(request["images"][0]["data"], "aGVsbG8=");
        assert!(state.active_turn);
    }

    /// 重放必须以显式的结束信号收尾。没有它时，`replaying_history` 只能等「下一条
    /// prompt」才复位，恢复后就闲置的会话会永远挂在「重放中」。
    #[test]
    fn history_replay_ends_with_an_explicit_finished_event() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();

        replay_history(
            &[serde_json::json!({"role": "user", "content": "旧问题"})],
            &event_tx,
            &mut state,
        );

        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::UserChunk(text)) if text == "旧问题"
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::HistoryReplayFinished)
        ));
        assert!(event_rx.try_recv().is_err());
    }

    /// 子会话文件不起 Pi 进程就能读成对话条目：只取活动分支，自定义消息照常还原。
    #[test]
    fn a_session_file_reads_as_conversation_entries_without_pi() {
        let lines = [
            serde_json::json!({"type": "session", "version": 3, "id": "sa-aaaaaaaaaaaa", "cwd": "/repo"}),
            serde_json::json!({"type": "message", "id": "u1", "parentId": null,
                "message": {"role": "user", "content": [{"type": "text", "text": "查登录"}]}}),
            serde_json::json!({"type": "message", "id": "a-old", "parentId": "u1",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "旧分支"}]}}),
            serde_json::json!({"type": "message", "id": "a1", "parentId": "u1",
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": "先看看"},
                {"type": "toolCall", "id": "t1", "name": "read", "arguments": {"path": "src/auth.rs"}}
            ]}}),
            serde_json::json!({"type": "message", "id": "r1", "parentId": "a1",
                "message": {"role": "toolResult", "toolCallId": "t1", "toolName": "read",
                    "content": [{"type": "text", "text": "fn login() {}"}]}}),
            serde_json::json!({"type": "message", "id": "a2", "parentId": "r1",
                "message": {"role": "assistant", "content": [{"type": "text", "text": "登录在 src/auth.rs"}]}}),
        ];
        let text = lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        let entries = transcript_entries_from_session_file(&text);
        let texts: Vec<String> = entries
            .iter()
            .filter_map(|entry| match entry {
                AcpEntry::User(text) => Some(format!("user:{text}")),
                AcpEntry::Assistant {
                    text,
                    thought: false,
                } => Some(format!("assistant:{text}")),
                AcpEntry::ToolCall { kind, .. } => Some(format!("tool:{kind:?}")),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            [
                "user:查登录",
                "assistant:先看看",
                "tool:Read",
                "assistant:登录在 src/auth.rs"
            ],
            "旧分支不算，工具调用和结果对上"
        );
    }

    #[test]
    fn replayed_subagent_tool_result_restores_the_dispatched_runs() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        let messages = [
            serde_json::json!({
                "role": "assistant",
                "content": [{
                    "type": "toolCall",
                    "id": "parallel-call",
                    "name": "subagent",
                    "arguments": {"tasks": [{"task": "a"}, {"task": "b"}]}
                }]
            }),
            serde_json::json!({
                "role": "toolResult",
                "toolCallId": "parallel-call",
                "toolName": "subagent",
                "content": [{"type": "text", "text": "Parallel: 1/2 succeeded"}],
                "details": {
                    "mode": "parallel",
                    "background": false,
                    "runs": [
                        {"id": "sa-a", "title": "审计 a", "agent": "reviewer", "status": "completed"},
                        {"id": "sa-b", "title": "审计 b", "agent": "reviewer", "status": "failed"}
                    ]
                }
            }),
        ];

        replay_history(&messages, &event_tx, &mut state);

        let mut restored = None;
        while let Ok(event) = event_rx.try_recv() {
            assert!(
                !matches!(event, ConversationEvent::ToolChildren { .. }),
                "子智能体的过程在子会话里，不挂进父会话"
            );
            if let ConversationEvent::SubagentRuns { id, runs } = event {
                assert_eq!(id, "parallel-call");
                restored = Some(runs);
            }
        }
        let runs = restored.expect("工具结果里的子智能体名单要还原");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].id, "sa-a");
        assert_eq!(runs[0].title, "审计 a");
        assert_eq!(
            runs[0].status,
            crate::acp_session::SubagentStatus::Completed
        );
        assert_eq!(runs[1].id, "sa-b");
        assert_eq!(runs[1].status, crate::acp_session::SubagentStatus::Failed);
    }

    #[test]
    fn only_the_subagent_tool_carries_runs() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        let messages = [
            serde_json::json!({
                "role": "assistant",
                "content": [{
                    "type": "toolCall",
                    "id": "read-call",
                    "name": "read",
                    "arguments": {"path": "README.md"}
                }]
            }),
            serde_json::json!({
                "role": "toolResult",
                "toolCallId": "read-call",
                "toolName": "read",
                "content": [{"type": "text", "text": "ok"}],
                "details": {"runs": [{"id": "sa-x", "title": "x", "status": "completed"}]}
            }),
        ];

        replay_history(&messages, &event_tx, &mut state);

        while let Ok(event) = event_rx.try_recv() {
            assert!(
                !matches!(event, ConversationEvent::SubagentRuns { .. }),
                "别的工具的 details 碰巧有 runs 也不是子智能体派发"
            );
        }
    }

    #[test]
    fn native_event_sequence_maps_stream_tool_and_turn_lifecycle() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;

        for event in [
            serde_json::json!({
                "type": "message_update",
                "assistantMessageEvent": {"type": "thinking_delta", "delta": "plan"}
            }),
            serde_json::json!({
                "type": "message_update",
                "assistantMessageEvent": {"type": "text_delta", "delta": "answer"}
            }),
            serde_json::json!({
                "type": "tool_execution_start",
                "toolCallId": "tool-1",
                "toolName": "bash",
                "args": {"command": "pwd"}
            }),
            serde_json::json!({
                "type": "tool_execution_update",
                "toolCallId": "tool-1",
                "toolName": "bash",
                "args": {"command": "pwd"},
                "partialResult": {"content": [{"type": "text", "text": "/work"}]}
            }),
            serde_json::json!({
                "type": "tool_execution_end",
                "toolCallId": "tool-1",
                "toolName": "bash",
                "result": {"content": [{"type": "text", "text": "/work\n"}]},
                "isError": false
            }),
            serde_json::json!({
                "type": "message_end",
                "message": {
                    "role": "assistant",
                    "stopReason": "stop",
                    "usage": {"totalTokens": 7, "cacheRead": 2}
                }
            }),
            serde_json::json!({"type": "agent_settled"}),
        ] {
            handle_event(event, &event_tx, &outbound_tx, &mut state, &launch);
        }

        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::AgentChunk {
                thought: true,
                text,
                ..
            }) if text == "plan"
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::AgentChunk {
                thought: false,
                text,
                ..
            }) if text == "answer"
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ToolStarted { id, kind: ToolKind::Execute, .. }) if id == "tool-1"
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ToolDebug {
                id,
                name: Some(name),
                raw_input: Some(args),
            }) if id == "tool-1" && name == "bash" && args["command"] == "pwd"
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ToolOutputDelta { id, delta }) if id == "tool-1" && delta == "/work"
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ToolFinished {
                id,
                status: ToolCallStatus::Completed,
                ..
            }) if id == "tool-1"
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::TurnEnded(StopReason::EndTurn))
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderIdle)
        ));
        assert!(!state.active_turn);
        assert!(!state.provider_busy);
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn maps_native_pi_tool_content_without_acp_schema_round_trip() {
        let result = serde_json::json!({
            "content": [{"type": "text", "text": "done"}],
            "details": {"diff": "@@ -1 +1 @@\n-old\n+new"}
        });
        let output =
            tool_output_parts(&result, "edit", Some(&serde_json::json!({"path": "a.txt"})));
        assert!(matches!(&output[0], ToolOutputPart::Text(text) if text == "done"));
        assert!(matches!(
            &output[1],
            ToolOutputPart::Diff {
                path,
                old_text: Some(old),
                new_text
            } if path == "a.txt" && old.contains("old") && new_text.contains("new")
        ));
        assert_eq!(tool_kind("bash"), ToolKind::Execute);
        assert_eq!(tool_kind("subagent"), ToolKind::Collaborate);
        assert_eq!(tool_kind("stock_quote"), ToolKind::Other);
    }

    /// 后台子智能体的结果实时到达时画成状态行，不能变成用户消息；后台任务的同类
    /// 自定义消息不在这里发，避免和守护进程自己发的状态行重复。
    #[test]
    fn live_subagent_result_becomes_a_task_note_once() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();

        let custom = |custom_type: &str, id: &str| {
            serde_json::json!({
                "type": "message_end",
                "message": {
                    "role": "custom",
                    "customType": custom_type,
                    "display": false,
                    "content": [{"type": "text", "text": "<subagent-notification>…"}],
                    "details": {"id": id, "title": "迁移", "status": "completed", "outputTail": "改好了"}
                }
            })
        };
        handle_event(
            custom(crate::acp_chat::SMELT_SUBAGENT_RESULT_CUSTOM_TYPE, "sub-1"),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        handle_event(
            custom(crate::acp_chat::SMELT_BACKGROUND_TASK_CUSTOM_TYPE, "bg-1"),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );

        let events: Vec<_> = std::iter::from_fn(|| event_rx.try_recv().ok()).collect();
        let notes: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ConversationEvent::BackgroundNotice(note) => Some(note),
                _ => None,
            })
            .collect();
        assert_eq!(notes.len(), 1, "子智能体发一条，后台任务这里不发");
        assert_eq!(notes[0].id, "sub-1");
        assert_eq!(notes[0].kind, crate::acp_chat::TaskNoteKind::Subagent);
    }

    #[test]
    fn live_subagent_progress_reports_the_runs_instead_of_nested_children() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;

        handle_event(
            serde_json::json!({
                "type": "tool_execution_start",
                "toolCallId": "call-1",
                "toolName": "subagent",
                "args": {"agent": "scout", "task": "find auth"}
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        handle_event(
            serde_json::json!({
                "type": "tool_execution_update",
                "toolCallId": "call-1",
                "toolName": "subagent",
                "partialResult": {
                    "content": [{"type": "text", "text": "(running scout...)"}],
                    "details": {
                        "mode": "single",
                        "background": false,
                        "runs": [{"id": "sa-1", "title": "find auth", "agent": "scout", "status": "running"}]
                    }
                }
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );

        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ToolStarted {
                id,
                title,
                kind: ToolKind::Collaborate
            }) if id == "call-1" && title == "find auth"
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ToolDebug { id, .. }) if id == "call-1"
        ));
        let ConversationEvent::SubagentRuns { id, runs } = event_rx.try_recv().unwrap() else {
            panic!("expected the dispatched runs");
        };
        assert_eq!(id, "call-1");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, "sa-1");
        assert_eq!(runs[0].status, crate::acp_session::SubagentStatus::Running);
        assert!(event_rx.try_recv().is_err());
    }

    /// `session_send` 开始时就知道发给谁；结束后补上门卫回的标题和消息 id，被拒时带原因。
    #[test]
    fn session_send_reports_who_it_went_to_and_why_it_failed() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        let peer_sends = |event_rx: &smol::channel::Receiver<ConversationEvent>| -> Vec<crate::acp_chat::PeerSend> {
            std::iter::from_fn(|| event_rx.try_recv().ok())
                .filter_map(|event| match event {
                    ConversationEvent::PeerSend { id, send } if id == "call-9" => Some(send),
                    _ => None,
                })
                .collect()
        };

        handle_event(
            serde_json::json!({
                "type": "tool_execution_start",
                "toolCallId": "call-9",
                "toolName": "session_send",
                "args": {"to": "acp-b", "text": "跑测试", "kind": "task"}
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        let started = peer_sends(&event_rx);
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].to_id, "acp-b");
        assert_eq!(started[0].kind, crate::acp_session::PeerMessageKind::Task);
        assert_eq!(started[0].message_id, None);

        handle_event(
            serde_json::json!({
                "type": "tool_execution_end",
                "toolCallId": "call-9",
                "toolName": "session_send",
                "isError": false,
                "result": {
                    "content": [{"type": "text", "text": "已发给〈测试〉"}],
                    "details": {"to": {"id": "acp-b", "title": "测试"}, "kind": "task", "messageId": "pm-1"}
                }
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        let finished = peer_sends(&event_rx);
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].to_title, "测试");
        assert_eq!(finished[0].message_id.as_deref(), Some("pm-1"));
        assert_eq!(finished[0].error, None);

        handle_event(
            serde_json::json!({
                "type": "tool_execution_start",
                "toolCallId": "call-9",
                "toolName": "session_send",
                "args": {"to": "acp-c", "text": "x"}
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        handle_event(
            serde_json::json!({
                "type": "tool_execution_end",
                "toolCallId": "call-9",
                "toolName": "session_send",
                "isError": true,
                "result": {
                    "content": [{"type": "text", "text": "对方还有 8 条消息没收"}],
                    "details": {"error": true, "reason": "inbox_full", "to": {"id": "acp-c", "title": ""}}
                }
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        let refused = peer_sends(&event_rx);
        let last = refused.last().expect("被拒也要说发给了谁");
        assert_eq!(last.to_id, "acp-c");
        assert_eq!(last.error.as_deref(), Some("对方还有 8 条消息没收"));

        assert!(
            crate::acp_chat::peer_send_from_tool("bash", None, None, None).is_none(),
            "别的工具不算"
        );
    }

    #[test]
    fn tool_title_uses_command_or_path_from_args() {
        assert_eq!(
            tool_title("bash", Some(&serde_json::json!({"command": "git status"}))),
            "git status"
        );
        assert_eq!(
            tool_title("write", Some(&serde_json::json!({"path": "src/main.rs"}))),
            "src/main.rs"
        );
        assert_eq!(
            tool_title(
                "grep",
                Some(&serde_json::json!({"pattern": "TODO", "path": "crates"}))
            ),
            "TODO · crates"
        );
        assert_eq!(tool_title("bash", None), "bash");
        assert_eq!(
            tool_title(
                "subagent",
                Some(&serde_json::json!({"label": "查登录", "task": "find auth"}))
            ),
            "查登录"
        );
        assert_eq!(
            tool_title("subagent", Some(&serde_json::json!({"task": "find auth"}))),
            "find auth"
        );
    }

    #[test]
    fn tool_result_images_survive_as_image_parts() {
        let output = tool_output_parts(
            &serde_json::json!({
                "content": [
                    {"type": "text", "text": "Read image file [image/png]"},
                    {"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png"}
                ]
            }),
            "read",
            Some(&serde_json::json!({"path": "shot.png"})),
        );
        assert!(matches!(&output[0], ToolOutputPart::Text(text) if text.contains("Read image")));
        assert!(matches!(
            &output[1],
            ToolOutputPart::Image(image)
                if image.mime == "image/png" && image.data_b64 == "iVBORw0KGgo="
        ));
    }

    #[test]
    fn write_args_become_a_structured_diff() {
        let output = tool_output_parts(
            &serde_json::json!({"content": [{"type": "text", "text": "wrote file"}]}),
            "write",
            Some(&serde_json::json!({"path": "new.rs", "content": "fn main() {}\n"})),
        );
        assert!(matches!(&output[0], ToolOutputPart::Text(text) if text == "wrote file"));
        assert!(matches!(
            &output[1],
            ToolOutputPart::Diff {
                path,
                old_text: None,
                new_text
            } if path == "new.rs" && new_text.contains("fn main")
        ));
    }

    #[test]
    fn toolcall_stream_assembles_args_into_the_card_title() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;

        handle_event(
            serde_json::json!({
                "type": "message_update",
                "assistantMessageEvent": {
                    "type": "toolcall_start",
                    "id": "call-1",
                    "toolName": "bash"
                }
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        handle_event(
            serde_json::json!({
                "type": "message_update",
                "assistantMessageEvent": {
                    "type": "toolcall_delta",
                    "id": "call-1",
                    "delta": "{\"command\":\"ls -la\"}"
                }
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );

        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ToolStarted { id, title, kind: ToolKind::Execute })
                if id == "call-1" && title == "bash"
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ToolDebug {
                id,
                name: Some(name),
                raw_input: None,
            }) if id == "call-1" && name == "bash"
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ToolStarted { id, title, kind: ToolKind::Execute })
                if id == "call-1" && title == "ls -la"
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ToolDebug {
                id,
                name: Some(name),
                raw_input: Some(args),
            }) if id == "call-1" && name == "bash" && args["command"] == "ls -la"
        ));
    }

    #[test]
    fn message_usage_only_publishes_cache_without_clobbering_context() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        publish_usage(
            &serde_json::json!({
                "usage": {"totalTokens": 35_400_000, "cacheRead": 31_000_000}
            }),
            Some(128_000),
            &event_tx,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Usage {
                used: 0,
                size: 128_000,
                cached_read: Some(31_000_000),
                ..
            })
        ));
    }

    #[test]
    fn session_stats_publish_cost_cache_and_context_usage() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        publish_session_stats(
            &serde_json::json!({
                "tokens": {"input": 50000, "output": 10000, "cacheRead": 40000, "total": 105000},
                "cost": 0.45,
                "contextUsage": {"tokens": 60000, "contextWindow": 200000, "percent": 30}
            }),
            Some(200_000),
            &event_tx,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Usage {
                used: 60_000,
                size: 200_000,
                cached_read: Some(40_000),
                cost: Some(cost),
                ..
            }) if (cost - 0.45).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn stale_session_stats_response_cannot_overwrite_the_latest_context() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.model = Some(PiModel {
            provider: "anthropic".into(),
            id: "opus".into(),
            name: "Opus".into(),
            context_window: Some(200_000),
        });

        request_session_stats(&mut state, &outbound_tx);
        let stale_id = outbound_rx.try_recv().unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        request_session_stats(&mut state, &outbound_tx);
        let latest_id = outbound_rx.try_recv().unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let in_flight = AtomicUsize::new(0);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_response(
            serde_json::json!({
                "id": latest_id,
                "type": "response",
                "command": "get_session_stats",
                "success": true,
                "data": {
                    "tokens": {"cacheRead": 20_000},
                    "cost": 0.5,
                    "contextUsage": {"tokens": 32_000, "contextWindow": 200_000}
                }
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Usage {
                used: 32_000,
                size: 200_000,
                ..
            })
        ));

        smol::block_on(handle_response(
            serde_json::json!({
                "id": stale_id,
                "type": "response",
                "command": "get_session_stats",
                "success": true,
                "data": {
                    "tokens": {"cacheRead": 100_000},
                    "cost": 1.0,
                    "contextUsage": {"tokens": 150_000, "contextWindow": 200_000}
                }
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert!(
            event_rx.try_recv().is_err(),
            "迟到的旧统计不能覆盖较新的活动上下文"
        );
    }

    #[test]
    fn session_stats_do_not_replace_unknown_context_with_lifetime_tokens() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        publish_session_stats(
            &serde_json::json!({
                "tokens": {"cacheRead": 31_000_000, "total": 35_400_000},
                "cost": 1.25,
                "contextUsage": {"tokens": null, "contextWindow": 128_000, "percent": null}
            }),
            Some(128_000),
            &event_tx,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Usage {
                used: 0,
                size: 128_000,
                cached_read: Some(31_000_000),
                cost: Some(cost),
                ..
            }) if (cost - 1.25).abs() < f64::EPSILON
        ));
    }

    #[test]
    fn background_task_widget_publishes_the_task_list() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        publish_extension_widget(
            &serde_json::json!({
                "widgetKey": "smelt-background-tasks",
                "widgetLines": ["{\"tasks\":[{\"id\":\"bg-1\",\"title\":\"测试\",\"command\":\"npm test\",\"status\":\"running\",\"exitCode\":null,\"output\":\"hi\",\"startedAtMs\":10}]}"]
            }),
            &event_tx,
        );
        let Ok(ConversationEvent::BackgroundTasks(tasks)) = event_rx.try_recv() else {
            panic!("expected background tasks");
        };
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "bg-1");
        assert_eq!(
            tasks[0].status,
            crate::acp_session::BackgroundTaskStatus::Running
        );
        assert_eq!(tasks[0].output, "hi");
        assert_eq!(tasks[0].exit_code, None);
    }

    #[test]
    fn context_usage_widget_publishes_breakdown_without_clobbering_totals() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        publish_extension_widget(
            &serde_json::json!({
                "widgetKey": "smelt-context-usage",
                "widgetLines": ["{\"systemPrompt\":2000,\"toolsDefinition\":5600,\"rules\":1100,\"skills\":1700,\"mcpDynamic\":0,\"subagent\":0,\"summarized\":0,\"conversation\":1600}"]
            }),
            &event_tx,
        );
        let Ok(ConversationEvent::Usage {
            used,
            size,
            breakdown: Some(breakdown),
            ..
        }) = event_rx.try_recv()
        else {
            panic!("expected usage breakdown");
        };
        assert_eq!(used, 0);
        assert_eq!(size, 0);
        assert_eq!(breakdown.system_prompt, 2_000);
        assert_eq!(breakdown.tools_definition, 5_600);
        assert_eq!(breakdown.occupied(), 12_000);
    }

    #[test]
    fn runtime_debug_widget_publishes_call_history_and_redacts_sensitive_paths() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        publish_extension_widget(
            &serde_json::json!({
                "widgetKey": "smelt-runtime-debug",
                "widgetLines": [serde_json::json!({
                    "version": 3,
                    "source": "pi_runtime_debug",
                    "systemPrompt": "system line 1\n<project_context>真实上下文</project_context>",
                    "tools": [{
                        "name": "bash",
                        "description": "Run a shell command",
                        "parameters": {
                            "type": "object",
                            "required": ["command"],
                            "properties": {"command": {"type": "string"}}
                        },
                        "source": "builtin"
                    }],
                    "modelCalls": [{
                        "sequence": 3,
                        "turn": 2,
                        "compactionSequence": 1,
                        "capturedAtMs": 1725000000000_u64,
                        "source": "pi_context_with_system",
                        "payloadSource": "pi_before_provider_request",
                        "piContext": [
                            {"role": "system", "content": "exact Pi system message"},
                            {"role": "user", "content": "hello", "api_key": "context-secret"}
                        ],
                        "piContextRedactedPaths": [],
                        "model": {
                            "provider": "openai",
                            "id": "gpt-test",
                            "api": "responses",
                            "thinkingLevel": "high"
                        },
                        "requestConfig": {
                            "systemPrompt": "request-specific system prompt",
                            "tools": [{
                                "name": "bash",
                                "description": "Run a shell command",
                                "parameters": {"type": "object"},
                                "source": "builtin"
                            }]
                        },
                        "requestHeaders": [{
                            "capturedAtMs": 1725000000001_u64,
                            "headers": {
                                "Authorization": "Bearer header-secret",
                                "X-Api-Key": "header-api-secret",
                                "X-Request-Source": "smelt"
                            },
                            "redactedPaths": []
                        }],
                        "requestCapturedAtMs": 1725000000002_u64,
                        "payload": {
                            "model": "gpt-test",
                            "max_tokens": 4096,
                            "messages": [{"role": "user", "content": "hello"}],
                            "api_key": "malicious-widget-secret",
                            "nested": {"Authorization": "Bearer secret"},
                            "image": {
                                "type": "image",
                                "data": "large-base64-payload",
                                "url": "data:image/png;base64,also-large"
                            }
                        },
                        "response": {
                            "role": "assistant",
                            "content": [{"type": "text", "text": "done"}],
                            "usage": {"input": 100, "output": 20},
                            "provider_metadata": {"api_key": "response-secret"}
                        },
                        "responseCapturedAtMs": 1725000000100_u64,
                        "responseRedactedPaths": [],
                        "responseMetadata": [{
                            "capturedAtMs": 1725000000050_u64,
                            "status": 200,
                            "headers": {
                                "X-Request-Id": "request-1",
                                "Set-Cookie": "cookie-secret"
                            },
                            "redactedPaths": []
                        }],
                        "redactedPaths": []
                    }],
                    "modelCallsOmitted": 0,
                    "compactions": [{
                        "sequence": 1,
                        "turn": 2,
                        "status": "completed",
                        "reason": "manual",
                        "willRetry": false,
                        "startedAtMs": 1725000000000_u64,
                        "finishedAtMs": 1725000001000_u64,
                        "firstKeptEntryId": "entry-20",
                        "tokensBefore": 90000,
                        "summarizedMessageCount": 1,
                        "turnPrefixMessageCount": 0,
                        "sourceMessages": [{
                            "segment": "summarized",
                            "role": "user",
                            "preview": "hello",
                            "truncated": false
                        }],
                        "sourceMessagesOmitted": 0,
                        "summary": "keep the login constraints",
                        "usage": {"api_key": "compaction-secret"},
                        "fromExtension": false
                    }],
                    "compactionsOmitted": 0
                }).to_string()]
            }),
            &event_tx,
        );
        let Ok(ConversationEvent::RuntimeDebug(debug)) = event_rx.try_recv() else {
            panic!("expected runtime debug event");
        };
        assert_eq!(debug.version, 3);
        assert_eq!(debug.source, "pi_runtime_debug");
        assert_eq!(
            debug.system_prompt.as_deref(),
            Some("system line 1\n<project_context>真实上下文</project_context>")
        );
        assert_eq!(debug.tools.len(), 1);
        assert_eq!(debug.tools[0].name, "bash");
        assert_eq!(debug.tools[0].parameters["required"][0], "command");
        assert_eq!(debug.model_calls_omitted, 0);
        let call = debug.model_calls.first().expect("model call");
        assert_eq!(call.sequence, 3);
        assert_eq!(call.turn, Some(2));
        assert_eq!(call.compaction_sequence, Some(1));
        assert_eq!(call.captured_at_ms, 1_725_000_000_000);
        assert_eq!(call.model.provider.as_deref(), Some("openai"));
        assert_eq!(call.source, "pi_context_with_system");
        assert_eq!(
            call.payload_source.as_deref(),
            Some("pi_before_provider_request")
        );
        assert_eq!(call.pi_context[0]["content"], "exact Pi system message");
        assert_eq!(call.pi_context[1]["api_key"], "[REDACTED]");
        assert_eq!(
            call.pi_context_redacted_paths,
            vec!["$.piContext[1].api_key"]
        );
        assert_eq!(
            call.request_headers[0].headers["Authorization"],
            "[REDACTED]"
        );
        assert_eq!(call.request_headers[0].headers["X-Api-Key"], "[REDACTED]");
        assert_eq!(call.request_headers[0].headers["X-Request-Source"], "smelt");
        assert_eq!(
            call.request_headers[0].redacted_paths,
            vec![
                "$.requestHeaders[0].headers.Authorization",
                "$.requestHeaders[0].headers.X-Api-Key"
            ]
        );
        assert_eq!(call.response_metadata[0].status, 200);
        assert_eq!(
            call.response_metadata[0].headers["X-Request-Id"],
            "request-1"
        );
        assert_eq!(
            call.response_metadata[0].headers["Set-Cookie"],
            "[REDACTED]"
        );
        assert_eq!(
            call.response_metadata[0].redacted_paths,
            vec!["$.responseMetadata[0].headers.Set-Cookie"]
        );
        assert_eq!(
            call.request_config.system_prompt,
            "request-specific system prompt"
        );
        assert_eq!(call.request_config.tools[0].name, "bash");
        assert_eq!(call.payload["max_tokens"], 4096);
        assert_eq!(call.payload["api_key"], "[REDACTED]");
        assert_eq!(call.payload["nested"]["Authorization"], "[REDACTED]");
        assert_eq!(call.payload["image"]["data"], "[IMAGE DATA OMITTED]");
        assert_eq!(call.payload["image"]["url"], "[IMAGE DATA OMITTED]");
        assert_eq!(call.response.as_ref().unwrap()["usage"]["output"], 20);
        assert_eq!(
            call.response.as_ref().unwrap()["provider_metadata"]["api_key"],
            "[REDACTED]"
        );
        assert_eq!(call.response_captured_at_ms, Some(1_725_000_000_100));
        assert_eq!(
            call.response_redacted_paths.as_ref().unwrap(),
            &["$.response.provider_metadata.api_key"]
        );
        assert_eq!(
            call.redacted_paths,
            vec![
                "$.payload.api_key".to_string(),
                "$.payload.nested.Authorization".to_string(),
                "$.payload.image.data".to_string(),
                "$.payload.image.url".to_string()
            ]
        );
        assert_eq!(debug.compactions.len(), 1);
        assert_eq!(debug.compactions[0].turn, Some(2));
        assert_eq!(debug.compactions[0].status, "completed");
        assert_eq!(
            debug.compactions[0].first_kept_entry_id.as_deref(),
            Some("entry-20")
        );
        assert_eq!(
            debug.compactions[0].usage.as_ref().unwrap()["api_key"],
            "[REDACTED]"
        );
    }

    #[test]
    fn runtime_debug_widget_migrates_legacy_single_call_into_history() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        publish_extension_widget(
            &serde_json::json!({
                "widgetKey": "smelt-runtime-debug",
                "widgetLines": [serde_json::json!({
                    "version": 2,
                    "source": "pi_runtime_debug",
                    "systemPrompt": "system",
                    "tools": [],
                    "modelCall": {
                        "sequence": 1,
                        "source": "pi_before_provider_request",
                        "model": {},
                        "payload": {"messages": []},
                        "redactedPaths": []
                    }
                }).to_string()]
            }),
            &event_tx,
        );
        let Ok(ConversationEvent::RuntimeDebug(debug)) = event_rx.try_recv() else {
            panic!("expected migrated runtime debug event");
        };
        assert_eq!(debug.version, 3);
        assert_eq!(debug.model_call, None);
        assert_eq!(debug.model_calls.len(), 1);
        assert_eq!(debug.model_calls[0].sequence, 1);
    }

    #[test]
    fn runtime_debug_keeps_only_the_newest_calls_and_compactions() {
        let calls = (1..=40)
            .map(|sequence| {
                serde_json::json!({
                    "sequence": sequence,
                    "source": "pi_before_provider_request",
                    "model": {},
                    "requestConfig": {"systemPrompt": "system", "tools": []},
                    "payload": {"messages": [{"role": "user", "content": sequence}]},
                    "redactedPaths": []
                })
            })
            .collect::<Vec<_>>();
        let preview = "x".repeat(4_000);
        let source_messages = (0..80)
            .map(|index| {
                serde_json::json!({
                    "segment": "summarized",
                    "role": "user",
                    "preview": format!("{index}:{preview}"),
                    "truncated": false
                })
            })
            .collect::<Vec<_>>();
        let compactions = (1..=6)
            .map(|sequence| {
                serde_json::json!({
                    "sequence": sequence,
                    "status": "completed",
                    "reason": "manual",
                    "willRetry": false,
                    "startedAtMs": 1725000000000_u64 + sequence,
                    "sourceMessages": if sequence == 6 {
                        source_messages.clone()
                    } else {
                        Vec::<serde_json::Value>::new()
                    },
                    "sourceMessagesOmitted": 0,
                    "summary": "complete summary"
                })
            })
            .collect::<Vec<_>>();
        let (event_tx, event_rx) = smol::channel::unbounded();
        publish_extension_widget(
            &serde_json::json!({
                "widgetKey": "smelt-runtime-debug",
                "widgetLines": [serde_json::json!({
                    "version": 3,
                    "source": "pi_runtime_debug",
                    "systemPrompt": "system",
                    "tools": [],
                    "modelCalls": calls,
                    "compactions": compactions
                }).to_string()]
            }),
            &event_tx,
        );
        let Ok(ConversationEvent::RuntimeDebug(debug)) = event_rx.try_recv() else {
            panic!("full model and compaction history should be accepted");
        };
        assert_eq!(debug.model_calls.len(), 4);
        assert_eq!(debug.model_calls[0].sequence, 37);
        assert_eq!(debug.model_calls[3].sequence, 40);
        assert_eq!(debug.model_calls_omitted, 36);
        assert_eq!(debug.compactions.len(), 4);
        assert_eq!(debug.compactions[0].sequence, 3);
        assert_eq!(debug.compactions_omitted, 2);
        assert_eq!(debug.compactions[3].source_messages.len(), 80);
        assert_eq!(debug.compactions[3].source_messages[0].preview.len(), 4_002);
    }

    #[test]
    fn runtime_debug_can_report_compaction_before_any_system_prompt_capture() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        publish_extension_widget(
            &serde_json::json!({
                "widgetKey": "smelt-runtime-debug",
                "widgetLines": [serde_json::json!({
                    "version": 3,
                    "source": "pi_runtime_debug",
                    "systemPrompt": "",
                    "tools": [],
                    "modelCalls": [],
                    "modelCallsOmitted": 0,
                    "compactions": [{
                        "sequence": 1,
                        "status": "started",
                        "reason": "manual",
                        "willRetry": false,
                        "startedAtMs": 1725000000000_u64,
                        "sourceMessages": [],
                        "sourceMessagesOmitted": 0
                    }],
                    "compactionsOmitted": 0
                }).to_string()]
            }),
            &event_tx,
        );
        let Ok(ConversationEvent::RuntimeDebug(debug)) = event_rx.try_recv() else {
            panic!("a real compaction capture must not require a prior prompt capture");
        };
        assert_eq!(debug.system_prompt.as_deref(), Some(""));
        assert_eq!(debug.compactions.len(), 1);
    }

    #[test]
    fn runtime_debug_widget_rejects_unknown_versions_and_empty_prompts() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        for payload in [
            serde_json::json!({
                "version": 4,
                "source": "pi_runtime_debug",
                "systemPrompt": "future",
                "tools": []
            }),
            serde_json::json!({
                "version": 1,
                "source": "pi_before_agent_start",
                "systemPrompt": "",
                "tools": []
            }),
            serde_json::json!({
                "version": 1,
                "source": "another_extension",
                "systemPrompt": "spoofed",
                "tools": []
            }),
        ] {
            publish_extension_widget(
                &serde_json::json!({
                    "widgetKey": "smelt-runtime-debug",
                    "widgetLines": [payload.to_string()]
                }),
                &event_tx,
            );
        }
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn set_title_request_forwards_session_title_event() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let state = PiState::new();
        handle_extension_ui(
            &serde_json::json!({
                "type": "extension_ui_request",
                "id": "title-1",
                "method": "setTitle",
                "title": "pi - my project"
            }),
            &event_tx,
            &outbound_tx,
            &state,
            &launch,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::SessionTitle(Some(title))) if title == "pi - my project"
        ));
        // fire-and-forget：不得有任何回包。
        let (outbound_tx, outbound_rx) = smol::channel::unbounded();
        handle_extension_ui(
            &serde_json::json!({
                "type": "extension_ui_request",
                "id": "title-2",
                "method": "setTitle",
                "title": "x"
            }),
            &event_tx,
            &outbound_tx,
            &state,
            &launch,
        );
        assert!(outbound_rx.try_recv().is_err());
    }

    #[test]
    fn notify_request_reaches_the_notice_redacted() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let state = PiState::new();
        handle_extension_ui(
            &serde_json::json!({
                "type": "extension_ui_request",
                "id": "notify-1",
                "method": "notify",
                "message": "MCP servers need attention:\n  docs: failed: Error POSTing to https://u:p@mcp.example.com/mcp?api_key=k1 (HTTP 401): {\"access_token\":\"t1\"}\nRun /mcp to fix.",
                "notifyType": "warning"
            }),
            &event_tx,
            &outbound_tx,
            &state,
            &launch,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Notice(message)) if message
                == "MCP servers need attention:\n  docs: failed: Error POSTing to https://***@mcp.example.com/mcp?api_key=*** (HTTP 401): {\"access_token\":\"***\"}\nRun /mcp to fix."
        ));
    }

    #[test]
    fn permission_payload_is_fail_closed_and_keeps_structured_command() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded();
        let state = PiState::new();
        handle_extension_ui(
            &serde_json::json!({
                "type": "extension_ui_request",
                "id": "permission-1",
                "method": "confirm",
                "title": SMELT_PERMISSION_TITLE,
                "message": serde_json::json!({
                    "version": 1,
                    "toolCallId": "tool-1",
                    "toolName": "bash",
                    "input": {"command": "git status"}
                }).to_string()
            }),
            &event_tx,
            &outbound_tx,
            &state,
            &launch,
        );
        let ConversationEvent::Permission {
            question,
            details,
            responder,
            ..
        } = event_rx.try_recv().unwrap()
        else {
            panic!("expected permission event");
        };
        assert!(question.contains("git status"));
        assert!(
            matches!(details, ApprovalDetailsView::Command { command, .. } if command == "git status")
        );
        responder.select("reject_once".to_string());
        let response = outbound_rx.try_recv().unwrap();
        assert_eq!(response["confirmed"], false);
    }

    #[test]
    fn bypass_mode_auto_confirms_only_smelt_permission_requests() {
        let mut launch = test_launch(SMELT_PI_AGENT_COMMAND);
        launch.cwd = None;
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.permission_mode = "bypassPermissions".to_string();
        handle_extension_ui(
            &serde_json::json!({
                "type": "extension_ui_request",
                "id": "permission-1",
                "method": "confirm",
                "title": SMELT_PERMISSION_TITLE,
                "message": serde_json::json!({
                    "version": 1,
                    "toolCallId": "tool-1", "toolName": "write", "input": {"path": "a.txt"}
                }).to_string()
            }),
            &event_tx,
            &outbound_tx,
            &state,
            &launch,
        );
        assert!(event_rx.try_recv().is_err());
        assert_eq!(outbound_rx.try_recv().unwrap()["confirmed"], true);
    }

    #[test]
    fn reserved_permission_title_rejects_unknown_payload_versions() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.permission_mode = "bypassPermissions".to_string();
        handle_extension_ui(
            &serde_json::json!({
                "type": "extension_ui_request",
                "id": "permission-1",
                "method": "confirm",
                "title": SMELT_PERMISSION_TITLE,
                "message": serde_json::json!({
                    "version": 2,
                    "toolCallId": "tool-1",
                    "toolName": "write",
                    "input": {"path": "a.txt"}
                }).to_string()
            }),
            &event_tx,
            &outbound_tx,
            &state,
            &launch,
        );
        assert!(event_rx.try_recv().is_err());
        assert_eq!(outbound_rx.try_recv().unwrap()["cancelled"], true);
    }

    #[test]
    fn select_stays_single_choice_without_multiselect_tool_args() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded();
        let state = PiState::new();
        handle_extension_ui(
            &serde_json::json!({
                "type": "extension_ui_request",
                "id": "select-1",
                "method": "select",
                "title": "用哪种节奏？",
                "options": ["紧凑续集", "开放番外"]
            }),
            &event_tx,
            &outbound_tx,
            &state,
            &launch,
        );
        let ConversationEvent::Elicitation {
            fields, responder, ..
        } = event_rx.try_recv().unwrap()
        else {
            panic!("expected elicitation event");
        };
        // 没有同题面的题目工具在飞：不开放自定义输入，普通 select 行为不变。
        assert!(!fields[0].allow_custom_input);
        assert!(matches!(fields[0].kind, ElicitFieldKind::Select(_)));
        let mut content = BTreeMap::new();
        content.insert(
            "value".to_string(),
            ElicitationContentValue::String("开放番外".to_string()),
        );
        responder.accept(content);
        assert_eq!(outbound_rx.try_recv().unwrap()["value"], "开放番外");
    }

    #[test]
    fn select_from_question_tool_allows_custom_answer() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.tool_args.insert(
            "tool-q".to_string(),
            serde_json::json!({
                "question": "要发布到哪个环境？",
                "options": [
                    {"label": "测试", "description": "先验一圈"},
                    {"label": "生产", "description": "直接上"}
                ]
            }),
        );
        handle_extension_ui(
            &serde_json::json!({
                "type": "extension_ui_request",
                "id": "select-custom",
                "method": "select",
                "title": "要发布到哪个环境？",
                "options": ["测试 — 先验一圈", "生产 — 直接上"]
            }),
            &event_tx,
            &outbound_tx,
            &state,
            &launch,
        );
        let ConversationEvent::Elicitation {
            fields, responder, ..
        } = event_rx.try_recv().unwrap()
        else {
            panic!("expected elicitation event");
        };
        // 题目工具发起的 select：仍里单选，但允许用户自己写答案。
        assert!(matches!(fields[0].kind, ElicitFieldKind::Select(_)));
        assert!(fields[0].allow_custom_input);
        // 用户没点选项、直接提交自己写的答案：原样透传给扩展。
        let mut content = BTreeMap::new();
        content.insert(
            "value".to_string(),
            ElicitationContentValue::String("灰度环境".to_string()),
        );
        responder.accept(content);
        assert_eq!(outbound_rx.try_recv().unwrap()["value"], "灰度环境");
    }

    #[test]
    fn select_with_multiselect_tool_args_emits_a_multi_select_card() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.tool_args.insert(
            "tool-1".to_string(),
            serde_json::json!({
                "questions": [{
                    "question": "提醒走哪个渠道？",
                    "multiSelect": true,
                    "options": [
                        {"label": "Gitea Issue", "description": "仓库待办"},
                        {"label": "Bark", "description": "手机推送"}
                    ]
                }]
            }),
        );
        handle_extension_ui(
            &serde_json::json!({
                "type": "extension_ui_request",
                "id": "select-2",
                "method": "select",
                "title": "提醒走哪个渠道？",
                "options": ["Gitea Issue — 仓库待办", "Bark — 手机推送"]
            }),
            &event_tx,
            &outbound_tx,
            &state,
            &launch,
        );
        let ConversationEvent::Elicitation {
            fields, responder, ..
        } = event_rx.try_recv().unwrap()
        else {
            panic!("expected elicitation event");
        };
        let ElicitFieldKind::MultiSelect(options) = &fields[0].kind else {
            panic!("multiSelect tool args should render a multi-select card");
        };
        assert_eq!(options.len(), 2);
        // 题目卡一律允许选项之外自己写答案。
        assert!(fields[0].allow_custom_input);
        let mut content = BTreeMap::new();
        content.insert(
            "value".to_string(),
            ElicitationContentValue::StringArray(vec![
                "Gitea Issue — 仓库待办".to_string(),
                "Bark — 手机推送".to_string(),
            ]),
        );
        responder.accept(content);
        assert_eq!(
            outbound_rx.try_recv().unwrap()["value"],
            r#"["Gitea Issue — 仓库待办","Bark — 手机推送"]"#
        );
    }

    #[test]
    fn select_title_mark_emits_a_multi_select_card_without_tool_args() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let state = PiState::new();
        handle_extension_ui(
            &serde_json::json!({
                "type": "extension_ui_request",
                "id": "select-3",
                "method": "select",
                "title": format!("{MULTI_SELECT_TITLE_MARK}选几种水果看看"),
                "options": ["苹果", "香蕉", "橙子"]
            }),
            &event_tx,
            &outbound_tx,
            &state,
            &launch,
        );
        let ConversationEvent::Elicitation {
            message, fields, ..
        } = event_rx.try_recv().unwrap()
        else {
            panic!("expected elicitation event");
        };
        assert_eq!(message, "选几种水果看看");
        assert!(matches!(fields[0].kind, ElicitFieldKind::MultiSelect(_)));
        assert_eq!(fields[0].title, "选几种水果看看");
        // 标记多选同样来自题目工具：也允许自定义答案。
        assert!(fields[0].allow_custom_input);
    }

    #[test]
    fn prompt_during_an_active_turn_is_steered_instead_of_dropped() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        let in_flight = AtomicUsize::new(2);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::Prompt {
                text: "second".to_string(),
                images: Vec::new(),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        let request: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.into_inner()).unwrap().trim()).unwrap();
        assert_eq!(request["type"], "steer");
        assert_eq!(request["message"], "second");
        assert!(state.active_turn);
        assert_eq!(in_flight.load(Ordering::SeqCst), 2);
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn background_notify_becomes_a_status_event_and_is_not_a_pi_prompt() {
        let event = background_notice_from_outbound(&serde_json::json!({
            "type": "smelt_background_notify",
            "id": "bg-1",
            "title": "编译",
            "status": "failed",
            "exitCode": 1,
            "outputTail": "error: boom",
        }));
        let Some(ConversationEvent::BackgroundNotice(note)) = event else {
            panic!("完成通知必须是状态事件");
        };
        assert_eq!(note.id, "bg-1");
        assert_eq!(note.exit_code, Some(1));
        assert_eq!(note.output_tail, "error: boom");
        assert!(note.summary().contains("失败"));
        assert!(background_notice_from_outbound(&serde_json::json!({"type": "other"})).is_none());
        assert!(
            background_notice_from_outbound(&serde_json::json!({
                "type": "smelt_background_notify",
                "message": "后台任务 bg-1 已结束",
            }))
            .is_none(),
            "只有一句提示词的旧通知不能再变成事件"
        );
    }

    #[test]
    fn background_task_custom_message_replays_as_a_status_line() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        replay_messages(
            &[
                serde_json::json!({
                    "role": "custom",
                    "customType": crate::acp_chat::SMELT_BACKGROUND_TASK_CUSTOM_TYPE,
                    "display": false,
                    "content": [{"type": "text", "text": "<task-notification>\nexit_code: 1\n</task-notification>"}],
                    "details": {
                        "id": "bg-13",
                        "title": "编译",
                        "status": "failed",
                        "exitCode": 1,
                        "outputTail": "error: boom",
                    },
                }),
                serde_json::json!({
                    "role": "custom",
                    "customType": "note",
                    "display": false,
                    "content": "隐藏的其它消息",
                }),
                serde_json::json!({
                    "role": "custom",
                    "customType": "note",
                    "display": true,
                    "content": "可见扩展消息",
                }),
            ],
            &event_tx,
            &mut state,
        );
        let first = event_rx.try_recv().unwrap();
        let ConversationEvent::BackgroundNotice(note) = first else {
            panic!("后台任务的自定义消息必须重放成状态行");
        };
        assert_eq!(note.id, "bg-13");
        assert_eq!(note.exit_code, Some(1));
        assert_eq!(note.output_tail, "error: boom");
        let second = event_rx.try_recv().unwrap();
        match second {
            ConversationEvent::AgentChunk { text, .. } => assert_eq!(text, "可见扩展消息"),
            _ => panic!("其它可见自定义消息仍是助手文本"),
        }
        assert!(event_rx.try_recv().is_err());
    }

    fn answer_pi(
        state: &mut PiState,
        event_tx: &smol::channel::Sender<ConversationEvent>,
        response: serde_json::Value,
    ) -> Vec<serde_json::Value> {
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_response(
            response,
            &mut writer,
            event_tx,
            state,
            &in_flight,
        ))
        .unwrap();
        String::from_utf8(writer.into_inner())
            .unwrap()
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn prompt_refused_while_pi_is_streaming_is_not_a_failed_turn() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::Prompt {
                text: "后台任务 bg-1 已结束".to_string(),
                images: Vec::new(),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        let request: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.into_inner()).unwrap().trim()).unwrap();
        let prompt_id = request["id"].as_str().unwrap();
        assert!(state.active_turn);
        assert!(state.provider_busy);
        assert!(!state.prompt_accepted);

        let checks = answer_pi(
            &mut state,
            &event_tx,
            serde_json::json!({
                "id": prompt_id,
                "type": "response",
                "success": false,
                "error": "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message."
            }),
        );
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["type"], "get_state");
        assert!(event_rx.try_recv().is_err());
        assert!(state.active_turn);

        let follow = answer_pi(
            &mut state,
            &event_tx,
            serde_json::json!({
                "id": checks[0]["id"].as_str().unwrap(),
                "type": "response",
                "success": true,
                "data": {"isStreaming": true, "isCompacting": false}
            }),
        );
        assert!(follow.is_empty());
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::PromptNotAccepted)
        ));
        assert!(event_rx.try_recv().is_err());
        assert!(state.active_turn);
        assert!(state.provider_busy);
        assert!(!state.prompt_accepted);

        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (outbound_tx, _) = smol::channel::unbounded();
        handle_event(
            serde_json::json!({"type": "agent_settled"}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::TurnEnded(StopReason::EndTurn))
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderIdle)
        ));
        assert!(!state.active_turn);
        assert!(!state.provider_busy);
    }

    #[test]
    fn prompt_refused_after_the_run_already_stopped_reopens_admission() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        state.provider_busy = true;
        state.prompt_request_id = Some("prompt-1".to_string());
        let checks = answer_pi(
            &mut state,
            &event_tx,
            serde_json::json!({
                "id": "prompt-1",
                "success": false,
                "error": "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry."
            }),
        );
        assert!(
            answer_pi(
                &mut state,
                &event_tx,
                serde_json::json!({
                    "id": checks[0]["id"].as_str().unwrap(),
                    "success": true,
                    "data": {"isStreaming": false, "isCompacting": false}
                }),
            )
            .is_empty()
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::PromptNotAccepted)
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderIdle)
        ));
        assert!(event_rx.try_recv().is_err());
        assert!(!state.active_turn);
        assert!(!state.provider_busy);
    }

    #[test]
    fn compaction_end_admits_a_prompt_pi_never_accepted() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        state.provider_busy = true;
        state.prompt_request_id = Some("prompt-1".to_string());
        let checks = answer_pi(
            &mut state,
            &event_tx,
            serde_json::json!({
                "id": "prompt-1",
                "success": false,
                "error": "Cannot submit a prompt while compaction is in progress. Wait for compaction to finish and retry."
            }),
        );
        assert!(
            answer_pi(
                &mut state,
                &event_tx,
                serde_json::json!({
                    "id": checks[0]["id"].as_str().unwrap(),
                    "success": true,
                    "data": {"isStreaming": false, "isCompacting": true}
                }),
            )
            .is_empty()
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::PromptNotAccepted)
        ));
        assert!(event_rx.try_recv().is_err(), "压缩还没结束不能放行");
        assert!(state.active_turn);
        assert!(state.provider_busy);

        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (outbound_tx, _) = smol::channel::unbounded();
        handle_event(
            serde_json::json!({
                "type": "compaction_end",
                "aborted": false,
                "result": {"tokensBefore": 10, "estimatedTokensAfter": 4}
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Compaction { running: false, .. })
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderIdle)
        ));
        assert!(!state.active_turn);
        assert!(!state.provider_busy);

        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::Prompt {
                text: "后台任务 bg-4 已结束".to_string(),
                images: Vec::new(),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        let request: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.into_inner()).unwrap().trim()).unwrap();
        assert_eq!(request["type"], "prompt");
    }

    #[test]
    fn compaction_end_during_an_accepted_turn_does_not_admit() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        state.prompt_accepted = true;
        state.provider_busy = true;
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        handle_event(
            serde_json::json!({"type": "compaction_end", "aborted": false}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Compaction { running: false, .. })
        ));
        assert!(event_rx.try_recv().is_err());
        assert!(state.active_turn);
        assert!(state.provider_busy);
    }

    #[test]
    fn stale_compacting_snapshot_after_compaction_end_still_admits() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        state.provider_busy = true;
        state.prompt_request_id = Some("prompt-1".to_string());
        let checks = answer_pi(
            &mut state,
            &event_tx,
            serde_json::json!({
                "id": "prompt-1",
                "success": false,
                "error": "Cannot submit a prompt while compaction is in progress."
            }),
        );
        assert!(event_rx.try_recv().is_err());
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        handle_event(
            serde_json::json!({"type": "compaction_end", "aborted": false}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Compaction { running: false, .. })
        ));
        assert!(
            event_rx.try_recv().is_err(),
            "快照还没回来时不能先放行，否则回队发生在下一拍"
        );
        assert!(state.active_turn);

        assert!(
            answer_pi(
                &mut state,
                &event_tx,
                serde_json::json!({
                    "id": checks[0]["id"].as_str().unwrap(),
                    "success": true,
                    "data": {"isStreaming": false, "isCompacting": true}
                }),
            )
            .is_empty()
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::PromptNotAccepted)
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderIdle)
        ));
        assert!(!state.active_turn);
        assert!(!state.provider_busy);
    }

    #[test]
    fn a_real_prompt_error_still_fails_the_turn_once_pi_is_idle() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        state.provider_busy = true;
        state.prompt_request_id = Some("prompt-1".to_string());
        let checks = answer_pi(
            &mut state,
            &event_tx,
            serde_json::json!({
                "id": "prompt-1",
                "success": false,
                "error": "no api key configured"
            }),
        );
        assert!(
            answer_pi(
                &mut state,
                &event_tx,
                serde_json::json!({
                    "id": checks[0]["id"].as_str().unwrap(),
                    "success": true,
                    "data": {"isStreaming": false}
                }),
            )
            .is_empty()
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::TurnFailed(message)) if message.contains("no api key")
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderIdle)
        ));
        assert!(!state.active_turn);
        assert!(!state.provider_busy);
    }

    #[test]
    fn cancel_keeps_pi_admission_closed_until_the_provider_is_idle() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        state.provider_busy = true;
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::Cancel,
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::TurnEnded(StopReason::Cancelled))
        ));
        assert!(event_rx.try_recv().is_err());
        assert!(!state.active_turn);
        assert!(state.provider_busy);
        let lines: Vec<serde_json::Value> = String::from_utf8(writer.into_inner())
            .unwrap()
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let state_id = lines
            .iter()
            .find(|line| line["type"] == "get_state")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            answer_pi(
                &mut state,
                &event_tx,
                serde_json::json!({
                    "id": state_id,
                    "success": true,
                    "data": {"isStreaming": true}
                }),
            )
            .is_empty()
        );
        assert!(event_rx.try_recv().is_err());
        assert!(state.provider_busy);

        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (outbound_tx, _) = smol::channel::unbounded();
        handle_event(
            serde_json::json!({"type": "agent_settled"}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderIdle)
        ));
        assert!(!state.provider_busy);
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn steer_stays_steer_while_pi_is_busy_after_the_ui_turn_closed() {
        let (event_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.provider_busy = true;
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::Steer {
                text: "接着说".to_string(),
                images: Vec::new(),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        let request: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.into_inner()).unwrap().trim()).unwrap();
        assert_eq!(request["type"], "steer");
        assert!(!state.active_turn);
    }

    #[test]
    fn settled_previous_run_does_not_drop_a_prompt_still_waiting_on_its_refusal() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        state.provider_busy = true;
        state.prompt_request_id = Some("prompt-1".to_string());
        let checks = answer_pi(
            &mut state,
            &event_tx,
            serde_json::json!({
                "id": "prompt-1",
                "success": false,
                "error": "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message."
            }),
        );
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (outbound_tx, _) = smol::channel::unbounded();
        handle_event(
            serde_json::json!({"type": "agent_settled"}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderIdle)
        ));
        assert!(state.active_turn);
        assert!(event_rx.try_recv().is_err());

        assert!(
            answer_pi(
                &mut state,
                &event_tx,
                serde_json::json!({
                    "id": checks[0]["id"].as_str().unwrap(),
                    "success": true,
                    "data": {"isStreaming": false, "isCompacting": false}
                }),
            )
            .is_empty()
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::PromptNotAccepted)
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderIdle)
        ));
        assert!(!state.active_turn);
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn reload_slash_is_only_the_exact_command() {
        assert!(parse_reload_slash("  /reload  "));
        assert!(!parse_reload_slash("/reload 现在"));
        assert!(!parse_reload_slash("/reloader"));
        assert!(!parse_reload_slash("reload"));
    }

    fn rpc_lines(writer: futures::io::Cursor<Vec<u8>>) -> Vec<serde_json::Value> {
        String::from_utf8(writer.into_inner())
            .unwrap()
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn slash_reload_is_rejected_without_a_prompt() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state
            .available_commands
            .push(("reload".into(), "Reload skills".into()));
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::Prompt {
                text: "/reload".to_string(),
                images: Vec::new(),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        assert!(rpc_lines(writer).is_empty());
        assert!(!state.active_turn);
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Notice(text)) if text.contains("不支持 /reload")
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderIdle)
        ));
        assert!(event_rx.try_recv().is_err());
    }

    #[test]
    fn slash_reload_during_a_turn_is_not_sent_to_the_model() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::Prompt {
                text: "/reload".to_string(),
                images: Vec::new(),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        assert!(
            String::from_utf8(writer.into_inner())
                .unwrap()
                .trim()
                .is_empty()
        );
        assert!(state.active_turn);
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Notice(text)) if text.contains("不支持 /reload")
        ));
    }

    #[test]
    fn compact_slash_is_only_the_exact_command() {
        assert_eq!(parse_compact_slash("/compact"), Some(None));
        assert_eq!(
            parse_compact_slash("  /compact  只保留代码变更  "),
            Some(Some("只保留代码变更".to_string()))
        );
        assert_eq!(parse_compact_slash("/compaction"), None);
        assert_eq!(parse_compact_slash("compact"), None);
    }

    #[test]
    fn slash_compact_sends_native_compact_instead_of_a_prompt() {
        let (event_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::Prompt {
                text: "/compact 聚焦 diff".to_string(),
                images: Vec::new(),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        let request: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.into_inner()).unwrap().trim()).unwrap();
        assert_eq!(request["type"], "compact");
        assert_eq!(request["customInstructions"], "聚焦 diff");
        assert!(state.active_turn);
        assert_eq!(state.compact_request_ids.len(), 1);
    }

    #[test]
    fn handoff_slash_is_only_the_exact_command() {
        assert_eq!(parse_handoff_slash("/handoff"), Some(None));
        assert_eq!(
            parse_handoff_slash("  /handoff  重点关注发布流程  "),
            Some(Some("重点关注发布流程".to_string()))
        );
        assert_eq!(parse_handoff_slash("/handoffs"), None);
        assert_eq!(parse_handoff_slash("handoff"), None);
    }

    #[test]
    fn slash_handoff_sends_native_compact_instead_of_a_prompt() {
        let (event_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::Prompt {
                text: "/handoff 关注测试".to_string(),
                images: Vec::new(),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        let request: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.into_inner()).unwrap().trim()).unwrap();
        assert_eq!(request["type"], "compact");
        assert_eq!(request["customInstructions"], "关注测试");
        assert!(state.active_turn);
        assert_eq!(state.compact_request_ids.len(), 1);
    }

    #[test]
    fn shake_slash_is_only_the_exact_command() {
        assert!(parse_shake_slash("  /shake  "));
        assert!(!parse_shake_slash("/shake 现在"));
        assert!(!parse_shake_slash("/shaker"));
        assert!(!parse_shake_slash("shake"));
    }

    #[test]
    fn slash_shake_blocked_during_active_turn() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::Prompt {
                text: "  /shake  ".to_string(),
                images: Vec::new(),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        assert!(
            String::from_utf8(writer.into_inner())
                .unwrap()
                .trim()
                .is_empty()
        );
        assert!(state.active_turn);
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Notice(text)) if text.contains("结束后再")
        ));
    }

    #[test]
    fn session_commands_are_not_delivered_as_steer_or_follow_up() {
        let cases = [
            (
                ConversationCommand::Steer {
                    text: "/reload".to_string(),
                    images: Vec::new(),
                },
                None,
                Some("不支持 /reload"),
            ),
            (
                ConversationCommand::Steer {
                    text: "/compact 聚焦 diff".to_string(),
                    images: Vec::new(),
                },
                Some("compact"),
                None,
            ),
            (
                ConversationCommand::FollowUp {
                    text: "/handoff 关注测试".to_string(),
                    images: Vec::new(),
                },
                Some("compact"),
                None,
            ),
            (
                ConversationCommand::FollowUp {
                    text: "/shake".to_string(),
                    images: Vec::new(),
                },
                None,
                Some("结束后再"),
            ),
        ];
        for (command, wire_type, status_fragment) in cases {
            let (event_tx, event_rx) = smol::channel::unbounded();
            let mut state = PiState::new();
            state.active_turn = true;
            let in_flight = AtomicUsize::new(1);
            let mut writer = futures::io::Cursor::new(Vec::new());
            smol::block_on(handle_command(
                command,
                &mut writer,
                &event_tx,
                &mut state,
                &in_flight,
            ))
            .unwrap();
            let written = String::from_utf8(writer.into_inner()).unwrap();
            assert!(state.active_turn);
            if let Some(wire_type) = wire_type {
                let request: serde_json::Value = serde_json::from_str(written.trim()).unwrap();
                assert_eq!(request["type"], wire_type);
                assert_ne!(request["type"], "steer");
                assert_ne!(request["type"], "follow_up");
                assert_eq!(in_flight.load(Ordering::SeqCst), 1);
            } else {
                assert!(written.trim().is_empty());
                assert_eq!(in_flight.load(Ordering::SeqCst), 0);
                assert!(matches!(
                    event_rx.try_recv(),
                    Ok(ConversationEvent::Notice(text)) if status_fragment.is_some_and(|fragment| text.contains(fragment))
                ));
            }
        }
    }

    #[test]
    fn follow_up_queues_until_the_turn_settles() {
        let (event_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::FollowUp {
                text: "做完再总结".to_string(),
                images: Vec::new(),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        let request: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.into_inner()).unwrap().trim()).unwrap();
        assert_eq!(request["type"], "follow_up");
        assert_eq!(request["message"], "做完再总结");
        assert!(state.active_turn);
        assert_eq!(state.follow_up_request_ids.len(), 1);
    }

    #[test]
    fn cancel_closes_an_idle_turn_without_waiting_for_pi() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        let in_flight = AtomicUsize::new(0);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::Cancel,
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert!(!state.active_turn);
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::TurnEnded(StopReason::Cancelled))
        ));
    }

    #[test]
    fn cancel_closes_the_running_turn_before_pi_agrees() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::Cancel,
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert!(!state.active_turn);
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::TurnEnded(StopReason::Cancelled))
        ));
        let sent = String::from_utf8(writer.into_inner()).unwrap();
        assert!(sent.contains("\"type\":\"abort\"") || sent.contains("\"type\": \"abort\""));
    }

    #[test]
    fn cancel_clears_the_queue_before_abort() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::Cancel,
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        let sent = String::from_utf8(writer.into_inner()).unwrap();
        let mut lines = sent.lines().filter(|line| !line.is_empty());
        let clear: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        let abort: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        let state_check: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert!(lines.next().is_none());
        assert_eq!(clear["type"], "clear_queue");
        assert_eq!(abort["type"], "abort");
        assert_eq!(state_check["type"], "get_state");
        assert!(!state.active_turn);
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::TurnEnded(StopReason::Cancelled))
        ));
        let id = clear["id"].as_str().unwrap().to_string();

        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_response(
            serde_json::json!({
                "id": id,
                "type": "response",
                "command": "clear_queue",
                "success": true,
                "data": {
                    "steering": ["换个方向"],
                    "followUp": ["最后总结"]
                }
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ComposerRestore {
                revision: 1,
                texts,
                ..
            }) if texts == ["换个方向".to_string(), "最后总结".to_string()]
        ));
        assert!(String::from_utf8(writer.into_inner()).unwrap().is_empty());
        assert_eq!(in_flight.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancel_leaves_background_tasks_running() {
        let (event_tx, _event_rx) = smol::channel::unbounded();
        let (outbound_tx, _outbound_rx) = smol::channel::unbounded();
        let supervisor = crate::background_tasks::BackgroundSupervisor::start(
            "test-cancel-bg",
            None,
            event_tx.clone(),
            outbound_tx,
        )
        .unwrap();

        use std::io::{BufRead, BufReader, Write};
        let mut stream =
            crate::background_tasks::connect_task_stream(supervisor.socket_path()).unwrap();
        stream
            .write_all(b"{\"op\":\"start\",\"command\":\"sleep 30\",\"title\":\"task\"}\n")
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let started: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(started["ok"], true);

        let mut state = PiState::new();
        state.active_turn = true;
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::Cancel,
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        let mut stream = crate::background_tasks::connect_task_stream(supervisor.socket_path()).unwrap();
        let wait = serde_json::json!({"op": "wait", "id": started["id"]});
        stream.write_all(format!("{wait}\n").as_bytes()).unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let finished: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(finished["tasks"][0]["status"], "running", "{finished}");
    }

    #[test]
    fn pi_auto_retry_becomes_a_structured_retry_state() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        handle_event(
            serde_json::json!({
                "type": "auto_retry_start",
                "attempt": 2,
                "maxAttempts": 3,
                "delayMs": 4000,
                "errorMessage": " 429 rate limited "
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        match event_rx.try_recv() {
            Ok(ConversationEvent::ProviderRetry(Some(retry))) => {
                assert_eq!(retry.attempt, 2);
                assert_eq!(retry.max_attempts, 3);
                assert_eq!(retry.delay_ms, 4000);
                assert!(retry.started_at_ms > 0, "界面要据此倒数");
                assert_eq!(retry.error, "429 rate limited");
            }
            _ => panic!("expected a retry state"),
        }
        handle_event(
            serde_json::json!({"type": "auto_retry_end", "success": true, "attempt": 2}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderRetry(None))
        ));
    }

    #[test]
    fn the_user_can_stop_a_background_task_from_the_ui() {
        let (event_tx, _event_rx) = smol::channel::unbounded();
        let (outbound_tx, _outbound_rx) = smol::channel::unbounded();
        let supervisor = crate::background_tasks::BackgroundSupervisor::start(
            "test-ui-stop-bg",
            None,
            event_tx.clone(),
            outbound_tx,
        )
        .unwrap();

        use std::io::{BufRead, BufReader, Write};
        let mut stream =
            crate::background_tasks::connect_task_stream(supervisor.socket_path()).unwrap();
        stream
            .write_all(b"{\"op\":\"start\",\"command\":\"sleep 30\",\"title\":\"task\"}\n")
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let started: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        let id = started["id"].as_str().unwrap().to_string();

        let mut state = PiState::new();
        state.background = Some(supervisor.control());
        let in_flight = AtomicUsize::new(0);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::StopBackgroundTask { id: id.clone() },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert!(
            String::from_utf8(writer.into_inner()).unwrap().is_empty(),
            "停后台任务不经过 Pi"
        );

        let started_at = std::time::Instant::now();
        let status = loop {
            let mut stream = crate::background_tasks::connect_task_stream(supervisor.socket_path()).unwrap();
            let wait = serde_json::json!({"op": "wait", "id": id});
            stream.write_all(format!("{wait}\n").as_bytes()).unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            let snapshot: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            let status = snapshot["tasks"][0]["status"]
                .as_str()
                .unwrap_or("")
                .to_string();
            if status != "running" || started_at.elapsed() > std::time::Duration::from_secs(5) {
                break status;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(status, "stopped");
    }

    #[test]
    fn queue_update_during_clear_is_not_treated_as_insert() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.pending_clear = Some(("smelt-clear-queue-1".into(), true));

        handle_event(
            serde_json::json!({
                "type": "queue_update",
                "steering": [],
                "followUp": []
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );

        assert!(event_rx.try_recv().is_err(), "清空中的空队列不能当成已插入");
    }

    #[test]
    fn a_run_pi_starts_on_its_own_opens_and_settles_a_turn() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();

        // 后台任务结束，扩展带 triggerTurn 送回结果。
        handle_event(
            serde_json::json!({"type": "agent_start"}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        assert!(state.active_turn);
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderTurnStarted)
        ));
        // 同一次运行里的重试、续跑还会再报 agent_start。
        handle_event(
            serde_json::json!({"type": "agent_start"}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        assert!(event_rx.try_recv().is_err(), "一次运行只开一轮");

        let in_flight = AtomicUsize::new(0);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::Prompt {
                text: "顺便看看日志".to_string(),
                images: Vec::new(),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        let request: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.into_inner()).unwrap().trim()).unwrap();
        assert_eq!(request["type"], "steer", "Pi 正在跑，裸 prompt 会被拒");

        handle_event(
            serde_json::json!({"type": "agent_settled", "aborted": false}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        assert!(!state.active_turn);
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::TurnEnded(StopReason::EndTurn))
        ));
    }

    #[test]
    fn a_run_still_winding_down_after_stop_is_not_a_new_turn() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        state.provider_busy = true;
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::Cancel,
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::TurnEnded(StopReason::Cancelled))
        ));

        // 停止前发出的 prompt，Pi 这才开跑，随后就会被 abort。
        handle_event(
            serde_json::json!({"type": "agent_start"}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );

        assert!(!state.active_turn);
        assert!(event_rx.try_recv().is_err());
    }

    fn sent_request(writer: futures::io::Cursor<Vec<u8>>) -> serde_json::Value {
        serde_json::from_str(String::from_utf8(writer.into_inner()).unwrap().trim()).unwrap()
    }

    #[test]
    fn stopping_returns_queued_images_with_their_text() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        state.provider_busy = true;
        let in_flight = AtomicUsize::new(3);
        let image = crate::acp_chat::AcpImage::new("image/png", "aGk=");

        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::Steer {
                text: "看这张".to_string(),
                images: vec![image.clone()],
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        let steer_id = sent_request(writer)["id"].as_str().unwrap().to_string();
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::FollowUp {
                text: String::new(),
                images: vec![image.clone()],
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        let follow_up_id = sent_request(writer)["id"].as_str().unwrap().to_string();
        handle_event(
            serde_json::json!({"type": "queue_update", "steering": ["看这张"], "followUp": [""]}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        for id in [steer_id, follow_up_id] {
            smol::block_on(handle_response(
                serde_json::json!({
                    "id": id,
                    "type": "response",
                    "success": true,
                    "data": {"disposition": "queued"}
                }),
                &mut futures::io::Cursor::new(Vec::new()),
                &event_tx,
                &mut state,
                &in_flight,
            ))
            .unwrap();
        }
        while event_rx.try_recv().is_ok() {}

        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::Cancel,
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        let sent = String::from_utf8(writer.into_inner()).unwrap();
        let clear: serde_json::Value = serde_json::from_str(sent.lines().next().unwrap()).unwrap();
        while event_rx.try_recv().is_ok() {}
        smol::block_on(handle_response(
            serde_json::json!({
                "id": clear["id"],
                "type": "response",
                "command": "clear_queue",
                "success": true,
                "data": {"steering": ["看这张"], "followUp": [""]}
            }),
            &mut futures::io::Cursor::new(Vec::new()),
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        match event_rx.try_recv() {
            Ok(ConversationEvent::ComposerRestore { texts, images, .. }) => {
                assert_eq!(texts, ["看这张".to_string(), String::new()]);
                assert_eq!(images, vec![vec![image.clone()], vec![image]]);
            }
            _ => panic!("expected a composer restore"),
        }
    }

    #[test]
    fn an_image_only_steer_leaves_the_queue_once_pi_delivers_it() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        state.provider_busy = true;
        let in_flight = AtomicUsize::new(2);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::Steer {
                text: String::new(),
                images: vec![crate::acp_chat::AcpImage::new("image/png", "aGk=")],
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        handle_event(
            serde_json::json!({"type": "queue_update", "steering": [""], "followUp": []}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::PromptQueue { steering, .. }) if steering == [String::new()]
        ));

        // Pi 交给模型，但因为文字是空串，不出队也不报 queue_update。
        handle_event(
            serde_json::json!({
                "type": "message_start",
                "message": {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": ""},
                        {"type": "image", "data": "aGk=", "mimeType": "image/png"}
                    ]
                }
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::PromptQueue { steering, follow_up })
                if steering.is_empty() && follow_up.is_empty()
        ));

        // 之后 Pi 报的队列仍带着那个空串，不能再冒出来。
        handle_event(
            serde_json::json!({"type": "queue_update", "steering": ["", "再补一句"], "followUp": []}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::PromptQueue { steering, .. })
                if steering == ["再补一句".to_string()]
        ));
    }

    #[test]
    fn compaction_and_queue_events_surface_to_the_host() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.model = Some(PiModel {
            provider: "anthropic".into(),
            id: "opus".into(),
            name: "Opus".into(),
            context_window: Some(200_000),
        });

        handle_event(
            serde_json::json!({"type": "compaction_start", "reason": "threshold"}),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        handle_event(
            serde_json::json!({
                "type": "compaction_end",
                "reason": "threshold",
                "aborted": false,
                "result": {"tokensBefore": 150000, "estimatedTokensAfter": 32000}
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );
        handle_event(
            serde_json::json!({
                "type": "queue_update",
                "steering": ["先改测试"],
                "followUp": ["再提交"]
            }),
            &event_tx,
            &outbound_tx,
            &mut state,
            &launch,
        );

        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Compaction { running: true, .. })
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Compaction {
                running: false,
                used: Some(32_000),
                size: Some(200_000),
                ..
            })
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderIdle)
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::PromptQueue { steering, follow_up })
                if steering == ["先改测试".to_string()] && follow_up == ["再提交".to_string()]
        ));
    }

    /// 从 Smelt 的统一运行时入口走完受管 Bun -> Pi RPC -> Ready，
    /// 避免逻辑命令意外落回 ACP parser 却只有单元测试通过。
    #[test]
    #[ignore = "需要先编出 smelt-pi（scripts/build-smelt-pi.sh）"]
    fn managed_native_driver_reaches_ready() {
        let agent_dir =
            std::env::temp_dir().join(format!("smelt-pi-rpc-driver-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&agent_dir).unwrap();
        let mut launch = test_launch("smelt-pi-agent --offline --no-approve");
        launch.cwd = None;
        launch.launch.env.insert(
            "PI_CODING_AGENT_DIR".to_string(),
            agent_dir.to_string_lossy().into_owned(),
        );

        let handle = crate::acp_conn::spawn_agent_runtime(launch, None);
        let outcome = smol::block_on(smol::future::race(
            async {
                loop {
                    match handle.event_rx.recv().await {
                        Ok(ConversationEvent::Ready {
                            session_id,
                            kind: ReadyKind::Fresh,
                            supports_image: true,
                        }) if !session_id.0.is_empty() => return Ok(()),
                        Ok(ConversationEvent::Fatal(error)) => return Err(error),
                        Ok(ConversationEvent::RestoreFailed(_)) => {
                            return Err("fresh Pi runtime 不应进入恢复失败".to_string());
                        }
                        Ok(_) => {}
                        Err(_) => return Err("Pi RPC 事件通道在 Ready 前关闭".to_string()),
                    }
                }
            },
            async {
                smol::Timer::after(Duration::from_secs(45)).await;
                Err("Pi RPC 统一入口在 45 秒内没有 Ready".to_string())
            },
        ));
        let stopped = crate::acp_conn::shutdown_and_wait(handle, Duration::from_secs(3));
        let _ = std::fs::remove_dir_all(&agent_dir);

        assert!(stopped, "Pi RPC 子进程应被完整回收");
        outcome.unwrap();
    }

    #[test]
    #[ignore = "需要先编出 smelt-pi（scripts/build-smelt-pi.sh）"]
    fn missing_native_session_is_classified_for_fresh_fallback() {
        let agent_dir = std::env::temp_dir().join(format!(
            "smelt-pi-rpc-missing-session-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&agent_dir).unwrap();
        let mut launch = test_launch("smelt-pi-agent --offline --no-approve");
        launch.cwd = None;
        launch.launch.env.insert(
            "PI_CODING_AGENT_DIR".to_string(),
            agent_dir.to_string_lossy().into_owned(),
        );
        launch.resume_session_id =
            Some(SessionId::new(format!("missing-{}", uuid::Uuid::new_v4())));

        let handle = crate::acp_conn::spawn_agent_runtime(launch, None);
        let outcome = smol::block_on(smol::future::race(
            async {
                loop {
                    match handle.event_rx.recv().await {
                        Ok(ConversationEvent::RestoreFailed(
                            ConversationRestoreFailure::HistoryMissing,
                        )) => {
                            return Ok(());
                        }
                        Ok(ConversationEvent::RestoreFailed(
                            ConversationRestoreFailure::Failed(error),
                        )) => {
                            return Err(format!("错误地分类为普通恢复失败：{error}"));
                        }
                        Ok(ConversationEvent::Fatal(error)) => return Err(error),
                        Ok(ConversationEvent::Ready { .. }) => {
                            return Err("不存在的 Pi 历史不应 Ready".to_string());
                        }
                        Ok(_) => {}
                        Err(_) => return Err("未产生恢复失败事件".to_string()),
                    }
                }
            },
            async {
                smol::Timer::after(Duration::from_secs(45)).await;
                Err("等待 Pi 历史缺失结果超时".to_string())
            },
        ));
        let stopped = crate::acp_conn::shutdown_and_wait(handle, Duration::from_secs(3));
        let _ = std::fs::remove_dir_all(&agent_dir);

        assert!(stopped, "退出的 Pi RPC 子进程应被回收");
        outcome.unwrap();
    }

    #[test]
    fn set_model_success_publishes_the_picker_value_even_if_payload_id_differs() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.models = vec![
            PiModel {
                provider: "github-copilot".into(),
                id: "claude-opus-5".into(),
                name: "Claude Opus 5".into(),
                context_window: None,
            },
            PiModel {
                provider: "github-copilot".into(),
                id: "kimi-k3".into(),
                name: "Kimi K3".into(),
                context_window: None,
            },
        ];
        state.model = Some(state.models[0].clone());
        let in_flight = AtomicUsize::new(1);
        state.pending_configs.insert(
            "config-model-1".into(),
            PendingConfig::Model("github-copilot/kimi-k3".into()),
        );

        smol::block_on(handle_response(
            serde_json::json!({
                "id": "config-model-1",
                "type": "response",
                "command": "set_model",
                "success": true,
                "data": {
                    "id": "kimi-k3-latest",
                    "name": "Kimi K3",
                    "provider": "github-copilot"
                }
            }),
            &mut futures::io::Cursor::new(Vec::new()),
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        let ConversationEvent::Model(model) = event_rx.try_recv().unwrap() else {
            panic!("set_model 成功后必须刷新模型胶囊");
        };
        assert_eq!(model.current_value, "github-copilot/kimi-k3");
        assert_eq!(model.current_name, "Kimi K3");
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);
    }

    fn deepseek_session_with_refresh_command() -> PiState {
        let mut state = PiState::new();
        state.models = vec![PiModel {
            provider: "deepseek".into(),
            id: "deepseek-flash".into(),
            name: "DeepSeek-V4.1-Flash".into(),
            context_window: None,
        }];
        state.model = Some(state.models[0].clone());
        state
            .available_commands
            .push((SMELT_REFRESH_MODELS_COMMAND.to_string(), String::new()));
        state
    }

    /// 复现：会话启动后才在 Smelt 里加的 provider，菜单里有、Pi 不认识，直接 set_model 回
    /// Model not found，胶囊又跳回原来的模型。要先让 Pi 重读 models.json，回执到了再切。
    #[test]
    fn a_model_pi_does_not_know_yet_reloads_models_before_switching() {
        let (event_tx, _) = smol::channel::unbounded();
        let mut state = deepseek_session_with_refresh_command();
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::SetConfigOption {
                config_id: "model".to_string(),
                value: ConfigValue::Select("minmax/minimax-m3.1-flash".to_string()),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        let requests = rpc_lines(writer);
        assert_eq!(requests.len(), 1, "回执到之前不能先发 set_model");
        assert_eq!(requests[0]["type"], "prompt");
        assert_eq!(requests[0]["message"], "/smelt-refresh-models");

        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_response(
            serde_json::json!({
                "id": requests[0]["id"],
                "type": "response",
                "command": "prompt",
                "success": true,
                "data": {"disposition": "handled"}
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        let requests = rpc_lines(writer);
        assert!(requests.iter().any(|r| r["type"] == "get_available_models"));
        let set = requests
            .iter()
            .find(|r| r["type"] == "set_model")
            .expect("重读完要接着切模型");
        assert_eq!(set["provider"], "minmax");
        assert_eq!(set["modelId"], "minimax-m3.1-flash");
        assert_eq!(in_flight.load(Ordering::SeqCst), 1, "等 set_model 的回执再还账");
    }

    /// 先点了 Pi 还不认识的 A（在等重读），又点了认识的 B：B 是最后的选择，重读回执到了
    /// 不能再把 A 切回来；被顶掉的 A 那次记账要还。
    #[test]
    fn a_later_direct_switch_cancels_the_switch_waiting_for_reload() {
        let (event_tx, _) = smol::channel::unbounded();
        let mut state = deepseek_session_with_refresh_command();
        state.models.push(PiModel {
            provider: "deepseek".into(),
            id: "deepseek-v4-pro".into(),
            name: "DeepSeek-V4-Pro".into(),
            context_window: None,
        });
        let in_flight = AtomicUsize::new(2);
        let mut writer = futures::io::Cursor::new(Vec::new());
        for value in ["minmax/minimax-m3.1-flash", "deepseek/deepseek-v4-pro"] {
            smol::block_on(handle_command(
                ConversationCommand::SetConfigOption {
                    config_id: "model".to_string(),
                    value: ConfigValue::Select(value.to_string()),
                },
                &mut writer,
                &event_tx,
                &mut state,
                &in_flight,
            ))
            .unwrap();
        }
        let requests = rpc_lines(writer);
        assert_eq!(requests[1]["type"], "set_model");
        assert_eq!(requests[1]["modelId"], "deepseek-v4-pro");
        assert_eq!(in_flight.load(Ordering::SeqCst), 1, "被顶掉的 A 那次要还账");

        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_response(
            serde_json::json!({
                "id": requests[0]["id"], "type": "response", "command": "prompt",
                "success": true, "data": {"disposition": "handled"}
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert!(
            rpc_lines(writer).iter().all(|r| r["type"] != "set_model"),
            "重读回执到了不能再切回 A"
        );
    }

    #[test]
    fn a_model_pi_already_knows_switches_directly() {
        let (event_tx, _) = smol::channel::unbounded();
        let mut state = deepseek_session_with_refresh_command();
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_command(
            ConversationCommand::SetConfigOption {
                config_id: "model".to_string(),
                value: ConfigValue::Select("deepseek/deepseek-flash".to_string()),
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        let requests = rpc_lines(writer);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["type"], "set_model");
    }

    #[test]
    fn set_model_failure_uses_the_session_config_failure_prefix() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.model = Some(PiModel {
            provider: "github-copilot".into(),
            id: "claude-opus-5".into(),
            name: "Claude Opus 5".into(),
            context_window: None,
        });
        let in_flight = AtomicUsize::new(1);
        state.pending_configs.insert(
            "config-model-1".into(),
            PendingConfig::Model("github-copilot/kimi-k3".into()),
        );

        smol::block_on(handle_response(
            serde_json::json!({
                "id": "config-model-1",
                "type": "response",
                "command": "set_model",
                "success": false,
                "error": "No API key for github-copilot/kimi-k3"
            }),
            &mut futures::io::Cursor::new(Vec::new()),
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        let ConversationEvent::Status(status) = event_rx.try_recv().unwrap() else {
            panic!("失败必须写成会话配置失败，GUI 才能清掉「下轮生效」");
        };
        assert!(status.starts_with("更新会话配置失败"), "{status}");
        assert!(status.contains("No API key"));
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn rewind_command_is_gated_on_active_turn_and_duplicates() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::Rewind {
                text: "hi".to_string(),
                occurrence: 0,
                truncate_from: 0,
                then: crate::acp_conn::AfterRewind::RestoreComposer,
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert_eq!(writer.get_ref().len(), 0);
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Notice(_))
        ));
        // daemon 发命令前记的在途 RPC 拒绝时也要销账，否则热升级永远等不到它。
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);

        // 回合结束了，但一次回退已在途：同文本第二次请求直接拒绝。
        state.active_turn = false;
        in_flight.store(1, Ordering::SeqCst);
        state.pending_rewind = Some(PendingRewind {
            truncate_from: 0,
            occurrence: 0,
            text: "hi".to_string(),
            then: crate::acp_conn::AfterRewind::RestoreComposer,
            list_request_id: "smelt-rewind-list-1".to_string(),
            fork_request_id: None,
        });
        smol::block_on(handle_command(
            ConversationCommand::Rewind {
                text: "again".to_string(),
                occurrence: 0,
                truncate_from: 0,
                then: crate::acp_conn::AfterRewind::RestoreComposer,
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert_eq!(writer.get_ref().len(), 0);
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Notice(_))
        ));
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn rewind_two_phase_flow_matches_occurrence_forks_and_restores() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        // 阶段一：回退命令只发 get_fork_messages，不碰状态。
        smol::block_on(handle_command(
            ConversationCommand::Rewind {
                text: "same".to_string(),
                occurrence: 1,
                truncate_from: 3,
                then: crate::acp_conn::AfterRewind::RestoreComposer,
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        let request: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.get_ref().clone()).unwrap().trim())
                .unwrap();
        assert_eq!(request["type"], "get_fork_messages");
        let list_id = request["id"].as_str().unwrap().to_string();
        assert!(state.pending_rewind.is_some());

        // 阶段二：同文本取第 1 条（occurrence=1），必须是 e2 不是 e1。
        writer.get_mut().clear();
        writer.set_position(0);
        smol::block_on(handle_response(
            serde_json::json!({
                "id": list_id,
                "type": "response",
                "command": "get_fork_messages",
                "success": true,
                "data": {"messages": [
                    {"entryId": "e1", "text": "same"},
                    {"entryId": "e2", "text": "same"},
                    {"entryId": "e3", "text": "other"}
                ]}
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        let request: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.get_ref().clone()).unwrap().trim())
                .unwrap();
        assert_eq!(request["type"], "fork");
        assert_eq!(request["entryId"], "e2");
        let fork_id = request["id"].as_str().unwrap().to_string();

        // 阶段三：fork 成功 → 截断事件 + 原文回输入框 + 补设回退前的模型 + 补发 get_state。
        // 出错后换过模型再重试：fork 会把模型还原成那条分支当时的，必须按回退前的设回去。
        state.model = Some(PiModel {
            provider: "github-copilot".into(),
            id: "gemini-3.8-flash".into(),
            name: "Gemini 3.8 Flash".into(),
            context_window: None,
        });
        state.thinking_level = "high".into();
        writer.get_mut().clear();
        writer.set_position(0);
        smol::block_on(handle_response(
            serde_json::json!({
                "id": fork_id,
                "type": "response",
                "command": "fork",
                "success": true,
                "data": {"text": "same", "cancelled": false}
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Rewound {
                truncate_from: 3,
                then: crate::acp_conn::AfterRewind::RestoreComposer,
            })
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ComposerRestore { texts, .. })
                if texts == ["same".to_string()]
        ));
        let requests = rpc_lines(writer.clone());
        let types: Vec<_> = requests.iter().map(|r| r["type"].as_str().unwrap()).collect();
        assert_eq!(types, ["set_model", "set_thinking_level", "get_state"]);
        assert_eq!(requests[0]["provider"], "github-copilot");
        assert_eq!(requests[0]["modelId"], "gemini-3.8-flash");
        assert_eq!(requests[1]["level"], "high");
        let request = requests[2].clone();
        let state_id = request["id"].as_str().unwrap().to_string();
        // in_flight 记账挂到 get_state 回执上：热升级不得跨越身份刷新。
        assert_eq!(in_flight.load(Ordering::SeqCst), 1);

        // 阶段四：新身份落地，in_flight 归零。
        smol::block_on(handle_response(
            serde_json::json!({
                "id": state_id,
                "type": "response",
                "command": "get_state",
                "success": true,
                "data": {"sessionId": "forked-branch"}
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ProviderSessionIdChanged(sid))
                if &*sid.0 == "forked-branch"
        ));
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);
        assert!(state.pending_rewind.is_none());
        assert_eq!(state.session_id, "forked-branch");
    }

    /// 搭一个只在启动期存在的假 Pi：预设回实行流 + 永远沉默的 outbound 通道，
    /// `rpc_call` 的串行问答就能直接跑。
    fn fake_pi_lines(
        responses: Vec<serde_json::Value>,
    ) -> impl Stream<Item = std::io::Result<String>> {
        futures::stream::iter(
            responses
                .into_iter()
                .map(|value| Ok(serde_json::to_string(&value).unwrap())),
        )
    }

    #[test]
    fn resumed_initialization_replays_raw_current_branch_entries() {
        let (event_tx, event_rx) = smol::channel::unbounded::<ConversationEvent>();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded::<serde_json::Value>();
        let mut state = PiState::new();
        let mut launch = test_launch("smelt-pi-agent --offline");
        launch.resume_session_id = Some(SessionId::new("pi-session"));
        let agent_dir =
            std::env::temp_dir().join(format!("smelt-pi-resume-file-{}", uuid::Uuid::new_v4()));
        let sessions = agent_dir.join("sessions/--workspace--");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("20260101T000000_pi-session.jsonl"),
            concat!(
                "{\"type\":\"session\",\"version\":2,\"id\":\"pi-session\",\"cwd\":\"/tmp\"}\n",
                "{\"type\":\"model_change\",\"id\":\"model\",\"parentId\":null,\"provider\":\"test\",\"modelId\":\"seed-model\"}\n",
                "{\"type\":\"message\",\"id\":\"old\",\"parentId\":\"model\",\"message\":{\"role\":\"user\",\"content\":\"压缩前\"}}\n",
                "{\"type\":\"compaction\",\"id\":\"compact\",\"parentId\":\"old\",\"summary\":\"模型只看摘要\",\"firstKeptEntryId\":\"old\"}\n",
                "{\"type\":\"message\",\"id\":\"new\",\"parentId\":\"compact\",\"message\":{\"role\":\"user\",\"content\":\"压缩后\"}}\n",
            ),
        )
        .unwrap();
        let agent = agent_dir.to_string_lossy().into_owned();
        launch
            .launch
            .env
            .insert("PI_CODING_AGENT_DIR".into(), agent.clone());
        // 生产路径在 spawn 之前把会话文件读进种子，只为了尽早发布模型名。
        let seed = crate::session_history::read_pi_session_branch(
            Some(&agent),
            None,
            Some("/workspace"),
            "pi-session",
        )
        .expect("启动前能读到会话文件");
        let mut lines = fake_pi_lines(vec![
            serde_json::json!({
                "id": "smelt-init-state", "type": "response", "command": "get_state",
                "success": true,
                "data": {
                    "sessionId": "pi-session",
                    "model": {
                        "provider": "test",
                        "id": "model",
                        "name": "Test Model",
                        "contextWindow": 100_000
                    },
                    "thinkingLevel": "medium"
                }
            }),
            serde_json::json!({
                "id": "smelt-init-models", "type": "response",
                "command": "get_available_models", "success": true,
                "data": {"models": [{
                    "provider": "test", "id": "model", "name": "Test Model",
                    "contextWindow": 100_000
                }]}
            }),
            serde_json::json!({
                "id": "smelt-init-thinking", "type": "response",
                "command": "get_available_thinking_levels", "success": true,
                "data": {"levels": ["medium"]}
            }),
            serde_json::json!({
                "id": "smelt-init-commands", "type": "response",
                "command": "get_commands", "success": true,
                "data": {"commands": []}
            }),
        ]);
        let mut writer = futures::io::Cursor::new(Vec::new());
        let mut trace = HandshakeTrace {
            started: std::time::Instant::now(),
            published_model: None,
        };

        smol::block_on(initialize_session(
            &mut lines,
            &mut writer,
            &outbound_tx,
            &outbound_rx,
            &event_tx,
            &mut state,
            &launch,
            Some(seed),
            &mut trace,
        ))
        .unwrap();
        let _ = std::fs::remove_dir_all(&agent_dir);

        let requests: Vec<serde_json::Value> = String::from_utf8(writer.into_inner())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            requests
                .iter()
                .map(|request| request["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "get_state",
                "get_available_models",
                "get_available_thinking_levels",
                "get_commands",
            ]
        );
        assert!(
            requests
                .iter()
                .all(|request| request["type"] != "get_entries"),
            "恢复转录读会话文件，启动握手不再拉整棵 entry 树"
        );

        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        let replayed_user_text: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ConversationEvent::UserChunk(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(replayed_user_text, ["压缩前", "压缩后"]);
        let ready_at = events
            .iter()
            .position(|event| matches!(event, ConversationEvent::Ready { .. }))
            .expect("Ready");
        let replay_at = events
            .iter()
            .position(|event| matches!(event, ConversationEvent::HistoryReplayStarted))
            .expect("重放开始");
        assert!(replay_at < ready_at, "转录必须在 Ready 之前重放完");
        let finished_at = events
            .iter()
            .position(|event| matches!(event, ConversationEvent::HistoryReplayFinished))
            .expect("重放结束");
        assert!(finished_at < ready_at, "Ready 不能插在重放结束之前");
        let models: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ConversationEvent::Model(model) => Some(model.current_value.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(models.first().copied(), Some("test/seed-model"));
        assert!(
            models.contains(&"test/model"),
            "get_state 的模型要盖过文件里的种子模型：{models:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ConversationEvent::HistoryReplayFinished))
        );
    }

    #[test]
    fn handshake_reread_replaces_a_stale_session_seed() {
        let (event_tx, event_rx) = smol::channel::unbounded::<ConversationEvent>();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded::<serde_json::Value>();
        let mut state = PiState::new();
        let mut launch = test_launch("smelt-pi-agent --offline");
        launch.resume_session_id = Some(SessionId::new("pi-session"));
        let agent_dir =
            std::env::temp_dir().join(format!("smelt-pi-reread-{}", uuid::Uuid::new_v4()));
        let sessions = agent_dir.join("sessions/--workspace--");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("20260101T000000_pi-session.jsonl"),
            concat!(
                "{\"type\":\"session\",\"version\":2,\"id\":\"pi-session\",\"cwd\":\"/workspace\"}\n",
                "{\"type\":\"message\",\"id\":\"only\",\"parentId\":null,\"message\":{\"role\":\"user\",\"content\":\"文件里的新消息\"}}\n",
            ),
        )
        .unwrap();
        launch.launch.env.insert(
            "PI_CODING_AGENT_DIR".into(),
            agent_dir.to_string_lossy().into_owned(),
        );
        let stale = crate::session_history::PiSessionBranch {
            entries: vec![serde_json::json!({
                "type": "message",
                "id": "stale",
                "parentId": null,
                "message": {"role": "user", "content": "过期种子"}
            })],
        };
        let mut lines = fake_pi_lines(vec![
            serde_json::json!({
                "id": "smelt-init-state", "type": "response", "command": "get_state",
                "success": true,
                "data": {
                    "sessionId": "pi-session",
                    "thinkingLevel": "medium",
                    "model": {"provider": "test", "id": "live", "name": "Live"}
                }
            }),
            serde_json::json!({
                "id": "smelt-init-models", "type": "response",
                "command": "get_available_models", "success": true,
                "data": {"models": []}
            }),
            serde_json::json!({
                "id": "smelt-init-thinking", "type": "response",
                "command": "get_available_thinking_levels", "success": true,
                "data": {"levels": ["medium"]}
            }),
            serde_json::json!({
                "id": "smelt-init-commands", "type": "response",
                "command": "get_commands", "success": true,
                "data": {"commands": []}
            }),
        ]);
        let mut writer = futures::io::Cursor::new(Vec::new());
        let mut trace = HandshakeTrace {
            started: std::time::Instant::now(),
            published_model: None,
        };
        smol::block_on(initialize_session(
            &mut lines,
            &mut writer,
            &outbound_tx,
            &outbound_rx,
            &event_tx,
            &mut state,
            &launch,
            Some(stale),
            &mut trace,
        ))
        .unwrap();
        let _ = std::fs::remove_dir_all(&agent_dir);
        let mut texts = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            if let ConversationEvent::UserChunk(text) = event {
                texts.push(text);
            }
        }
        assert_eq!(texts, ["文件里的新消息"]);
        let requests: Vec<serde_json::Value> = String::from_utf8(writer.into_inner())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(
            requests
                .iter()
                .all(|request| request["type"] != "get_entries"),
            "文件能读时不向 Pi 要 get_entries"
        );
    }

    #[test]
    fn missing_session_file_replays_get_entries_before_ready() {
        let (event_tx, event_rx) = smol::channel::unbounded::<ConversationEvent>();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded::<serde_json::Value>();
        let mut state = PiState::new();
        let mut launch = test_launch("smelt-pi-agent --offline");
        launch.resume_session_id = Some(SessionId::new("missing-session"));
        let agent_dir =
            std::env::temp_dir().join(format!("smelt-pi-missing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(agent_dir.join("sessions/--workspace--")).unwrap();
        launch.launch.env.insert(
            "PI_CODING_AGENT_DIR".into(),
            agent_dir.to_string_lossy().into_owned(),
        );
        let mut lines = fake_pi_lines(vec![
            serde_json::json!({
                "id": "smelt-init-state", "type": "response", "command": "get_state",
                "success": true,
                "data": {
                    "sessionId": "missing-session",
                    "thinkingLevel": "medium",
                    "model": {"provider": "test", "id": "live", "name": "Live"}
                }
            }),
            serde_json::json!({
                "id": "smelt-init-models", "type": "response",
                "command": "get_available_models", "success": true,
                "data": {"models": []}
            }),
            serde_json::json!({
                "id": "smelt-init-thinking", "type": "response",
                "command": "get_available_thinking_levels", "success": true,
                "data": {"levels": ["medium"]}
            }),
            serde_json::json!({
                "id": "smelt-init-commands", "type": "response",
                "command": "get_commands", "success": true,
                "data": {"commands": []}
            }),
            serde_json::json!({
                "id": "smelt-init-entries", "type": "response", "command": "get_entries",
                "success": true,
                "data": {
                    "leafId": "assistant",
                    "entries": [
                        {
                            "type": "message", "id": "user", "parentId": null,
                            "message": {"role": "user", "content": "文件里没有的消息"}
                        },
                        {
                            "type": "message", "id": "assistant", "parentId": "user",
                            "message": {"role": "assistant", "content": [{"type": "text", "text": "回过"}]}
                        }
                    ]
                }
            }),
        ]);
        let mut writer = futures::io::Cursor::new(Vec::new());
        let mut trace = HandshakeTrace {
            started: std::time::Instant::now(),
            published_model: None,
        };
        smol::block_on(initialize_session(
            &mut lines,
            &mut writer,
            &outbound_tx,
            &outbound_rx,
            &event_tx,
            &mut state,
            &launch,
            None,
            &mut trace,
        ))
        .unwrap();
        let _ = std::fs::remove_dir_all(&agent_dir);
        let requests: Vec<serde_json::Value> = String::from_utf8(writer.into_inner())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(
            requests
                .iter()
                .any(|request| request["type"] == "get_entries"),
            "文件不在时必须改问 Pi"
        );
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        let texts: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                ConversationEvent::UserChunk(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["文件里没有的消息"]);
        let ready_at = events
            .iter()
            .position(|event| matches!(event, ConversationEvent::Ready { .. }))
            .expect("Ready");
        let finished_at = events
            .iter()
            .position(|event| matches!(event, ConversationEvent::HistoryReplayFinished))
            .expect("重放结束");
        assert!(finished_at < ready_at);
    }

    #[test]
    fn header_only_session_file_is_an_empty_transcript() {
        let (event_tx, event_rx) = smol::channel::unbounded::<ConversationEvent>();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded::<serde_json::Value>();
        let mut state = PiState::new();
        let mut launch = test_launch("smelt-pi-agent --offline");
        launch.resume_session_id = Some(SessionId::new("empty-session"));
        let agent_dir =
            std::env::temp_dir().join(format!("smelt-pi-empty-{}", uuid::Uuid::new_v4()));
        let sessions = agent_dir.join("sessions/--workspace--");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join("empty-session.jsonl"),
            "{\"type\":\"session\",\"version\":2,\"id\":\"empty-session\",\"cwd\":\"/workspace\"}
",
        )
        .unwrap();
        launch.launch.env.insert(
            "PI_CODING_AGENT_DIR".into(),
            agent_dir.to_string_lossy().into_owned(),
        );
        let mut lines = fake_pi_lines(vec![
            serde_json::json!({
                "id": "smelt-init-state", "type": "response", "command": "get_state",
                "success": true,
                "data": {
                    "sessionId": "empty-session",
                    "thinkingLevel": "medium",
                    "model": {"provider": "test", "id": "live", "name": "Live"}
                }
            }),
            serde_json::json!({
                "id": "smelt-init-models", "type": "response",
                "command": "get_available_models", "success": true,
                "data": {"models": []}
            }),
            serde_json::json!({
                "id": "smelt-init-thinking", "type": "response",
                "command": "get_available_thinking_levels", "success": true,
                "data": {"levels": ["medium"]}
            }),
            serde_json::json!({
                "id": "smelt-init-commands", "type": "response",
                "command": "get_commands", "success": true,
                "data": {"commands": []}
            }),
        ]);
        let mut writer = futures::io::Cursor::new(Vec::new());
        let mut trace = HandshakeTrace {
            started: std::time::Instant::now(),
            published_model: None,
        };
        smol::block_on(initialize_session(
            &mut lines,
            &mut writer,
            &outbound_tx,
            &outbound_rx,
            &event_tx,
            &mut state,
            &launch,
            None,
            &mut trace,
        ))
        .unwrap();
        let _ = std::fs::remove_dir_all(&agent_dir);
        let requests = String::from_utf8(writer.into_inner()).unwrap();
        assert!(!requests.contains("get_entries"));
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ConversationEvent::Ready { .. }))
        );
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, ConversationEvent::UserChunk(_)))
        );
        // 空转录也要成对发开始和结束，而且在 Ready 之前：投影靠它清掉旧消息。
        let position = |want: fn(&ConversationEvent) -> bool| events.iter().position(want);
        let started = position(|event| matches!(event, ConversationEvent::HistoryReplayStarted))
            .expect("空转录也要发重放开始");
        let finished = position(|event| matches!(event, ConversationEvent::HistoryReplayFinished))
            .expect("空转录也要发重放结束");
        let ready = position(|event| matches!(event, ConversationEvent::Ready { .. })).unwrap();
        assert!(started < finished && finished < ready);
    }

    #[test]
    fn fork_cut_at_startup_forks_before_the_cut_message() {
        let (event_tx, _event_rx) = smol::channel::unbounded::<ConversationEvent>();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded::<serde_json::Value>();
        let mut state = PiState::new();
        let launch = test_launch("smelt-pi-agent --offline");
        let cut = AcpForkCut {
            text: "同文".to_string(),
            occurrence: 1,
        };
        let mut lines = fake_pi_lines(vec![
            serde_json::json!({
                "id": "smelt-fork-cut-list", "type": "response", "command": "get_fork_messages",
                "success": true,
                "data": {"messages": [
                    {"entryId": "e1", "text": "同文"},
                    {"entryId": "e2", "text": "同文"},
                    {"entryId": "e3", "text": "别的"}
                ]}
            }),
            serde_json::json!({
                "id": "smelt-fork-cut", "type": "response", "command": "fork",
                "success": true, "data": {"cancelled": false}
            }),
        ]);
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(apply_fork_cut(
            &mut lines,
            &mut writer,
            &outbound_tx,
            &outbound_rx,
            &event_tx,
            &mut state,
            &launch,
            &cut,
        ))
        .unwrap();
        // 两阶段请求都发出：先列表定位，再 fork 到 e2（同文本第 1 条）。
        let requests: Vec<serde_json::Value> = String::from_utf8(writer.get_ref().clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(requests[0]["type"], "get_fork_messages");
        assert_eq!(requests[1]["type"], "fork");
        assert_eq!(requests[1]["entryId"], "e2");
    }

    #[test]
    fn fork_cut_at_startup_fails_when_message_missing_or_cancelled() {
        // 切点消息不在 agent 列表上：报错，不发 fork。
        let (event_tx, _event_rx) = smol::channel::unbounded::<ConversationEvent>();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded::<serde_json::Value>();
        let mut state = PiState::new();
        let launch = test_launch("smelt-pi-agent --offline");
        let cut = AcpForkCut {
            text: "不存在".to_string(),
            occurrence: 0,
        };
        let mut lines = fake_pi_lines(vec![serde_json::json!({
            "id": "smelt-fork-cut-list", "type": "response", "command": "get_fork_messages",
            "success": true,
            "data": {"messages": [{"entryId": "e1", "text": "别的"}]}
        })]);
        let mut writer = futures::io::Cursor::new(Vec::new());
        let error = smol::block_on(apply_fork_cut(
            &mut lines,
            &mut writer,
            &outbound_tx,
            &outbound_rx,
            &event_tx,
            &mut state,
            &launch,
            &cut,
        ))
        .unwrap_err();
        assert!(error.contains("找不到"), "{error}");
        // 只发出了 get_fork_messages 一行：切点配不上绝不能发 fork。
        let text = String::from_utf8(writer.get_ref().clone()).unwrap();
        assert_eq!(text.lines().count(), 1);

        // 扩展取消：同样如实报错。
        let (event_tx, _event_rx) = smol::channel::unbounded::<ConversationEvent>();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded::<serde_json::Value>();
        let mut state = PiState::new();
        let cut = AcpForkCut {
            text: "在的".to_string(),
            occurrence: 0,
        };
        let mut lines = fake_pi_lines(vec![
            serde_json::json!({
                "id": "smelt-fork-cut-list", "type": "response", "command": "get_fork_messages",
                "success": true,
                "data": {"messages": [{"entryId": "e1", "text": "在的"}]}
            }),
            serde_json::json!({
                "id": "smelt-fork-cut", "type": "response", "command": "fork",
                "success": true, "data": {"cancelled": true}
            }),
        ]);
        let mut writer = futures::io::Cursor::new(Vec::new());
        let error = smol::block_on(apply_fork_cut(
            &mut lines,
            &mut writer,
            &outbound_tx,
            &outbound_rx,
            &event_tx,
            &mut state,
            &launch,
            &cut,
        ))
        .unwrap_err();
        assert!(error.contains("取消"), "{error}");
    }

    /// 重试：fork 成功后只报 `Rewound`（带着要重发的原文和附图），不把原文还回输入框；
    /// 再发一次由 daemon 走普通发送，这里不自己写 `prompt`。
    #[test]
    fn retry_rewind_hands_the_message_back_for_resend_instead_of_the_composer() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());
        let image = crate::acp_conn::PromptImage::new("image/png", "aGk=");
        state.pending_rewind = Some(PendingRewind {
            truncate_from: 2,
            occurrence: 0,
            text: "分析一下".to_string(),
            then: crate::acp_conn::AfterRewind::Resend {
                text: "分析一下".to_string(),
                images: vec![image],
            },
            list_request_id: "smelt-rewind-list-1".to_string(),
            fork_request_id: Some("smelt-rewind-fork-2".to_string()),
        });

        smol::block_on(handle_response(
            serde_json::json!({
                "id": "smelt-rewind-fork-2",
                "type": "response",
                "command": "fork",
                "success": true,
                "data": {"text": "分析一下", "cancelled": false}
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        let Ok(ConversationEvent::Rewound {
            truncate_from: 2,
            then: crate::acp_conn::AfterRewind::Resend { text, images },
        }) = event_rx.try_recv()
        else {
            panic!("重试的回退要把原文和附图原样带回去");
        };
        assert_eq!(text, "分析一下");
        assert_eq!(images.len(), 1);
        assert!(
            event_rx.try_recv().is_err(),
            "重试不能把原文还回输入框，否则用户还得再点一次发送"
        );
        let requests = rpc_lines(writer);
        assert!(
            requests.iter().all(|request| request["type"] != "prompt"),
            "重发归 daemon，驱动不自己写 prompt"
        );
        assert_eq!(requests.last().unwrap()["type"], "get_state");
    }

    #[test]
    fn rewind_aborts_when_the_message_is_not_on_the_agent_branch() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
        let in_flight = AtomicUsize::new(1);
        let mut writer = futures::io::Cursor::new(Vec::new());

        smol::block_on(handle_command(
            ConversationCommand::Rewind {
                text: "gone".to_string(),
                occurrence: 0,
                truncate_from: 2,
                then: crate::acp_conn::AfterRewind::RestoreComposer,
            },
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        let request: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.get_ref().clone()).unwrap().trim())
                .unwrap();
        let list_id = request["id"].as_str().unwrap().to_string();

        // agent 分支上没有这条消息（比如 /compact 条目只存在于本地投影）：
        // 如实报错，不猜最近似，不发出 fork。
        writer.get_mut().clear();
        writer.set_position(0);
        smol::block_on(handle_response(
            serde_json::json!({
                "id": list_id,
                "type": "response",
                "command": "get_fork_messages",
                "success": true,
                "data": {"messages": [{"entryId": "e1", "text": "other"}]}
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert_eq!(writer.get_ref().len(), 0);
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Notice(_))
        ));
        assert!(state.pending_rewind.is_none());
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);
    }
}
