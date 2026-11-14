use super::{WorkspaceRoute, agents::agent_engine_kinds, settings::ConversationAgentKind};

#[test]
fn product_and_session_routes_are_independent_top_level_routes() {
    assert_eq!(
        serde_json::to_string(&WorkspaceRoute::Agents).unwrap(),
        "\"agents\""
    );
    assert_eq!(
        serde_json::to_string(&WorkspaceRoute::Automations).unwrap(),
        "\"automations\""
    );
    assert_eq!(
        serde_json::to_string(&WorkspaceRoute::Session).unwrap(),
        "\"session\""
    );
    assert_eq!(
        serde_json::from_str::<WorkspaceRoute>("\"session\"").unwrap(),
        WorkspaceRoute::Session
    );
    // 宿主不认识的名字都是插件页面 id；那一页还在不在由启动恢复时查登记（见
    // `Workspace::sync_page_route`），没有了就回会话。
    assert_eq!(
        serde_json::from_str::<WorkspaceRoute>("\"capabilities\"").unwrap(),
        WorkspaceRoute::Page("capabilities".into())
    );
    assert_eq!(
        serde_json::to_string(&WorkspaceRoute::Page("capabilities".into())).unwrap(),
        "\"capabilities\""
    );
}

#[test]
fn agent_creation_engines_come_from_the_provider_registry() {
    let actual = agent_engine_kinds();

    assert_eq!(actual, vec![ConversationAgentKind::Pi]);
}
