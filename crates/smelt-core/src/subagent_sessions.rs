//! Pi 子智能体的子会话（`~/.smelt/subagents/<id>.jsonl`）跟着父会话记录走。
//!
//! 谁引用了它，以父会话 JSONL 里 `subagent` 工具结果的 `details.runs[].id` 为准，不另外
//! 记账：父会话回退、分叉出来的记录带着同一批 id，删掉最后一份引用它的记录，子会话才跟着删。

use std::collections::BTreeSet;
use std::path::Path;

/// 宿主分的子智能体 id：`sa-` 加 12 位小写十六进制。只认这种，才能拿它拼路径。
pub fn is_subagent_id(id: &str) -> bool {
    id.strip_prefix("sa-").is_some_and(|hex| {
        hex.len() == 12
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// 这个子智能体的子会话文件。不是宿主分的 id、或找不到 Smelt 主目录时返回 `None`。
pub fn session_file(id: &str) -> Option<std::path::PathBuf> {
    is_subagent_id(id)
        .then(|| smelt_model::agent_paths::subagent_session_file(id))
        .flatten()
}

/// 一份 Pi 会话记录引用的子智能体：只认 `subagent` 工具结果里的 `details.runs`。
pub fn referenced_subagent_ids(session_text: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for line in session_text.lines() {
        // 大多数行是普通消息，先按字面过滤，省掉解析。
        if !line.contains("\"subagent\"") {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(message) = entry.get("message") else {
            continue;
        };
        if message.get("role").and_then(serde_json::Value::as_str) != Some("toolResult")
            || message.get("toolName").and_then(serde_json::Value::as_str) != Some("subagent")
        {
            continue;
        }
        let runs = message
            .get("details")
            .and_then(|details| details.get("runs"))
            .and_then(serde_json::Value::as_array);
        for run in runs.into_iter().flatten() {
            if let Some(id) = run.get("id").and_then(serde_json::Value::as_str)
                && is_subagent_id(id)
            {
                ids.insert(id.to_string());
            }
        }
    }
    ids
}

/// 刚删掉的那份记录引用过 `released` 里的子智能体。`remaining` 里的记录（同一项目下的
/// 其余 Pi 记录，分叉、回退出来的都在这里）不再引用的，删掉它们的子会话。读不出来的
/// 记录当作仍在引用：宁可留下，不能错删。返回删掉了哪些。
pub fn remove_unreferenced(
    released: BTreeSet<String>,
    remaining: impl IntoIterator<Item = impl AsRef<Path>>,
    sessions_dir: &Path,
) -> Vec<String> {
    let mut unreferenced = released;
    for path in remaining {
        if unreferenced.is_empty() {
            break;
        }
        match std::fs::read_to_string(path.as_ref()) {
            Ok(text) => unreferenced.retain(|id| !text.contains(id.as_str())),
            Err(_) => return Vec::new(),
        }
    }
    let mut removed = Vec::new();
    for id in unreferenced {
        let Some(file) = smelt_model::agent_paths::subagent_session_file_in(sessions_dir, &id) else {
            continue;
        };
        match std::fs::remove_file(&file) {
            Ok(()) => removed.push(id),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => eprintln!("smelt: 删不掉子会话 {}：{error}", file.display()),
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_result(name: &str, runs: serde_json::Value) -> String {
        serde_json::json!({
            "type": "message",
            "message": {"role": "toolResult", "toolName": name, "details": {"runs": runs}},
        })
        .to_string()
    }

    #[test]
    fn only_well_formed_ids_name_a_child_session() {
        assert!(is_subagent_id("sa-e8fd14d08fd4"));
        assert!(!is_subagent_id("sa-E8FD14D08FD4"));
        assert!(!is_subagent_id("sa-../../etc"));
        assert!(!is_subagent_id("sa-e8fd14d08fd"));
        assert!(!is_subagent_id("bg-e8fd14d08fd4"));
    }

    #[test]
    fn a_record_references_the_runs_of_its_subagent_tool_results() {
        let text = [
            r#"{"type":"session","id":"pi-1"}"#.to_string(),
            tool_result(
                "subagent",
                serde_json::json!([{"id": "sa-aaaaaaaaaaaa"}, {"id": "sa-bbbbbbbbbbbb"}]),
            ),
            tool_result("read", serde_json::json!([{"id": "sa-cccccccccccc"}])),
            tool_result("subagent", serde_json::json!([{"id": "sub-1"}])),
            "not json \"subagent\"".to_string(),
        ]
        .join("\n");
        assert_eq!(
            referenced_subagent_ids(&text),
            BTreeSet::from(["sa-aaaaaaaaaaaa".to_string(), "sa-bbbbbbbbbbbb".to_string()])
        );
    }

    #[test]
    fn a_child_session_is_removed_only_when_no_remaining_record_references_it() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("subagents");
        std::fs::create_dir_all(&sessions).unwrap();
        for id in ["sa-aaaaaaaaaaaa", "sa-bbbbbbbbbbbb"] {
            std::fs::write(sessions.join(format!("{id}.jsonl")), "{}").unwrap();
        }
        // 分叉出来的那份记录还引用着 b。
        let fork = dir.path().join("fork.jsonl");
        std::fs::write(
            &fork,
            tool_result("subagent", serde_json::json!([{"id": "sa-bbbbbbbbbbbb"}])),
        )
        .unwrap();

        let released = BTreeSet::from(["sa-aaaaaaaaaaaa".to_string(), "sa-bbbbbbbbbbbb".to_string()]);
        let removed = remove_unreferenced(released, [&fork], &sessions);
        assert_eq!(removed, ["sa-aaaaaaaaaaaa"]);
        assert!(!sessions.join("sa-aaaaaaaaaaaa.jsonl").exists());
        assert!(sessions.join("sa-bbbbbbbbbbbb.jsonl").exists());
    }

    #[test]
    fn an_unreadable_record_keeps_every_child_session() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("subagents");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("sa-aaaaaaaaaaaa.jsonl"), "{}").unwrap();
        let released = BTreeSet::from(["sa-aaaaaaaaaaaa".to_string()]);
        let removed = remove_unreferenced(released, [dir.path().join("missing.jsonl")], &sessions);
        assert!(removed.is_empty());
        assert!(sessions.join("sa-aaaaaaaaaaaa.jsonl").exists());
    }
}
