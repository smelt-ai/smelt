//! ACP 视图状态决策、Markdown/差异展示辅助逻辑的回归测试。

use super::trajectory::runtime_debug_contexts;
use super::{
    CachedDiff, ComposerMenuSection, LoadedSkillsRefresh, LoadedSkillsSnapshot, OwnerIdCache,
    RESTORED_ENTRY_HEIGHT_HINT_PX, TurnOutcomeAction, ViewHistoryIdentity, append_prompt_text,
    apply_pending_config_selection, background_task_elapsed, background_task_one_line,
    background_task_summary, build_conversation_layout, build_conversation_layout_with_timings,
    build_markdown_cache, build_tool_image_parts, cached_diff_stats, can_load_older_history,
    classify_attached_paths, compact_token_count, compact_tool_headline,
    composer_config_section_label, composer_menu_sections, composer_model_label,
    composer_native_queue_shortcut_hint, composer_next_turn_notice,
    composer_usage_breakdown, config_rejection_is_new, config_selection_is_pending,
    consume_composer_restore, conversation_input_for_submit, diff_cache_matches_output,
    diff_stats_for_output, dispatch_elapsed_ms, dispatch_progress, entries_to_fade_in,
    external_clipboard_image_paths, filter_trajectory_events, format_attached_paths,
    is_active_permission_selection, is_fresh_conversation_start, is_match_count_line,
    is_new_conversation_command, is_stale_blank_history_id, latest_user_message_index,
    loaded_capabilities_summary, loaded_entries_end, loaded_skills_refresh, memory_changes_summary,
    memory_changes_to_show, merge_rejected_prompt, merge_snapshot_entries,
    model_label_with_provider, native_queue_item_kind_label, next_prompt_dispatch_pending,
    notice_to_show, overlay_model_state, overlay_session_configs, pending_config_choice_names,
    plan_current_step, prefix_skill_command, process_group_header_label, progress_has_details,
    progress_summary, provider_retry_label, provider_switch_value, reconcile_pending_config_values,
    refresh_markdown_cache, rows_with_changed_shape, running_background_tasks, running_subagents,
    search_summary_text, selected_provider_group, session_trajectory_events,
    settled_thought_elapsed, settled_thought_label, should_apply_snapshot_revision,
    should_cancel_for_immediate_prompt, should_clear_history_session_id_after_snapshot,
    should_seed_restored_height_hints, subagent_elapsed_ms, subagent_run_title, subagent_status,
    task_body_from_selection, tool_card_default_expanded, tool_image_cache_matches_output,
    tool_output_has_content, tool_result_summary, tool_uses_compact_process_row, trajectory_counts,
    turn_outcome_card, usage_percent, usage_ring_color, usage_warn_color, user_bubble_enter_ready,
    view_history_identity,
};
use gpui::{
    ClipboardEntry, ExternalPaths, Focusable as _, FollowMode, ListAlignment, ListState, px,
};
use smelt_plugin_api::model::acp_chat::{
    AcpEntry, TaskNote, ToolCallStatus, ToolKind, ToolOutputPart,
};
use smelt_plugin_api::model::acp_conn::{ModelProviderGroup, ModelState, SessionConfigState};
use smelt_plugin_api::model::acp_session::{
    AcpTurnOutcome, ApprovalDetailsView, BackgroundTaskStatus, BackgroundTaskView,
    ElicitFieldKindView, ElicitFieldView, ElicitOptionView, PendingElicitation, PendingPermission,
    PermissionOptionKindView, PermissionOptionView, PlanEntryStatusView, PlanEntryView, PlanView,
    RuntimeDebug, RuntimeDebugModel, RuntimeDebugModelCall, RuntimeDebugTool, SubagentStatus,
    SubagentView, TurnTiming,
};
use smelt_plugin_api::model::agent_kind::{ConversationAgentKind, ConversationLaunchSpec};
use smelt_plugin_api::model::daemon_state::DaemonPhase;
use smelt_plugin_api::ui::ui_theme;

#[cfg(target_os = "macos")]
mod snapshots;

#[test]
fn cleanshot_external_png_is_treated_as_an_image_attachment() {
    let path = std::path::PathBuf::from(
        "/Users/shared/Library/Application Support/CleanShot/media/example/screenshot.PNG",
    );
    let entries = vec![
        ClipboardEntry::ExternalPaths(ExternalPaths(vec![path.clone()].into())),
        ClipboardEntry::String(gpui::ClipboardString::new(path.display().to_string())),
    ];

    assert_eq!(external_clipboard_image_paths(&entries), vec![path]);
}

/// 输入栏能力按钮的提示：只说有的那一类，都没有时明说。
#[test]
fn loaded_capabilities_summary_names_only_what_is_loaded() {
    assert_eq!(loaded_capabilities_summary(0, 0), "No skills or MCP loaded");
    assert_eq!(loaded_capabilities_summary(2, 0), "2 skills loaded");
    assert_eq!(loaded_capabilities_summary(0, 1), "1 MCP server loaded");
    assert_eq!(
        loaded_capabilities_summary(2, 1),
        "2 skills and 1 MCP server loaded"
    );
}

#[test]
fn loaded_skills_are_not_rescanned_until_launch_or_cwd_changes() {
    let launch = ConversationLaunchSpec::from_command("pi");
    let cwd = Some("/tmp/project".to_string());
    let cached = LoadedSkillsSnapshot {
        launch: launch.clone(),
        cwd: cwd.clone(),
        skills: Vec::new(),
        mcp_servers: Vec::new(),
    };

    assert_eq!(
        loaded_skills_refresh(ConversationAgentKind::Pi, &launch, &cwd, Some(&cached)),
        LoadedSkillsRefresh::Keep,
        "同一场 Pi 对话的静态重绘不能再扫技能目录"
    );
    assert_eq!(
        loaded_skills_refresh(ConversationAgentKind::Pi, &launch, &cwd, None),
        LoadedSkillsRefresh::Rescan
    );
    let changed_launch = launch.clone().with_env("SMELT_AGENT_PLUGIN_ARGS", "[]");
    assert_eq!(
        loaded_skills_refresh(
            ConversationAgentKind::Pi,
            &changed_launch,
            &cwd,
            Some(&cached)
        ),
        LoadedSkillsRefresh::Rescan
    );
    assert_eq!(
        loaded_skills_refresh(ConversationAgentKind::Pi, &launch, &None, Some(&cached)),
        LoadedSkillsRefresh::Rescan
    );
    assert_eq!(
        loaded_skills_refresh(ConversationAgentKind::Claude, &launch, &cwd, Some(&cached)),
        LoadedSkillsRefresh::Clear
    );
    assert_eq!(
        loaded_skills_refresh(ConversationAgentKind::Claude, &launch, &cwd, None),
        LoadedSkillsRefresh::Keep
    );
}

#[test]
fn external_non_image_file_is_left_for_normal_text_paste() {
    let path = std::path::PathBuf::from("/workspace/project/README.md");
    let entries = vec![
        ClipboardEntry::ExternalPaths(ExternalPaths(vec![path].into())),
        ClipboardEntry::String(gpui::ClipboardString::new("README.md".into())),
    ];

    assert!(external_clipboard_image_paths(&entries).is_empty());
}

#[test]
fn classify_attached_paths_splits_images_from_other_files() {
    let png = std::path::PathBuf::from("/tmp/shot.PNG");
    let jpeg = std::path::PathBuf::from("/tmp/photo.jpeg");
    let rust = std::path::PathBuf::from("/repo/src/main.rs");
    let (images, files) = classify_attached_paths(&[png.clone(), rust.clone(), jpeg.clone()], true);
    assert_eq!(images, vec![png, jpeg]);
    assert_eq!(files, vec![rust]);
}

#[test]
fn classify_attached_paths_keeps_images_as_paths_when_agent_cannot_take_them() {
    let png = std::path::PathBuf::from("/tmp/shot.png");
    let (images, files) = classify_attached_paths(std::slice::from_ref(&png), false);
    assert!(images.is_empty());
    assert_eq!(files, vec![png]);
}

#[test]
fn attached_project_files_are_inserted_as_at_mentions() {
    assert_eq!(
        format_attached_paths(
            &[
                std::path::PathBuf::from("/repo/src/main.rs"),
                std::path::PathBuf::from("/tmp/a b.md"),
            ],
            Some("/repo"),
        ),
        "@src/main.rs \"/tmp/a b.md\""
    );
}

#[test]
fn composer_model_label_prefers_the_actual_model_name() {
    let model = ModelState {
        config_id: "model".into(),
        current_value: "opencode-go/deepseek-v4-flash".into(),
        current_name: "DeepSeek V4 Flash Vision Exp".into(),
        options: Vec::new(),
        provider_groups: Vec::new(),
    };
    assert_eq!(
        composer_model_label(Some(&model), "OpenCode"),
        "DeepSeek V4 Flash Vision Exp"
    );

    let unnamed = ModelState {
        current_name: "  ".into(),
        current_value: "grok-4".into(),
        ..model
    };
    assert_eq!(composer_model_label(Some(&unnamed), "OpenCode"), "grok-4");
    assert_eq!(composer_model_label(None, "OpenCode"), "OpenCode");
}

#[test]
fn model_popover_identifies_thinking_configs_and_filters_models() {
    use super::render::model_popover::{is_thinking_config, subsequence_score};

    // ACP 协议带 `is_thought_level: true` 时不靠写死的 config_id 猜测也能识别。
    let custom_id_thinking = SessionConfigState {
        config_id: "custom_reasoning".into(),
        name: "Thinking Level".into(),
        description: None,
        current_name: "High".into(),
        options: vec![
            ("low".into(), "Low".into()),
            ("medium".into(), "Medium".into()),
            ("high".into(), "High".into()),
            ("extra".into(), "Extra".into()),
        ],
        boolean: None,
        smelt: None,
        is_thought_level: true,
    };
    assert!(is_thinking_config(
        &custom_id_thinking,
        ConversationAgentKind::Antigravity
    ));
    assert!(!is_thinking_config(
        &mode_config("agent"),
        ConversationAgentKind::Antigravity
    ));

    assert!(subsequence_score("Gemini 4 Neon", "g4n").is_some());
    assert!(subsequence_score("Gemini 4 Neon", "opus").is_none());
    assert!(
        subsequence_score("Opus 4.6", "op").unwrap()
            > subsequence_score("Claude Opus 4.6", "op").unwrap()
    );
}

#[test]
fn model_pill_menu_lists_provider_and_model_before_session_configs() {
    // 胶囊写的是模型名，单 provider/单模型也得列出来，否则点开只剩「权限模式」。
    assert_eq!(
        composer_menu_sections(1, 1, 1),
        vec![
            ComposerMenuSection::Provider,
            ComposerMenuSection::Model,
            ComposerMenuSection::Config(0),
        ]
    );
    assert_eq!(
        composer_menu_sections(2, 6, 2),
        vec![
            ComposerMenuSection::Provider,
            ComposerMenuSection::Model,
            ComposerMenuSection::Config(0),
            ComposerMenuSection::Config(1),
        ]
    );
    // agent 没上报模型时才只剩会话配置。
    assert_eq!(
        composer_menu_sections(0, 0, 1),
        vec![ComposerMenuSection::Config(0)]
    );
    assert!(composer_menu_sections(0, 0, 0).is_empty());
}

fn mode_config(current: &str) -> SessionConfigState {
    SessionConfigState {
        config_id: "mode".into(),
        name: "Mode".into(),
        description: None,
        current_name: if current == "plan" {
            "Plan".into()
        } else {
            "Agent".into()
        },
        options: vec![
            ("agent".into(), "Agent".into()),
            ("plan".into(), "Plan".into()),
            ("ask".into(), "Read-only".into()),
        ],
        boolean: None,
        smelt: None,
        is_thought_level: false,
    }
}

fn sample_model(current: &str) -> ModelState {
    ModelState {
        config_id: "model".into(),
        current_value: current.into(),
        current_name: if current == "fast" {
            "Fast".into()
        } else {
            "GPT-5.6-Sol".into()
        },
        options: vec![
            ("sol".into(), "GPT-5.6-Sol".into()),
            ("fast".into(), "Fast".into()),
        ],
        provider_groups: Vec::new(),
    }
}

#[test]
fn pending_config_selection_shows_immediately_and_keeps_latest() {
    let mut pending = Vec::new();
    apply_pending_config_selection(&mut pending, "mode".into(), "plan".into(), Some("agent"));
    apply_pending_config_selection(&mut pending, "mode".into(), "ask".into(), Some("agent"));
    apply_pending_config_selection(&mut pending, "model".into(), "fast".into(), Some("sol"));

    let configs = overlay_session_configs(&[mode_config("agent")], &pending);
    assert_eq!(configs[0].current_name, "Read-only");
    assert!(config_selection_is_pending(&pending, "mode"));

    let model = overlay_model_state(&sample_model("sol"), &pending);
    assert_eq!(model.current_value, "fast");
    assert_eq!(model.current_name, "Fast");
    assert_eq!(composer_model_label(Some(&model), "Grok"), "Fast");
}

#[test]
fn selecting_confirmed_value_clears_that_pending_entry() {
    let mut pending = Vec::new();
    apply_pending_config_selection(&mut pending, "mode".into(), "plan".into(), Some("agent"));
    apply_pending_config_selection(&mut pending, "mode".into(), "agent".into(), Some("agent"));
    assert!(pending.is_empty());
    assert!(!config_selection_is_pending(&pending, "mode"));
}

#[test]
fn stale_snapshot_does_not_revert_pending_config_overlay() {
    let mut pending = Vec::new();
    apply_pending_config_selection(&mut pending, "mode".into(), "plan".into(), Some("agent"));
    apply_pending_config_selection(&mut pending, "model".into(), "fast".into(), Some("sol"));

    reconcile_pending_config_values(
        &mut pending,
        &[mode_config("agent")],
        Some(&sample_model("sol")),
    );
    assert_eq!(
        pending,
        vec![
            ("mode".into(), "plan".into()),
            ("model".into(), "fast".into())
        ]
    );
    assert_eq!(
        overlay_session_configs(&[mode_config("agent")], &pending)[0].current_name,
        "Plan"
    );
}

#[test]
fn matching_snapshot_clears_pending_config_overlay() {
    let mut pending = Vec::new();
    apply_pending_config_selection(&mut pending, "mode".into(), "plan".into(), Some("agent"));
    apply_pending_config_selection(&mut pending, "model".into(), "fast".into(), Some("sol"));

    reconcile_pending_config_values(
        &mut pending,
        &[mode_config("plan")],
        Some(&sample_model("sol")),
    );
    assert_eq!(pending, vec![("model".into(), "fast".into())]);

    reconcile_pending_config_values(
        &mut pending,
        &[mode_config("plan")],
        Some(&sample_model("fast")),
    );
    assert!(pending.is_empty());
}

#[test]
fn pending_model_stays_until_snapshot_uses_the_picker_value() {
    let mut pending = vec![("model".into(), "github-copilot/kimi-k3".into())];
    let current = ModelState {
        config_id: "model".into(),
        current_value: "github-copilot/claude-opus-5".into(),
        current_name: "Claude Opus 5".into(),
        options: vec![
            (
                "github-copilot/claude-opus-5".into(),
                "Claude Opus 5".into(),
            ),
            ("github-copilot/kimi-k3".into(), "Kimi K3".into()),
        ],
        provider_groups: Vec::new(),
    };
    reconcile_pending_config_values(&mut pending, &[], Some(&current));
    assert_eq!(
        pending,
        vec![("model".into(), "github-copilot/kimi-k3".into())]
    );

    let confirmed = ModelState {
        current_value: "github-copilot/kimi-k3".into(),
        current_name: "Kimi K3".into(),
        ..current
    };
    reconcile_pending_config_values(&mut pending, &[], Some(&confirmed));
    assert!(pending.is_empty());
}

#[test]
fn config_rejection_is_recognized_by_kind_and_id() {
    use smelt_plugin_api::model::acp_session::{AcpNoticeKind, AcpNoticeView};
    let notice = |id, kind| AcpNoticeView {
        id,
        kind,
        text: "Couldn't update the session setting: timeout".into(),
        note: None,
    };
    let rejected = notice(10, AcpNoticeKind::ConfigRejected);
    assert!(config_rejection_is_new(None, Some(&rejected)));
    assert!(!config_rejection_is_new(Some(&rejected), Some(&rejected)));
    assert!(
        config_rejection_is_new(
            Some(&rejected),
            Some(&notice(11, AcpNoticeKind::ConfigRejected))
        ),
        "同一句失败连来两次，第二次也要撤掉勾选"
    );
    assert!(!config_rejection_is_new(
        None,
        Some(&notice(12, AcpNoticeKind::Info))
    ));
    assert!(!config_rejection_is_new(Some(&rejected), None));
}

#[test]
fn next_turn_hint_only_when_pending_and_turn_is_active() {
    assert_eq!(
        composer_config_section_label("Mode", true, true),
        "Mode · applies next turn"
    );
    assert_eq!(composer_config_section_label("Mode", true, false), "Mode");
    assert_eq!(composer_config_section_label("Mode", false, true), "Mode");
    assert_eq!(
        composer_config_section_label("模型", true, true),
        "模型 · applies next turn"
    );
}

#[test]
fn native_queue_copy_names_current_round_and_after_round() {
    assert_eq!(
        composer_native_queue_shortcut_hint().to_string(),
        "Enter adds to the current turn · ⌥Enter sends after the turn"
    );
    assert_eq!(
        native_queue_item_kind_label(false).to_string(),
        "Current turn"
    );
    assert_eq!(native_queue_item_kind_label(true).to_string(), "Next turn");
}

#[test]
fn composer_restore_keeps_only_a_newer_revision() {
    let restored = consume_composer_restore(0, 1, vec!["换方向".into()], Vec::new());
    assert_eq!(restored.last_revision, 1);
    assert_eq!(
        restored.restore_texts.as_deref(),
        Some(&["换方向".to_string()][..])
    );

    let stale = consume_composer_restore(1, 1, vec!["换方向".into()], Vec::new());
    assert_eq!(stale.last_revision, 1);
    assert_eq!(stale.restore_texts, None);

    let image = smelt_plugin_api::model::acp_chat::AcpImage::new("image/png", "aGk=");
    let images_only = consume_composer_restore(1, 2, Vec::new(), vec![image.clone()]);
    assert_eq!(images_only.restore_texts, None);
    assert_eq!(images_only.restore_images, Some(vec![image]));
}

#[test]
fn next_turn_notice_stays_on_the_composer_not_a_toast() {
    let mut pending = Vec::new();
    apply_pending_config_selection(&mut pending, "mode".into(), "plan".into(), Some("agent"));
    apply_pending_config_selection(&mut pending, "fast".into(), "true".into(), Some("false"));
    let fast = SessionConfigState {
        config_id: "fast".into(),
        name: "Fast mode".into(),
        description: None,
        current_name: "Off".into(),
        options: vec![("true".into(), "On".into()), ("false".into(), "Off".into())],
        boolean: Some(false),
        smelt: None,
        is_thought_level: false,
    };
    let names = pending_config_choice_names(
        &pending,
        &[mode_config("agent"), fast],
        Some(&sample_model("sol")),
    );
    assert_eq!(names, vec!["Plan".to_string(), "Fast mode On".to_string()]);
    assert_eq!(
        composer_next_turn_notice(&names, true).as_deref(),
        Some("Plan, Fast mode On · applies next turn")
    );
    assert_eq!(composer_next_turn_notice(&names, false), None);
    assert_eq!(
        composer_next_turn_notice(&["Plan".into()], true).as_deref(),
        Some("Plan · applies next turn")
    );
    assert_eq!(
        composer_next_turn_notice(&["Plan".into(), "Max".into(), "Fast".into()], true).as_deref(),
        Some("Plan and 2 more · applies next turn")
    );
}

#[test]
fn composer_usage_is_readonly_and_only_warns_when_high() {
    assert_eq!(compact_token_count(12_400), "12.4k");
    assert_eq!(compact_token_count(200_000), "200k");
    assert_eq!(compact_token_count(999), "999");
    assert_eq!(usage_percent(164_000, 200_000), 82);
    assert_eq!(usage_warn_color(6), None);
    assert_eq!(usage_warn_color(75), Some(ui_theme::need()));
    assert_eq!(usage_warn_color(90), Some(ui_theme::bad()));
    assert_eq!(usage_ring_color(38), ui_theme::run());
    assert_eq!(usage_ring_color(75), ui_theme::need());
    assert_eq!(usage_ring_color(90), ui_theme::bad());
    let conversation = composer_usage_breakdown(12_400, 200_000, None, None, ui_theme::text());
    assert_eq!(conversation[0].label, "Conversation");
    assert_eq!(conversation[0].tokens, 12_400);
    assert_eq!(conversation.len(), 1);
    let buckets = smelt_plugin_api::model::acp_conn::ContextUsageBreakdown {
        system_prompt: 1_200,
        tools_definition: 11_800,
        rules: 302,
        skills: 3_700,
        mcp_dynamic: 2_200,
        subagent: 1_600,
        summarized: 8_700,
        conversation: 333_000,
    };
    let rows = composer_usage_breakdown(362_502, 1_000_000, None, Some(&buckets), ui_theme::text());
    assert_eq!(
        rows.iter()
            .map(|row| row.label.as_ref())
            .collect::<Vec<_>>(),
        [
            "System prompt",
            "Tool definitions",
            "Rules",
            "Skills",
            "MCP & dynamic tools",
            "Subagent definitions",
            "Summarized conversation",
            "Conversation",
        ]
    );
    assert_eq!(rows.iter().map(|row| row.tokens).sum::<u64>(), 362_502);
}

#[test]
fn model_labels_include_provider_for_scoped_values() {
    assert_eq!(
        model_label_with_provider("sub2api/deepseek-v4-flash", "deepseek-v4-flash"),
        "sub2api · deepseek-v4-flash"
    );
    assert_eq!(
        model_label_with_provider("sonnet-4-5", "Claude Sonnet 4.5"),
        "Claude Sonnet 4.5"
    );
}

#[test]
fn selects_the_provider_containing_the_current_model() {
    let model = ModelState {
        config_id: "model".to_string(),
        current_value: "deepseek/deepseek-v4-pro".to_string(),
        current_name: "DeepSeek-V4-Pro".to_string(),
        options: Vec::new(),
        provider_groups: vec![
            ModelProviderGroup {
                id: "openai".to_string(),
                name: "OpenAI".to_string(),
                options: vec![("openai/gpt-5".to_string(), "GPT-5".to_string())],
                preferred_value: None,
            },
            ModelProviderGroup {
                id: "deepseek".to_string(),
                name: "DeepSeek".to_string(),
                options: vec![(
                    "deepseek/deepseek-v4-pro".to_string(),
                    "DeepSeek-V4-Pro".to_string(),
                )],
                preferred_value: None,
            },
        ],
    };

    assert_eq!(
        selected_provider_group(&model).map(|provider| provider.id.as_str()),
        Some("deepseek")
    );
}

#[test]
fn provider_switch_keeps_the_model_name_when_available() {
    let provider = ModelProviderGroup {
        id: "backup".to_string(),
        name: "Backup".to_string(),
        options: vec![
            ("backup/flash".to_string(), "Flash".to_string()),
            ("backup/pro".to_string(), "Pro".to_string()),
        ],
        preferred_value: None,
    };

    assert_eq!(provider_switch_value(&provider, "Pro"), Some("backup/pro"));
    assert_eq!(
        provider_switch_value(&provider, "Missing"),
        Some("backup/flash")
    );

    // 没有同名模型时落到首选（会话里上次用的或智能体默认），不是组里第一个。
    let provider = ModelProviderGroup {
        preferred_value: Some("backup/pro".to_string()),
        ..provider
    };
    assert_eq!(
        provider_switch_value(&provider, "Missing"),
        Some("backup/pro")
    );
}

#[test]
fn restored_history_height_hints_cover_unmeasured_entries() {
    let state = ListState::new(0, ListAlignment::Top, px(800.));
    state.reset_with_uniform_height(100, px(RESTORED_ENTRY_HEIGHT_HINT_PX));

    assert_eq!(state.item_count(), 100);
    assert_eq!(state.max_offset_for_scrollbar().y, px(9_600.));
}

