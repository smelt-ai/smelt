//! 线格式快照 → Dart 视图模型。只做投影，不做 IO；图片的 base64 在这里解成字节。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use smelt_model::acp_chat::{
    self, AcpEntry, AcpImage, TaskNote, ToolCallStatus, ToolKind as WireToolKind, ToolOutputPart,
};
use smelt_model::acp_conn::{ModelProviderGroup, ModelState, SessionConfigState};
use smelt_model::acp_session::{
    ApprovalDetailsView, ConversationSnapshot, ElicitFieldKindView, PendingElicitation,
    PendingPermission, PermissionOptionKindView, PlanEntryStatusView,
};
use smelt_model::daemon_state::DaemonPhase;

use crate::api::conversation::{
    ApprovalDetails, ApprovalKind, ChoiceOption, ConfigOption, ContextUsage, ConversationEntry,
    ConversationImage, ConversationPhase, ConversationView, DiffView, Elicitation,
    ElicitationField, ElicitationFieldKind, EntryKind, ImageRef, ModelProvider, ModelSelector,
    PermissionOption, PermissionOptionKind, PermissionRequest, PlanStep, PlanStepStatus,
    SlashCommand, TaskNoteKind, TaskNoteView, ToolCallView, ToolKind, ToolOutput, ToolOutputKind,
    ToolStatus,
};

pub(crate) fn conversation_view(
    session_id: &str,
    snapshot: &ConversationSnapshot,
) -> ConversationView {
    let offset = snapshot.entries_offset;
    let entries: Vec<ConversationEntry> = snapshot
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| conversation_entry(to_i64(offset + index), entry))
        .collect();
    let phase = phase(snapshot.phase);
    ConversationView {
        session_id: session_id.to_string(),
        phase,
        status_line: snapshot.status_line.clone(),
        end_reason: snapshot.end_reason.clone(),
        session_title: snapshot.session_title.clone(),
        turn_started_at_ms: snapshot.turn_started_at_ms.map(to_i64_u64),
        last_turn_duration_ms: snapshot
            .turn_timings
            .last()
            .and_then(|timing| timing.completed_elapsed_ms())
            .map(to_i64_u64),
        plan: snapshot
            .plan
            .iter()
            .flat_map(|plan| &plan.entries)
            .map(|step| PlanStep {
                content: step.content.clone(),
                status: match step.status {
                    PlanEntryStatusView::Pending => PlanStepStatus::Pending,
                    PlanEntryStatusView::InProgress => PlanStepStatus::InProgress,
                    PlanEntryStatusView::Completed => PlanStepStatus::Completed,
                },
            })
            .collect(),
        pending_permissions: permissions(&snapshot.pending_permissions),
        pending_elicitation: snapshot.pending_elicitation.as_ref().map(elicitation),
        final_answer_index: final_answer_index(phase, &entries),
        entries,
        entries_offset: to_i64(offset),
        has_more_before: offset > 0,
        supports_image: snapshot.supports_image,
        available_commands: snapshot
            .available_commands
            .iter()
            .map(|(name, description)| SlashCommand {
                name: name.clone(),
                description: description.clone(),
            })
            .collect(),
        usage: snapshot.usage.map(|(used, window)| ContextUsage {
            used_tokens: to_i64_u64(used),
            context_window: to_i64_u64(window),
        }),
        model: snapshot.model.as_ref().map(model_selector),
        config_options: snapshot.config_options.iter().map(config_option).collect(),
    }
}

fn to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn to_i64_u64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn phase(phase: DaemonPhase) -> ConversationPhase {
    match phase {
        DaemonPhase::Connecting => ConversationPhase::Connecting,
        DaemonPhase::Thinking => ConversationPhase::Thinking,
        DaemonPhase::ExecutingTool => ConversationPhase::ExecutingTool,
        DaemonPhase::AwaitingApproval => ConversationPhase::AwaitingApproval,
        DaemonPhase::WaitingForUser => ConversationPhase::WaitingForUser,
        DaemonPhase::Succeeded => ConversationPhase::Succeeded,
        DaemonPhase::Failed => ConversationPhase::Failed,
        DaemonPhase::Idle => ConversationPhase::Idle,
        DaemonPhase::Dead => ConversationPhase::Dead,
    }
}

