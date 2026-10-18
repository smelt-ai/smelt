//! 浏览器画面流（设计文档 D9）：运行时用 CDP 把常驻 Chrome 的页面画面推给查看的人，
//! 人接管时把鼠标键盘转回去。
//!
//! 连接首行 `{"op":"browser_attach"}`，运行时回一行 [`BrowserAttachReply`]，之后双向都是
//! `[类型 u8][长度 u32 BE][内容]` 的帧。画面一次只有一帧在途：客户端画上之后回
//! [`CLIENT_FRAME_ACK`]，运行时才回 Chrome 的 `Page.screencastFrameAck`，Chrome 才出下一帧。
//! 慢网络自动降帧，中间不积压。
//!
//! 只推页面本身：标签栏、地址栏由客户端自己画，Chrome 的原生界面（权限气泡、系统文件框）
//! 不在画面里。

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// 运行时 → 客户端：一帧画面。内容是 `[u32 视口宽][u32 视口高][JPEG]`，宽高是页面的
/// CSS 像素，客户端按它把点击位置换算回页面坐标（JPEG 本身可能被缩小过）。
pub const SERVER_FRAME: u8 = 1;
/// 运行时 → 客户端：[`BrowserState`] JSON。
pub const SERVER_STATE: u8 = 2;
/// 客户端 → 运行时：上一帧已经画上，可以要下一帧了。内容为空。
pub const CLIENT_FRAME_ACK: u8 = 1;
/// 客户端 → 运行时：[`BrowserInput`] JSON。只在接管时生效。
pub const CLIENT_INPUT: u8 = 2;
/// 客户端 → 运行时：[`BrowserCommand`] JSON。除接管开关外都只在接管时生效。
pub const CLIENT_COMMAND: u8 = 3;

/// 单帧内容上限。一帧 JPEG 一般几十 KB，状态 JSON 更小；超出按坏连接处理。
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserAttachReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserTab {
    pub id: String,
    pub title: String,
    pub url: String,
}

/// 浏览器当前的样子。标签页、正在看的那一页或这条连接是否在接管变了就推一份。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserState {
    pub tabs: Vec<BrowserTab>,
    /// 正在推画面的标签页 id。
    pub active: Option<String>,
    /// 这条连接是不是在接管：是的话输入和导航会转给浏览器。
    pub controlling: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseEventKind {
    Pressed,
    Released,
    Moved,
    Wheel,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    #[default]
    None,
    Left,
    Middle,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyEventKind {
    Down,
    Up,
}

/// 修饰键，按 CDP 的位：Alt=1、Ctrl=2、Meta=4、Shift=8。
pub mod modifiers {
    pub const ALT: u32 = 1;
    pub const CTRL: u32 = 2;
    pub const META: u32 = 4;
    pub const SHIFT: u32 = 8;
}

/// 人在画面上的操作。坐标是页面 CSS 像素。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserInput {
    Mouse {
        event: MouseEventKind,
        x: f64,
        y: f64,
        #[serde(default)]
        button: MouseButton,
        #[serde(default)]
        click_count: u32,
        #[serde(default)]
        delta_x: f64,
        #[serde(default)]
        delta_y: f64,
        #[serde(default)]
        modifiers: u32,
    },
    /// 不产生文字的按键：回车、退格、方向键、快捷键。`key`/`code` 用 DOM 的名字，
    /// `key_code` 是 Windows 虚拟键码（页面脚本常看 `keyCode`）。
    Key {
        event: KeyEventKind,
        key: String,
        code: String,
        key_code: u32,
        /// 按下时要插入的文字，比如回车是 "\r"；没有就是纯功能键。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default)]
        modifiers: u32,
    },
    /// 打出来的文字（含输入法上屏的中文）。
    Text { text: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserCommand {
    /// 接管或交还。交还后这条连接只看不动。
    Control {
        on: bool,
    },
    /// 只认 http、https 和 about:blank。
    Navigate {
        url: String,
    },
    Back,
    Forward,
    Reload,
    ActivateTab {
        id: String,
    },
    NewTab,
    CloseTab {
        id: String,
    },
}

/// 地址栏能打开的地址：只放 http(s) 和空白页，挡掉 `file:`、`chrome:`、`javascript:`
/// 这类能读运行时本机文件或改浏览器设置的。
pub fn navigable_url(url: &str) -> bool {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower == "about:blank"
}

pub fn write_frame(writer: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    let len = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "帧太大"))?;
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(kind);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(payload);
    writer.write_all(&buf)
}