#[test]
fn first_history_page_on_an_empty_list_stays_scrollable() {
    let state = ListState::new(0, ListAlignment::Top, px(800.))
        .with_uniform_item_height(px(RESTORED_ENTRY_HEIGHT_HINT_PX));
    state.set_follow_mode(FollowMode::Tail);

    // 桌面第一帧省略已提交历史，列表是空的。补上的第一页若没有高度，
    // gpui 会把视口收成 0，条目永远测不出来，舞台只剩一块黑。
    super::reveal_first_history_page(&state, 100);

    assert_eq!(state.item_count(), 100);
    assert_eq!(state.max_offset_for_scrollbar().y, px(9_600.));
    assert!(state.is_following_tail());
}

#[test]
fn restored_height_hints_only_reset_for_new_history_entries() {
    assert!(should_seed_restored_height_hints(
        true, false, true, true, 20, 20,
    ));
    assert!(should_seed_restored_height_hints(
        false, true, true, true, 20, 21,
    ));
    assert!(!should_seed_restored_height_hints(
        false, false, true, true, 20, 20,
    ));
    assert!(!should_seed_restored_height_hints(
        false, true, true, false, 20, 20,
    ));
    assert!(!should_seed_restored_height_hints(
        false, true, true, true, 20, 20,
    ));
    assert!(!should_seed_restored_height_hints(
        true, true, false, true, 20, 21,
    ));
    assert!(!should_seed_restored_height_hints(
        true, true, true, true, 0, 0,
    ));
}

#[test]
fn selected_task_body_rejects_whitespace_and_preserves_selected_content() {
    assert_eq!(task_body_from_selection(" \n\t ".into()), None);
    assert_eq!(
        task_body_from_selection("  keep this indentation\nnext line  ".into()),
        Some("  keep this indentation\nnext line  ".into())
    );
}

#[test]
fn appended_prompt_keeps_the_caret_after_inserted_text() {
    for (current, text, expected) in [
        ("", "检查当前改动", "检查当前改动 "),
        ("已有草稿", "检查当前改动", "已有草稿 检查当前改动 "),
        ("已有草稿 ", "检查当前改动", "已有草稿 检查当前改动 "),
    ] {
        let (value, cursor_offset) = append_prompt_text(current, text);
        assert_eq!(value, expected);
        assert_eq!(cursor_offset, value.len());
    }
}

#[test]
fn search_summary_recognizes_match_count_lines() {
    // 汇总行：各形态都应识别
    assert!(is_match_count_line("found 3 matches"));
    assert!(is_match_count_line("Found 2 matches"));
    assert!(is_match_count_line("3 matches"));
    assert!(is_match_count_line("(5 matches)"));
    assert!(is_match_count_line("12 results"));
    assert!(is_match_count_line("found 1 match"));
    // 普通匹配行 / 无数字行不误判
    assert!(!is_match_count_line("src/a.rs:12: found a match here"));
    assert!(!is_match_count_line("fn foo() -> bool { matches!() }"));
    assert!(!is_match_count_line("no matches"));
    assert!(!is_match_count_line(""));
    assert!(!is_match_count_line("path/to/file.rs"));
}

#[test]
fn search_summary_reuses_output_line_verbatim() {
    // 头部摘要直接复用输出里的汇总行原文，与展开内容一字不差。
    let parts = vec![ToolOutputPart::Text(
        "src/a.rs:12: foo\nsrc/b.rs:5: bar\n\nfound 3 matches".into(),
    )];
    assert_eq!(
        search_summary_text(&parts),
        Some("found 3 matches".to_string())
    );

    // 无汇总行 → None（头部不显示摘要）
    let parts2 = vec![ToolOutputPart::Text(
        "src/a.rs:12: foo\nsrc/b.rs:5: bar".into(),
    )];
    assert_eq!(search_summary_text(&parts2), None);

    // 代码围栏包裹也要能认出汇总行
    let parts3 = vec![ToolOutputPart::Text("```console\n(3 matches)\n```".into())];
    assert_eq!(
        search_summary_text(&parts3),
        Some("(3 matches)".to_string())
    );
}

#[test]
fn completed_tools_expose_a_compact_result_summary() {
    let read_output = vec![ToolOutputPart::Text("one\ntwo\nthree\n".into())];
    assert_eq!(
        tool_result_summary(ToolKind::Read, ToolCallStatus::Completed, &read_output).as_deref(),
        Some("3 lines")
    );

    let execute_output = vec![ToolOutputPart::Text(
        "test a ... ok\ntest b ... ok\n".into(),
    )];
    assert_eq!(
        tool_result_summary(
            ToolKind::Execute,
            ToolCallStatus::Completed,
            &execute_output,
        )
        .as_deref(),
        Some("2 lines of output")
    );

    assert_eq!(
        tool_result_summary(
            ToolKind::Search,
            ToolCallStatus::Completed,
            &[ToolOutputPart::Text("Found 12 matches".into())],
        )
        .as_deref(),
        Some("Found 12 matches")
    );
    assert_eq!(
        tool_result_summary(ToolKind::Read, ToolCallStatus::InProgress, &read_output),
        None,
        "运行中的工具不应把尚未稳定的输出包装成最终结果"
    );
}

#[test]
fn paged_history_reconnect_fallback_keeps_the_loaded_tail() {
    let mut entries = vec![AcpEntry::User("900".into()), AcpEntry::User("901".into())];
    let mut loaded_offset = 900;
    let mut entries_total = 1_000;
    let retained_end = loaded_entries_end(loaded_offset, entries.len());

    assert_eq!(retained_end, 902);
    assert_eq!(
        merge_snapshot_entries(
            &mut entries,
            &mut loaded_offset,
            &mut entries_total,
            retained_end,
            retained_end,
            Vec::new(),
            false,
        ),
        None
    );
    assert_eq!(loaded_offset, 900);
    assert_eq!(entries.len(), 2);
}

#[test]
fn history_pagination_does_not_require_a_live_control_connection() {
    assert!(can_load_older_history(false, 900));
    assert!(!can_load_older_history(true, 900));
    assert!(!can_load_older_history(false, 0));
}

#[test]
fn recognizes_only_the_builtin_new_command() {
    assert!(is_new_conversation_command("/clear"));
    assert!(is_new_conversation_command("  /CLEAR  "));
    assert!(!is_new_conversation_command("/clear please"));
    assert!(!is_new_conversation_command("please /clear"));
    assert!(!is_new_conversation_command("/new"));
}

#[test]
fn fresh_conversation_start_is_the_only_silent_startup() {
    assert!(is_fresh_conversation_start(
        &DaemonPhase::Connecting,
        true,
        false,
        false,
    ));
    assert!(!is_fresh_conversation_start(
        &DaemonPhase::Idle,
        true,
        false,
        false,
    ));
    assert!(!is_fresh_conversation_start(
        &DaemonPhase::Connecting,
        false,
        false,
        false,
    ));
    assert!(!is_fresh_conversation_start(
        &DaemonPhase::Connecting,
        true,
        true,
        false,
    ));
    assert!(!is_fresh_conversation_start(
        &DaemonPhase::Connecting,
        true,
        false,
        true,
    ));
}

#[test]
fn starting_placeholder_only_covers_non_fresh_empty_conversations() {
    use super::should_show_starting_placeholder;

    assert!(!should_show_starting_placeholder(
        &DaemonPhase::Connecting,
        true,
        false,
        false,
    ));
    assert!(should_show_starting_placeholder(
        &DaemonPhase::Connecting,
        true,
        true,
        false,
    ));
    assert!(should_show_starting_placeholder(
        &DaemonPhase::Connecting,
        true,
        false,
        true,
    ));
    assert!(!should_show_starting_placeholder(
        &DaemonPhase::Connecting,
        false,
        true,
        false,
    ));
    assert!(!should_show_starting_placeholder(
        &DaemonPhase::Idle,
        true,
        true,
        false,
    ));
}

#[test]
fn empty_conversation_state_only_shows_before_the_first_turn() {
    use super::should_show_empty_conversation_state;

    assert!(should_show_empty_conversation_state(
        &DaemonPhase::Connecting,
        true,
        false,
        false,
        true,
        false,
    ));
    assert!(should_show_empty_conversation_state(
        &DaemonPhase::Idle,
        true,
        false,
        false,
        true,
        false,
    ));
    assert!(!should_show_empty_conversation_state(
        &DaemonPhase::Connecting,
        true,
        true,
        false,
        true,
        false,
    ));
    assert!(!should_show_empty_conversation_state(
        &DaemonPhase::Idle,
        false,
        false,
        false,
        true,
        false,
    ));
    assert!(!should_show_empty_conversation_state(
        &DaemonPhase::Idle,
        true,
        false,
        true,
        true,
        false,
    ));
    assert!(!should_show_empty_conversation_state(
        &DaemonPhase::Idle,
        true,
        false,
        false,
        false,
        false,
    ));
    assert!(!should_show_empty_conversation_state(
        &DaemonPhase::Idle,
        true,
        false,
        false,
        true,
        true,
    ));
    assert!(!should_show_empty_conversation_state(
        &DaemonPhase::Thinking,
        true,
        false,
        false,
        true,
        false,
    ));
}

#[test]
fn empty_conversation_welcome_stays_while_composer_has_unsent_draft() {
    use super::should_show_empty_conversation_state;

    // 打了几个字、贴了图都还没发出去：快捷起点必须还在。
    assert!(should_show_empty_conversation_state(
        &DaemonPhase::Idle,
        true,
        false,
        false,
        true,
        false,
    ));
}

#[test]
fn blank_runtime_id_is_not_reused_as_history() {
    let same = "blank-session";
    let other = "other-session";
    assert!(is_stale_blank_history_id(true, Some(same), Some(same)));
    assert!(!is_stale_blank_history_id(true, Some(same), Some(other)));
    assert!(!is_stale_blank_history_id(false, Some(same), Some(same)));
    assert!(!is_stale_blank_history_id(true, Some(same), None));
}

#[test]
fn starting_status_copy_explains_resume_and_wait() {
    use super::starting_status_copy;

    assert_eq!(
        starting_status_copy(None, true, "Claude Code", 12),
        (
            "Restoring the last session".to_string(),
            "Restoring the message history and working context".to_string(),
            "Waited 12s".to_string(),
        )
    );
    assert_eq!(
        starting_status_copy(Some("  正在连接 agent  "), false, "Codex", 1),
        (
            "Starting Codex".to_string(),
            "正在连接 agent".to_string(),
            "Waited 1s".to_string(),
        )
    );
}

#[test]
fn immediate_send_cancels_only_after_the_user_chooses_it() {
    assert!(!should_cancel_for_immediate_prompt(
        &DaemonPhase::Connecting,
        false
    ));
    assert!(!should_cancel_for_immediate_prompt(
        &DaemonPhase::Connecting,
        true
    ));
    assert!(!should_cancel_for_immediate_prompt(
        &DaemonPhase::Idle,
        false
    ));
    assert!(should_cancel_for_immediate_prompt(&DaemonPhase::Idle, true));
    assert!(should_cancel_for_immediate_prompt(
        &DaemonPhase::Thinking,
        false
    ));
    assert!(should_cancel_for_immediate_prompt(
        &DaemonPhase::AwaitingApproval,
        false
    ));
    assert!(should_cancel_for_immediate_prompt(
        &DaemonPhase::WaitingForUser,
        false
    ));
}

#[test]
fn stale_idle_does_not_clear_unconfirmed_dispatch() {
    // 旧 Idle 快照不能清掉 pending——那是防并发派发的闸门。
    assert!(next_prompt_dispatch_pending(
        &DaemonPhase::Idle,
        None,
        true,
        None
    ));
}

#[test]
fn failed_idle_releases_stuck_dispatch_pending() {
    assert!(!next_prompt_dispatch_pending(
        &DaemonPhase::Idle,
        None,
        true,
        Some(AcpTurnOutcome::Failed),
    ));
}

#[test]
fn running_snapshot_confirms_dispatch() {
    assert!(!next_prompt_dispatch_pending(
        &DaemonPhase::Thinking,
        Some(1),
        true,
        None
    ));
}

#[test]
fn dismissed_notice_stays_hidden_until_a_new_one_arrives() {
    use smelt_plugin_api::model::acp_session::AcpNoticeView;
    let first = AcpNoticeView {
        id: 10,
        kind: Default::default(),
        text: "Couldn't compact the context: busy".into(),
        note: None,
    };
    assert_eq!(notice_to_show(Some(&first), None), Some(&first));
    assert_eq!(notice_to_show(Some(&first), Some(10)), None);
    let second = AcpNoticeView {
        id: 11,
        kind: Default::default(),
        text: "Pi extension: boom".into(),
        note: None,
    };
    assert_eq!(notice_to_show(Some(&second), Some(10)), Some(&second));
    assert_eq!(notice_to_show(None, None), None);
}

#[test]
fn rejected_submission_restores_without_erasing_new_draft_text() {
    assert_eq!(merge_rejected_prompt("", "原消息"), "原消息");
    assert_eq!(
        merge_rejected_prompt("发送期间新输入", "原消息"),
        "原消息\n\n发送期间新输入"
    );
}

#[test]
fn unknown_plugin_submission_reuses_its_id_only_for_the_unchanged_draft() {
    let previous = smelt_plugin_api::model::conversation::ConversationInput {
        submission_id: "stable-submission".into(),
        text: "继续处理".into(),
        images: Vec::new(),
    };

    let retry = conversation_input_for_submit("继续处理".into(), Vec::new(), Some(&previous));
    assert_eq!(retry.submission_id, "stable-submission");

    let edited =
        conversation_input_for_submit("换一种方式处理".into(), Vec::new(), Some(&previous));
    assert_ne!(edited.submission_id, "stable-submission");
}

#[test]
fn stale_snapshot_cannot_reopen_the_prompt_dispatch_gate() {
    assert!(should_apply_snapshot_revision(0, 1));
    assert!(should_apply_snapshot_revision(1, 2));
    assert!(!should_apply_snapshot_revision(2, 2));
    assert!(!should_apply_snapshot_revision(2, 1));
    // 旧 daemon 和本地 socket 断开都使用 revision 0；当前连接的兜底终态
    // 必须仍能显示，旧连接则由 stream generation 在 attach_handle 处过滤。
    assert!(should_apply_snapshot_revision(2, 0));
}

#[test]
fn starting_or_ended_empty_snapshot_keeps_history_identity() {
    assert!(!should_clear_history_session_id_after_snapshot(
        &DaemonPhase::Connecting,
        true,
        0,
        false,
        2,
        false,
    ));
    assert!(!should_clear_history_session_id_after_snapshot(
        &DaemonPhase::Dead,
        true,
        0,
        false,
        3,
        false,
    ));
    assert!(should_clear_history_session_id_after_snapshot(
        &DaemonPhase::Idle,
        true,
        0,
        false,
        4,
        true,
    ));
    assert!(!should_clear_history_session_id_after_snapshot(
        &DaemonPhase::Idle,
        false,
        4,
        false,
        4,
        true,
    ));
    assert!(
        !should_clear_history_session_id_after_snapshot(
            &DaemonPhase::Idle,
            true,
            660,
            false,
            4,
            true,
        ),
        "这一帧没装正文，不代表对话是空的"
    );
    assert!(
        !should_clear_history_session_id_after_snapshot(&DaemonPhase::Idle, true, 0, true, 4, true,),
        "实时帧省略历史时不能丢掉恢复身份"
    );
}

#[test]
fn abandoned_history_clears_the_view_cache_unless_a_new_id_is_bound() {
    assert!(matches!(
        view_history_identity(None, true, false),
        ViewHistoryIdentity::Clear
    ));
    assert!(matches!(
        view_history_identity(Some("  "), true, false),
        ViewHistoryIdentity::Clear
    ));
    assert!(matches!(
        view_history_identity(Some("pi-next"), true, true),
        ViewHistoryIdentity::Set(id) if id == "pi-next"
    ));
    assert!(matches!(
        view_history_identity(None, false, false),
        ViewHistoryIdentity::Keep
    ));
}

#[test]
fn snapshot_tail_uses_global_offsets_for_live_updates() {
    let mut entries = Vec::new();
    let mut loaded_offset = 0;
    let mut entries_total = 0;

    assert_eq!(
        merge_snapshot_entries(
            &mut entries,
            &mut loaded_offset,
            &mut entries_total,
            900,
            1_000,
            vec![AcpEntry::User("900".into()), AcpEntry::User("901".into())],
            true,
        ),
        Some(0)
    );
    assert_eq!(loaded_offset, 900);
    assert_eq!(entries_total, 1_000);

    assert_eq!(
        merge_snapshot_entries(
            &mut entries,
            &mut loaded_offset,
            &mut entries_total,
            902,
            904,
            vec![AcpEntry::User("902".into()), AcpEntry::User("903".into())],
            false,
        ),
        Some(2)
    );
    assert_eq!(loaded_offset, 900);
    assert_eq!(entries_total, 904);
    assert_eq!(entries.len(), 4);
    assert!(matches!(&entries[3], AcpEntry::User(text) if text == "903"));
}

#[test]
fn overlapping_snapshot_keeps_the_contiguous_loaded_suffix() {
    let mut entries = vec![AcpEntry::User("2".into()), AcpEntry::User("3".into())];
    let mut loaded_offset = 2;
    let mut entries_total = 4;

    assert_eq!(
        merge_snapshot_entries(
            &mut entries,
            &mut loaded_offset,
            &mut entries_total,
            1,
            4,
            vec![AcpEntry::User("1".into()), AcpEntry::User("2".into())],
            false,
        ),
        Some(0)
    );
    let texts = entries
        .iter()
        .map(|entry| match entry {
            AcpEntry::User(text) => text.as_str(),
            _ => panic!("expected user entry"),
        })
        .collect::<Vec<_>>();
    assert_eq!(loaded_offset, 1);
    assert_eq!(texts, vec!["1", "2", "3"]);
}

#[test]
fn snapshot_gap_resets_to_the_newest_contiguous_suffix() {
    let mut entries = vec![AcpEntry::User("10".into()), AcpEntry::User("11".into())];
    let mut loaded_offset = 10;
    let mut entries_total = 12;

    assert_eq!(
        merge_snapshot_entries(
            &mut entries,
            &mut loaded_offset,
            &mut entries_total,
            14,
            15,
            vec![AcpEntry::User("14".into())],
            false,
        ),
        Some(0)
    );
    assert_eq!(loaded_offset, 14);
    assert_eq!(entries_total, 15);
    assert!(matches!(&entries[0], AcpEntry::User(text) if text == "14"));
}

#[test]
fn empty_gap_snapshot_keeps_the_loaded_conversation() {
    let mut entries = vec![AcpEntry::User("10".into()), AcpEntry::User("11".into())];
    let mut loaded_offset = 10;
    let mut entries_total = 12;

    assert_eq!(
        merge_snapshot_entries(
            &mut entries,
            &mut loaded_offset,
            &mut entries_total,
            14,
            14,
            Vec::new(),
            false,
        ),
        None
    );
    assert_eq!(loaded_offset, 10);
    assert_eq!(entries.len(), 2);
    assert!(matches!(&entries[0], AcpEntry::User(text) if text == "10"));
}

#[test]
fn empty_history_still_replaces_the_loaded_conversation() {
    let mut entries = vec![AcpEntry::User("10".into())];
    let mut loaded_offset = 10;
    let mut entries_total = 11;

    assert_eq!(
        merge_snapshot_entries(
            &mut entries,
            &mut loaded_offset,
            &mut entries_total,
            0,
            0,
            Vec::new(),
            false,
        ),
        Some(0)
    );
    assert!(entries.is_empty());
    assert_eq!(loaded_offset, 0);
    assert_eq!(entries_total, 0);
}

#[test]
fn state_only_snapshot_does_not_rebuild_loaded_entries() {
    let mut entries = vec![AcpEntry::User("10".into()), AcpEntry::User("11".into())];
    let mut loaded_offset = 10;
    let mut entries_total = 12;

    assert_eq!(
        merge_snapshot_entries(
            &mut entries,
            &mut loaded_offset,
            &mut entries_total,
            12,
            12,
            Vec::new(),
            false,
        ),
        None
    );
    assert_eq!(loaded_offset, 10);
    assert_eq!(entries.len(), 2);
}

/// 交给 Markdown 的正文就是消息原文：链接不在原文里改写（代码块里的链接、复制出来的
/// 内容都不该变），HTML 也不在原文里加反斜杠（交给渲染层按原文显示）。
#[test]
fn markdown_cache_keeps_message_text_verbatim() {
    let user = "见 [文件](src/index.html) 和 <panel>";
    let answer =
        "改 [入口](src/main.rs:3)：\n\n```markdown\n见 [设计](docs/a.md)\n```\n\n行内 `[x](y.md)`";
    let entries = vec![
        AcpEntry::User(user.into()),
        AcpEntry::Assistant {
            text: answer.into(),
            thought: false,
        },
    ];
    let cache = build_markdown_cache(&entries);
    assert_eq!(cache[0].as_deref(), Some(user));
    assert_eq!(cache[1].as_deref(), Some(answer));
}

fn long_tool(id: &str) -> AcpEntry {
    AcpEntry::ToolCall {
        id: id.to_string(),
        title: String::new(),
        kind: ToolKind::Other,
        status: ToolCallStatus::Completed,
        output: Vec::new(),
        children: Vec::new(),
        subagents: Vec::new(),
        peer: None,
        asked: None,
    }
}

#[test]
fn owner_id_cache_reuses_stable_tool_id() {
    let stable = "tool-call-id-longer-than-inline";
    let added = "another-tool-call-id-longer-than";
    let mut entries = vec![long_tool(stable)];
    let mut cache = OwnerIdCache::default();
    cache.refresh(&entries, 0);
    let first = cache.lookup.get(stable).unwrap().clone();

    entries[0] = long_tool(stable);
    cache.refresh(&entries, 0);
    assert_eq!(cache.lookup.get(stable).unwrap().as_ptr(), first.as_ptr());

    entries.push(long_tool(added));
    cache.refresh(&entries, 1);
    assert_eq!(cache.lookup.get(stable).unwrap().as_ptr(), first.as_ptr());
    assert!(cache.lookup.contains(added));
}

#[test]
fn markdown_cache_reuses_prefix_and_refreshes_changed_tail() {
    let mut entries = vec![
        AcpEntry::User("见 [旧文件](old.rs) 和 <panel>".into()),
        AcpEntry::Assistant {
            text: "查看 [旧回答](answer.rs)".into(),
            thought: false,
        },
    ];
    let mut cache = build_markdown_cache(&entries);
    let unchanged_prefix = cache[0].clone();

    entries.truncate(1);
    entries.push(AcpEntry::Assistant {
        text: "查看 [新回答](new.rs)".into(),
        thought: false,
    });
    entries.push(AcpEntry::Divider("next".into()));
    refresh_markdown_cache(&entries, 1, &mut cache);

    assert_eq!(cache.len(), entries.len());
    assert_eq!(cache[0], unchanged_prefix);
    // 前缀没重建：还是同一份分配，`retained_markdown` 靠指针认出没变。
    assert_eq!(
        cache[0].as_ref().unwrap().as_ptr(),
        unchanged_prefix.as_ref().unwrap().as_ptr()
    );
    assert_eq!(cache[1].as_deref(), Some("查看 [新回答](new.rs)"));
    assert!(cache[2].is_none());
}

