use super::*;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// 假的 Chrome 调试端口：`/json/*` 回一个页面，页面的 WebSocket 收到开始推画面就出一帧，
/// 每收到一次帧确认再出下一帧。收到的 CDP 方法名按顺序记下来。
struct FakeChrome {
    endpoint: String,
    received: Arc<Mutex<Vec<String>>>,
}

fn fake_chrome() -> FakeChrome {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let received = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&received);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let log = Arc::clone(&log);
            std::thread::spawn(move || serve(stream, log));
        }
    });
    FakeChrome { endpoint, received }
}

fn serve(mut stream: TcpStream, log: Arc<Mutex<Vec<String>>>) {
    let mut head = [0u8; 16];
    let n = stream.peek(&mut head).unwrap_or(0);
    let head = String::from_utf8_lossy(&head[..n]).to_string();
    if !head.contains("/devtools/") {
        // 读完整个请求头再回，没读完就关会让对端收到 RST。
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap_or(0) == 1 {
            request.push(byte[0]);
        }
        let body = r#"[{"id":"page-1","type":"page","title":"首页","url":"https://example.com/"},
                       {"id":"sw","type":"service_worker","title":"","url":""}]"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        // 跟真 Chrome 一样回完不关连接：读的一方得按 Content-Length 收尾，不能等对端关。
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    let mut ws = tungstenite::accept(stream).unwrap();
    let mut frame = 0;
    let send_frame = |ws: &mut WebSocket<TcpStream>, frame: &mut i64| {
        *frame += 1;
        let data = base64::engine::general_purpose::STANDARD.encode(format!("frame{frame}"));
        let event = json!({
            "method": "Page.screencastFrame",
            "params": {
                "sessionId": *frame,
                "data": data,
                "metadata": { "deviceWidth": 800.0, "deviceHeight": 600.0 },
            }
        });
        ws.send(Message::text(event.to_string())).unwrap();
    };
    while let Ok(message) = ws.read() {
        let Message::Text(text) = message else {
            continue;
        };
        let value: Value = serde_json::from_str(text.as_str()).unwrap();
        let method = value["method"].as_str().unwrap_or_default().to_string();
        log.lock().unwrap().push(method.clone());
        match method.as_str() {
            "Page.startScreencast" | "Page.screencastFrameAck" => send_frame(&mut ws, &mut frame),
            _ => {}
        }
    }
}

struct Viewer {
    stream: UnixStream,
}

impl Viewer {
    fn attach(endpoint: &str) -> (Viewer, BrowserAttachReply) {
        let (client, server) = UnixStream::pair().unwrap();
        let endpoint = endpoint.to_string();
        std::thread::spawn(move || {
            let reader = BufReader::new(server.try_clone().unwrap());
            handle_browser_attach(server, reader, &endpoint);
        });
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        // 逐字节读到换行：后面紧跟着二进制帧，带缓冲读会把它们一起吞掉。
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        let mut reader = &client;
        while reader.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
            line.push(byte[0]);
        }
        (
            Viewer { stream: client },
            serde_json::from_slice(&line).unwrap(),
        )
    }

    /// 下一帧画面，跳过中间的状态帧；`wait` 内没有就是 None。
    fn next_screen(&mut self, wait: Duration) -> Option<(u32, u32, Vec<u8>)> {
        self.stream.set_read_timeout(Some(wait)).unwrap();
        loop {
            match read_frame(&mut self.stream) {
                Ok(Some((SERVER_FRAME, payload))) => {
                    let (w, h, jpeg) =
                        smelt_runtime_client::protocol::browser::decode_screen_frame(&payload)
                            .unwrap();
                    return Some((w, h, jpeg.to_vec()));
                }
                Ok(Some(_)) => continue,
                _ => return None,
            }
        }
    }

    fn send(&mut self, kind: u8, payload: &[u8]) {
        write_frame(&mut self.stream, kind, payload).unwrap();
    }
}

