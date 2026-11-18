//! 运行时自己要告诉人的事：回合为什么没完成、对话为什么结束了。
//!
//! 运行时不知道看的人用什么语言，所以不替人措辞：它发种类和参数，各端按自己的语言说。
//! 桌面、手机用当前界面语言（[`SessionNote::text`]）；推送在运行时里按每台手机登记的语言说
//! （[`SessionNote::text_in`]）。文案在本 crate 的 `locales/ui.yml`。
//!
//! 种类只往后加；读的一方不认识的读成 `Other`，改说细节或原话。

use rust_i18n::t;
use serde::{Deserialize, Serialize};

/// 会话列表上「待处理」那一行里运行时自己说的话。智能体的原话（审批问题、最后一句回复）
/// 不在这里，还在 `pending_question`。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionNote {
    /// 这一轮没有正常完成。
    TurnFailed { outcome: TurnFailure },
    /// 对话结束了。`detail` 是底层报错，没有就空。
    Ended {
        reason: ConversationEndReason,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        detail: String,
    },
    /// 比读的一方新的种类。
    #[serde(other)]
    Other,
}

impl SessionNote {
    /// 按当前界面语言说；[`SessionNote::Other`] 没有自己的话，给 `None`。
    pub fn text(&self) -> Option<String> {
        self.text_in(&rust_i18n::locale())
    }

    /// 按指定语言说（推送用，运行时进程自己的语言不算数）。
    pub fn text_in(&self, locale: &str) -> Option<String> {
        match self {
            Self::TurnFailed { outcome } => Some(outcome.text_in(locale)),
            Self::Ended { reason, detail } => Some(reason.describe_in(detail, locale)),
            Self::Other => None,
        }
    }
}

/// 一轮没有正常完成的原因。用户自己取消的不算。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TurnFailure {
    /// 回复中途断了，重启后可以接着来。
    Interrupted,
    /// 用完了这一轮的最大令牌数。
    MaxTokens,
    /// 用完了这一轮的最大请求数。
    MaxTurnRequests,
    /// 智能体拒绝继续。
    Refused,
    /// 其他没完成的情况。
    Failed,
    /// 比读的一方新的种类。
    #[serde(other)]
    Other,
}

impl TurnFailure {
    /// 按当前界面语言说。
    pub fn text(&self) -> String {
        self.text_in(&rust_i18n::locale())
    }

    /// 按指定语言说；[`TurnFailure::Other`] 说成通用的「没有正常完成」。
    pub fn text_in(&self, locale: &str) -> String {
        match self {
            Self::Interrupted => t!("turn_failure.interrupted", locale = locale),
            Self::MaxTokens => t!("turn_failure.max_tokens", locale = locale),
            Self::MaxTurnRequests => t!("turn_failure.max_turn_requests", locale = locale),
            Self::Refused => t!("turn_failure.refused", locale = locale),
            Self::Failed | Self::Other => t!("turn_failure.failed", locale = locale),
        }
        .into_owned()
    }
}

/// 对话为什么结束了。会话快照里存这个种类（`end_code`），底层报错放在 `end_reason` 当细节；
/// 智能体自己报的错没有种类，`end_reason` 就是它的原话。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConversationEndReason {
    /// 和智能体的连接意外断了。
    ConnectionLost,
    /// 连接接连断开，自动重连了这么多次都没撑住。
    ReconnectExhausted { attempts: u32 },
    /// 自动重连没成（细节是原因）。
    ReconnectFailed,
    /// 旧的智能体进程没能退出，不敢再起新的。
    PreviousAgentStillRunning,
    /// 运行时里驱动这条对话的部分异常退出（细节是原因）。
    DriverCrashed,
    /// 对话被删除了。
    Deleted,
    /// 要恢复的智能体会话已经被另一条对话占着（细节是原因）。
    SessionInUse,
    /// 旧会话记录不存在，没法恢复。
    HistoryMissing,
    /// 智能体不支持加载旧会话，没法恢复历史。
    RestoreUnsupported,
    /// 恢复旧会话失败（细节是原因）。
    RestoreFailed,
    /// 打开对话的请求读不懂。
    InvalidOpenRequest,
    /// 对话已经归档，先恢复才能打开。
    Archived,
    /// 对话没能打开（细节是原因）。
    OpenFailed,
    /// 智能体要求先登录（ACP 的 `auth_required`），细节是它的原话。
    SignInRequired {
        #[serde(default)]
        agent: SignInAgent,
    },
    /// 比读的一方新的种类。
    #[serde(other)]
    Other,
}

/// 要求先登录的是哪家智能体，决定怎么教人登录。只写确认过登录方式的几家，其余说通用的。
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignInAgent {
    Copilot,
    Claude,
    Codex,
    /// 别的智能体，或比读的一方新的种类。
    #[default]
    #[serde(other)]
    Other,
}

impl ConversationEndReason {
    /// 按当前界面语言说这个原因；[`ConversationEndReason::Other`] 没有自己的话，给 `None`。
    pub fn text(&self) -> Option<String> {
        self.text_in(&rust_i18n::locale())
    }

