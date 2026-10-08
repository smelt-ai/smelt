//! 会话宿主里的后台任务。
//!
//! 进程归会话宿主所有，不放进 Pi 的进程组。Pi 扩展通过 Unix 套接字下命令；
//! `/reload` 只拆掉扩展，任务继续跑。会话连接结束时宿主才把任务停掉。
//! 任务什么时候结束看 shell 的 `wait`，不看管道何时关掉。shell 退出后，
//! 同一进程组里剩下的进程一起收掉。宿主被硬杀掉时来不及做这件事，父进程
//! 按任务目录里记下的进程组号来收。

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::acp_conn::ConversationEvent;
use crate::acp_session::{BackgroundTaskStatus, BackgroundTaskView};

const MAX_TASKS: usize = 12;
/// `sockaddr_un.sun_path` 在 macOS 上是 104 字节，还要留一个结尾的 NUL。
const MAX_UNIX_SOCKET_PATH: usize = 103;
const LOG_CAP: u64 = 2 * 1024 * 1024;
const OUTPUT_DEFAULT: u64 = 16 * 1024;
const OUTPUT_MAX: u64 = 64 * 1024;
const UI_TAIL: u64 = 2_000;
/// 交给模型的输出尾部。人看到的状态行只用其中最后几行。
const MODEL_TAIL: u64 = 8 * 1024;
/// 有新输出时，界面最快这么久刷新一次。没有新输出就不再醒。
const LOG_PUBLISH_GAP: Duration = Duration::from_millis(400);
/// 前台等待的上限。再长就不是这一次工具调用该守着的时间。
const MAX_WAIT_MS: u64 = 24 * 60 * 60 * 1000;
/// 任务开始时还没有监听连接。这个观察不属于任何一条后来的连接。
const UNBOUND_WATCH: u64 = 0;

struct UiNotice {
    id: String,
    title: String,
    status: String,
    exit_code: Option<i32>,
    output_tail: String,
}

#[derive(Clone)]
struct ModelNotice {
    id: String,
    title: String,
    status: String,
    exit_code: Option<i32>,
    output: String,
    output_tail: String,
    wake: bool,
}

struct LiveTask {
    id: String,
    title: String,
    command: String,
    status: BackgroundTaskStatus,
    exit_code: Option<i32>,
    started_at_ms: u64,
    finished_at_ms: Option<u64>,
    log_path: PathBuf,
    pgid: Option<i32>,
    /// 先于信号记下的取消原因。进程随后以 0 退出也保持这个状态。
    cancel: Option<BackgroundTaskStatus>,
    /// 工具还拿着结果时记下那条监听。`None` 表示没有工具在等。
    /// 监听结束只放开自己名下的任务，后来连上的不继承。
    watched_by: Option<u64>,
    /// 界面上的状态行已经发出。和模型是否收到是两件事。
    ui_published: bool,
    /// 当前监听的套接字写成功之后才算模型收到。入队、换连接都不算。
    model_delivered: bool,
    /// 正在写给这条监听。写失败就放开，让当前连接再写一次。
    delivering_to: Option<u64>,
    deadline: Option<Instant>,
}

struct State {
    tasks: Vec<LiveTask>,
    last_log_publish: Option<Instant>,
}

/// 当前这条监听，连同用来叫醒它的写端。换监听时一起换掉。
struct CurrentListen {
    id: Option<u64>,
    poke: Option<std::os::unix::net::UnixStream>,
}

/// 阻塞着等事件的写端。有任务结束、到点或要停机时写一个字节。
struct Pokes {
    accept: Option<std::os::unix::net::UnixStream>,
    waits: Vec<std::os::unix::net::UnixStream>,
}

struct Inner {
    stop: AtomicBool,
    root: PathBuf,
    socket_path: PathBuf,
    next_id: AtomicU64,
    mu: Mutex<State>,
    cv: Condvar,
    events: smol::channel::Sender<ConversationEvent>,
    outbound: smol::channel::Sender<serde_json::Value>,
    /// 当前这一条监听。新连接换掉旧的，旧线程看到自己不再是当前就退出。
    listen: Mutex<CurrentListen>,
    pokes: Mutex<Pokes>,
    next_listener: AtomicU64,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

/// 一个会话一份。`Drop` 停掉仍在跑的任务并删掉套接字。
pub struct BackgroundSupervisor {
    inner: Arc<Inner>,
}

impl BackgroundSupervisor {
    pub fn start(
        session_id: &str,
        cwd: Option<String>,
        events: smol::channel::Sender<ConversationEvent>,
        outbound: smol::channel::Sender<serde_json::Value>,
    ) -> Result<Self, String> {
        static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
        let nonce = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "smelt-bg-{}-{}-{}",
            safe_component(session_id),
            std::process::id(),
            nonce
        ));
        fs::create_dir_all(&root).map_err(|error| format!("无法创建后台任务目录：{error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&root, fs::Permissions::from_mode(0o700));
        }
        let socket_path = control_socket_path(std::env::temp_dir(), std::process::id(), nonce);
        let _ = fs::remove_file(&socket_path);
        let listener = std::os::unix::net::UnixListener::bind(&socket_path)
            .map_err(|error| format!("无法监听后台任务套接字：{error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("无法设置后台任务套接字：{error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600));
        }

        let inner = Arc::new(Inner {
            stop: AtomicBool::new(false),
            root,
            socket_path,
            next_id: AtomicU64::new(1),
            mu: Mutex::new(State {
                tasks: Vec::new(),
                last_log_publish: None,
            }),
            cv: Condvar::new(),
            events,
            outbound,
            listen: Mutex::new(CurrentListen {
                id: None,
                poke: None,
            }),
            pokes: Mutex::new(Pokes {
                accept: None,
                waits: Vec::new(),
            }),
            next_listener: AtomicU64::new(1),
            threads: Mutex::new(Vec::new()),
        });
        let (accept_wake, accept_poke) =
            wake_pair().map_err(|error| format!("无法创建后台任务唤醒：{error}"))?;
        inner.pokes.lock().unwrap().accept = Some(accept_poke);
        let session_cwd = cwd.unwrap_or_else(|| {
            std::env::current_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "/".to_string())
        });
        let accept_inner = Arc::clone(&inner);
        let deadline_inner = Arc::clone(&inner);
        let accept = thread::Builder::new()
            .name("smelt-bg-accept".into())
            .spawn(move || accept_loop(accept_inner, listener, session_cwd, accept_wake))
            .map_err(|error| format!("无法启动后台任务监听：{error}"))?;
        let deadline = thread::Builder::new()
            .name("smelt-bg-deadline".into())
            .spawn(move || deadline_loop(deadline_inner))
            .map_err(|error| format!("无法启动后台任务期限：{error}"))?;
        inner.threads.lock().unwrap().extend([accept, deadline]);
        Ok(Self { inner })
    }

    pub fn socket_path(&self) -> &Path {
        &self.inner.socket_path
    }
}

impl Drop for BackgroundSupervisor {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        poke(&self.inner);
        self.inner.cv.notify_all();
        if let Ok(state) = self.inner.mu.lock() {
            for task in &state.tasks {
                if is_running(task.status) {
                    signal_group(task.pgid, libc::SIGKILL);
                }
            }
        }
        let threads = std::mem::take(&mut *self.inner.threads.lock().unwrap());
        for thread in threads {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.inner.socket_path);
        let _ = fs::remove_dir_all(&self.inner.root);
    }
}

fn accept_loop(
    inner: Arc<Inner>,
    listener: std::os::unix::net::UnixListener,
    session_cwd: String,
    mut wake: std::os::unix::net::UnixStream,
) {
    loop {
        if inner.stop.load(Ordering::SeqCst) {
            return;
        }
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let inner = Arc::clone(&inner);
                    let session_cwd = session_cwd.clone();
                    let _ = thread::Builder::new()
                        .name("smelt-bg-req".into())
                        .spawn(move || handle_client(inner, stream, &session_cwd));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return,
            }
        }
        if inner.stop.load(Ordering::SeqCst) {
            return;
        }
        let ready = match poll_wait(
            &[
                (listener.as_raw_fd(), libc::POLLIN),
                (wake.as_raw_fd(), libc::POLLIN),
            ],
            None,
        ) {
            Ok(ready) => ready,
            Err(_) => return,
        };
        if ready.get(1) == Some(&true) {
            drain_wake(&mut wake);
        }
    }
}

