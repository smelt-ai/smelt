//! 会话宿主里的后台任务。
//!
//! 进程归会话宿主所有，不放进 Pi 的进程组。Pi 扩展通过 Unix 套接字下命令；
//! `/reload` 只拆掉扩展，任务继续跑。会话连接结束时宿主才把任务停掉。

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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
const ACCEPT_POLL: Duration = Duration::from_millis(50);

struct LiveTask {
    id: String,
    title: String,
    command: String,
    status: BackgroundTaskStatus,
    exit_code: Option<i32>,
    started_at_ms: u64,
    finished_at_ms: Option<u64>,
    log_path: PathBuf,
    child: Option<Child>,
    pgid: Option<i32>,
    /// 先于信号记下的取消原因。进程随后以 0 退出也保持这个状态。
    cancel: Option<BackgroundTaskStatus>,
    notified: bool,
    deadline: Option<Instant>,
}

struct State {
    tasks: Vec<LiveTask>,
    /// 正在被 `wait` 覆盖的任务。被等待的完成只由那次工具调用回报。
    waiting: HashMap<String, usize>,
    log_dirty: bool,
    last_log_publish: Option<Instant>,
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
                waiting: HashMap::new(),
                log_dirty: false,
                last_log_publish: None,
            }),
            cv: Condvar::new(),
            events,
            outbound,
            threads: Mutex::new(Vec::new()),
        });
        let session_cwd = cwd.unwrap_or_else(|| {
            std::env::current_dir()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "/".to_string())
        });
        let accept_inner = Arc::clone(&inner);
        let reaper_inner = Arc::clone(&inner);
        let accept = thread::Builder::new()
            .name("smelt-bg-accept".into())
            .spawn(move || accept_loop(accept_inner, listener, session_cwd))
            .map_err(|error| format!("无法启动后台任务监听：{error}"))?;
        let reaper = thread::Builder::new()
            .name("smelt-bg-reaper".into())
            .spawn(move || reaper_loop(reaper_inner))
            .map_err(|error| format!("无法启动后台任务回收：{error}"))?;
        inner.threads.lock().unwrap().extend([accept, reaper]);
        Ok(Self { inner })
    }

    pub fn socket_path(&self) -> &Path {
        &self.inner.socket_path
    }
}

impl Drop for BackgroundSupervisor {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        self.inner.cv.notify_all();
        let threads = std::mem::take(&mut *self.inner.threads.lock().unwrap());
        for thread in threads {
            let _ = thread.join();
        }
        if let Ok(mut state) = self.inner.mu.lock() {
            for task in &mut state.tasks {
                force_kill(task);
            }
        }
        let _ = fs::remove_file(&self.inner.socket_path);
        let _ = fs::remove_dir_all(&self.inner.root);
    }
}

fn accept_loop(inner: Arc<Inner>, listener: std::os::unix::net::UnixListener, session_cwd: String) {
    while !inner.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let inner = Arc::clone(&inner);
                let session_cwd = session_cwd.clone();
                let _ = thread::Builder::new()
                    .name("smelt-bg-req".into())
                    .spawn(move || handle_client(inner, stream, &session_cwd));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
            }
            Err(_) => break,
        }
    }
}