#[test]
fn final_answer_is_the_last_body_in_each_user_turn() {
    let entries = vec![
        AcpEntry::User("修一下".into()),
        AcpEntry::Assistant {
            text: "先检查".into(),
            thought: false,
        },
        AcpEntry::ToolCall {
            id: "read-1".into(),
            title: "Read file".into(),
            kind: ToolKind::Read,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::Assistant {
            text: "已修复".into(),
            thought: false,
        },
        AcpEntry::User("再看看".into()),
        AcpEntry::Assistant {
            text: "没问题".into(),
            thought: false,
        },
    ];
    let layout = build_conversation_layout(&entries, false);
    assert!(!layout[1].final_answer);
    assert!(layout[3].final_answer);
    assert!(layout[5].final_answer);

    assert!(layout[1].process_group.is_none(), "中间正文也应可见");
    let group = layout[2].process_group.expect("工具应进入执行过程组");
    assert_eq!((group.first, group.end), (2, 3));
    assert!(layout[3].process_group.is_none());
    assert!(layout[5].process_group.is_none());
}

#[test]
fn assistant_bodies_split_process_groups_in_active_and_completed_turns() {
    let entries = vec![
        AcpEntry::User("检查并修复".into()),
        AcpEntry::Assistant {
            text: "先检查调用方。".into(),
            thought: false,
        },
        thought("先读实现"),
        completed_tool("read-1", ToolKind::Read, "a.rs"),
        AcpEntry::Assistant {
            text: "发现问题，接下来修复。".into(),
            thought: false,
        },
        thought("修改后再验证"),
        completed_tool("read-2", ToolKind::Read, "b.rs"),
        AcpEntry::Assistant {
            text: "修复完成。".into(),
            thought: false,
        },
    ];

    for active in [true, false] {
        let layout = build_conversation_layout(&entries, active);
        for ix in [1, 4, 7] {
            assert!(
                layout[ix].process_group.is_none(),
                "正文 {ix} 必须可见，active={active}"
            );
        }
        assert!(!layout[1].final_answer);
        assert!(!layout[4].final_answer);
        assert_eq!(layout[7].final_answer, !active);
        for (first, end) in [(2, 4), (5, 7)] {
            let group = layout[first].process_group.expect("思考和工具应归入过程组");
            assert_eq!((group.first, group.end), (first, end));
            assert_eq!(layout[first + 1].process_group, Some(group));
        }
    }
}

fn background_wakeup_entries() -> Vec<AcpEntry> {
    vec![
        AcpEntry::User("在后台运行检查".into()),
        completed_tool("start", ToolKind::Execute, "start check"),
        AcpEntry::Assistant {
            text: "检查已在后台启动。".into(),
            thought: false,
        },
        AcpEntry::TaskNote(TaskNote {
            kind: Default::default(),
            id: "bg-1".into(),
            title: "检查".into(),
            status: "completed".into(),
            exit_code: Some(0),
            output_tail: "检查通过".into(),
            runs: Vec::new(),
            peer: None,
        }),
        AcpEntry::Assistant {
            text: "后台检查完成，我再核对结果。".into(),
            thought: false,
        },
        thought("核对输出"),
        completed_tool("read", ToolKind::Read, "result.log"),
        AcpEntry::Assistant {
            text: "结果正常。".into(),
            thought: false,
        },
        completed_tool("late", ToolKind::Read, "late.log"),
    ]
}

#[test]
fn background_wakeup_keeps_all_bodies_and_the_task_note_visible() {
    let entries = background_wakeup_entries();
    for active in [true, false] {
        let layout = build_conversation_layout(&entries, active);
        for ix in [2, 3, 4, 7] {
            assert!(
                layout[ix].process_group.is_none(),
                "正文或状态行 {ix} 必须可见，active={active}"
            );
        }
        assert!(!layout[2].final_answer);
        assert!(!layout[4].final_answer);
        assert_eq!(layout[7].final_answer, !active);
        for (first, end) in [(1, 2), (5, 7), (8, 9)] {
            let group = layout[first].process_group.expect("工具应归入过程组");
            assert_eq!((group.first, group.end), (first, end));
            for ix in first..end {
                assert_eq!(layout[ix].process_group, Some(group));
            }
        }
    }
}

#[test]
fn incremental_open_turn_layout_matches_full_rebuild() {
    let mut entries = vec![
        AcpEntry::User("先做".into()),
        AcpEntry::ToolCall {
            id: "read-1".into(),
            title: "Read file".into(),
            kind: ToolKind::Read,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::Assistant {
            text: "好".into(),
            thought: false,
        },
        AcpEntry::User("继续".into()),
        AcpEntry::Assistant {
            text: "正在".into(),
            thought: false,
        },
    ];
    let mut layout = build_conversation_layout(&entries, true);
    entries.push(AcpEntry::ToolCall {
        id: "read-2".into(),
        title: "Read more".into(),
        kind: ToolKind::Read,
        status: ToolCallStatus::InProgress,
        output: Vec::new(),
        children: Vec::new(),
        subagents: Vec::new(),
        peer: None,
        asked: None,
    });
    let from = super::layout_recompute_start(&entries, entries.len() - 1);
    assert!(from >= 3, "已结束回合不应重算，起点={from}");
    super::refresh_conversation_layout_from(&mut layout, &entries, from, true, 0, &[]);
    assert_eq!(layout, build_conversation_layout(&entries, true));
    assert!(
        layout[4].process_group.is_none(),
        "增量工具不能吞掉已有正文"
    );
    let group = layout[5].process_group.expect("新工具应归入过程组");
    assert_eq!((group.first, group.end), (5, 6));
}

#[test]
fn active_turn_has_no_provisional_final_answer() {
    let entries = vec![
        AcpEntry::User("修一下".into()),
        AcpEntry::Assistant {
            text: "正在检查".into(),
            thought: false,
        },
    ];

    let layout = build_conversation_layout(&entries, true);

    assert!(!layout[1].final_answer);
    assert!(
        layout[1].process_group.is_none(),
        "没有工具时，流式正文不是执行记录"
    );
}

#[test]
fn thought_only_turn_is_not_an_execution_record() {
    let entries = vec![
        AcpEntry::User("你好".into()),
        AcpEntry::Assistant {
            text: "用户只是打了个招呼".into(),
            thought: true,
        },
        AcpEntry::Assistant {
            text: "你好！有什么可以帮你的吗？".into(),
            thought: false,
        },
    ];
    let layout = build_conversation_layout(&entries, false);
    assert!(layout[2].final_answer);
    assert!(
        layout[1].process_group.is_none(),
        "纯思考不应包成「执行记录 · 1 步」"
    );
    assert!(layout[2].process_group.is_none());
}

#[test]
fn live_thoughts_stay_out_of_process_groups() {
    let entries = vec![
        AcpEntry::User("你会啥技能".into()),
        AcpEntry::Assistant {
            text: "The user is asking what skills I have. I should look at the available sk…"
                .into(),
            thought: true,
        },
        AcpEntry::Assistant {
            text: "Next I'll list the tools.".into(),
            thought: true,
        },
    ];
    let layout = build_conversation_layout(&entries, true);
    assert!(layout[1].process_group.is_none());
    assert!(layout[2].process_group.is_none());
    assert!(!layout[1].final_answer);
}

#[test]
fn edit_resend_targets_only_the_latest_real_user_message() {
    let entries = vec![
        AcpEntry::User("第一条".into()),
        AcpEntry::Assistant {
            text: "回答一".into(),
            thought: false,
        },
        AcpEntry::User("最后一条".into()),
        AcpEntry::Assistant {
            text: "回答二".into(),
            thought: false,
        },
        AcpEntry::User("[Request interrupted by user]".into()),
    ];

    assert_eq!(latest_user_message_index(&entries), Some(2));
    assert_eq!(latest_user_message_index(&[]), None);
}

#[test]
fn edit_resend_recognizes_a_latest_image_message() {
    let entries = vec![
        AcpEntry::User("上一条".into()),
        AcpEntry::UserWithImages {
            text: "看这张图".into(),
            images: Vec::new(),
        },
    ];

    assert_eq!(latest_user_message_index(&entries), Some(1));
}

#[test]
fn completed_turn_folds_its_process_into_one_summary() {
    let entries = vec![
        AcpEntry::User("看看今天的新闻".into()),
        completed_tool("s1", ToolKind::Search, "today news"),
        AcpEntry::Assistant {
            text: "再看看国际和国内".into(),
            thought: false,
        },
        completed_tool("s2", ToolKind::Search, "international"),
        completed_tool("s3", ToolKind::Search, "china"),
        AcpEntry::Assistant {
            text: "今天是星期二".into(),
            thought: false,
        },
    ];
    let timings = [TurnTiming {
        user_index: 0,
        started_at_ms: 1_000,
        ended_at_ms: Some(5_000),
    }];
    let layout = build_conversation_layout_with_timings(&entries, false, 0, &timings);
    let fold = layout[1].turn_fold.expect("跑完的一轮收成摘要");
    assert_eq!((fold.first, fold.end), (1, 5));
    for ix in 1..5 {
        assert_eq!(layout[ix].turn_fold, Some(fold));
    }
    assert!(layout[0].turn_fold.is_none());
    assert!(layout[5].turn_fold.is_none(), "回答留在摘要外面");
    assert_eq!(fold.elapsed_ms, Some(4_000));
    assert_eq!(
        super::turn_fold_label(fold),
        "Worked for 4s · Searched 3 times"
    );
    let group = layout[1].process_group.expect("展开后仍按段分");
    assert!(!group.show_elapsed, "耗时只在摘要上写一次");

    let live = build_conversation_layout_with_timings(&entries, true, 0, &[]);
    assert!(live[1].turn_fold.is_none(), "在跑的回合不收成摘要");
    let live_group = live[1].process_group.expect("进行中也应收成一段");
    assert!(live_group.active);
    assert_eq!(
        live_group.elapsed_ms, None,
        "进行中的耗时只在末尾的「现在」里"
    );
}

/// 回答之前只有一段、段外没有别的可见行：这一段自己就是那一行，不再套一层回合摘要，
/// 否则展开摘要后紧跟着又是一行同样的计数。回合耗时改挂在这一段上。
#[test]
fn a_turn_with_one_segment_is_not_wrapped_in_a_summary() {
    let entries = vec![
        AcpEntry::User("看看今天的新闻".into()),
        thought("先搜一下"),
        completed_tool("s1", ToolKind::Search, "today news"),
        completed_tool("s2", ToolKind::Search, "international"),
        AcpEntry::Assistant {
            text: "今天是星期二".into(),
            thought: false,
        },
    ];
    let timings = [TurnTiming {
        user_index: 0,
        started_at_ms: 1_000,
        ended_at_ms: Some(5_000),
    }];
    let layout = build_conversation_layout_with_timings(&entries, false, 0, &timings);
    assert!(layout.iter().all(|row| row.turn_fold.is_none()));
    let group = layout[1].process_group.expect("思考和工具是一段");
    assert_eq!((group.first, group.end), (1, 4));
    assert!(group.show_elapsed);
    assert_eq!(
        process_group_header_label(group, false),
        "Worked for 4s · Searched 2 times"
    );
    assert!(layout[4].final_answer);
}

#[test]
fn turn_summary_names_edits_commands_and_subagents() {
    let edit = |id: &str, path: &str, added: &str| {
        AcpEntry::tool_call(
            id,
            "Edit",
            ToolKind::Edit,
            ToolCallStatus::Completed,
            vec![ToolOutputPart::Diff {
                path: path.into(),
                old_text: Some(String::new()),
                new_text: added.into(),
            }],
        )
    };
    let entries = vec![
        AcpEntry::User("修一下".into()),
        completed_tool("r", ToolKind::Read, "a.rs"),
        edit("e1", "a.rs", "x"),
        edit("e2", "a.rs", "y"),
        edit("e3", "b.rs", "z"),
        AcpEntry::Assistant {
            text: "先跑测试。".into(),
            thought: false,
        },
        completed_tool("t", ToolKind::Execute, "cargo test"),
        AcpEntry::ToolCall {
            id: "agent".into(),
            title: "Explore".into(),
            kind: ToolKind::Collaborate,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::Assistant {
            text: "修好了。".into(),
            thought: false,
        },
    ];
    let layout = build_conversation_layout(&entries, false);
    let fold = layout[1].turn_fold.expect("跑完的一轮收成摘要");
    assert_eq!((fold.first, fold.end), (1, 8));
    assert!(layout[5].turn_fold.is_some(), "过程说明也收进摘要");
    assert_eq!(
        super::turn_fold_label(fold),
        "Edited 2 files · Ran 1 command · 1 subagent",
        "同一文件改几次只算一个；没有计时就不写耗时"
    );
}

#[test]
fn edit_chips_sum_each_file_in_order_and_skip_failures() {
    let diff_tool = |id: &str, status, paths: &[&str]| {
        AcpEntry::tool_call(
            id,
            "Edit",
            ToolKind::Edit,
            status,
            paths
                .iter()
                .map(|path| ToolOutputPart::Diff {
                    path: (*path).into(),
                    old_text: Some("a".into()),
                    new_text: "b".into(),
                })
                .collect(),
        )
    };
    let entries = vec![
        AcpEntry::User("改".into()),
        diff_tool("e1", ToolCallStatus::Completed, &["src/x/mod.rs"]),
        completed_tool("r", ToolKind::Read, "a.rs"),
        diff_tool(
            "e2",
            ToolCallStatus::Completed,
            &["src/y/mod.rs", "src/x/mod.rs"],
        ),
        diff_tool("e3", ToolCallStatus::Failed, &["src/z.rs"]),
    ];
    let cached = |added, removed| Some(CachedDiff { added, removed });
    let diffs = std::collections::HashMap::from([
        ("e1".to_string(), vec![cached(3, 1)]),
        ("e2".to_string(), vec![cached(2, 0), cached(4, 2)]),
        ("e3".to_string(), vec![cached(9, 9)]),
    ]);
    let chips = super::segment_edit_chips(&entries, &diffs, 1, entries.len());
    let summary: Vec<_> = chips
        .iter()
        .map(|chip| {
            (
                chip.label.as_str(),
                chip.added,
                chip.removed,
                chip.parts.clone(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("x/mod.rs", 7, 3, vec![(1, 0), (3, 1)]),
            ("y/mod.rs", 2, 0, vec![(3, 0)]),
        ],
        "同一文件的多次改动合计、按次序记下；同名文件带上目录；失败的不算"
    );

    let layout = build_conversation_layout(&entries, true);
    let group = layout[1].process_group.unwrap();
    assert_eq!(group.tally.edited_files, 2);
    assert_eq!(group.tally.diff_parts, 3);
    assert_eq!(group.tally.failed, 1);
    assert_eq!(
        process_group_header_label(group, false),
        "Read 1 file · Edited 2 files"
    );
    assert_eq!(
        process_group_header_label(group, true),
        "Read 1 file",
        "文件露成标签时计数行不再重复"
    );
}

#[test]
fn thought_only_turn_is_not_folded() {
    let entries = vec![
        AcpEntry::User("你好".into()),
        thought("打个招呼"),
        AcpEntry::Assistant {
            text: "你好！".into(),
            thought: false,
        },
    ];
    let layout = build_conversation_layout(&entries, false);
    assert!(layout.iter().all(|row| row.turn_fold.is_none()));
}

#[test]
fn split_process_groups_show_turn_elapsed_only_once() {
    let entries = background_wakeup_entries();
    let timings = [TurnTiming {
        user_index: 0,
        started_at_ms: 1_000,
        ended_at_ms: Some(5_000),
    }];
    let completed = build_conversation_layout_with_timings(&entries, false, 0, &timings);
    let fold = completed[1].turn_fold.expect("回答之前的过程收成摘要");
    assert_eq!((fold.first, fold.end, fold.elapsed_ms), (1, 7, Some(4_000)));
    for ix in [1, 5, 8] {
        let group = completed[ix].process_group.unwrap();
        assert!(!group.show_elapsed, "耗时已经写在摘要上");
        assert_eq!(group.elapsed_ms, None);
    }
    assert!(completed[8].turn_fold.is_none(), "回答之后的工具不收进摘要");

    let live = build_conversation_layout_with_timings(&entries, true, 0, &[]);
    for ix in [1, 5] {
        let group = live[ix].process_group.unwrap();
        assert!(!group.show_elapsed);
        assert_eq!(group.elapsed_ms, None);
    }
    let last = live[8].process_group.unwrap();
    assert!(last.show_elapsed, "最后一段是正在跑的那段");
    assert_eq!(last.elapsed_ms, None, "进行中的耗时只在末尾的「现在」里");
}

#[test]
fn a_trailing_thought_ends_the_running_segment() {
    let entries = vec![
        AcpEntry::User("检查".into()),
        completed_tool("read", ToolKind::Read, "a.rs"),
        AcpEntry::Assistant {
            text: "继续分析。".into(),
            thought: false,
        },
        thought("下一步"),
        AcpEntry::Assistant {
            text: " ".into(),
            thought: false,
        },
    ];
    let layout = build_conversation_layout_with_timings(&entries, true, 0, &[]);
    let group = layout[1].process_group.unwrap();
    assert!(!group.show_elapsed);
    assert!(layout[3].process_group.is_none());
    assert!(!layout[3].show_settled_thought_elapsed);
}

#[test]
fn standalone_thought_does_not_duplicate_completed_process_elapsed() {
    let entries = vec![
        AcpEntry::User("检查".into()),
        thought("先想一想"),
        AcpEntry::Assistant {
            text: "开始检查。".into(),
            thought: false,
        },
        completed_tool("read", ToolKind::Read, "a.rs"),
        AcpEntry::Assistant {
            text: "完成。".into(),
            thought: false,
        },
    ];
    let layout = build_conversation_layout(&entries, false);
    assert!(layout[1].process_group.is_none());
    assert!(!layout[1].show_settled_thought_elapsed);
    assert!(layout[1].turn_fold.is_some(), "思考和过程一起收进摘要");
    assert!(!layout[3].process_group.unwrap().show_elapsed);
}

#[test]
fn empty_body_neither_steals_final_answer_nor_splits_a_segment() {
    let entries = vec![
        AcpEntry::User("检查".into()),
        completed_tool("one", ToolKind::Read, "a.rs"),
        AcpEntry::Assistant {
            text: " ".into(),
            thought: false,
        },
        completed_tool("two", ToolKind::Read, "b.rs"),
        AcpEntry::Assistant {
            text: "结果".into(),
            thought: false,
        },
        AcpEntry::Assistant {
            text: " ".into(),
            thought: false,
        },
    ];
    let layout = build_conversation_layout(&entries, false);
    assert!(layout[4].final_answer);
    assert!(!layout[5].final_answer);
    let group = layout[1].process_group.unwrap();
    assert_eq!(layout[3].process_group, Some(group));
}

#[test]
fn unknown_tools_count_as_other_tools_not_an_english_kind() {
    let entries = vec![
        AcpEntry::User("做点事".into()),
        completed_tool("a", ToolKind::Other, "mcp__x__y"),
        completed_tool("b", ToolKind::Other, "mcp__x__z"),
        completed_tool("c", ToolKind::Execute, "ls"),
    ];
    let layout = build_conversation_layout(&entries, true);
    let group = layout[1].process_group.unwrap();
    assert_eq!(
        process_group_header_label(group, false),
        "Ran 1 command · 2 other tool calls"
    );
    assert_eq!(compact_tool_headline(ToolKind::Other, " "), "Tool");
}

#[test]
fn now_names_the_running_tool_in_the_present_tense() {
    let running = |kind: ToolKind, title: &str| AcpEntry::ToolCall {
        id: "now-1".into(),
        title: title.into(),
        kind,
        status: ToolCallStatus::InProgress,
        output: Vec::new(),
        children: Vec::new(),
        subagents: Vec::new(),
        peer: None,
        asked: None,
    };
    let entries = vec![
        AcpEntry::User("跑测试".into()),
        completed_tool("read", ToolKind::Read, "src/lib.rs"),
        running(ToolKind::Execute, "cargo test\n--all"),
    ];
    let state = super::now_state(&entries, &[], None, |_| true);
    assert_eq!(
        state.activity,
        super::NowActivity::Tool(ToolKind::Execute, "cargo test\n--all".into())
    );
    assert_eq!(super::now_activity_label(&state), "Running cargo test");
    assert_eq!(state.thought_excerpt, None, "在跑工具时不露思考摘要");
    assert_eq!(
        super::now_tool_label(ToolKind::Read, "crates/a/Cargo.toml"),
        "Reading Cargo.toml"
    );
    assert_eq!(
        super::now_tool_label(ToolKind::Other, " "),
        "Running a tool"
    );

    let waiting = super::now_state(&entries, &[], Some(super::UserWait::Approval), |_| true);
    assert_eq!(
        waiting.activity,
        super::NowActivity::Waiting(super::UserWait::Approval)
    );
    assert_eq!(
        super::now_activity_label(&waiting),
        "Waiting for you to confirm"
    );

    let previous_turn = vec![
        AcpEntry::User("上一轮".into()),
        running(ToolKind::Execute, "sleep 100"),
        AcpEntry::User("这一轮".into()),
    ];
    assert_eq!(
        super::now_state(&previous_turn, &[], None, |_| true).activity,
        super::NowActivity::Thinking,
        "上一轮没收尾的工具不算这一轮在做的事"
    );
}

#[test]
fn now_shows_a_thought_excerpt_only_when_the_thought_is_folded_away() {
    let entries = vec![
        AcpEntry::User("看看".into()),
        completed_tool("read", ToolKind::Read, "a.rs"),
        thought("先看调用方"),
        AcpEntry::Assistant {
            text: " ".into(),
            thought: false,
        },
    ];
    let hidden = super::now_state(&entries, &[], None, |ix| ix == 2);
    assert_eq!(hidden.activity, super::NowActivity::Thinking);
    assert_eq!(hidden.thought_excerpt.as_deref(), Some("先看调用方"));
    let visible = super::now_state(&entries, &[], None, |_| false);
    assert_eq!(visible.thought_excerpt, None, "看得见的思考不再抄一遍");

    let mut answering = entries.clone();
    answering.push(AcpEntry::Assistant {
        text: "结论是".into(),
        thought: false,
    });
    let state = super::now_state(&answering, &[], None, |_| true);
    assert_eq!(state.activity, super::NowActivity::Answering);
    assert_eq!(state.thought_excerpt, None);
}

#[test]
fn now_counts_this_turns_running_subagents() {
    let dispatch = AcpEntry::ToolCall {
        id: "call-1".into(),
        title: "审计".into(),
        kind: ToolKind::Other,
        status: ToolCallStatus::InProgress,
        output: Vec::new(),
        children: Vec::new(),
        subagents: vec![dispatched("sa-1", SubagentStatus::Running)],
        peer: None,
        asked: None,
    };
    let entries = vec![AcpEntry::User("审一下".into()), dispatch];
    let mut other_turn = subagent_view("sa-9", SubagentStatus::Running);
    other_turn.tool_call_id = "call-old".into();
    let live = [
        subagent_view("sa-1", SubagentStatus::Running),
        subagent_view("sa-2", SubagentStatus::Completed),
        other_turn,
    ];
    let state = super::now_state(&entries, &live, None, |_| true);
    assert_eq!(
        state.activity,
        super::NowActivity::Agents,
        "派发本身不算在跑的工具"
    );
    assert_eq!(super::now_activity_label(&state), "Waiting for a subagent");
    assert_eq!(state.running_agents, 1, "跑完的和上一轮派的不算");
}

#[test]
fn settled_thought_uses_the_completed_turn() {
    let entries = vec![
        AcpEntry::User("想一下".into()),
        AcpEntry::Assistant {
            text: "先看调用方".into(),
            thought: true,
        },
    ];
    let done = [TurnTiming {
        user_index: 10,
        started_at_ms: 1_000,
        ended_at_ms: Some(4_000),
    }];
    assert_eq!(settled_thought_elapsed(&entries, 1, 10, &done), Some(3_000));
    assert_eq!(settled_thought_elapsed(&entries, 1, 0, &done), None);
    assert_eq!(settled_thought_label(3_000), "Thought for 3s");

    let answer = vec![
        AcpEntry::User("想一下".into()),
        AcpEntry::Assistant {
            text: "结论".into(),
            thought: false,
        },
    ];
    assert_eq!(settled_thought_elapsed(&answer, 1, 10, &done), None);

    let live = [TurnTiming {
        user_index: 10,
        started_at_ms: 1_000,
        ended_at_ms: None,
    }];
    assert_eq!(settled_thought_elapsed(&entries, 1, 10, &live), None);
}

#[test]
fn only_a_few_freshly_generated_non_user_entries_fade_in() {
    let entries = vec![
        AcpEntry::User("改一下".into()),
        AcpEntry::Assistant {
            text: "好".into(),
            thought: true,
        },
        AcpEntry::Assistant {
            text: "改好了".into(),
            thought: false,
        },
    ];
    assert_eq!(
        entries_to_fade_in(&entries, 0..3),
        vec![1, 2],
        "用户消息有自己的气泡入场，不再叠一层淡入"
    );
    assert!(entries_to_fade_in(&entries, 3..3).is_empty());

    let replayed: Vec<AcpEntry> = (0..4)
        .map(|n| AcpEntry::Assistant {
            text: format!("第 {n} 段"),
            thought: false,
        })
        .collect();
    assert!(
        entries_to_fade_in(&replayed, 0..4).is_empty(),
        "一次冒出一大批是切回会话或补历史，不是眼看着生成的，不该一起闪"
    );
}

#[test]
fn user_bubble_enter_waits_until_the_new_message_exists() {
    assert!(user_bubble_enter_ready(Some(0), 0));
    assert!(!user_bubble_enter_ready(Some(5), 8));
    assert!(!user_bubble_enter_ready(None, 0));
    assert!(user_bubble_enter_ready(Some(8), 8));
}

#[test]
fn active_turn_wraps_tools_in_a_live_process_group() {
    let entries = vec![
        AcpEntry::User("定位实现".into()),
        AcpEntry::ToolCall {
            id: "search-1".into(),
            title: "BottomPanel".into(),
            kind: ToolKind::Search,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::ToolCall {
            id: "read-1".into(),
            title: "crates/smelt/src/main.rs".into(),
            kind: ToolKind::Read,
            status: ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
    ];

    let layout = build_conversation_layout(&entries, true);
    let group = layout[1].process_group.expect("进行中也应收成过程组");
    assert!(group.active);
    assert_eq!(layout[2].process_group.map(|item| item.first), Some(1));
}

#[test]
fn task_note_stays_outside_the_process_group() {
    let entries = vec![
        AcpEntry::User("跑一下".into()),
        AcpEntry::ToolCall {
            id: "bash-1".into(),
            title: "cargo test".into(),
            kind: ToolKind::Execute,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::TaskNote(TaskNote {
            kind: Default::default(),
            id: "bg-1".into(),
            title: "编译".into(),
            status: "failed".into(),
            exit_code: Some(1),
            output_tail: "error: boom".into(),
            runs: Vec::new(),
            peer: None,
        }),
        AcpEntry::Assistant {
            text: "编译失败了".into(),
            thought: false,
        },
    ];

    let layout = build_conversation_layout(&entries, false);
    assert!(layout[1].process_group.is_some());
    assert!(
        layout[2].process_group.is_none(),
        "后台任务状态行不收进执行过程，折叠后仍要看得见"
    );
    assert!(layout[3].final_answer);
}

/// 发给别的对话的一条是事件行：不收进执行过程，折叠后仍看得见，也不打断前后同类工具的合并判断。
#[test]
fn peer_send_stays_outside_the_process_group() {
    let entries = vec![
        AcpEntry::User("让测试那边跑一下".into()),
        AcpEntry::ToolCall {
            id: "read-1".into(),
            title: "README.md".into(),
            kind: ToolKind::Read,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::ToolCall {
            id: "send-1".into(),
            title: "session_send".into(),
            kind: ToolKind::Other,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: Some(smelt_plugin_api::model::acp_chat::PeerSend {
                to_id: "acp-b".into(),
                to_title: "测试".into(),
                kind: smelt_plugin_api::model::acp_session::PeerMessageKind::Task,
                text: "跑 e2e".into(),
                message_id: Some("pm-1".into()),
                error: None,
                spawned: false,
            }),
            asked: None,
        },
        AcpEntry::Assistant {
            text: "已经交给测试那边了".into(),
            thought: false,
        },
    ];

    let layout = build_conversation_layout(&entries, false);
    assert!(layout[1].process_group.is_some());
    assert!(
        layout[2].process_group.is_none(),
        "发给别的对话的一条不收进执行过程"
    );
    assert!(layout[3].final_answer);
}

/// 问用户的一次是问答行：不收进执行过程、不计进工具数，折叠后仍看得见问了什么、答了什么。
#[test]
fn an_answered_question_stays_outside_the_process_group() {
    let asked = |id: &str, status: ToolCallStatus| AcpEntry::ToolCall {
        id: id.into(),
        title: "smelt_ask_user".into(),
        kind: ToolKind::Other,
        status,
        output: Vec::new(),
        children: Vec::new(),
        subagents: Vec::new(),
        peer: None,
        asked: Some(smelt_plugin_api::model::acp_chat::AskRecord {
            answers: vec![smelt_plugin_api::model::acp_chat::AskAnswer {
                question: "按哪个方案做？".into(),
                answer: vec!["按推荐方案".into()],
            }],
            skipped: false,
        }),
    };
    let read = AcpEntry::ToolCall {
        id: "read-1".into(),
        title: "Read src/lib.rs".into(),
        kind: ToolKind::Read,
        status: ToolCallStatus::Completed,
        output: Vec::new(),
        children: Vec::new(),
        subagents: Vec::new(),
        peer: None,
        asked: None,
    };
    let entries = vec![
        AcpEntry::User("插件图片显示不出来".into()),
        read,
        asked("ask-1", ToolCallStatus::Completed),
        AcpEntry::Assistant {
            text: "那就按推荐方案改".into(),
            thought: false,
        },
    ];

    let layout = build_conversation_layout(&entries, false);
    assert!(layout[1].process_group.is_some());
    assert!(layout[2].process_group.is_none(), "问答不收进执行过程");
    assert!(layout[3].final_answer);
    let tally = super::tally_tools(&entries, 1..3);
    assert_eq!((tally.reads, tally.others), (1, 0), "问答不算工具");

    // 还在等答案时这一条不占行：不画，也不把前后的工具隔成两段。
    let waiting = vec![
        entries[0].clone(),
        entries[1].clone(),
        asked("ask-2", ToolCallStatus::InProgress),
        entries[1].clone(),
    ];
    assert!(super::draws_nothing(&waiting[2]));
    let layout = build_conversation_layout(&waiting, true);
    let group = |ix: usize| layout[ix].process_group.map(|group| group.first);
    assert_eq!(group(3), Some(1), "等答案的提问前后是同一段过程");
}

#[test]
fn a_message_from_another_conversation_reads_as_who_sent_what() {
    let note = TaskNote {
        kind: smelt_plugin_api::model::acp_chat::TaskNoteKind::PeerMessage,
        id: "pm-1".into(),
        title: String::new(),
        status: String::new(),
        exit_code: None,
        output_tail: "跑 e2e".into(),
        runs: Vec::new(),
        peer: Some(smelt_plugin_api::model::acp_chat::PeerNote {
            source_session_id: "acp-a".into(),
            source_title: "修登录".into(),
            kind: smelt_plugin_api::model::acp_session::PeerMessageKind::Task,
        }),
    };
    assert_eq!(note.summary(), "修登录 sent a task");
    let entries = vec![AcpEntry::User("在吗".into()), AcpEntry::TaskNote(note)];
    let layout = build_conversation_layout(&entries, false);
    assert!(layout[1].process_group.is_none());
}

#[test]
fn progress_summary_is_visible_but_long_details_stay_expandable() {
    let text = "\n**先定位终端面板入口**\n\n我会继续检查状态管理、快捷键和 session list 的调用方。";

    assert_eq!(progress_summary(text), "先定位终端面板入口");
    assert!(progress_has_details(text));
    assert!(!progress_has_details("只需一句简短进展"));
}

/// 摘要是纯文本：正文里成对的加粗、行内代码标记要剥掉，落单的照原样留着。
#[test]
fn progress_summary_strips_paired_inline_markdown() {
    assert_eq!(
        progress_summary("找到关键线索：返回**一模一样**的空正文"),
        "找到关键线索：返回一模一样的空正文"
    );
    assert_eq!(
        progress_summary("Interesting: `api.ipify.org` reports overseas"),
        "Interesting: api.ipify.org reports overseas"
    );
    // 代码里的星号是正文，不是标记。
    assert_eq!(progress_summary("看 `a**b` 和 **重点**"), "看 a**b 和 重点");
    // 落单的标记不动。
    assert_eq!(progress_summary("算 2**10 的值"), "算 2**10 的值");
    assert_eq!(progress_summary("半个 `反引号"), "半个 `反引号");
}

/// 摘要按剥掉标记后的字数截断，是否有详情也按同一把尺子量。
#[test]
fn progress_details_are_measured_after_stripping_markers() {
    let exactly_72 = "字".repeat(72);
    let marked = format!("**{}**{}", "字".repeat(10), "字".repeat(62));
    assert_eq!(progress_summary(&marked), exactly_72);
    assert!(
        !progress_has_details(&marked),
        "剥掉标记后正好 72 字，没有可展开的内容"
    );

    let long = format!("开头{}结尾", "很长".repeat(40));
    let summary = progress_summary(&long);
    assert!(summary.ends_with('…'));
    assert_eq!(summary.chars().count(), 73);
    assert!(progress_has_details(&long));
}

#[test]
fn plan_summary_surfaces_the_current_step_in_collapsed_mode() {
    let plan = PlanView {
        entries: vec![
            PlanEntryView {
                content: "定位相关代码".into(),
                status: PlanEntryStatusView::Completed,
            },
            PlanEntryView {
                content: "重构活动时间线".into(),
                status: PlanEntryStatusView::InProgress,
            },
            PlanEntryView {
                content: "运行回归测试".into(),
                status: PlanEntryStatusView::Pending,
            },
        ],
    };

    assert_eq!(plan_current_step(&plan), Some("重构活动时间线"));
}

#[test]
fn collaboration_tools_join_their_segment_as_rows() {
    let entries = vec![
        AcpEntry::User("并行检查".into()),
        AcpEntry::ToolCall {
            id: "agent-1".into(),
            title: "Start subagent explorer".into(),
            kind: ToolKind::Collaborate,
            status: ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::ToolCall {
            id: "read-1".into(),
            title: "Read source".into(),
            kind: ToolKind::Read,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
    ];

    let live = build_conversation_layout(&entries, true);
    let group = live[1].process_group.expect("子智能体和前后的工具同在一段");
    assert_eq!(live[2].process_group, Some(group));
    assert!(group.active);
    assert_eq!(group.tally.agents, 1);
    assert_eq!(
        process_group_header_label(group, false),
        "Read 1 file",
        "子智能体在计数行下面逐个列出，计数里不再重复"
    );

    let alone = build_conversation_layout(&entries[..2], true);
    let group = alone[1].process_group.expect("只派了子智能体也是一段");
    assert_eq!(
        process_group_header_label(group, false),
        "1 subagent",
        "只剩子智能体时拿它当标题"
    );
}

#[test]
fn completed_process_group_keeps_inner_tool_failures_inside() {
    let entries = vec![
        AcpEntry::User("跑测试".into()),
        AcpEntry::ToolCall {
            id: "test-1".into(),
            title: "cargo test".into(),
            kind: ToolKind::Execute,
            status: ToolCallStatus::Failed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::ToolCall {
            id: "test-2".into(),
            title: "cargo test retry".into(),
            kind: ToolKind::Execute,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::Assistant {
            text: "测过了，已经修好".into(),
            thought: false,
        },
    ];

    let layout = build_conversation_layout(&entries, false);
    let group = layout[1].process_group.expect("工具应进入执行过程组");
    assert!(!group.active);
    assert_eq!(process_group_header_label(group, false), "Ran 2 commands");
    assert_eq!(
        super::tally_failed_label(&group.tally).as_deref(),
        Some("1 failed"),
        "失败数单独给出，界面上标红"
    );
}

#[test]
fn previous_turn_subagent_does_not_join_the_next_process_group() {
    let entries = vec![
        AcpEntry::User("先检查旧问题".into()),
        AcpEntry::ToolCall {
            id: "old-agent".into(),
            title: "Explore old problem".into(),
            kind: ToolKind::Collaborate,
            // 历史快照可能缺少最后一次状态更新；子代理仍是旧回合的独立卡片。
            status: ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::Assistant {
            text: "旧问题已完成".into(),
            thought: false,
        },
        AcpEntry::User("开始新问题".into()),
        AcpEntry::Assistant {
            text: "正在分析".into(),
            thought: true,
        },
    ];

    let layout = build_conversation_layout(&entries, true);
    let old = layout[1]
        .process_group
        .expect("上一回合的子智能体在它那一回合的段里");
    assert!(!old.active, "上一回合的段不算在跑");
    assert_eq!(old.end, 2, "上一回合的子代理不能并进当前执行过程");
    assert!(layout[4].process_group.is_none());
}

#[test]
fn idle_last_turn_keeps_background_agent_working() {
    let child = AcpEntry::tool_call(
        "read-1",
        "Read README",
        ToolKind::Read,
        ToolCallStatus::InProgress,
        Vec::new(),
    );
    let mut parent = AcpEntry::tool_call(
        "agent-1",
        "Task",
        ToolKind::Collaborate,
        ToolCallStatus::Completed,
        Vec::new(),
    );
    let AcpEntry::ToolCall { children, .. } = &mut parent else {
        panic!("parent");
    };
    children.push(child);

    let entries = vec![
        AcpEntry::User("explore".into()),
        parent,
        AcpEntry::Assistant {
            text: "后台还在看".into(),
            thought: false,
        },
    ];
    let layout = build_conversation_layout(&entries, false);
    assert!(
        layout[1].turn_fold.is_none(),
        "后台子智能体还在跑，这一轮不收成摘要"
    );
    let group = layout[1].process_group.expect("子智能体在它那一段里");
    assert_eq!(group.tally.agents, 1);
}

#[test]
fn task_complete_is_final_and_not_counted_as_a_process_tool() {
    let entries = vec![
        AcpEntry::User("修一下".into()),
        AcpEntry::ToolCall {
            id: "edit".into(),
            title: "Edit file".into(),
            kind: ToolKind::Edit,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::Assistant {
            text: "已经修好".into(),
            thought: false,
        },
        AcpEntry::ToolCall {
            id: "done".into(),
            title: "task_complete".into(),
            kind: ToolKind::Other,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
    ];

    let layout = build_conversation_layout(&entries, false);

    assert!(layout[3].final_answer);
    let before = layout[1].process_group.expect("编辑应在过程组");
    assert_eq!(before.end, 2);
    assert!(
        layout[2].process_group.is_none(),
        "完成操作前的说明仍是正文"
    );
    assert!(!layout[2].final_answer);
    assert!(layout[3].process_group.is_none());
}

#[test]
fn task_complete_is_final_when_no_assistant_answer_exists() {
    let entries = vec![
        AcpEntry::User("检查状态".into()),
        AcpEntry::ToolCall {
            id: "done".into(),
            title: "task_complete".into(),
            kind: ToolKind::Other,
            status: ToolCallStatus::Completed,
            output: vec![ToolOutputPart::Text("全部完成".into())],
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
    ];

    let layout = build_conversation_layout(&entries, false);

    assert!(layout[1].final_answer);
    assert!(layout[1].process_group.is_none());
}

#[test]
fn tool_cards_start_collapsed() {
    assert!(!tool_card_default_expanded());
}

#[test]
fn compact_process_rows_cover_completed_and_failed_tools() {
    assert!(tool_uses_compact_process_row(
        ToolCallStatus::Completed,
        false
    ));
    assert!(tool_uses_compact_process_row(ToolCallStatus::Failed, false));
    assert!(!tool_uses_compact_process_row(
        ToolCallStatus::InProgress,
        false
    ));
    assert!(!tool_uses_compact_process_row(
        ToolCallStatus::Completed,
        true
    ));
    assert!(!tool_uses_compact_process_row(ToolCallStatus::Failed, true));
}

fn completed_tool(id: &str, kind: ToolKind, title: &str) -> AcpEntry {
    AcpEntry::ToolCall {
        id: id.into(),
        title: title.into(),
        kind,
        status: ToolCallStatus::Completed,
        output: Vec::new(),
        children: Vec::new(),
        subagents: Vec::new(),
        peer: None,
        asked: None,
    }
}

#[test]
fn runtime_debug_contexts_show_request_config_and_multiple_turn_linked_calls() {
    let contexts = runtime_debug_contexts(&RuntimeDebug {
        version: 3,
        source: "pi_runtime_debug".into(),
        system_prompt: Some("system line 1\n<project_context>真实上下文</project_context>".into()),
        tools: vec![RuntimeDebugTool {
            name: "bash".into(),
            description: "Run a shell command".into(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["command"],
                "properties": {"command": {"type": "string"}}
            }),
            source: Some("builtin".into()),
        }],
        model_calls: vec![RuntimeDebugModelCall {
            sequence: 3,
            turn: Some(2),
            compaction_sequence: None,
            captured_at_ms: 1_725_000_000_000,
            source: "pi_context_with_system".into(),
            model: RuntimeDebugModel {
                provider: Some("openai".into()),
                id: Some("gpt-test".into()),
                api: Some("responses".into()),
                thinking_level: Some("high".into()),
            },
            request_config: smelt_plugin_api::model::acp_session::RuntimeDebugRequestConfig {
                system_prompt: "prompt for call #3".into(),
                tools: vec![RuntimeDebugTool {
                    name: "bash".into(),
                    description: "Run a shell command".into(),
                    parameters: serde_json::json!({"type": "object"}),
                    source: Some("builtin".into()),
                }],
            },
            pi_context: serde_json::json!([
                {"role": "system", "content": "per-call system"},
                {"role": "user", "content": "hello"}
            ]),
            payload: serde_json::json!({
                "model": "gpt-test",
                "max_tokens": 4096,
                "messages": [{"role": "user", "content": "hello"}],
                "api_key": "[REDACTED]"
            }),
            response: Some(serde_json::json!({
                "role": "assistant",
                "usage": {"input": 100, "output": 20}
            })),
            response_captured_at_ms: Some(1_725_000_000_100),
            response_redacted_paths: Some(Vec::new()),
            redacted_paths: vec!["$.payload.api_key".into()],
            ..RuntimeDebugModelCall::default()
        }],
        ..RuntimeDebug::default()
    });

    assert_eq!(contexts.len(), 2, "调用 payload 与组装配置是不同证据边界");
    let config_source: serde_json::Value = serde_json::from_str(&contexts[0].source).unwrap();
    let call_source: serde_json::Value = serde_json::from_str(&contexts[1].source).unwrap();
    assert_eq!(contexts[0].label, "Pi REQUEST CONFIG");
    assert_eq!(contexts[0].lane, super::TrajectoryLane::Request);
    assert_eq!(
        config_source["systemPrompt"],
        "system line 1\n<project_context>真实上下文</project_context>"
    );
    assert_eq!(config_source["tools"][0]["name"], "bash");
    assert!(contexts[0].preview.contains("System prompt: "));
    assert!(contexts[0].preview.contains("1 tools"));
    assert!(config_source.get("available").is_none());
    assert!(config_source.get("classification").is_none());
    assert!(config_source.get("not_captured").is_none());

    assert_eq!(contexts[1].label, "MODEL CALL #3 · PI TURN 2");
    assert_eq!(contexts[1].lane, super::TrajectoryLane::ModelCall);
    assert_eq!(contexts[1].pi_turn, Some(2));
    assert_eq!(contexts[1].captured_at_ms, Some(1_725_000_000_000));
    assert!(contexts[1].preview.contains("openai"));
    assert!(contexts[1].preview.contains("gpt-test"));
    assert!(contexts[1].preview.contains("2 Pi messages"));
    assert!(contexts[1].source.contains("high"));
    assert!(contexts[1].source.contains("max_tokens"));
    assert!(contexts[1].source.contains("[REDACTED]"));
    assert_eq!(call_source["piContext"][0]["content"], "per-call system");
    assert_eq!(call_source["payload"]["messages"][0]["content"], "hello");
    assert_eq!(call_source["redactedPaths"][0], "$.payload.api_key");
    assert_eq!(
        call_source["requestConfig"]["systemPrompt"], "prompt for call #3",
        "每次模型调用都要保留当次的 Pi 组装配置，而不是用最近一份覆盖旧调用"
    );
    assert_eq!(call_source["response"]["usage"]["output"], 20);
}

#[test]
fn compaction_traces_show_the_observed_boundary_without_joining_the_fake_timeline() {
    let contexts = runtime_debug_contexts(&RuntimeDebug {
        version: 3,
        source: "pi_runtime_debug".into(),
        compactions: vec![
            smelt_plugin_api::model::acp_session::RuntimeDebugCompaction {
                sequence: 1,
                turn: Some(3),
                status: "completed".into(),
                reason: "overflow".into(),
                will_retry: true,
                started_at_ms: 1_725_000_000_000,
                finished_at_ms: Some(1_725_000_000_500),
                first_kept_entry_id: Some("entry-20".into()),
                tokens_before: Some(90_000),
                summarized_message_count: Some(12),
                source_messages: vec![
                    smelt_plugin_api::model::acp_session::RuntimeDebugCompactionMessage {
                        segment: "summarized".into(),
                        role: "user".into(),
                        preview: "retain the auth constraint".into(),
                        truncated: false,
                    },
                ],
                summary: Some("The user is debugging auth".into()),
                ..Default::default()
            },
        ],
        ..RuntimeDebug::default()
    });
    let events = session_trajectory_events(&[], contexts, &Default::default());

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].lane, super::TrajectoryLane::Compaction);
    assert_eq!(
        events[0].turn, 0,
        "a Pi capture is not inserted into ACP message order"
    );
    assert_eq!(events[0].pi_turn, Some(3));
    assert!(events[0].text.contains("OVERFLOW · COMPLETED"));
    assert!(events[0].source.contains("entry-20"));
    assert!(events[0].preview.contains("12 messages summarized"));
    assert!(events[0].source.contains("retain the auth constraint"));
    assert_eq!(
        filter_trajectory_events(&events, "pi turn 3 entry-20").len(),
        1
    );
}

#[test]
fn runtime_debug_contexts_show_only_request_config_without_a_model_call() {
    let contexts = runtime_debug_contexts(&RuntimeDebug {
        version: 1,
        source: "pi_before_agent_start".into(),
        system_prompt: Some("actual system prompt".into()),
        ..RuntimeDebug::default()
    });

    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].lane, super::TrajectoryLane::Request);
    assert_eq!(contexts[0].label, "Pi REQUEST CONFIG");
    let source: serde_json::Value = serde_json::from_str(&contexts[0].source).unwrap();
    assert_eq!(source["systemPrompt"], "actual system prompt");
    assert!(!contexts[0].preview.contains("未捕获"));
}

#[test]
fn runtime_debug_contexts_are_empty_without_a_runtime_capture() {
    let contexts = runtime_debug_contexts(&RuntimeDebug::default());
    assert!(
        contexts.is_empty(),
        "Trajectory 只能投影 runtime 实际上报的数据，不能合成未捕获占位记录"
    );
}

#[test]
fn nested_subagent_entries_are_independent_trajectory_events() {
    let entries = vec![
        AcpEntry::User("排查认证".into()),
        AcpEntry::ToolCall {
            id: "sa-1".into(),
            title: "scout auth".into(),
            kind: ToolKind::Collaborate,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: vec![
                AcpEntry::Assistant {
                    text: "先检查配置".into(),
                    thought: true,
                },
                AcpEntry::tool_call(
                    "sa-1-child-1",
                    "config.rs",
                    ToolKind::Read,
                    ToolCallStatus::Completed,
                    vec![ToolOutputPart::Text("issuer = demo".into())],
                ),
            ],
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
    ];
    let tool_debug = std::collections::BTreeMap::from([(
        "sa-1-child-1".to_string(),
        smelt_plugin_api::model::acp_session::ToolCallDebug {
            name: Some("read".into()),
            raw_input: Some(serde_json::json!({"path": "config.rs"})),
        },
    )]);
    let events = session_trajectory_events(&entries, Vec::new(), &tool_debug);

    assert_eq!(events.len(), 4);
    assert_eq!(events[2].lane, super::TrajectoryLane::Thinking);
    assert_eq!(events[2].depth, 1);
    assert_eq!(events[2].parent_tool_id.as_deref(), Some("sa-1"));
    assert_eq!(events[3].lane, super::TrajectoryLane::Tool);
    assert_eq!(events[3].turn, 1);
    assert_eq!(events[3].depth, 1);
    assert_eq!(events[3].parent_tool_id.as_deref(), Some("sa-1"));
    assert!(events[3].preview.contains("Tool name: read"));
    assert!(events[3].source.contains("config.rs"));
    assert_eq!(trajectory_counts(&entries), (1, 2));
}

#[test]
fn session_trajectory_events_keep_debug_sources_and_model_thinking() {
    let entries = vec![
        AcpEntry::User("你好".into()),
        AcpEntry::Assistant {
            text: "先找图标定义".into(),
            thought: true,
        },
        AcpEntry::ToolCall {
            id: "s1".into(),
            title: "icon".into(),
            kind: ToolKind::Search,
            status: ToolCallStatus::Completed,
            output: vec![ToolOutputPart::Text("found icon.rs".into())],
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        completed_tool("done", ToolKind::Other, "task_complete"),
        AcpEntry::Assistant {
            text: "好了".into(),
            thought: false,
        },
    ];
    let contexts = vec![super::TrajectoryContext {
        lane: super::TrajectoryLane::Request,
        label: "最近一次 Pi 请求组装配置".into(),
        preview: "system prompt".into(),
        source: "{\"system_prompt\":\"system prompt\",\"tools\":[]}".into(),
        pi_turn: None,
        captured_at_ms: None,
    }];
    let tool_debug = std::collections::BTreeMap::from([(
        "s1".to_string(),
        smelt_plugin_api::model::acp_session::ToolCallDebug {
            name: Some("grep".into()),
            raw_input: Some(serde_json::json!({"pattern": "IconName", "path": "src"})),
        },
    )]);
    let events = session_trajectory_events(&entries, contexts, &tool_debug);
    assert_eq!(events[0].lane, super::TrajectoryLane::Request);
    assert_eq!(events[1].lane, super::TrajectoryLane::User);
    assert_eq!(events[1].text, "你好");
    assert_eq!(events[2].lane, super::TrajectoryLane::Thinking);
    assert_eq!(events[3].lane, super::TrajectoryLane::Tool);
    assert_eq!(events[3].text, "Searched icon");
    assert!(events[3].preview.contains("Tool name: grep"));
    assert!(events[3].preview.contains("found icon.rs"));
    assert!(events[3].source.contains("IconName"));
    assert!(events[3].source.contains("raw_input"));
    assert!(!events[3].source.contains("raw_input_available"));
    assert_eq!(events[4].lane, super::TrajectoryLane::Assistant);
    assert_eq!(
        super::trajectory_lane_label(events[4].lane),
        "ASSISTANT",
        "模型产出的消息不是一次模型调用"
    );
    assert_eq!(events[1].turn, 1);
    assert_eq!(trajectory_counts(&entries), (1, 1));
    assert_eq!(filter_trajectory_events(&events, "IconName src").len(), 1);
    assert_eq!(filter_trajectory_events(&events, "turn 1").len(), 4);
}

#[test]
fn tool_trajectory_omits_debug_fields_when_no_sidecar_was_reported() {
    let entries = vec![
        AcpEntry::User("执行".into()),
        AcpEntry::tool_call(
            "call-1",
            "展示标题",
            ToolKind::Other,
            ToolCallStatus::Completed,
            vec![ToolOutputPart::Text("真实结果".into())],
        ),
    ];

    let events = session_trajectory_events(&entries, Vec::new(), &Default::default());
    let tool = &events[1];
    assert_eq!(tool.lane, super::TrajectoryLane::Tool);
    assert!(tool.preview.contains("Call ID: call-1"));
    assert!(tool.preview.contains("真实结果"));
    assert!(!tool.preview.contains("Tool name:"));
    assert!(!tool.preview.contains("Arguments\n"));
    assert!(!tool.preview.contains("未提供"));
    assert!(!tool.source.contains("provider_debug"));
}

fn thought(text: &str) -> AcpEntry {
    AcpEntry::Assistant {
        text: text.into(),
        thought: true,
    }
}

#[test]
fn compact_tool_headlines_use_verbs_like_opened_page() {
    assert_eq!(
        compact_tool_headline(
            ToolKind::Fetch,
            "https://www.ag-grid.com/vue-data-grid/grouping/"
        ),
        "Opened ag-grid.com/vue-data-grid/grouping/"
    );
    assert_eq!(
        compact_tool_headline(ToolKind::Search, "ag-grid master detail"),
        "Searched ag-grid master detail"
    );
    assert_eq!(
        compact_tool_headline(ToolKind::Read, "crates/smelt/src/main.rs"),
        "Read main.rs"
    );
    assert_eq!(
        compact_tool_headline(ToolKind::Execute, "cargo test -p smelt-acp-view"),
        "Ran cargo test -p smelt-acp-view"
    );
}

/// 没正常结束的回合在末尾给卡：出错、被拒重试，截断、到上限接着说。
/// 成功和用户自己停的不给。
#[test]
fn only_an_unfinished_turn_gets_a_card_with_the_right_action() {
    use smelt_plugin_api::model::acp_session::AcpTurnOutcome;

    let action = |outcome| turn_outcome_card(Some(outcome), true).map(|(_, action)| action);
    assert_eq!(turn_outcome_card(None, true), None);
    assert_eq!(action(AcpTurnOutcome::Succeeded), None);
    assert_eq!(action(AcpTurnOutcome::Cancelled), None);
    assert_eq!(
        action(AcpTurnOutcome::Failed),
        Some(Some(TurnOutcomeAction::Retry))
    );
    assert_eq!(
        action(AcpTurnOutcome::Refused),
        Some(Some(TurnOutcomeAction::Retry))
    );
    assert_eq!(
        action(AcpTurnOutcome::MaxTokens),
        Some(Some(TurnOutcomeAction::Continue))
    );
    assert_eq!(
        action(AcpTurnOutcome::MaxTurnRequests),
        Some(Some(TurnOutcomeAction::Continue))
    );
}

/// 不能回退的 agent 没有真正的重试：只能当新消息再发，会叠出一条一样的用户消息。
/// 卡照样给，说清这一轮没完成，但不给「重试」；「继续」本来就是新消息，照给。
#[test]
fn retry_is_offered_only_when_the_agent_can_rewind() {
    use smelt_plugin_api::model::acp_session::AcpTurnOutcome;

    assert_eq!(
        turn_outcome_card(Some(AcpTurnOutcome::Failed), false),
        Some((gpui::SharedString::from("This turn didn't finish"), None))
    );
    assert_eq!(
        turn_outcome_card(Some(AcpTurnOutcome::Refused), false).map(|(_, action)| action),
        Some(None)
    );
    assert_eq!(
        turn_outcome_card(Some(AcpTurnOutcome::MaxTokens), false).map(|(_, action)| action),
        Some(Some(TurnOutcomeAction::Continue))
    );
}

/// 退避期间倒数；重发出去以后报已经等了多久，provider 迟迟不回时这行字也在走。
#[test]
fn provider_retry_label_counts_down_then_reports_the_wait() {
    let retry = smelt_plugin_api::model::acp_session::ProviderRetryView {
        attempt: 2,
        max_attempts: 3,
        delay_ms: 4_200,
        started_at_ms: 10_000,
        error: "429 rate limited".into(),
    };
    assert_eq!(
        provider_retry_label(&retry, 10_000),
        "Model request failed; retry 2/3 in 5s"
    );
    assert_eq!(
        provider_retry_label(&retry, 12_500),
        "Model request failed; retry 2/3 in 2s"
    );
    assert_eq!(
        provider_retry_label(&retry, 14_200),
        "Model request failed; retry 2/3 in progress · 0s"
    );
    assert_eq!(
        provider_retry_label(&retry, 14_200 + 69_000),
        "Model request failed; retry 2/3 in progress · 1m 9s"
    );
    // 本机时钟比守护进程慢：不倒数成负数，按刚开始算。
    assert_eq!(
        provider_retry_label(&retry, 9_000),
        "Model request failed; retry 2/3 in 5s"
    );
    let no_wait = smelt_plugin_api::model::acp_session::ProviderRetryView {
        delay_ms: 0,
        max_attempts: 0,
        ..retry.clone()
    };
    assert_eq!(
        provider_retry_label(&no_wait, 13_000),
        "Model request failed; retry 2 in progress · 3s"
    );
    let unknown_start = smelt_plugin_api::model::acp_session::ProviderRetryView {
        started_at_ms: 0,
        ..retry
    };
    assert_eq!(
        provider_retry_label(&unknown_start, 99_000),
        "Model request failed; retry 2/3 in progress"
    );
}

#[test]
fn background_task_elapsed_counts_from_the_start() {
    assert_eq!(
        background_task_elapsed(1_000, 193_000),
        "Running for 3m 12s"
    );
    assert_eq!(background_task_elapsed(1_000, 1_500), "Running for 0s");
    assert_eq!(
        background_task_elapsed(5_000, 1_000),
        "Running for 0s",
        "时钟往回拨时不能下溢"
    );
}

#[test]
fn background_task_summary_counts_running_tasks() {
    let running = BackgroundTaskView {
        id: "bg-1".into(),
        title: "测试".into(),
        command: "npm test".into(),
        status: BackgroundTaskStatus::Running,
        exit_code: None,
        output: String::new(),
        started_at_ms: 1,
        finished_at_ms: None,
    };
    let done = BackgroundTaskView {
        status: BackgroundTaskStatus::Completed,
        ..running.clone()
    };
    assert_eq!(background_task_summary(1), "1 background task");
    assert_eq!(
        running_background_tasks(&[running, done.clone()])
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        ["bg-1"]
    );
    assert!(running_background_tasks(&[done]).is_empty());
    let command = "git commit -m \"$(cat <<'EOF'\nfeat: 补上目录\n\n侧栏新对话\nEOF\n)\"";
    assert_eq!(
        background_task_one_line(command),
        "git commit -m \"$(cat <<'EOF' feat: 补上目录 侧栏新对话 EOF )\""
    );
    assert!(!background_task_one_line("  a\r\n\tb  ").contains(['\n', '\r']));
}

#[test]
fn subagent_headline_strips_spawn_prefixes() {
    assert_eq!(
        smelt_plugin_api::model::acp_chat::subagent_visible_title("Start subagent explorer"),
        "explorer"
    );
    assert_eq!(
        smelt_plugin_api::model::acp_chat::subagent_visible_title("Task"),
        ""
    );
    assert_eq!(
        smelt_plugin_api::model::acp_chat::subagent_visible_title("subagent"),
        ""
    );
    assert_eq!(
        smelt_plugin_api::model::acp_chat::subagent_visible_title(""),
        ""
    );
    assert_eq!(
        smelt_plugin_api::model::acp_chat::subagent_visible_title("查登录"),
        "查登录"
    );
}

#[test]
fn process_header_counts_tools_in_a_fixed_order() {
    let entries = vec![
        AcpEntry::User("看看今天的新闻".into()),
        completed_tool("s1", ToolKind::Search, "today news"),
        completed_tool("s2", ToolKind::Search, "international"),
        completed_tool("s3", ToolKind::Search, "china"),
        completed_tool("r1", ToolKind::Read, "notes.md"),
        AcpEntry::Assistant {
            text: "今天是星期二".into(),
            thought: false,
        },
    ];
    let layout = build_conversation_layout(&entries, false);
    let group = layout[1].process_group.expect("结束后应收成过程组");
    assert!(!group.active);
    assert_eq!(
        process_group_header_label(group, false),
        "Read 1 file · Searched 3 times",
        "固定顺序：命令、读取、改动、搜索"
    );
}

#[test]
fn compact_diff_stats_sum_all_cached_diff_parts() {
    let cached = vec![
        Some(CachedDiff {
            added: 3,
            removed: 1,
        }),
        None,
        Some(CachedDiff {
            added: 2,
            removed: 4,
        }),
    ];

    assert_eq!(cached_diff_stats(Some(&cached)), Some((5, 5)));
    assert_eq!(cached_diff_stats(None), None);
}

#[test]
fn uncached_diff_stats_are_available_from_tool_output() {
    let output = vec![ToolOutputPart::Diff {
        path: "src/lib.rs".into(),
        old_text: Some("old\n".into()),
        new_text: "new\nadded\n".into(),
    }];

    assert_eq!(diff_stats_for_output(&output), Some((2, 1)));
}

#[test]
fn diff_cache_shape_must_follow_tool_output_parts() {
    let cached = vec![None];
    let output = vec![ToolOutputPart::Diff {
        path: "src/lib.rs".into(),
        old_text: None,
        new_text: "fn main() {}".into(),
    }];

    assert!(!diff_cache_matches_output(&cached, &output));
}

#[test]
fn empty_tool_output_does_not_offer_expandable_content() {
    assert!(!tool_output_has_content(&[]));
    assert!(!tool_output_has_content(&[ToolOutputPart::Text(
        " \n".into()
    )]));
    assert!(!tool_output_has_content(&[ToolOutputPart::Text(
        "```console\n```".into()
    )]));
    assert!(tool_output_has_content(&[ToolOutputPart::Text(
        "result".into()
    )]));
    assert!(tool_output_has_content(&[ToolOutputPart::Diff {
        path: "src/lib.rs".into(),
        old_text: None,
        new_text: "fn main() {}".into(),
    }]));
}

#[test]
fn tool_image_output_is_expandable_and_decodes_once() {
    // 1x1 PNG
    let image = smelt_plugin_api::model::acp_chat::AcpImage::new(
        "image/png",
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
    );
    let output = vec![
        ToolOutputPart::Text("Read image file [image/png]".into()),
        ToolOutputPart::Image(image),
    ];

    assert!(tool_output_has_content(&output));
    let decoded = build_tool_image_parts(&output);
    assert!(decoded[0].is_none());
    assert!(decoded[1].is_some());
    assert!(tool_image_cache_matches_output(&decoded, &output));
    // 形状变了（少一段输出）就必须重建，不能拿旧下标去索引新 output。
    assert!(!tool_image_cache_matches_output(&decoded, &output[..1]));
}

#[test]
fn permission_selection_only_accepts_the_queue_head() {
    let permission = |tool_call_id: &str, option_id: &str| PendingPermission {
        id: "p1".into(),
        question: tool_call_id.into(),
        tool_call_id: tool_call_id.into(),
        options: vec![PermissionOptionView {
            option_id: option_id.into(),
            name: "Allow once".into(),
            kind: PermissionOptionKindView::AllowOnce,
        }],
        details: ApprovalDetailsView::Generic,
    };
    let permissions = vec![
        permission("tool-1", "allow-1"),
        permission("tool-2", "allow-2"),
    ];

    assert!(is_active_permission_selection(
        &permissions,
        "tool-1",
        "allow-1"
    ));
    assert!(!is_active_permission_selection(
        &permissions,
        "tool-2",
        "allow-2"
    ));
    assert!(!is_active_permission_selection(
        &permissions,
        "tool-1",
        "unknown"
    ));
}

// 回归守卫：`AcpEntry::User`/`UserWithImages` 气泡是"收缩到内容大小"的 flex
// item（只有 max_w，没有 width），而 gpui-base 的 markdown 有序/无序列表
// 内部用 `w_full()`/`flex_1()` 排布"序号 + 正文"。两者叠加时，短列表内容会被
// 误测成只有序号那么宽，正文被列表内容行的 `overflow_hidden()` 裁没，聊天里
// 只剩悬浮的 "1." "2."（bug 复现见 PR 描述）。锁死气泡有个不为零的最小宽度，
// 防止以后有人顺手把 `min_w` 从气泡样式里删掉。
struct ListBubbleTestRoot {
    text: &'static str,
}

impl gpui::Render for ListBubbleTestRoot {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{InteractiveElement, ParentElement, Styled, div, px};
        // 复刻 acp_view 里用户气泡的真实结构：外层限宽窗口 -> 右对齐 flex 行
        // -> 收缩到内容大小、带 max_w/min_w 的气泡 -> markdown 正文。
        div().w(px(500.)).child(
            div().flex().w_full().justify_end().child(
                div()
                    .max_w(px(400.))
                    .min_w(px(160.))
                    .debug_selector(|| "BUBBLE".to_string())
                    .child(
                        smelt_plugin_api::ui::markdown_mermaid::markdown_view_clickable(
                            "list-bubble-test",
                            self.text.to_string(),
                        ),
                    ),
            ),
        )
    }
}

#[gpui::test]
fn short_ordered_list_bubble_does_not_collapse_below_min_width(cx: &mut gpui::TestAppContext) {
    use gpui::{VisualTestContext, px};

    cx.update(init_ui);
    // gpui_base::Root 构造时要装 macOS 无障碍 hit-test 转发器，需要真实
    // 平台窗口句柄（TestWindow 不提供）；直接把复刻结构的根元素作为窗口根视图，
    // 绕开 Root，`Window::draw` 在测试窗口是安全的。
    let (_, cx) = cx.add_window_view(|_window, _cx| ListBubbleTestRoot {
        text: "1. xxxx\n2. bbbbb",
    });
    let cx: &mut VisualTestContext = cx;
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let bounds = cx
        .debug_bounds("BUBBLE")
        .expect("bubble should have painted");
    assert!(
        bounds.size.width >= px(160.),
        "list bubble collapsed below its min-width floor: {:?}; short list item \
             text would be clipped invisible by the list renderer's overflow_hidden()",
        bounds.size.width,
    );
}

#[gpui::test]
fn expanded_process_paints_thought_and_tools_after_the_visible_body(cx: &mut gpui::TestAppContext) {
    use gpui::VisualTestContext;

    cx.update(init_ui);
    let entries = vec![
        AcpEntry::User("优化 ACP 对话".into()),
        AcpEntry::Assistant {
            text: "先定位会话展示入口\n接着检查工具状态更新。".into(),
            thought: false,
        },
        AcpEntry::ToolCall {
            id: "search-1".into(),
            title: "BottomPanel".into(),
            kind: ToolKind::Search,
            status: ToolCallStatus::Completed,
            output: vec![ToolOutputPart::Text("Found 12 matches".into())],
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::Assistant {
            text: "已经定位入口，正在读取实现".into(),
            thought: true,
        },
        AcpEntry::ToolCall {
            id: "read-1".into(),
            title: "crates/smelt-acp-view/src/acp_view.rs".into(),
            kind: ToolKind::Read,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        completed_tool("read-2", ToolKind::Read, "crates/smelt-acp-view/src/mod.rs"),
        AcpEntry::Assistant {
            text: "已经改好过程组展示".into(),
            thought: false,
        },
    ];
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        let mut view = super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Claude,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: Some("/tmp/smelt".into()),
                reason: "visual test".into(),
                resume_session_id: None,
                saved_sid: Some("acp-process-visual-test".into()),
            },
        );
        view.apply_snapshot(tail_snapshot(0, 0, entries, DaemonPhase::Idle), cx);
        view.expanded_turn_folds.insert(1);
        view.expanded_process_groups.insert((2, false));
        view
    });
    let cx: &mut VisualTestContext = cx;
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let body = view
        .read_with(cx, |view, _| view.list_state.bounds_for_item(1))
        .expect("中间正文应完整绘制");
    let header = cx
        .debug_bounds("ACP_PROCESS_GROUP_2")
        .expect("process header should paint");
    assert!(
        cx.debug_bounds("ACP_PROGRESS_1").is_none(),
        "正文不能画成过程摘要"
    );
    let tool = cx
        .debug_bounds("ACP_TOOL_COMPACT_2")
        .expect("completed tool should paint as a compact timeline row");
    let thought = cx
        .debug_bounds("ACP_PROGRESS_3")
        .expect("thought content should paint on the timeline");
    let read = cx
        .debug_bounds("ACP_TOOL_COMPACT_4")
        .expect("later read should stay a compact row");

    assert!(body.size.height > px(0.));
    assert!(header.size.width > gpui::px(300.));
    assert!(header.origin.y >= body.origin.y + body.size.height);
    assert!(tool.origin.y >= header.origin.y + header.size.height);
    assert!(thought.origin.y >= tool.origin.y + tool.size.height);
    assert!(read.origin.y >= thought.origin.y + thought.size.height);
    assert_eq!(
        tool.origin.x, read.origin.x,
        "能预览输出的行（搜索）和不能预览的行（读文件）左边缘对齐"
    );
    let next_read = cx
        .debug_bounds("ACP_TOOL_COMPACT_5")
        .expect("段标题已经汇总过，展开后相邻的同类工具也各占一行");
    assert!(next_read.origin.y >= read.origin.y + read.size.height);
    let thought_text = cx
        .debug_bounds("ACP_PROGRESS_TEXT_3")
        .expect("thought preview text should paint");
    let tool_text = cx
        .debug_bounds("ACP_TOOL_LABEL_2")
        .expect("tool label should paint");
    assert_eq!(
        thought_text.origin.x, tool_text.origin.x,
        "思考是旁注：图标列空着，字和工具名对齐"
    );
}

/// 展开回合摘要、各段仍收着时，后台唤醒前后的说明和后台任务状态行要实际占据可见的行。
#[gpui::test]
fn collapsed_background_wakeup_paints_every_body_and_task_note(cx: &mut gpui::TestAppContext) {
    let (view, cx) = open_conversation(
        cx,
        "acp-background-wakeup-bodies",
        background_wakeup_entries(),
        DaemonPhase::Idle,
    );
    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.expanded_turn_folds.insert(1);
            view.list_state.remeasure();
            cx.notify();
        })
    });
    for ix in [2, 3, 4, 7] {
        cx.update(|_window, cx| {
            view.update(cx, |view, cx| {
                view.list_state.scroll_to(gpui::ListOffset {
                    item_ix: ix,
                    offset_in_item: px(0.),
                });
                cx.notify();
            })
        });
        draw_settled(cx);
        draw_settled(cx);
        let bounds = view
            .read_with(cx, |view, _| view.list_state.bounds_for_item(ix))
            .expect("正文或状态行应已绘制");
        assert!(
            bounds.size.height > px(0.),
            "行 {ix} 不能被折叠: {bounds:?}"
        );
        if ix == 4 {
            assert!(
                cx.debug_bounds("ACP_PROGRESS_4").is_none(),
                "唤醒后的说明不能变成过程摘要"
            );
        }
    }
}

#[gpui::test]
fn empty_streaming_body_keeps_the_now_tail_visible(cx: &mut gpui::TestAppContext) {
    let (_view, cx) = open_conversation(
        cx,
        "acp-empty-streaming-body",
        vec![
            AcpEntry::User("你好".into()),
            AcpEntry::Assistant {
                text: " ".into(),
                thought: false,
            },
        ],
        DaemonPhase::Thinking,
    );
    draw_settled(cx);
    assert!(cx.debug_bounds("ACP_NOW_1").is_some());
}

/// 在跑的段收着时，最后一条藏在段里高度为 0；「现在」照样挂在它这一行上露出来。
#[gpui::test]
fn the_now_tail_shows_under_a_collapsed_running_segment(cx: &mut gpui::TestAppContext) {
    let (view, cx) = open_conversation(
        cx,
        "acp-now-collapsed-segment",
        vec![
            AcpEntry::User("跑一下测试".into()),
            completed_tool("read", ToolKind::Read, "src/lib.rs"),
            AcpEntry::ToolCall {
                id: "run".into(),
                title: "cargo test".into(),
                kind: ToolKind::Execute,
                status: ToolCallStatus::InProgress,
                output: Vec::new(),
                children: Vec::new(),
                subagents: Vec::new(),
                peer: None,
                asked: None,
            },
        ],
        DaemonPhase::Thinking,
    );
    draw_settled(cx);
    assert!(cx.debug_bounds("ACP_PROCESS_GROUP_1").is_some());
    assert!(
        cx.debug_bounds("ACP_NOW_2").is_some(),
        "收起的段里最后一条也要挂上「现在」"
    );
    let tail_row = view
        .read_with(cx, |view, _| view.list_state.bounds_for_item(2))
        .expect("最后一行应已绘制");
    assert!(tail_row.size.height > px(0.));
}

/// 最终回答多一排操作按钮，但相同正文的字号和行高不应因此不同。
#[gpui::test]
fn final_answer_changes_actions_not_body_text_size(cx: &mut gpui::TestAppContext) {
    let body = |text: String| AcpEntry::Assistant {
        text,
        thought: false,
    };
    let short = "正文内容".to_string();
    let long = (0..12)
        .map(|_| short.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let (view, cx) = open_conversation(
        cx,
        "acp-answer-text-size",
        vec![
            AcpEntry::User("短回答".into()),
            body(short.clone()),
            body(short),
            AcpEntry::User("长回答".into()),
            body(long.clone()),
            body(long),
        ],
        DaemonPhase::Idle,
    );
    // 非最终的那条收在回合摘要里，展开才看得到正文。
    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.expanded_turn_folds.extend([1, 4]);
            view.list_state.remeasure();
            cx.notify();
        })
    });
    let mut heights = Vec::new();
    for ix in [1, 2, 4, 5] {
        cx.update(|_window, cx| {
            view.update(cx, |view, cx| {
                view.list_state.scroll_to(gpui::ListOffset {
                    item_ix: ix,
                    offset_in_item: px(0.),
                });
                cx.notify();
            })
        });
        draw_settled(cx);
        draw_settled(cx);
        let bounds = view
            .read_with(cx, |view, _| view.list_state.bounds_for_item(ix))
            .expect("正文行应已测量");
        heights.push(bounds.size.height);
    }
    let non_final_growth = heights[2] - heights[0];
    let final_growth = heights[3] - heights[1];
    assert!(non_final_growth > px(0.) && final_growth > px(0.));
    assert!(
        (non_final_growth - final_growth).abs() < px(1.),
        "同样增加正文，最终与非最终回答的高度增量应相同: {heights:?}"
    );
}

/// 摆对话的一方聚焦这段对话（新建、切过来）时，键盘焦点落进输入框，能直接打字。
#[gpui::test]
fn focusing_the_conversation_lands_in_the_composer(cx: &mut gpui::TestAppContext) {
    use gpui::Focusable as _;

    cx.update(init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut view = super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Claude,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: None,
                reason: "composer focus".into(),
                resume_session_id: None,
                saved_sid: Some("acp-composer-focus".into()),
            },
        );
        view.phase = DaemonPhase::Idle;
        view.init_input(window, cx);
        view
    });
    cx.update(|window, cx| {
        let target = view.read(cx).focus_target(cx);
        window.focus(&target, cx);
        let input = view.read(cx).input.clone().expect("输入框应该建好");
        assert!(input.focus_handle(cx).is_focused(window));
    });
}

/// 访达里的图片拖到输入框上：收进待发缩略图，跟「+」选图一样。落在输入框外不收。
#[gpui::test]
fn image_dropped_on_the_composer_is_attached(cx: &mut gpui::TestAppContext) {
    use gpui::{FileDropEvent, VisualTestContext};

    let dir = std::env::temp_dir().join(format!("smelt-composer-drop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let image = dir.join("shot.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\n").unwrap();

    cx.update(init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut view = super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Claude,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: None,
                reason: "composer drop".into(),
                resume_session_id: None,
                saved_sid: Some("acp-composer-drop".into()),
            },
        );
        view.phase = DaemonPhase::Idle;
        view.init_input(window, cx);
        view
    });
    let cx: &mut VisualTestContext = cx;
    draw_settled(cx);

    let composer = cx.debug_bounds("ACP_COMPOSER").expect("输入框应该画出来");
    let outside = gpui::point(composer.origin.x + px(1.), composer.origin.y - px(20.));

    let drop_at = |position: gpui::Point<gpui::Pixels>, cx: &mut VisualTestContext| {
        cx.simulate_event(FileDropEvent::Entered {
            position,
            paths: ExternalPaths(vec![image.clone()].into()),
        });
        draw_settled(cx);
        cx.simulate_event(FileDropEvent::Submit { position });
        draw_settled(cx);
    };

    drop_at(outside, cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.pending_images.len()),
        0,
        "落在输入框外不收"
    );

    drop_at(composer.center(), cx);
    view.read_with(cx, |view, _| {
        assert_eq!(view.pending_images.len(), 1, "拖进来的图片进待发列表");
        assert_eq!(view.pending_external_image_loads, 0);
    });
    let _ = std::fs::remove_dir_all(&dir);
}