fn handle_client(inner: Arc<Inner>, stream: std::os::unix::net::UnixStream, session_cwd: &str) {
    use std::io::{BufRead, BufReader};
    // macOS 上，非阻塞监听套接字 accept 出来的连接也是非阻塞的。
    // 请求行还没写到时，读会立刻返回 WouldBlock；那不是对端关掉了。
    set_fd_blocking(stream.as_raw_fd());
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let request: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
    let mut stream = reader.into_inner();
    let _ = stream.set_read_timeout(None);
    match request.get("op").and_then(serde_json::Value::as_str) {
        Some("listen") => serve_listen(inner, stream),
        Some("wait") if wait_timeout(&request).is_some() => {
            serve_wait_until(inner, stream, &request);
        }
        _ => {
            let response = dispatch(&inner, &request, session_cwd);
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(b"\n");
        }
    }
}

/// 扩展保持这条连接。结果在任务记录里：这条连接把还没写成功的读出来，写进套接字才算送到。
/// 对端关掉、任务结束、自己被换掉，都由这一次等待返回，不再隔一段时间醒来看。
fn serve_listen(inner: Arc<Inner>, mut stream: std::os::unix::net::UnixStream) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Ok((mut wake, poke_tx)) = wake_pair() else {
        return;
    };
    let id = register_listener(&inner, poke_tx);
    if write_line(&mut stream, &serde_json::json!({"ok": true}).to_string()).is_err() {
        end_listener(&inner, id);
        return;
    }
    if write_ready(&inner, id, &mut stream).is_err() {
        end_listener(&inner, id);
        return;
    }
    let mut buf = [0u8; 64];
    loop {
        if inner.stop.load(Ordering::SeqCst) {
            clear_if_current(&inner, id);
            return;
        }
        if !is_current(&inner, id) {
            abandon_listener(&inner, id);
            return;
        }
        let ready = match poll_wait(
            &[
                (stream.as_raw_fd(), libc::POLLIN),
                (wake.as_raw_fd(), libc::POLLIN),
            ],
            None,
        ) {
            Ok(ready) => ready,
            Err(_) => {
                end_listener(&inner, id);
                return;
            }
        };
        if ready.get(1) == Some(&true) {
            drain_wake(&mut wake);
        }
        if inner.stop.load(Ordering::SeqCst) {
            clear_if_current(&inner, id);
            return;
        }
        if !is_current(&inner, id) {
            abandon_listener(&inner, id);
            return;
        }
        if ready.get(0) == Some(&true) {
            let peer_gone = match stream.read(&mut buf) {
                Ok(0) => true,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::Interrupted =>
                {
                    false
                }
                Err(_) => true,
                Ok(_) => false,
            };
            if peer_gone {
                end_listener(&inner, id);
                return;
            }
        }
        if write_ready(&inner, id, &mut stream).is_err() {
            end_listener(&inner, id);
            return;
        }
    }
}

fn register_listener(inner: &Inner, poke_tx: std::os::unix::net::UnixStream) -> u64 {
    let id = inner.next_listener.fetch_add(1, Ordering::Relaxed);
    let mut slot = inner.listen.lock().unwrap();
    slot.id = Some(id);
    if let Some(old) = slot.poke.replace(poke_tx) {
        poke_stream(&old);
    }
    id
}

fn is_current(inner: &Inner, id: u64) -> bool {
    inner.listen.lock().unwrap().id == Some(id)
}

fn current_listener(inner: &Inner) -> Option<u64> {
    inner.listen.lock().unwrap().id
}

fn clear_if_current(inner: &Inner, id: u64) {
    let mut slot = inner.listen.lock().unwrap();
    if slot.id == Some(id) {
        slot.id = None;
        slot.poke = None;
    }
}

/// 这条连接结束。它名下还被工具看着的任务放开，界面补一行；模型等下一条连接来写。
fn end_listener(inner: &Inner, id: u64) {
    clear_if_current(inner, id);
    if inner.stop.load(Ordering::SeqCst) {
        return;
    }
    abandon_listener(inner, id);
}

