//! 浏览器画面流的运行时一侧（设计文档 D9）：连上常驻 Chrome 的 CDP，把正在看的那一页的
//! 画面推给查看的人；人接管时把鼠标键盘、导航转给浏览器。协议见
//! [`smelt_runtime_client::protocol::browser`]。
//!
//! 每个查看者各开一条到页面的 CDP 连接（Chrome 允许多个客户端同时连，和 agent 用的
//! Playwright 互不影响），一个线程轮流处理 CDP 消息和查看者发来的帧。画面一次只有一帧
//! 在途：查看者确认了才回 Chrome 的 `screencastFrameAck`，慢网络自动降帧。
//!
//! 不接管时跟着浏览器的前台标签走（`/json/list` 按最近激活排序，第一个就是前台）：agent
//! 新开了标签页，画面就切过去。接管时停在人选的那一页。

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};
use smelt_runtime_client::protocol::browser::{
    BrowserAttachReply, BrowserCommand, BrowserInput, BrowserState, BrowserTab, CLIENT_COMMAND,
    CLIENT_FRAME_ACK, CLIENT_INPUT, KeyEventKind, MouseButton, MouseEventKind, SERVER_FRAME,
    SERVER_STATE, encode_screen_frame, navigable_url, read_frame, write_frame, write_json_frame,
};
use smelt_runtime_client::protocol::write_json_line;
use tungstenite::{Message, WebSocket};

/// Chrome 远程调试端口：常驻浏览器在 Xvfb 里起，`--remote-debugging-port=9222` 只听本机。
/// 别的地址用 `SMELT_BROWSER_CDP` 指定。
const DEFAULT_CDP_ENDPOINT: &str = "127.0.0.1:9222";

/// CDP 连接的读超时：线程在等 CDP 消息和处理查看者的帧之间轮转，这也是输入的最长延迟。
const POLL: Duration = Duration::from_millis(15);
/// 多久重新看一次标签页列表。
const TABS_EVERY: Duration = Duration::from_secs(1);

pub(crate) fn cdp_endpoint() -> String {
    std::env::var("SMELT_BROWSER_CDP").unwrap_or_else(|_| DEFAULT_CDP_ENDPOINT.to_string())
}

pub(crate) fn handle_browser_attach(
    mut conn: UnixStream,
    reader: BufReader<UnixStream>,
    endpoint: &str,
) {
    let tabs = match page_targets(endpoint) {
        Ok(tabs) if !tabs.is_empty() => tabs,
        Ok(_) => return reply_error(&mut conn, "浏览器里没有打开的页面".to_string()),
        Err(error) => {
            return reply_error(
                &mut conn,
                format!("连不上这台机器上的浏览器（{endpoint}）：{error}"),
            );
        }
    };
    if write_json_line(
        &mut conn,
        &BrowserAttachReply {
            ok: true,
            error: None,
        },
    )
    .is_err()
    {
        return;
    }
    let (events, inbox) = mpsc::channel();
    std::thread::Builder::new()
        .name("smelt-browser-viewer".into())
        .spawn(move || read_viewer(reader, events))
        .ok();
    let mut session = Session {
        endpoint: endpoint.to_string(),
        out: conn,
        inbox,
        page: None,
        tabs,
        controlling: false,
        frame_in_flight: None,
        next_id: 1,
        tabs_at: Instant::now(),
        sent: None,
    };
    let first = session.tabs[0].id.clone();
    if session.switch_to(&first).is_ok() {
        session.run();
    }
    session.stop_page();
}

fn reply_error(conn: &mut UnixStream, error: String) {
    let _ = write_json_line(
        conn,
        &BrowserAttachReply {
            ok: false,
            error: Some(error),
        },
    );
}

enum ViewerEvent {
    FrameAck,
    Input(BrowserInput),
    Command(BrowserCommand),
    Gone,
}

