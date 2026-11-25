//! 非 Pi ACP 会话的轻量 peer 套接字：只服务跨对话请求和 MCP 握手，不带后台任务或子智能体。
//!
//! 每个非 Pi ACP 会话在拉起时起一只专属套接字，路径只传给该会话经 ACP `mcpServers`
//! 注入的 `smeltd --peer-mcp` 进程（环境变量 [`SMELT_PEER_SOCK_ENV`]），不塞进 agent 主进程环境。
//! `Drop` 停掉监听线程并删除套接字文件。

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use super::{
    PeerRequestHandler, TaskListener, TaskStream, WakeReader, WakeWriter, bind_control_listener,
    drain_wake, poke_stream, poll_ready, wake_pair,
};

/// `smeltd --peer-mcp` 读取的会话专属 peer 套接字路径。
pub const SMELT_PEER_SOCK_ENV: &str = "SMELT_PEER_SOCK";

#[cfg(unix)]
const MAX_UNIX_SOCKET_PATH: usize = 103;

struct PeerSocketInner {
    stop: AtomicBool,
    socket_path: PathBuf,
    handler: PeerRequestHandler,
    mcp_connected: Arc<AtomicBool>,
    accept_poke: Mutex<Option<WakeWriter>>,
}

/// 一条非 Pi ACP 会话的 peer 套接字服务。`Drop` 时停机并清掉套接字文件。
pub struct PeerSocket {
    inner: Arc<PeerSocketInner>,
    accept_thread: Option<JoinHandle<()>>,
}

impl PeerSocket {
    pub fn start(
        handler: PeerRequestHandler,
        mcp_connected: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        static NEXT_NONCE: AtomicU64 = AtomicU64::new(1);
        let nonce = NEXT_NONCE.fetch_add(1, Ordering::Relaxed);
        let socket_path = peer_socket_path(std::env::temp_dir(), std::process::id(), nonce);
        let _ = fs::remove_file(&socket_path);
        let listener = bind_control_listener(&socket_path)?;
        let (accept_wake, accept_poke) = wake_pair()
            .map_err(|error| format!("couldn't create peer socket wake pipe: {error}"))?;
        let inner = Arc::new(PeerSocketInner {
            stop: AtomicBool::new(false),
            socket_path,
            handler,
            mcp_connected,
            accept_poke: Mutex::new(Some(accept_poke)),
        });
        let accept_inner = Arc::clone(&inner);
        let accept_thread = thread::Builder::new()
            .name("smelt-peer-sock".into())
            .spawn(move || accept_loop(accept_inner, listener, accept_wake))
            .map_err(|error| format!("couldn't start peer socket listener: {error}"))?;
        Ok(Self {
            inner,
            accept_thread: Some(accept_thread),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.inner.socket_path
    }
}

impl Drop for PeerSocket {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        if let Ok(mut guard) = self.inner.accept_poke.lock()
            && let Some(writer) = guard.as_mut()
        {
            poke_stream(writer);
        }
        #[cfg(windows)]
        {
            if let Some(name) = self.inner.socket_path.to_str() {
                let _ = smelt_sys::named_pipe::PipeStream::connect(name);
            }
        }
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
        let _ = fs::remove_file(&self.inner.socket_path);
    }
}

fn peer_socket_path(temp: PathBuf, pid: u32, nonce: u64) -> PathBuf {
    #[cfg(unix)]
    {
        let name = format!("smpm-{pid}-{nonce}.sock");
        let in_temp = temp.join(&name);
        if in_temp.as_os_str().len() <= MAX_UNIX_SOCKET_PATH {
            return in_temp;
        }
        PathBuf::from("/tmp").join(name)
    }
    #[cfg(windows)]
    {
        let _ = temp;
        let tag = format!("pm-{pid}-{nonce}");
        PathBuf::from(
            smelt_sys::named_pipe::user_pipe_path(&tag)
                .unwrap_or_else(|_| format!(r"\\.\pipe\smelt-{tag}")),
        )
    }
}

fn accept_loop(inner: Arc<PeerSocketInner>, listener: TaskListener, mut wake_rx: WakeReader) {
    #[cfg(unix)]
    {
        loop {
            if inner.stop.load(Ordering::SeqCst) {
                return;
            }
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let inner = Arc::clone(&inner);
                        let _ = thread::Builder::new()
                            .name("smelt-peer-req".into())
                            .spawn(move || handle_client(inner, stream));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => return,
                }
            }
            if inner.stop.load(Ordering::SeqCst) {
                return;
            }
            let ready = match poll_ready(&[&listener, &wake_rx], None) {
                Ok(ready) => ready,
                Err(_) => return,
            };
            if ready.get(1) == Some(&true) {
                drain_wake(&mut wake_rx);
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = &mut wake_rx;
        loop {
            if inner.stop.load(Ordering::SeqCst) {
                return;
            }
            match listener.accept() {
                Ok(stream) => {
                    if inner.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let inner = Arc::clone(&inner);
                    let _ = thread::Builder::new()
                        .name("smelt-peer-req".into())
                        .spawn(move || handle_client(inner, stream));
                }
                Err(_) => return,
            }
        }
    }
}

fn handle_client(inner: Arc<PeerSocketInner>, stream: TaskStream) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut stream = reader.into_inner();
    let reply = match serde_json::from_str::<serde_json::Value>(line.trim()) {
        Ok(request) => {
            let op = request
                .get("op")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if op == "mcp_hello" {
                inner.mcp_connected.store(true, Ordering::SeqCst);
                serde_json::json!({"ok": true})
            } else {
                let mut body = request.clone();
                if let Some(fields) = body.as_object_mut() {
                    fields.remove("op");
                    fields.remove("owner");
                }
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    (inner.handler)(op, &body)
                })) {
                    Ok(reply) => reply,
                    Err(_) => serde_json::json!({
                        "ok": false,
                        "error": "Something went wrong handling this request"
                    }),
                }
            }
        }
        Err(error) => serde_json::json!({
            "ok": false,
            "error": format!("invalid request: {error}")
        }),
    };
    let _ = stream.write_all(reply.to_string().as_bytes());
    let _ = stream.write_all(b"\n");
    let _ = stream.flush();
}

