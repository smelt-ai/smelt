//! 工作台路由与侧栏对话的对齐规则。

use super::{
    WorkspaceRoute, agent_conversation_occupies_project_stage, conversation_route_needs_target,
    should_correct_agent_conversation_on_project_stage,
};

#[test]
fn a_restored_conversation_route_waits_for_sessions_then_finds_its_target() {
    // 看哪段对话不落盘：会话还没恢复完就动路由，会把用户甩到一个马上要变的地方。
    assert!(!conversation_route_needs_target(
        false,
        &WorkspaceRoute::Conversation,
        false,
    ));
    assert!(conversation_route_needs_target(
        true,
        &WorkspaceRoute::Conversation,
        false,
    ));
    // 对上了就不再动。
    assert!(!conversation_route_needs_target(
        true,
        &WorkspaceRoute::Conversation,
        true,
    ));
    // 别的路由不归这条管：用户看的就是别处，不许抢。
    for route in [
        WorkspaceRoute::Page("agents".into()),
        WorkspaceRoute::Session,
    ] {
        assert!(
            !conversation_route_needs_target(true, &route, false),
            "{route:?}"
        );
    }
}

#[test]
fn an_agent_conversation_never_occupies_the_project_stage() {
    // 项目舞台画的是活动会话；智能体对话不属于项目列表，落在这条路由上就要纠正。
    assert!(agent_conversation_occupies_project_stage(
        &WorkspaceRoute::Session,
        true,
    ));
    assert!(!agent_conversation_occupies_project_stage(
        &WorkspaceRoute::Conversation,
        true,
    ));
    assert!(!agent_conversation_occupies_project_stage(
        &WorkspaceRoute::Session,
        false,
    ));
}

#[test]
fn project_stage_is_not_corrected_until_sessions_finish_restoring() {
    // 恢复窗口期存档下标会对到先插入的智能体对话；这时不能把舞台拽去「对话」。
    assert!(!should_correct_agent_conversation_on_project_stage(
        false,
        &WorkspaceRoute::Session,
        true,
    ));
    assert!(should_correct_agent_conversation_on_project_stage(
        true,
        &WorkspaceRoute::Session,
        true,
    ));
}

/// 工具面板必须跟着当前上下文走。智能体对话的托管工作目录不进项目分组，
/// 一旦它压不过「上一个打开的项目」，右侧文件树就会显示不相干的仓库。
#[test]
fn agent_conversation_workspace_outranks_the_previously_opened_project() {
    let conversation = "/Users/me/.smelt/workspaces/conversations/4eba284a";
    assert_eq!(
        crate::session_list::tool_panel_context_root(
            Some(conversation),
            None,
            true,
            Some("/repo"),
            Some("/repo"),
        )
        .as_deref(),
        Some(conversation)
    );
}

/// 点项目行仍然直接生效：命中项目组时用组 root，不看对话。
#[test]
fn selecting_a_project_row_still_wins_over_the_open_conversation() {
    assert_eq!(
        crate::session_list::tool_panel_context_root(
            Some("/repo"),
            Some("/repo"),
            false,
            Some("/other"),
            Some("/other"),
        )
        .as_deref(),
        Some("/repo")
    );
}

/// 对话被关掉（或目录已清理）后不能把面板钉死在失效路径上，回退到项目分组。
#[test]
fn closed_conversation_workspace_falls_back_to_the_project_groups() {
    assert_eq!(
        crate::session_list::tool_panel_context_root(
            Some("/Users/me/.smelt/workspaces/conversations/gone"),
            None,
            false,
            Some("/active"),
            Some("/first"),
        )
        .as_deref(),
        Some("/active")
    );
    assert_eq!(
        crate::session_list::tool_panel_context_root(None, None, false, None, Some("/first"))
            .as_deref(),
        Some("/first")
    );
}