fn read_viewer(mut reader: BufReader<UnixStream>, events: mpsc::Sender<ViewerEvent>) {
    loop {
        let event = match read_frame(&mut reader) {
            Ok(Some((CLIENT_FRAME_ACK, _))) => ViewerEvent::FrameAck,
            Ok(Some((CLIENT_INPUT, payload))) => match serde_json::from_slice(&payload) {
                Ok(input) => ViewerEvent::Input(input),
                Err(_) => continue,
            },
            Ok(Some((CLIENT_COMMAND, payload))) => match serde_json::from_slice(&payload) {
                Ok(command) => ViewerEvent::Command(command),
                Err(_) => continue,
            },
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => ViewerEvent::Gone,
        };
        let gone = matches!(event, ViewerEvent::Gone);
        if events.send(event).is_err() || gone {
            return;
        }
    }
}

struct Page {
    id: String,
    ws: WebSocket<TcpStream>,
}

struct Session {
    endpoint: String,
    out: UnixStream,
    inbox: mpsc::Receiver<ViewerEvent>,
    page: Option<Page>,
    tabs: Vec<BrowserTab>,
    controlling: bool,
    /// 发给查看者、还没确认的那一帧对应的 screencast session id。
    frame_in_flight: Option<i64>,
    next_id: u64,
    tabs_at: Instant,
    sent: Option<BrowserState>,
}

