//! 会话输入的宿主级路由契约。
//!
//! ACP 是执行通道；插件路由是用户输入先送往远端系统的渠道。两者不能复用同一
//! `AcpUserAction::Prompt` 语义，否则后台 Task delivery 也会被再次路由。daemon
//! 自动化绑定则保留审批/问答 action，但拒绝普通 composer 输入。

use crate::acp_chat::AcpImage;
use smelt_plugin_api::{PluginId, PluginInputRouteBinding};

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConversationBinding {
    #[default]
    Direct,
    /// A daemon-owned automation Run. Approval and elicitation actions still use the ACP action
    /// channel, but ordinary composer prompts must not enter this single-turn execution slot.
    Automation { run_id: String },
    Plugin {
        plugin_id: PluginId,
        route: PluginInputRouteBinding,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationInput {
    /// 一次用户提交跨 daemon 与插件边界保持不变的身份。旧客户端缺失时 daemon
    /// 会补齐；未来远端接口支持幂等键后可直接继续向下传递。
    #[serde(default)]
    pub submission_id: String,
    pub text: String,
    #[serde(default)]
    pub images: Vec<AcpImage>,
}

impl ConversationInput {
    pub fn new(text: String, images: Vec<AcpImage>) -> Self {
        Self {
            submission_id: uuid::Uuid::new_v4().simple().to_string(),
            text,
            images,
        }
    }

    pub fn ensure_submission_id(&mut self) {
        if self.submission_id.trim().is_empty() {
            self.submission_id = uuid::Uuid::new_v4().simple().to_string();
        }
    }
}

/// daemon 拥有的会话输入状态镜像。`ConversationSnapshot` 里外层使用 `Option`：缺失表示
/// 对端版本尚不支持该状态，不能据此把客户端已有的待发预设清空。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationStateSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<ConversationBinding>,
    /// 产品级智能体/控制器身份。它不参与 ACP 协议执行，也不替代输入 binding；
    /// 客户端据此选择插件提供的会话 UI 和生命周期操作。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<smelt_plugin_api::AgentSessionBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_agent_preset: Option<String>,
}

/// 智能体预设只装饰第一次交互输入，不单独制造一轮。空白预设按未配置处理；
/// 图片首包没有正文时，预设本身会成为这一轮的文本部分。
pub fn merge_agent_preset(preset: Option<&str>, input: &str) -> String {
    let Some(preset) = preset.map(str::trim).filter(|preset| !preset.is_empty()) else {
        return input.to_string();
    };
    if input.trim().is_empty() {
        preset.to_string()
    } else {
        format!("{preset}\n\n{input}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationInputRoute {
    Direct,
    Plugin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationSubmitErrorKind {
    /// 请求被明确拒绝，远端没有接受本次提交。
    Rejected,
    /// 已越过可能产生副作用的边界，但结果无法证明；直接重试可能重复。
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationSubmitError {
    pub kind: ConversationSubmitErrorKind,
    pub message: String,
}

impl ConversationSubmitError {
    pub fn rejected(message: impl Into<String>) -> Self {
        Self {
            kind: ConversationSubmitErrorKind::Rejected,
            message: message.into(),
        }
    }

    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            kind: ConversationSubmitErrorKind::Unknown,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ConversationSubmitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ConversationSubmitError {}

#[cfg(test)]
mod tests {
    use super::{ConversationStateSnapshot, merge_agent_preset};

    #[test]
    fn agent_preset_and_first_input_share_one_prompt() {
        assert_eq!(
            merge_agent_preset(Some("你是严谨的代码审查者"), "检查这次改动"),
            "你是严谨的代码审查者\n\n检查这次改动"
        );
        assert_eq!(
            merge_agent_preset(Some("  \n"), "检查这次改动"),
            "检查这次改动"
        );
        assert_eq!(merge_agent_preset(Some("分析图片"), ""), "分析图片");
    }

    #[test]
    fn legacy_conversation_state_does_not_invent_a_direct_binding() {
        let state: ConversationStateSnapshot = serde_json::from_value(serde_json::json!({
            "pending_agent_preset": "review"
        }))
        .unwrap();

        assert!(state.binding.is_none());
    }
}
