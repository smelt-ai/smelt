//! 手机推送的线上格式：手机 → 推送中继（换句柄），运行时 → 推送中继 → APNs → 手机。
//!
//! APNs 只认持有开发者 `.p8` 私钥的发送方，这把私钥不能随开源的 smeltd 分发，所以由推送
//! 中继（`smelt-push-relay`）代发。中继不存设备表：手机把 APNs 令牌交给中继，中继用自己的
//! 密钥把令牌加密成一个**句柄**还给手机；手机把句柄登记到各台运行时，运行时发推送时只带
//! 句柄，中继解开句柄才知道令牌。运行时库里因此没有裸令牌，中继也不能被拿去给任意令牌发推送。
//!
//! 推送只叫醒用户，不带会话内容：载荷里只有提醒类型（手机按它显示本地化的固定文案）和
//! 不透明的 `runtime_id`、`session_id`（点开后跳到对应会话）。会话内容仍然只走 iroh。
//!
//! 运行时（smeltd）、中继、手机是三个程序，格式在这里只定义一份。Dart / Swift 一侧照这里的
//! 字段名手写。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 手机换句柄：`POST`，[`RegisterRequest`] → [`RegisterResponse`]。
pub const RELAY_REGISTER_PATH: &str = "/v1/register";

/// 运行时发推送：`POST`，[`SendRequest`]，见 [`SendRequest`] 上的回应约定。
pub const RELAY_SEND_PATH: &str = "/v1/send";

/// 设备令牌属于哪套 APNs：开发签名的包走 sandbox，TestFlight / App Store 走 production。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApnsEnvironment {
    Sandbox,
    Production,
}

/// 手机交给中继的 APNs 登记。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterRequest {
    /// APNs 设备令牌，十六进制。
    pub device_token: String,
    pub environment: ApnsEnvironment,
    /// App 的 Bundle ID，即 APNs 的 topic。中继只接受配置里允许的。
    pub topic: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterResponse {
    /// 不透明。只有发出它的中继解得开；中继换了密钥就全部作废（发送时回 `410`）。
    pub handle: String,
}

/// 会推到手机的提醒类型。和 `smelt_model::attention::AttentionKind` 里需要用户处理的
/// 三种一一对应；「完成」第一阶段不推。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushKind {
    Approval,
    Input,
    Failure,
}

impl PushKind {
    /// App 里 `Localizable.strings` 的键：系统按手机语言显示对应文案，中继和运行时不出文字。
    pub fn loc_key(self) -> &'static str {
        match self {
            Self::Approval => "SMELT_PUSH_APPROVAL",
            Self::Input => "SMELT_PUSH_INPUT",
            Self::Failure => "SMELT_PUSH_FAILURE",
        }
    }
}

/// 运行时发给中继的一条推送。
///
/// 中继的回应：
/// - `200`：APNs 已收下。
/// - `410`：句柄或令牌已失效（App 卸载、中继换了密钥）。运行时删掉这条登记，不再重试。
/// - `400`：请求本身不对。不重试。
/// - `429`、`5xx`：中继限流，或 APNs 暂时不行。退避重试。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRequest {
    pub handle: String,
    pub kind: PushKind,
    pub runtime_id: String,
    pub session_id: String,
    /// 同一会话的新推送替换通知中心里的旧推送，见 [`collapse_id`]。
    pub collapse_id: String,
    /// APNs 的 `apns-id`（UUID）。同一事件重试时不变，APNs 按它去重。
    pub apns_id: String,
}

/// 一段会话在一台手机上的 collapse id。混进句柄，中继和 APNs 从它推不出会话 id，也关联不了
/// 不同手机上的同一段会话。APNs 限 64 字节，取 32 位十六进制。
pub fn collapse_id(handle: &str, session_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(handle.as_bytes());
    hasher.update([0]);
    hasher.update(session_id.as_bytes());
    hasher.finalize()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 中继交给 APNs 的载荷。
///
/// `mutable-content` 不设：第一阶段没有 Notification Service Extension。`thread-id` 按机器分组，
/// 通知中心里同一台机器的提醒叠在一起。`smelt` 里是点开后跳转用的不透明 id。
pub fn apns_payload(request: &SendRequest) -> serde_json::Value {
    serde_json::json!({
        "aps": {
            "alert": { "loc-key": request.kind.loc_key() },
            "sound": "default",
            "thread-id": request.runtime_id,
        },
        "smelt": {
            "kind": request.kind,
            "runtime_id": request.runtime_id,
            "session_id": request.session_id,
        },
    })
}

/// APNs 设备令牌：非空的十六进制串。现在是 32 字节，Apple 说过长度可能变，不卡死。
pub fn valid_device_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 200
        && token.len().is_multiple_of(2)
        && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send_request() -> SendRequest {
        SendRequest {
            handle: "h".into(),
            kind: PushKind::Approval,
            runtime_id: "runtime-1".into(),
            session_id: "session-1".into(),
            collapse_id: collapse_id("h", "session-1"),
            apns_id: "00000000-0000-4000-8000-000000000000".into(),
        }
    }

    #[test]
    fn collapse_ids_are_stable_per_handle_and_session() {
        let id = collapse_id("handle-a", "session-1");
        assert_eq!(id.len(), 32);
        assert_eq!(id, collapse_id("handle-a", "session-1"));
        assert_ne!(id, collapse_id("handle-a", "session-2"));
        assert_ne!(id, collapse_id("handle-b", "session-1"));
        assert!(!id.contains("session"));
    }

    /// 载荷只有类型和两个 id，文案靠 loc-key 在手机上本地化。
    #[test]
    fn apns_payload_carries_no_session_content() {
        assert_eq!(
            apns_payload(&send_request()),
            serde_json::json!({
                "aps": {
                    "alert": { "loc-key": "SMELT_PUSH_APPROVAL" },
                    "sound": "default",
                    "thread-id": "runtime-1",
                },
                "smelt": {
                    "kind": "approval",
                    "runtime_id": "runtime-1",
                    "session_id": "session-1",
                },
            })
        );
    }

    #[test]
    fn wire_shapes_use_snake_case() {
        let register = RegisterRequest {
            device_token: "ab".into(),
            environment: ApnsEnvironment::Sandbox,
            topic: "ai.smelt.mobile".into(),
        };
        assert_eq!(
            serde_json::to_value(&register).unwrap(),
            serde_json::json!({
                "device_token": "ab",
                "environment": "sandbox",
                "topic": "ai.smelt.mobile",
            })
        );
        let send = serde_json::to_value(send_request()).unwrap();
        assert_eq!(send["kind"], "approval");
        assert_eq!(send["apns_id"], "00000000-0000-4000-8000-000000000000");
    }

    #[test]
    fn device_tokens_are_validated() {
        assert!(valid_device_token("a1B2"));
        assert!(!valid_device_token(""));
        assert!(!valid_device_token("abc"));
        assert!(!valid_device_token("zz"));
        assert!(!valid_device_token(&"ab".repeat(101)));
    }
}