impl Session {
    /// 查看者走了或写不出去时返回。
    fn run(&mut self) {
        loop {
            loop {
                match self.inbox.try_recv() {
                    Ok(ViewerEvent::Gone) | Err(mpsc::TryRecvError::Disconnected) => return,
                    Ok(event) => {
                        if self.on_viewer(event).is_err() {
                            return;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                }
            }
            if self.read_cdp().is_err() {
                return;
            }
            if self.tabs_at.elapsed() >= TABS_EVERY && self.refresh_tabs().is_err() {
                return;
            }
        }
    }

    fn state(&self) -> BrowserState {
        BrowserState {
            tabs: self.tabs.clone(),
            active: self.page.as_ref().map(|page| page.id.clone()),
            controlling: self.controlling,
        }
    }

    fn send_state(&mut self) -> io::Result<()> {
        let state = self.state();
        if self.sent.as_ref() == Some(&state) {
            return Ok(());
        }
        write_json_frame(&mut self.out, SERVER_STATE, &state)?;
        self.sent = Some(state);
        Ok(())
    }

    /// 读一条 CDP 消息（最多等 [`POLL`]）。只有写不到查看者才返回错误；页面连接断了就丢掉，
    /// 等下次看标签页时重连。
    fn read_cdp(&mut self) -> io::Result<()> {
        let Some(page) = self.page.as_mut() else {
            std::thread::sleep(POLL);
            return Ok(());
        };
        let text = match page.ws.read() {
            Ok(Message::Text(text)) => text,
            Ok(_) => return Ok(()),
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(());
            }
            Err(_) => {
                self.page = None;
                self.frame_in_flight = None;
                self.tabs_at = Instant::now() - TABS_EVERY;
                return Ok(());
            }
        };
        let Ok(message) = serde_json::from_str::<Value>(&text) else {
            return Ok(());
        };
        if message["method"] == "Page.screencastFrame" {
            let params = &message["params"];
            let Some(session_id) = params["sessionId"].as_i64() else {
                return Ok(());
            };
            let Ok(jpeg) = base64::engine::general_purpose::STANDARD
                .decode(params["data"].as_str().unwrap_or(""))
            else {
                return Ok(());
            };
            let width = params["metadata"]["deviceWidth"]
                .as_f64()
                .unwrap_or(0.0)
                .round() as u32;
            let height = params["metadata"]["deviceHeight"]
                .as_f64()
                .unwrap_or(0.0)
                .round() as u32;
            write_frame(
                &mut self.out,
                SERVER_FRAME,
                &encode_screen_frame(width, height, &jpeg),
            )?;
            self.frame_in_flight = Some(session_id);
        }
        Ok(())
    }

    fn on_viewer(&mut self, event: ViewerEvent) -> io::Result<()> {
        match event {
            ViewerEvent::FrameAck => {
                if let Some(session_id) = self.frame_in_flight.take() {
                    self.cdp(
                        "Page.screencastFrameAck",
                        json!({ "sessionId": session_id }),
                    );
                }
            }
            ViewerEvent::Input(input) if self.controlling => {
                let (method, params) = cdp_input(&input);
                self.cdp(method, params);
            }
            ViewerEvent::Command(BrowserCommand::Control { on }) => {
                self.controlling = on;
                self.send_state()?;
            }
            ViewerEvent::Command(command) if self.controlling => self.on_command(command)?,
            ViewerEvent::Input(_) | ViewerEvent::Command(_) | ViewerEvent::Gone => {}
        }
        Ok(())
    }

    fn on_command(&mut self, command: BrowserCommand) -> io::Result<()> {
        match command {
            BrowserCommand::Navigate { url } if navigable_url(&url) => {
                self.cdp("Page.navigate", json!({ "url": url.trim() }));
            }
            BrowserCommand::Back => self.evaluate("history.back()"),
            BrowserCommand::Forward => self.evaluate("history.forward()"),
            BrowserCommand::Reload => self.cdp("Page.reload", json!({})),
            BrowserCommand::ActivateTab { id } => {
                if devtools_http(&self.endpoint, "GET", &format!("/json/activate/{id}")).is_ok() {
                    self.switch_to(&id)?;
                }
            }
            BrowserCommand::NewTab => {
                if let Ok(body) = devtools_http(&self.endpoint, "PUT", "/json/new?about:blank")
                    && let Ok(target) = serde_json::from_str::<Target>(&body)
                {
                    self.refresh_tabs()?;
                    self.switch_to(&target.id)?;
                }
            }
            BrowserCommand::CloseTab { id } => {
                let _ = devtools_http(&self.endpoint, "GET", &format!("/json/close/{id}"));
                self.refresh_tabs()?;
            }
            BrowserCommand::Navigate { .. } | BrowserCommand::Control { .. } => {}
        }
        Ok(())
    }

    fn evaluate(&mut self, expression: &str) {
        self.cdp("Runtime.evaluate", json!({ "expression": expression }));
    }

    /// 发一条 CDP 命令，不等回包：回包和事件混在同一条流里，读的时候按方法名挑事件，
    /// 其余丢掉。写失败说明页面连接断了，交给下一次读去收拾。
    fn cdp(&mut self, method: &str, params: Value) {
        let id = self.next_id;
        self.next_id += 1;
        if let Some(page) = self.page.as_mut() {
            let message = json!({ "id": id, "method": method, "params": params }).to_string();
            let _ = page.ws.send(Message::text(message));
        }
    }

    /// 换到另一个标签页：断开旧页面，连上新页面开始推画面。
    fn switch_to(&mut self, id: &str) -> io::Result<()> {
        self.stop_page();
        let page = match connect_page(&self.endpoint, id) {
            Ok(page) => page,
            Err(_) => return self.send_state(),
        };
        self.page = Some(page);
        self.cdp(
            "Page.startScreencast",
            json!({
                "format": "jpeg",
                "quality": 60,
                "maxWidth": 1600,
                "maxHeight": 1600,
                "everyNthFrame": 1,
            }),
        );
        self.send_state()
    }

    fn stop_page(&mut self) {
        self.cdp("Page.stopScreencast", json!({}));
        if let Some(mut page) = self.page.take() {
            let _ = page.ws.close(None);
        }
        self.frame_in_flight = None;
    }

    fn refresh_tabs(&mut self) -> io::Result<()> {
        self.tabs_at = Instant::now();
        let Ok(tabs) = page_targets(&self.endpoint) else {
            return Ok(());
        };
        self.tabs = tabs;
        let active = self.page.as_ref().map(|page| page.id.clone());
        let front = self.tabs.first().map(|tab| tab.id.clone());
        let still_open = active
            .as_ref()
            .is_some_and(|id| self.tabs.iter().any(|tab| &tab.id == id));
        let target = if !still_open || !self.controlling {
            front
        } else {
            active.clone()
        };
        match target {
            Some(id) if Some(&id) != active.as_ref() => self.switch_to(&id),
            Some(_) => self.send_state(),
            None => {
                self.stop_page();
                self.send_state()
            }
        }
    }
}

/// 查看者的操作换成 CDP 命令。
fn cdp_input(input: &BrowserInput) -> (&'static str, Value) {
    match input {
        BrowserInput::Mouse {
            event,
            x,
            y,
            button,
            click_count,
            delta_x,
            delta_y,
            modifiers,
        } => (
            "Input.dispatchMouseEvent",
            json!({
                "type": match event {
                    MouseEventKind::Pressed => "mousePressed",
                    MouseEventKind::Released => "mouseReleased",
                    MouseEventKind::Moved => "mouseMoved",
                    MouseEventKind::Wheel => "mouseWheel",
                },
                "x": x,
                "y": y,
                "button": match button {
                    MouseButton::None => "none",
                    MouseButton::Left => "left",
                    MouseButton::Middle => "middle",
                    MouseButton::Right => "right",
                },
                "clickCount": click_count,
                "deltaX": delta_x,
                "deltaY": delta_y,
                "modifiers": modifiers,
            }),
        ),
        BrowserInput::Key {
            event,
            key,
            code,
            key_code,
            text,
            modifiers,
        } => {
            // 带文字的按下用 keyDown（回车会插入 "\r" 并提交表单），纯功能键用 rawKeyDown。
            let kind = match (event, text) {
                (KeyEventKind::Down, Some(_)) => "keyDown",
                (KeyEventKind::Down, None) => "rawKeyDown",
                (KeyEventKind::Up, _) => "keyUp",
            };
            let mut params = json!({
                "type": kind,
                "key": key,
                "code": code,
                "windowsVirtualKeyCode": key_code,
                "nativeVirtualKeyCode": key_code,
                "modifiers": modifiers,
            });
            if let (KeyEventKind::Down, Some(text)) = (event, text) {
                params["text"] = json!(text);
                params["unmodifiedText"] = json!(text);
            }
            ("Input.dispatchKeyEvent", params)
        }
        BrowserInput::Text { text } => ("Input.insertText", json!({ "text": text })),
    }
}

#[derive(Deserialize)]
struct Target {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
}

/// 浏览器里的页面，按最近激活排（第一个是前台标签）。
fn page_targets(endpoint: &str) -> io::Result<Vec<BrowserTab>> {
    let body = devtools_http(endpoint, "GET", "/json/list")?;
    let targets: Vec<Target> = serde_json::from_str(&body)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(targets
        .into_iter()
        .filter(|target| target.kind == "page")
        .map(|target| BrowserTab {
            id: target.id,
            title: target.title,
            url: target.url,
        })
        .collect())
}

fn connect(endpoint: &str) -> io::Result<TcpStream> {
    let addr = endpoint
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "地址解析不出来"))?;
    TcpStream::connect_timeout(&addr, Duration::from_secs(2))
}