/// 回合结束后，最后一条非空正文或完成信号是「最终回答」。
fn final_answer_index(phase: ConversationPhase, entries: &[ConversationEntry]) -> Option<u32> {
    let finished = matches!(
        phase,
        ConversationPhase::Idle
            | ConversationPhase::Succeeded
            | ConversationPhase::Failed
            | ConversationPhase::Dead
    );
    if !finished {
        return None;
    }
    entries
        .iter()
        .rposition(|entry| match entry.kind {
            EntryKind::Assistant => !entry.thought && !entry.text.trim().is_empty(),
            EntryKind::Completion => true,
            _ => false,
        })
        .map(to_u32)
}

fn blank_entry(index: i64, kind: EntryKind) -> ConversationEntry {
    ConversationEntry {
        index,
        kind,
        text: String::new(),
        thought: false,
        images: Vec::new(),
        tool_call: None,
        task_note: None,
    }
}

fn conversation_entry(index: i64, entry: &AcpEntry) -> ConversationEntry {
    match entry {
        AcpEntry::User(text) if acp_chat::is_interrupt_marker(text) => ConversationEntry {
            text: text.clone(),
            ..blank_entry(index, EntryKind::Interrupted)
        },
        AcpEntry::User(text) => ConversationEntry {
            text: text.clone(),
            ..blank_entry(index, EntryKind::User)
        },
        AcpEntry::UserWithImages { text, images } => ConversationEntry {
            text: text.clone(),
            images: images.iter().map(image).collect(),
            ..blank_entry(index, EntryKind::User)
        },
        AcpEntry::Assistant { text, thought } => ConversationEntry {
            text: text.clone(),
            thought: *thought,
            ..blank_entry(index, EntryKind::Assistant)
        },
        AcpEntry::ToolCall {
            id,
            title,
            kind,
            status,
            output,
            ..
        } => {
            let completion = acp_chat::is_task_completion_tool_title(title);
            ConversationEntry {
                text: if completion {
                    acp_chat::completion_summary_text(output)
                } else {
                    String::new()
                },
                tool_call: Some(ToolCallView {
                    id: id.clone(),
                    title: title.clone(),
                    kind: tool_kind(*kind),
                    status: tool_status(*status),
                    output: output.iter().map(tool_output).collect(),
                }),
                ..blank_entry(
                    index,
                    if completion {
                        EntryKind::Completion
                    } else {
                        EntryKind::ToolCall
                    },
                )
            }
        }
        AcpEntry::Divider(label) => ConversationEntry {
            text: label.clone(),
            ..blank_entry(index, EntryKind::Divider)
        },
        AcpEntry::TaskNote(note) => ConversationEntry {
            text: note.summary(),
            task_note: Some(task_note(note)),
            ..blank_entry(index, EntryKind::TaskNote)
        },
    }
}

/// 大图（`original` 有值）不内联；内联的 base64 解不开就当没有字节，界面画占位。
fn image(image: &AcpImage) -> ConversationImage {
    let original = image.original.as_ref().map(|original| ImageRef {
        entry: to_i64_u64(original.entry),
        digest: original.digest.clone(),
    });
    let bytes = (!image.data_b64.is_empty())
        .then(|| BASE64.decode(image.data_b64.as_bytes()).ok())
        .flatten();
    ConversationImage {
        mime: image.mime.clone(),
        bytes,
        original,
    }
}

fn tool_kind(kind: WireToolKind) -> ToolKind {
    match kind {
        WireToolKind::Read => ToolKind::Read,
        WireToolKind::Edit => ToolKind::Edit,
        WireToolKind::Delete => ToolKind::Delete,
        WireToolKind::Move => ToolKind::Move,
        WireToolKind::Search => ToolKind::Search,
        WireToolKind::Execute => ToolKind::Execute,
        WireToolKind::Think => ToolKind::Think,
        WireToolKind::Fetch => ToolKind::Fetch,
        WireToolKind::SwitchMode => ToolKind::SwitchMode,
        WireToolKind::Collaborate => ToolKind::Collaborate,
        WireToolKind::Review => ToolKind::Review,
        WireToolKind::Image => ToolKind::Image,
        WireToolKind::Compact => ToolKind::Compact,
        WireToolKind::Wait => ToolKind::Wait,
        WireToolKind::Other => ToolKind::Other,
    }
}

