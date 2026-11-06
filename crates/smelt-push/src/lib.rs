//! 手机推送的线上格式：手机 → 推送中继（换句柄），运行时 → 推送中继 → APNs → 手机。
//!
//! APNs 只认持有开发者 `.p8` 私钥的发送方，这把私钥不能随开源的 smeltd 分发，所以由推送
//! 中继（`smelt-push-relay`）代发。中继不存设备表：手机把 APNs 令牌交给中继，中继用自己的
//! 密钥把令牌加密成一个**句柄**还给手机；手机把句柄登记到各台运行时，运行时发推送时只带
//! 句柄，中继解开句柄才知道令牌。运行时库里因此没有裸令牌，中继也不能被拿去给任意令牌发推送。
//!
//! 推送带一小段明文：会话名、项目名和一行摘要（等批准的命令、智能体最后一条回复的开头等），
//! 经过中继和 APNs。提醒类型的那几个字由手机按 `loc-key` 本地化，运行时和中继不出文案。
//! 智能体主动发的消息（[`PushKind::Message`]）例外：正文就是智能体写的那段话，原样显示。
//! 完整的会话内容仍然只走 iroh。
//!
//! 运行时（smeltd）、中继、手机是三个程序，格式在这里只定义一份。Dart / Swift 一侧照这里的
//! 字段名手写。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 手机换句柄：`POST`，[`RegisterRequest`] → [`RegisterResponse`]。
pub const RELAY_REGISTER_PATH: &str = "/v1/register";

/// 运行时发推送：`POST`，[`SendRequest`]，见 [`SendRequest`] 上的回应约定。
pub const RELAY_SEND_PATH: &str = "/v1/send";

/// 推送标题（会话名）最多这么多字。
pub const TITLE_MAX_CHARS: usize = 80;
/// 推送副标题（项目名）最多这么多字。
pub const SUBTITLE_MAX_CHARS: usize = 60;
/// 推送正文里的摘要最多这么多字。三段加起来离 APNs 4KB 的载荷上限很远。
pub const DETAIL_MAX_CHARS: usize = 200;
/// 智能体主动发的消息正文最多这么多字。全是汉字也只有 1.5KB，加上标题仍在 4KB 内。
pub const MESSAGE_MAX_CHARS: usize = 500;

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

/// 会推到手机的提醒类型。前四种和 `smelt_model::attention::AttentionKind` 里除响铃、普通提示外
/// 的四种一一对应；[`Self::Message`] 是智能体自己决定发的消息，不来自会话状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushKind {
    Approval,
    Input,
    Failure,
    Success,
    Message,
}

impl PushKind {
    /// 没有摘要时正文的 `Localizable.strings` 键：一句完整的话。
    pub fn loc_key(self) -> &'static str {
        match self {
            Self::Approval => "SMELT_PUSH_APPROVAL",
            Self::Input => "SMELT_PUSH_INPUT",
            Self::Failure => "SMELT_PUSH_FAILURE",
            Self::Success => "SMELT_PUSH_SUCCESS",
            Self::Message => "SMELT_PUSH_MESSAGE",
        }
    }

    /// 有摘要时正文的键：带一个 `%@`，填摘要（「等你批准：%@」）。
    pub fn detail_loc_key(self) -> &'static str {
        match self {
            Self::Approval => "SMELT_PUSH_APPROVAL_DETAIL",
            Self::Input => "SMELT_PUSH_INPUT_DETAIL",
            Self::Failure => "SMELT_PUSH_FAILURE_DETAIL",
            Self::Success => "SMELT_PUSH_SUCCESS_DETAIL",
            // 消息正文走 `body` 原样显示，不经本地化模板。
            Self::Message => "SMELT_PUSH_MESSAGE",
        }
    }

    /// `detail` 最多这么多字。
    pub fn detail_max_chars(self) -> usize {
        match self {
            Self::Message => MESSAGE_MAX_CHARS,
            _ => DETAIL_MAX_CHARS,
        }
    }

    /// iOS 15 起的打扰级别。等你批准、等你回复不处理智能体就停着，用 `time-sensitive`，
    /// 专注模式下也提醒（App 要有 Time Sensitive Notifications 能力，没有时系统按 `active`
    /// 处理）。结果类的用 `active`。
    pub fn interruption_level(self) -> &'static str {
        match self {
            Self::Approval | Self::Input => "time-sensitive",
            Self::Failure | Self::Success | Self::Message => "active",
        }
    }

    /// 通知摘要里的排序权重（0 到 1）：要你处理的在前，失败其次，完成最后。
    pub fn relevance_score(self) -> f64 {
        match self {
            Self::Approval | Self::Input => 1.0,
            Self::Failure | Self::Message => 0.8,
            Self::Success => 0.5,
        }
    }

    /// 要你处理、处理完就过时的提醒。手机在事情处理掉后从通知中心撤掉它们；结果类的
    /// 留作记录，打开那段会话时才清。
    pub fn is_request(self) -> bool {
        matches!(self, Self::Approval | Self::Input)
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
    /// 这条提醒在这台手机上的 collapse id，见 [`collapse_id`]。
    pub collapse_id: String,
    /// APNs 的 `apns-id`（UUID）。同一条提醒重试时不变。
    pub apns_id: String,
    /// 会话名。没有就不带，系统只显示 App 名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 项目名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// 一行摘要：等批准的命令、等回复的问题、失败原因、最后一条回复的开头。
    /// [`PushKind::Message`] 时是智能体写的消息正文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl SendRequest {
    /// 文字段都在长度上限内。运行时发之前用 [`excerpt`] 截好，中继据此拒收超长的。
    pub fn text_within_limits(&self) -> bool {
        let fits =
            |text: Option<&str>, max: usize| text.is_none_or(|text| text.chars().count() <= max);
        fits(self.title.as_deref(), TITLE_MAX_CHARS)
            && fits(self.subtitle.as_deref(), SUBTITLE_MAX_CHARS)
            && fits(self.detail.as_deref(), self.kind.detail_max_chars())
    }
}