fn write_line(writer: &mut impl Write, line: &str) -> std::io::Result<()> {
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn write_model_notice(writer: &mut impl Write, notice: &ModelNotice) -> std::io::Result<()> {
    write_line(
        writer,
        &serde_json::json!({
            "op": "completion",
            "id": notice.id,
            "title": notice.title,
            "status": notice.status,
            "exitCode": notice.exit_code,
            "output": notice.output,
            "outputTail": notice.output_tail,
            "wake": notice.wake,
        })
        .to_string(),
    )
}

struct Reply {
    text: String,
    fields: serde_json::Value,
}

fn reply(text: impl Into<String>, fields: serde_json::Value) -> Reply {
    Reply {
        text: text.into(),
        fields,
    }
}

fn dispatch(inner: &Arc<Inner>, request: &serde_json::Value, session_cwd: &str) -> String {
    let op = request
        .get("op")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let body = match op {
        "start" => start_task(inner, request, session_cwd),
        "output" => output_task(inner, request),
        "stop" => stop_task(inner, request),
        "wait" => wait_tasks(inner, request),
        "unwatch" => unwatch_task(inner, request),
        "list" => Ok(reply(list_text(inner), serde_json::json!({}))),
        _ => Err("未知的后台任务操作".to_string()),
    };
    match body {
        Ok(body) => {
            let mut value = serde_json::json!({"ok": true, "text": body.text});
            if let (Some(target), Some(fields)) = (value.as_object_mut(), body.fields.as_object()) {
                for (key, field) in fields {
                    target.insert(key.clone(), field.clone());
                }
            }
            value.to_string()
        }
        Err(error) => serde_json::json!({"ok": false, "error": error}).to_string(),
    }
}

fn start_task(
    inner: &Arc<Inner>,
    request: &serde_json::Value,
    session_cwd: &str,
) -> Result<Reply, String> {
    let command = request
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if command.is_empty() {
        return Err("缺少 command".into());
    }
    let cwd = resolve_cwd(
        request.get("cwd").and_then(serde_json::Value::as_str),
        session_cwd,
    )?;
    let title = request
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| command.chars().take(80).collect());
    let timeout = request
        .get("timeoutSeconds")
        .and_then(serde_json::Value::as_u64)
        .filter(|seconds| *seconds > 0 && *seconds <= 24 * 60 * 60);

    let watch = request
        .get("watch")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    // 先记下当前监听，再拿任务锁，避免和写结果的线程互相等待。
    let watched_by = watch.then(|| current_listener(inner).unwrap_or(UNBOUND_WATCH));
    let mut state = inner.mu.lock().unwrap();
    if state
        .tasks
        .iter()
        .filter(|task| is_running(task.status))
        .count()
        >= MAX_TASKS
    {
        return Err(format!(
            "已经有 {MAX_TASKS} 个后台任务，先停止或等其中一个结束"
        ));
    }
    // 已经写到界面和模型的结束任务可以让位。还没写成功的留下，条数不能当成已送达。
    state
        .tasks
        .retain(|task| is_running(task.status) || !task.ui_published || !task.model_delivered);
    let id = format!("bg-{}", inner.next_id.fetch_add(1, Ordering::Relaxed));
    let log_path = inner.root.join(format!("{id}.log"));
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| format!("无法创建任务日志：{error}"))?;
    drop(log);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&log_path, fs::Permissions::from_mode(0o600));
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut command_process = Command::new(shell);
    command_process
        .arg("-lc")
        .arg(&command)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("SMELT_BACKGROUND_TASK_SOCK");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command_process.process_group(0);
    }
    let mut child = command_process
        .spawn()
        .map_err(|error| format!("无法启动命令：{error}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let pgid = child.id() as i32;
    record_task_group(&inner.root, pgid);
    state.tasks.push(LiveTask {
        id: id.clone(),
        title: title.clone(),
        command,
        status: BackgroundTaskStatus::Running,
        exit_code: None,
        started_at_ms: now_ms(),
        finished_at_ms: None,
        log_path,
        pgid: Some(pgid),
        cancel: None,
        watched_by,
        ui_published: false,
        model_delivered: false,
        delivering_to: None,
        deadline: timeout.map(|seconds| Instant::now() + Duration::from_secs(seconds)),
    });
    publish_locked(&state, &inner.events);
    drop(state);
    inner.cv.notify_all();
    let task_inner = Arc::clone(inner);
    let waited_id = id.clone();
    let handle = thread::Builder::new()
        .name("smelt-bg-task".into())
        .spawn(move || supervise_task(task_inner, waited_id, child, stdout, stderr))
        .map_err(|error| {
            signal_group(Some(pgid), libc::SIGKILL);
            forget_task_group(&inner.root, pgid);
            format!("无法看管后台任务：{error}")
        })?;
    inner.threads.lock().unwrap().push(handle);
    Ok(reply(
        format!("已在后台启动 {id}：{title}"),
        serde_json::json!({"id": id, "status": "running"}),
    ))
}

fn output_task(inner: &Inner, request: &serde_json::Value) -> Result<Reply, String> {
    let id = required_id(request)?;
    let offset = request
        .get("offset")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let limit = request
        .get("limitBytes")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(OUTPUT_DEFAULT)
        .clamp(1, OUTPUT_MAX);
    let state = inner.mu.lock().unwrap();
    let task = state
        .tasks
        .iter()
        .find(|task| task.id == id)
        .ok_or_else(|| format!("没有后台任务 {id}"))?;
    let page = read_log_page(&task.log_path, offset, limit)?;
    let ended = !is_running(task.status) && page.end;
    let status = task.status;
    let exit_code = task.exit_code;
    Ok(reply(
        format!(
            "{} {}{}\nnextOffset={} endOfLog={}\n{}",
            task.id,
            status.label(),
            exit_code
                .map(|code| format!(" 退出码 {code}"))
                .unwrap_or_default(),
            page.next_offset,
            ended,
            if page.text.is_empty() {
                "（还没有输出）".to_string()
            } else {
                page.text.clone()
            }
        ),
        serde_json::json!({
            "id": task.id,
            "status": status_name(status),
            "exitCode": exit_code,
            "output": page.text,
            "nextOffset": page.next_offset,
            "endOfLog": ended,
        }),
    ))
}

fn stop_task(inner: &Inner, request: &serde_json::Value) -> Result<Reply, String> {
    let id = required_id(request)?;
    let mut state = inner.mu.lock().unwrap();
    let task = state
        .tasks
        .iter_mut()
        .find(|task| task.id == id)
        .ok_or_else(|| format!("没有后台任务 {id}"))?;
    if !is_running(task.status) {
        let status = task.status;
        return Ok(reply(
            format!("{} 已是{}", task.id, status.label()),
            serde_json::json!({"id": id, "status": status_name(status)}),
        ));
    }
    task.cancel = Some(BackgroundTaskStatus::Stopped);
    signal_group(task.pgid, libc::SIGTERM);
    Ok(reply(
        format!("{id} 正在停止"),
        serde_json::json!({"id": id, "status": "stopped"}),
    ))
}

/// 不带 `timeoutMs` 时只回报当前状态，不把这次调用挂住。
/// 进程退出由看管线程标记；带了期限的等待在 `serve_wait_until`。
fn wait_tasks(inner: &Inner, request: &serde_json::Value) -> Result<Reply, String> {
    let ids = request_ids(request)?;
    let state = inner.mu.lock().unwrap();
    let (_done, reply) = wait_reply(&state, &ids)?;
    drop(state);
    Ok(reply)
}

fn wait_reply(state: &State, ids: &[String]) -> Result<(bool, Reply), String> {
    for id in ids {
        if !state.tasks.iter().any(|task| task.id == *id) {
            return Err(format!("没有后台任务 {id}"));
        }
    }
    let done = ids.iter().all(|id| {
        state
            .tasks
            .iter()
            .find(|task| task.id == *id)
            .is_some_and(|task| !is_running(task.status))
    });
    let mut lines = Vec::new();
    for id in ids {
        let Some(task) = state.tasks.iter().find(|task| task.id == *id) else {
            continue;
        };
        let code = task
            .exit_code
            .map(|code| format!(" 退出码 {code}"))
            .unwrap_or_default();
        lines.push(format!("{} {}{code}", task.id, task.status.label()));
    }
    let tasks = serde_json::Value::Array(
        ids.iter()
            .filter_map(|id| state.tasks.iter().find(|task| task.id == *id))
            .map(|task| {
                serde_json::json!({
                    "id": task.id,
                    "status": status_name(task.status),
                    "exitCode": task.exit_code,
                })
            })
            .collect(),
    );
    Ok((
        done,
        reply(lines.join("\n"), serde_json::json!({"tasks": tasks})),
    ))
}

fn unwatch_task(inner: &Inner, request: &serde_json::Value) -> Result<Reply, String> {
    let id = required_id(request)?;
    let consumed = request
        .get("consumed")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let notice = {
        let mut state = inner.mu.lock().unwrap();
        let task = state
            .tasks
            .iter_mut()
            .find(|task| task.id == id)
            .ok_or_else(|| format!("没有后台任务 {id}"))?;
        task.watched_by = None;
        if consumed {
            // 工具已经拿到输出。界面和模型都不再送。
            task.ui_published = true;
            task.model_delivered = true;
            task.delivering_to = None;
            None
        } else {
            take_ui(task)
        }
    };
    if let Some(notice) = notice {
        publish_ui(inner, &notice);
    }
    poke(inner);
    Ok(reply(
        format!("{id} 已不再由工具观察"),
        serde_json::json!({"id": id}),
    ))
}

fn status_name(status: BackgroundTaskStatus) -> &'static str {
    match status {
        BackgroundTaskStatus::Running => "running",
        BackgroundTaskStatus::Completed => "completed",
        BackgroundTaskStatus::Failed => "failed",
        BackgroundTaskStatus::Stopped => "stopped",
        BackgroundTaskStatus::TimedOut => "timed_out",
    }
}

fn list_text(inner: &Inner) -> String {
    let state = inner.mu.lock().unwrap();
    if state.tasks.is_empty() {
        return "没有后台任务".to_string();
    }
    state
        .tasks
        .iter()
        .map(|task| format!("{} {} {}", task.id, task.status.label(), task.title))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 等到下一条命令期限。没有期限就一直等，直到有新任务或要停机。
fn deadline_loop(inner: Arc<Inner>) {
    let mut state = inner.mu.lock().unwrap();
    while !inner.stop.load(Ordering::SeqCst) {
        let now = Instant::now();
        let mut due = Vec::new();
        let mut next: Option<Instant> = None;
        for task in &state.tasks {
            if !is_running(task.status) || task.cancel.is_some() {
                continue;
            }
            let Some(deadline) = task.deadline else {
                continue;
            };
            if deadline <= now {
                due.push((task.id.clone(), task.pgid));
            } else {
                next = Some(next.map_or(deadline, |soonest| soonest.min(deadline)));
            }
        }
        for (id, pgid) in due {
            if let Some(task) = state.tasks.iter_mut().find(|task| task.id == id) {
                if is_running(task.status) && task.cancel.is_none() {
                    task.cancel = Some(BackgroundTaskStatus::TimedOut);
                    signal_group(pgid, libc::SIGTERM);
                }
            }
        }
        state = if let Some(deadline) = next {
            let wait = deadline.saturating_duration_since(Instant::now());
            inner
                .cv
                .wait_timeout(state, wait)
                .unwrap_or_else(|poison| poison.into_inner())
                .0
        } else {
            inner
                .cv
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner())
        };
    }
}

/// 组长的 `wait` 写一个字节到这条管道，监督线程不用再开第二条 `waitpid`。
struct ExitWait {
    wake: std::io::PipeReader,
    status_rx: std::sync::mpsc::Receiver<Option<i32>>,
}

impl ExitWait {
    /// 管道或等待线程起不来时把子进程还回来，调用方退回「先读完再 wait」。
    fn start(child: Child) -> Result<Self, Child> {
        let (wake, mut write) = match std::io::pipe() {
            Ok(pair) => pair,
            Err(_) => return Err(child),
        };
        set_fd_nonblocking(wake.as_raw_fd());
        let slot = Arc::new(Mutex::new(Some(child)));
        let thread_slot = Arc::clone(&slot);
        let (tx, status_rx) = std::sync::mpsc::channel();
        let spawned = thread::Builder::new()
            .name("smelt-bg-wait".into())
            .spawn(move || {
                let code = thread_slot
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .take()
                    .and_then(|mut child| child.wait().ok())
                    .and_then(|status| status.code());
                let _ = write.write_all(&[1]);
                drop(write);
                let _ = tx.send(code);
            });
        if spawned.is_err() {
            return Err(slot
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .take()
                .expect("等待线程没有启动，子进程应还在"));
        }
        Ok(Self { wake, status_rx })
    }
}