#[gpui::test]
fn text_elicitation_inputs_are_ready_before_paint(cx: &mut gpui::TestAppContext) {
    use gpui::VisualTestContext;

    cx.update(init_ui);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        let mut view = super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Claude,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: None,
                reason: "test placeholder".into(),
                resume_session_id: None,
                saved_sid: Some("acp-render-test".into()),
            },
        );
        view.elicitation = Some(PendingElicitation {
            id: "q1".into(),
            message: "请输入访问令牌".into(),
            fields: vec![ElicitFieldView {
                key: "token".into(),
                title: "访问令牌".into(),
                required: true,
                allow_custom_input: false,
                kind: ElicitFieldKindView::Text { secret: true },
            }],
            chosen: Default::default(),
            text_values: Default::default(),
        });
        view
    });
    let cx: &mut VisualTestContext = cx;
    view.update(cx, |view, cx| view.schedule_surface_inputs(cx));
    cx.run_until_parked();

    assert_eq!(
        view.read_with(cx, |view, _| view.elicitation_inputs.len()),
        1,
        "提问输入框在绘制前就建好",
    );
}

#[gpui::test]
fn acp_view_render_multiselect_elicitation_is_a_stacked_choice_list(cx: &mut gpui::TestAppContext) {
    use gpui::VisualTestContext;

    cx.update(init_ui);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        let mut view = super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Claude,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: None,
                reason: "test placeholder".into(),
                resume_session_id: None,
                saved_sid: Some("acp-elicit-visual-test".into()),
            },
        );
        view.elicitation = Some(PendingElicitation {
            id: "q1".into(),
            message: "选几种水果看看".into(),
            fields: vec![ElicitFieldView {
                key: "value".into(),
                title: "选几种水果看看".into(),
                required: true,
                allow_custom_input: true,
                kind: ElicitFieldKindView::MultiSelect(vec![
                    ElicitOptionView {
                        label: "苹果".into(),
                        description: None,
                        flag: None,
                    },
                    ElicitOptionView {
                        label: "香蕉".into(),
                        description: None,
                        flag: None,
                    },
                    ElicitOptionView {
                        label: "橙子".into(),
                        description: None,
                        flag: None,
                    },
                ]),
            }],
            chosen: Default::default(),
            text_values: Default::default(),
        });
        view
    });
    let cx: &mut VisualTestContext = cx;
    view.update(cx, |view, cx| view.schedule_surface_inputs(cx));
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let apple = cx
        .debug_bounds("acp-elicit-opt-0-0")
        .expect("first option should paint");
    let banana = cx
        .debug_bounds("acp-elicit-opt-0-1")
        .expect("second option should paint below the first");
    assert!(
        banana.origin.y >= apple.origin.y + apple.size.height - px(1.),
        "multi-select options must stack as rows, not wrap as chips: apple={apple:?} banana={banana:?}"
    );
    assert!(
        apple.size.width > px(240.),
        "option rows should fill the card, not shrink to a pill: {:?}",
        apple.size
    );
    cx.debug_bounds("acp-elicit-submit")
        .expect("multi-select must show a submit button");
    let card = cx
        .debug_bounds("acp-elicit-card")
        .expect("choice card should paint");
    assert!(
        card.size.width <= ui_theme::conversation_max_width() + px(1.),
        "choice card must stay on the conversation column, not stretch across the window: {:?}",
        card.size
    );
}

