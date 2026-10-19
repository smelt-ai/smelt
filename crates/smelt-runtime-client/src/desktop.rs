//! 远程桌面的本机转发口，桌面和手机共用。协议见 [`crate::protocol::desktop`]。
//!
//! 客户端用网页视图里的 noVNC 看远程桌面。网页只会说 WebSocket，运行时那头是原始 RFB
//! 字节流，中间这一层就是 websockify 的角色：在 127.0.0.1 上开一个口，提供 noVNC 页面，
//! 把页面的 WebSocket 换成到运行时的 `desktop_attach` 连接，两边原样转发。
//!
//! 这个口本机任何程序、浏览器里任何网页都连得到，所以 WebSocket 必须带上启动时生成的
//! 令牌，且来源必须是这个口自己的页面。

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use rust_embed::RustEmbed;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{Message, WebSocket};

use crate::RuntimeClient;
use crate::protocol::daemon::DaemonOperation;
use crate::protocol::desktop::DesktopAttachReply;
use crate::protocol::write_json_line;

/// noVNC（Debian 的 1.6.0 包，MPL-2.0，见 COPYRIGHT）和 Smelt 自己的宿主页 `desktop.html`。
#[derive(RustEmbed)]
#[folder = "assets/novnc/"]
struct Assets;

/// WebSocket 一边的读超时：一个线程在读网页发来的帧和转发运行时的字节之间轮转，这也是
/// 鼠标键盘的最长额外延迟。
const POLL: Duration = Duration::from_millis(10);

pub struct DesktopProxy {
    port: u16,
    token: String,
    stop: Arc<AtomicBool>,
}

impl DesktopProxy {
    /// 在 127.0.0.1 的随机端口上开转发口，之后每条 WebSocket 都连到 `client` 那台运行时。
    pub fn start(client: RuntimeClient) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let token = uuid::Uuid::new_v4().simple().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            client,
            token: token.clone(),
            origin: format!("http://127.0.0.1:{port}"),
        });
        let stopped = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("smelt-desktop-proxy".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stopped.load(Ordering::Acquire) {
                        return;
                    }
                    let Ok(stream) = stream else { continue };
                    let shared = Arc::clone(&shared);
                    let _ = std::thread::Builder::new()
                        .name("smelt-desktop-conn".into())
                        .spawn(move || serve(stream, &shared));
                }
            })?;
        Ok(Self { port, token, stop })
    }

    /// 给网页视图打开的地址。`control` 为真时页面一打开就是接管状态。
    pub fn page_url(&self, control: bool) -> String {
        format!(
            "http://127.0.0.1:{}/desktop.html?token={}{}",
            self.port,
            self.token,
            if control { "&control=1" } else { "" }
        )
    }
}

impl Drop for DesktopProxy {
    /// 停掉接受循环：它卡在 accept 上，连自己一下把它叫醒。已经接上的连接随网页关掉而断。
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

struct Shared {
    client: RuntimeClient,
    token: String,
    origin: String,
}

fn serve(stream: TcpStream, shared: &Shared) {
    let mut head = [0u8; 8];
    let n = stream.peek(&mut head).unwrap_or(0);
    if head[..n].starts_with(b"GET /ws") {
        bridge(stream, shared);
    } else {
        serve_asset(stream);
    }
}

/// 只读文件：读完请求头，按路径回嵌入的文件。
fn serve_asset(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut request = Vec::new();
    let mut byte = [0u8; 1];
    while !request.ends_with(b"\r\n\r\n") && request.len() < 16 * 1024 {
        match stream.read(&mut byte) {
            Ok(1) => request.push(byte[0]),
            _ => return,
        }
    }
    let line = String::from_utf8_lossy(&request);
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .trim_start_matches('/');
    let response = match line
        .starts_with("GET ")
        .then(|| Assets::get(path))
        .flatten()
    {
        Some(file) => {
            let mut response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                content_type(path),
                file.data.len()
            )
            .into_bytes();
            response.extend_from_slice(&file.data);
            response
        }
        None => {
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        }
    };
    let _ = stream.write_all(&response);
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        // ES 模块必须是 JavaScript 类型，否则网页视图拒绝执行。
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// WebSocket 一边：核对令牌和来源，连上运行时的远程桌面，之后两边原样转发。
// 握手回调的错误类型是 tungstenite 定的完整 HTTP 响应，大也只能这样。
#[allow(clippy::result_large_err)]
fn bridge(stream: TcpStream, shared: &Shared) {
    let check = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        let token_ok = request.uri().query().is_some_and(|query| {
            query
                .split('&')
                .any(|pair| pair == format!("token={}", shared.token))
        });
        let origin_ok = request
            .headers()
            .get("origin")
            .is_some_and(|origin| origin.as_bytes() == shared.origin.as_bytes());
        if token_ok && origin_ok {
            Ok(response)
        } else {
            let mut refused = ErrorResponse::new(Some("拒绝".into()));
            *refused.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
            Err(refused)
        }
    };
    let Ok(mut ws) = tungstenite::accept_hdr(stream, check) else {
        return;
    };
    let runtime = match attach(&shared.client) {
        Ok(runtime) => runtime,
        Err(reason) => {
            let _ = ws.close(Some(tungstenite::protocol::CloseFrame {
                code: tungstenite::protocol::frame::coding::CloseCode::Error,
                reason: reason.into(),
            }));
            let _ = ws.flush();
            return;
        }
    };
    pump(ws, runtime);
}