fn tool_status(status: ToolCallStatus) -> ToolStatus {
    match status {
        ToolCallStatus::Pending => ToolStatus::Pending,
        ToolCallStatus::InProgress => ToolStatus::InProgress,
        ToolCallStatus::Completed => ToolStatus::Completed,
        ToolCallStatus::Failed => ToolStatus::Failed,
    }
}

fn blank_output(kind: ToolOutputKind) -> ToolOutput {
    ToolOutput {
        kind,
        text: String::new(),
        diff: None,
        image: None,
        exit_code: None,
        truncated: false,
    }
}

fn tool_output(part: &ToolOutputPart) -> ToolOutput {
    match part {
        ToolOutputPart::Text(text) => ToolOutput {
            text: text.clone(),
            ..blank_output(ToolOutputKind::Text)
        },
        ToolOutputPart::Diff {
            path,
            old_text,
            new_text,
        } => ToolOutput {
            diff: Some(DiffView {
                path: path.clone(),
                old_text: old_text.clone(),
                new_text: new_text.clone(),
            }),
            ..blank_output(ToolOutputKind::Diff)
        },
        ToolOutputPart::Image(picture) => ToolOutput {
            image: Some(image(picture)),
            ..blank_output(ToolOutputKind::Image)
        },
        ToolOutputPart::Terminal {
            output,
            truncated,
            exit_code,
            ..
        } => ToolOutput {
            text: output.clone(),
            exit_code: *exit_code,
            truncated: *truncated,
            ..blank_output(ToolOutputKind::Terminal)
        },
    }
}

fn task_note(note: &TaskNote) -> TaskNoteView {
    TaskNoteView {
        kind: match note.kind {
            acp_chat::TaskNoteKind::BackgroundTask => TaskNoteKind::BackgroundTask,
            acp_chat::TaskNoteKind::Subagent => TaskNoteKind::Subagent,
        },
        id: note.id.clone(),
        title: note.title.clone(),
        status: note.status.clone(),
        exit_code: note.exit_code,
        output_tail: note.output_tail.clone(),
    }
}

pub(crate) fn permissions(pending: &[PendingPermission]) -> Vec<PermissionRequest> {
    pending.iter().map(permission).collect()
}

fn permission(pending: &PendingPermission) -> PermissionRequest {
    PermissionRequest {
        tool_call_id: pending.tool_call_id.clone(),
        question: pending.question.clone(),
        options: pending
            .options
            .iter()
            .map(|option| PermissionOption {
                option_id: option.option_id.clone(),
                name: option.name.clone(),
                kind: match option.kind {
                    PermissionOptionKindView::AllowOnce => PermissionOptionKind::AllowOnce,
                    PermissionOptionKindView::AllowAlways => PermissionOptionKind::AllowAlways,
                    PermissionOptionKindView::RejectOnce => PermissionOptionKind::RejectOnce,
                    PermissionOptionKindView::RejectAlways => PermissionOptionKind::RejectAlways,
                },
            })
            .collect(),
        details: approval_details(&pending.details),
    }
}

fn approval_details(details: &ApprovalDetailsView) -> ApprovalDetails {
    let blank = |kind| ApprovalDetails {
        kind,
        command: None,
        cwd: None,
        reason: None,
        grant_root: None,
        summary: None,
    };
    match details {
        ApprovalDetailsView::Command {
            command,
            cwd,
            reason,
        } => ApprovalDetails {
            command: Some(command.clone()),
            cwd: cwd.clone(),
            reason: reason.clone(),
            ..blank(ApprovalKind::Command)
        },
        ApprovalDetailsView::FileChange { reason, grant_root } => ApprovalDetails {
            reason: reason.clone(),
            grant_root: grant_root.clone(),
            ..blank(ApprovalKind::FileChange)
        },
        ApprovalDetailsView::Permissions { summary } => ApprovalDetails {
            summary: Some(summary.clone()),
            ..blank(ApprovalKind::Permissions)
        },
        ApprovalDetailsView::Generic => blank(ApprovalKind::Generic),
    }
}

