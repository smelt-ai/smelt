//! 几个功能都要的会话派生事实：对话叫什么、是哪家 CLI、是不是自动化跑出来的。状态见
//! [`WorkspaceConversation::status`]。都从工作区目录和会话状态现算，规则只有这一份：Rust 插件
//! 直接调，JS 插件经 `conversation_facts` 拿。

use crate::model::agent_kind::{ConversationAgentKind, TerminalAgentKind};
use crate::model::daemon_state::DaemonSessionState;
use crate::model::session_control::is_background_acp_session_id;
use crate::model::workspace_directory::WorkspaceConversation;

/// 对话的名字：运行时算好的展示名（对话的、或第一格终端的），没有就用用户起的名字；都没有
/// 是 `None`，由画的一方写通用名（「对话」「终端」）。
pub fn conversation_name<'a>(
    conversation: &'a WorkspaceConversation,
    states: &'a [DaemonSessionState],
) -> Option<&'a str> {
    let lead = conversation
        .acp
        .as_ref()
        .map(|acp| acp.sid.as_str())
        .or_else(|| {
            conversation
                .terminals
                .first()
                .and_then(|terminal| terminal.id.as_deref())
        });
    let named = lead
        .and_then(|id| states.iter().find(|state| state.id == id))
        .and_then(|state| state.conversation_name.as_deref());
    [named, conversation.custom_title.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|name| !name.is_empty())
}

/// 对话是哪家 CLI。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationAgent {
    /// 跟智能体的对话（ACP）。
    Conversation(ConversationAgentKind),
    /// 终端里跑的 CLI。
    Terminal(TerminalAgentKind),
}

impl ConversationAgent {
    pub fn id(self) -> &'static str {
        match self {
            Self::Conversation(kind) => kind.id(),
            Self::Terminal(kind) => kind.id(),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Conversation(kind) => kind.label(),
            Self::Terminal(kind) => kind.label(),
        }
    }

    /// 图标的资源路径（`smelt-icons/agent-<id>.svg`）。
    pub fn icon_asset(self) -> String {
        match self {
            Self::Conversation(kind) => kind.icon_asset(),
            Self::Terminal(kind) => kind.icon_asset(),
        }
    }
}

/// 对话是哪家 CLI：跟智能体的对话按它的智能体；终端按第一格的会话状态报的提供方，没报的
/// 按它的启动命令。认不出是 `None`。
pub fn conversation_agent(
    conversation: &WorkspaceConversation,
    states: &[DaemonSessionState],
) -> Option<ConversationAgent> {
    if let Some(acp) = &conversation.acp {
        return ConversationAgentKind::from_id(&acp.agent).map(ConversationAgent::Conversation);
    }
    let first = conversation.terminals.first()?;
    first
        .id
        .as_deref()
        .and_then(|id| states.iter().find(|state| state.id == id))
        .and_then(|state| state.provider.as_deref())
        .and_then(TerminalAgentKind::from_id)
        .or_else(|| {
            first
                .launch_cmd
                .as_deref()
                .and_then(TerminalAgentKind::from_command_prefix)
        })
        .map(ConversationAgent::Terminal)
}

/// 自动化跑出来的会话：智能体对话绑着自动化，或者是后台起的 ACP 会话。
pub fn is_automation_run(conversation: &WorkspaceConversation) -> bool {
    conversation
        .acp
        .as_ref()
        .is_some_and(|acp| acp.automation_id.is_some() || is_background_acp_session_id(&acp.sid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::workspace_directory::{WorkspaceAcp, WorkspaceTerminal};

    fn acp(agent: &str) -> WorkspaceConversation {
        WorkspaceConversation {
            id: "c".into(),
            acp: Some(WorkspaceAcp {
                sid: "a1".into(),
                agent: agent.into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn terminal(launch: Option<&str>) -> WorkspaceConversation {
        WorkspaceConversation {
            id: "t".into(),
            terminals: vec![WorkspaceTerminal {
                id: Some("t1".into()),
                launch_cmd: launch.map(Into::into),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn state(id: &str) -> DaemonSessionState {
        DaemonSessionState {
            id: id.into(),
            ..Default::default()
        }
    }

    #[test]
    fn name_prefers_the_runtime_then_the_users_then_none() {
        let mut conversation = acp("claude");
        assert_eq!(conversation_name(&conversation, &[]), None);
        conversation.custom_title = Some(" 我起的 ".into());
        assert_eq!(conversation_name(&conversation, &[]), Some("我起的"));
        let mut named = state("a1");
        named.conversation_name = Some("运行时的".into());
        let states = [named];
        assert_eq!(conversation_name(&conversation, &states), Some("运行时的"));
        // 空白的名字不算。
        conversation.custom_title = Some("  ".into());
        assert_eq!(conversation_name(&conversation, &[]), None);
    }

    #[test]
    fn agent_comes_from_the_acp_agent_or_the_first_terminal() {
        assert_eq!(
            conversation_agent(&acp("claude"), &[]),
            Some(ConversationAgent::Conversation(
                ConversationAgentKind::Claude
            ))
        );
        assert_eq!(conversation_agent(&acp("unknown"), &[]), None);

        // 终端：会话状态报了提供方的按它，没报的按启动命令的程序名。
        let launched = terminal(Some("/usr/local/bin/claude --resume"));
        assert_eq!(
            conversation_agent(&launched, &[]),
            Some(ConversationAgent::Terminal(
                TerminalAgentKind::from_id("claude").unwrap()
            ))
        );
        let mut reported = state("t1");
        reported.provider = Some("copilot".into());
        assert_eq!(
            conversation_agent(&launched, &[reported]).map(ConversationAgent::id),
            Some("copilot")
        );
        assert_eq!(conversation_agent(&terminal(Some("zsh")), &[]), None);
        assert_eq!(conversation_agent(&terminal(None), &[]), None);
    }
}