/// 一条提醒在一台手机上的 collapse id，由句柄和这条提醒的 `apns-id` 算出。
///
/// 同一会话的不同提醒各留一条（通知中心按会话分组、保留历史）；同一条提醒重试时 id 不变，
/// 万一 APNs 其实已经收下了，重发的那条顶掉前一条，不会叠出两条。混进句柄是为了不同手机上
/// 的 id 关联不起来。APNs 限 64 字节，取 32 位十六进制。
pub fn collapse_id(handle: &str, apns_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(handle.as_bytes());
    hasher.update([0]);
    hasher.update(apns_id.as_bytes());
    hasher.finalize()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 把一段文字压成推送里的一行：空白（含换行）并成一个空格，超过 `max_chars` 个字就截断并以
/// 「…」结尾。空的返回 `None`。
pub fn excerpt(text: &str, max_chars: usize) -> Option<String> {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() || max_chars == 0 {
        return None;
    }
    if line.chars().count() <= max_chars {
        return Some(line);
    }
    let mut cut: String = line.chars().take(max_chars - 1).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    Some(cut)
}

/// 中继交给 APNs 的载荷。
///
/// 标题是会话名、副标题是项目名，原样显示；正文用 `loc-key` / `loc-args`，由手机按自己的
/// 语言拼成「等你批准：<摘要>」。智能体的消息正文用 `body` 原样显示，哪一版 App 都认。`thread-id` 是会话：通知中心里一段会话叠成一组。
/// `mutable-content` 不设：没有 Notification Service Extension。`smelt` 里是点开后跳转、
/// 处理掉后撤回用的 id 和类型。
pub fn apns_payload(request: &SendRequest) -> serde_json::Value {
    let mut alert = serde_json::Map::new();
    if let Some(title) = &request.title {
        alert.insert("title".into(), title.as_str().into());
    }
    if let Some(subtitle) = &request.subtitle {
        alert.insert("subtitle".into(), subtitle.as_str().into());
    }
    match &request.detail {
        Some(detail) if request.kind == PushKind::Message => {
            alert.insert("body".into(), detail.as_str().into());
        }
        Some(detail) => {
            alert.insert("loc-key".into(), request.kind.detail_loc_key().into());
            alert.insert("loc-args".into(), serde_json::json!([detail]));
        }
        None => {
            alert.insert("loc-key".into(), request.kind.loc_key().into());
        }
    }
    serde_json::json!({
        "aps": {
            "alert": alert,
            "sound": "default",
            "thread-id": request.session_id,
            "interruption-level": request.kind.interruption_level(),
            "relevance-score": request.kind.relevance_score(),
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

    const APNS_ID: &str = "00000000-0000-4000-8000-000000000000";

    fn send_request() -> SendRequest {
        SendRequest {
            handle: "h".into(),
            kind: PushKind::Approval,
            runtime_id: "runtime-1".into(),
            session_id: "session-1".into(),
            collapse_id: collapse_id("h", APNS_ID),
            apns_id: APNS_ID.into(),
            title: Some("修推送".into()),
            subtitle: Some("smelt".into()),
            detail: Some("rm -rf build".into()),
        }
    }

    #[test]
    fn collapse_ids_are_stable_per_handle_and_event() {
        let id = collapse_id("handle-a", APNS_ID);
        assert_eq!(id.len(), 32);
        assert_eq!(id, collapse_id("handle-a", APNS_ID));
        assert_ne!(
            id,
            collapse_id("handle-a", "00000000-0000-4000-8000-000000000001")
        );
        assert_ne!(id, collapse_id("handle-b", APNS_ID));
    }

    /// 标题、副标题原样带上；正文是带摘要的 loc-key；按会话分组。
    #[test]
    fn apns_payload_carries_title_subtitle_and_localized_detail() {
        assert_eq!(
            apns_payload(&send_request()),
            serde_json::json!({
                "aps": {
                    "alert": {
                        "title": "修推送",
                        "subtitle": "smelt",
                        "loc-key": "SMELT_PUSH_APPROVAL_DETAIL",
                        "loc-args": ["rm -rf build"],
                    },
                    "sound": "default",
                    "thread-id": "session-1",
                    "interruption-level": "time-sensitive",
                    "relevance-score": 1.0,
                },
                "smelt": {
                    "kind": "approval",
                    "runtime_id": "runtime-1",
                    "session_id": "session-1",
                },
            })
        );
    }

    /// 什么文字都没有时只剩一句本地化的完整提醒。
    #[test]
    fn apns_payload_without_text_falls_back_to_the_plain_key() {
        let mut request = send_request();
        request.kind = PushKind::Success;
        request.title = None;
        request.subtitle = None;
        request.detail = None;
        let payload = apns_payload(&request);
        assert_eq!(
            payload["aps"]["alert"],
            serde_json::json!({ "loc-key": "SMELT_PUSH_SUCCESS" })
        );
        assert_eq!(payload["aps"]["interruption-level"], "active");
        assert_eq!(payload["smelt"]["kind"], "success");
    }

    /// 智能体的消息正文原样放进 `body`，不走本地化模板。
    #[test]
    fn apns_payload_shows_message_body_verbatim() {
        let mut request = send_request();
        request.kind = PushKind::Message;
        request.detail = Some("巡检发现 3 台机器磁盘告警".into());
        let payload = apns_payload(&request);
        assert_eq!(
            payload["aps"]["alert"],
            serde_json::json!({
                "title": "修推送",
                "subtitle": "smelt",
                "body": "巡检发现 3 台机器磁盘告警",
            })
        );
        assert_eq!(payload["aps"]["interruption-level"], "active");
        assert_eq!(payload["smelt"]["kind"], "message");
        assert!(!PushKind::Message.is_request());
    }

    /// 消息正文比摘要长：按消息的上限查。
    #[test]
    fn message_body_has_its_own_limit() {
        let mut request = send_request();
        request.kind = PushKind::Message;
        request.detail = Some("x".repeat(MESSAGE_MAX_CHARS));
        assert!(request.text_within_limits());
        request.detail = Some("x".repeat(MESSAGE_MAX_CHARS + 1));
        assert!(!request.text_within_limits());
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
        assert_eq!(send["apns_id"], APNS_ID);
        assert_eq!(send["detail"], "rm -rf build");
    }

    /// 没有文字段的旧请求照样解得开。
    #[test]
    fn text_fields_are_optional_on_the_wire() {
        let request: SendRequest = serde_json::from_value(serde_json::json!({
            "handle": "h",
            "kind": "failure",
            "runtime_id": "r",
            "session_id": "s",
            "collapse_id": "c",
            "apns_id": APNS_ID,
        }))
        .unwrap();
        assert_eq!(request.title, None);
        assert_eq!(request.detail, None);
        assert!(request.text_within_limits());
    }

    #[test]
    fn excerpts_collapse_whitespace_and_truncate() {
        assert_eq!(excerpt("  \n ", 10), None);
        assert_eq!(excerpt("a\n\n b\tc ", 10).as_deref(), Some("a b c"));
        assert_eq!(excerpt("一二三四五六", 6).as_deref(), Some("一二三四五六"));
        assert_eq!(excerpt("一二三四五六七", 6).as_deref(), Some("一二三四五…"));
        assert_eq!(
            excerpt("ab cd ef", 4).as_deref(),
            Some("ab…"),
            "截断处不留空格"
        );
        let long = excerpt(&"字".repeat(500), DETAIL_MAX_CHARS).unwrap();
        assert_eq!(long.chars().count(), DETAIL_MAX_CHARS);
    }

    #[test]
    fn overlong_text_is_out_of_limits() {
        let mut request = send_request();
        assert!(request.text_within_limits());
        request.detail = Some("x".repeat(DETAIL_MAX_CHARS + 1));
        assert!(!request.text_within_limits());
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
