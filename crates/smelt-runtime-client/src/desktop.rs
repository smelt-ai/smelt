//! 远程桌面的本机转发口，桌面和手机共用。协议见 [`crate::protocol::desktop`]。
//!
//! 客户端用网页视图里的 noVNC 看远程桌面。网页只会说 WebSocket，运行时那头是原始 RFB
//! 字节流，中间这一层就是 websockify 的角色：在 127.0.0.1 上开一个口，提供 noVNC 页面，
//! 把页面的 WebSocket 换成到运行时的 `desktop_attach` 连接，两边原样转发。
//!
//! 这个口本机任何程序、浏览器里任何网页都连得到，所以 WebSocket 必须带上启动时生成的
//! 令牌，且来源必须是这个口自己的页面。

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use rust_embed::RustEmbed;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{Message, WebSocket};

use crate::protocol::daemon::DaemonOperation;
use crate::protocol::desktop::DesktopAttachReply;
use crate::protocol::write_json_line;
use crate::{RuntimeClient, RuntimeStream};

/// noVNC（上游 1.7.0，版本和校验值见 VERSION，许可见 LICENSE.txt）和 Smelt 自己的宿主页
/// `desktop.html`。
#[derive(RustEmbed)]
#[folder = "assets/novnc/"]
struct Assets;

/// 连上运行时、等它答复的时限。只管握手，之后的长连接不设时限。
const ATTACH_TIMEOUT: Duration = Duration::from_secs(10);
/// 单次从网页或运行时读取的上限。
const CHUNK_BYTES: usize = 64 * 1024;
/// 转发队列最多排几块。页面跟不上时读线程停在这里，压力一路传回运行时，内存不会涨；
/// 键鼠和画面共用这条队列，所以不能排太多，否则操作要等排在前面的画面发完。写运行时不走
/// 这条队列，见 [`spawn_runtime_writer`]。
const QUEUE_CHUNKS: usize = 8;

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
    // 键鼠事件很小，不能让 Nagle 攒着等上一个包的确认。
    if stream.set_nodelay(true).is_err() {
        return;
    }
    let Ok(mut ws) = tungstenite::accept_hdr(PageSocket::new(stream), check) else {
        return;
    };
    let runtime = match attach(&shared.client, ATTACH_TIMEOUT) {
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

/// 连上运行时的远程桌面，回 ok 之后这条连接就是 RFB 字节流。`timeout` 只管握手：画面静止时
/// 运行时可以很久不发数据，留着时限会把静止的桌面当成断线。
fn attach(client: &RuntimeClient, timeout: Duration) -> Result<RuntimeStream, String> {
    let mut stream = client
        .connect_with_timeout(timeout)
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
        stream
            .set_read_timeout(None)
            .and_then(|()| stream.set_write_timeout(None))
            .map_err(|error| format!("连不上运行时：{error}"))?;
        Ok(stream)
    } else {
        Err(reply.error.unwrap_or_else(|| "运行时拒绝了".to_string()))
    }
}

/// WebSocket 用的底层连接。握手时直接读 TCP；握手之后读线程把收到的字节喂进 `inbound`，
/// 这里读空了报 WouldBlock，tungstenite 会留着半帧等下次。写一直直接写 TCP。
struct PageSocket {
    tcp: TcpStream,
    fed: bool,
    inbound: VecDeque<u8>,
}

impl PageSocket {
    fn new(tcp: TcpStream) -> Self {
        Self {
            tcp,
            fed: false,
            inbound: VecDeque::new(),
        }
    }
}

impl Read for PageSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.fed {
            return self.tcp.read(buf);
        }
        if self.inbound.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.inbound.read(buf)
    }
}

impl Write for PageSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tcp.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tcp.flush()
    }
}

enum PumpEvent {
    FromPage(Vec<u8>),
    FromRuntime(Vec<u8>),
    /// 有一边读到头或出错了。排在它前面的字节都已经处理完。
    Closed,
}