fn handle_client(inner: Arc<Inner>, stream: std::os::unix::net::UnixStream, session_cwd: &str) {
    use std::io::{BufRead, BufReader};
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let request: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
    let response = dispatch(&inner, &request, session_cwd);
    let _ = writer.write_all(response.as_bytes());
    let _ = writer.write_all(b"\n");
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

fn dispatch(inner: &Inner, request: &serde_json::Value, session_cwd: &str) -> String {
    let op = request
        .get("op")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let body = match op {
        "start" => start_task(inner, request, session_cwd),
        "output" => output_task(inner, request),
        "stop" => stop_task(inner, request),
        "wait" => wait_tasks(inner, request),
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
    inner: &Inner,
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

    let mut state = inner.mu.lock().unwrap();
    if state.tasks.len() >= MAX_TASKS && !state.tasks.iter().any(|task| !is_running(task.status)) {
        return Err(format!(
            "已经有 {MAX_TASKS} 个后台任务，先停止或等其中一个结束"
        ));
    }
    while state.tasks.len() >= MAX_TASKS {
        let Some(index) = state.tasks.iter().position(|task| !is_running(task.status)) else {
            break;
        };
        state.tasks.remove(index);
    }
    let id = format!("bg-{}", inner.next_id.fetch_add(1, Ordering::Relaxed));
    let log_path = inner.root.join(format!("{id}.log"));
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| format!("无法创建任务日志：{error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&log_path, fs::Permissions::from_mode(0o600));
    }
    let stderr = log
        .try_clone()
        .map_err(|error| format!("无法打开任务日志：{error}"))?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut command_process = Command::new(shell);
    command_process
        .arg("-lc")
        .arg(&command)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(stderr)
        .env_remove("SMELT_BACKGROUND_TASK_SOCK");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command_process.process_group(0);
    }
    let child = command_process
        .spawn()
        .map_err(|error| format!("无法启动命令：{error}"))?;
    let pgid = child.id() as i32;
    state.tasks.push(LiveTask {
        id: id.clone(),
        title: title.clone(),
        command,
        status: BackgroundTaskStatus::Running,
        exit_code: None,
        started_at_ms: now_ms(),
        finished_at_ms: None,
        log_path,
        child: Some(child),
        pgid: Some(pgid),
        cancel: None,
        notified: false,
        deadline: timeout.map(|seconds| Instant::now() + Duration::from_secs(seconds)),
    });
    publish_locked(&state, &inner.events);
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

/// 只回报当前状态。进程退出由宿主标记后通知对话，这里不把回合挂住。
fn wait_tasks(inner: &Inner, request: &serde_json::Value) -> Result<Reply, String> {
    let ids = request_ids(request)?;
    let mut state = inner.mu.lock().unwrap();
    for id in &ids {
        if !state.tasks.iter().any(|task| task.id == *id) {
            return Err(format!("没有后台任务 {id}"));
        }
    }
    reap_finished(&mut state, inner);
    let mut lines = Vec::new();
    for id in &ids {
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
    Ok(reply(lines.join("\n"), serde_json::json!({"tasks": tasks})))
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

fn reaper_loop(inner: Arc<Inner>) {
    while !inner.stop.load(Ordering::SeqCst) {
        let mut notify = Vec::new();
        {
            let mut state = inner.mu.lock().unwrap();
            let mut changed = false;
            let waiting = state.waiting.clone();
            for task in &mut state.tasks {
                if !is_running(task.status) {
                    continue;
                }
                if task
                    .deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                    && task.cancel.is_none()
                {
                    task.cancel = Some(BackgroundTaskStatus::TimedOut);
                    signal_group(task.pgid, libc::SIGTERM);
                }
                if log_len(&task.log_path) > LOG_CAP && task.cancel.is_none() {
                    task.cancel = Some(BackgroundTaskStatus::Failed);
                    signal_group(task.pgid, libc::SIGKILL);
                }
                if let Some(exit_code) = poll_task_exit(task) {
                    finish_task(task, exit_code.or(task.exit_code));
                    changed = true;
                    if should_notify(&waiting, task) {
                        task.notified = true;
                        notify.push(notice(task));
                    }
                }
            }
            if changed {
                publish_locked(&state, &inner.events);
                inner.cv.notify_all();
            } else if state.log_dirty
                && state
                    .last_log_publish
                    .is_none_or(|last| last.elapsed() >= Duration::from_millis(400))
            {
                state.log_dirty = false;
                state.last_log_publish = Some(Instant::now());
                publish_locked(&state, &inner.events);
            } else if state.tasks.iter().any(|task| is_running(task.status)) {
                state.log_dirty = true;
            }
        }
        for text in notify {
            let _ = inner.outbound.try_send(serde_json::json!({
                "type": "smelt_background_notify",
                "message": text,
            }));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// 进程已经不在时返回 `Some(退出码)`。`try_wait` 报错或 pid 已消失都算结束，
/// 不能再把任务留在 Running 里让 `wait` 空等。
fn poll_task_exit(task: &mut LiveTask) -> Option<Option<i32>> {
    if !is_running(task.status) {
        return None;
    }
    let Some(child) = task.child.as_mut() else {
        return Some(task.exit_code);
    };
    match child.try_wait() {
        Ok(Some(status)) => Some(status.code()),
        Ok(None) => None,
        Err(_) => Some(task.exit_code),
    }
}

fn reap_finished(state: &mut State, inner: &Inner) -> bool {
    let waiting = state.waiting.clone();
    let mut changed = false;
    for task in &mut state.tasks {
        let Some(exit_code) = poll_task_exit(task) else {
            continue;
        };
        finish_task(task, exit_code.or(task.exit_code));
        changed = true;
        if should_notify(&waiting, task) {
            task.notified = true;
            let _ = inner.outbound.try_send(serde_json::json!({
                "type": "smelt_background_notify",
                "message": notice(task),
            }));
        }
    }
    if changed {
        publish_locked(state, &inner.events);
    }
    changed
}

fn finish_task(task: &mut LiveTask, exit_code: Option<i32>) {
    task.child = None;
    task.exit_code = exit_code;
    task.finished_at_ms = Some(now_ms());
    task.status = match task.cancel {
        Some(status) => status,
        None if exit_code == Some(0) => BackgroundTaskStatus::Completed,
        None => BackgroundTaskStatus::Failed,
    };
}

fn should_notify(waiting: &HashMap<String, usize>, task: &LiveTask) -> bool {
    if task.notified || matches!(task.status, BackgroundTaskStatus::Stopped) {
        return false;
    }
    waiting.get(&task.id).copied().unwrap_or(0) == 0
}

fn notice(task: &LiveTask) -> String {
    let code = task
        .exit_code
        .map(|code| format!("，退出码 {code}"))
        .unwrap_or_default();
    format!(
        "后台任务 {}（{}）已结束，状态 {}{code}。需要输出时用 background_task_output。",
        task.id,
        task.title,
        task.status.label()
    )
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

fn force_kill(task: &mut LiveTask) {
    if !is_running(task.status) {
        return;
    }
    signal_group(task.pgid, libc::SIGKILL);
    if let Some(child) = task.child.as_mut() {
        let _ = child.wait();
    }
    task.child = None;
    task.status = BackgroundTaskStatus::Stopped;
    task.finished_at_ms = Some(now_ms());
}

fn signal_group(pgid: Option<i32>, signal: i32) {
    let Some(pgid) = pgid.filter(|pgid| *pgid > 0) else {
        return;
    };
    unsafe {
        libc::kill(-pgid, signal);
    }
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
    let len = log_len(path);
    let offset = len.saturating_sub(UI_TAIL);
    read_log_page(path, offset, UI_TAIL)
        .map(|page| page.text)
        .unwrap_or_default()
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

    fn supervisor() -> (
        BackgroundSupervisor,
        smol::channel::Receiver<ConversationEvent>,
    ) {
        let (events, event_rx) = smol::channel::unbounded();
        let (outbound, _outbound_rx) = smol::channel::unbounded();
        let cwd = std::env::current_dir().unwrap().display().to_string();
        let supervisor = BackgroundSupervisor::start("test", Some(cwd), events, outbound).unwrap();
        (supervisor, event_rx)
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
}
