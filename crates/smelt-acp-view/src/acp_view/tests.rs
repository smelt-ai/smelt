//! ACP 视图状态决策、Markdown/差异展示辅助逻辑的回归测试。

use super::trajectory::runtime_debug_contexts;
use super::{
    CachedDiff, ComposerMenuSection, LoadedSkillsRefresh, LoadedSkillsSnapshot, OwnerIdCache,
    ProcessGroupInfo, RESTORED_ENTRY_HEIGHT_HINT_PX, ViewHistoryIdentity, append_prompt_text,
    apply_pending_config_selection, background_task_one_line, background_task_output_lines,
    background_task_running_for, background_task_summary, build_conversation_layout,
    build_conversation_layout_with_timings, build_markdown_cache, build_tool_image_parts,
    cached_diff_stats, can_load_older_history, classify_attached_paths, compact_token_count,
    compact_tool_headline, compact_tool_run_label, composer_config_section_label,
    composer_menu_sections, composer_model_label, composer_model_scroll,
    composer_native_queue_shortcut_hint, composer_next_turn_notice, composer_should_nest_models,
    composer_usage_breakdown, config_selection_is_pending, config_update_failure_is_new,
    consecutive_compact_tool_run, consume_composer_restore, conversation_input_for_submit,
    current_turn_has_agent_output, did_recover_from_ended, diff_cache_matches_output,
    diff_stats_for_output, entries_to_fade_in, escape_html_tags_for_markdown,
    external_clipboard_image_paths, filter_trajectory_events, format_attached_paths,
    is_active_permission_selection, is_dispatch_in_flight, is_fresh_conversation_start,
    is_match_count_line, is_new_conversation_command, is_stale_blank_history_id,
    latest_user_message_index, loaded_entries_end, loaded_skills_refresh, markdown_text_for_cwd,
    markdown_user_text_for_cwd, merge_rejected_prompt, merge_snapshot_entries,
    model_label_with_provider, move_queue_item_to_front, native_queue_item_kind_label,
    next_snapshot_prompt_gate, overlay_model_state, overlay_pending_initial_config,
    overlay_session_configs, pending_config_choice_names, plan_current_step, preformatted_html,
    process_group_header_label, process_group_label, progress_has_details, progress_summary,
    provider_menu_label, provider_switch_value, reconcile_pending_config_values,
    refresh_markdown_cache, resolve_restart_launch, restorable_gui_prompt, rows_with_changed_shape,
    running_background_tasks, search_summary_text, selected_provider_group,
    session_trajectory_events, settled_thought_elapsed, settled_thought_label,
    should_apply_snapshot_revision, should_cancel_for_immediate_prompt,
    should_clear_history_session_id_after_snapshot, should_seed_restored_height_hints,
    task_body_from_selection, tool_card_default_expanded, tool_image_cache_matches_output,
    tool_output_has_content, tool_result_summary, tool_uses_compact_process_row, trajectory_counts,
    usage_percent, usage_ring_color, usage_warn_color, user_bubble_enter_ready,
    view_history_identity, working_status_label,
};
use gpui::{ClipboardEntry, ExternalPaths, FollowMode, ListAlignment, ListState, px};
use smelt_core::acp_chat::{AcpEntry, TaskNote, ToolCallStatus, ToolKind, ToolOutputPart};
use smelt_core::acp_conn::{ModelProviderGroup, ModelState, SessionConfigState};
use smelt_core::acp_session::{
    AcpTurnOutcome, ApprovalDetailsView, BackgroundTaskStatus, BackgroundTaskView,
    ElicitFieldKindView, ElicitFieldView, ElicitOptionView, PendingElicitation, PendingPermission,
    PermissionOptionKindView, PermissionOptionView, PlanEntryStatusView, PlanEntryView, PlanView,
    RuntimeDebug, RuntimeDebugModel, RuntimeDebugModelCall, RuntimeDebugTool, TurnTiming,
};
use smelt_core::agent_kind::{AcpProfile, ConversationAgentKind, ConversationLaunchSpec};
use smelt_core::daemon_state::DaemonPhase;
use smelt_ui::provider_status_global::{ProviderStatus, QuotaStatus, QuotaTarget};
use smelt_ui::ui_theme;

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

