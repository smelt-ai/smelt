//! `history.*` 控制方法背后的运行时实现：扫描、解析、删除各家 agent 的历史存档。
//!
//! workspace profile 的存档目录由运行时按 `profile_id` 自己解析（`agent_options`），
//! 客户端不传路径，桌面和手机看到的是同一份列表。

use std::path::Path;

use chrono::{DateTime, Utc};
use smelt_runtime_client::protocol::history::{
    HistoryDeleteParams, HistoryDetail, HistoryDetailParams, HistoryHandoffParams,
    HistoryListParams, HistoryListResult, HistorySession, HistoryTurn,
};

use crate::acp_chat::is_interrupt_marker;
use crate::agent_kind::{ConversationAgentKind, HistorySourceKind};
use crate::fs::{FileSystem as _, LocalFs};
use crate::session_handoff::{HandoffContext, HandoffPeer, build_handoff_prompt};
use crate::session_history::handoff_turns_from_history;

#[derive(Debug, PartialEq, Eq)]
pub enum HistoryError {
    UnknownAgent(String),
    /// 要删的存档不在这个项目的历史列表里（列表过期，或者路径不是历史存档）。
    NotInHistory(String),
    Io(String),
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownAgent(agent) => write!(f, "unknown history source: {agent}"),
            Self::NotInHistory(_) => f.write_str("The past session no longer exists"),
            Self::Io(message) => f.write_str(message),
        }
    }
}

fn source(agent: &str) -> Result<HistorySourceKind, HistoryError> {
    HistorySourceKind::from_id(agent).ok_or_else(|| HistoryError::UnknownAgent(agent.to_string()))
}

fn millis(at: Option<DateTime<Utc>>) -> Option<i64> {
    at.map(|at| at.timestamp_millis())
}

/// workspace profile 的存档目录。纯终端来源没有 profile。
fn override_dir(kind: HistorySourceKind, profile_id: Option<&str>) -> Option<String> {
    let acp = kind.acp()?;
    crate::session_control::find_agent_option_for(acp, profile_id)?.history_dir
}

pub fn list(params: &HistoryListParams) -> Result<HistoryListResult, HistoryError> {
    let kind = source(&params.agent)?;
    let profile_id = params.profile_id.as_deref();
    let sessions = crate::session_control::list_history_for(
        kind,
        profile_id,
        &params.cwd,
        override_dir(kind, profile_id).as_deref(),
    )
    .into_iter()
    .map(|session| HistorySession {
        path: session.path,
        resume_id: session.resume_id,
        title: session.title,
        custom_title: session.custom_title,
        started_at_ms: millis(session.started_at),
        last_active_at_ms: millis(session.last_active_at),
        message_count: session.message_count,
        total_tokens: session.total_tokens,
    })
    .collect();
    let agent_definition_ids = crate::session_metadata::agent_definition_ids(kind, profile_id)
        .into_iter()
        .collect();
    Ok(HistoryListResult {
        sessions,
        agent_definition_ids,
    })
}

/// 一份历史会话 `offset` 起的至多 `limit` 轮：长会话的全文几十 MB，客户端按页取。
pub fn detail(params: &HistoryDetailParams) -> Result<Option<HistoryDetail>, HistoryError> {
    let kind = source(&params.agent)?;
    let Some(detail) =
        crate::session_history::load_agent_session_detail(kind, Path::new(&params.path))
    else {
        return Ok(None);
    };
    let total = detail.turns.len();
    let turns = detail
        .turns
        .into_iter()
        .skip(params.offset)
        .take(params.limit.unwrap_or(usize::MAX))
        .map(|turn| HistoryTurn {
            is_user: turn.is_user,
            timestamp_ms: millis(turn.timestamp),
            interrupted: turn.is_user && is_interrupt_marker(&turn.text),
            text: if turn.text.trim().is_empty() {
                String::new()
            } else {
                turn.text
            },
            tools: turn.tools,
        })
        .collect();
    Ok(Some(HistoryDetail {
        turns,
        total,
        model: detail.model,
    }))
}