/// 只有一道单选题的提问卡：选项一行一个，说明排在标题下面；点一下就是答案，不另要
/// 「提交」。自己写了答案时，答案就在输入那一行里交。
fn ask_card(fields: Vec<ElicitFieldView>) -> PendingElicitation {
    PendingElicitation {
        id: "q1".into(),
        message: "下一步要我执行哪项？".into(),
        fields,
        chosen: Default::default(),
        text_values: Default::default(),
    }
}

fn choice(label: &str, description: Option<&str>) -> ElicitOptionView {
    ElicitOptionView {
        label: label.into(),
        description: description.map(Into::into),
        flag: None,
    }
}

fn single_choice_field(options: Vec<ElicitOptionView>) -> ElicitFieldView {
    ElicitFieldView {
        key: "q0".into(),
        title: "下一步要我执行哪项？".into(),
        required: true,
        allow_custom_input: true,
        kind: ElicitFieldKindView::Select(options),
    }
}

/// 一段正在等用户回答的对话：输入框建好、焦点在输入框里，提问卡刚到。
fn ask_view(
    cx: &mut gpui::TestAppContext,
    card: PendingElicitation,
) -> (
    gpui::Entity<super::ConversationView>,
    &mut gpui::VisualTestContext,
    smol::channel::Receiver<smelt_plugin_api::model::acp_session::AcpUserAction>,
) {
    cx.update(init_ui);
    let (handle, actions) = crate::acp_client::ConversationClientHandle::detached();
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut view = super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: None,
                reason: "ask card".into(),
                resume_session_id: None,
                saved_sid: Some("acp-ask-card".into()),
            },
        );
        view.phase = DaemonPhase::WaitingForUser;
        view.handle = Some(handle);
        view.init_input(window, cx);
        view
    });
    cx.update(|window, cx| {
        let target = view.read(cx).focus_target(cx);
        window.focus(&target, cx);
    });
    draw_settled(cx);
    view.update(cx, |view, cx| {
        view.elicitation = Some(card);
        view.elicit_focus_move = Some(super::ElicitFocusMove::Arrive);
        view.schedule_surface_inputs(cx);
    });
    draw_settled(cx);
    (view, cx, actions)
}