#[test]
fn loaded_skills_are_not_rescanned_until_launch_or_cwd_changes() {
    let launch = ConversationLaunchSpec::from_command("pi");
    let cwd = Some("/tmp/project".to_string());
    let cached = LoadedSkillsSnapshot {
        launch: launch.clone(),
        cwd: cwd.clone(),
        skills: Vec::new(),
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
fn session_menu_nests_models_when_other_sections_exist() {
    assert!(composer_should_nest_models(2, 1, 12));
    assert!(composer_should_nest_models(0, 3, 8));
    assert!(!composer_should_nest_models(0, 1, 40));
    assert!(!composer_should_nest_models(2, 2, 1));
}

#[test]
fn long_model_menu_scrolls_without_hiding_the_submenu() {
    // 可滚动的根菜单画不出「模型」二级，超长列表必须滚在没有再嵌套的那一层。
    let nested = composer_model_scroll(true);
    assert!(nested.submenu);
    assert!(!nested.root);

    let flat = composer_model_scroll(false);
    assert!(flat.root);
    assert!(!flat.submenu);
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
fn user_model_choice_overrides_restored_initial_config() {
    let mut pending_initial = vec![
        ("model".into(), "opus".into()),
        ("mode".into(), "default".into()),
    ];
    overlay_pending_initial_config(&mut pending_initial, "model".into(), "sonnet".into());
    assert_eq!(
        pending_initial,
        vec![
            ("model".into(), "sonnet".into()),
            ("mode".into(), "default".into()),
        ]
    );

    let mut flushed = Vec::new();
    overlay_pending_initial_config(&mut flushed, "model".into(), "sonnet".into());
    assert!(
        flushed.is_empty(),
        "第一份 Idle 已经发出去之后，不能把用户选择重新塞回待发列表"
    );
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
fn config_update_failure_status_is_a_rising_edge() {
    assert!(config_update_failure_is_new(
        None,
        Some("更新会话配置失败：agent rejected")
    ));
    assert!(config_update_failure_is_new(
        Some("正在恢复"),
        Some("更新会话配置失败：timeout")
    ));
    assert!(!config_update_failure_is_new(
        Some("更新会话配置失败：timeout"),
        Some("更新会话配置失败：timeout")
    ));
    assert!(!config_update_failure_is_new(
        Some("正在恢复"),
        Some("正在恢复")
    ));
    assert!(!config_update_failure_is_new(None, Some("正在启动 Grok")));
}

#[test]
fn next_turn_hint_only_when_pending_and_turn_is_active() {
    assert_eq!(
        composer_config_section_label("Mode", true, true),
        "Mode · 下轮生效"
    );
    assert_eq!(composer_config_section_label("Mode", true, false), "Mode");
    assert_eq!(composer_config_section_label("Mode", false, true), "Mode");
    assert_eq!(
        composer_config_section_label("模型", true, true),
        "模型 · 下轮生效"
    );
}

#[test]
fn native_queue_copy_names_current_round_and_after_round() {
    assert_eq!(
        composer_native_queue_shortcut_hint(),
        "Enter 插入当前回合 · ⌥Enter 回合后发送"
    );
    assert_eq!(native_queue_item_kind_label(false), "当前回合");
    assert_eq!(native_queue_item_kind_label(true), "下一回合");
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

    let image = smelt_core::acp_chat::AcpImage {
        mime: "image/png".into(),
        data_b64: "aGk=".into(),
    };
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
        current_name: "关".into(),
        options: vec![("true".into(), "开".into()), ("false".into(), "关".into())],
        boolean: Some(false),
    };
    let names = pending_config_choice_names(
        &pending,
        &[mode_config("agent"), fast],
        Some(&sample_model("sol")),
    );
    assert_eq!(names, vec!["Plan".to_string(), "Fast mode 开".to_string()]);
    assert_eq!(
        composer_next_turn_notice(&names, true).as_deref(),
        Some("Plan、Fast mode 开 · 下轮生效")
    );
    assert_eq!(composer_next_turn_notice(&names, false), None);
    assert_eq!(
        composer_next_turn_notice(&["Plan".into()], true).as_deref(),
        Some("Plan · 下轮生效")
    );
    assert_eq!(
        composer_next_turn_notice(&["Plan".into(), "Max".into(), "Fast".into()], true).as_deref(),
        Some("Plan 等 3 项 · 下轮生效")
    );
}

#[test]
fn composer_usage_is_readonly_and_only_warns_when_high() {
    assert_eq!(compact_token_count(12_400), "12.4k");
    assert_eq!(compact_token_count(200_000), "200k");
    assert_eq!(compact_token_count(999), "999");
    assert_eq!(usage_percent(164_000, 200_000), 82);
    assert_eq!(usage_warn_color(6), None);
    assert_eq!(usage_warn_color(75), Some(ui_theme::yellow()));
    assert_eq!(usage_warn_color(90), Some(ui_theme::red()));
    assert_eq!(usage_ring_color(38), ui_theme::blue());
    assert_eq!(usage_ring_color(75), ui_theme::yellow());
    assert_eq!(usage_ring_color(90), ui_theme::red());
    let conversation = composer_usage_breakdown(12_400, 200_000, None, None, ui_theme::accent());
    assert_eq!(conversation[0].label, "Conversation");
    assert_eq!(conversation[0].tokens, 12_400);
    assert_eq!(conversation.len(), 1);
    let buckets = smelt_core::acp_conn::ContextUsageBreakdown {
        system_prompt: 1_200,
        tools_definition: 11_800,
        rules: 302,
        skills: 3_700,
        mcp_dynamic: 2_200,
        subagent: 1_600,
        summarized: 8_700,
        conversation: 333_000,
    };
    let rows =
        composer_usage_breakdown(362_502, 1_000_000, None, Some(&buckets), ui_theme::accent());
    assert_eq!(
        rows.iter().map(|row| row.label).collect::<Vec<_>>(),
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
            },
            ModelProviderGroup {
                id: "deepseek".to_string(),
                name: "DeepSeek".to_string(),
                options: vec![(
                    "deepseek/deepseek-v4-pro".to_string(),
                    "DeepSeek-V4-Pro".to_string(),
                )],
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
    };

    assert_eq!(provider_switch_value(&provider, "Pro"), Some("backup/pro"));
    assert_eq!(
        provider_switch_value(&provider, "Missing"),
        Some("backup/flash")
    );
}

fn provider_group(id: &str) -> ModelProviderGroup {
    ModelProviderGroup {
        id: id.to_string(),
        name: id.to_string(),
        options: vec![(format!("{id}/model"), "Model".to_string())],
    }
}

fn published_quota(used_percent: f64) -> smelt_core::provider_quota::ProviderQuotaData {
    smelt_core::provider_quota::ProviderQuotaData::Windows(
        smelt_core::provider_quota::ProviderQuota {
            windows: vec![smelt_core::provider_quota::ProviderQuotaWindow {
                label: "5h".to_string(),
                used_percent,
                resets_at_ms: None,
                reset_after_seconds: None,
            }],
        },
    )
}

/// 把一份额度放进全局单例，模拟刷新循环刚查完一轮。
fn publish_pi_quota(provider_id: &'static str, used_percent: f64, cx: &mut gpui::App) {
    cx.set_global(ProviderStatus::default());
    ProviderStatus::apply_quota(
        vec![(
            QuotaTarget::pi(provider_id),
            QuotaStatus {
                quota: Some(published_quota(used_percent)),
                checked_at_ms: 1,
                error: None,
            },
        )],
        cx,
    );
}

#[gpui::test]
fn pi_provider_menu_shows_the_quota_in_parentheses(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        publish_pi_quota("anthropic", 25.0, cx);

        assert_eq!(
            provider_menu_label(ConversationAgentKind::Pi, &provider_group("anthropic"), cx),
            "anthropic (25%)"
        );
    });
}

/// 查不到额度就是原名，不能留一个空括号。API key 型的 provider
/// （modelsight / volces 这类）永远走这条路。
#[gpui::test]
fn providers_without_a_known_quota_keep_their_plain_name(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        publish_pi_quota("anthropic", 25.0, cx);

        assert_eq!(
            provider_menu_label(ConversationAgentKind::Pi, &provider_group("modelsight"), cx),
            "modelsight"
        );
    });
}

/// 额度查的是 Pi 自己 `auth.json` 里的账户；别家 agent 里的同名 provider 是另一
/// 套凭据，不能把这个数字套上去。
#[gpui::test]
fn non_pi_agents_never_show_pi_quota(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        publish_pi_quota("xai", 80.0, cx);

        assert_eq!(
            provider_menu_label(ConversationAgentKind::Claude, &provider_group("xai"), cx),
            "xai"
        );
    });
}

/// 没装全局单例时不能 panic，只是不画括号。
#[gpui::test]
fn missing_global_degrades_to_the_plain_name(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| {
        assert_eq!(
            provider_menu_label(ConversationAgentKind::Pi, &provider_group("anthropic"), cx),
            "anthropic"
        );
    });
}
use smelt_ui::agent_host_state::AgentHostState;
use std::collections::VecDeque;