/// 连上运行时的远程桌面，回 ok 之后这条连接就是 RFB 字节流。
fn attach(client: &RuntimeClient) -> Result<UnixStream, String> {
    let mut stream = client
        .connect_with_timeout(Duration::from_secs(10))
        .map_err(|error| format!("连不上运行时：{error}"))?;
    write_json_line(
        &mut stream,
        &serde_json::json!({ "op": DaemonOperation::DesktopAttach }),
    )
    .map_err(|error| format!("连不上运行时：{error}"))?;
    // 逐字节读回复行：后面紧跟着 RFB 字节，带缓冲读会把它们一起吞掉。
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(1) if byte[0] == b'\n' => break,
            Ok(1) if line.len() < 64 * 1024 => line.push(byte[0]),
            _ => return Err("运行时没回应".to_string()),
        }
    }
    let reply: DesktopAttachReply =
        serde_json::from_slice(&line).map_err(|_| "运行时不支持远程桌面，需要升级".to_string())?;
    if reply.ok {
        Ok(stream)
    } else {
        Err(reply.error.unwrap_or_else(|| "运行时拒绝了".to_string()))
    }
}

/// 一个线程把运行时的字节读进通道，另一个（当前）线程轮流读网页的帧、把通道里的字节
/// 发给网页。WebSocket 不能拆成两半分别读写，所以写都在这一个线程。
fn pump(mut ws: WebSocket<TcpStream>, runtime: UnixStream) {
    if ws.get_ref().set_read_timeout(Some(POLL)).is_err() {
        return;
    }
    let (chunks, inbox) = mpsc::channel::<Vec<u8>>();
    let Ok(mut from_runtime) = runtime.try_clone() else {
        return;
    };
    std::thread::Builder::new()
        .name("smelt-desktop-down".into())
        .spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match from_runtime.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        if chunks.send(buf[..n].to_vec()).is_err() {
                            return;
                        }
                    }
                }
            }
        })
        .ok();
    let mut to_runtime = runtime;
    'pump: loop {
        loop {
            match inbox.try_recv() {
                Ok(chunk) => {
                    if ws.send(Message::binary(chunk)).is_err() {
                        break 'pump;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break 'pump,
            }
        }
        match ws.read() {
            Ok(Message::Binary(bytes)) => {
                if to_runtime.write_all(&bytes).is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    let _ = to_runtime.shutdown(Shutdown::Both);
    let _ = ws.close(None);
    let _ = ws.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tungstenite::client::IntoClientRequest;

    /// 假运行时：回 ok，之后把收到的字节原样回过去。
    fn echo_runtime() -> RuntimeClient {
        RuntimeClient::new(move || {
            let (client, mut server) = UnixStream::pair()?;
            std::thread::spawn(move || {
                let mut line = Vec::new();
                let mut byte = [0u8; 1];
                while server.read(&mut byte).unwrap_or(0) == 1 && byte[0] != b'\n' {
                    line.push(byte[0]);
                }
                let request: serde_json::Value = serde_json::from_slice(&line).unwrap();
                assert_eq!(request["op"], "desktop_attach");
                write_json_line(
                    &mut server,
                    &DesktopAttachReply {
                        ok: true,
                        error: None,
                    },
                )
                .unwrap();
                let mut reader = server.try_clone().unwrap();
                let _ = io::copy(&mut reader, &mut server);
            });
            Ok(client)
        })
    }

    fn port_and_token(proxy: &DesktopProxy) -> (u16, String) {
        (proxy.port, proxy.token.clone())
    }

    fn ws_request(port: u16, token: &str, origin: &str) -> tungstenite::handshake::client::Request {
        let mut request = format!("ws://127.0.0.1:{port}/ws?token={token}")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
        request
    }

    #[test]
    fn websocket_bytes_round_trip_through_the_runtime() {
        let proxy = DesktopProxy::start(echo_runtime()).unwrap();
        let (port, token) = port_and_token(&proxy);
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (mut ws, _) = tungstenite::client(
            ws_request(port, &token, &format!("http://127.0.0.1:{port}")),
            stream,
        )
        .unwrap();
        ws.send(Message::binary(b"RFB 003.008\n".to_vec())).unwrap();
        let mut got = Vec::new();
        while got.len() < 12 {
            if let Message::Binary(bytes) = ws.read().unwrap() {
                got.extend_from_slice(&bytes);
            }
        }
        assert_eq!(got, b"RFB 003.008\n");
    }

    /// 本机别的网页或程序拿不到令牌、来源也不对，连不上。
    #[test]
    fn wrong_token_or_foreign_origin_is_refused() {
        let attached = Arc::new(Mutex::new(0));
        let counter = Arc::clone(&attached);
        let client = RuntimeClient::new(move || {
            *counter.lock().unwrap() += 1;
            Err(io::Error::other("不该连运行时"))
        });
        let proxy = DesktopProxy::start(client).unwrap();
        let (port, token) = port_and_token(&proxy);
        for (token, origin) in [
            ("wrong", format!("http://127.0.0.1:{port}")),
            (token.as_str(), "https://evil.example".to_string()),
        ] {
            let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            assert!(tungstenite::client(ws_request(port, token, &origin), stream).is_err());
        }
        assert_eq!(*attached.lock().unwrap(), 0);
    }

    #[test]
    fn page_and_modules_are_served_with_the_right_types() {
        let proxy = DesktopProxy::start(echo_runtime()).unwrap();
        let (port, _) = port_and_token(&proxy);
        for (path, expect) in [
            ("/desktop.html", "text/html"),
            ("/core/rfb.js", "text/javascript"),
        ] {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            assert!(
                response.starts_with("HTTP/1.1 200"),
                "{path}: {}",
                &response[..40]
            );
            assert!(response.contains(expect), "{path}");
        }
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(b"GET /../Cargo.toml HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 404"));
    }
}
