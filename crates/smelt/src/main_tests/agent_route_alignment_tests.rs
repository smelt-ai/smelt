//! 工作台路由与智能体对话的对齐规则。
//!
//! 这里锁住一条真实踩过的坑：冷恢复补位必须是一次性的。它曾经每帧都跑，把用户点
//! 侧栏「智能体」返回目录的动作在下一帧顶回对话页，表现为「点了完全没反应」。

use super::{
    WorkspaceRoute, agent_conversation_occupies_project_stage,
    should_correct_agent_conversation_on_project_stage, should_open_restored_agent_conversation,
};

#[test]
fn returning_to_the_agent_catalog_is_not_undone_on_the_next_frame() {
    // 冷恢复第一帧：活动项是智能体对话，路由停在工作台 → 补位打开那段对话。
    assert!(should_open_restored_agent_conversation(
        true,
        false,
        true,
        &WorkspaceRoute::Agents,
    ));

    // 补位过之后用户点「智能体」回目录：后续每一帧都不能再抢路由。
    assert!(!should_open_restored_agent_conversation(
        true,
        true,
        true,
        &WorkspaceRoute::Agents,
    ));
}

#[test]
fn restore_alignment_waits_for_sessions_and_stays_off_other_routes() {
    // 会话还没恢复完就动路由，会把用户甩到一个马上要变的地方。
    assert!(!should_open_restored_agent_conversation(
        false,
        false,
        true,
        &WorkspaceRoute::Agents,
    ));

    // 活动项不是智能体对话时没有要补位的东西。
    assert!(!should_open_restored_agent_conversation(
        true,
        false,
        false,
        &WorkspaceRoute::Agents,
    ));

    // 恢复出来的路由在别处，用户看的就是别处，不许抢。
    for route in [
        WorkspaceRoute::Automations,
        WorkspaceRoute::Session,
        WorkspaceRoute::Plugin {
            key: "com.example/board".into(),
        },
    ] {
        assert!(
            !should_open_restored_agent_conversation(true, false, true, &route),
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

    // 这条是不变量，和一次性补位无关：工作台路由自己管子页，不受它影响。
    assert!(!agent_conversation_occupies_project_stage(
        &WorkspaceRoute::Agents,
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
