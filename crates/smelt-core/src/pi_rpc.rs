//! Pi 原生 RPC 驱动。
//!
//! Pi 不实现 ACP；它为 IDE/桌面应用提供 `--mode rpc` JSONL 协议。本模块直接
//! 驱动该协议，再把 Pi 事件归一化到 Smelt 现有的会话状态机。`ConversationEvent` 等类型名
//! 是历史 wire 兼容名，不代表这里向 Pi 发送过任何 ACP 消息。

use std::collections::{BTreeMap, HashMap, HashSet};
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
};
use crate::agent_kind::{ConversationLaunchSpec, SMELT_PI_AGENT_COMMAND};

const PI_RPC_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const SMELT_PERMISSION_TITLE: &str = "smelt.permission.v1";
const SMELT_CONTEXT_USAGE_WIDGET: &str = "smelt-context-usage";
const SMELT_RUNTIME_DEBUG_WIDGET: &str = "smelt-runtime-debug";

#[derive(Clone, Debug)]
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
    available_commands: Vec<(String, String)>,
    active_turn: bool,
    cancel_requested: bool,
    last_stop_reason: Option<String>,
    last_error: Option<String>,
    prompt_request_id: Option<String>,
    prompt_state_request_id: Option<String>,
    /// 已发出、等 Pi 回执的 steer 请求。只用来把 `in_flight_rpc` 记账还回去；
    /// steer 不开新回合，不能碰 `prompt_request_id`。
    steer_request_ids: HashSet<String>,
    follow_up_request_ids: HashSet<String>,
    compact_request_ids: HashSet<String>,
    reload_request_ids: HashSet<String>,
    /// `reload` 成功后补发的 `get_commands`。技能和提示词可能刚变过。
    commands_refresh_id: Option<String>,
    /// `clear_queue` 回执后是否要把 in_flight 还回去（ClearQueue 动作才要）。
    pending_clear: Option<(String, bool)>,
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
            let runtime = crate::acp_conn::sync_managed_pi_agent(&|message| {
                let _ = event_tx.try_send(ConversationEvent::Status(message.to_string()));
            });
            let runtime = match runtime {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = event_tx.try_send(ConversationEvent::Fatal(error));
                    return;
                }
            };
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
                if result.is_err()
                    && let Some(drain) = stderr_drain.lock().unwrap().take()
                {
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
    runtime: crate::acp_conn::ManagedPiRuntime,
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
    // `runtime` 在同步完成、全局锁释放前已经取得 shared generation lease；将它持有
    // 到本连接结束，消除 prepare 与 spawn 之间被升级 GC 删除目录的窗口。
    let process_args = runtime.process_args(trailing);
    let (child_stdin, child_stdout, child_stderr, child) = {
        let _spawn_permit = spawn_gate.as_ref().map(|gate| gate.read().unwrap());
        let mut stdio = stdio_out.lock().unwrap();
        if shutdown_requested.load(Ordering::SeqCst) {
            return Ok(());
        }
        let spawned = spawn_process(launch, inline_env, process_args)?;
        *stdio = Some(AcpStdio {
            pid: spawned.3.id() as i32,
            stdin_fd: spawned.0.as_raw_fd(),
            stdout_fd: spawned.1.as_raw_fd(),
        });
        spawned
    };
    let mut process = PiProcessGuard::new(child);
    *stderr_drain.lock().unwrap() = Some(spawn_stderr_drain(child_stderr, stderr_tail));

    let result = async {
        let mut writer = child_stdin;
        let mut lines = futures::io::BufReader::new(child_stdout).lines();
        let (outbound_tx, outbound_rx) = smol::channel::unbounded::<serde_json::Value>();
        let mut state = PiState::new();
        let initialize = initialize_session(
            &mut lines,
            &mut writer,
            &outbound_tx,
            &outbound_rx,
            &event_tx,
            &mut state,
            launch,
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
                Next::Outbound(Ok(message)) => write_rpc(&mut writer, &message).await?,
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
    // 必须先确认直属子进程已退出，再让 `runtime` 的 generation lease 析构。
    // 仅发送 SIGKILL 不等于进程已经消失，期间原地修复会与旧进程并发读模块树。
    process.kill_and_reap().await;
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

fn spawn_process(
    launch: &ConversationLaunch,
    inline_env: BTreeMap<String, String>,
    mut process_args: Vec<String>,
) -> Result<SpawnedPi, String> {
    if process_args.len() < 2 {
        return Err("Pi 受管运行时启动参数不完整".to_string());
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
    command.env("PATH", crate::agent_kind::prepend_dsh_cli_shim(&path));
    command.envs(environment);
    // 会话身份与 capability token 必须由 daemon 决定；用户在旧式命令
    // 前缀或配置 env 中写了同名变量也不能覆盖。
    command.env("SMELT_SESSION_ID", &launch.sid);
    command.env("SMELT_AGENT_TOKEN", &launch.agent_token);
    command.env(
        "SMELT_SOCK",
        crate::daemon_state::smeltd_sock_path()
            .to_string_lossy()
            .as_ref(),
    );
    if let Some(cwd) = launch.cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()) {
        command.current_dir(cwd);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut command = async_process::Command::from(command);
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法启动 Pi RPC：{error}"))?;
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

    fn kill_process_tree(&mut self) {
        unsafe {
            libc::kill(-self.pid, libc::SIGKILL);
        }
        // 子进程理论上仍是组长；即便它异常改了进程组，也要保证直属 child 会死，
        // 否则下面的 status/try_status 会永久等住，generation lease 也永不释放。
        let _ = self.child.kill();
    }

    async fn kill_and_reap(&mut self) {
        self.kill_process_tree();
        if self.child.status().await.is_ok() {
            self.reaped = true;
        }
    }
}

impl Drop for PiProcessGuard {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        // 正常路径由 `kill_and_reap` 异步回收。这里只覆盖 panic/未来提前返回，
        // 仍须在 generation lease 释放前同步确认主子进程已经退出。
        self.kill_process_tree();
        loop {
            match self.child.try_status() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    self.kill_process_tree();
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => break,
                Err(_) => {
                    self.kill_process_tree();
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn initialize_session<R, W>(
    lines: &mut R,
    writer: &mut W,
    outbound_tx: &smol::channel::Sender<serde_json::Value>,
    outbound_rx: &smol::channel::Receiver<serde_json::Value>,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    launch: &ConversationLaunch,
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
        "smelt-init-state",
        serde_json::json!({"type": "get_state"}),
    )
    .await?;
    apply_state_response(state, response_data(&response)?)?;

    // 分叉副本的切点：必须在重放历史之前切分支（切完 `get_state` 才是分支的
    // session id）；放在首个 `get_state` 之后，先用回执确认进程活着。
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
        // fork 后 Pi 已切到新 session 文件，补一次 get_state 拿分支身份。
        let response = rpc_call(
            lines,
            writer,
            outbound_tx,
            outbound_rx,
            event_tx,
            state,
            launch,
            "smelt-init-state",
            serde_json::json!({"type": "get_state"}),
        )
        .await?;
        apply_state_response(state, response_data(&response)?)?;
    }

    let response = rpc_call(
        lines,
        writer,
        outbound_tx,
        outbound_rx,
        event_tx,
        state,
        launch,
        "smelt-init-models",
        serde_json::json!({"type": "get_available_models"}),
    )
    .await?;
    state.models = response_data(&response)?["models"]
        .as_array()
        .map(|models| models.iter().filter_map(parse_model).collect())
        .unwrap_or_default();

    if let Ok(settings) = crate::pi_model_settings::load_pi_model_settings() {
        for cp in settings.custom_providers {
            for m in cp.models {
                if !state
                    .models
                    .iter()
                    .any(|existing| existing.provider == cp.id && existing.id == m.id)
                {
                    state.models.push(PiModel {
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

    let force_agent_model = agent_model_from_launch(launch);
    if force_agent_model.is_some()
        || state
            .model
            .as_ref()
            .is_none_or(|m| m.provider == "unknown" || m.id == "unknown")
    {
        let default_target = force_agent_model
            .or_else(|| {
                crate::pi_model_settings::load_pi_model_settings()
                    .ok()
                    .and_then(|settings| {
                        let p = settings.default_model.provider.trim().to_string();
                        let m = settings.default_model.model.trim().to_string();
                        if !p.is_empty() && !m.is_empty() {
                            Some((p, m))
                        } else {
                            None
                        }
                    })
            })
            .or_else(|| {
                state
                    .models
                    .first()
                    .map(|m| (m.provider.clone(), m.id.clone()))
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
                && let Some(m) = res.get("data").and_then(parse_model)
            {
                state.model = Some(m);
            }
            if state.model.is_none() {
                if let Some(m) = state
                    .models
                    .iter()
                    .find(|m| m.provider == provider && m.id == model_id)
                {
                    state.model = Some(m.clone());
                } else {
                    state.model = Some(PiModel {
                        provider: provider.clone(),
                        id: model_id.clone(),
                        name: model_id,
                        context_window: None,
                    });
                }
            }
        }
    }

    let response = rpc_call(
        lines,
        writer,
        outbound_tx,
        outbound_rx,
        event_tx,
        state,
        launch,
        "smelt-init-thinking",
        serde_json::json!({"type": "get_available_thinking_levels"}),
    )
    .await?;
    state.thinking_levels = response_data(&response)?["levels"]
        .as_array()
        .map(|levels| {
            levels
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let response = rpc_call(
        lines,
        writer,
        outbound_tx,
        outbound_rx,
        event_tx,
        state,
        launch,
        "smelt-init-commands",
        serde_json::json!({"type": "get_commands"}),
    )
    .await?;
    state.available_commands = parse_pi_commands(response_data(&response)?);

    let resumed = launch.resume_session_id.is_some() || launch.fork_session_id.is_some();
    if resumed {
        let _ = event_tx.try_send(ConversationEvent::HistoryReplayStarted);
        let response = rpc_call(
            lines,
            writer,
            outbound_tx,
            outbound_rx,
            event_tx,
            state,
            launch,
            "smelt-init-messages",
            serde_json::json!({"type": "get_messages"}),
        )
        .await?;
        if let Some(messages) = response_data(&response)?["messages"].as_array() {
            crate::app_log::info(
                "pi-rpc",
                &format!("会话 {} 开始重放 {} 条历史消息", launch.sid, messages.len()),
            );
            replay_history(messages, event_tx, state);
            crate::app_log::info("pi-rpc", &format!("会话 {} 历史重放完成", launch.sid));
        }
    }

    publish_configuration(state, event_tx);
    let _ = event_tx.try_send(ConversationEvent::SessionControls {
        compaction: true,
        native_queue: true,
        rewind: true,
    });
    let _ = event_tx.try_send(ConversationEvent::AvailableCommands(
        state.available_commands.clone(),
    ));
    if let Some(title) = state.session_name.clone() {
        let _ = event_tx.try_send(ConversationEvent::SessionTitle(Some(title)));
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
            "会话 {} 握手完成，已发出 Ready（resumed={resumed}）",
            launch.sid
        ),
    );
    Ok(())
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
            Wait::Outbound(Ok(message)) => write_rpc(writer, &message).await?,
            Wait::Outbound(Err(_)) => return Err("Pi RPC 回执通道已关闭".to_string()),
            Wait::Line(Some(Ok(line))) => {
                let value = parse_rpc_line(&line)?;
                if value.get("type").and_then(serde_json::Value::as_str) == Some("response") {
                    if value.get("id").and_then(serde_json::Value::as_str) == Some(id) {
                        ensure_response_success(&value)?;
                        crate::app_log::info(
                            "pi-rpc",
                            &format!(
                                "会话 {} 握手 {id} 用时 {:?}（其间 {events} 条事件）",
                                launch.sid,
                                started.elapsed()
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

/// `set_model` 成功后必须让 `current_value` 等于用户点的 `provider/id`。
/// Pi 回包有时是解析后的 pinned id，对不上 picker 里的值，pending 就永远清不掉，
/// 之后每一轮都会显示「下轮生效」。
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

fn publish_configuration(state: &PiState, event_tx: &smol::channel::Sender<ConversationEvent>) {
    let effective_model = state
        .model
        .as_ref()
        .filter(|m| m.provider != "unknown" && m.id != "unknown")
        .or_else(|| state.models.first());

    if let Some(current) = effective_model {
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

    let mut levels = state.thinking_levels.clone();
    if !levels.iter().any(|level| level == &state.thinking_level) {
        levels.push(state.thinking_level.clone());
    }
    let permission_name = if state.permission_mode == "bypassPermissions" {
        "完全访问"
    } else {
        "逐次审批"
    };
    let _ = event_tx.try_send(ConversationEvent::ConfigOptions(vec![
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
    ]));
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

fn json_string_list(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
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

async fn handle_command<W: AsyncWrite + Unpin>(
    command: ConversationCommand,
    writer: &mut W,
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    in_flight_rpc: &AtomicUsize,
) -> Result<(), String> {
    match command {
        ConversationCommand::Prompt { text, images } => {
            if parse_reload_slash(&text) {
                if state.active_turn {
                    finish_in_flight(in_flight_rpc);
                    let _ = event_tx.try_send(ConversationEvent::Status(
                        "回合进行中，结束后再 /reload".to_string(),
                    ));
                    return Ok(());
                }
                return send_reload(writer, state).await;
            }
            if let Some(instructions) = parse_compact_slash(&text) {
                return send_compact(instructions, writer, state).await;
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
        }
        ConversationCommand::Steer { text, images } => {
            // steer 只对正在跑的回合有意义。回合已经结束时降级成普通 prompt，
            // 否则这条消息会被 Pi 丢掉（用户看得到回显却永远等不到回复）。
            if !state.active_turn {
                return Box::pin(handle_command(
                    ConversationCommand::Prompt { text, images },
                    writer,
                    event_tx,
                    state,
                    in_flight_rpc,
                ))
                .await;
            }
            let id = state.request_id("steer");
            state.steer_request_ids.insert(id.clone());
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
            if !state.active_turn {
                return Box::pin(handle_command(
                    ConversationCommand::Prompt { text, images },
                    writer,
                    event_tx,
                    state,
                    in_flight_rpc,
                ))
                .await;
            }
            let id = state.request_id("follow-up");
            state.follow_up_request_ids.insert(id.clone());
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
        } => {
            if state.active_turn {
                let _ = event_tx.try_send(ConversationEvent::Status(
                    "回退失败：回合还在运行，请先停止".to_string(),
                ));
                return Ok(());
            }
            if state.pending_rewind.is_some() {
                let _ = event_tx.try_send(ConversationEvent::Status(
                    "回退失败：已有一次回退在进行中".to_string(),
                ));
                return Ok(());
            }
            let list_id = state.request_id("rewind-list");
            state.pending_rewind = Some(PendingRewind {
                truncate_from,
                occurrence,
                text,
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
            state.cancel_requested = true;
            // 先清队列再 abort，但两者必须连续写出。如果等 clear_queue 回执
            // 再 abort，Pi 还在跑模型时 Stop 会一直没反应。
            send_clear_queue(writer, state, false).await?;
            let abort_id = state.request_id("abort");
            write_rpc(
                writer,
                &serde_json::json!({"id": abort_id, "type": "abort"}),
            )
            .await?;
        }
        ConversationCommand::SetConfigOption { config_id, value } => {
            handle_config_command(config_id, value, writer, event_tx, state, in_flight_rpc).await?;
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

async fn send_reload<W: AsyncWrite + Unpin>(
    writer: &mut W,
    state: &mut PiState,
) -> Result<(), String> {
    let id = state.request_id("reload");
    state.reload_request_ids.insert(id.clone());
    write_rpc(writer, &serde_json::json!({"id": id, "type": "reload"})).await
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

fn emit_queue_restore(
    event_tx: &smol::channel::Sender<ConversationEvent>,
    state: &mut PiState,
    steering: Vec<String>,
    follow_up: Vec<String>,
) {
    let mut texts = steering;
    texts.extend(follow_up);
    state.composer_restore_seq = state.composer_restore_seq.saturating_add(1);
    // 只发还原，不清空队列当「已插入」。否则 apply 会把撤回的 steering
    // 写成会话气泡，和还回输入框叠在一起。
    let _ = event_tx.try_send(ConversationEvent::ComposerRestore {
        revision: state.composer_restore_seq,
        texts,
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
        "model" => {
            let Some((provider, model_id)) = value.split_once('/') else {
                finish_in_flight(in_flight_rpc);
                let _ = event_tx.try_send(ConversationEvent::Status(config_update_failure_status(
                    "Pi 模型值格式无效",
                )));
                return Ok(());
            };
            let provider = provider.to_string();
            let model_id = model_id.to_string();
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
            .await?;
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
        if let Err(error) = ensure_response_success(&response) {
            finish_turn(state, event_tx, Some(error));
        } else if state.active_turn {
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
        return Ok(());
    }
    if state.prompt_state_request_id.as_deref() == Some(&id) {
        state.prompt_state_request_id = None;
        match ensure_response_success(&response) {
            Ok(()) => {
                let streaming = response
                    .get("data")
                    .and_then(|data| data.get("isStreaming"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                if state.active_turn && !streaming {
                    finish_turn(state, event_tx, None);
                }
            }
            Err(error) => finish_turn(state, event_tx, Some(error)),
        }
        return Ok(());
    }
    if state.steer_request_ids.remove(&id) {
        if let Err(error) = ensure_response_success(&response) {
            let _ = event_tx.try_send(ConversationEvent::Status(format!("插入消息失败：{error}")));
        }
        finish_in_flight(in_flight_rpc);
        return Ok(());
    }
    if state.follow_up_request_ids.remove(&id) {
        if let Err(error) = ensure_response_success(&response) {
            let _ = event_tx.try_send(ConversationEvent::Status(format!(
                "排队后续消息失败：{error}"
            )));
        }
        finish_in_flight(in_flight_rpc);
        return Ok(());
    }
    if state.compact_request_ids.remove(&id) {
        if let Err(error) = ensure_response_success(&response) {
            let _ = event_tx.try_send(ConversationEvent::Status(format!(
                "压缩上下文失败：{error}"
            )));
        }
        finish_in_flight(in_flight_rpc);
        return Ok(());
    }
    if state.reload_request_ids.remove(&id) {
        match ensure_response_success(&response) {
            Ok(()) => {
                let _ = event_tx.try_send(ConversationEvent::Status(
                    "已重新加载技能、扩展、提示词和上下文".to_string(),
                ));
                let refresh_id = state.request_id("commands");
                state.commands_refresh_id = Some(refresh_id.clone());
                write_rpc(
                    writer,
                    &serde_json::json!({"id": refresh_id, "type": "get_commands"}),
                )
                .await?;
            }
            Err(error) => {
                let _ =
                    event_tx.try_send(ConversationEvent::Status(format!("重新加载失败：{error}")));
            }
        }
        finish_in_flight(in_flight_rpc);
        return Ok(());
    }
    if state.commands_refresh_id.as_deref() == Some(&id) {
        state.commands_refresh_id = None;
        if let Ok(data) = response_data(&response) {
            state.available_commands = parse_pi_commands(data);
            let _ = event_tx.try_send(ConversationEvent::AvailableCommands(
                state.available_commands.clone(),
            ));
        }
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
                let data = response.get("data").cloned().unwrap_or_default();
                emit_queue_restore(
                    event_tx,
                    state,
                    json_string_list(&data, "steering"),
                    json_string_list(&data, "followUp"),
                );
            }
            Err(error) => {
                let _ =
                    event_tx.try_send(ConversationEvent::Status(format!("清空队列失败：{error}")));
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
    if let Some(pending) = state.pending_configs.remove(&id) {
        match ensure_response_success(&response) {
            Ok(()) => match pending {
                PendingConfig::Model(value) => {
                    apply_requested_pi_model(state, &value, response.get("data"));
                }
                PendingConfig::Thinking(level) => state.thinking_level = level,
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
    let _ = counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
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
/// 时保持原分支不动；成功时广播 `Rewound`（截断游标）+ `ComposerRestore`
/// （原文回输入框），再补一发 `get_state` 换 session id。
#[allow(clippy::too_many_arguments)]
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
    let truncate_from = pending.truncate_from;
    let _ = event_tx.try_send(ConversationEvent::Rewound { truncate_from });
    let prefill = data
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&pending.text)
        .to_string();
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
        emit_queue_restore(event_tx, state, vec![prefill], Vec::new());
        finish_in_flight(in_flight_rpc);
    } else {
        emit_queue_restore(event_tx, state, vec![prefill], Vec::new());
    }
    Ok(())
}

fn abort_rewind(
    event_tx: &smol::channel::Sender<ConversationEvent>,
    in_flight_rpc: &AtomicUsize,
    message: String,
) {
    let _ = event_tx.try_send(ConversationEvent::Status(message));
    finish_in_flight(in_flight_rpc);
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
    state.prompt_request_id = None;
    state.prompt_state_request_id = None;
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
        Some("message_end") => {
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
        }
        Some("tool_execution_update") => {
            let Some(id) = event.get("toolCallId").and_then(serde_json::Value::as_str) else {
                return;
            };
            if let Some((children, debug)) = event
                .get("partialResult")
                .and_then(|value| value.get("details"))
                .and_then(|details| subagent_children_from_details(id, details))
            {
                let _ = event_tx.try_send(ConversationEvent::ToolChildren {
                    id: id.to_string(),
                    children,
                    debug,
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
            if let Some((children, debug)) = event
                .get("result")
                .and_then(|value| value.get("details"))
                .and_then(|details| subagent_children_from_details(id, details))
            {
                let _ = event_tx.try_send(ConversationEvent::ToolChildren {
                    id: id.to_string(),
                    children,
                    debug,
                });
            }
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
            finish_turn(state, event_tx, None);
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
            handle_extension_ui(&event, event_tx, outbound_tx, state, launch)
        }
        Some("extension_error") => {
            let message = event
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Pi Extension 执行失败");
            let _ = event_tx.try_send(ConversationEvent::Status(format!(
                "Pi Extension：{message}"
            )));
        }
        Some("auto_retry_start") => {
            let _ = event_tx.try_send(ConversationEvent::Status(
                "Pi 正在重试模型请求…".to_string(),
            ));
        }
        Some("queue_update") => {
            if state.pending_clear.is_some() {
                // clear_queue 进行中：接下来会 ComposerRestore。空队列不能当成已插入。
                return;
            }
            let _ = event_tx.try_send(ConversationEvent::PromptQueue {
                steering: json_string_list(&event, "steering"),
                follow_up: json_string_list(&event, "followUp"),
            });
        }
        Some("compaction_start") => {
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

fn subagent_children_from_details(
    parent_id: &str,
    details: &serde_json::Value,
) -> Option<(
    Vec<AcpEntry>,
    BTreeMap<String, crate::acp_session::ToolCallDebug>,
)> {
    let results = details.get("results")?.as_array()?;
    if results.len() <= 1 {
        let result = results.first()?;
        return Some(entries_from_subagent_messages(
            result.get("messages").and_then(serde_json::Value::as_array),
            &format!("{parent_id}-child"),
            subagent_result_running(result),
        ));
    }
    let mut entries = Vec::with_capacity(results.len());
    let mut debug = BTreeMap::new();
    for (index, result) in results.iter().enumerate() {
        let agent = result
            .get("agent")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("agent");
        let running = subagent_result_running(result);
        let failed = subagent_result_failed(result);
        let (children, child_debug) = entries_from_subagent_messages(
            result.get("messages").and_then(serde_json::Value::as_array),
            &format!("{parent_id}-result-{index}-child"),
            running,
        );
        debug.extend(child_debug);
        entries.push(AcpEntry::ToolCall {
            id: format!("{parent_id}-result-{index}"),
            title: agent.to_string(),
            kind: ToolKind::Collaborate,
            status: if running {
                ToolCallStatus::InProgress
            } else if failed {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            },
            output: Vec::new(),
            children,
        });
    }
    Some((entries, debug))
}

fn subagent_result_running(result: &serde_json::Value) -> bool {
    match result.get("exitCode").and_then(serde_json::Value::as_i64) {
        Some(-1) => true,
        Some(_) => false,
        None => result
            .get("stopReason")
            .and_then(serde_json::Value::as_str)
            .is_none(),
    }
}

fn subagent_result_failed(result: &serde_json::Value) -> bool {
    result
        .get("exitCode")
        .and_then(serde_json::Value::as_i64)
        .is_some_and(|code| code != 0 && code != -1)
        || matches!(
            result.get("stopReason").and_then(serde_json::Value::as_str),
            Some("error" | "aborted")
        )
}

fn entries_from_subagent_messages(
    messages: Option<&Vec<serde_json::Value>>,
    id_prefix: &str,
    running: bool,
) -> (
    Vec<AcpEntry>,
    BTreeMap<String, crate::acp_session::ToolCallDebug>,
) {
    let Some(messages) = messages else {
        return (Vec::new(), BTreeMap::new());
    };
    let mut entries = Vec::new();
    let mut debug = BTreeMap::new();
    let mut tool_index = 0_usize;
    for message in messages {
        if message.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(parts) = message.get("content").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for part in parts {
            let kind = part
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            match kind {
                "text" => {
                    if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
                        if !text.is_empty() {
                            entries.push(AcpEntry::Assistant {
                                text: text.to_string(),
                                thought: false,
                            });
                        }
                    }
                }
                "thinking" => {
                    // pi 协议 thinking block 的字段是 `thinking`，不是 `text`。
                    if let Some(text) = part.get("thinking").and_then(serde_json::Value::as_str) {
                        if !text.is_empty() {
                            entries.push(AcpEntry::Assistant {
                                text: text.to_string(),
                                thought: true,
                            });
                        }
                    }
                }
                "toolCall" | "tool_call" => {
                    let name = part
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("tool");
                    let args = part.get("arguments").or_else(|| part.get("args"));
                    tool_index += 1;
                    let id = format!("{id_prefix}-{tool_index}");
                    debug.insert(
                        id.clone(),
                        crate::acp_session::ToolCallDebug {
                            name: Some(name.to_string()),
                            raw_input: args.cloned(),
                        },
                    );
                    entries.push(AcpEntry::tool_call(
                        id,
                        tool_title(name, args),
                        tool_kind(name),
                        ToolCallStatus::Completed,
                        Vec::new(),
                    ));
                }
                _ => {}
            }
        }
    }
    if running {
        for entry in entries.iter_mut().rev() {
            if let AcpEntry::ToolCall { status, .. } = entry {
                *status = ToolCallStatus::InProgress;
                break;
            }
        }
    }
    (entries, debug)
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
            (!data.is_empty()).then(|| crate::acp_chat::AcpImage {
                mime: mime.to_string(),
                data_b64: data.to_string(),
            })
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
            path: path.clone(),
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

/// 全量重放 + 显式收尾。pi 是先把 `get_messages` 重放完再握手，边界在这里就确定了；
/// 不发结束信号的话，恢复后只要用户不再发消息，`replaying_history` 就一直挂着。
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
                let args = state.tool_args.get(id).cloned();
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
            Some("custom")
                if message.get("display").and_then(serde_json::Value::as_bool) != Some(false) =>
            {
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
                        crate::acp_chat::AcpImage {
                            mime: mime.to_string(),
                            data_b64: data.to_string(),
                        },
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

fn normalized_sensitive_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_sensitive_payload_key(key: &str) -> bool {
    matches!(
        normalized_sensitive_key(key).as_str(),
        "authorization"
            | "proxyauthorization"
            | "headers"
            | "apikey"
            | "accesstoken"
            | "refreshtoken"
            | "token"
            | "clientsecret"
            | "secret"
            | "password"
            | "passwd"
            | "cookie"
            | "setcookie"
            | "credential"
            | "credentials"
    )
}

fn redact_runtime_payload(value: &mut serde_json::Value, path: &str, paths: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                redact_runtime_payload(item, &format!("{path}[{index}]"), paths);
            }
        }
        serde_json::Value::Object(fields) => {
            for (key, child) in fields {
                let child_path = format!("{path}.{key}");
                if is_sensitive_payload_key(key) {
                    *child = serde_json::Value::String("[REDACTED]".to_string());
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

fn validate_runtime_debug(debug: &mut RuntimeDebug) -> bool {
    if debug.system_prompt.as_deref().is_none_or(str::is_empty) {
        return false;
    }
    match (debug.version, debug.source.as_str()) {
        (1, "pi_before_agent_start") => debug.model_call.is_none(),
        (2, "pi_runtime_debug") => {
            if let Some(call) = &mut debug.model_call {
                if call.sequence == 0 || call.source != "pi_before_provider_request" {
                    return false;
                }
                redact_runtime_payload(&mut call.payload, "$", &mut call.redacted_paths);
            }
            true
        }
        _ => false,
    }
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
            let _ = event_tx.try_send(ConversationEvent::RuntimeDebug(debug));
        }
        _ => {}
    }
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
            if let Some(message) = request.get("message").and_then(serde_json::Value::as_str) {
                let _ = event_tx.try_send(ConversationEvent::Status(message.to_string()));
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
            agent_mcp: false,
            agent_mcp_cli_args: Vec::new(),
            resume_session_id: None,
            fork_session_id: None,
            fork_cut: None,
            resume_needs_transcript_check: false,
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
        smol::block_on(guard.kill_and_reap());

        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "guard 返回前必须已回收直属子进程"
        );
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
                images: vec![crate::acp_chat::AcpImage {
                    mime: "image/png".to_string(),
                    data_b64: "aGVsbG8=".to_string(),
                }],
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
        assert!(!state.active_turn);
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

    #[test]
    fn subagent_details_become_nested_children_instead_of_output_text() {
        let launch = test_launch(SMELT_PI_AGENT_COMMAND);
        let (event_tx, event_rx) = smol::channel::unbounded();
        let (outbound_tx, _) = smol::channel::unbounded();
        let mut state = PiState::new();
        state.active_turn = true;

        handle_event(
            serde_json::json!({
                "type": "tool_execution_start",
                "toolCallId": "sa-1",
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
                "toolCallId": "sa-1",
                "toolName": "subagent",
                "partialResult": {
                    "content": [{"type": "text", "text": "(running...)"}],
                    "details": {
                        "mode": "single",
                        "results": [{
                            "agent": "scout",
                            "exitCode": -1,
                            "messages": [{
                                "role": "assistant",
                                "content": [
                                    {"type": "thinking", "thinking": "looking"},
                                    {
                                        "type": "toolCall",
                                        "name": "read",
                                        "arguments": {"path": "README.md"}
                                    }
                                ]
                            }]
                        }]
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
            }) if id == "sa-1" && title == "find auth"
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ToolDebug {
                id,
                name: Some(name),
                raw_input: Some(args),
            }) if id == "sa-1" && name == "subagent" && args["task"] == "find auth"
        ));
        let ConversationEvent::ToolChildren {
            id,
            children,
            debug,
        } = event_rx.try_recv().unwrap()
        else {
            panic!("expected nested subagent children");
        };
        assert_eq!(id, "sa-1");
        assert_eq!(children.len(), 2);
        assert_eq!(debug["sa-1-child-1"].name.as_deref(), Some("read"));
        assert_eq!(
            debug["sa-1-child-1"].raw_input,
            Some(serde_json::json!({"path": "README.md"}))
        );
        assert!(matches!(
            &children[0],
            AcpEntry::Assistant { thought: true, text } if text == "looking"
        ));
        assert!(matches!(
            &children[1],
            AcpEntry::ToolCall {
                kind: ToolKind::Read,
                status: ToolCallStatus::InProgress,
                title,
                ..
            } if title == "README.md"
        ));
        assert!(event_rx.try_recv().is_err());
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
    fn runtime_debug_widget_publishes_exact_prompt_tools_and_redacted_model_call() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        publish_extension_widget(
            &serde_json::json!({
                "widgetKey": "smelt-runtime-debug",
                "widgetLines": [serde_json::json!({
                    "version": 2,
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
                    "modelCall": {
                        "sequence": 3,
                        "source": "pi_before_provider_request",
                        "model": {
                            "provider": "openai",
                            "id": "gpt-test",
                            "api": "responses",
                            "thinkingLevel": "high"
                        },
                        "payload": {
                            "model": "gpt-test",
                            "max_tokens": 4096,
                            "messages": [{"role": "user", "content": "hello"}],
                            "api_key": "malicious-widget-secret",
                            "nested": {"Authorization": "Bearer secret"}
                        },
                        "redactedPaths": []
                    }
                }).to_string()]
            }),
            &event_tx,
        );
        let Ok(ConversationEvent::RuntimeDebug(debug)) = event_rx.try_recv() else {
            panic!("expected runtime debug event");
        };
        assert_eq!(debug.version, 2);
        assert_eq!(debug.source, "pi_runtime_debug");
        assert_eq!(
            debug.system_prompt.as_deref(),
            Some("system line 1\n<project_context>真实上下文</project_context>")
        );
        assert_eq!(debug.tools.len(), 1);
        assert_eq!(debug.tools[0].name, "bash");
        assert_eq!(debug.tools[0].parameters["required"][0], "command");
        let call = debug.model_call.expect("model call");
        assert_eq!(call.sequence, 3);
        assert_eq!(call.model.provider.as_deref(), Some("openai"));
        assert_eq!(call.payload["max_tokens"], 4096);
        assert_eq!(call.payload["api_key"], "[REDACTED]");
        assert_eq!(call.payload["nested"]["Authorization"], "[REDACTED]");
        assert_eq!(
            call.redacted_paths,
            vec![
                "$.api_key".to_string(),
                "$.nested.Authorization".to_string()
            ]
        );
    }

    #[test]
    fn runtime_debug_widget_rejects_unknown_versions_and_empty_prompts() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        for payload in [
            serde_json::json!({
                "version": 3,
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
    fn reload_slash_is_only_the_exact_command() {
        assert!(parse_reload_slash("  /reload  "));
        assert!(!parse_reload_slash("/reload 现在"));
        assert!(!parse_reload_slash("/reloader"));
        assert!(!parse_reload_slash("reload"));
    }

    #[test]
    fn slash_reload_sends_native_reload_instead_of_a_prompt() {
        let (event_tx, event_rx) = smol::channel::unbounded();
        let mut state = PiState::new();
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

        let request: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.into_inner()).unwrap().trim()).unwrap();
        assert_eq!(request["type"], "reload");
        assert_eq!(state.reload_request_ids.len(), 1);
        assert!(event_rx.try_recv().is_err());
        assert!(state.active_turn == false);

        let id = request["id"].as_str().unwrap().to_string();
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_response(
            serde_json::json!({
                "id": id,
                "type": "response",
                "command": "reload",
                "success": true
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();

        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::Status(text)) if text.contains("已重新加载")
        ));
        let refresh: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.into_inner()).unwrap().trim()).unwrap();
        assert_eq!(refresh["type"], "get_commands");
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);

        let refresh_id = refresh["id"].as_str().unwrap();
        let mut writer = futures::io::Cursor::new(Vec::new());
        smol::block_on(handle_response(
            serde_json::json!({
                "id": refresh_id,
                "type": "response",
                "command": "get_commands",
                "success": true,
                "data": {
                    "commands": [
                        {"name": "reload", "description": "Reload skills"},
                        {"name": "skill:new", "description": "刚加载的技能"}
                    ]
                }
            }),
            &mut writer,
            &event_tx,
            &mut state,
            &in_flight,
        ))
        .unwrap();
        assert_eq!(
            state.available_commands,
            vec![
                ("reload".to_string(), "Reload skills".to_string()),
                ("skill:new".to_string(), "刚加载的技能".to_string()),
            ]
        );
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::AvailableCommands(commands))
                if commands.iter().any(|(name, _)| name == "skill:new")
        ));
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
            Ok(ConversationEvent::Status(text)) if text.contains("结束后再")
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
        assert_eq!(clear["type"], "clear_queue");
        assert_eq!(abort["type"], "abort");
        assert!(state.cancel_requested);
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
            Ok(ConversationEvent::ComposerRestore { revision: 1, texts })
                if texts == ["换个方向".to_string(), "最后总结".to_string()]
        ));
        assert!(String::from_utf8(writer.into_inner()).unwrap().is_empty());
        assert_eq!(in_flight.load(Ordering::SeqCst), 1);
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
            Ok(ConversationEvent::PromptQueue { steering, follow_up })
                if steering == ["先改测试".to_string()] && follow_up == ["再提交".to_string()]
        ));
    }

    /// 从 Smelt 的统一运行时入口走完受管 Bun -> Pi RPC -> Ready，
    /// 避免逻辑命令意外落回 ACP parser 却只有单元测试通过。
    #[test]
    #[ignore = "会写 ~/.smelt/runtime 并可能联网安装 Pi 依赖"]
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
    #[ignore = "会写 ~/.smelt/runtime 并可能联网安装 Pi 依赖"]
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
            Ok(ConversationEvent::Status(_))
        ));

        // 回合结束了，但一次回退已在途：同文本第二次请求直接拒绝。
        state.active_turn = false;
        state.pending_rewind = Some(PendingRewind {
            truncate_from: 0,
            occurrence: 0,
            text: "hi".to_string(),
            list_request_id: "smelt-rewind-list-1".to_string(),
            fork_request_id: None,
        });
        smol::block_on(handle_command(
            ConversationCommand::Rewind {
                text: "again".to_string(),
                occurrence: 0,
                truncate_from: 0,
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
            Ok(ConversationEvent::Status(_))
        ));
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

        // 阶段三：fork 成功 → 截断事件 + 原文回输入框 + 补发 get_state。
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
            Ok(ConversationEvent::Rewound { truncate_from: 3 })
        ));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ConversationEvent::ComposerRestore { texts, .. })
                if texts == ["same".to_string()]
        ));
        let request: serde_json::Value =
            serde_json::from_str(String::from_utf8(writer.get_ref().clone()).unwrap().trim())
                .unwrap();
        assert_eq!(request["type"], "get_state");
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
            Ok(ConversationEvent::Status(_))
        ));
        assert!(state.pending_rewind.is_none());
        assert_eq!(in_flight.load(Ordering::SeqCst), 0);
    }
}