/// 任务寿命跟组长走。管道只收集输出；孙进程握着管道时，组长一退出就结束。
fn supervise_task(
    inner: Arc<Inner>,
    id: String,
    child: Child,
    mut stdout: Option<ChildStdout>,
    mut stderr: Option<ChildStderr>,
) {
    if let Some(pipe) = stdout.as_ref() {
        set_fd_nonblocking(pipe.as_raw_fd());
    }
    if let Some(pipe) = stderr.as_ref() {
        set_fd_nonblocking(pipe.as_raw_fd());
    }
    let log_path = {
        let state = inner.mu.lock().unwrap();
        state
            .tasks
            .iter()
            .find(|task| task.id == id)
            .map(|task| task.log_path.clone())
    };
    let mut log = log_path.and_then(|path| OpenOptions::new().append(true).open(path).ok());
    let mut total = 0u64;
    let mut capped = false;
    let mut output = LiveOutput {
        stdout: &mut stdout,
        stderr: &mut stderr,
        log: &mut log,
        total: &mut total,
        capped: &mut capped,
    };
    let exit_code = match ExitWait::start(child) {
        Ok(wait) => {
            let leader_exited = watch_output(&inner, &id, &mut output, Some(wait.wake.as_raw_fd()));
            if leader_exited {
                let wrote = take_available(output.stdout, output.log, output.total)
                    || take_available(output.stderr, output.log, output.total);
                if !*output.capped && *output.total > LOG_CAP {
                    mark_cancel(&inner, &id, BackgroundTaskStatus::Failed, libc::SIGKILL);
                }
                if wrote {
                    note_output(&inner);
                }
                // 丢掉读端，孙子再写会得到 EPIPE。组长已经退出，不再为它延长任务。
                drop(output.stdout.take());
                drop(output.stderr.take());
            }
            wait.status_rx.recv().unwrap_or(None)
        }
        Err(mut child) => {
            // 连唤醒管道都建不出来时，不再读到管道关闭才 wait，那样会又被孙进程拖住。
            signal_group(Some(child.id() as i32), libc::SIGKILL);
            child.wait().ok().and_then(|status| status.code())
        }
    };
    if let Some(file) = log.as_mut() {
        let _ = file.flush();
    }
    let pgid = inner
        .mu
        .lock()
        .unwrap()
        .tasks
        .iter()
        .find(|task| task.id == id)
        .and_then(|task| task.pgid);
    settle_task(&inner, &id, exit_code);
    // shell 已经收走。同组里还活着的（比如 `sleep &`）跟着结束。
    // 自己 setsid 脱离的不在这组里，也不归这条任务管。
    // 组长的号若已经被别人拿去用了，就不再按这个号杀。
    if let Some(pgid) = pgid {
        kill_leftovers(pgid);
        forget_task_group(&inner.root, pgid);
    }
}

struct LiveOutput<'a> {
    stdout: &'a mut Option<ChildStdout>,
    stderr: &'a mut Option<ChildStderr>,
    log: &'a mut Option<File>,
    total: &'a mut u64,
    capped: &'a mut bool,
}

/// 读到组长退出的唤醒就返回 true。管道先关则返回 false，调用方仍等同一条 wait。
fn watch_output(inner: &Inner, id: &str, output: &mut LiveOutput<'_>, wake: Option<RawFd>) -> bool {
    loop {
        if output.stdout.is_none() && output.stderr.is_none() && wake.is_none() {
            return false;
        }
        let mut fds = Vec::new();
        let stdout_at = output.stdout.as_ref().map(|pipe| {
            fds.push((pipe.as_raw_fd(), libc::POLLIN));
            fds.len() - 1
        });
        let stderr_at = output.stderr.as_ref().map(|pipe| {
            fds.push((pipe.as_raw_fd(), libc::POLLIN));
            fds.len() - 1
        });
        let wake_at = wake.map(|fd| {
            fds.push((fd, libc::POLLIN));
            fds.len() - 1
        });
        if fds.is_empty() {
            return false;
        }
        let ready = match poll_wait(&fds, None) {
            Ok(ready) => ready,
            Err(_) => return false,
        };
        let wrote = consume_ready(output.stdout, stdout_at, &ready, output.log, output.total)
            || consume_ready(output.stderr, stderr_at, &ready, output.log, output.total);
        if !*output.capped && *output.total > LOG_CAP {
            *output.capped = true;
            mark_cancel(inner, id, BackgroundTaskStatus::Failed, libc::SIGKILL);
        }
        if wrote {
            note_output(inner);
        }
        if wake_at.is_some_and(|index| ready.get(index).copied().unwrap_or(false)) {
            return true;
        }
        if output.stdout.is_none() && output.stderr.is_none() {
            return false;
        }
    }
}

fn consume_ready(
    pipe: &mut Option<impl Read>,
    index: Option<usize>,
    ready: &[bool],
    log: &mut Option<File>,
    total: &mut u64,
) -> bool {
    let Some(index) = index else {
        return false;
    };
    if !ready.get(index).copied().unwrap_or(false) {
        return false;
    }
    take_available(pipe, log, total)
}

fn take_available(pipe: &mut Option<impl Read>, log: &mut Option<File>, total: &mut u64) -> bool {
    let Some(inner) = pipe.as_mut() else {
        return false;
    };
    match drain_pipe(inner, log, total) {
        Ok((false, wrote)) => wrote,
        Ok((true, _)) | Err(_) => {
            *pipe = None;
            false
        }
    }
}

/// `(对端已关, 这次读到了输出)`。
fn drain_pipe(
    pipe: &mut impl Read,
    log: &mut Option<File>,
    total: &mut u64,
) -> std::io::Result<(bool, bool)> {
    let mut wrote = false;
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => return Ok((true, wrote)),
            Ok(size) => {
                wrote = true;
                *total += size as u64;
                if let Some(file) = log.as_mut() {
                    let _ = file.write_all(&buf[..size]);
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::Interrupted =>
            {
                return Ok((false, wrote));
            }
            Err(error) => return Err(error),
        }
    }
}

fn mark_cancel(inner: &Inner, id: &str, status: BackgroundTaskStatus, signal: i32) {
    let pgid = {
        let mut state = inner.mu.lock().unwrap();
        let Some(task) = state.tasks.iter_mut().find(|task| task.id == id) else {
            return;
        };
        if !is_running(task.status) || task.cancel.is_some() {
            return;
        }
        task.cancel = Some(status);
        task.pgid
    };
    signal_group(pgid, signal);
}

fn note_output(inner: &Inner) {
    let mut state = inner.mu.lock().unwrap();
    if state
        .last_log_publish
        .is_some_and(|last| last.elapsed() < LOG_PUBLISH_GAP)
    {
        return;
    }
    state.last_log_publish = Some(Instant::now());
    publish_locked(&state, &inner.events);
}

fn settle_task(inner: &Inner, id: &str, exit_code: Option<i32>) {
    let notice = {
        let mut state = inner.mu.lock().unwrap();
        let Some(task) = state.tasks.iter_mut().find(|task| task.id == id) else {
            return;
        };
        if !is_running(task.status) {
            return;
        }
        let stopping = inner.stop.load(Ordering::SeqCst);
        finish_task(task, exit_code);
        let notice = if stopping { None } else { take_ui(task) };
        if !stopping {
            publish_locked(&state, &inner.events);
        }
        inner.cv.notify_all();
        notice
    };
    if let Some(notice) = notice {
        publish_ui(inner, &notice);
    }
    if !inner.stop.load(Ordering::SeqCst) {
        poke(inner);
    }
}

fn wait_timeout(request: &serde_json::Value) -> Option<Duration> {
    let ms = request
        .get("timeoutMs")
        .and_then(serde_json::Value::as_u64)?;
    if ms == 0 {
        return None;
    }
    Some(Duration::from_millis(ms.min(MAX_WAIT_MS)))
}

/// 守到这些任务结束，或期限到了，或对端把连接关掉。期限是这一次等待的上限。
fn serve_wait_until(
    inner: Arc<Inner>,
    mut stream: std::os::unix::net::UnixStream,
    request: &serde_json::Value,
) {
    let Some(timeout) = wait_timeout(request) else {
        return;
    };
    let deadline = Instant::now() + timeout;
    let ids = match request_ids(request) {
        Ok(ids) => ids,
        Err(error) => {
            let _ = write_line(
                &mut stream,
                &serde_json::json!({"ok": false, "error": error}).to_string(),
            );
            return;
        }
    };
    let Ok((mut wake, poke_tx)) = wake_pair() else {
        return;
    };
    let poke_fd = poke_tx.as_raw_fd();
    inner.pokes.lock().unwrap().waits.push(poke_tx);
    let outcome = loop {
        if inner.stop.load(Ordering::SeqCst) {
            break None;
        }
        let snapshot = {
            let state = inner.mu.lock().unwrap();
            wait_reply(&state, &ids)
        };
        match snapshot {
            Err(error) => break Some(Err(error)),
            Ok((true, body)) => break Some(Ok(body)),
            Ok((false, body)) if Instant::now() >= deadline => break Some(Ok(body)),
            Ok((false, _)) => {}
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let state = inner.mu.lock().unwrap();
            break Some(wait_reply(&state, &ids).map(|(_done, body)| body));
        }
        let ready = match poll_wait(
            &[
                (stream.as_raw_fd(), libc::POLLIN),
                (wake.as_raw_fd(), libc::POLLIN),
            ],
            Some(remaining),
        ) {
            Ok(ready) => ready,
            Err(_) => break None,
        };
        if ready.get(1) == Some(&true) {
            drain_wake(&mut wake);
        }
        if ready.get(0) == Some(&true) {
            let mut buf = [0u8; 64];
            match stream.read(&mut buf) {
                Ok(0) => break None,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break None,
                Ok(_) => {}
            }
        }
    };
    {
        let mut pokes = inner.pokes.lock().unwrap();
        pokes.waits.retain(|waiting| waiting.as_raw_fd() != poke_fd);
    }
    match outcome {
        Some(Ok(body)) => {
            let mut value = serde_json::json!({"ok": true, "text": body.text});
            if let (Some(target), Some(fields)) = (value.as_object_mut(), body.fields.as_object()) {
                for (key, field) in fields {
                    target.insert(key.clone(), field.clone());
                }
            }
            let _ = write_line(&mut stream, &value.to_string());
        }
        Some(Err(error)) => {
            let _ = write_line(
                &mut stream,
                &serde_json::json!({"ok": false, "error": error}).to_string(),
            );
        }
        None => {}
    }
}