#[test]
fn legacy_delivery_prompt_is_not_restored_by_the_gui() {
    assert_eq!(
        restorable_gui_prompt(Some("legacy delivery prompt".to_string()), Some("run-1")),
        None
    );
    assert_eq!(
        restorable_gui_prompt(Some("ordinary handoff".to_string()), None),
        Some("ordinary handoff".to_string())
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
        Some("3 行")
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
        Some("2 行输出")
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
fn recovered_edge_requires_ended_to_non_ended_transition() {
    assert!(did_recover_from_ended(true, &DaemonPhase::Idle));
    assert!(did_recover_from_ended(true, &DaemonPhase::Thinking));
    assert!(!did_recover_from_ended(true, &DaemonPhase::Connecting));
    assert!(!did_recover_from_ended(true, &DaemonPhase::Dead));
    assert!(!did_recover_from_ended(false, &DaemonPhase::Idle));
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
    use agent_client_protocol::schema::v1::SessionId;

    let same = SessionId::new("blank-session");
    let other = SessionId::new("other-session");
    assert!(is_stale_blank_history_id(true, Some(&same), Some(&same)));
    assert!(!is_stale_blank_history_id(true, Some(&same), Some(&other)));
    assert!(!is_stale_blank_history_id(false, Some(&same), Some(&same)));
    assert!(!is_stale_blank_history_id(true, Some(&same), None));
}

#[test]
fn starting_status_copy_explains_resume_and_wait() {
    use super::starting_status_copy;

    assert_eq!(
        starting_status_copy(None, true, "Claude Code", 12),
        (
            "正在恢复上次的会话".to_string(),
            "正在恢复历史消息和工作上下文".to_string(),
            "已等待 12 秒".to_string(),
        )
    );
    assert_eq!(
        starting_status_copy(Some("  正在连接 agent  "), false, "Codex", 1),
        (
            "正在启动 Codex".to_string(),
            "正在连接 agent".to_string(),
            "已等待 1 秒".to_string(),
        )
    );
}

#[test]
fn queued_prompt_can_be_moved_next_without_dropping_any_message() {
    let mut queue = VecDeque::from(["first", "second", "third"]);
    assert!(move_queue_item_to_front(&mut queue, 2));
    assert_eq!(
        queue.into_iter().collect::<Vec<_>>(),
        vec!["third", "first", "second"]
    );

    let mut queue = VecDeque::from(["only"]);
    assert!(!move_queue_item_to_front(&mut queue, 1));
    assert_eq!(queue.into_iter().collect::<Vec<_>>(), vec!["only"]);
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
fn immediate_send_idle_releases_stuck_dispatch_pending() {
    // 上一条 prompt 已发出但 Running 尚未回来，用户点了排队消息的立即发送。
    // 取消后的 Idle 必须放开 dispatch 闸门，否则队首永远 flush 不出去，
    // 后续输入会一直堆在排队条里。
    let gate = next_snapshot_prompt_gate(&DaemonPhase::Idle, None, true, true, false, None);
    assert!(!gate.prompt_dispatch_pending);
    assert!(gate.should_flush_queue);
    assert!(!gate.immediate_cancel_pending);
}

#[test]
fn stale_idle_does_not_clear_unconfirmed_dispatch() {
    // 没有立即发送时，旧 Idle 快照不能清掉 pending——那是防并发派发的闸门。
    let gate = next_snapshot_prompt_gate(&DaemonPhase::Idle, None, true, false, true, None);
    assert!(gate.prompt_dispatch_pending);
    assert!(!gate.should_flush_queue);
    assert!(!gate.immediate_cancel_pending);
}

#[test]
fn failed_idle_releases_stuck_dispatch_pending() {
    let gate = next_snapshot_prompt_gate(
        &DaemonPhase::Idle,
        None,
        true,
        false,
        true,
        Some(AcpTurnOutcome::Failed),
    );
    assert!(!gate.prompt_dispatch_pending);
    assert!(!gate.should_flush_queue);
}

#[test]
fn running_snapshot_confirms_dispatch_and_does_not_flush() {
    let gate = next_snapshot_prompt_gate(&DaemonPhase::Thinking, Some(1), true, false, false, None);
    assert!(!gate.prompt_dispatch_pending);
    assert!(!gate.should_flush_queue);
    assert!(!gate.immediate_cancel_pending);
}

#[test]
fn settled_idle_after_immediate_send_flushes_queue() {
    let gate = next_snapshot_prompt_gate(&DaemonPhase::Idle, None, false, true, false, None);
    assert!(!gate.prompt_dispatch_pending);
    assert!(gate.should_flush_queue);
    assert!(!gate.immediate_cancel_pending);
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
    let previous = smelt_core::conversation::ConversationInput {
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
fn dispatch_stays_in_flight_until_the_prompt_and_cancel_both_settle() {
    assert!(is_dispatch_in_flight(true, false));
    assert!(is_dispatch_in_flight(false, true));
    assert!(!is_dispatch_in_flight(false, false));
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

#[test]
fn markdown_local_files_become_internal_file_urls() {
    let rendered = markdown_text_for_cwd(
        "看 [workspace.md](docs/workspace.md)、[源码](/tmp/source.rs#L42) 和 [官网](https://example.com)",
        Some("/tmp/project"),
    );
    assert!(rendered.contains("[workspace.md](smelt-file:///tmp/project/docs/workspace.md)"));
    assert!(rendered.contains("[源码](smelt-file:///tmp/source.rs#L42)"));
    assert!(rendered.contains("[官网](https://example.com)"));
}

#[test]
fn markdown_absolute_files_resolve_without_session_cwd() {
    let rendered = markdown_text_for_cwd(
        "无弹窗：[截图](/tmp/smelt-current-notification-final.png)",
        None,
    );
    assert_eq!(
        rendered,
        "无弹窗：[截图](smelt-file:///tmp/smelt-current-notification-final.png)"
    );
}

/// grep / 编译器诊断常见的 `path:行号` 引用格式（不是 `#L行号` 片段）也要能
/// 拆出行号——原来只认 `#`，`:2765` 会整段被当成文件名拼进路径，导致读不到
/// 文件、误报“可能是二进制文件”（见用户反馈：点 ACP 对话里的文件引用链接）。
#[test]
fn markdown_colon_line_refs_resolve_to_fragment() {
    let rendered = markdown_text_for_cwd(
        "见 [acp_view.rs](crates/smelt-acp-view/src/acp_view.rs:2765)",
        Some("/tmp/project"),
    );
    assert!(rendered.contains(
        "[acp_view.rs](smelt-file:///tmp/project/crates/smelt-acp-view/src/acp_view.rs#L2765)"
    ));
}

/// `path:行号:列号` 形式（列号可选的第二段）也只取行号，列号丢弃。
#[test]
fn markdown_colon_line_col_refs_take_line_not_col() {
    let rendered = markdown_text_for_cwd("见 [x](src/main.rs:10:5)", Some("/tmp/project"));
    assert!(rendered.contains("[x](smelt-file:///tmp/project/src/main.rs#L10)"));
}

#[test]
fn user_markdown_keeps_html_tags_literal() {
    let escaped = escape_html_tags_for_markdown(
        "前 <section class=\"card\">内容</section> <https://example.com> ` <span> `\n\
             ```html\n<div>代码</div>\n```\n",
    );
    assert_eq!(
        escaped,
        "前 \\<section class=\"card\">内容\\</section> <https://example.com> ` <span> `\n\
             ```html\n<div>代码</div>\n```\n"
    );

    let rendered = markdown_user_text_for_cwd("见 [文件](src/index.html) 和 <panel>", None);
    assert!(rendered.contains("\\<panel>"));
}

fn long_tool(id: &str) -> AcpEntry {
    AcpEntry::ToolCall {
        id: id.to_string(),
        title: String::new(),
        kind: ToolKind::Other,
        status: ToolCallStatus::Completed,
        output: Vec::new(),
        children: Vec::new(),
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
    let mut cache = build_markdown_cache(&entries, Some("/tmp/project"));
    let unchanged_prefix = cache[0].clone();

    entries.truncate(1);
    entries.push(AcpEntry::Assistant {
        text: "查看 [新回答](new.rs)".into(),
        thought: false,
    });
    entries.push(AcpEntry::Divider("next".into()));
    refresh_markdown_cache(&entries, 1, Some("/tmp/project"), &mut cache);

    assert_eq!(cache.len(), entries.len());
    assert_eq!(cache[0], unchanged_prefix);
    assert!(cache[0].as_deref().unwrap().contains("\\<panel>"));
    assert!(
        cache[1]
            .as_deref()
            .unwrap()
            .contains("smelt-file:///tmp/project/new.rs")
    );
    assert!(cache[2].is_none());
}

#[test]
fn preformatted_html_preserves_literal_tool_output() {
    assert_eq!(
        preformatted_html("# heading\n* item\n`code` <tag> & \"quoted\" 'single'"),
        "<pre># heading\n* item\n`code` &lt;tag&gt; &amp; &quot;quoted&quot; &#39;single&#39;</pre>"
    );
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

    let group = layout[1].process_group.expect("过程正文应进入执行过程组");
    assert_eq!(group.first, 1);
    assert_eq!(group.end, 3);
    assert!(layout[2].process_group.is_some());
    assert!(layout[3].process_group.is_none());
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
    });
    let from = super::layout_recompute_start(&entries, entries.len() - 1);
    assert!(from >= 3, "已结束回合不应重算，起点={from}");
    super::refresh_conversation_layout_from(&mut layout, &entries, from, true, 0, &[], None);
    assert_eq!(layout, build_conversation_layout(&entries, true));
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
fn live_thoughts_count_as_agent_output() {
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
    assert!(current_turn_has_agent_output(&entries));
    assert!(!current_turn_has_agent_output(&[AcpEntry::User(
        "你会啥技能".into()
    )]));
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
fn completed_process_group_appends_elapsed_like_grok() {
    let entries = vec![
        AcpEntry::User("看看今天的新闻".into()),
        completed_tool("s1", ToolKind::Search, "today news"),
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
    let layout = build_conversation_layout_with_timings(&entries, false, 0, &timings, None);
    let group = layout[1].process_group.expect("结束后应收成过程组");
    assert_eq!(group.elapsed_ms, Some(4_000));
    assert_eq!(
        process_group_header_label(&entries, group).as_deref(),
        Some("工作了 4s")
    );

    let live = build_conversation_layout_with_timings(&entries, true, 0, &[], Some(6_000));
    let live_group = live[1].process_group.expect("进行中也应收成过程组");
    assert!(live_group.active);
    assert_eq!(live_group.elapsed_ms, Some(6_000));
}

#[test]
fn active_process_group_without_a_running_tool_says_thinking() {
    let thinking = ProcessGroupInfo {
        active: true,
        ..ProcessGroupInfo::default()
    };
    assert_eq!(process_group_label(thinking), "正在想");

    let searching = ProcessGroupInfo {
        active: true,
        active_tool_kind: Some(ToolKind::Search),
        ..ProcessGroupInfo::default()
    };
    assert_eq!(process_group_label(searching), "正在搜索");
}

#[test]
fn working_status_names_the_wait() {
    assert_eq!(working_status_label("6s"), "正在想 · 6s");
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
    assert_eq!(settled_thought_label(3_000), "想了 3s");

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
        },
        AcpEntry::ToolCall {
            id: "read-1".into(),
            title: "crates/smelt/src/main.rs".into(),
            kind: ToolKind::Read,
            status: ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
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
        },
        AcpEntry::TaskNote(TaskNote {
            kind: Default::default(),
            id: "bg-1".into(),
            title: "编译".into(),
            status: "failed".into(),
            exit_code: Some(1),
            output_tail: "error: boom".into(),
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

#[test]
fn progress_summary_is_visible_but_long_details_stay_expandable() {
    let text = "\n**先定位终端面板入口**\n\n我会继续检查状态管理、快捷键和 session list 的调用方。";

    assert_eq!(progress_summary(text), "先定位终端面板入口");
    assert!(progress_has_details(text));
    assert!(!progress_has_details("只需一句简短进展"));
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
fn collaboration_tools_are_not_folded_into_process_groups() {
    let entries = vec![
        AcpEntry::User("并行检查".into()),
        AcpEntry::ToolCall {
            id: "agent-1".into(),
            title: "Start subagent explorer".into(),
            kind: ToolKind::Collaborate,
            status: ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
        },
        AcpEntry::ToolCall {
            id: "read-1".into(),
            title: "Read source".into(),
            kind: ToolKind::Read,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
        },
    ];

    let live = build_conversation_layout(&entries, true);
    assert!(
        live[1].process_group.is_none(),
        "子代理是独立卡片，不收进执行过程组"
    );
    let live_tools = live[2].process_group.expect("普通工具仍收成过程组");
    assert!(live_tools.active);
    assert_eq!(live_tools.agents, 0);

    let layout = build_conversation_layout(&entries, false);
    assert!(layout[1].process_group.is_none());
    let group = layout[2].process_group.expect("普通工具结束后仍收成过程组");
    assert_eq!(
        process_group_header_label(&entries, group).as_deref(),
        Some("读取了 1 次")
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
        },
        AcpEntry::ToolCall {
            id: "test-2".into(),
            title: "cargo test retry".into(),
            kind: ToolKind::Execute,
            status: ToolCallStatus::Completed,
            output: Vec::new(),
            children: Vec::new(),
        },
        AcpEntry::Assistant {
            text: "测过了，已经修好".into(),
            thought: false,
        },
    ];

    let layout = build_conversation_layout(&entries, false);
    let group = layout[1].process_group.expect("工具应进入执行过程组");
    assert!(!group.active);
    assert_eq!(process_group_label(group), "执行过程");
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
    assert!(
        layout[1].process_group.is_none(),
        "上一回合的子代理不能并进当前执行过程"
    );
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
        layout[1].process_group.is_none(),
        "子代理完成后仍是独立卡片，不收进执行过程组"
    );
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
        },
    ];

    let layout = build_conversation_layout(&entries, false);

    assert!(layout[3].final_answer);
    let before = layout[1].process_group.expect("编辑应在过程组");
    assert_eq!(before.end, 3);
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
        },
    ];

    let layout = build_conversation_layout(&entries, false);

    assert!(layout[1].final_answer);
    assert!(layout[1].process_group.is_none());
}

#[test]
fn restart_uses_updated_profile_launch_spec_when_profile_still_exists() {
    let current = ConversationLaunchSpec::from_command("claude --old")
        .with_env("CLAUDE_CONFIG_DIR", "~/Claude Workspaces/old");
    let mut config =
        AgentHostState::default().with_acp_cmd(ConversationAgentKind::Claude, "claude --current");
    config.profiles.push(AcpProfile {
        id: "quant".into(),
        kind_id: "claude".into(),
        label: "Quant".into(),
        workspace_dir: "~/Claude Workspaces/new quant".into(),
    });

    let resolved = resolve_restart_launch(
        &current,
        Some("quant"),
        &config,
        ConversationAgentKind::Claude,
        false,
    );

    assert_eq!(
        resolved.command, "claude --current",
        "profile 会话重启时应沿用该 agent 当前配置的命令"
    );
    assert_eq!(
        resolved.env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
        Some("~/Claude Workspaces/new quant"),
        "profile 会话重启时应重新读取 profile 的当前 workspace 配置"
    );
}

#[test]
fn restart_keeps_persisted_launch_when_profile_was_deleted() {
    let current = ConversationLaunchSpec::from_command("claude --persisted")
        .with_env("CLAUDE_CONFIG_DIR", "~/Claude Workspaces/quant");

    let resolved = resolve_restart_launch(
        &current,
        Some("quant"),
        &AgentHostState::default(),
        ConversationAgentKind::Claude,
        false,
    );

    assert_eq!(resolved, current);
}

#[test]
fn restart_keeps_persisted_launch_when_profile_agent_is_unknown() {
    let current = ConversationLaunchSpec::from_command("future-agent --persisted")
        .with_env("FUTURE_HOME", "~/.future-agent");
    let mut config = AgentHostState::default();
    config.profiles.push(AcpProfile {
        id: "future-profile".into(),
        kind_id: "future-agent".into(),
        label: "Future Agent".into(),
        workspace_dir: "~/.future-agent".into(),
    });

    let resolved = resolve_restart_launch(
        &current,
        Some("future-profile"),
        &config,
        ConversationAgentKind::Claude,
        false,
    );

    assert_eq!(resolved, current);
}

#[test]
fn restart_refreshes_ordinary_session_from_current_agent_command() {
    let current = ConversationLaunchSpec::from_command("claude --stale");
    let config =
        AgentHostState::default().with_acp_cmd(ConversationAgentKind::Claude, "claude --current");

    let resolved =
        resolve_restart_launch(&current, None, &config, ConversationAgentKind::Claude, true);

    assert_eq!(
        resolved,
        ConversationLaunchSpec::from_command("claude --current")
    );
}

#[test]
fn restart_refresh_preserves_product_agent_instructions() {
    let current = ConversationLaunchSpec::from_command("smelt-pi-agent --stale").with_env(
        smelt_core::agent_kind::SMELT_AGENT_INSTRUCTIONS_ENV,
        "Review every change carefully",
    );
    let config =
        AgentHostState::default().with_acp_cmd(ConversationAgentKind::Pi, "smelt-pi-agent");

    let resolved = resolve_restart_launch(&current, None, &config, ConversationAgentKind::Pi, true);

    assert_eq!(resolved.command, "smelt-pi-agent");
    assert_eq!(
        resolved
            .env
            .get(smelt_core::agent_kind::SMELT_AGENT_INSTRUCTIONS_ENV)
            .map(String::as_str),
        Some("Review every change carefully")
    );
}

#[test]
fn restart_refresh_preserves_product_agent_plugin_and_mcp_selection() {
    use smelt_core::agent_kind::{SMELT_AGENT_MCP_SERVERS_ENV, SMELT_AGENT_PLUGIN_ARGS_ENV};
    let current = ConversationLaunchSpec::from_command("smelt-pi-agent --stale")
        .with_env(SMELT_AGENT_PLUGIN_ARGS_ENV, "[\"--no-extensions\"]")
        .with_env(SMELT_AGENT_MCP_SERVERS_ENV, "[\"rea\"]");
    let config =
        AgentHostState::default().with_acp_cmd(ConversationAgentKind::Pi, "smelt-pi-agent");

    let resolved = resolve_restart_launch(&current, None, &config, ConversationAgentKind::Pi, true);

    assert_eq!(resolved.command, "smelt-pi-agent");
    assert_eq!(
        resolved
            .env
            .get(SMELT_AGENT_PLUGIN_ARGS_ENV)
            .map(String::as_str),
        Some("[\"--no-extensions\"]"),
        "丢了插件参数，重启后就成了全量加载的裸 Pi"
    );
    assert_eq!(
        resolved
            .env
            .get(SMELT_AGENT_MCP_SERVERS_ENV)
            .map(String::as_str),
        Some("[\"rea\"]")
    );
}

#[test]
fn restart_refresh_preserves_product_agent_memory_directories() {
    use smelt_core::agent_kind::{SMELT_AGENT_MEMORY_DIR_ENV, SMELT_USER_MEMORY_DIR_ENV};
    let current = ConversationLaunchSpec::from_command("smelt-pi-agent")
        .with_env(
            SMELT_AGENT_MEMORY_DIR_ENV,
            "/home/me/.smelt/agents/a/memory",
        )
        .with_env(SMELT_USER_MEMORY_DIR_ENV, "/home/me/.smelt/memory");
    let config =
        AgentHostState::default().with_acp_cmd(ConversationAgentKind::Pi, "smelt-pi-agent");

    let resolved = resolve_restart_launch(&current, None, &config, ConversationAgentKind::Pi, true);

    assert_eq!(
        resolved
            .env
            .get(SMELT_AGENT_MEMORY_DIR_ENV)
            .map(String::as_str),
        Some("/home/me/.smelt/agents/a/memory")
    );
    assert_eq!(
        resolved
            .env
            .get(SMELT_USER_MEMORY_DIR_ENV)
            .map(String::as_str),
        Some("/home/me/.smelt/memory")
    );
}

#[test]
fn restart_keeps_legacy_launch_when_refresh_is_disabled() {
    let current =
        ConversationLaunchSpec::from_command("CLAUDE_CONFIG_DIR=~/Claude Workspaces/quant claude");
    let config =
        AgentHostState::default().with_acp_cmd(ConversationAgentKind::Claude, "claude --current");

    let resolved = resolve_restart_launch(
        &current,
        None,
        &config,
        ConversationAgentKind::Claude,
        false,
    );

    assert_eq!(resolved, current);
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
            request_config: smelt_core::acp_session::RuntimeDebugRequestConfig {
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
        compactions: vec![smelt_core::acp_session::RuntimeDebugCompaction {
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
            source_messages: vec![smelt_core::acp_session::RuntimeDebugCompactionMessage {
                segment: "summarized".into(),
                role: "user".into(),
                preview: "retain the auth constraint".into(),
                truncated: false,
            }],
            summary: Some("The user is debugging auth".into()),
            ..Default::default()
        }],
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
        },
    ];
    let tool_debug = std::collections::BTreeMap::from([(
        "sa-1-child-1".to_string(),
        smelt_core::acp_session::ToolCallDebug {
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
    assert!(events[3].preview.contains("工具名称: read"));
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
        smelt_core::acp_session::ToolCallDebug {
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
    assert_eq!(events[3].text, "搜索 icon");
    assert!(events[3].preview.contains("工具名称: grep"));
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
    assert!(tool.preview.contains("调用 ID: call-1"));
    assert!(tool.preview.contains("真实结果"));
    assert!(!tool.preview.contains("工具名称:"));
    assert!(!tool.preview.contains("参数\n"));
    assert!(!tool.preview.contains("未提供"));
    assert!(!tool.source.contains("provider_debug"));
}

#[test]
fn consecutive_same_kind_tools_collapse_into_one_run() {
    let entries = vec![
        AcpEntry::User("查一下".into()),
        completed_tool("s1", ToolKind::Search, "icon"),
        completed_tool("s2", ToolKind::Search, "graph"),
        completed_tool("s3", ToolKind::Search, "terminal"),
        completed_tool("r1", ToolKind::Read, "main.rs"),
    ];
    let run = consecutive_compact_tool_run(&entries, 2, 1, 5, None).unwrap();
    assert_eq!(run.kind, ToolKind::Search);
    assert_eq!(run.start, 1);
    assert_eq!(run.indices, vec![1, 2, 3]);
    assert_eq!(
        compact_tool_run_label(run.kind, run.indices.len()),
        "搜索了 3 次"
    );
    assert!(consecutive_compact_tool_run(&entries, 4, 1, 5, None).is_none());
}

fn thought(text: &str) -> AcpEntry {
    AcpEntry::Assistant {
        text: text.into(),
        thought: true,
    }
}

#[test]
fn thoughts_break_compact_tool_runs() {
    let entries = vec![
        AcpEntry::User("看实现".into()),
        completed_tool("r1", ToolKind::Read, "acp_view.rs"),
        thought("There's already format_duration"),
        completed_tool("r2", ToolKind::Read, "mod.rs"),
        thought("Sidecar timings on session snapshot is cleaner"),
        completed_tool("r3", ToolKind::Read, "apply.rs"),
        completed_tool("r4", ToolKind::Read, "tests.rs"),
        completed_tool("b1", ToolKind::Execute, "rg ProcessGroupInfo"),
    ];
    assert!(consecutive_compact_tool_run(&entries, 1, 1, 8, None).is_none());
    assert!(consecutive_compact_tool_run(&entries, 3, 1, 8, None).is_none());
    let run = consecutive_compact_tool_run(&entries, 5, 1, 8, None).unwrap();
    assert_eq!(run.kind, ToolKind::Read);
    assert_eq!(run.start, 5);
    assert_eq!(run.indices, vec![5, 6]);
    assert_eq!(
        compact_tool_run_label(run.kind, run.indices.len()),
        "读取了 2 个文件"
    );
    assert!(consecutive_compact_tool_run(&entries, 7, 1, 8, None).is_none());
}

#[test]
fn failed_tools_do_not_join_compact_runs() {
    let entries = vec![
        completed_tool("r1", ToolKind::Read, "a.rs"),
        AcpEntry::ToolCall {
            id: "r2".into(),
            title: "b.rs".into(),
            kind: ToolKind::Read,
            status: ToolCallStatus::Failed,
            output: vec![ToolOutputPart::Text("boom".into())],
            children: Vec::new(),
        },
        completed_tool("r3", ToolKind::Read, "c.rs"),
        completed_tool("r4", ToolKind::Read, "d.rs"),
    ];
    assert!(consecutive_compact_tool_run(&entries, 0, 0, 4, None).is_none());
    assert!(consecutive_compact_tool_run(&entries, 1, 0, 4, None).is_none());
    let run = consecutive_compact_tool_run(&entries, 2, 0, 4, None).unwrap();
    assert_eq!(run.start, 2);
    assert_eq!(run.indices, vec![2, 3]);
}

#[test]
fn spoken_progress_still_breaks_tool_runs() {
    let entries = vec![
        completed_tool("r1", ToolKind::Read, "a.rs"),
        AcpEntry::Assistant {
            text: "可以。我先看过程组有没有起止时间。".into(),
            thought: false,
        },
        completed_tool("r2", ToolKind::Read, "b.rs"),
        completed_tool("r3", ToolKind::Read, "c.rs"),
    ];
    assert!(consecutive_compact_tool_run(&entries, 0, 0, 4, None).is_none());
    let run = consecutive_compact_tool_run(&entries, 2, 0, 4, None).unwrap();
    assert_eq!(run.start, 2);
    assert_eq!(run.indices, vec![2, 3]);
}

#[test]
fn compact_tool_run_breaks_on_kind_change_or_progress() {
    let entries = vec![
        completed_tool("s1", ToolKind::Search, "a"),
        completed_tool("r1", ToolKind::Read, "b.rs"),
        completed_tool("s2", ToolKind::Search, "c"),
        AcpEntry::ToolCall {
            id: "s3".into(),
            title: "d".into(),
            kind: ToolKind::Search,
            status: ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
        },
        completed_tool("s4", ToolKind::Search, "e"),
        completed_tool("s5", ToolKind::Search, "f"),
    ];
    assert!(consecutive_compact_tool_run(&entries, 0, 0, 6, None).is_none());
    assert!(consecutive_compact_tool_run(&entries, 2, 0, 6, None).is_none());
    let run = consecutive_compact_tool_run(&entries, 5, 0, 6, None).unwrap();
    assert_eq!(run.start, 4);
    assert_eq!(run.indices, vec![4, 5]);
    assert_eq!(compact_tool_run_label(ToolKind::Read, 2), "读取了 2 个文件");
    assert_eq!(
        compact_tool_run_label(ToolKind::Execute, 3),
        "运行了 3 条命令"
    );
}

#[test]
fn compact_tool_headlines_use_verbs_like_opened_page() {
    assert_eq!(
        compact_tool_headline(
            ToolKind::Fetch,
            "https://www.ag-grid.com/vue-data-grid/grouping/"
        ),
        "打开了 ag-grid.com/vue-data-grid/grouping/"
    );
    assert_eq!(
        compact_tool_headline(ToolKind::Search, "ag-grid master detail"),
        "搜索 ag-grid master detail"
    );
    assert_eq!(
        compact_tool_headline(ToolKind::Read, "crates/smelt/src/main.rs"),
        "读取了 main.rs"
    );
    assert_eq!(
        compact_tool_headline(ToolKind::Execute, "cargo test -p smelt-acp-view"),
        "运行了 cargo test -p smelt-acp-view"
    );
}

#[test]
fn background_task_summary_counts_running_tasks_and_output_keeps_the_tail() {
    let running = BackgroundTaskView {
        id: "bg-1".into(),
        title: "测试".into(),
        command: "npm test".into(),
        status: BackgroundTaskStatus::Running,
        exit_code: None,
        output: String::new(),
        started_at_ms: 1,
        finished_at_ms: None,
        tool_call_id: None,
    };
    let done = BackgroundTaskView {
        status: BackgroundTaskStatus::Completed,
        ..running.clone()
    };
    assert_eq!(background_task_summary(1), "1 个后台任务");
    let subagent = BackgroundTaskView {
        id: "bg-2".into(),
        tool_call_id: Some("call-1".into()),
        ..running.clone()
    };
    assert!(background_task_running_for(
        &[running.clone(), subagent.clone()],
        "call-1"
    ));
    assert!(
        !background_task_running_for(&[running.clone()], "call-1"),
        "宿主自己起的命令不挂在任何工具调用上"
    );
    let finished = BackgroundTaskView {
        status: BackgroundTaskStatus::Stopped,
        ..subagent
    };
    assert!(
        !background_task_running_for(&[finished], "call-1"),
        "子智能体结束（含 Pi 被杀、连接断开）后卡片不能再转"
    );
    assert_eq!(
        running_background_tasks(&[running, done.clone()])
            .iter()
            .map(|task| task.id.as_str())
            .collect::<Vec<_>>(),
        ["bg-1"]
    );
    assert!(running_background_tasks(&[done]).is_empty());
    let output = (1..=12)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        background_task_output_lines(&output),
        ["5", "6", "7", "8", "9", "10", "11", "12"]
    );
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
        smelt_core::acp_chat::subagent_visible_title("Start subagent explorer"),
        "explorer"
    );
    assert_eq!(smelt_core::acp_chat::subagent_visible_title("Task"), "");
    assert_eq!(smelt_core::acp_chat::subagent_visible_title("subagent"), "");
    assert_eq!(smelt_core::acp_chat::subagent_visible_title(""), "");
    assert_eq!(
        smelt_core::acp_chat::subagent_visible_title("查登录"),
        "查登录"
    );
}

#[test]
fn completed_process_header_summarizes_tools_in_appearance_order() {
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
        process_group_header_label(&entries, group).as_deref(),
        Some("搜索了 3 次 · 读取了 1 次")
    );
}

#[test]
fn compact_diff_stats_sum_all_cached_diff_parts() {
    let cached = vec![
        Some(CachedDiff {
            lines: std::rc::Rc::new(Vec::new()),
            added: 3,
            removed: 1,
        }),
        None,
        Some(CachedDiff {
            lines: std::rc::Rc::new(Vec::new()),
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
    let image = smelt_core::acp_chat::AcpImage {
        mime: "image/png".into(),
        data_b64: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==".into(),
    };
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
// item（只有 max_w，没有 width），而 gpui-component 的 markdown 有序/无序列表
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
                    .child(smelt_ui::markdown_mermaid::markdown_view_clickable(
                        "list-bubble-test",
                        self.text.to_string(),
                    )),
            ),
        )
    }
}

#[gpui::test]
fn short_ordered_list_bubble_does_not_collapse_below_min_width(cx: &mut gpui::TestAppContext) {
    use gpui::{VisualTestContext, px};

    cx.update(gpui_component::init);
    // gpui_component::Root 构造时要装 macOS 无障碍 hit-test 转发器，需要真实
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
fn expanded_process_paints_as_header_progress_and_tool_timeline(cx: &mut gpui::TestAppContext) {
    use gpui::VisualTestContext;

    cx.update(gpui_component::init);
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
        },
        AcpEntry::Assistant {
            text: "已经改好过程组展示".into(),
            thought: false,
        },
    ];
    let (_view, cx) = cx.add_window_view(move |_window, cx| {
        let mut view = super::AcpView::placeholder(
            cx,
            super::AcpViewOrigin {
                agent: ConversationAgentKind::Claude,
                launch: ConversationLaunchSpec::from_command("true"),
                refresh_launch_from_settings: false,
                profile_id: None,
                cwd: Some("/tmp/smelt".into()),
                reason: "visual test".into(),
                entries,
                resume_session_id: None,
                saved_sid: Some("acp-process-visual-test".into()),
            },
        );
        view.phase = DaemonPhase::Idle;
        view.expanded_process_groups.insert(1);
        view
    });
    let cx: &mut VisualTestContext = cx;
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });

    let header = cx
        .debug_bounds("ACP_PROCESS_GROUP_1")
        .expect("process header should paint");
    let progress = cx
        .debug_bounds("ACP_PROGRESS_1")
        .expect("progress summary should paint");
    let tool = cx
        .debug_bounds("ACP_TOOL_COMPACT_2")
        .expect("completed tool should paint as a compact timeline row");
    let thought = cx
        .debug_bounds("ACP_PROGRESS_3")
        .expect("thought content should paint on the timeline");
    let read = cx
        .debug_bounds("ACP_TOOL_COMPACT_4")
        .expect("later read should stay a compact row");

    assert!(header.size.width > gpui::px(300.));
    assert!(progress.origin.y >= header.origin.y + header.size.height);
    assert!(tool.origin.y >= progress.origin.y + progress.size.height);
    assert!(thought.origin.y >= tool.origin.y + tool.size.height);
    assert!(read.origin.y >= thought.origin.y + thought.size.height);
}

#[gpui::test]
fn text_elicitation_inputs_are_ready_before_paint(cx: &mut gpui::TestAppContext) {
    use gpui::VisualTestContext;

    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        let mut view = super::AcpView::placeholder(
            cx,
            super::AcpViewOrigin {
                agent: ConversationAgentKind::Claude,
                launch: ConversationLaunchSpec::from_command("true"),
                refresh_launch_from_settings: false,
                profile_id: None,
                cwd: None,
                reason: "test placeholder".into(),
                entries: Vec::new(),
                resume_session_id: None,
                saved_sid: Some("acp-render-test".into()),
            },
        );
        view.elicitation = Some(PendingElicitation {
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

    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        let mut view = super::AcpView::placeholder(
            cx,
            super::AcpViewOrigin {
                agent: ConversationAgentKind::Claude,
                launch: ConversationLaunchSpec::from_command("true"),
                refresh_launch_from_settings: false,
                profile_id: None,
                cwd: None,
                reason: "test placeholder".into(),
                entries: Vec::new(),
                resume_session_id: None,
                saved_sid: Some("acp-elicit-visual-test".into()),
            },
        );
        view.elicitation = Some(PendingElicitation {
            message: "选几种水果看看".into(),
            fields: vec![ElicitFieldView {
                key: "value".into(),
                title: "选几种水果看看".into(),
                required: true,
                allow_custom_input: true,
                kind: ElicitFieldKindView::MultiSelect(vec![
                    ElicitOptionView {
                        label: "苹果".into(),
                    },
                    ElicitOptionView {
                        label: "香蕉".into(),
                    },
                    ElicitOptionView {
                        label: "橙子".into(),
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

#[gpui::test]
fn single_select_with_custom_answer_renders_a_submit_button(cx: &mut gpui::TestAppContext) {
    use gpui::VisualTestContext;

    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        let mut view = super::AcpView::placeholder(
            cx,
            super::AcpViewOrigin {
                agent: ConversationAgentKind::Claude,
                launch: ConversationLaunchSpec::from_command("true"),
                refresh_launch_from_settings: false,
                profile_id: None,
                cwd: None,
                reason: "test placeholder".into(),
                entries: Vec::new(),
                resume_session_id: None,
                saved_sid: Some("acp-custom-answer-test".into()),
            },
        );
        view.elicitation = Some(PendingElicitation {
            message: "下一步要我执行哪项？".into(),
            fields: vec![ElicitFieldView {
                key: "answer".into(),
                title: "请选择或输入自己的答案".into(),
                required: true,
                allow_custom_input: true,
                kind: ElicitFieldKindView::Select(vec![ElicitOptionView {
                    label: "暂时都不做".into(),
                }]),
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

    cx.debug_bounds("acp-elicit-submit")
        .expect("single-select cards with custom input need an explicit submit button");
}

#[gpui::test]
fn snapshot_agent_session_becomes_the_views_persisted_product_identity(
    cx: &mut gpui::TestAppContext,
) {
    use gpui::VisualTestContext;
    use smelt_core::acp_session::AcpSessionState;
    use smelt_core::conversation::ConversationStateSnapshot;
    use smelt_plugin_api::{
        AgentSessionBinding, ContributionId, PluginContributionRef, PluginId, PluginResourceId,
        PluginResourceRef, PluginResourceType,
    };

    cx.update(gpui_component::init);
    let binding = AgentSessionBinding {
        agent: PluginContributionRef {
            plugin_id: PluginId::new("com.example.quant").unwrap(),
            contribution_id: ContributionId::new("quant-agent").unwrap(),
        },
        controller: PluginContributionRef {
            plugin_id: PluginId::new("com.example.quant").unwrap(),
            contribution_id: ContributionId::new("quant-session").unwrap(),
        },
        instance: PluginResourceRef {
            plugin_id: PluginId::new("com.example.quant").unwrap(),
            resource_type: PluginResourceType::new("strategy").unwrap(),
            resource_id: PluginResourceId::new("strategy-1").unwrap(),
        },
    };
    let (view, cx) = cx.add_window_view(|_window, cx| {
        super::AcpView::placeholder(
            cx,
            super::AcpViewOrigin {
                agent: ConversationAgentKind::Codex,
                launch: ConversationLaunchSpec::from_command("true"),
                refresh_launch_from_settings: false,
                profile_id: None,
                cwd: None,
                reason: "test placeholder".into(),
                entries: Vec::new(),
                resume_session_id: None,
                saved_sid: Some("acp-agent-session-test".into()),
            },
        )
    });
    let cx: &mut VisualTestContext = cx;
    let mut snapshot = AcpSessionState::default().to_snapshot(false);
    snapshot.snapshot_revision = 1;
    snapshot.conversation_state = Some(ConversationStateSnapshot {
        binding: Some(smelt_core::conversation::ConversationBinding::Plugin {
            plugin_id: PluginId::new("com.example.quant").unwrap(),
            route: smelt_plugin_api::PluginInputRouteBinding {
                contribution_id: ContributionId::new("quant-input").unwrap(),
                context: serde_json::json!({"strategy_id": "strategy-1"}),
            },
        }),
        agent_session: Some(binding.clone()),
        pending_agent_preset: None,
    });
    cx.update(|_window, cx| {
        view.update(cx, |view, cx| view.apply_snapshot(snapshot, cx));
    });

    assert_eq!(
        view.read_with(cx, |view, _| view.agent_session_for_save()),
        Some(binding)
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.session_action_payload())
            .unwrap()
            .context["strategy_id"],
        "strategy-1"
    );
}

#[gpui::test]
fn stop_button_click_requests_cancel_then_force_restarts_on_second_click(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        super::AcpView::placeholder(
            cx,
            super::AcpViewOrigin {
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                refresh_launch_from_settings: false,
                profile_id: None,
                cwd: None,
                reason: "test placeholder".into(),
                entries: Vec::new(),
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

    cx.update(gpui_component::init);
    let (_view, cx) = cx.add_window_view(move |_window, cx| {
        let mut view = super::AcpView::placeholder(
            cx,
            super::AcpViewOrigin {
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                refresh_launch_from_settings: false,
                profile_id: None,
                cwd: Some("/tmp/smelt".into()),
                reason: "log viewport".into(),
                entries,
                resume_session_id: None,
                saved_sid: Some("acp-log-viewport".into()),
            },
        );
        view.phase = DaemonPhase::Idle;
        view.expanded_process_groups.insert(group_ix);
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
        },
    ];
    let layout = super::build_conversation_layout(&entries, false);
    let group_ix = layout
        .iter()
        .find_map(|entry| entry.process_group.map(|group| group.first))
        .expect("tool belongs to a process group");

    cx.update(gpui_component::init);
    let (_view, cx) = cx.add_window_view(move |_window, cx| {
        let mut view = super::AcpView::placeholder(
            cx,
            super::AcpViewOrigin {
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                refresh_launch_from_settings: false,
                profile_id: None,
                cwd: Some("/tmp/smelt".into()),
                reason: "popover scroll".into(),
                entries,
                resume_session_id: None,
                saved_sid: Some("acp-pop-scroll".into()),
            },
        );
        view.phase = DaemonPhase::Idle;
        view.expanded_process_groups.insert(group_ix);
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

    cx.simulate_event(gpui::ScrollWheelEvent {
        position: viewport.center(),
        delta: ScrollDelta::Lines(point(0., -40.)),
        modifiers: Default::default(),
        touch_phase: TouchPhase::Moved,
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

    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        let mut view = super::AcpView::placeholder(
            cx,
            super::AcpViewOrigin {
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                refresh_launch_from_settings: false,
                profile_id: None,
                cwd: Some("/tmp/smelt".into()),
                reason: "empty open frame".into(),
                entries: Vec::new(),
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
            view.rendered_markdown = build_markdown_cache(&view.entries, view.cwd.as_deref());
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
fn revealing_the_pinned_prompt_parks_it_below_the_title_island(cx: &mut gpui::TestAppContext) {
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
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        let mut view = super::AcpView::placeholder(
            cx,
            super::AcpViewOrigin {
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                refresh_launch_from_settings: false,
                profile_id: None,
                cwd: Some("/tmp/smelt".into()),
                reason: "pinned prompt".into(),
                entries,
                resume_session_id: None,
                saved_sid: Some("acp-pinned-prompt".into()),
            },
        );
        view.phase = DaemonPhase::Idle;
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

    cx.update(|_window, cx| view.update(cx, |view, cx| view.reveal_pinned_prompt(cx)));
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
    use smelt_core::acp_session::AcpSessionState;

    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|_window, cx| {
        super::AcpView::placeholder(
            cx,
            super::AcpViewOrigin {
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                refresh_launch_from_settings: false,
                profile_id: None,
                cwd: None,
                reason: "background tasks".into(),
                entries: Vec::new(),
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
        tool_call_id: None,
    };
    let mut snapshot = AcpSessionState::default().to_snapshot(false);
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

#[test]
fn live_elapsed_hangs_on_the_current_action_row() {
    let entries = vec![
        AcpEntry::User("查一下".into()),
        completed_tool("s1", ToolKind::Search, "icon"),
        completed_tool("s2", ToolKind::Search, "graph"),
        AcpEntry::ToolCall {
            id: "read-1".into(),
            title: "main.rs".into(),
            kind: ToolKind::Read,
            status: ToolCallStatus::InProgress,
            output: Vec::new(),
            children: Vec::new(),
        },
    ];
    let layout = build_conversation_layout_with_timings(&entries, true, 0, &[], Some(6_000));
    let group = layout[1].process_group.expect("进行中的工具要收成过程组");
    assert_eq!(super::live_elapsed_anchor(&entries, group, None), 3);
    assert_eq!(
        super::row_live_elapsed(true, 3, &entries, Some(group), group.elapsed_ms, None),
        Some(6_000)
    );
    assert_eq!(
        super::row_live_elapsed(true, 1, &entries, Some(group), group.elapsed_ms, None),
        None
    );

    let thought = vec![
        AcpEntry::User("想一下".into()),
        AcpEntry::Assistant {
            text: "先看调用方".into(),
            thought: true,
        },
    ];
    assert_eq!(
        super::row_live_elapsed(true, 1, &thought, None, Some(20_000), None),
        Some(20_000)
    );
    let answer = vec![
        AcpEntry::User("写下来".into()),
        AcpEntry::Assistant {
            text: "结论是这样".into(),
            thought: false,
        },
    ];
    assert_eq!(
        super::row_live_elapsed(true, 1, &answer, None, Some(20_000), None),
        None,
        "正文自己在往外长，不再另挂一行耗时"
    );
}

fn numbered_paragraphs(label: &str, count: usize) -> String {
    (0..count)
        .map(|n| format!("{label} 第 {n} 段"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 打开一个已有消息的会话视图。不等首帧快照，后续快照按增量尾片合并。
fn open_conversation<'a>(
    cx: &'a mut gpui::TestAppContext,
    sid: &'static str,
    entries: Vec<AcpEntry>,
    phase: DaemonPhase,
) -> (
    gpui::Entity<super::AcpView>,
    &'a mut gpui::VisualTestContext,
) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        let mut view = super::AcpView::placeholder(
            cx,
            super::AcpViewOrigin {
                agent: ConversationAgentKind::Pi,
                launch: ConversationLaunchSpec::from_command("true"),
                refresh_launch_from_settings: false,
                profile_id: None,
                cwd: Some("/tmp/smelt".into()),
                reason: "scroll".into(),
                entries,
                resume_session_id: None,
                saved_sid: Some(sid.into()),
            },
        );
        view.phase = phase;
        view.awaiting_initial_history_snapshot = false;
        view
    });
    (view, cx)
}

fn tail_snapshot(
    revision: u64,
    entries_offset: usize,
    entries: Vec<AcpEntry>,
    phase: DaemonPhase,
) -> smelt_core::acp_session::ConversationSnapshot {
    let mut snapshot = smelt_core::acp_session::AcpSessionState::default().to_snapshot(false);
    snapshot.snapshot_revision = revision;
    snapshot.entries_total = entries_offset + entries.len();
    snapshot.entries_offset = entries_offset;
    snapshot.entries = entries;
    snapshot.phase = phase;
    snapshot
}

fn wheel(position: gpui::Point<gpui::Pixels>, up: gpui::Pixels) -> gpui::ScrollWheelEvent {
    gpui::ScrollWheelEvent {
        position,
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), up)),
        modifiers: Default::default(),
        touch_phase: gpui::TouchPhase::Moved,
    }
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
            gpui::PlatformInput::ScrollWheel(wheel(viewport.center(), px(30.))),
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

/// 上一回合结束后，它的工具行收成一行「执行过程」，其余行高度变成 0。这些行早就
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
        cx.debug_bounds("ACP_TOOL_COMPACT_2").is_some(),
        "回合进行中，工具逐行展开"
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
        "滚到第一回合，看得到收起后的执行过程"
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
        super::AcpView::placeholder(
            cx,
            super::AcpViewOrigin {
                agent: ConversationAgentKind::Codex,
                launch: ConversationLaunchSpec::from_command("true"),
                refresh_launch_from_settings: false,
                profile_id: None,
                cwd: None,
                reason: "test placeholder".into(),
                entries: Vec::new(),
                resume_session_id: None,
                saved_sid: Some("acp-release-test".into()),
            },
        )
    });
    let weak = view.downgrade();
    drop(view);
    assert!(
        weak.upgrade().is_none(),
        "关掉会话后消息流不能再把 AcpView 留住"
    );
}