/// 把 `source` 读到的字节按顺序放进事件队列，结束时放一个 `Closed`。
fn spawn_reader(
    name: &str,
    mut source: impl Read + Send + 'static,
    wrap: fn(Vec<u8>) -> PumpEvent,
    events: mpsc::SyncSender<PumpEvent>,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let mut buf = vec![0u8; CHUNK_BYTES];
            loop {
                let event = match source.read(&mut buf) {
                    Ok(0) => PumpEvent::Closed,
                    Ok(n) => wrap(buf[..n].to_vec()),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => PumpEvent::Closed,
                };
                let closed = matches!(event, PumpEvent::Closed);
                if events.send(event).is_err() || closed {
                    return;
                }
            }
        })
        .map(drop)
}

fn spawn_readers(
    page: &TcpStream,
    runtime: &RuntimeStream,
    events: mpsc::SyncSender<PumpEvent>,
) -> io::Result<()> {
    let page = page.try_clone()?;
    let runtime = runtime.try_clone()?;
    spawn_reader(
        "smelt-desktop-up",
        page,
        PumpEvent::FromPage,
        events.clone(),
    )?;
    spawn_reader(
        "smelt-desktop-down",
        runtime,
        PumpEvent::FromRuntime,
        events,
    )
}

/// 写运行时单开一个线程，转发线程不能卡在这里：对端可能要等自己的数据被读走才接着读，而读
/// 它的数据正要靠转发线程腾出队列，三方就互相等死了。这条队列不设上限：上行只有本机页面的
/// 键鼠和剪贴板，量很小。写失败就关掉连接，下行读线程读到头后整条转发随之结束。
fn spawn_runtime_writer(runtime: &RuntimeStream) -> io::Result<mpsc::Sender<Vec<u8>>> {
    let mut to_runtime = runtime.try_clone()?;
    let (sender, chunks) = mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new()
        .name("smelt-desktop-write".into())
        .spawn(move || {
            for chunk in chunks {
                if to_runtime.write_all(&chunk).is_err() {
                    let _ = to_runtime.shutdown(Shutdown::Both);
                    return;
                }
            }
        })?;
    Ok(sender)
}

/// 两个读线程各自阻塞在自己的连接上，把字节放进同一条有界队列；当前线程阻塞在队列上，
/// 独占 WebSocket 做帧的编解码和写入。WebSocket 不能拆成两半分别读写，这样不用定时醒来
/// 就能同时等两边。
fn pump(mut ws: WebSocket<PageSocket>, runtime: RuntimeStream) {
    ws.get_mut().fed = true;
    let (events, inbox) = mpsc::sync_channel(QUEUE_CHUNKS);
    let to_runtime = spawn_readers(&ws.get_ref().tcp, &runtime, events)
        .and_then(|()| spawn_runtime_writer(&runtime));
    if let Ok(to_runtime) = to_runtime {
        // 握手时可能连带读到了第一帧，它在 WebSocket 自己的缓冲里，先处理掉。
        let mut open = forward_page_frames(&mut ws, &to_runtime);
        while open {
            open = match inbox.recv() {
                Ok(PumpEvent::FromPage(bytes)) => {
                    ws.get_mut().inbound.extend(bytes);
                    forward_page_frames(&mut ws, &to_runtime)
                }
                Ok(PumpEvent::FromRuntime(chunk)) => ws.send(Message::binary(chunk)).is_ok(),
                Ok(PumpEvent::Closed) | Err(_) => false,
            };
        }
    }
    // 写线程和下行读线程手里都有这条连接，关掉才能把它们叫醒。
    let _ = runtime.shutdown(Shutdown::Both);
    let _ = ws.close(None);
    let _ = ws.flush();
    // 读线程手里还有一份连接，关掉才能把它们从阻塞的读里叫醒。
    let _ = ws.get_ref().tcp.shutdown(Shutdown::Both);
}