/// Chrome 调试端口上的 HTTP 接口（`/json/list`、`/json/activate/…`）。Chrome 回完不关
/// 连接（不理会 `Connection: close`），所以按 Content-Length 读满正文就返回。
fn devtools_http(endpoint: &str, method: &str, path: &str) -> io::Result<String> {
    let stream = connect(endpoint)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    // 整个请求一次写出：`write!` 会按格式片段分几次 send，对端读到一半就回包关连接。
    let request =
        format!("{method} {path} HTTP/1.1\r\nHost: {endpoint}\r\nConnection: close\r\n\r\n");
    (&stream).write_all(request.as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status)?;
    let mut length = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "回包不完整"));
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    if !status
        .split_whitespace()
        .nth(1)
        .is_some_and(|code| code == "200")
    {
        return Err(io::Error::other(status.trim().to_string()));
    }
    let length = length
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "回包没有 Content-Length"))?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    String::from_utf8(body).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn connect_page(endpoint: &str, id: &str) -> io::Result<Page> {
    let stream = connect(endpoint)?;
    let url = format!("ws://{endpoint}/devtools/page/{id}");
    let (ws, _) = tungstenite::client::client(url, stream).map_err(io::Error::other)?;
    ws.get_ref().set_read_timeout(Some(POLL))?;
    Ok(Page {
        id: id.to_string(),
        ws,
    })
}

#[cfg(test)]
mod tests;
