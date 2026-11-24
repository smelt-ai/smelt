//! 终端相关的控制方法。
//!
//! 字节流与 resize 仍走 `watch` / `input`；图片无法直接塞进 PTY 字节流，因此先经
//! [`TerminalStageImageCall`] 把图片落盘到运行时临时目录，再把路径贴进 PTY。

use serde::{Deserialize, Serialize};

use super::control::{ControlCall, ControlMethod};
use super::files::Base64Bytes;

/// 单张终端图片的大小上限（与对话输入框一致：5 MiB）。
pub const MAX_TERMINAL_IMAGE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalStageImageParams {
    pub session_id: String,
    pub mime: String,
    pub data: Base64Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalStageImageResult {
    /// 运行时机器上的绝对路径。
    pub path: String,
}

/// 把一张客户端图片暂存到运行时机器的临时目录，供终端会话的 CLI 读取。
pub enum TerminalStageImageCall {}

impl ControlCall for TerminalStageImageCall {
    type Params = TerminalStageImageParams;
    type Result = TerminalStageImageResult;

    const METHOD: ControlMethod = ControlMethod::TerminalStageImage;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_image_call_binds_method_and_round_trips_wire_json() {
        assert_eq!(
            TerminalStageImageCall::METHOD,
            ControlMethod::TerminalStageImage
        );
        let params = TerminalStageImageParams {
            session_id: "term-1".into(),
            mime: "image/png".into(),
            data: Base64Bytes(vec![0x89, b'P', b'N', b'G']),
        };
        let wire = serde_json::to_value(&params).unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "session_id": "term-1",
                "mime": "image/png",
                "data": "iVBORw==",
            })
        );
        assert_eq!(
            serde_json::from_value::<TerminalStageImageParams>(wire).unwrap(),
            params
        );

        let result = TerminalStageImageResult {
            path: "/tmp/smelt-terminal-images-1000/img-1.png".into(),
        };
        let result_wire = serde_json::to_value(&result).unwrap();
        assert_eq!(
            serde_json::from_value::<TerminalStageImageResult>(result_wire).unwrap(),
            result
        );
    }
}
