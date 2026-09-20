//! 自动化 Run 的完整对话存档。
//!
//! 对话存在 SQLite 表 `automation_run_transcript`，随 `automation_run` 级联删除。
//! 不进 JSON 文件，也不进 Pi 的交互 session 库。

use std::collections::HashSet;

use smelt_store::Store;

use crate::acp_chat::{AcpEntry, AcpImage, ToolCallStatus, ToolOutputPart};

pub fn save_run_transcript_on(
    store: &Store,
    run_id: &str,
    entries: &[AcpEntry],
) -> Result<(), String> {
    let archived = archive_entries(entries);
    let encoded = serde_json::to_vec(&archived)
        .map_err(|error| format!("序列化自动化 Run 对话失败: {error}"))?;
    store
        .put_automation_run_transcript(run_id.trim(), &encoded)
        .map_err(Into::into)
}

pub fn load_run_transcript_on(store: &Store, run_id: &str) -> Option<Vec<AcpEntry>> {
    let raw = store.get_automation_run_transcript(run_id.trim()).ok()??;
    serde_json::from_slice(&raw).ok()
}

pub fn retain_run_transcripts_on(store: &Store, keep: &HashSet<String>) -> Result<(), String> {
    store
        .retain_automation_run_transcripts(keep)
        .map_err(Into::into)
}

pub fn load_run_transcript_markdown(run_id: &str) -> Option<String> {
    let store = crate::sqlite_state::default_sqlite_store().ok()?;
    let entries = load_run_transcript_on(&store, run_id)?;
    let markdown = render_run_transcript(&entries);
    (!markdown.trim().is_empty()).then_some(markdown)
}

pub fn render_run_transcript(entries: &[AcpEntry]) -> String {
    let mut out = String::new();
    for entry in entries {
        render_entry(entry, &mut out, 0);
    }
    out
}

fn archive_entries(entries: &[AcpEntry]) -> Vec<AcpEntry> {
    entries.iter().map(archive_entry).collect()
}

fn archive_entry(entry: &AcpEntry) -> AcpEntry {
    match entry {
        AcpEntry::UserWithImages { text, images } => AcpEntry::UserWithImages {
            text: text.clone(),
            images: images
                .iter()
                .map(|image| AcpImage {
                    mime: image.mime.clone(),
                    data_b64: String::new(),
                })
                .collect(),
        },
        AcpEntry::ToolCall {
            id,
            title,
            kind,
            status,
            output,
            children,
        } => AcpEntry::ToolCall {
            id: id.clone(),
            title: title.clone(),
            kind: *kind,
            status: *status,
            output: output.clone(),
            children: children.iter().map(archive_entry).collect(),
        },
        other => other.clone(),
    }
}

fn render_entry(entry: &AcpEntry, out: &mut String, depth: usize) {
    let heading = if depth == 0 { "###" } else { "####" };
    match entry {
        AcpEntry::User(text) => {
            push_block(out, heading, "用户", text);
        }
        AcpEntry::UserWithImages { text, images } => {
            let mut body = text.clone();
            if !images.is_empty() {
                if !body.trim().is_empty() {
                    body.push_str("\n\n");
                }
                body.push_str(&format!("（{} 张图片，未写入存档）", images.len()));
            }
            push_block(out, heading, "用户", &body);
        }
        AcpEntry::Assistant { text, thought } => {
            let title = if *thought { "思考" } else { "助手" };
            push_block(out, heading, title, text);
        }
        AcpEntry::ToolCall {
            title,
            status,
            output,
            children,
            ..
        } => {
            let status = tool_status_label(*status);
            push_block(
                out,
                heading,
                &format!("工具 · {title} · {status}"),
                &render_tool_output(output),
            );
            for child in children {
                render_entry(child, out, depth.saturating_add(1));
            }
        }
        AcpEntry::Divider(label) => {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("---\n");
            if !label.trim().is_empty() {
                out.push_str(label.trim());
                out.push_str("\n\n");
            }
        }
    }
}

fn push_block(out: &mut String, heading: &str, title: &str, body: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(heading);
    out.push(' ');
    out.push_str(title);
    out.push_str("\n\n");
    let body = body.trim();
    if body.is_empty() {
        out.push_str("（无内容）\n");
    } else {
        out.push_str(body);
        out.push('\n');
    }
}

fn render_tool_output(parts: &[ToolOutputPart]) -> String {
    if parts.is_empty() {
        return String::new();
    }
    let mut body = String::new();
    for part in parts {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        match part {
            ToolOutputPart::Text(text) => body.push_str(text.trim()),
            ToolOutputPart::Diff {
                path,
                old_text,
                new_text,
            } => {
                body.push_str("```diff\n");
                body.push_str(&format!("# {path}\n"));
                if let Some(old_text) = old_text {
                    for line in old_text.lines() {
                        body.push('-');
                        body.push_str(line);
                        body.push('\n');
                    }
                }
                for line in new_text.lines() {
                    body.push('+');
                    body.push_str(line);
                    body.push('\n');
                }
                body.push_str("```");
            }
            ToolOutputPart::Terminal {
                output,
                exit_code,
                truncated,
                ..
            } => {
                if let Some(exit_code) = exit_code {
                    body.push_str(&format!("exit {exit_code}\n"));
                }
                body.push_str(output.trim());
                if *truncated {
                    body.push_str("\n…（输出已截断）");
                }
            }
        }
    }
    body
}