fn sent(
    actions: &smol::channel::Receiver<smelt_plugin_api::model::acp_session::AcpUserAction>,
) -> Vec<String> {
    std::iter::from_fn(|| actions.try_recv().ok())
        .map(|action| format!("{action:?}"))
        .collect()
}

#[gpui::test]
fn a_single_choice_is_answered_by_one_click(cx: &mut gpui::TestAppContext) {
    let (view, cx, actions) = ask_view(
        cx,
        ask_card(vec![single_choice_field(vec![
            choice("按推荐方案", Some("两个仓库一起改")),
            choice("先不改", None),
        ])]),
    );
    assert!(
        cx.debug_bounds("acp-elicit-submit").is_none(),
        "单个单选题点了就是答案，不要底部的「提交」"
    );
    assert!(cx.debug_bounds("acp-elicit-custom-submit").is_none());

    let second = cx
        .debug_bounds("acp-elicit-opt-0-1")
        .expect("第二个选项要画出来");
    cx.simulate_click(second.center(), gpui::Modifiers::none());
    assert_eq!(
        sent(&actions),
        vec![
            "ElicitationChoose { field_ix: 0, opt_ix: 1 }",
            "ElicitationSubmit"
        ]
    );

    // 自己写了答案：在输入那一行里交。
    view.update_in(cx, |view, window, cx| {
        let input = view.elicitation_inputs[&0].clone();
        input.update(cx, |input, cx| input.set_value("灰度环境", window, cx));
    });
    draw_settled(cx);
    let submit = cx
        .debug_bounds("acp-elicit-custom-submit")
        .expect("写了答案就在输入行里给提交");
    cx.simulate_click(submit.center(), gpui::Modifiers::none());
    assert_eq!(
        sent(&actions),
        vec![
            "ElicitationText { field_ix: 0, value: \"灰度环境\" }",
            "ElicitationSubmit"
        ]
    );
}

#[gpui::test]
fn the_card_takes_the_keyboard_when_it_arrives(cx: &mut gpui::TestAppContext) {
    let (view, cx, actions) = ask_view(
        cx,
        ask_card(vec![single_choice_field(vec![
            choice("按推荐方案", None),
            choice("先不改", None),
        ])]),
    );
    cx.update(|window, cx| {
        assert!(
            view.read(cx).elicit_focus.is_focused(window),
            "卡片到了，输入框里没在写东西，键盘交给卡片"
        );
    });

    // ↓ 到第二个选项，再 ↓ 进「其他答案」的输入框，↑ 回到选项。
    cx.dispatch_action(super::ElicitDown);
    assert_eq!(view.read_with(cx, |view, _| view.elicit_cursor), 1);
    cx.dispatch_action(super::ElicitDown);
    cx.update(|window, cx| {
        let view = view.read(cx);
        assert!(
            view.elicitation_inputs[&0]
                .focus_handle(cx)
                .is_focused(window)
        );
    });
    cx.dispatch_action(gpui_base::input::MoveUp);
    cx.update(|window, cx| {
        let view = view.read(cx);
        assert_eq!(view.elicit_cursor, 1);
        assert!(view.elicit_focus.is_focused(window));
    });

    // 数字键直接选；回车选高亮的那项。
    cx.simulate_keystrokes("1");
    assert_eq!(
        sent(&actions),
        vec![
            "ElicitationChoose { field_ix: 0, opt_ix: 0 }",
            "ElicitationSubmit"
        ]
    );
    cx.dispatch_action(super::ElicitPick);
    assert_eq!(
        sent(&actions),
        vec![
            "ElicitationChoose { field_ix: 0, opt_ix: 0 }",
            "ElicitationSubmit"
        ]
    );

    // Esc 跳过这道题，不停整轮。
    cx.dispatch_action(super::ElicitSkip);
    assert_eq!(sent(&actions), vec!["ElicitationDismiss"]);

    // 卡片收走后焦点回到输入框。
    view.update(cx, |view, cx| {
        view.elicitation = None;
        view.elicitation_inputs.clear();
        view.elicit_focus_move = Some(super::ElicitFocusMove::Leave);
        view.schedule_surface_inputs(cx);
    });
    draw_settled(cx);
    cx.update(|window, cx| {
        let input = view.read(cx).input.clone().unwrap();
        assert!(input.focus_handle(cx).is_focused(window), "答完回到输入框");
    });
}

#[gpui::test]
fn a_half_written_message_keeps_the_keyboard(cx: &mut gpui::TestAppContext) {
    cx.update(init_ui);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let mut view = super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: None,
                reason: "ask card typing".into(),
                resume_session_id: None,
                saved_sid: Some("acp-ask-card-typing".into()),
            },
        );
        view.phase = DaemonPhase::WaitingForUser;
        view.init_input(window, cx);
        view
    });
    view.update_in(cx, |view, window, cx| {
        let input = view.input.clone().unwrap();
        input.update(cx, |input, cx| {
            input.set_value("写到一半", window, cx);
            input.focus(window, cx);
        });
        view.elicitation = Some(ask_card(vec![single_choice_field(vec![choice(
            "继续", None,
        )])]));
        view.elicit_focus_move = Some(super::ElicitFocusMove::Arrive);
        view.schedule_surface_inputs(cx);
    });
    draw_settled(cx);
    cx.update(|window, cx| {
        let input = view.read(cx).input.clone().unwrap();
        assert!(
            input.focus_handle(cx).is_focused(window),
            "输入框里写到一半，卡片不抢键盘"
        );
    });
}

#[gpui::test]
fn snapshot_binding_becomes_the_views_persisted_binding(cx: &mut gpui::TestAppContext) {
    use gpui::VisualTestContext;
    use smelt_plugin_api::model::conversation::{ConversationBinding, ConversationStateSnapshot};

    cx.update(init_ui);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Codex,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: None,
                reason: "test placeholder".into(),
                resume_session_id: None,
                saved_sid: Some("acp-binding-test".into()),
            },
        )
    });
    let cx: &mut VisualTestContext = cx;
    let mut snapshot = blank_snapshot();
    snapshot.snapshot_revision = 1;
    let binding = ConversationBinding::Automation {
        run_id: "run-1".into(),
    };
    snapshot.conversation_state = Some(ConversationStateSnapshot {
        binding: Some(binding.clone()),
        ..Default::default()
    });
    cx.update(|_window, cx| {
        view.update(cx, |view, cx| view.apply_snapshot(snapshot, cx));
    });

    assert_eq!(
        view.read_with(cx, |view, _| view.conversation_binding.clone()),
        binding
    );
}

#[gpui::test]
fn stop_button_click_requests_cancel_then_force_restarts_on_second_click(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(init_ui);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: None,
                reason: "test placeholder".into(),
                resume_session_id: None,
                saved_sid: None,
            },
        )
    });
    // 第一次点击：发起温和取消，记录 cancel_requested_at
    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            assert!(view.cancel_requested_at.is_none());
            view.on_stop_button_click(cx);
            assert!(view.cancel_requested_at.is_some());
            assert!(!view.restarting);
        });
    });

    // 第二次点击：进入 force_restart
    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.on_stop_button_click(cx);
            assert!(view.restarting);
        });
    });
}

#[test]
fn short_tool_log_stays_inline() {
    assert_eq!(super::tool_log_mode(0, true), super::ToolLogMode::Inline);
    assert_eq!(super::tool_log_mode(8, false), super::ToolLogMode::Inline);
    assert_eq!(super::tool_log_mode(8, true), super::ToolLogMode::Inline);
}

#[test]
fn long_tool_log_expands_into_a_bounded_viewport() {
    assert_eq!(super::tool_log_mode(9, false), super::ToolLogMode::Preview);
    assert_eq!(
        super::tool_log_mode(4_000, false),
        super::ToolLogMode::Preview
    );
    assert_eq!(
        super::tool_log_mode(4_000, true),
        super::ToolLogMode::Viewport
    );
    assert_eq!(
        super::tool_log_viewport_height(9),
        9. * super::TOOL_LOG_LINE_HEIGHT_PX
    );
    assert_eq!(
        super::tool_log_viewport_height(4_000),
        super::TOOL_LOG_VIEWPORT_LINES as f32 * super::TOOL_LOG_LINE_HEIGHT_PX
    );
}

#[test]
fn tool_log_preview_keeps_the_head_and_counts_every_line() {
    let body = "a\r\n\nb\n";
    assert_eq!(super::split_log_lines(body), vec!["a", "", "b"]);
    let long = (0..20)
        .map(|index| format!("l{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let preview = super::tool_log_preview(&long);
    assert_eq!(preview.total, 20);
    assert!(preview.truncated());
    assert_eq!(
        preview.shown.lines().count(),
        super::TOOL_OUTPUT_PREVIEW_LINES
    );
    assert!(preview.shown.starts_with("l0\n"));
    assert!(preview.shown.ends_with("l7"));
}

#[test]
fn widest_log_line_counts_wide_characters() {
    let lines = vec![
        "short".to_string(),
        "一二三四五六七八九十".to_string(),
        "abcdefghij".to_string(),
    ];
    assert_eq!(super::widest_log_line_index(&lines), 1);
}

#[gpui::test]
fn expanded_tool_log_viewport_stays_within_its_cap(cx: &mut gpui::TestAppContext) {
    use gpui::VisualTestContext;

    let log = (0..200)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let entries = vec![
        AcpEntry::User("跑一下".into()),
        AcpEntry::ToolCall {
            id: "log-1".into(),
            title: "bash".into(),
            kind: ToolKind::Execute,
            status: ToolCallStatus::Completed,
            output: vec![ToolOutputPart::Text(log)],
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::Assistant {
            text: "好了".into(),
            thought: false,
        },
    ];
    let layout = super::build_conversation_layout(&entries, false);
    let group_ix = layout
        .iter()
        .find_map(|entry| entry.process_group.map(|group| group.first))
        .expect("tool output belongs to a process group");

    cx.update(init_ui);
    let (_view, cx) = cx.add_window_view(move |_window, cx| {
        let mut view = super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: Some("/tmp/smelt".into()),
                reason: "log viewport".into(),
                resume_session_id: None,
                saved_sid: Some("acp-log-viewport".into()),
            },
        );
        view.apply_snapshot(tail_snapshot(0, 0, entries, DaemonPhase::Idle), cx);
        view.expanded_process_groups.insert((group_ix, false));
        view.expanded_tool_cards.insert("log-1".into());
        view.expanded_tools.insert("log-1".into());
        view
    });
    let cx: &mut VisualTestContext = cx;
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let bounds = cx
        .debug_bounds("ACP_TOOL_LOG_log-1_0")
        .expect("expanded log viewport should paint");
    let cap = gpui::px(super::tool_log_viewport_height(200));
    assert!(
        (bounds.size.height - cap).abs() < gpui::px(1.),
        "log viewport grew with the full output: {:?} cap {cap:?}",
        bounds.size.height,
    );
}

/// 紧凑工具行点开的卡片以前只放前 8 行，再标「共 N 行」。卡片挡住后面的列表，
/// 滚轮落在卡片上时两边都不动。长日志必须在卡片自己的行视口里能滚到后面的行。
#[gpui::test]
fn tool_output_popover_scrolls_past_the_preview(cx: &mut gpui::TestAppContext) {
    use gpui::{ScrollDelta, TouchPhase, VisualTestContext, point};

    let log = (0..50)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let entries = vec![
        AcpEntry::User("看一下".into()),
        AcpEntry::ToolCall {
            id: "read-1".into(),
            title: "managed_runtime.rs".into(),
            kind: ToolKind::Read,
            status: ToolCallStatus::Completed,
            output: vec![ToolOutputPart::Text(log)],
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
    ];
    let layout = super::build_conversation_layout(&entries, false);
    let group_ix = layout
        .iter()
        .find_map(|entry| entry.process_group.map(|group| group.first))
        .expect("tool belongs to a process group");

    cx.update(init_ui);
    let (_view, cx) = cx.add_window_view(move |_window, cx| {
        let mut view = super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: Some("/tmp/smelt".into()),
                reason: "popover scroll".into(),
                resume_session_id: None,
                saved_sid: Some("acp-pop-scroll".into()),
            },
        );
        view.apply_snapshot(tail_snapshot(0, 0, entries, DaemonPhase::Idle), cx);
        view.expanded_process_groups.insert((group_ix, false));
        view
    });
    let cx: &mut VisualTestContext = cx;
    draw_settled(cx);

    let row = cx
        .debug_bounds("ACP_TOOL_COMPACT_1")
        .expect("completed tool should paint as a compact row");
    cx.simulate_click(row.center(), Default::default());
    // 弹层要等触发器量完边界才挂上内容。
    draw_settled(cx);
    draw_settled(cx);

    let viewport = cx
        .debug_bounds("ACP_TOOL_POP_LOG_text_read-1_0")
        .expect("popover log viewport should paint");
    assert!(
        cx.debug_bounds("ACP_TOOL_POP_LINE_read-1_0_0").is_some(),
        "viewport should paint the first line"
    );
    assert!(
        cx.debug_bounds("ACP_TOOL_POP_LINE_read-1_0_40").is_none(),
        "line 40 is past the first screen and must not be built yet"
    );

    cx.simulate_event(gpui::ScrollEvent {
        position: viewport.center(),
        delta: ScrollDelta::Lines(point(0., -40.)),
        modifiers: Default::default(),
        touch_phase: TouchPhase::Moved,
        source: gpui::ScrollSource::Pointer,
    });
    draw_settled(cx);

    assert!(
        cx.debug_bounds("ACP_TOOL_POP_LINE_read-1_0_40").is_some(),
        "scrolling the popover should reveal later lines"
    );
}

fn draw_settled(cx: &mut gpui::VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

/// 桌面先画一帧空列表（实时帧省略已提交历史），再把第一页补进来。
/// 消息列表是缓存视图，内容按根节点单独排。根节点只有 `flex_1` 时高度是 0，
/// 欢迎语因条目非空而消失，舞台就只剩一块黑。
#[gpui::test]
fn first_history_page_paints_after_the_empty_open_frame(cx: &mut gpui::TestAppContext) {
    use gpui::VisualTestContext;

    cx.update(init_ui);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        let mut view = super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: Some("/tmp/smelt".into()),
                reason: "empty open frame".into(),
                resume_session_id: None,
                saved_sid: Some("acp-empty-then-history".into()),
            },
        );
        view.phase = DaemonPhase::Idle;
        view.loaded_entries_offset = 1;
        view.entries_total = 1;
        view
    });
    let cx: &mut VisualTestContext = cx;
    draw_settled(cx);

    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.entries = vec![AcpEntry::User("已经写好的那一句".into())];
            view.loaded_entries_offset = 0;
            view.rendered_markdown = build_markdown_cache(&view.entries);
            super::reveal_first_history_page(&view.list_state, view.entries.len());
            cx.notify();
        });
    });
    draw_settled(cx);

    let viewport = view.read_with(cx, |view, _| view.list_state.viewport_bounds().size.height);
    assert!(
        viewport > px(100.),
        "补上历史后列表视口仍是 {viewport:?}，条目没有地方可画"
    );
    let bubble = cx
        .debug_bounds("ACP_USER_MESSAGE_0")
        .expect("补上的第一页历史要画出来");
    assert!(
        bubble.size.height > px(0.),
        "用户消息画出来了但高度是 0: {bubble:?}"
    );
}