pub(crate) fn elicitation(pending: &PendingElicitation) -> Elicitation {
    let fields: Vec<ElicitationField> = pending
        .fields
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let (kind, options, secret, url) = match &field.kind {
                ElicitFieldKindView::Select(options) => (
                    ElicitationFieldKind::Select,
                    options.iter().map(|option| option.label.clone()).collect(),
                    false,
                    None,
                ),
                ElicitFieldKindView::MultiSelect(options) => (
                    ElicitationFieldKind::MultiSelect,
                    options.iter().map(|option| option.label.clone()).collect(),
                    false,
                    None,
                ),
                ElicitFieldKindView::Text { secret } => {
                    (ElicitationFieldKind::Text, Vec::new(), *secret, None)
                }
                ElicitFieldKindView::ExternalUrl(url) => (
                    ElicitationFieldKind::ExternalUrl,
                    Vec::new(),
                    false,
                    Some(url.clone()),
                ),
            };
            ElicitationField {
                key: field.key.clone(),
                title: field.title.clone(),
                required: field.required,
                allow_custom_input: field.allow_custom_input,
                kind,
                options,
                secret,
                url,
                chosen: pending
                    .chosen
                    .get(&index)
                    .map(|chosen| chosen.iter().copied().map(to_u32).collect())
                    .unwrap_or_default(),
                text_value: pending.text_values.get(&index).cloned(),
            }
        })
        .collect();
    let is_ready = fields.iter().all(field_is_ready);
    Elicitation {
        message: pending.message.clone(),
        fields,
        is_ready,
    }
}

/// 和旧 Dart `PendingElicitation.isReady` 同一规则：只有必填项拦提交；选择题要选过，
/// 文字要非空白（允许自填的选择题，填了字也算答过）。
fn field_is_ready(field: &ElicitationField) -> bool {
    let typed = || {
        field
            .text_value
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
    };
    if !field.required {
        return true;
    }
    match field.kind {
        ElicitationFieldKind::Select | ElicitationFieldKind::MultiSelect => {
            !field.chosen.is_empty() || (field.allow_custom_input && typed())
        }
        ElicitationFieldKind::Text => typed(),
        ElicitationFieldKind::ExternalUrl => true,
    }
}

/// 当前 provider：先按模型值匹配，再按模型名，都对不上取第一组。与桌面
/// `selected_provider_group` 同一套判定。
fn selected_provider(model: &ModelState) -> Option<usize> {
    let groups = &model.provider_groups;
    groups
        .iter()
        .position(|group| {
            group
                .options
                .iter()
                .any(|(value, _)| value == &model.current_value)
        })
        .or_else(|| {
            groups.iter().position(|group| {
                group
                    .options
                    .iter()
                    .any(|(_, name)| name == &model.current_name)
            })
        })
        .or_else(|| (!groups.is_empty()).then_some(0))
}

/// 切到这个 provider 要写回的模型值：同名模型优先，否则组里第一个。
fn provider_switch_value(group: &ModelProviderGroup, current_name: &str) -> Option<String> {
    group
        .options
        .iter()
        .find(|(_, name)| name == current_name)
        .or_else(|| group.options.first())
        .map(|(value, _)| value.clone())
}

fn model_selector(model: &ModelState) -> ModelSelector {
    // 协议给了值就按值认当前项：分组里可能有同名模型，按名字会勾上好几个。
    let model_choices = |options: &[(String, String)]| -> Vec<ChoiceOption> {
        options
            .iter()
            .map(|(value, name)| ChoiceOption {
                value: value.clone(),
                name: name.clone(),
                selected: if model.current_value.is_empty() {
                    name == &model.current_name
                } else {
                    value == &model.current_value
                },
            })
            .collect()
    };
    let current = selected_provider(model);
    let providers = model
        .provider_groups
        .iter()
        .enumerate()
        .map(|(index, group)| ModelProvider {
            id: group.id.clone(),
            name: if group.name.is_empty() {
                group.id.clone()
            } else {
                group.name.clone()
            },
            options: model_choices(&group.options),
            switch_value: provider_switch_value(group, &model.current_name),
            selected: current == Some(index),
        })
        .collect();
    let options = match current {
        Some(index) => model_choices(&model.provider_groups[index].options),
        None => model_choices(&model.options),
    };
    ModelSelector {
        config_id: model.config_id.clone(),
        current_value: model.current_value.clone(),
        current_name: model.current_name.clone(),
        providers,
        current_provider: current.map(to_u32),
        options,
    }
}