fn wait_for(received: &Arc<Mutex<Vec<String>>>, method: &str) -> usize {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let count = received
            .lock()
            .unwrap()
            .iter()
            .filter(|m| *m == method)
            .count();
        if count > 0 || Instant::now() > deadline {
            return count;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 画面一次只有一帧在途：查看者不确认，运行时就不回 Chrome 的帧确认，下一帧就不来。
#[test]
fn next_frame_waits_for_the_viewer_to_ack() {
    let chrome = fake_chrome();
    let (mut viewer, reply) = Viewer::attach(&chrome.endpoint);
    assert!(reply.ok, "{reply:?}");

    let (w, h, jpeg) = viewer.next_screen(Duration::from_secs(5)).expect("第一帧");
    assert_eq!((w, h, jpeg.as_slice()), (800, 600, &b"frame1"[..]));
    assert!(
        viewer.next_screen(Duration::from_millis(300)).is_none(),
        "没确认之前不能有第二帧"
    );

    viewer.send(CLIENT_FRAME_ACK, &[]);
    let (_, _, jpeg) = viewer
        .next_screen(Duration::from_secs(5))
        .expect("确认后的下一帧");
    assert_eq!(jpeg, b"frame2");
}

/// 只看不动：没接管时的鼠标和导航不转给浏览器，接管之后才转。
#[test]
fn input_only_reaches_the_browser_while_controlling() {
    let chrome = fake_chrome();
    let (mut viewer, reply) = Viewer::attach(&chrome.endpoint);
    assert!(reply.ok, "{reply:?}");
    viewer.next_screen(Duration::from_secs(5)).expect("第一帧");

    let click = serde_json::to_vec(&BrowserInput::Mouse {
        event: MouseEventKind::Pressed,
        x: 10.0,
        y: 20.0,
        button: MouseButton::Left,
        click_count: 1,
        delta_x: 0.0,
        delta_y: 0.0,
        modifiers: 0,
    })
    .unwrap();
    let navigate = serde_json::to_vec(&BrowserCommand::Navigate {
        url: "https://example.org/".into(),
    })
    .unwrap();
    viewer.send(CLIENT_INPUT, &click);
    viewer.send(CLIENT_COMMAND, &navigate);
    viewer.send(
        CLIENT_COMMAND,
        &serde_json::to_vec(&BrowserCommand::Control { on: true }).unwrap(),
    );
    viewer.send(CLIENT_INPUT, &click);
    // 不能打开的地址接管了也不转。
    viewer.send(
        CLIENT_COMMAND,
        &serde_json::to_vec(&BrowserCommand::Navigate {
            url: "file:///etc/passwd".into(),
        })
        .unwrap(),
    );
    viewer.send(CLIENT_COMMAND, &navigate);

    assert_eq!(wait_for(&chrome.received, "Page.navigate"), 1);
    let received = chrome.received.lock().unwrap().clone();
    assert_eq!(
        received
            .iter()
            .filter(|m| *m == "Input.dispatchMouseEvent")
            .count(),
        1,
        "{received:?}"
    );
}

#[test]
fn unreachable_browser_is_reported_to_the_viewer() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    drop(listener);
    let (_viewer, reply) = Viewer::attach(&endpoint);
    assert!(!reply.ok);
    assert!(reply.error.unwrap().contains("连不上"));
}

#[test]
fn keys_with_text_insert_it_and_function_keys_do_not() {
    let enter = BrowserInput::Key {
        event: KeyEventKind::Down,
        key: "Enter".into(),
        code: "Enter".into(),
        key_code: 13,
        text: Some("\r".into()),
        modifiers: 0,
    };
    let (method, params) = cdp_input(&enter);
    assert_eq!(method, "Input.dispatchKeyEvent");
    assert_eq!(params["type"], "keyDown");
    assert_eq!(params["text"], "\r");

    let left = BrowserInput::Key {
        event: KeyEventKind::Down,
        key: "ArrowLeft".into(),
        code: "ArrowLeft".into(),
        key_code: 37,
        text: None,
        modifiers: 0,
    };
    let (_, params) = cdp_input(&left);
    assert_eq!(params["type"], "rawKeyDown");
    assert!(params.get("text").is_none());
}