/// 前一条回答超过 4KB，markdown 走后台解析，滚出屏幕后再画会先以空内容出现。
/// 跳回不能拿它的高度定位，否则提问会被解析完长出来的正文挤走。
#[gpui::test]
fn jumping_to_a_prompt_parks_it_below_the_title_island(cx: &mut gpui::TestAppContext) {
    use gpui::{ListOffset, VisualTestContext};

    let long_reply = (0..300)
        .map(|line| format!("第 {line} 段回答"))
        .collect::<Vec<_>>()
        .join("\n\n");
    assert!(long_reply.len() > 4 * 1024);
    let entries = vec![
        AcpEntry::User("第一问".into()),
        AcpEntry::Assistant {
            text: long_reply.clone(),
            thought: false,
        },
        AcpEntry::User("  第二问\n  换行  ".into()),
        AcpEntry::Assistant {
            text: long_reply,
            thought: false,
        },
    ];
    cx.update(init_ui);
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        let mut view = super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: Some("/tmp/smelt".into()),
                reason: "pinned prompt".into(),
                resume_session_id: None,
                saved_sid: Some("acp-pinned-prompt".into()),
            },
        );
        view.apply_snapshot(tail_snapshot(0, 0, entries, DaemonPhase::Idle), cx);
        view
    });
    let cx: &mut VisualTestContext = cx;
    draw_settled(cx);
    draw_settled(cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.pinned_prompt())
            .as_deref(),
        Some("第二问 换行"),
        "跟着尾部读回答时，这一问已经整条滚出视口"
    );

    cx.update(|_window, cx| view.update(cx, |view, cx| view.jump_to_prompt(2, cx)));
    draw_settled(cx);
    draw_settled(cx);
    let viewport = view.read_with(cx, |view, _| view.list_state.viewport_bounds());
    let bubble = cx
        .debug_bounds("ACP_USER_MESSAGE_2")
        .expect("跳回后第二问要画出来");
    assert_eq!(
        bubble.top() - viewport.top(),
        px(ui_theme::CHROME_ISLAND_CLEARANCE_PX),
        "提问停在标题胶囊下面"
    );
    view.read_with(cx, |view, _| {
        assert!(view.viewing_history, "跳回后要能回到最新");
        assert_eq!(view.pinned_prompt().as_deref(), Some("第一问"));
    });

    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.list_state.scroll_to(ListOffset {
                item_ix: 3,
                offset_in_item: px(0.),
            });
            cx.notify();
        })
    });
    draw_settled(cx);
    view.read_with(cx, |view, _| {
        assert_eq!(view.revealed_prompt, None, "滚过这一问后留白收起");
        assert_eq!(view.pinned_prompt().as_deref(), Some("第二问 换行"));
    });
}

#[gpui::test]
fn background_task_popover_closes_when_the_last_task_finishes(cx: &mut gpui::TestAppContext) {
    cx.update(init_ui);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: None,
                reason: "background tasks".into(),
                resume_session_id: None,
                saved_sid: Some("acp-background-popover".into()),
            },
        )
    });
    let running = BackgroundTaskView {
        id: "bg-1".into(),
        title: "dev server".into(),
        command: "npm run dev".into(),
        status: BackgroundTaskStatus::Running,
        exit_code: None,
        output: String::new(),
        started_at_ms: 1,
        finished_at_ms: None,
    };
    let mut snapshot = blank_snapshot();
    snapshot.snapshot_revision = 1;
    snapshot.background_tasks = vec![running.clone()];
    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.apply_snapshot(snapshot.clone(), cx);
            view.background_tasks_open = true;
        })
    });

    snapshot.snapshot_revision = 2;
    snapshot.background_tasks = vec![BackgroundTaskView {
        status: BackgroundTaskStatus::Completed,
        finished_at_ms: Some(2),
        ..running
    }];
    cx.update(|_window, cx| view.update(cx, |view, cx| view.apply_snapshot(snapshot, cx)));
    assert!(
        !view.read_with(cx, |view, _| view.background_tasks_open),
        "任务都结束后弹层要收起，下一个任务开始时不能自己弹出来"
    );
}

fn numbered_paragraphs(label: &str, count: usize) -> String {
    (0..count)
        .map(|n| format!("{label} 第 {n} 段"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn conversation_fixture(
    sid: &str,
    entries: Vec<AcpEntry>,
    phase: DaemonPhase,
    cx: &mut gpui::Context<super::ConversationView>,
) -> super::ConversationView {
    let mut view = super::ConversationView::placeholder(
        cx,
        super::ConversationOrigin {
            runtime: offline_runtime(),
            launch_prep: None,
            agent: ConversationAgentKind::Pi,
            launch: ConversationLaunchSpec::from_command("true"),
            profile_id: None,
            cwd: Some("/tmp/smelt".into()),
            reason: "scroll".into(),
            resume_session_id: None,
            saved_sid: Some(sid.into()),
        },
    );
    // 版本 0 的快照总会收下，后面按版本递增发的尾片照常合并。
    view.apply_snapshot(tail_snapshot(0, 0, entries, phase), cx);
    view.awaiting_initial_history_snapshot = false;
    view
}

/// 打开一个已有消息的会话视图。不等首帧快照，后续快照按增量尾片合并。
fn open_conversation<'a>(
    cx: &'a mut gpui::TestAppContext,
    sid: &'static str,
    entries: Vec<AcpEntry>,
    phase: DaemonPhase,
) -> (
    gpui::Entity<super::ConversationView>,
    &'a mut gpui::VisualTestContext,
) {
    cx.update(init_ui);
    let (view, cx) =
        cx.add_window_view(move |_window, cx| conversation_fixture(sid, entries, phase, cx));
    (view, cx)
}

/// 对话视图挂在 `Root` 下打开，弹窗这类窗口层才装得上。
fn open_conversation_in_root<'a>(
    cx: &'a mut gpui::TestAppContext,
    sid: &'static str,
    entries: Vec<AcpEntry>,
    phase: DaemonPhase,
) -> (
    gpui::Entity<super::ConversationView>,
    &'a mut gpui::VisualTestContext,
) {
    use gpui::AppContext as _;
    cx.update(init_ui);
    let mut view = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let conversation = cx.new(|cx| conversation_fixture(sid, entries, phase, cx));
        view = Some(conversation.clone());
        gpui_base::Root::new(conversation, window, cx)
    });
    (view.expect("窗口建好时对话视图已建"), cx)
}

/// 正式窗口的根上有文字选择层，拖选才登记得到。测试把对话视图直接当窗口根时没有这一层。
struct SelectingConversation {
    view: gpui::Entity<super::ConversationView>,
}

impl gpui::Render for SelectingConversation {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{ParentElement, Styled, div};
        div()
            .relative()
            .size_full()
            .child(gpui_base::TextSelectionLayer)
            .child(self.view.clone())
    }
}

fn open_conversation_with_text_selection<'a>(
    cx: &'a mut gpui::TestAppContext,
    sid: &'static str,
    entries: Vec<AcpEntry>,
    phase: DaemonPhase,
) -> (
    gpui::Entity<super::ConversationView>,
    &'a mut gpui::VisualTestContext,
) {
    use gpui::AppContext as _;
    cx.update(init_ui);
    let (host, cx) = cx.add_window_view(move |_window, cx| SelectingConversation {
        view: cx.new(|cx| conversation_fixture(sid, entries, phase, cx)),
    });
    let view = host.read_with(cx, |host, _| host.view.clone());
    (view, cx)
}

/// 守护刚接上、什么都还没有的一份快照（只填协议里必填的字段）。
fn blank_snapshot() -> smelt_plugin_api::model::acp_session::ConversationSnapshot {
    serde_json::from_value(serde_json::json!({
        "entries": [],
        "phase": "connecting",
        "supports_image": false,
        "available_commands": [],
        "config_options": [],
        "completed_unread": false,
        "should_persist": false,
    }))
    .expect("最小快照应能解析")
}

fn tail_snapshot(
    revision: u64,
    entries_offset: usize,
    entries: Vec<AcpEntry>,
    phase: DaemonPhase,
) -> smelt_plugin_api::model::acp_session::ConversationSnapshot {
    let mut snapshot = blank_snapshot();
    snapshot.snapshot_revision = revision;
    snapshot.entries_total = entries_offset + entries.len();
    snapshot.entries_offset = entries_offset;
    snapshot.entries = entries;
    snapshot.phase = phase;
    snapshot
}

fn wheel(position: gpui::Point<gpui::Pixels>, up: gpui::Pixels) -> gpui::ScrollEvent {
    gpui::ScrollEvent {
        position,
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), up)),
        modifiers: Default::default(),
        touch_phase: gpui::TouchPhase::Moved,
        source: gpui::ScrollSource::Pointer,
    }
}

/// 拖选文字时，上游按最近的裁剪区判断指针是否贴边，贴边就合成滚轮滚动列表。
/// 消息行若自己裁剪，一条短回答的行就成了这块区域：上下 16px 的触发带盖满整行，
/// 在里面横着拖也会让整个列表跟着滚。最近的裁剪区必须是列表视口。
///
/// 文字选择层装在应用根上，不在对话视图里。这里把同一层装进测试窗口，拖选才走正式路径。
#[gpui::test]
fn dragging_a_selection_across_a_short_answer_does_not_scroll_the_list(
    cx: &mut gpui::TestAppContext,
) {
    let long_answer = || AcpEntry::Assistant {
        text: numbered_paragraphs("长回答", 40),
        thought: false,
    };
    let short_text = "这是一条只有一行的短回答，横着拖选它不该滚动列表。";
    let (view, cx) = open_conversation_with_text_selection(
        cx,
        "acp-drag-select-short-answer",
        vec![
            AcpEntry::User("第一问".into()),
            long_answer(),
            AcpEntry::User("第二问".into()),
            AcpEntry::Assistant {
                text: short_text.into(),
                thought: false,
            },
            AcpEntry::User("第三问".into()),
            long_answer(),
        ],
        DaemonPhase::Succeeded,
    );
    draw_settled(cx);
    draw_settled(cx);

    // 第二问顶到视口上沿，短回答落在它下面，离视口上下沿都远。
    let short_answer = 3;
    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.list_state.scroll_to(gpui::ListOffset {
                item_ix: short_answer - 1,
                offset_in_item: px(0.),
            });
            cx.notify();
        })
    });
    draw_settled(cx);
    draw_settled(cx);

    let (viewport, row, before) = view.read_with(cx, |view, _| {
        (
            view.list_state.viewport_bounds(),
            view.list_state.bounds_for_item(short_answer),
            view.list_state.logical_scroll_top(),
        )
    });
    let row = row.expect("短回答在视口里画过");
    assert!(
        row.top() > viewport.top() + px(40.) && row.bottom() < viewport.bottom() - px(40.),
        "短回答要离视口上下沿都远: row={row:?} viewport={viewport:?}"
    );

    let column = ui_theme::conversation_max_width().min(viewport.size.width - px(32.));
    let left = viewport.center().x - column * 0.5;
    let y = row.top() + px(8.);
    cx.simulate_mouse_down(
        gpui::point(left + px(2.), y),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    for step in 1..=4 {
        cx.simulate_mouse_move(
            gpui::point(left + px(60. * step as f32), y),
            Some(gpui::MouseButton::Left),
            gpui::Modifiers::default(),
        );
    }
    // 自动滚动每 16ms 一跳；给它足够的时间跳上好几次。
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(300));
    draw_settled(cx);

    let selected = cx.update(gpui_base::TextSelection::selected_text);
    let selected = selected.trim();
    assert!(
        !selected.is_empty() && short_text.contains(selected),
        "拖动要真的选中了短回答里的字: {selected:?}"
    );
    let after = view.read_with(cx, |view, _| view.list_state.logical_scroll_top());
    assert_eq!(
        (after.item_ix, after.offset_in_item),
        (before.item_ix, before.offset_in_item),
        "横着拖选没贴视口边，列表不该滚"
    );

    cx.simulate_mouse_up(
        gpui::point(left + px(240.), y),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
}

/// 流式正文每来一段就改一次最后一条。快照落地到下一帧画出来之间，滚轮事件照常
/// 进来，按上一帧的位置换算。改动的行这时不能变成没有高度的新行：总高度少了
/// 这一截，滚动位置会被夹到很靠上的地方，视口一下跳走。
#[gpui::test]
fn wheel_between_a_streaming_snapshot_and_the_next_frame_moves_by_its_delta(
    cx: &mut gpui::TestAppContext,
) {
    let answer = |paragraphs| AcpEntry::Assistant {
        text: numbered_paragraphs("回答", paragraphs),
        thought: false,
    };
    let (view, cx) = open_conversation(
        cx,
        "acp-streaming-wheel",
        vec![AcpEntry::User("问".into()), answer(80)],
        DaemonPhase::Thinking,
    );
    draw_settled(cx);
    draw_settled(cx);
    let viewport = view.read_with(cx, |view, _| view.list_state.viewport_bounds());
    assert!(
        viewport.size.height > px(100.),
        "列表要有视口: {viewport:?}"
    );

    cx.simulate_event(wheel(viewport.center(), px(200.)));
    draw_settled(cx);
    let before = view.read_with(cx, |view, _| {
        assert!(
            !view.list_state.is_following_tail(),
            "往上滚过就不再跟随最新"
        );
        view.list_state.logical_scroll_top()
    });
    assert_eq!(before.item_ix, 1, "还停在回答里");
    assert!(before.offset_in_item > px(30.), "回答比视口高: {before:?}");

    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.apply_snapshot(
                tail_snapshot(1, 1, vec![answer(81)], DaemonPhase::Thinking),
                cx,
            )
        });
        window.dispatch_event(
            gpui::PlatformInput::Scroll(wheel(viewport.center(), px(30.))),
            cx,
        );
    });
    draw_settled(cx);

    let after = view.read_with(cx, |view, _| view.list_state.logical_scroll_top());
    assert_eq!(after.item_ix, before.item_ix, "滚轮只走了 30px: {after:?}");
    assert_eq!(
        after.offset_in_item,
        before.offset_in_item - px(30.),
        "滚轮只走了 30px"
    );
}

/// 上一回合结束后，它的过程收成一行摘要，其余行高度变成 0。这些行早就
/// 滚出了视口，列表里还记着展开时的高度；往回滚时按旧高度换算，内容一顿一顿地
/// 往回跳，滚不上去。
#[gpui::test]
fn scrolling_back_over_a_collapsed_turn_moves_by_the_wheel_delta(cx: &mut gpui::TestAppContext) {
    let kinds = [
        ToolKind::Read,
        ToolKind::Edit,
        ToolKind::Execute,
        ToolKind::Search,
    ];
    let mut first_turn = vec![AcpEntry::User("第一问".into())];
    first_turn.extend((0..16).map(|n| {
        completed_tool(
            &format!("tool-{n}"),
            kinds[n % kinds.len()],
            &format!("步骤 {n}"),
        )
    }));
    let first_turn_len = first_turn.len();
    let (view, cx) = open_conversation(
        cx,
        "acp-collapsed-turn-wheel",
        first_turn,
        DaemonPhase::Thinking,
    );
    draw_settled(cx);
    draw_settled(cx);
    assert!(
        cx.debug_bounds("ACP_TOOL_COMPACT_2").is_none(),
        "回合进行中，一段默认只露一行计数"
    );
    // 用户点开了在跑的这段：工具逐行展开，列表量下的是展开时的高度。
    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.expanded_process_groups.insert((1, true));
            view.list_state.remeasure();
            cx.notify();
        })
    });
    draw_settled(cx);
    draw_settled(cx);
    assert!(
        cx.debug_bounds("ACP_TOOL_COMPACT_2").is_some(),
        "展开后工具逐行显示"
    );

    let second_prompt = first_turn_len + 1;
    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.apply_snapshot(
                tail_snapshot(
                    1,
                    first_turn_len,
                    vec![
                        AcpEntry::Assistant {
                            text: "第一问的回答".into(),
                            thought: false,
                        },
                        AcpEntry::User("第二问".into()),
                        AcpEntry::Assistant {
                            text: numbered_paragraphs("答二", 120),
                            thought: false,
                        },
                    ],
                    DaemonPhase::Thinking,
                ),
                cx,
            )
        })
    });
    draw_settled(cx);
    draw_settled(cx);
    assert!(
        cx.debug_bounds("ACP_TOOL_COMPACT_2").is_none(),
        "第一回合结束后工具收起，而且已经滚出视口"
    );

    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.list_state.scroll_to(gpui::ListOffset {
                item_ix: second_prompt + 1,
                offset_in_item: px(0.),
            });
            cx.notify();
        })
    });
    draw_settled(cx);

    let viewport = view.read_with(cx, |view, _| view.list_state.viewport_bounds());
    assert_eq!(second_prompt, 18);
    let selector = "ACP_USER_MESSAGE_18";
    let step = px(40.);
    let mut compared = 0;
    for _ in 0..12 {
        let before = cx.debug_bounds(selector);
        cx.simulate_event(wheel(viewport.center(), step));
        draw_settled(cx);
        let after = cx.debug_bounds(selector);
        let top = view.read_with(cx, |view, _| view.list_state.logical_scroll_top());
        if top.item_ix == 0 && top.offset_in_item == px(0.) {
            // 到顶了，剩下的距离不够一整步。
            break;
        }
        if let (Some(before), Some(after)) = (before, after)
            && before.top() < viewport.bottom()
            && after.top() < viewport.bottom()
        {
            let moved = after.top() - before.top();
            assert!(
                (moved - step).abs() < px(1.),
                "第 {compared} 次比对：滚轮往上 {step:?}，第二问却移动了 {moved:?}"
            );
            compared += 1;
        }
    }
    assert!(
        compared >= 5,
        "第二问要在视口里停留多次比对，实际 {compared} 次"
    );
    assert!(
        cx.debug_bounds("ACP_PROCESS_GROUP_1").is_some(),
        "滚到第一回合，看得到收起后的那一段"
    );
}

#[test]
fn closing_a_turn_remeasures_the_rows_its_process_group_folds() {
    let mut entries = vec![AcpEntry::User("第一问".into())];
    entries.extend((0..4).map(|n| {
        let kind = if n % 2 == 0 {
            ToolKind::Read
        } else {
            ToolKind::Edit
        };
        completed_tool(&format!("tool-{n}"), kind, &format!("步骤 {n}"))
    }));
    entries.push(AcpEntry::Assistant {
        text: "回答".into(),
        thought: false,
    });
    let running = build_conversation_layout(&entries, true);
    let closed = build_conversation_layout(&entries, false);
    assert_eq!(
        rows_with_changed_shape(&running, 0, &closed, 0, 0),
        vec![1..6],
        "工具行收起、回答成为最终回答，都要重新量"
    );
    assert!(
        rows_with_changed_shape(&closed, 0, &closed, 0, 0).is_empty(),
        "外观没变的行不打扰列表"
    );
}

#[test]
fn prepending_history_does_not_remeasure_unchanged_rows() {
    let turn = |label: &str| {
        vec![
            AcpEntry::User(label.into()),
            completed_tool(&format!("{label}-read"), ToolKind::Read, "a.rs"),
            completed_tool(&format!("{label}-edit"), ToolKind::Edit, "a.rs"),
            AcpEntry::Assistant {
                text: "回答".into(),
                thought: false,
            },
        ]
    };
    let later = turn("后一问");
    let mut all = turn("前一问");
    all.extend(later.clone());
    let before = build_conversation_layout(&later, false);
    let after = build_conversation_layout(&all, false);
    assert!(
        rows_with_changed_shape(&before, 4, &after, 0, 0).is_empty(),
        "往前补一页，已有的行只是整体后移"
    );
}

/// 超过 4KB 的回答在后台解析。解析结果挂在元素上，行一滚出列表的渲染范围就跟着
/// 丢掉，滚回来先以空内容出现、解析完才撑开。往回滚经过长回答时，内容会被它
/// 一下顶开，往回跳。
#[gpui::test]
fn scrolling_back_over_a_long_answer_moves_by_the_wheel_delta(cx: &mut gpui::TestAppContext) {
    let long_answer = |label: &str| AcpEntry::Assistant {
        text: numbered_paragraphs(label, 400),
        thought: false,
    };
    let entries = vec![
        AcpEntry::User("第一问".into()),
        long_answer("答一"),
        AcpEntry::User("第二问".into()),
        long_answer("答二"),
    ];
    assert!(numbered_paragraphs("答一", 400).len() > 4 * 1024);
    let (view, cx) = open_conversation(cx, "acp-long-answer-wheel", entries, DaemonPhase::Idle);
    draw_settled(cx);
    draw_settled(cx);

    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.list_state.scroll_to(gpui::ListOffset {
                item_ix: 3,
                offset_in_item: px(0.),
            });
            cx.notify();
        })
    });
    draw_settled(cx);
    draw_settled(cx);

    let viewport = view.read_with(cx, |view, _| view.list_state.viewport_bounds());
    let selector = "ACP_USER_MESSAGE_2";
    let step = px(40.);
    let mut compared = 0;
    for _ in 0..10 {
        let before = cx.debug_bounds(selector);
        cx.simulate_event(wheel(viewport.center(), step));
        draw_settled(cx);
        let after = cx.debug_bounds(selector);
        if let (Some(before), Some(after)) = (before, after)
            && before.top() < viewport.bottom()
            && after.top() < viewport.bottom()
        {
            let moved = after.top() - before.top();
            assert!(
                (moved - step).abs() < px(1.),
                "第 {compared} 次比对：滚轮往上 {step:?}，第二问却移动了 {moved:?}"
            );
            compared += 1;
        }
    }
    assert!(
        compared >= 5,
        "第二问要在视口里停留多次比对，实际 {compared} 次"
    );
}

#[gpui::test]
fn dropped_session_releases_its_view(cx: &mut gpui::TestAppContext) {
    use gpui::AppContext;

    let view = cx.new(|cx| {
        super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Codex,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: None,
                reason: "test placeholder".into(),
                resume_session_id: None,
                saved_sid: Some("acp-release-test".into()),
            },
        )
    });
    let weak = view.downgrade();
    drop(view);
    assert!(
        weak.upgrade().is_none(),
        "关掉会话后消息流不能再把 ConversationView 留住"
    );
}

/// 空闲会话重开时，首帧不带已提交的正文（桌面连接只订阅进行中的一轮），正文靠往前
/// 翻页补上。补上之后就不再是在等首帧：之后的帧（后台任务多了一行输出、已读状态变了）
/// 只带快照头，按增量合并，不能把翻出来的对话整个换成那一帧的空尾片。
#[gpui::test]
fn frames_after_the_first_history_page_keep_the_loaded_conversation(cx: &mut gpui::TestAppContext) {
    cx.update(init_ui);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: Some("/tmp/smelt".into()),
                reason: "restored".into(),
                resume_session_id: None,
                saved_sid: Some("acp-idle-reopen".into()),
            },
        )
    });
    let history = vec![
        AcpEntry::User("截一张图给我看".into()),
        AcpEntry::Assistant {
            text: "图在 http://example.com/shot.png".into(),
            thought: false,
        },
    ];
    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.apply_snapshot(tail_snapshot(1, 2, Vec::new(), DaemonPhase::Idle), cx);
            view.prepend_history_page(tail_snapshot(0, 0, history.clone(), DaemonPhase::Idle), cx);
            view.apply_snapshot(tail_snapshot(2, 2, Vec::new(), DaemonPhase::Idle), cx);
        });
    });

    view.read_with(cx, |view, _| {
        assert_eq!(view.entries, history, "只带快照头的帧不能清空已翻出的对话");
        assert_eq!(view.loaded_entries_offset, 0);
    });
}

/// 测试里的视图不连任何运行时：一连就失败，和运行时离线时一样。
fn offline_runtime() -> smelt_plugin_api::Runtime {
    smelt_plugin_api::Runtime::new(
        smelt_plugin_api::RuntimeKey::Remote("test".to_string()),
        smelt_plugin_api::client::RuntimeClient::new(|| {
            Err(std::io::Error::other("测试里不连运行时"))
        }),
    )
}

fn subagent_view(id: &str, status: SubagentStatus) -> SubagentView {
    SubagentView {
        id: id.into(),
        tool_call_id: "call-1".into(),
        parent_session_id: "pi-1".into(),
        title: "审计".into(),
        agent: "reviewer".into(),
        model: None,
        background: true,
        status,
        started_at_ms: 1_000,
        finished_at_ms: (!status.is_running()).then_some(61_000),
        turns: 3,
        tool_calls: 5,
        last_activity: Some("bash rg".into()),
        revision: 4,
    }
}

fn dispatched(id: &str, status: SubagentStatus) -> smelt_plugin_api::model::acp_chat::SubagentRun {
    smelt_plugin_api::model::acp_chat::SubagentRun {
        id: id.into(),
        title: "审计".into(),
        agent: "reviewer".into(),
        status,
    }
}

