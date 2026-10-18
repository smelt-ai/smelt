//! 浏览器画面流的客户端，桌面和手机共用。协议见 [`crate::protocol::browser`]。
//!
//! [`BrowserConnection::attach`] 连上以后起一个后台线程收帧，交给调用方的回调；帧确认、
//! 输入、命令从 [`BrowserConnection`] 发。画完一帧记得 [`BrowserConnection::ack_frame`]，
//! 不确认运行时就不会推下一帧。

use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::RuntimeClient;
use crate::protocol::browser::{
    BrowserAttachReply, BrowserCommand, BrowserInput, BrowserState, CLIENT_COMMAND,
    CLIENT_FRAME_ACK, CLIENT_INPUT, SERVER_FRAME, SERVER_STATE, decode_screen_frame, read_frame,
    write_frame, write_json_frame,
};
use crate::protocol::daemon::DaemonOperation;
use crate::protocol::write_json_line;

pub enum BrowserEvent {
    /// 一帧画面：页面 CSS 视口的宽高，和 JPEG。
    Frame {
        css_width: u32,
        css_height: u32,
        jpeg: Vec<u8>,
    },
    State(BrowserState),
    /// 连接断了。`None` 是运行时正常关掉。
    Closed(Option<String>),
}

pub struct BrowserConnection {
    writer: Mutex<UnixStream>,
}

