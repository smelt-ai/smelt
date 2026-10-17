//! 侧栏「新对话」的默认智能体。
//!
//! 这是一次明确选择，不是「最近用过」或「列表第一个」。没选过就让用户选；
//! 选过的人被删掉或已经不能开对话，就把记录清掉，退回选择，不退回裸引擎。

use serde::{Deserialize, Serialize};

use crate::agent_definition::AgentDefinition;

const SQLITE_NAMESPACE: &str = "preferences";
const SQLITE_KEY: &str = "default_conversation_agent";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDefaultConversationAgent {
    agent_id: String,
}

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

/// 读出侧栏默认智能体。记录损坏时视为没设过，不阻塞界面。由运行时调用，
/// 随 `agent.host.*` 一起交给客户端。
pub fn load_on(store: &smelt_store::Store) -> Option<String> {
    let raw = match store.get_blob(SQLITE_NAMESPACE, SQLITE_KEY) {
        Ok(Some(raw)) => raw,
        Ok(None) => return None,
        Err(error) => {
            eprintln!("[default-conversation] 读取默认智能体失败，视为未设置: {error}");
            return None;
        }
    };
    match serde_json::from_slice::<StoredDefaultConversationAgent>(&raw) {
        Ok(stored) => {
            let id = stored.agent_id.trim();
            (!id.is_empty()).then(|| id.to_string())
        }
        Err(error) => {
            eprintln!("[default-conversation] 默认智能体记录损坏，视为未设置: {error}");
            None
        }
    }
}

/// 设为 `Some` 记下默认智能体，`None` 清掉。
pub fn store_on(store: &smelt_store::Store, agent_id: Option<&str>) -> Result<(), String> {
    match agent_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(agent_id) => {
            let raw = serde_json::to_vec(&StoredDefaultConversationAgent {
                agent_id: agent_id.to_string(),
            })
            .map_err(|error| error.to_string())?;
            store
                .put_blob(SQLITE_NAMESPACE, SQLITE_KEY, &raw)
                .map_err(|error| error.to_string())
        }
        None => store
            .delete_blob(SQLITE_NAMESPACE, SQLITE_KEY)
            .map(|_| ())
            .map_err(|error| error.to_string()),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_kind::ConversationAgentKind;

    fn agent(id: &str, name: &str, engine: ConversationAgentKind) -> AgentDefinition {
        AgentDefinition {
            id: id.to_string(),
            name: name.to_string(),
            engine_kind_id: engine.id().to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn no_agents_goes_to_create_and_does_not_start() {
        let resolution = resolve_new_conversation(&[], None, None);
        assert_eq!(resolution.intent, NewConversationIntent::CreateAgent);
        assert!(!resolution.clear_stored_default);
        assert!(resolution.ready.is_empty());
    }

    #[test]
    fn a_single_ready_agent_is_not_guessed() {
        let agents = [agent("writer", "写作", ConversationAgentKind::Pi)];
        let resolution = resolve_new_conversation(&agents, None, None);
        assert_eq!(resolution.intent, NewConversationIntent::PickAgent);
        assert_eq!(resolution.ready.len(), 1);
        assert!(resolution.default_name.is_none());
    }

    #[test]
    fn stored_default_starts_that_agent() {
        let agents = [
            agent("writer", "写作", ConversationAgentKind::Pi),
            agent("research", "研究", ConversationAgentKind::Pi),
        ];
        let resolution = resolve_new_conversation(&agents, Some("research"), None);
        assert_eq!(
            resolution.intent,
            NewConversationIntent::Start {
                agent_id: "research".to_string()
            }
        );
        assert_eq!(resolution.default_name.as_deref(), Some("研究"));
        assert!(!resolution.clear_stored_default);
    }

    #[test]
    fn missing_or_unready_default_is_cleared_instead_of_falling_back() {
        let agents = [agent("claude", "Claude", ConversationAgentKind::Claude)];
        let missing = resolve_new_conversation(&agents, Some("gone"), None);
        assert_eq!(missing.intent, NewConversationIntent::CreateAgent);
        assert!(missing.clear_stored_default);
        assert!(missing.ready.is_empty());

        let unready = resolve_new_conversation(&agents, Some("claude"), None);
        assert_eq!(unready.intent, NewConversationIntent::CreateAgent);
        assert!(unready.clear_stored_default, "认得出引擎不等于能开对话");
    }

    #[test]
    fn draft_is_excluded_until_the_user_commits_it() {
        let agents = [agent("draft", "草稿", ConversationAgentKind::Pi)];
        let resolution = resolve_new_conversation(&agents, Some("draft"), Some("draft"));
        assert_eq!(resolution.intent, NewConversationIntent::CreateAgent);
        assert!(resolution.clear_stored_default);
        assert!(resolution.ready.is_empty());
    }

    #[test]
    fn stored_default_round_trips_only_the_agent_id() {
        let stored = StoredDefaultConversationAgent {
            agent_id: "writer".to_string(),
        };
        let json = serde_json::to_value(&stored).unwrap();
        assert_eq!(json, serde_json::json!({ "agent_id": "writer" }));
        let restored: StoredDefaultConversationAgent = serde_json::from_value(json).unwrap();
        assert_eq!(restored.agent_id, "writer");
        assert!(
            serde_json::from_value::<StoredDefaultConversationAgent>(
                serde_json::json!({ "agent_id": "writer", "engine": "pi" })
            )
            .is_err()
        );
    }
}
