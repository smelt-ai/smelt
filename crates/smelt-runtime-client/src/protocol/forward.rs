//! `op=forward`：经本机运行时转发到它的一个对端节点（见 `docs/cross-machine.md`）。
//!
//! 首帧 `{"op":"forward","runtime_id":"…"}`，运行时拨通后回一行 [`ForwardReply`]，
//! 成功之后这条连接只搬字节，里面照常跑任何 op。客户端读到回执才发后续帧，所以回执
//! 之后没有被预读的字节。

use std::io::{self, Read};

use serde::{Deserialize, Serialize};

use super::daemon::DaemonOperation;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardRequest {
    pub op: DaemonOperation,
    pub runtime_id: String,
}

impl ForwardRequest {
    pub fn new(runtime_id: impl Into<String>) -> Self {
        Self {
            op: DaemonOperation::Forward,
            runtime_id: runtime_id.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ForwardErrorCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ForwardReply {
    pub fn ok() -> Self {
        Self {
            ok: true,
            code: None,
            error: None,
        }
    }

    pub fn failed(code: ForwardErrorCode, error: impl Into<String>) -> Self {
        Self {
            ok: false,
            code: Some(code),
            error: Some(error.into()),
        }
    }

    /// 失败的回执换成 io 错误，原因原样带上。
    pub fn into_result(self) -> io::Result<()> {
        if self.ok {
            return Ok(());
        }
        let kind = match self.code {
            Some(ForwardErrorCode::NotFound) => io::ErrorKind::NotFound,
            Some(ForwardErrorCode::PermissionDenied) => io::ErrorKind::PermissionDenied,
            Some(ForwardErrorCode::Unreachable) => io::ErrorKind::ConnectionRefused,
            Some(ForwardErrorCode::Unknown) | None => io::ErrorKind::Other,
        };
        Err(io::Error::new(
            kind,
            self.error
                .unwrap_or_else(|| "the runtime refused to forward".into()),
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardErrorCode {
    /// 不是这个运行时的对端。
    NotFound,
    /// 调用方不许经这个运行时去那个对端：只收本机；智能体会话要对端打开了智能体权限。
    PermissionDenied,
    /// 连不上对端。
    Unreachable,
    #[serde(other)]
    Unknown,
}

/// 回执最长多少字节。远超任何正常回执，防对端发来没有换行的垃圾。
const MAX_REPLY: usize = 64 * 1024;

/// 逐字节读一行回执。不用 `BufReader`：它会把回执之后转发来的字节一起读走。
pub fn read_reply(stream: &mut impl Read) -> io::Result<ForwardReply> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the runtime closed the forward request without a reply",
                ));
            }
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => {
                line.push(byte[0]);
                if line.len() > MAX_REPLY {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the forward reply is too long",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    serde_json::from_slice(&line).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_is_a_forward_frame() {
        assert_eq!(
            serde_json::to_value(ForwardRequest::new("rt")).unwrap(),
            serde_json::json!({"op": "forward", "runtime_id": "rt"})
        );
    }

    /// 回执之后的字节留在流里，不被读走。
    #[test]
    fn reading_the_reply_leaves_the_forwarded_bytes() {
        let mut input: &[u8] = b"{\"ok\":true}\nrest";
        assert!(read_reply(&mut input).unwrap().ok);
        assert_eq!(input, b"rest");
    }

    #[test]
    fn failed_reply_keeps_its_reason() {
        let mut input: &[u8] =
            b"{\"ok\":false,\"code\":\"permission_denied\",\"error\":\"no agents\"}\n";
        let error = read_reply(&mut input).unwrap().into_result().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "no agents");
    }
}