impl BrowserConnection {
    /// 连上运行时的浏览器。阻塞，放后台调。运行时那边没有浏览器时返回它给的原因。
    pub fn attach(
        client: &RuntimeClient,
        mut on_event: impl FnMut(BrowserEvent) + Send + 'static,
    ) -> Result<Arc<Self>, String> {
        let mut stream = client
            .connect_with_timeout(Duration::from_secs(10))
            .map_err(|error| format!("连不上运行时：{error}"))?;
        write_json_line(
            &mut stream,
            &serde_json::json!({ "op": DaemonOperation::BrowserAttach }),
        )
        .map_err(|error| format!("连不上运行时：{error}"))?;
        let reply =
            read_reply_line(&mut stream).map_err(|error| format!("运行时没回应：{error}"))?;
        let reply: BrowserAttachReply = serde_json::from_slice(&reply)
            .map_err(|_| "运行时不支持浏览器画面，需要升级".to_string())?;
        if !reply.ok {
            return Err(reply.error.unwrap_or_else(|| "运行时拒绝了".to_string()));
        }
        let mut reader = stream.try_clone().map_err(|error| error.to_string())?;
        std::thread::Builder::new()
            .name("smelt-browser-stream".into())
            .spawn(move || {
                loop {
                    match read_frame(&mut reader) {
                        Ok(Some((SERVER_FRAME, payload))) => {
                            if let Some((css_width, css_height, jpeg)) =
                                decode_screen_frame(&payload)
                            {
                                on_event(BrowserEvent::Frame {
                                    css_width,
                                    css_height,
                                    jpeg: jpeg.to_vec(),
                                });
                            }
                        }
                        Ok(Some((SERVER_STATE, payload))) => {
                            if let Ok(state) = serde_json::from_slice(&payload) {
                                on_event(BrowserEvent::State(state));
                            }
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => return on_event(BrowserEvent::Closed(None)),
                        Err(error) => {
                            return on_event(BrowserEvent::Closed(Some(error.to_string())));
                        }
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Arc::new(Self {
            writer: Mutex::new(stream),
        }))
    }

    /// 上一帧画上了，要下一帧。
    pub fn ack_frame(&self) -> io::Result<()> {
        write_frame(&mut *self.writer.lock().unwrap(), CLIENT_FRAME_ACK, &[])
    }

    pub fn input(&self, input: &BrowserInput) -> io::Result<()> {
        write_json_frame(&mut *self.writer.lock().unwrap(), CLIENT_INPUT, input)
    }

    pub fn command(&self, command: &BrowserCommand) -> io::Result<()> {
        write_json_frame(&mut *self.writer.lock().unwrap(), CLIENT_COMMAND, command)
    }
}

impl Drop for BrowserConnection {
    /// 关掉连接，收帧线程随之读到结束退出。
    fn drop(&mut self) {
        if let Ok(stream) = self.writer.lock() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// 逐字节读回复行：后面紧跟二进制帧，带缓冲读会把它们一起吞掉。
fn read_reply_line(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stream.read(&mut byte)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "连接关了"));
        }
        if byte[0] == b'\n' {
            return Ok(line);
        }
        line.push(byte[0]);
        if line.len() > 64 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "回复太长"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::browser::{BrowserTab, encode_screen_frame};
    use std::sync::mpsc;

    /// 假运行时：回 ok，推一份状态和一帧画面，再看客户端回了什么。
    #[test]
    fn attach_delivers_state_and_frames_and_sends_acks() {
        let (client_side, server_side) = UnixStream::pair().unwrap();
        let slot = Arc::new(Mutex::new(Some(client_side)));
        let client = RuntimeClient::new(move || {
            slot.lock()
                .unwrap()
                .take()
                .ok_or_else(|| io::Error::other("只连一次"))
        });
        let server = std::thread::spawn(move || {
            let mut server = server_side;
            let request = read_reply_line(&mut server).unwrap();
            let request: serde_json::Value = serde_json::from_slice(&request).unwrap();
            assert_eq!(request["op"], "browser_attach");
            write_json_line(
                &mut server,
                &BrowserAttachReply {
                    ok: true,
                    error: None,
                },
            )
            .unwrap();
            let state = BrowserState {
                tabs: vec![BrowserTab {
                    id: "p1".into(),
                    title: "首页".into(),
                    url: "https://example.com/".into(),
                }],
                active: Some("p1".into()),
                controlling: false,
            };
            write_json_frame(&mut server, SERVER_STATE, &state).unwrap();
            write_frame(
                &mut server,
                SERVER_FRAME,
                &encode_screen_frame(800, 600, b"jpeg"),
            )
            .unwrap();
            let (kind, _) = read_frame(&mut server).unwrap().unwrap();
            assert_eq!(kind, CLIENT_FRAME_ACK);
            let (kind, payload) = read_frame(&mut server).unwrap().unwrap();
            assert_eq!(kind, CLIENT_COMMAND);
            assert_eq!(
                serde_json::from_slice::<BrowserCommand>(&payload).unwrap(),
                BrowserCommand::Control { on: true }
            );
        });

        let (events, inbox) = mpsc::channel();
        let connection = BrowserConnection::attach(&client, move |event| {
            let _ = events.send(event);
        })
        .unwrap();
        let Ok(BrowserEvent::State(state)) = inbox.recv_timeout(Duration::from_secs(5)) else {
            panic!("先收到状态");
        };
        assert_eq!(state.active.as_deref(), Some("p1"));
        let Ok(BrowserEvent::Frame {
            css_width,
            css_height,
            jpeg,
        }) = inbox.recv_timeout(Duration::from_secs(5))
        else {
            panic!("再收到画面");
        };
        assert_eq!(
            (css_width, css_height, jpeg.as_slice()),
            (800, 600, &b"jpeg"[..])
        );
        connection.ack_frame().unwrap();
        connection
            .command(&BrowserCommand::Control { on: true })
            .unwrap();
        server.join().unwrap();
        assert!(matches!(
            inbox.recv_timeout(Duration::from_secs(5)),
            Ok(BrowserEvent::Closed(_))
        ));
    }

    #[test]
    fn refusal_reason_comes_back_to_the_caller() {
        let (client_side, server_side) = UnixStream::pair().unwrap();
        let slot = Arc::new(Mutex::new(Some(client_side)));
        let client = RuntimeClient::new(move || {
            slot.lock()
                .unwrap()
                .take()
                .ok_or_else(|| io::Error::other("只连一次"))
        });
        std::thread::spawn(move || {
            let mut server = server_side;
            let _ = read_reply_line(&mut server);
            let reply = BrowserAttachReply {
                ok: false,
                error: Some("浏览器里没有打开的页面".into()),
            };
            write_json_line(&mut server, &reply).unwrap();
        });
        let error = BrowserConnection::attach(&client, |_| {}).err().unwrap();
        assert_eq!(error, "浏览器里没有打开的页面");
    }
}