/// 把已经收到的完整帧转给运行时。连接还能继续用时返回 `true`。
fn forward_page_frames(ws: &mut WebSocket<PageSocket>, to_runtime: &mpsc::Sender<Vec<u8>>) -> bool {
    loop {
        match ws.read() {
            Ok(Message::Binary(bytes)) => {
                if to_runtime.send(bytes.to_vec()).is_err() {
                    return false;
                }
            }
            Ok(Message::Close(_)) => return false,
            Ok(_) => {}
            Err(tungstenite::Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                return true;
            }
            Err(_) => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tungstenite::client::IntoClientRequest;

    /// 假运行时：回 ok，之后把收到的字节原样回过去。
    fn echo_runtime() -> RuntimeClient {
        RuntimeClient::new(move || {
            let (client, mut server) = RuntimeStream::pair()?;
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

    /// 画面静止时运行时很久不发数据：握手的时限不能留在长连接上，否则静止的桌面会被当成断线。
    #[test]
    fn the_attach_timeout_does_not_outlive_the_handshake() {
        const SHORT: Duration = Duration::from_millis(50);
        let mut stream = attach(&echo_runtime(), SHORT).unwrap();
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            let mut byte = [0u8; 1];
            let _ = done.send(
                stream
                    .read(&mut byte)
                    .map(drop)
                    .map_err(|error| error.kind()),
            );
        });
        assert_eq!(
            finished.recv_timeout(SHORT * 10),
            Err(mpsc::RecvTimeoutError::Timeout),
            "没有数据时读应该一直等着"
        );
    }

    /// 一条消息要分好几次读才能收齐：半帧要留到下次，不能丢。
    ///
    /// 回声运行时写不完就不读，这条同时盯着转发不能和它互相等死。
    #[test]
    fn a_message_split_across_reads_round_trips() {
        let proxy = DesktopProxy::start(echo_runtime()).unwrap();
        let (port, token) = port_and_token(&proxy);
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (mut ws, _) = tungstenite::client(
            ws_request(port, &token, &format!("http://127.0.0.1:{port}")),
            stream,
        )
        .unwrap();
        let payload: Vec<u8> = (0..200 * 1024).map(|i| i as u8).collect();
        ws.send(Message::binary(payload.clone())).unwrap();
        let mut got = Vec::new();
        while got.len() < payload.len() {
            if let Message::Binary(bytes) = ws.read().unwrap() {
                got.extend_from_slice(&bytes);
            }
        }
        assert_eq!(got, payload);
    }

    /// 网页不读了，运行时那头要被堵住，而不是全堆进转发口的内存。
    #[test]
    fn a_page_that_stops_reading_stalls_the_runtime() {
        const LIMIT: usize = 256 * 1024 * 1024;
        let (done, finished) = mpsc::channel();
        let client = RuntimeClient::new(move || {
            let (client, mut server) = RuntimeStream::pair()?;
            let done = done.clone();
            std::thread::spawn(move || {
                let mut byte = [0u8; 1];
                while server.read(&mut byte).unwrap_or(0) == 1 && byte[0] != b'\n' {}
                write_json_line(
                    &mut server,
                    &DesktopAttachReply {
                        ok: true,
                        error: None,
                    },
                )
                .unwrap();
                server
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let chunk = vec![0u8; CHUNK_BYTES];
                let mut sent = 0;
                while sent < LIMIT && server.write_all(&chunk).is_ok() {
                    sent += chunk.len();
                }
                let _ = done.send(sent);
            });
            Ok(client)
        });
        let proxy = DesktopProxy::start(client).unwrap();
        let (port, token) = port_and_token(&proxy);
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_ws, _) = tungstenite::client(
            ws_request(port, &token, &format!("http://127.0.0.1:{port}")),
            stream,
        )
        .unwrap();
        let sent = finished.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(sent < LIMIT, "运行时一直写得进去：{sent} 字节");
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