    /// 按指定语言说这个原因。
    pub fn text_in(&self, locale: &str) -> Option<String> {
        let text = match self {
            Self::ConnectionLost => t!("conversation_end.connection_lost", locale = locale),
            Self::ReconnectExhausted { attempts } => t!(
                "conversation_end.reconnect_exhausted",
                locale = locale,
                attempts = attempts
            ),
            Self::ReconnectFailed => t!("conversation_end.reconnect_failed", locale = locale),
            Self::PreviousAgentStillRunning => {
                t!(
                    "conversation_end.previous_agent_still_running",
                    locale = locale
                )
            }
            Self::DriverCrashed => t!("conversation_end.driver_crashed", locale = locale),
            Self::Deleted => t!("conversation_end.deleted", locale = locale),
            Self::SessionInUse => t!("conversation_end.session_in_use", locale = locale),
            Self::HistoryMissing => t!("conversation_end.history_missing", locale = locale),
            Self::RestoreUnsupported => {
                t!("conversation_end.restore_unsupported", locale = locale)
            }
            Self::RestoreFailed => t!("conversation_end.restore_failed", locale = locale),
            Self::InvalidOpenRequest => {
                t!("conversation_end.invalid_open_request", locale = locale)
            }
            Self::Archived => t!("conversation_end.archived", locale = locale),
            Self::OpenFailed => t!("conversation_end.open_failed", locale = locale),
            Self::SignInRequired { agent } => match agent {
                SignInAgent::Copilot => t!("conversation_end.sign_in.copilot", locale = locale),
                SignInAgent::Claude => t!("conversation_end.sign_in.claude", locale = locale),
                SignInAgent::Codex => t!("conversation_end.sign_in.codex", locale = locale),
                SignInAgent::Other => t!("conversation_end.sign_in.other", locale = locale),
            },
            Self::Other => return None,
        };
        Some(text.into_owned())
    }

    /// 原因连同细节按当前界面语言说成一句；不认识的种类只说细节。
    pub fn describe(&self, detail: &str) -> String {
        self.describe_in(detail, &rust_i18n::locale())
    }

    /// 原因连同细节按指定语言说成一句。
    pub fn describe_in(&self, detail: &str, locale: &str) -> String {
        let detail = detail.trim();
        match self.text_in(locale) {
            Some(text) if detail.is_empty() => text,
            Some(text) => t!(
                "conversation_end.with_detail",
                locale = locale,
                reason = text,
                detail = detail
            )
            .into_owned(),
            None => detail.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_end_reason_says_reason_then_detail() {
        assert_eq!(
            serde_json::to_value(ConversationEndReason::ReconnectExhausted { attempts: 3 })
                .unwrap(),
            serde_json::json!({"kind": "reconnect_exhausted", "attempts": 3})
        );
        assert_eq!(
            serde_json::from_value::<ConversationEndReason>(serde_json::json!({"kind": "later"}))
                .unwrap(),
            ConversationEndReason::Other
        );
        assert_eq!(
            ConversationEndReason::ConnectionLost.describe_in("", "en"),
            "The connection dropped unexpectedly"
        );
        assert_eq!(
            ConversationEndReason::RestoreFailed.describe_in("timeout", "en"),
            "Couldn't restore the conversation: timeout"
        );
        assert_eq!(
            ConversationEndReason::RestoreFailed.describe_in("timeout", "zh-CN"),
            "恢复对话失败：timeout"
        );
        assert_eq!(
            ConversationEndReason::Other.describe_in("boom", "en"),
            "boom"
        );
    }

    /// 要求登录按智能体教人怎么登；不认识的智能体（含比读的一方新的）说通用的。
    #[test]
    fn sign_in_required_names_how_to_sign_in() {
        let copilot = ConversationEndReason::SignInRequired {
            agent: SignInAgent::Copilot,
        };
        assert_eq!(
            serde_json::to_value(&copilot).unwrap(),
            serde_json::json!({"kind": "sign_in_required", "agent": "copilot"})
        );
        assert!(copilot.text_in("en").unwrap().contains("/login"));
        assert_eq!(
            serde_json::from_value::<ConversationEndReason>(
                serde_json::json!({"kind": "sign_in_required", "agent": "later"})
            )
            .unwrap(),
            ConversationEndReason::SignInRequired {
                agent: SignInAgent::Other
            }
        );
        assert!(
            ConversationEndReason::SignInRequired {
                agent: SignInAgent::Codex
            }
            .describe_in("auth required", "en")
            .ends_with(": auth required")
        );
    }

    #[test]
    fn notes_nest_and_unknown_kinds_read_as_other() {
        let note = SessionNote::Ended {
            reason: ConversationEndReason::Deleted,
            detail: String::new(),
        };
        assert_eq!(
            serde_json::to_value(&note).unwrap(),
            serde_json::json!({"kind": "ended", "reason": {"kind": "deleted"}})
        );
        assert_eq!(
            serde_json::to_value(SessionNote::TurnFailed {
                outcome: TurnFailure::MaxTokens
            })
            .unwrap(),
            serde_json::json!({"kind": "turn_failed", "outcome": "max_tokens"})
        );
        assert_eq!(
            serde_json::from_value::<SessionNote>(serde_json::json!({"kind": "later"})).unwrap(),
            SessionNote::Other
        );
        assert_eq!(
            serde_json::from_value::<TurnFailure>(serde_json::json!("later")).unwrap(),
            TurnFailure::Other
        );
        assert_eq!(
            SessionNote::TurnFailed {
                outcome: TurnFailure::MaxTokens
            }
            .text_in("zh-CN")
            .as_deref(),
            Some("已达到本轮最大令牌数")
        );
    }
}