/// 后台派发的工具调用一启动就「完成」。子智能体失败了，卡片要说失败，不能打勾。
#[test]
fn a_failed_background_subagent_is_shown_as_failed() {
    let run = dispatched("sa-1", SubagentStatus::Running);
    let live = [subagent_view("sa-1", SubagentStatus::Failed)];
    let status = subagent_status(&run, &live, false);
    assert_eq!(status, SubagentStatus::Failed);
    let progress = dispatch_progress(&[status], None);
    assert_eq!(progress.text, "Failed");
    assert!(progress.problem);
    assert!(!progress.running);
}

/// 宿主报的实时状态优先；没有实时记录时用卡片记下的（结果消息定下的）；记下的还在跑、
/// 派发也结束了，宿主却不再报它：宿主在它跑完前退出过，是已中断。
#[test]
fn a_subagent_without_live_state_uses_its_recorded_status_or_is_interrupted() {
    let live = [subagent_view("sa-1", SubagentStatus::Running)];
    assert_eq!(
        subagent_status(&dispatched("sa-1", SubagentStatus::Running), &live, false),
        SubagentStatus::Running
    );
    assert_eq!(
        subagent_status(&dispatched("sa-2", SubagentStatus::Completed), &live, false),
        SubagentStatus::Completed,
        "重新加载后由结果消息定下的状态"
    );
    assert_eq!(
        subagent_status(&dispatched("sa-3", SubagentStatus::Running), &live, false),
        SubagentStatus::Interrupted
    );
    assert_eq!(
        subagent_status(&dispatched("sa-3", SubagentStatus::Running), &[], true),
        SubagentStatus::Running,
        "前台派发还在跑时，宿主的记录可能还没到"
    );
}

#[test]
fn a_dispatch_line_summarizes_its_subagents() {
    use SubagentStatus::*;
    assert_eq!(
        dispatch_progress(&[Running], Some("bash rg")).text,
        "Running · bash rg"
    );
    let parallel = dispatch_progress(&[Completed, Running, Running], None);
    assert_eq!(parallel.text, "1/3 done");
    assert!(parallel.running);
    assert_eq!(
        dispatch_progress(&[Completed, Completed], None).text,
        "All done"
    );
    let mixed = dispatch_progress(&[Completed, Failed, Interrupted, Stopped], None);
    assert_eq!(
        mixed.text,
        "1/4 done · 1 failed · 1 interrupted · 1 stopped"
    );
    assert!(mixed.problem);
    assert!(
        !dispatch_progress(&[Stopped, Completed], None).problem,
        "被叫停不是问题"
    );
}

#[test]
fn a_dispatch_runs_from_its_first_start_to_its_last_finish() {
    let first = subagent_view("sa-1", SubagentStatus::Completed);
    let mut second = subagent_view("sa-2", SubagentStatus::Completed);
    second.started_at_ms = 5_000;
    second.finished_at_ms = Some(90_000);
    assert_eq!(
        dispatch_elapsed_ms(&[&first, &second], 200_000),
        Some(89_000)
    );
    let running = subagent_view("sa-3", SubagentStatus::Running);
    assert_eq!(
        dispatch_elapsed_ms(&[&first, &running], 200_000),
        Some(199_000)
    );
    assert_eq!(dispatch_elapsed_ms(&[], 200_000), None);
}

#[test]
fn a_subagent_row_is_named_and_timed_by_its_own_record() {
    let mut run = dispatched("sa-1", SubagentStatus::Running);
    assert_eq!(subagent_run_title(&run, "2 个并行任务"), "审计");
    run.title = "  ".into();
    assert_eq!(subagent_run_title(&run, "迁移数据库"), "迁移数据库");

    let finished = subagent_view("sa-1", SubagentStatus::Completed);
    assert_eq!(subagent_elapsed_ms(&finished, 500_000), 60_000);
    let running = subagent_view("sa-2", SubagentStatus::Running);
    assert_eq!(subagent_elapsed_ms(&running, 31_000), 30_000);
    assert_eq!(
        running_subagents(&[finished, running])
            .iter()
            .map(|view| view.id.as_str())
            .collect::<Vec<_>>(),
        ["sa-2"],
        "输入栏只数在跑的"
    );
}

/// 过程组里跑完的长思考只露出一行摘要，必须能点开看全文。
#[gpui::test]
fn long_thought_inside_a_process_group_can_be_expanded(cx: &mut gpui::TestAppContext) {
    use gpui::VisualTestContext;

    cx.update(init_ui);
    let long_thought = format!(
        "I found a key clue: a similar project reported this exact issue two days ago. {}",
        "The fix is to retry with a fresh identity. ".repeat(4)
    );
    let entries = vec![
        AcpEntry::User("查一下".into()),
        AcpEntry::Assistant {
            text: "先看看".into(),
            thought: false,
        },
        AcpEntry::ToolCall {
            id: "search-1".into(),
            title: "issues".into(),
            kind: ToolKind::Search,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::Assistant {
            text: long_thought,
            thought: true,
        },
        AcpEntry::ToolCall {
            id: "read-1".into(),
            title: "notes.md".into(),
            kind: ToolKind::Read,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
            subagents: Vec::new(),
            peer: None,
            asked: None,
        },
        AcpEntry::Assistant {
            text: "结论".into(),
            thought: false,
        },
    ];
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        let mut view = super::ConversationView::placeholder(
            cx,
            super::ConversationOrigin {
                runtime: offline_runtime(),
                launch_prep: None,
                agent: ConversationAgentKind::Claude,
                launch: ConversationLaunchSpec::from_command("true"),
                profile_id: None,
                cwd: Some("/tmp/smelt".into()),
                reason: "visual test".into(),
                resume_session_id: None,
                saved_sid: Some("acp-process-thought-expand".into()),
            },
        );
        view.apply_snapshot(tail_snapshot(0, 0, entries, DaemonPhase::Idle), cx);
        view.expanded_turn_folds.insert(1);
        view.expanded_process_groups.insert((2, false));
        view
    });
    let cx: &mut VisualTestContext = cx;
    draw_settled(cx);

    let row = cx
        .debug_bounds("ACP_PROGRESS_3")
        .expect("thought summary should paint in the process group");
    cx.simulate_click(row.center(), Default::default());
    draw_settled(cx);

    assert!(
        view.read_with(cx, |view, _| view.expanded_thoughts.contains(&3)),
        "过程组里的长思考点了要展开"
    );
}

#[test]
fn memory_card_summary_counts_new_updated_and_pending() {
    use smelt_plugin_api::model::acp_session::{MemoryChangeView, MemoryChangesView};
    use smelt_plugin_api::model::control_api::AgentMemoryLayer;
    let entry = |file: &str, created, pending: Option<&str>| MemoryChangeView {
        layer: AgentMemoryLayer::Agent,
        file_name: file.into(),
        name: file.into(),
        description: String::new(),
        created,
        pending_reason: pending.map(str::to_string),
        body: String::new(),
    };
    let entries = vec![
        entry("a.md", true, None),
        entry("b.md", true, Some("外部内容")),
        entry("c.md", false, None),
    ];
    assert_eq!(
        memory_changes_summary(&entries),
        "Memory · 2 new · 1 updated · 1 to confirm"
    );
    assert_eq!(memory_changes_summary(&entries[2..]), "Memory · 1 updated");

    let changes = MemoryChangesView {
        id: 7,
        agent_id: "a1".into(),
        entries,
    };
    assert!(memory_changes_to_show(Some(&changes), None).is_some());
    assert!(memory_changes_to_show(Some(&changes), Some(7)).is_none());
    assert!(memory_changes_to_show(Some(&changes), Some(6)).is_some());
}

#[test]
fn skill_command_goes_to_the_front_and_replaces_an_existing_one() {
    assert_eq!(prefix_skill_command("", "review"), "/skill:review ");
    assert_eq!(
        prefix_skill_command("  看下这个文件", "review"),
        "/skill:review 看下这个文件"
    );
    assert_eq!(
        prefix_skill_command("/skill:old 看下这个文件", "review"),
        "/skill:review 看下这个文件"
    );
    assert_eq!(
        prefix_skill_command("/skill:old", "review"),
        "/skill:review "
    );
}

/// 视图测试要的全局件：gpui-base 的主题和键位，smelt-ui 的字体、菜单和对话框层。
fn init_ui(cx: &mut gpui::App) {
    gpui_base::init(cx);
    smelt_plugin_api::ui::init(cx);
}

/// 左侧提问导航：没加载的那一截用运行时给的索引，已加载的现算；当前读到的那问加长，
/// 指上去出预览，点已加载的直接滚过去，点没加载的先往前翻页。窄了就不画。
#[gpui::test]
fn the_prompt_rail_lists_every_prompt_and_jumps_to_it(cx: &mut gpui::TestAppContext) {
    let (view, cx) = open_conversation(
        cx,
        "acp-prompt-rail",
        vec![
            AcpEntry::User("第二问".into()),
            AcpEntry::Assistant {
                text: "第二答".into(),
                thought: false,
            },
            AcpEntry::User("[Request interrupted by user]".into()),
            AcpEntry::User("第三问".into()),
            AcpEntry::Assistant {
                text: "第三答".into(),
                thought: false,
            },
        ],
        DaemonPhase::Idle,
    );
    cx.simulate_resize(gpui::size(px(1400.), px(900.)));
    view.update(cx, |view, cx| {
        view.loaded_entries_offset = 10;
        view.entries_total = 15;
        view.prompt_index_before = Some(10);
        view.prompt_index = vec![smelt_plugin_api::model::acp_chat::PromptIndexItem {
            entry_ix: 4,
            prompt: "第一问".into(),
            images: 0,
            answer: "第一答".into(),
        }];
        cx.notify();
    });
    draw_settled(cx);
    assert_eq!(
        view.read_with(cx, |view, _| view.prompt_rail_entries()),
        [4, 10, 13],
        "中断标记不算一问"
    );
    assert!(cx.debug_bounds("ACP_PROMPT_RAIL").is_some());
    assert!(cx.debug_bounds("ACP_PROMPT_TICK_3").is_none());

    let tick = cx.debug_bounds("ACP_PROMPT_TICK_2").expect("每问一条刻度");
    cx.simulate_mouse_move(tick.center(), None, gpui::Modifiers::default());
    draw_settled(cx);
    let hovered = view.read_with(cx, |view, _| view.prompt_rail_hover.clone());
    assert_eq!(
        hovered.map(|item| (item.entry_ix, item.prompt, item.answer)),
        Some((13, "第三问".into(), "第三答".into()))
    );
    assert!(cx.debug_bounds("ACP_PROMPT_PEEK").is_some());

    cx.simulate_click(tick.center(), gpui::Modifiers::default());
    assert_eq!(view.read_with(cx, |view, _| view.revealed_prompt), Some(3));

    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.jump_to_prompt(4, cx);
            assert_eq!(view.pending_prompt_jump, Some(4), "没加载的先记下");
            assert!(view.history_loading, "并往前翻页");
        })
    });

    // 前面那页回来了：翻到了那一问就滚过去，不再接着翻。
    cx.update(|_window, cx| {
        view.update(cx, |view, cx| {
            view.history_loading = false;
            let page = (0..10)
                .map(|ix| {
                    if ix == 4 {
                        AcpEntry::User("第一问".into())
                    } else {
                        AcpEntry::Assistant {
                            text: format!("第 {ix} 条"),
                            thought: false,
                        }
                    }
                })
                .collect();
            view.prepend_history_page(tail_snapshot(0, 0, page, DaemonPhase::Idle), cx);
            view.continue_prompt_jump(cx);
            assert_eq!(view.loaded_entries_offset, 0);
            assert_eq!(view.pending_prompt_jump, None);
            assert_eq!(view.revealed_prompt, Some(4));
            assert!(!view.history_loading);
        })
    });

    cx.simulate_resize(gpui::size(px(700.), px(900.)));
    draw_settled(cx);
    assert!(cx.debug_bounds("ACP_PROMPT_RAIL").is_none(), "放不下就不画");
}

/// 从头滚到尾，读到的那一问按先后走过每一问：挤在最后一屏、永远到不了视口顶的
/// 那几问也轮得到，滚到底停在最新一问。跳到某一问时亮的就是它。
#[gpui::test]
fn the_prompt_rail_reaches_every_prompt_while_scrolling(cx: &mut gpui::TestAppContext) {
    let long_answer = (0..80)
        .map(|ix| format!("第 {ix} 段回答"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let short = || AcpEntry::Assistant {
        text: "短".into(),
        thought: false,
    };
    let (view, cx) = open_conversation(
        cx,
        "acp-prompt-rail-sweep",
        vec![
            AcpEntry::User("第一问".into()),
            AcpEntry::Assistant {
                text: long_answer,
                thought: false,
            },
            AcpEntry::User("第二问".into()),
            short(),
            AcpEntry::User("第三问".into()),
            short(),
            AcpEntry::User("第四问".into()),
            short(),
        ],
        DaemonPhase::Idle,
    );
    cx.simulate_resize(gpui::size(px(1400.), px(900.)));
    draw_settled(cx);
    let reading = |cx: &mut gpui::VisualTestContext| {
        view.read_with(cx, |view, _| {
            let rail = view.prompt_rail();
            rail.reading
                .and_then(|ix| rail.prompts.iter().position(|p| *p == ix))
        })
    };
    assert_eq!(
        view.read_with(cx, |view, _| view.list_state.logical_scroll_top().item_ix),
        1,
        "贴底时视口顶还在第一问的回答里"
    );
    assert_eq!(reading(cx), Some(3), "滚到底读的是最新一问");

    view.update(cx, |view, cx| {
        view.list_state.scroll_to(gpui::ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        cx.notify();
    });
    draw_settled(cx);
    let mut seen = vec![reading(cx)];
    for _ in 0..400 {
        if view.read_with(cx, |view, _| view.list_state.is_scrolled_to_end()) == Some(true) {
            break;
        }
        view.update(cx, |view, cx| {
            view.list_state.scroll_by(px(40.));
            cx.notify();
        });
        draw_settled(cx);
        let tick = reading(cx);
        if seen.last() != Some(&tick) {
            seen.push(tick);
        }
    }
    assert_eq!(seen, [Some(0), Some(1), Some(2), Some(3)]);

    // 第二问挤在最后一屏，滚不到视口顶；跳过去时阅读线不在它头上，照样亮它。
    view.update(cx, |view, cx| view.jump_to_prompt(2, cx));
    draw_settled(cx);
    assert_eq!(reading(cx), Some(1), "跳到哪问亮哪问");

    let rail = cx.debug_bounds("ACP_PROMPT_RAIL").expect("宽屏画导航");
    cx.simulate_event(gpui::ScrollEvent {
        position: gpui::point(px(700.), rail.center().y),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-200.))),
        ..Default::default()
    });
    draw_settled(cx);
    assert_eq!(reading(cx), Some(3), "人一滚就回到按阅读线算");
}

/// 长对话里左侧提问导航按滑动窗口限制可见刻度数，不溢出视口；跳到边缘一问时窗口跟着滑过去。
#[gpui::test]
fn the_prompt_rail_windows_long_conversations_around_the_reading_prompt(
    cx: &mut gpui::TestAppContext,
) {
    let entries: Vec<AcpEntry> = (0..40)
        .flat_map(|ix| {
            [
                AcpEntry::User(format!("第 {} 问", ix + 1)),
                AcpEntry::Assistant {
                    text: format!("第 {} 答", ix + 1),
                    thought: false,
                },
            ]
        })
        .collect();
    let (view, cx) = open_conversation(cx, "acp-prompt-rail-window", entries, DaemonPhase::Idle);
    cx.simulate_resize(gpui::size(px(1400.), px(900.)));
    draw_settled(cx);

    let rail = cx.debug_bounds("ACP_PROMPT_RAIL").expect("宽屏画导航");
    assert_eq!(rail.size.height, px(110.), "最多画 11 条刻度 × 10px");
    assert!(cx.debug_bounds("ACP_PROMPT_TICK_0").is_none(), "最早的问滑出窗口");
    assert!(cx.debug_bounds("ACP_PROMPT_TICK_28").is_none());
    let top_tick = cx
        .debug_bounds("ACP_PROMPT_TICK_29")
        .expect("窗口从第 30 问开始");
    assert!(
        cx.debug_bounds("ACP_PROMPT_TICK_39").is_some(),
        "滚到底时最新一问在窗口底部"
    );

    // 点当前窗口最顶上的那一问，窗口以它为中心往前滑出更早的提问。
    cx.simulate_click(top_tick.center(), gpui::Modifiers::default());
    draw_settled(cx);
    let next_top = cx
        .debug_bounds("ACP_PROMPT_TICK_24")
        .expect("窗口往前滑出更早的提问");
    assert!(cx.debug_bounds("ACP_PROMPT_TICK_23").is_none());
    assert!(cx.debug_bounds("ACP_PROMPT_TICK_34").is_some());
    assert!(
        cx.debug_bounds("ACP_PROMPT_TICK_35").is_none(),
        "后面的提问滑出窗口"
    );

    // 移开指针再悬停到居中后的那一问上，预览卡按窗口内槽位定位，不按全局下标跑飞。
    cx.simulate_mouse_move(gpui::point(px(600.), px(400.)), None, gpui::Modifiers::default());
    draw_settled(cx);
    let centered_tick = cx
        .debug_bounds("ACP_PROMPT_TICK_29")
        .expect("刚跳到的那一问居中留在窗口内");
    cx.simulate_mouse_move(centered_tick.center(), None, gpui::Modifiers::default());
    draw_settled(cx);
    let peek = cx.debug_bounds("ACP_PROMPT_PEEK").expect("悬停弹出预览");
    assert!(
        (peek.top() - centered_tick.center().y).abs() < px(80.),
        "预览卡按窗口内槽位定位，不按全局下标跑飞: peek={peek:?}, tick={centered_tick:?}"
    );

    // 已在浏览历史时再点一次顶部刻度，窗口仍能继续往前滑。
    cx.simulate_click(next_top.center(), gpui::Modifiers::default());
    draw_settled(cx);
    assert!(
        cx.debug_bounds("ACP_PROMPT_TICK_19").is_some(),
        "连续往前跳时窗口继续前滑"
    );
    assert!(cx.debug_bounds("ACP_PROMPT_TICK_18").is_none());
    assert!(cx.debug_bounds("ACP_PROMPT_TICK_29").is_some());
    assert!(cx.debug_bounds("ACP_PROMPT_TICK_30").is_none());

    // 视口压矮时窗口跟着缩容，不超出视口。
    cx.simulate_resize(gpui::size(px(1400.), px(160.)));
    draw_settled(cx);
    let short_rail = cx.debug_bounds("ACP_PROMPT_RAIL").expect("矮视口按容量缩小窗口");
    let viewport_h = view.read_with(cx, |v, _| v.list_state.viewport_bounds().size.height);
    assert!(
        short_rail.size.height < px(110.) && short_rail.size.height < viewport_h,
        "刻度列高度 {:?} 应随矮视口 {:?} 缩容且不溢出",
        short_rail.size.height,
        viewport_h
    );
}

/// 授权请求长在输入框顶上、和输入框连成一体；选项一列菜单，点了就回给 agent。
#[gpui::test]
fn the_permission_tray_sits_inside_the_composer(cx: &mut gpui::TestAppContext) {
    let (view, cx) = open_conversation(
        cx,
        "acp-permission-tray",
        vec![
            AcpEntry::User("跑测试".into()),
            AcpEntry::tool_call(
                "run-1",
                "cargo test",
                ToolKind::Execute,
                ToolCallStatus::Pending,
                Vec::new(),
            ),
        ],
        DaemonPhase::AwaitingApproval,
    );
    cx.update(|window, cx| view.update(cx, |view, cx| view.init_input(window, cx)));
    view.update(cx, |view, cx| {
        view.permissions = vec![PendingPermission {
            id: "p1".into(),
            question: "cargo test".into(),
            tool_call_id: "run-1".into(),
            options: vec![
                PermissionOptionView {
                    option_id: "allow".into(),
                    name: "Allow once".into(),
                    kind: PermissionOptionKindView::AllowOnce,
                },
                PermissionOptionView {
                    option_id: "reject".into(),
                    name: "Reject".into(),
                    kind: PermissionOptionKindView::RejectOnce,
                },
            ],
            details: ApprovalDetailsView::Command {
                command: "cargo test -j 2".into(),
                cwd: None,
                reason: None,
            },
        }];
        cx.notify();
    });
    draw_settled(cx);
    let composer = cx.debug_bounds("ACP_COMPOSER").expect("输入框应已绘制");
    let tray = cx
        .debug_bounds("ACP_PERMISSION_TRAY")
        .expect("授权请求应已绘制");
    assert!(
        composer.contains(&tray.origin) && tray.top() == composer.top() + px(1.),
        "授权请求在输入框里面、贴着顶边: {tray:?} in {composer:?}"
    );
    assert!(cx.debug_bounds("ACP_PERMISSION_OPTION_0").is_some());
    let reject = cx
        .debug_bounds("ACP_PERMISSION_OPTION_1")
        .expect("每个选项一行");
    assert!(reject.top() > tray.top(), "选项排成一列");
}

/// 收起的段把派出的子智能体一个一行列在计数行下面；点一行展开这段、打开它的卡片。
#[gpui::test]
fn a_collapsed_segment_lists_its_subagents(cx: &mut gpui::TestAppContext) {
    let mut agent = AcpEntry::tool_call(
        "agent-1",
        "Explore callers",
        ToolKind::Collaborate,
        ToolCallStatus::InProgress,
        Vec::new(),
    );
    let AcpEntry::ToolCall { children, .. } = &mut agent else {
        panic!("agent");
    };
    children.push(completed_tool("child-read", ToolKind::Read, "src/lib.rs"));
    let entries = vec![
        AcpEntry::User("查调用方".into()),
        completed_tool("read-1", ToolKind::Read, "README.md"),
        agent,
    ];
    let (view, cx) = open_conversation(cx, "acp-segment-agents", entries, DaemonPhase::Thinking);
    draw_settled(cx);
    draw_settled(cx);
    let row = cx
        .debug_bounds("ACP_SEGMENT_AGENT_2")
        .expect("收起的段里列出子智能体");
    cx.simulate_click(row.center(), Default::default());
    draw_settled(cx);
    draw_settled(cx);
    assert!(
        view.read_with(cx, |view, _| view
            .expanded_process_groups
            .contains(&(1, true))),
        "点子智能体一行展开这段"
    );
    assert!(
        view.read_with(cx, |view, _| view.expanded_tool_cards.contains("agent-1")),
        "并打开它的卡片"
    );
    assert!(
        cx.debug_bounds("ACP_SEGMENT_AGENT_2").is_none(),
        "展开后子智能体就是段里的卡片，不再列一行"
    );
}

/// 在跑的段收成一行计数，改过的文件露成标签；点标签弹窗看这个文件的改动，对话里不展开。
#[gpui::test]
fn clicking_an_edit_chip_opens_its_diff_in_a_dialog(cx: &mut gpui::TestAppContext) {
    let entries = vec![
        AcpEntry::User("改一下".into()),
        AcpEntry::tool_call(
            "edit-1",
            "Edit src/lib.rs",
            ToolKind::Edit,
            ToolCallStatus::Completed,
            vec![ToolOutputPart::Diff {
                path: "src/lib.rs".into(),
                old_text: Some("let a = 1;\n".into()),
                new_text: "let a = 2;\nlet b = 3;\n".into(),
            }],
        ),
        completed_tool("run-1", ToolKind::Execute, "cargo test"),
    ];
    let (view, cx) = open_conversation_in_root(cx, "acp-edit-chip", entries, DaemonPhase::Thinking);
    draw_settled(cx);
    draw_settled(cx);
    let head_before = view
        .read_with(cx, |view, _| view.list_state.bounds_for_item(1))
        .expect("段首行应已绘制");
    let chip = cx
        .debug_bounds("ACP_EDIT_CHIP_1_0")
        .expect("改过的文件露成标签");
    cx.simulate_click(chip.center(), Default::default());
    draw_settled(cx);
    draw_settled(cx);
    assert!(
        cx.update(|window, cx| smelt_plugin_api::ui::dialog::is_open(window, cx)),
        "点标签弹出 diff"
    );
    assert!(
        view.read_with(cx, |view, _| !view
            .expanded_process_groups
            .contains(&(1, true))),
        "点标签不展开整段"
    );
    let head_after = view
        .read_with(cx, |view, _| view.list_state.bounds_for_item(1))
        .expect("段首行应已绘制");
    assert_eq!(head_after.size, head_before.size, "对话里不原地展开");
}
