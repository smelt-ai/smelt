//! 新对话默认用哪个智能体的判定规则。存取在运行时。

use crate::agent_definition::AgentDefinition;

/// 侧栏「新对话」这一下该做什么。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewConversationIntent {
    /// 没有可对话的智能体。去创建，不要开会话。
    CreateAgent,
    /// 有可对话的智能体，但没有有效默认。让用户选一次。
    PickAgent,
    /// 有有效默认。直接用它开对话。
    Start { agent_id: String },
}

/// 一个可以拿来开对话的智能体。名字是定义里的原文，显示名由界面自己处理。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationAgentChoice {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewConversationResolution {
    pub intent: NewConversationIntent,
    /// 存档里的 id 已经对不上任何可对话智能体。调用方应清掉记录。
    pub clear_stored_default: bool,
    /// 有效默认的原文名字。只有 [`NewConversationIntent::Start`] 时有值。
    pub default_name: Option<String>,
    /// 能开对话、且不是这次未提交草稿的智能体。
    pub ready: Vec<ConversationAgentChoice>,
}

/// 侧栏「新对话」的决议。
///
/// `draft_agent_id` 是还没点「开始对话」的新建草稿。它还不是默认，也不能出现在
/// 选择里，否则取消之前这场对话就已经归了人。
///
/// 只有一个可对话智能体时也不猜。没被选过就是 [`NewConversationIntent::PickAgent`]。
pub fn resolve_new_conversation(
    agents: &[AgentDefinition],
    stored_default_id: Option<&str>,
    draft_agent_id: Option<&str>,
) -> NewConversationResolution {
    let draft = draft_agent_id.map(str::trim).filter(|id| !id.is_empty());
    let ready = agents
        .iter()
        .filter(|agent| agent.is_conversation_ready())
        .filter(|agent| draft != Some(agent.id.as_str()))
        .map(|agent| ConversationAgentChoice {
            id: agent.id.clone(),
            name: agent.name.clone(),
        })
        .collect::<Vec<_>>();

    let stored = stored_default_id.map(str::trim).filter(|id| !id.is_empty());
    let default = stored.and_then(|id| ready.iter().find(|agent| agent.id == id));
    let default_id = default.map(|agent| agent.id.clone());
    let default_name = default.map(|agent| agent.name.clone());
    let intent = if let Some(agent_id) = default_id {
        NewConversationIntent::Start { agent_id }
    } else if ready.is_empty() {
        NewConversationIntent::CreateAgent
    } else {
        NewConversationIntent::PickAgent
    };

    NewConversationResolution {
        intent,
        clear_stored_default: stored.is_some() && default.is_none(),
        default_name,
        ready,
    }
}