/// 界面上的一行。工具还看着，或者已经发过，就不再发。
fn take_ui(task: &mut LiveTask) -> Option<UiNotice> {
    if task.watched_by.is_some() || is_running(task.status) || task.ui_published {
        return None;
    }
    task.ui_published = true;
    let output = log_tail_limited(&task.log_path, MODEL_TAIL);
    Some(UiNotice {
        id: task.id.clone(),
        title: task.title.clone(),
        status: status_name(task.status).to_string(),
        exit_code: task.exit_code,
        output_tail: display_tail(&output),
    })
}

fn completion_of(task: &LiveTask) -> ModelNotice {
    let output = log_tail_limited(&task.log_path, MODEL_TAIL);
    ModelNotice {
        id: task.id.clone(),
        title: task.title.clone(),
        status: status_name(task.status).to_string(),
        exit_code: task.exit_code,
        output_tail: display_tail(&output),
        output,
        wake: !matches!(task.status, BackgroundTaskStatus::Stopped),
    }
}

fn publish_ui(inner: &Inner, notice: &UiNotice) {
    let _ = inner.outbound.try_send(serde_json::json!({
        "type": "smelt_background_notify",
        "id": notice.id,
        "title": notice.title,
        "status": notice.status,
        "exitCode": notice.exit_code,
        "outputTail": notice.output_tail,
    }));
}

/// 把还没写进套接字的结束结果写给这条监听。写成功才记成模型已收到。
fn write_ready(
    inner: &Inner,
    listener_id: u64,
    stream: &mut std::os::unix::net::UnixStream,
) -> std::io::Result<()> {
    loop {
        if !is_current(inner, listener_id) {
            return Ok(());
        }
        let notice = {
            let mut state = inner.mu.lock().unwrap();
            claim_model(&mut state, listener_id)
        };
        let Some(notice) = notice else {
            return Ok(());
        };
        if !is_current(inner, listener_id) || !still_claimed(inner, &notice.id, listener_id) {
            unclaim(inner, &notice.id, listener_id);
            return Ok(());
        }
        if let Err(error) = write_model_notice(stream, &notice) {
            unclaim(inner, &notice.id, listener_id);
            return Err(error);
        }
        let mut state = inner.mu.lock().unwrap();
        if let Some(task) = state.tasks.iter_mut().find(|task| task.id == notice.id) {
            if task.delivering_to == Some(listener_id) {
                task.model_delivered = true;
                task.delivering_to = None;
            }
        }
    }
}

fn claim_model(state: &mut State, listener_id: u64) -> Option<ModelNotice> {
    for task in &mut state.tasks {
        if task.watched_by.is_some() || is_running(task.status) || task.model_delivered {
            continue;
        }
        if task
            .delivering_to
            .is_some_and(|holder| holder != listener_id)
        {
            continue;
        }
        task.delivering_to = Some(listener_id);
        return Some(completion_of(task));
    }
    None
}

fn still_claimed(inner: &Inner, task_id: &str, listener_id: u64) -> bool {
    let state = inner.mu.lock().unwrap();
    state.tasks.iter().any(|task| {
        task.id == task_id
            && task.delivering_to == Some(listener_id)
            && !task.model_delivered
            && task.watched_by.is_none()
            && !is_running(task.status)
    })
}

fn unclaim(inner: &Inner, task_id: &str, listener_id: u64) {
    let mut state = inner.mu.lock().unwrap();
    if let Some(task) = state.tasks.iter_mut().find(|task| task.id == task_id) {
        if task.delivering_to == Some(listener_id) && !task.model_delivered {
            task.delivering_to = None;
        }
    }
}

/// 这条监听结束。只放开它名下的观察，不把「还没写到模型」记成已送达。
fn abandon_listener(inner: &Inner, id: u64) {
    if id == UNBOUND_WATCH {
        return;
    }
    let notices = {
        let mut state = inner.mu.lock().unwrap();
        let mut notices = Vec::new();
        for task in &mut state.tasks {
            if task.watched_by != Some(id) {
                continue;
            }
            task.watched_by = None;
            if let Some(notice) = take_ui(task) {
                notices.push(notice);
            }
        }
        notices
    };
    for notice in notices {
        publish_ui(inner, &notice);
    }
    poke(inner);
}

fn finish_task(task: &mut LiveTask, exit_code: Option<i32>) {
    task.exit_code = exit_code;
    task.finished_at_ms = Some(now_ms());
    task.status = match task.cancel {
        Some(status) => status,
        None if exit_code == Some(0) => BackgroundTaskStatus::Completed,
        None => BackgroundTaskStatus::Failed,
    };
}

fn publish_locked(state: &State, events: &smol::channel::Sender<ConversationEvent>) {
    let views = state
        .tasks
        .iter()
        .map(|task| BackgroundTaskView {
            id: task.id.clone(),
            title: task.title.clone(),
            command: task.command.clone(),
            status: task.status,
            exit_code: task.exit_code,
            output: log_tail(&task.log_path),
            started_at_ms: task.started_at_ms,
            finished_at_ms: task.finished_at_ms,
        })
        .collect();
    let _ = events.try_send(ConversationEvent::BackgroundTasks(views));
}

fn wake_pair() -> std::io::Result<(
    std::os::unix::net::UnixStream,
    std::os::unix::net::UnixStream,
)> {
    let (read, write) = std::os::unix::net::UnixStream::pair()?;
    read.set_nonblocking(true)?;
    write.set_nonblocking(true)?;
    Ok((read, write))
}

fn poke_stream(stream: &std::os::unix::net::UnixStream) {
    let byte = [1u8];
    let _ = unsafe { libc::write(stream.as_raw_fd(), byte.as_ptr().cast(), 1) };
}

fn poke(inner: &Inner) {
    if let Some(stream) = inner.listen.lock().unwrap().poke.as_ref() {
        poke_stream(stream);
    }
    let pokes = inner.pokes.lock().unwrap();
    if let Some(stream) = pokes.accept.as_ref() {
        poke_stream(stream);
    }
    for stream in &pokes.waits {
        poke_stream(stream);
    }
}

fn drain_wake(stream: &mut std::os::unix::net::UnixStream) {
    let mut buf = [0u8; 64];
    loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => continue,
        }
    }
}

fn poll_wait(fds: &[(RawFd, i16)], timeout: Option<Duration>) -> std::io::Result<Vec<bool>> {
    if fds.is_empty() {
        return Ok(Vec::new());
    }
    let mut pollfds: Vec<libc::pollfd> = fds
        .iter()
        .map(|(fd, events)| libc::pollfd {
            fd: *fd,
            events: *events,
            revents: 0,
        })
        .collect();
    let timeout_ms = match timeout {
        None => -1,
        Some(duration) => i32::try_from(duration.as_millis()).unwrap_or(i32::MAX),
    };
    let rc = unsafe {
        libc::poll(
            pollfds.as_mut_ptr(),
            pollfds.len() as libc::nfds_t,
            timeout_ms,
        )
    };
    if rc < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            return Ok(vec![false; fds.len()]);
        }
        return Err(error);
    }
    Ok(pollfds
        .iter()
        .map(|fd| fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0)
        .collect())
}

fn set_fd_nonblocking(fd: RawFd) {
    set_fd_flag(fd, libc::O_NONBLOCK, true);
}

fn set_fd_blocking(fd: RawFd) {
    set_fd_flag(fd, libc::O_NONBLOCK, false);
}