fn config_option(config: &SessionConfigState) -> ConfigOption {
    ConfigOption {
        config_id: config.config_id.clone(),
        name: config.name.clone(),
        description: config.description.clone(),
        current_name: config.current_name.clone(),
        options: config
            .options
            .iter()
            .map(|(value, name)| ChoiceOption {
                value: value.clone(),
                name: name.clone(),
                selected: name == &config.current_name,
            })
            .collect(),
        boolean: config.boolean,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::conversation::merge::tests::snapshot_json;

    fn snapshot_from(patch: serde_json::Value) -> ConversationSnapshot {
        let mut value = snapshot_json(0, 0, 1, &[]);
        for (key, field) in patch.as_object().unwrap() {
            value[key] = field.clone();
        }
        let mut snapshot: ConversationSnapshot = serde_json::from_value(value).unwrap();
        crate::conversation::merge::normalize(&mut snapshot);
        snapshot
    }

    #[test]
    fn every_entry_kind_is_projected() {
        let small = BASE64.encode(b"png-bytes");
        let snapshot = snapshot_from(json!({
            "entries_offset": 10,
            "entries": [
                {"User": "你好"},
                {"User": "[Request interrupted by user]"},
                {"UserWithImages": {"text": "看图", "images": [
                    {"mime": "image/png", "data_b64": small},
                    {"mime": "image/jpeg", "data_b64": "", "original": {"entry": 12, "digest": "abc"}},
                ]}},
                {"Assistant": {"text": "想一想", "thought": true}},
                {"ToolCall": {"id": "t1", "title": "读文件", "kind": "read", "status": "completed", "output": [
                    {"Text": "内容"},
                    {"Diff": {"path": "a.rs", "old_text": null, "new_text": "fn main() {}"}},
                    {"Image": {"mime": "image/png", "data_b64": small}},
                    {"Terminal": {"id": "term", "output": "ok\n", "truncated": true, "exit_code": 0, "signal": null}},
                ]}},
                {"Divider": "重新开始"},
                {"TaskNote": {"kind": "subagent", "id": "b1", "title": "跑测试", "status": "failed", "exitCode": 1, "outputTail": "boom"}},
                {"ToolCall": {"id": "t2", "title": "task_complete", "kind": "other", "status": "completed", "output": [
                    {"Text": "```\n全部完成\n```"},
                ]}},
                {"Assistant": {"text": "  ", "thought": false}},
            ],
            "turn_timings": [
                {"user_index": 10, "started_at_ms": 1000, "ended_at_ms": 4500},
            ],
        }));
        let view = conversation_view("acp-1", &snapshot);
        assert_eq!(view.session_id, "acp-1");
        assert_eq!(view.entries_offset, 10);
        assert!(view.has_more_before);
        let kinds: Vec<EntryKind> = view.entries.iter().map(|entry| entry.kind).collect();
        assert_eq!(
            kinds,
            [
                EntryKind::User,
                EntryKind::Interrupted,
                EntryKind::User,
                EntryKind::Assistant,
                EntryKind::ToolCall,
                EntryKind::Divider,
                EntryKind::TaskNote,
                EntryKind::Completion,
                EntryKind::Assistant,
            ]
        );
        assert_eq!(view.entries[2].index, 12);

        let images = &view.entries[2].images;
        assert_eq!(images[0].bytes.as_deref(), Some(&b"png-bytes"[..]));
        assert_eq!(images[0].original, None);
        assert_eq!(images[1].bytes, None, "大图只给引用");
        assert_eq!(
            images[1].original,
            Some(ImageRef {
                entry: 12,
                digest: "abc".into()
            })
        );
        assert_eq!(images[1].mime, "image/jpeg");

        assert!(view.entries[3].thought);
        let tool = view.entries[4].tool_call.as_ref().unwrap();
        assert_eq!(tool.kind, ToolKind::Read);
        assert_eq!(tool.status, ToolStatus::Completed);
        let output_kinds: Vec<ToolOutputKind> = tool.output.iter().map(|part| part.kind).collect();
        assert_eq!(
            output_kinds,
            [
                ToolOutputKind::Text,
                ToolOutputKind::Diff,
                ToolOutputKind::Image,
                ToolOutputKind::Terminal
            ]
        );
        assert_eq!(tool.output[1].diff.as_ref().unwrap().old_text, None);
        assert!(tool.output[2].image.as_ref().unwrap().bytes.is_some());
        assert_eq!(tool.output[3].text, "ok\n");
        assert_eq!(tool.output[3].exit_code, Some(0));
        assert!(tool.output[3].truncated);

        assert_eq!(view.entries[5].text, "重新开始");
        let note = view.entries[6].task_note.as_ref().unwrap();
        assert_eq!(note.kind, TaskNoteKind::Subagent);
        assert_eq!(note.exit_code, Some(1));
        assert!(view.entries[6].text.contains("子智能体"));
        assert_eq!(view.entries[7].text, "全部完成");

        // 空白正文不算最终回答，往前找到完成信号。
        assert_eq!(view.final_answer_index, Some(7));
        assert_eq!(view.last_turn_duration_ms, Some(3500));
    }

    #[test]
    fn no_final_answer_while_the_turn_is_running() {
        let snapshot = snapshot_from(json!({
            "phase": "thinking",
            "turn_started_at_ms": 99,
            "entries": [{"Assistant": {"text": "还在写", "thought": false}}],
        }));
        let view = conversation_view("acp-1", &snapshot);
        assert_eq!(view.phase, ConversationPhase::Thinking);
        assert_eq!(view.final_answer_index, None);
        assert_eq!(view.turn_started_at_ms, Some(99));
    }

    #[test]
    fn approval_details_take_all_four_shapes() {
        let snapshot = snapshot_from(json!({
            "phase": "awaiting_approval",
            "pending_permissions": [
                {"question": "跑命令？", "tool_call_id": "t1", "details": {"kind": "command", "command": "ls", "cwd": "/tmp", "reason": null},
                 "options": [
                    {"option_id": "a", "name": "允许", "kind": "AllowOnce"},
                    {"option_id": "aa", "name": "总是允许", "kind": "AllowAlways"},
                    {"option_id": "r", "name": "拒绝", "kind": "RejectOnce"},
                    {"option_id": "rr", "name": "总是拒绝", "kind": "RejectAlways"},
                 ]},
                {"question": "改文件？", "tool_call_id": "t2", "options": [], "details": {"kind": "file_change", "reason": "修 bug", "grant_root": "/repo"}},
                {"question": "权限？", "tool_call_id": "t3", "options": [], "details": {"kind": "permissions", "summary": "网络"}},
                {"question": "旧快照", "tool_call_id": "t4", "options": []},
            ],
        }));
        let view = conversation_view("acp-1", &snapshot);
        let permissions = &view.pending_permissions;
        assert_eq!(permissions.len(), 4);
        assert_eq!(permissions[0].details.kind, ApprovalKind::Command);
        assert_eq!(permissions[0].details.command.as_deref(), Some("ls"));
        assert_eq!(permissions[0].details.cwd.as_deref(), Some("/tmp"));
        let kinds: Vec<PermissionOptionKind> = permissions[0]
            .options
            .iter()
            .map(|option| option.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                PermissionOptionKind::AllowOnce,
                PermissionOptionKind::AllowAlways,
                PermissionOptionKind::RejectOnce,
                PermissionOptionKind::RejectAlways
            ]
        );
        assert_eq!(permissions[1].details.kind, ApprovalKind::FileChange);
        assert_eq!(permissions[1].details.grant_root.as_deref(), Some("/repo"));
        assert_eq!(permissions[2].details.kind, ApprovalKind::Permissions);
        assert_eq!(permissions[2].details.summary.as_deref(), Some("网络"));
        assert_eq!(permissions[3].details.kind, ApprovalKind::Generic);
    }

    #[test]
    fn elicitation_fields_carry_choices_and_readiness() {
        let pending = json!({
            "message": "选一下",
            "fields": [
                {"key": "color", "title": "颜色", "required": true, "kind": {"Select": [{"label": "红"}, {"label": "绿"}]}},
                {"key": "tags", "title": "标签", "required": false, "kind": {"MultiSelect": [{"label": "a"}, {"label": "b"}]}},
                {"key": "name", "title": "名字", "kind": {"Text": {"secret": true}}},
                {"key": "login", "title": "登录", "required": true, "kind": {"ExternalUrl": "https://example.com"}},
            ],
            "chosen": {"0": [1]},
            "text_values": {},
        });
        let snapshot = snapshot_from(json!({"pending_elicitation": pending}));
        let view = conversation_view("acp-1", &snapshot);
        let elicitation = view.pending_elicitation.unwrap();
        let fields = &elicitation.fields;
        assert_eq!(fields[0].kind, ElicitationFieldKind::Select);
        assert_eq!(fields[0].options, ["红", "绿"]);
        assert_eq!(fields[0].chosen, [1]);
        assert_eq!(fields[1].kind, ElicitationFieldKind::MultiSelect);
        assert!(!fields[1].required);
        assert_eq!(fields[2].kind, ElicitationFieldKind::Text);
        assert!(fields[2].secret);
        assert!(fields[2].required, "旧快照缺 required 按必填");
        assert_eq!(fields[3].url.as_deref(), Some("https://example.com"));
        assert!(!elicitation.is_ready, "必填文字还没填");

        let mut filled = pending.clone();
        filled["text_values"] = json!({"2": "小明"});
        let snapshot = snapshot_from(json!({"pending_elicitation": filled}));
        let elicitation = conversation_view("acp-1", &snapshot)
            .pending_elicitation
            .unwrap();
        assert_eq!(elicitation.fields[2].text_value.as_deref(), Some("小明"));
        assert!(elicitation.is_ready);
    }

    #[test]
    fn models_are_grouped_by_provider() {
        let snapshot = snapshot_from(json!({
            "model": {
                "config_id": "model",
                "current_value": "openai/gpt",
                "current_name": "GPT",
                "options": [["anthropic/sonnet", "Sonnet"], ["openai/gpt", "GPT"], ["copilot/gpt", "GPT"]],
                "provider_groups": [
                    {"id": "anthropic", "name": "Anthropic", "options": [["anthropic/sonnet", "Sonnet"]]},
                    {"id": "openai", "name": "", "options": [["openai/gpt", "GPT"]]},
                    {"id": "copilot", "name": "Copilot", "options": [["copilot/sonnet", "Sonnet"], ["copilot/gpt", "GPT"]]},
                    {"id": "empty", "name": "Empty", "options": []},
                ],
            },
            "config_options": [
                {"config_id": "effort", "name": "推理", "description": null, "current_name": "高",
                 "options": [["low", "低"], ["high", "高"]]},
                {"config_id": "fast", "name": "快速", "description": "省钱", "current_name": "开",
                 "options": [], "boolean": true},
            ],
            "usage": [1200, 200000],
            "available_commands": [["help", "帮助"]],
        }));
        let view = conversation_view("acp-1", &snapshot);
        let model = view.model.unwrap();
        assert_eq!(model.current_provider, Some(1), "按值认出 openai");
        assert_eq!(model.providers[1].name, "openai", "没名字用 id");
        assert!(model.providers[1].selected);
        assert_eq!(
            model.providers[2].switch_value.as_deref(),
            Some("copilot/gpt"),
            "同名模型优先"
        );
        assert_eq!(
            model.providers[0].switch_value.as_deref(),
            Some("anthropic/sonnet"),
            "没有同名就取第一个"
        );
        assert_eq!(model.providers[3].switch_value, None);
        assert_eq!(model.options.len(), 1, "模型菜单只列当前 provider 的");
        assert!(model.options[0].selected);
        assert!(
            !model.providers[2].options[1].selected,
            "别的 provider 下的同名模型不算当前"
        );

        assert!(view.config_options[0].options[1].selected);
        assert_eq!(view.config_options[0].boolean, None);
        assert_eq!(view.config_options[1].boolean, Some(true));
        assert_eq!(
            view.usage,
            Some(ContextUsage {
                used_tokens: 1200,
                context_window: 200000
            })
        );
        assert_eq!(view.available_commands[0].name, "help");
    }

    #[test]
    fn provider_falls_back_to_name_then_first_group() {
        let model = |current_value: &str, current_name: &str| ModelState {
            config_id: "model".into(),
            current_value: current_value.into(),
            current_name: current_name.into(),
            options: Vec::new(),
            provider_groups: vec![
                ModelProviderGroup {
                    id: "a".into(),
                    name: "A".into(),
                    options: vec![("a/x".into(), "X".into())],
                },
                ModelProviderGroup {
                    id: "b".into(),
                    name: "B".into(),
                    options: vec![("b/y".into(), "Y".into())],
                },
            ],
        };
        assert_eq!(selected_provider(&model("", "Y")), Some(1));
        assert_eq!(selected_provider(&model("zzz", "zzz")), Some(0));
        let flat = ModelState {
            provider_groups: Vec::new(),
            options: vec![("m".into(), "M".into())],
            ..model("m", "M")
        };
        let selector = model_selector(&flat);
        assert_eq!(selector.current_provider, None);
        assert_eq!(selector.options.len(), 1);
        assert!(selector.options[0].selected);
    }
}