/// 一份历史会话压成交接给 `target_agent` 的首条 prompt。全文留在运行时：客户端只给路径，
/// 拿回的是按预算裁剪过的 prompt。
pub fn handoff(params: &HistoryHandoffParams) -> Result<Option<String>, HistoryError> {
    let kind = source(&params.agent)?;
    let source_agent = kind
        .acp()
        .ok_or_else(|| HistoryError::UnknownAgent(params.agent.clone()))?;
    let target_agent = ConversationAgentKind::from_id(&params.target_agent)
        .ok_or_else(|| HistoryError::UnknownAgent(params.target_agent.clone()))?;
    let Some(detail) =
        crate::session_history::load_agent_session_detail(kind, Path::new(&params.path))
    else {
        return Ok(None);
    };
    Ok(Some(build_handoff_prompt(&HandoffContext {
        turns: handoff_turns_from_history(&detail),
        source_title: &params.source_title,
        source: HandoffPeer::new(source_agent, params.source_profile_label.as_deref()),
        target: HandoffPeer::new(target_agent, params.target_profile_label.as_deref()),
        cwd: Some(&params.cwd),
        source_model: detail.model,
    })))
}

/// 删除一份历史存档。先确认它确实在这个项目的历史列表里，再删；删完同步清掉自定义
/// 标题，否则同一个 resume id 被复用时会继承旧标题。Pi 的记录还带走它引用、而同一
/// 项目下别的记录（分叉、回退出来的）不再引用的子智能体子会话。
pub fn delete(params: &HistoryDeleteParams) -> Result<(), HistoryError> {
    let kind = source(&params.agent)?;
    if !kind.history_deletable() {
        return Err(HistoryError::Io(format!(
            "{} past sessions can't be deleted here",
            kind.label()
        )));
    }
    let profile_id = params.profile_id.as_deref();
    let sessions = crate::session_control::list_history_for(
        kind,
        profile_id,
        &params.cwd,
        override_dir(kind, profile_id).as_deref(),
    );
    let target = Path::new(&params.path);
    let listed = sessions
        .iter()
        .any(|session| session.resume_id == params.resume_id && session.path == params.path);
    if !listed {
        return Err(HistoryError::NotInHistory(params.path.clone()));
    }
    let is_pi = kind == HistorySourceKind::Conversation(ConversationAgentKind::Pi);
    // 删之前读：删完就不知道它引用过谁了。
    let released = is_pi
        .then(|| std::fs::read_to_string(target).ok())
        .flatten()
        .map(|text| crate::subagent_sessions::referenced_subagent_ids(&text))
        .unwrap_or_default();
    delete_history_path(target)?;
    let _ = crate::session_metadata::set_custom_title(kind, profile_id, &params.resume_id, None);
    if !released.is_empty()
        && let Some(dir) = smelt_model::agent_paths::subagent_sessions_dir()
    {
        let remaining = sessions
            .iter()
            .filter(|session| session.path != params.path)
            .map(|session| session.path.clone());
        crate::subagent_sessions::remove_unreferenced(released, remaining, &dir);
    }
    Ok(())
}

