//! 侧栏「新对话」的默认智能体。
//!
//! 这是一次明确选择，不是「最近用过」或「列表第一个」。没选过就让用户选；
//! 选过的人被删掉或已经不能开对话，就把记录清掉，退回选择，不退回裸引擎。

pub use smelt_model::default_conversation::*;

/// 读出侧栏默认智能体。读失败时视为没设过，不阻塞界面。由运行时调用，
/// 随 `agent.host.*` 一起交给客户端。
pub fn load_on(store: &smelt_store::Store) -> Option<String> {
    match store.default_conversation_agent() {
        Ok(id) => id
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty()),
        Err(error) => {
            eprintln!("[default-conversation] 读取默认智能体失败，视为未设置: {error}");
            None
        }
    }
}

/// 设为 `Some` 记下默认智能体，`None` 清掉。
pub fn store_on(store: &smelt_store::Store, agent_id: Option<&str>) -> Result<(), String> {
    store
        .put_default_conversation_agent(agent_id.map(str::trim).filter(|id| !id.is_empty()))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_definition::AgentDefinition;
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
    fn stored_default_round_trips_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            smelt_store::Store::open_or_create(dir.path().join(smelt_store::DATABASE_FILE_NAME))
                .unwrap();
        assert_eq!(load_on(&store), None);
        store_on(&store, Some(" writer ")).unwrap();
        assert_eq!(load_on(&store).as_deref(), Some("writer"));
        store_on(&store, None).unwrap();
        assert_eq!(load_on(&store), None);
    }
}