fn set_fd_flag(fd: RawFd, flag: i32, on: bool) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return;
        }
        let next = if on { flags | flag } else { flags & !flag };
        if next != flags {
            libc::fcntl(fd, libc::F_SETFL, next);
        }
    }
}

/// 宿主活着时记下进程组，宿主被硬杀掉后来不及 `Drop`，父进程靠这份记录收尸。
/// 启动时间用来确认这个号没有被别人复用。读不到启动时间就不记，宁可不杀。
fn record_task_group(root: &Path, pgid: i32) {
    if pgid <= 1 {
        return;
    }
    let Some(started) = crate::acp_conn::process_start_time(pgid) else {
        return;
    };
    let Ok(elapsed) = started.duration_since(UNIX_EPOCH) else {
        return;
    };
    let _ = fs::write(
        root.join(format!("group-{pgid}")),
        format!("{} {}\n", elapsed.as_secs(), elapsed.subsec_nanos()),
    );
}

fn forget_task_group(root: &Path, pgid: i32) {
    if pgid > 1 {
        let _ = fs::remove_file(root.join(format!("group-{pgid}")));
    }
}

/// 宿主进程已经不在了。扫临时目录里属于它的任务目录，按记录杀掉还在的进程组。
pub fn reap_orphaned_task_groups(host_pid: u32) {
    if host_pid == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if task_dir_host_pid(&name) != Some(host_pid) {
            continue;
        }
        reap_group_records(&entry.path());
    }
}

fn task_dir_host_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("smelt-bg-")?;
    let (rest, nonce) = rest.rsplit_once('-')?;
    nonce.parse::<u64>().ok()?;
    let (_session, pid) = rest.rsplit_once('-')?;
    pid.parse().ok()
}

fn reap_group_records(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(pgid) = name
            .strip_prefix("group-")
            .and_then(|text| text.parse::<i32>().ok())
        else {
            continue;
        };
        if group_record_matches(&entry.path(), pgid) {
            signal_group(Some(pgid), libc::SIGKILL);
        }
        let _ = fs::remove_file(entry.path());
    }
}

fn group_record_matches(path: &Path, pgid: i32) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let mut parts = text.split_whitespace();
    let (Some(secs), Some(nanos)) = (
        parts.next().and_then(|part| part.parse::<u64>().ok()),
        parts.next().and_then(|part| part.parse::<u32>().ok()),
    ) else {
        return false;
    };
    let Some(live) = crate::acp_conn::process_start_time(pgid) else {
        return false;
    };
    let Ok(elapsed) = live.duration_since(UNIX_EPOCH) else {
        return false;
    };
    elapsed.as_secs() == secs && elapsed.subsec_nanos() == nanos
}

fn signal_group(pgid: Option<i32>, signal: i32) {
    let Some(pgid) = pgid.filter(|pgid| *pgid > 0) else {
        return;
    };
    unsafe {
        libc::kill(-pgid, signal);
    }
}

/// 组长已经 `wait` 过。这个号若又指着一个活进程，就是被系统拿去复用了，不能整组杀。
/// 号已经空着时，同组里没跟着退出的进程还挂在这个组号上。
fn kill_leftovers(pgid: i32) {
    if pgid <= 1 || process_alive(pgid) {
        return;
    }
    signal_group(Some(pgid), libc::SIGKILL);
}

fn process_alive(pid: i32) -> bool {
    if pid <= 1 {
        return false;
    }
    let sent = unsafe { libc::kill(pid, 0) };
    sent == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

struct LogPage {
    text: String,
    next_offset: u64,
    end: bool,
}

fn read_log_page(path: &Path, offset: u64, limit: u64) -> Result<LogPage, String> {
    let mut file = File::open(path).map_err(|error| format!("无法读取任务日志：{error}"))?;
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let offset = offset.min(len);
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| format!("无法读取任务日志：{error}"))?;
    let mut buf = vec![0_u8; limit as usize];
    let read = file
        .read(&mut buf)
        .map_err(|error| format!("无法读取任务日志：{error}"))?;
    buf.truncate(read);
    while buf
        .last()
        .is_some_and(|byte| (*byte & 0b1100_0000) == 0b1000_0000)
    {
        buf.pop();
    }
    let next_offset = offset + buf.len() as u64;
    let text = String::from_utf8_lossy(&buf).into_owned();
    Ok(LogPage {
        text,
        next_offset,
        end: next_offset >= len,
    })
}

fn log_tail(path: &Path) -> String {
    log_tail_limited(path, UI_TAIL)
}

fn log_tail_limited(path: &Path, cap: u64) -> String {
    let len = log_len(path);
    let offset = len.saturating_sub(cap);
    read_log_page(path, offset, cap)
        .map(|page| page.text)
        .unwrap_or_default()
}

/// 状态行只留最后几行，避免整段日志铺满对话。
fn display_tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(8);
    let joined = lines[start..].join("\n");
    let count = joined.chars().count();
    if count <= 800 {
        return joined;
    }
    joined.chars().skip(count - 800).collect()
}

fn log_len(path: &Path) -> u64 {
    fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn is_running(status: BackgroundTaskStatus) -> bool {
    matches!(status, BackgroundTaskStatus::Running)
}

fn required_id(request: &serde_json::Value) -> Result<String, String> {
    request
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| "缺少 id".to_string())
}

fn request_ids(request: &serde_json::Value) -> Result<Vec<String>, String> {
    if let Some(ids) = request.get("ids").and_then(serde_json::Value::as_array) {
        let ids: Vec<String> = ids
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        if !ids.is_empty() {
            return Ok(ids);
        }
    }
    Ok(vec![required_id(request)?])
}

fn resolve_cwd(requested: Option<&str>, session_cwd: &str) -> Result<String, String> {
    let raw = requested.map(str::trim).filter(|cwd| !cwd.is_empty());
    let path = match raw {
        Some(cwd) => {
            let path = PathBuf::from(cwd);
            if path.is_absolute() {
                path
            } else {
                PathBuf::from(session_cwd).join(path)
            }
        }
        None => PathBuf::from(session_cwd),
    };
    if !path.is_dir() {
        return Err(format!("工作目录不存在：{}", path.display()));
    }
    Ok(path.display().to_string())
}

/// 日志目录可以很长，套接字不行。文件名只带 pid 和序号，放在临时目录根上。
fn control_socket_path(temp: PathBuf, pid: u32, nonce: u64) -> PathBuf {
    let name = format!("smbg-{pid}-{nonce}.sock");
    let in_temp = temp.join(&name);
    if in_temp.as_os_str().len() <= MAX_UNIX_SOCKET_PATH {
        return in_temp;
    }
    PathBuf::from("/tmp").join(name)
}

