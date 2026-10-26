//! 远程桌面的运行时一侧（设计文档 D9）：把配对设备的连接转发到本机的 VNC 服务，并把整段
//! 文字注入到当前聚焦的窗口。协议见 [`smelt_runtime_client::protocol::desktop`]。
//!
//! 画面、鼠标、键盘都是 RFB，运行时只转发不解析：抓屏、编码、输入注入交给 VNC 服务
//! （TigerVNC 的 Xvnc，它本身就是这块虚拟屏幕的 X 服务器）。VNC 只听本机，外面只能经过
//! 运行时、也就是只有配对过的设备能连。

use super::ClientSocket;
use std::io::{self, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::process::Command;
use std::time::Duration;

use smelt_runtime_client::protocol::desktop::{DesktopAttachReply, MAX_TYPE_TEXT_CHARS};
use smelt_runtime_client::protocol::write_json_line;

/// VNC 服务的地址。`smelt-display.service` 起的 Xvnc 只听本机 5999；别的地址用
/// `SMELT_DESKTOP_VNC` 指定。
const DEFAULT_VNC_ENDPOINT: &str = "127.0.0.1:5999";
/// 那块虚拟屏幕的 X 显示号，注入文字用。别的用 `SMELT_DESKTOP_DISPLAY` 指定。
const DEFAULT_DISPLAY: &str = ":99";

pub(crate) fn vnc_endpoint() -> String {
    std::env::var("SMELT_DESKTOP_VNC").unwrap_or_else(|_| DEFAULT_VNC_ENDPOINT.to_string())
}

fn display() -> String {
    std::env::var("SMELT_DESKTOP_DISPLAY").unwrap_or_else(|_| DEFAULT_DISPLAY.to_string())
}

/// 连上本机 VNC 后回 ok，之后两个方向原样转发，任一边断了就都关掉。
///
/// `reader` 里可能已经预读了首行之后的字节（客户端紧接着发的 RFB 握手），所以往 VNC 那边
/// 转发必须从它读，不能另开一个裸流。
pub(crate) fn handle_desktop_attach<C: ClientSocket>(
    mut conn: C,
    mut reader: BufReader<C>,
    endpoint: &str,
) {
    let vnc = match connect(endpoint) {
        Ok(vnc) => vnc,
        Err(error) => {
            let _ = write_json_line(
                &mut conn,
                &DesktopAttachReply {
                    ok: false,
                    error: Some(format!("这台机器上的远程桌面连不上（{endpoint}）：{error}")),
                },
            );
            return;
        }
    };
    if write_json_line(
        &mut conn,
        &DesktopAttachReply {
            ok: true,
            error: None,
        },
    )
    .is_err()
    {
        return;
    }
    let Ok(mut to_vnc) = vnc.try_clone() else {
        return;
    };
    let closer = conn.try_clone().ok();
    let upstream = std::thread::Builder::new()
        .name("smelt-desktop-up".into())
        .spawn(move || {
            let _ = io::copy(&mut reader, &mut to_vnc);
            let _ = to_vnc.shutdown(Shutdown::Both);
            if let Some(closer) = closer {
                let _ = closer.shutdown(Shutdown::Both);
            }
        });
    let mut from_vnc = vnc;
    let _ = copy_flushing(&mut from_vnc, &mut conn);
    let _ = conn.shutdown(Shutdown::Both);
    let _ = from_vnc.shutdown(Shutdown::Both);
    if let Ok(upstream) = upstream {
        let _ = upstream.join();
    }
}

/// 读到多少写多少：画面更新是一阵一阵的，不能攒在缓冲里等下一阵。
fn copy_flushing(from: &mut impl Read, to: &mut impl Write) -> io::Result<()> {
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        to.write_all(&buf[..n])?;
    }
}

fn connect(endpoint: &str) -> io::Result<TcpStream> {
    let addr = endpoint
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "地址解析不出来"))?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// 把文字打进当前聚焦的窗口。xdotool 对键盘上没有的字（中文、表情）会临时改一个空闲
/// 键码的映射再按，所以不需要远端装输入法。
pub(crate) fn type_text(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    if text.chars().count() > MAX_TYPE_TEXT_CHARS {
        return Err(format!("一次最多输入 {MAX_TYPE_TEXT_CHARS} 个字"));
    }
    let output = type_text_command(&display(), text)
        .output()
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => "这台机器上没有 xdotool，装上才能从这里输入文字".to_string(),
            _ => format!("输入文字失败：{error}"),
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "输入文字失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn type_text_command(display: &str, text: &str) -> Command {
    let mut command = Command::new("xdotool");
    // --clearmodifiers：人可能正按着 Shift/Cmd，先松开再打，免得打成快捷键。
    // `--` 之后都是文字，以 `-` 开头的内容不会被当成参数。
    command
        .env("DISPLAY", display)
        .args(["type", "--clearmodifiers", "--delay", "8", "--", text]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use smelt_runtime_client::UnixStream;
    use std::net::TcpListener;

    /// 假 VNC：把收到的字节原样回过去。
    fn echo_vnc() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                std::thread::spawn(move || {
                    let mut reader = stream.try_clone().unwrap();
                    let _ = io::copy(&mut reader, &mut stream);
                });
            }
        });
        endpoint
    }

    fn read_line<C: ClientSocket>(stream: &mut C) -> Vec<u8> {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while stream.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
            line.push(byte[0]);
        }
        line
    }

    /// 首行之后紧跟着的字节（RFB 握手）不能丢：它们可能已经被首行的缓冲读走了。
    #[test]
    fn bytes_right_after_the_request_line_reach_the_vnc_server() {
        let endpoint = echo_vnc();
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .write_all(b"{\"op\":\"desktop_attach\"}\nRFB 003.008\n")
            .unwrap();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(server.try_clone().unwrap());
            let mut line = String::new();
            io::BufRead::read_line(&mut reader, &mut line).unwrap();
            handle_desktop_attach(server, reader, &endpoint);
        });
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let reply: DesktopAttachReply = serde_json::from_slice(&read_line(&mut client)).unwrap();
        assert!(reply.ok, "{reply:?}");
        let mut echoed = [0u8; 12];
        client.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"RFB 003.008\n");
        client.write_all(b"more").unwrap();
        let mut more = [0u8; 4];
        client.read_exact(&mut more).unwrap();
        assert_eq!(&more, b"more");
    }

    #[test]
    fn missing_vnc_is_reported() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap().to_string();
        drop(listener);
        let (mut client, server) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let reader = BufReader::new(server.try_clone().unwrap());
            handle_desktop_attach(server, reader, &endpoint);
        });
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let reply: DesktopAttachReply = serde_json::from_slice(&read_line(&mut client)).unwrap();
        assert!(!reply.ok);
        assert!(reply.error.unwrap().contains("连不上"));
    }

    #[test]
    fn text_goes_after_the_double_dash() {
        let command = type_text_command(":99", "-rf 北京");
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args.last().map(String::as_str), Some("-rf 北京"));
        assert_eq!(args[args.len() - 2], "--");
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "DISPLAY" && value.is_some_and(|v| v == ":99"))
        );
    }

    #[test]
    fn overly_long_text_is_refused_before_running_anything() {
        let text = "字".repeat(MAX_TYPE_TEXT_CHARS + 1);
        assert!(type_text(&text).unwrap_err().contains("最多"));
    }
}