/// 拒绝符号链接和没有文件名的路径，避免误删到更高层目录。
fn delete_history_path(path: &Path) -> Result<(), HistoryError> {
    if path.file_name().is_none() || path.parent().is_none() {
        return Err(HistoryError::Io("the past session path is invalid".into()));
    }
    let metadata = LocalFs
        .symlink_metadata(path)
        .ok_or_else(|| HistoryError::NotInHistory(path.display().to_string()))?;
    if metadata.is_symlink {
        return Err(HistoryError::Io(
            "refusing to delete a past session that is a symlink".into(),
        ));
    }
    let result = if metadata.is_dir {
        LocalFs.remove_dir_all(path)
    } else {
        LocalFs.remove_file(path)
    };
    result
        .map_err(|error| HistoryError::Io(format!("couldn't delete the session archive: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_paths::project_dir;

    fn sandbox(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smelt-history-service-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unknown_history_source_is_rejected() {
        let params = HistoryListParams {
            agent: "no-such-agent".into(),
            profile_id: None,
            cwd: "/x".into(),
        };
        assert_eq!(
            list(&params).unwrap_err(),
            HistoryError::UnknownAgent("no-such-agent".into())
        );
    }

    /// workspace profile 的目录里常有空格（`~/Claude Workspaces/quant`），读历史时
    /// 不能被拆断。
    #[test]
    fn history_reader_finds_workspace_override_with_spaces() {
        let home = sandbox("ovr");
        let workspace_dir = home.join("Claude Workspaces").join("quant");
        let project_root = workspace_dir.join("projects").join(project_dir("/x/y"));
        std::fs::create_dir_all(&project_root).unwrap();
        std::fs::write(
            project_root.join("quant.jsonl"),
            r#"{"type":"user","timestamp":"2026-07-05T00:00:00Z","message":{"content":"with spaces in override path"}}"#,
        )
        .unwrap();

        let sessions = crate::session_control::list_history_for(
            ConversationAgentKind::Claude.into(),
            None,
            "/x/y",
            Some(workspace_dir.to_str().unwrap()),
        );

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].display_title(), "with spaces in override path");
        let _ = std::fs::remove_dir_all(&home);
    }

    fn claude_archive(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = sandbox(name);
        let path = dir.join("s.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"type":"user","timestamp":"2026-07-01T00:00:00Z","message":{"content":"第一问"}}"#,
                r#"{"type":"assistant","timestamp":"2026-07-01T00:00:01Z","message":{"model":"claude-opus-5","content":[{"type":"text","text":"  "},{"type":"tool_use","name":"Bash"}]}}"#,
                r#"{"type":"user","timestamp":"2026-07-01T00:00:02Z","message":{"content":"[Request interrupted by user]"}}"#,
                r#"{"type":"user","timestamp":"2026-07-01T00:00:03Z","message":{"content":"第二问"}}"#,
                r#"{"type":"assistant","timestamp":"2026-07-01T00:00:04Z","message":{"content":[{"type":"text","text":"答完了"}]}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        (dir, path)
    }

    /// 按轮分页，`total` 是整份的轮数；中断标记、只有空白的正文由运行时认好。
    #[test]
    fn detail_pages_turns_and_marks_interrupts() {
        let (dir, path) = claude_archive("detail");
        let page = |offset, limit| {
            detail(&HistoryDetailParams {
                agent: "claude".into(),
                path: path.to_string_lossy().into_owned(),
                offset,
                limit,
            })
            .unwrap()
            .unwrap()
        };

        let first = page(0, Some(2));
        assert_eq!(first.total, 5);
        assert_eq!(first.model.as_deref(), Some("claude-opus-5"));
        let texts = |d: &HistoryDetail| -> Vec<(String, bool)> {
            d.turns
                .iter()
                .map(|t| (t.text.clone(), t.interrupted))
                .collect()
        };
        assert_eq!(
            texts(&first),
            [("第一问".into(), false), (String::new(), false)]
        );
        assert_eq!(first.turns[1].tools, ["Bash"]);
        let rest = page(2, None);
        assert_eq!(
            texts(&rest),
            [
                ("[Request interrupted by user]".into(), true),
                ("第二问".into(), false),
                ("答完了".into(), false),
            ]
        );
        assert!(page(9, Some(2)).turns.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 交接提示按存档路径在运行时拼好：原会话是谁由来源推出来，全文不经客户端。
    #[test]
    fn handoff_builds_the_prompt_from_the_archive() {
        let (dir, path) = claude_archive("handoff");
        let params = HistoryHandoffParams {
            agent: "claude".into(),
            path: path.to_string_lossy().into_owned(),
            source_title: "老会话".into(),
            source_profile_label: None,
            target_agent: "grok".into(),
            target_profile_label: None,
            cwd: "/repo".into(),
        };

        let prompt = handoff(&params).unwrap().unwrap();
        assert!(prompt.contains("「老会话」"), "{prompt}");
        assert!(prompt.contains("原会话由 Claude Code 进行"), "{prompt}");
        assert!(prompt.contains("现在由你（Grok）接手"), "{prompt}");
        assert!(prompt.contains("工作目录：/repo"), "{prompt}");
        assert!(
            prompt.contains("原会话使用的模型：claude-opus-5"),
            "{prompt}"
        );
        assert!(
            prompt.contains("第二问") && prompt.contains("答完了"),
            "{prompt}"
        );

        assert_eq!(
            handoff(&HistoryHandoffParams {
                target_agent: "nobody".into(),
                ..params.clone()
            })
            .unwrap_err(),
            HistoryError::UnknownAgent("nobody".into())
        );
        let gone = HistoryHandoffParams {
            path: dir.join("gone.jsonl").to_string_lossy().into_owned(),
            ..params
        };
        assert_eq!(handoff(&gone).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_a_path_outside_the_history_list_is_refused() {
        let dir = sandbox("delete");
        let victim = dir.join("not-history.jsonl");
        std::fs::write(&victim, "keep me").unwrap();

        let error = delete(&HistoryDeleteParams {
            agent: "claude".into(),
            profile_id: None,
            cwd: "/no/such/project".into(),
            resume_id: "x".into(),
            path: victim.to_string_lossy().into_owned(),
        })
        .unwrap_err();

        assert!(matches!(error, HistoryError::NotInHistory(_)));
        assert!(victim.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