fn tool_status_label(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "等待",
        ToolCallStatus::InProgress => "进行中",
        ToolCallStatus::Completed => "完成",
        ToolCallStatus::Failed => "失败",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp_chat::{ToolKind, ToolOutputPart};
    use std::fs;
    use std::path::PathBuf;

    fn temp_store(tag: &str) -> (PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!(
            "smelt-automation-transcript-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        let store = Store::open_or_create(dir.join(smelt_store::DATABASE_FILE_NAME)).unwrap();
        (dir, store)
    }

    fn seed_runs(store: &Store, run_ids: &[&str]) {
        let context = serde_json::to_vec(&serde_json::json!({
            "automation_name": "A",
            "cwd": "/",
            "permission_mode": "full_access",
            "action": {"type": "shell", "command": "true", "args": []}
        }))
        .unwrap();
        let runs = run_ids
            .iter()
            .enumerate()
            .map(|(index, id)| smelt_store::AutomationRunRecord {
                id: (*id).to_string(),
                automation_id: "auto-1".into(),
                source: "manual".into(),
                status: "completed".into(),
                created_at: index as i64 + 1,
                scheduled_for: None,
                started_at: None,
                delivery_attempt_at: None,
                delivery_attempts: 0,
                finished_at: None,
                session_id: None,
                provider_session_id: None,
                output: None,
                error: None,
                runtime_released_at: None,
                context_json: context.clone(),
            })
            .collect();
        store
            .put_automation_snapshot(&smelt_store::AutomationSnapshot {
                schema_version: 1,
                store_id: "s".into(),
                revision: 1,
                timezone_fingerprint: "UTC".into(),
                automations: vec![smelt_store::AutomationRecord {
                    id: "auto-1".into(),
                    name: "A".into(),
                    enabled: true,
                    workspace_dir: None,
                    trigger_json: b"{}".to_vec(),
                    action_json: serde_json::to_vec(&serde_json::json!({
                        "type": "shell",
                        "command": "true",
                        "args": []
                    }))
                    .unwrap(),
                    sinks_json: b"[]".to_vec(),
                }],
                states: Vec::new(),
                runs,
            })
            .unwrap();
    }

    #[test]
    fn transcript_round_trips_and_strips_image_payloads() {
        let (dir, store) = temp_store("roundtrip");
        seed_runs(&store, &["run-1"]);
        let entries = vec![
            AcpEntry::User("看盘".into()),
            AcpEntry::UserWithImages {
                text: "附图".into(),
                images: vec![AcpImage {
                    mime: "image/png".into(),
                    data_b64: "iVBORw0KGgo=".into(),
                }],
            },
            AcpEntry::Assistant {
                text: "结论".into(),
                thought: false,
            },
        ];

        save_run_transcript_on(&store, "run-1", &entries).unwrap();
        let loaded = load_run_transcript_on(&store, "run-1").unwrap();
        let AcpEntry::UserWithImages { images, .. } = &loaded[1] else {
            panic!("应保留带图用户消息");
        };
        assert!(images[0].data_b64.is_empty());
        let markdown = render_run_transcript(&loaded);
        assert!(markdown.contains("看盘"));
        assert!(markdown.contains("结论"));
        assert!(markdown.contains("1 张图片"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn schedule_run_ids_are_stored_as_sqlite_keys() {
        let (dir, store) = temp_store("schedule");
        seed_runs(&store, &["schedule:auto-1:1710000000"]);
        save_run_transcript_on(
            &store,
            "schedule:auto-1:1710000000",
            &[AcpEntry::User("tick".into())],
        )
        .unwrap();
        let loaded = load_run_transcript_on(&store, "schedule:auto-1:1710000000").unwrap();
        assert!(matches!(loaded.as_slice(), [AcpEntry::User(text)] if text == "tick"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn retain_deletes_pruned_runs_only() {
        let (dir, store) = temp_store("retain");
        seed_runs(&store, &["keep", "drop"]);
        save_run_transcript_on(&store, "keep", &[AcpEntry::User("a".into())]).unwrap();
        save_run_transcript_on(&store, "drop", &[AcpEntry::User("b".into())]).unwrap();
        retain_run_transcripts_on(&store, &HashSet::from(["keep".to_string()])).unwrap();
        assert!(load_run_transcript_on(&store, "keep").is_some());
        assert!(load_run_transcript_on(&store, "drop").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_empty_run_ids() {
        let (dir, store) = temp_store("empty");
        assert!(save_run_transcript_on(&store, "", &[]).is_err());
        assert!(save_run_transcript_on(&store, "\0id", &[]).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn leftover_sidecar_files_are_not_imported() {
        let (dir, store) = temp_store("leftover-sidecar");
        seed_runs(&store, &["run-1"]);
        let sidecar = dir.join("automation-runs");
        fs::create_dir_all(&sidecar).unwrap();
        fs::write(
            sidecar.join("run-1.json"),
            serde_json::to_vec(&[AcpEntry::User("旧文件".to_string())]).unwrap(),
        )
        .unwrap();
        assert!(load_run_transcript_on(&store, "run-1").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_calls_render_into_the_markdown_audit() {
        let markdown = render_run_transcript(&[AcpEntry::ToolCall {
            id: "t1".into(),
            title: "bash".into(),
            kind: ToolKind::Execute,
            status: ToolCallStatus::Completed,
            output: vec![ToolOutputPart::Text("ok".into())],
            children: Vec::new(),
        }]);
        assert!(markdown.contains("工具 · bash · 完成"));
        assert!(markdown.contains("ok"));
    }
}
