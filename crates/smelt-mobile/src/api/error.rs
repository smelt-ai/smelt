//! 暴露给 Dart 的结构化错误。界面按 `kind` 决定提示：未配对/被撤销要引导重新配对，
//! 离线只提示网络，`message` 只给人看。

use smelt_runtime_client::compat::Outdated;
use smelt_runtime_client::protocol::control::{
    CONTROL_API_VERSION, ControlError, ControlErrorCode,
};

/// 错误类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmeltErrorKind {
    /// 运行时不认这台设备：从没配对过，或已被撤销。要重新配对。
    Unpaired,
    /// 连不上或超时：运行时离线、网络不通。状态机会在后台重试。
    Offline,
    /// 配对串格式不对、endpoint / relay 不合法，或配对码无效、已过期。
    InvalidPairing,
    /// 运行时比这个 App 旧：协议版本不够，或缺方法、缺字段。要升级那台机器上的 Smelt。
    RuntimeOutdated,
    /// 这个 App 比运行时旧，认不了它的协议。要升级 App。
    AppOutdated,
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

/// 运行时拒绝这次控制调用是不是因为两边版本对不上，是的话该升级哪一边。缺方法是运行时
/// 旧；版本不支持时看它报的区间（`ControlError::unsupported_version` 总会带上）：最低要求
/// 比这个 App 高就是 App 旧。没带区间判断不了方向，按运行时旧处理。
pub(crate) fn outdated_kind(control_error: &ControlError) -> Option<SmeltErrorKind> {
    match control_error.code {
        ControlErrorCode::MethodNotFound => Some(SmeltErrorKind::RuntimeOutdated),
        ControlErrorCode::UnsupportedVersion => Some(match &control_error.supported_versions {
            Some(range) if range.min_supported_version > CONTROL_API_VERSION => {
                SmeltErrorKind::AppOutdated
            }
            _ => SmeltErrorKind::RuntimeOutdated,
        }),
        _ => None,
    }
}

/// 版本协商（[`smelt_runtime_client::compat`]）说的该升级哪一边。
pub(crate) fn outdated_side(outdated: Outdated) -> SmeltErrorKind {
    match outdated {
        Outdated::Runtime => SmeltErrorKind::RuntimeOutdated,
        Outdated::Client => SmeltErrorKind::AppOutdated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unsupported(min: u16, max: u16) -> ControlError {
        ControlError::unsupported_version(CONTROL_API_VERSION, min, max)
    }

    #[test]
    fn outdated_kind_tells_which_side_to_upgrade() {
        assert_eq!(
            outdated_kind(&ControlError::new(ControlErrorCode::MethodNotFound, "x")),
            Some(SmeltErrorKind::RuntimeOutdated)
        );
        assert_eq!(
            outdated_kind(&unsupported(
                CONTROL_API_VERSION + 1,
                CONTROL_API_VERSION + 1
            )),
            Some(SmeltErrorKind::AppOutdated)
        );
        assert_eq!(
            outdated_kind(&unsupported(0, CONTROL_API_VERSION - 1)),
            Some(SmeltErrorKind::RuntimeOutdated)
        );
        let mut no_range = unsupported(0, 0);
        no_range.supported_versions = None;
        assert_eq!(
            outdated_kind(&no_range),
            Some(SmeltErrorKind::RuntimeOutdated)
        );
        assert_eq!(
            outdated_kind(&ControlError::new(ControlErrorCode::PermissionDenied, "x")),
            None
        );
    }
}
