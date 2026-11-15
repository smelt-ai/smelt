//! 智能体对话与工具面板上下文的对齐规则。

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
