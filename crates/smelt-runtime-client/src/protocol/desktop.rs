//! 远程桌面（设计文档 D9）：运行时那台机器上的整块屏幕，人能看着 agent 干活、必要时接管。
//!
//! 画面和鼠标键盘走 VNC：连接首行 `{"op":"desktop_attach"}`，运行时回一行
//! [`DesktopAttachReply`]，之后这条连接就是到运行时本机 VNC 服务的原始 RFB 字节流，两个
//! 方向原样转发。客户端用现成的 VNC 客户端（noVNC）说 RFB，运行时不解析。
//!
//! VNC 只能一个键一个键地发，本机输入法拼出来的中文过不去，所以文字另走
//! [`DesktopTypeTextCall`]：运行时把整段文字注入到当前聚焦的窗口。

use serde::{Deserialize, Serialize};

use super::control::{ControlCall, ControlMethod, EmptyParams};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopAttachReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 一次最多注入的字符数：注入是逐字模拟按键，太长会让人等很久，也防误贴一大段。
pub const MAX_TYPE_TEXT_CHARS: usize = 4000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopTypeTextParams {
    pub text: String,
}

/// 把文字打进远程桌面当前聚焦的输入框，换行按回车。
pub enum DesktopTypeTextCall {}

impl ControlCall for DesktopTypeTextCall {
    type Params = DesktopTypeTextParams;
    type Result = EmptyParams;

    const METHOD: ControlMethod = ControlMethod::DesktopTypeText;
}