/// 客户端（`smeltd --peer-mcp`）向 [`PeerSocket`] 发一次短请求并读回一行 JSON 响应。
pub fn call_peer_socket(
    socket_path: &Path,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    #[cfg(unix)]
    let mut stream = std::os::unix::net::UnixStream::connect(socket_path)
        .map_err(|error| format!("couldn't connect to peer socket: {error}"))?;
    #[cfg(windows)]
    let mut stream = {
        let name = socket_path
            .to_str()
            .ok_or_else(|| "peer socket path is not valid UTF-8".to_string())?;
        smelt_sys::named_pipe::PipeStream::connect(name)
            .map_err(|error| format!("couldn't connect to peer pipe: {error}"))?
    };
    let payload = format!("{request}\n");
    stream
        .write_all(payload.as_bytes())
        .map_err(|error| format!("couldn't write peer request: {error}"))?;
    stream
        .flush()
        .map_err(|error| format!("couldn't flush peer request: {error}"))?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .map_err(|error| format!("couldn't read peer response: {error}"))?;
    if read == 0 {
        return Err("peer socket closed without a response".to_string());
    }
    serde_json::from_str(line.trim())
        .map_err(|error| format!("invalid JSON from peer socket: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_socket_marks_mcp_hello_and_forwards_peer_ops() {
        let connected = Arc::new(AtomicBool::new(false));
        let handler: PeerRequestHandler = Arc::new(|op, body| {
            serde_json::json!({
                "ok": true,
                "echoOp": op,
                "query": body.get("query").cloned().unwrap_or(serde_json::Value::Null),
            })
        });
        let sock = PeerSocket::start(handler, Arc::clone(&connected)).expect("start peer socket");
        assert!(!connected.load(Ordering::SeqCst));

        let hello = call_peer_socket(sock.socket_path(), &serde_json::json!({"op": "mcp_hello"}))
            .expect("mcp_hello");
        assert_eq!(hello["ok"], true);
        assert!(connected.load(Ordering::SeqCst));

        let listed = call_peer_socket(
            sock.socket_path(),
            &serde_json::json!({"op": "session_list", "query": "codex"}),
        )
        .expect("session_list");
        assert_eq!(listed["echoOp"], "session_list");
        assert_eq!(listed["query"], "codex");

        let path = sock.socket_path().to_path_buf();
        drop(sock);
        #[cfg(unix)]
        assert!(!path.exists(), "Drop 应清掉套接字文件");
    }
}