pub fn write_json_frame(
    writer: &mut impl Write,
    kind: u8,
    value: &impl Serialize,
) -> io::Result<()> {
    write_frame(writer, kind, &serde_json::to_vec(value)?)
}

/// 读一帧。对端正常关掉时返回 `Ok(None)`。
pub fn read_frame(reader: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut header = [0u8; 5];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "帧太大"));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;
    Ok(Some((header[0], payload)))
}

/// 画面帧：`[u32 视口宽][u32 视口高][JPEG]`。
pub fn encode_screen_frame(css_width: u32, css_height: u32, jpeg: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8 + jpeg.len());
    payload.extend_from_slice(&css_width.to_be_bytes());
    payload.extend_from_slice(&css_height.to_be_bytes());
    payload.extend_from_slice(jpeg);
    payload
}

/// 拆开画面帧，返回 (视口宽, 视口高, JPEG)。
pub fn decode_screen_frame(payload: &[u8]) -> Option<(u32, u32, &[u8])> {
    if payload.len() < 8 {
        return None;
    }
    let width = u32::from_be_bytes(payload[0..4].try_into().ok()?);
    let height = u32::from_be_bytes(payload[4..8].try_into().ok()?);
    Some((width, height, &payload[8..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_eof_is_a_clean_close() {
        let mut wire = Vec::new();
        write_frame(
            &mut wire,
            SERVER_FRAME,
            &encode_screen_frame(1280, 720, b"jpeg"),
        )
        .unwrap();
        write_json_frame(
            &mut wire,
            CLIENT_COMMAND,
            &BrowserCommand::Control { on: true },
        )
        .unwrap();
        let mut reader = wire.as_slice();
        let (kind, payload) = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(kind, SERVER_FRAME);
        assert_eq!(
            decode_screen_frame(&payload),
            Some((1280, 720, &b"jpeg"[..]))
        );
        let (kind, payload) = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(kind, CLIENT_COMMAND);
        assert_eq!(
            serde_json::from_slice::<BrowserCommand>(&payload).unwrap(),
            BrowserCommand::Control { on: true }
        );
        assert!(read_frame(&mut reader).unwrap().is_none());
    }

    #[test]
    fn oversized_frames_are_refused() {
        let mut wire = vec![SERVER_FRAME];
        wire.extend_from_slice(&(MAX_FRAME_BYTES as u32 + 1).to_be_bytes());
        assert!(read_frame(&mut wire.as_slice()).is_err());
    }

    #[test]
    fn only_web_pages_are_navigable() {
        assert!(navigable_url("https://www.google.com/travel/flights"));
        assert!(navigable_url(" HTTP://example.com "));
        assert!(navigable_url("about:blank"));
        for url in [
            "file:///etc/passwd",
            "chrome://settings",
            "javascript:alert(1)",
            "about:config",
        ] {
            assert!(!navigable_url(url), "{url}");
        }
    }

    #[test]
    fn input_wire_shape_is_tagged() {
        let text = serde_json::to_value(BrowserInput::Text {
            text: "你好".into(),
        })
        .unwrap();
        assert_eq!(text, serde_json::json!({ "kind": "text", "text": "你好" }));
        let mouse: BrowserInput = serde_json::from_value(serde_json::json!({
            "kind": "mouse", "event": "pressed", "x": 10.0, "y": 20.0, "button": "left", "click_count": 1
        }))
        .unwrap();
        assert!(matches!(
            mouse,
            BrowserInput::Mouse {
                event: MouseEventKind::Pressed,
                button: MouseButton::Left,
                ..
            }
        ));
    }
}