fn safe_component(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-')
        .take(40)
        .collect();
    if cleaned.is_empty() {
        "session".to_string()
    } else {
        cleaned
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt as _;

    fn supervisor() -> (
        BackgroundSupervisor,
        smol::channel::Receiver<ConversationEvent>,
    ) {
        let (supervisor, events, _outbound) = supervisor_with_outbound();
        (supervisor, events)
    }

    fn supervisor_with_outbound() -> (
        BackgroundSupervisor,
        smol::channel::Receiver<ConversationEvent>,
        smol::channel::Receiver<serde_json::Value>,
    ) {
        let (events, event_rx) = smol::channel::unbounded();
        let (outbound, outbound_rx) = smol::channel::unbounded();
        let cwd = std::env::current_dir().unwrap().display().to_string();
        let supervisor = BackgroundSupervisor::start("test", Some(cwd), events, outbound).unwrap();
        (supervisor, event_rx, outbound_rx)
    }

    struct Listen {
        reader: std::io::BufReader<UnixStream>,
    }

    impl Listen {
        fn connect(path: &Path) -> Self {
            let stream = UnixStream::connect(path).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = std::io::BufReader::new(stream);
            reader
                .get_mut()
                .write_all(b"{\"op\":\"listen\"}\n")
                .unwrap();
            let mut line = String::new();
            std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
            let ack: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(ack["ok"], true, "{ack}");
            Self { reader }
        }

        fn next(&mut self) -> serde_json::Value {
            let mut line = String::new();
            std::io::BufRead::read_line(&mut self.reader, &mut line).unwrap();
            serde_json::from_str(line.trim()).unwrap()
        }

        /// 短等一行。超时或对端已关都是「没有结果」，用来确认没有多送一次。
        fn try_next(&mut self, timeout: Duration) -> Option<serde_json::Value> {
            if self
                .reader
                .get_mut()
                .set_read_timeout(Some(timeout))
                .is_err()
            {
                return None;
            }
            let mut line = String::new();
            match std::io::BufRead::read_line(&mut self.reader, &mut line) {
                Ok(0) => None,
                Ok(_) => serde_json::from_str(line.trim()).ok(),
                Err(_) => None,
            }
        }

        fn shutdown(&self) {
            let _ = self.reader.get_ref().shutdown(std::net::Shutdown::Both);
        }
    }

    fn recv_outbound(
        outbound: &smol::channel::Receiver<serde_json::Value>,
        timeout: Duration,
    ) -> Option<serde_json::Value> {
        let started = Instant::now();
        loop {
            match outbound.try_recv() {
                Ok(value) => return Some(value),
                Err(smol::channel::TryRecvError::Empty) if started.elapsed() < timeout => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return None,
            }
        }
    }

    fn wait_until_group_gone(pgid: i32) -> bool {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(1) {
            let sent = unsafe { libc::kill(-pgid, 0) };
            if sent != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn until_settled(path: &Path, id: &str) -> serde_json::Value {
        let started = Instant::now();
        loop {
            let snapshot = call(path, serde_json::json!({"op": "wait", "id": id}));
            let status = snapshot["tasks"][0]["status"].as_str().unwrap_or("");
            if status != "running" || started.elapsed() > Duration::from_secs(5) {
                return snapshot;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn call(path: &Path, request: serde_json::Value) -> serde_json::Value {
        let mut stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(format!("{request}\n").as_bytes()).unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(stream), &mut line).unwrap();
        serde_json::from_str(line.trim()).unwrap()
    }

    #[test]
    fn control_socket_stays_within_sun_path_on_a_long_macos_temp_dir() {
        let temp = PathBuf::from("/var/folders/0y/st66nkz547z4jtx7c5mjsf3r0000gq/T");
        let path = control_socket_path(temp, 67627, 1);
        assert!(
            path.as_os_str().len() <= MAX_UNIX_SOCKET_PATH,
            "{}",
            path.display()
        );
        assert!(
            path.file_name().and_then(|name| name.to_str()) == Some("smbg-67627-1.sock"),
            "{}",
            path.display()
        );
    }

    #[test]
    fn host_runs_a_command_outside_the_caller_and_pages_the_log() {
        let (supervisor, _events) = supervisor();
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "start", "command": "printf ready", "title": "打印"}),
        );
        assert_eq!(started["ok"], true);
        assert_eq!(started["id"], "bg-1");
        assert_eq!(started["status"], "running");
        let finished = until_settled(supervisor.socket_path(), "bg-1");
        assert_eq!(finished["ok"], true, "{finished}");
        assert_eq!(finished["tasks"][0]["status"], "completed", "{finished}");
        assert_eq!(finished["tasks"][0]["exitCode"], 0, "{finished}");
        let page = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "output", "id": "bg-1", "offset": 0, "limitBytes": 3}),
        );
        assert_eq!(page["output"], "rea");
        assert_eq!(page["nextOffset"], 3);
        assert_eq!(page["endOfLog"], false);
    }

    #[test]
    fn stop_records_intent_before_the_process_exits() {
        let (supervisor, _events) = supervisor();
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "start", "command": "sleep 30", "title": "等待"}),
        );
        assert_eq!(started["ok"], true, "{started}");
        let stopped = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "stop", "id": "bg-1"}),
        );
        assert_eq!(stopped["ok"], true, "{stopped}");
        let finished = until_settled(supervisor.socket_path(), "bg-1");
        assert_eq!(finished["tasks"][0]["status"], "stopped", "{finished}");
    }

    #[test]
    fn a_dead_process_ends_the_wait_without_timing_out() {
        let (supervisor, _events) = supervisor();
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "start", "command": "sleep 30", "title": "会被杀掉"}),
        );
        assert_eq!(started["ok"], true, "{started}");
        let pid = supervisor.inner.mu.lock().unwrap().tasks[0]
            .pgid
            .expect("pid");
        unsafe { libc::kill(-pid, libc::SIGKILL) };
        let finished = until_settled(supervisor.socket_path(), "bg-1");
        assert_ne!(finished["tasks"][0]["status"], "running", "{finished}");
    }

    #[test]
    fn a_grandchild_holding_the_pipe_does_not_keep_the_task_running() {
        let (supervisor, _events) = supervisor();
        let started_at = Instant::now();
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({
                "op": "start",
                "command": "sh -c 'trap \"\" HUP; sleep 30 &'",
                "title": "孙进程握着管道",
            }),
        );
        assert_eq!(started["ok"], true, "{started}");
        let pgid = supervisor.inner.mu.lock().unwrap().tasks[0]
            .pgid
            .expect("pgid");
        struct KillGroup(i32);
        impl Drop for KillGroup {
            fn drop(&mut self) {
                unsafe { libc::kill(-self.0, libc::SIGKILL) };
            }
        }
        let _kill = KillGroup(pgid);
        let finished = until_settled(supervisor.socket_path(), "bg-1");
        let elapsed = started_at.elapsed();
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?} {finished}");
        assert_eq!(finished["tasks"][0]["status"], "completed", "{finished}");
        assert_eq!(finished["tasks"][0]["exitCode"], 0, "{finished}");
        assert!(
            wait_until_group_gone(pgid),
            "shell 退出后，同组里剩下的进程应被收掉"
        );
    }

    #[test]
    fn host_death_reaps_a_group_only_when_the_start_time_matches() {
        let host_pid: u32 = u32::MAX - 7;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("smelt-bg-reaptest-{host_pid}-{nonce}"));
        fs::create_dir_all(&root).unwrap();
        let child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn sleep");
        let pgid = child.id() as i32;
        struct Guard {
            child: Child,
            root: PathBuf,
            pgid: i32,
        }
        impl Drop for Guard {
            fn drop(&mut self) {
                unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
                let _ = self.child.wait();
                let _ = fs::remove_dir_all(&self.root);
            }
        }
        let mut matched = Guard {
            child,
            root: root.clone(),
            pgid,
        };
        record_task_group(&root, pgid);
        assert!(
            root.join(format!("group-{pgid}")).is_file(),
            "应记下这个进程组的启动时间"
        );
        reap_orphaned_task_groups(host_pid);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match matched.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                Ok(None) => panic!("启动时间对得上时，应杀掉这个进程组"),
                Err(error) => panic!("等待被杀掉的进程失败：{error}"),
            }
        }

        let other = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .expect("spawn sleep");
        let other_pgid = other.id() as i32;
        let mut mismatched = Guard {
            child: other,
            root: root.clone(),
            pgid: other_pgid,
        };
        fs::write(root.join(format!("group-{other_pgid}")), "1 0\n").unwrap();
        reap_orphaned_task_groups(host_pid);
        assert_eq!(
            unsafe { libc::kill(other_pgid, 0) },
            0,
            "启动时间对不上时不能杀"
        );
        let _ = mismatched.child.kill();
    }

    #[test]
    fn task_dir_name_keeps_the_host_pid_when_the_session_has_dashes() {
        assert_eq!(task_dir_host_pid("smelt-bg-my-session-42-7"), Some(42));
        assert_eq!(task_dir_host_pid("smelt-bg-s-42-7"), Some(42));
        assert_eq!(task_dir_host_pid("smelt-bg-42-7"), None);
        assert_eq!(task_dir_host_pid("smelt-bg-nosplit"), None);
        assert_eq!(task_dir_host_pid("other-bg-1-2-3"), None);
    }

    #[test]
    fn wait_reports_a_live_task_without_blocking() {
        let (supervisor, _events) = supervisor();
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "start", "command": "sleep 30", "title": "还在跑"}),
        );
        assert_eq!(started["ok"], true, "{started}");
        let began = Instant::now();
        let snapshot = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "wait", "id": "bg-1"}),
        );
        assert!(began.elapsed() < Duration::from_secs(2), "{snapshot}");
        assert_eq!(snapshot["tasks"][0]["status"], "running", "{snapshot}");
    }

    #[test]
    fn wait_with_a_deadline_returns_when_the_process_exits() {
        let (supervisor, _events) = supervisor();
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "start", "command": "printf ready", "title": "打印"}),
        );
        assert_eq!(started["ok"], true, "{started}");
        let began = Instant::now();
        let finished = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "wait", "id": "bg-1", "timeoutMs": 5_000}),
        );
        assert!(began.elapsed() < Duration::from_secs(2), "{finished}");
        assert_eq!(finished["ok"], true, "{finished}");
        assert_eq!(finished["tasks"][0]["status"], "completed", "{finished}");
        assert_eq!(finished["tasks"][0]["exitCode"], 0, "{finished}");
    }

    #[test]
    fn wait_with_a_deadline_returns_still_running_when_the_time_is_up() {
        let (supervisor, _events) = supervisor();
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "start", "command": "sleep 30", "title": "还在跑"}),
        );
        assert_eq!(started["ok"], true, "{started}");
        let began = Instant::now();
        let snapshot = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "wait", "id": "bg-1", "timeoutMs": 300}),
        );
        let elapsed = began.elapsed();
        assert_eq!(snapshot["tasks"][0]["status"], "running", "{snapshot}");
        assert!(
            elapsed >= Duration::from_millis(200),
            "{elapsed:?} {snapshot}"
        );
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?} {snapshot}");
    }

    #[test]
    fn command_timeout_stops_the_process_when_its_deadline_arrives() {
        let (supervisor, _events) = supervisor();
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({
                "op": "start",
                "command": "sleep 30",
                "title": "到点停止",
                "timeoutSeconds": 1,
            }),
        );
        assert_eq!(started["ok"], true, "{started}");
        let finished = until_settled(supervisor.socket_path(), "bg-1");
        assert_eq!(finished["tasks"][0]["status"], "timed_out", "{finished}");
    }

    #[test]
    fn a_finished_task_publishes_the_exit_code_and_output_tail() {
        let (supervisor, _events, outbound) = supervisor_with_outbound();
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "start", "command": "printf ready", "title": "打印"}),
        );
        assert_eq!(started["ok"], true, "{started}");
        let finished = until_settled(supervisor.socket_path(), "bg-1");
        assert_eq!(finished["tasks"][0]["status"], "completed", "{finished}");
        let notice = outbound.try_recv().expect("完成必须留下状态");
        assert_eq!(notice["id"], "bg-1");
        assert_eq!(notice["status"], "completed");
        assert_eq!(notice["exitCode"], 0);
        assert!(
            notice["outputTail"]
                .as_str()
                .unwrap_or("")
                .contains("ready"),
            "{notice}"
        );
        assert!(notice.get("message").is_none(), "{notice}");
    }

    #[test]
    fn a_watched_task_stays_silent_when_the_tool_takes_the_result() {
        let (supervisor, _events, outbound) = supervisor_with_outbound();
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({
                "op": "start",
                "command": "printf ready",
                "title": "打印",
                "watch": true,
            }),
        );
        assert_eq!(started["ok"], true, "{started}");
        let finished = until_settled(supervisor.socket_path(), "bg-1");
        assert_eq!(finished["tasks"][0]["status"], "completed", "{finished}");
        assert!(
            outbound.try_recv().is_err(),
            "工具还拿着结果时不能再通知一次"
        );
        let released = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "unwatch", "id": "bg-1", "consumed": true}),
        );
        assert_eq!(released["ok"], true, "{released}");
        assert!(outbound.try_recv().is_err(), "{released}");
    }

    #[test]
    fn unwatch_without_the_result_publishes_once() {
        let (supervisor, _events, outbound) = supervisor_with_outbound();
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({
                "op": "start",
                "command": "printf ready",
                "title": "打印",
                "watch": true,
            }),
        );
        assert_eq!(started["ok"], true, "{started}");
        let finished = until_settled(supervisor.socket_path(), "bg-1");
        assert_eq!(finished["tasks"][0]["status"], "completed", "{finished}");
        let released = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "unwatch", "id": "bg-1", "consumed": false}),
        );
        assert_eq!(released["ok"], true, "{released}");
        let notice = outbound.try_recv().expect("工具没拿走结果时要补一次通知");
        assert_eq!(notice["id"], "bg-1");
        assert_eq!(notice["exitCode"], 0);
        assert!(outbound.try_recv().is_err(), "不能通知两次");
    }

    #[test]
    fn a_stopped_task_reaches_the_listener_without_waking_the_model() {
        let (supervisor, _events, _outbound) = supervisor_with_outbound();
        let mut listen = Listen::connect(supervisor.socket_path());
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "start", "command": "sleep 30", "title": "等待"}),
        );
        assert_eq!(started["ok"], true, "{started}");
        let stopped = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "stop", "id": "bg-1"}),
        );
        assert_eq!(stopped["ok"], true, "{stopped}");
        let finished = until_settled(supervisor.socket_path(), "bg-1");
        assert_eq!(finished["tasks"][0]["status"], "stopped", "{finished}");
        let completion = listen.next();
        assert_eq!(completion["op"], "completion");
        assert_eq!(completion["id"], "bg-1");
        assert_eq!(completion["status"], "stopped");
        assert_eq!(completion["wake"], false, "{completion}");
    }

    #[test]
    fn a_listener_receives_the_exit_code_and_output() {
        let (supervisor, _events, _outbound) = supervisor_with_outbound();
        let mut listen = Listen::connect(supervisor.socket_path());
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "start", "command": "printf ready", "title": "打印"}),
        );
        assert_eq!(started["ok"], true, "{started}");
        let finished = until_settled(supervisor.socket_path(), "bg-1");
        assert_eq!(finished["tasks"][0]["status"], "completed", "{finished}");
        let completion = listen.next();
        assert_eq!(completion["id"], "bg-1");
        assert_eq!(completion["exitCode"], 0);
        assert_eq!(completion["wake"], true);
        assert!(
            completion["output"]
                .as_str()
                .unwrap_or("")
                .contains("ready"),
            "{completion}"
        );
    }

    #[test]
    fn a_second_listener_replaces_the_first_and_receives_the_completion_once() {
        let (supervisor, _events, _outbound) = supervisor_with_outbound();
        let mut first = Listen::connect(supervisor.socket_path());
        let mut second = Listen::connect(supervisor.socket_path());
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({"op": "start", "command": "printf ready", "title": "打印"}),
        );
        assert_eq!(started["ok"], true, "{started}");
        let finished = until_settled(supervisor.socket_path(), "bg-1");
        assert_eq!(finished["tasks"][0]["status"], "completed", "{finished}");
        let completion = second.next();
        assert_eq!(completion["op"], "completion");
        assert_eq!(completion["id"], "bg-1");
        assert_eq!(completion["exitCode"], 0);
        assert!(
            second.try_next(Duration::from_millis(500)).is_none(),
            "活着的连接不能再收一次"
        );
        assert!(
            first.try_next(Duration::from_millis(200)).is_none(),
            "已经被换掉的连接不能再收一次"
        );
    }

    #[test]
    fn a_dropped_listener_publishes_a_finished_watched_task_once() {
        let (supervisor, _events, outbound) = supervisor_with_outbound();
        let listen = Listen::connect(supervisor.socket_path());
        let started = call(
            supervisor.socket_path(),
            serde_json::json!({
                "op": "start",
                "command": "printf ready",
                "title": "打印",
                "watch": true,
            }),
        );
        assert_eq!(started["ok"], true, "{started}");
        let finished = until_settled(supervisor.socket_path(), "bg-1");
        assert_eq!(finished["tasks"][0]["status"], "completed", "{finished}");
        assert!(
            outbound.try_recv().is_err(),
            "工具还连着时，结束了也不能先通知"
        );
        listen.shutdown();
        drop(listen);
        let notice = recv_outbound(&outbound, Duration::from_secs(2))
            .expect("监听断开后，工具没拿走的结果要补一次状态");
        assert_eq!(notice["id"], "bg-1");
        assert_eq!(notice["exitCode"], 0);
        assert!(
            recv_outbound(&outbound, Duration::from_millis(300)).is_none(),
            "状态行只能有一行"
        );
        let mut next = Listen::connect(supervisor.socket_path());
        let completion = next.next();
        assert_eq!(completion["id"], "bg-1");
        assert_eq!(completion["wake"], true);
        assert!(
            next.try_next(Duration::from_millis(500)).is_none(),
            "下一条连接只能再写一次给模型"
        );
        assert!(
            outbound.try_recv().is_err(),
            "状态行不能因为下一条连接再出现一次"
        );
    }

    #[test]
    fn a_later_listener_receives_every_finished_task() {
        let (supervisor, _events, _outbound) = supervisor_with_outbound();
        let mut ids = Vec::new();
        for index in 0..40 {
            let started = call(
                supervisor.socket_path(),
                serde_json::json!({
                    "op": "start",
                    "command": format!("printf {index}"),
                    "title": "打印",
                }),
            );
            assert_eq!(started["ok"], true, "{started}");
            let id = started["id"].as_str().unwrap().to_string();
            let finished = until_settled(supervisor.socket_path(), &id);
            assert_ne!(finished["tasks"][0]["status"], "running", "{finished}");
            ids.push(id);
        }
        let mut listen = Listen::connect(supervisor.socket_path());
        let mut got = Vec::new();
        while let Some(completion) = listen.try_next(Duration::from_secs(2)) {
            got.push(completion["id"].as_str().unwrap().to_string());
        }
        assert_eq!(got, ids, "没写成功的结果不能因为条数被丢掉");
    }
}
