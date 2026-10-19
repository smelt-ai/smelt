//! 暴露给 Dart 的结构化错误。界面按 `kind` 决定提示：未配对/被撤销要引导重新配对，
//! 离线只提示网络，`message` 只给人看。

/// 错误类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmeltErrorKind {
    /// 运行时不认这台设备：从没配对过，或已被撤销。要重新配对。
    Unpaired,
    /// 连不上或超时：运行时离线、网络不通。状态机会在后台重试。
    Offline,
    /// 配对串格式不对、endpoint / relay 不合法，或配对码无效、已过期。
    InvalidPairing,
    /// 两端版本对不上（协议版本不兼容、运行时太旧缺方法或字段）。要升级其中一端。
    Incompatible,
    /// 这次连接请求被更新的 connect / pair / disconnect 取代，结果已作废。
    Cancelled,
    /// 其它失败。
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmeltError {
    pub kind: SmeltErrorKind,
    pub message: String,
}

pub(crate) fn error(kind: SmeltErrorKind, message: impl Into<String>) -> SmeltError {
    SmeltError {
        kind,
        message: message.into(),
    }
}
