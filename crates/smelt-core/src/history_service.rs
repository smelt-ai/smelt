//! `history.*` 控制方法背后的运行时实现：扫描、解析、删除各家 agent 的历史存档。
//!
//! workspace profile 的存档目录由运行时按 `profile_id` 自己解析（`agent_options`），
//! 客户端不传路径，桌面和手机看到的是同一份列表。

use std::path::Path;

use chrono::{DateTime, Utc};
use smelt_runtime_client::protocol::history::{
    HistoryDeleteParams, HistoryDetail, HistoryListParams, HistoryListResult,
    HistoryRememberAgentParams, HistorySession, HistoryTitle, HistoryTitlesResult, HistoryTurn,
};

use crate::agent_kind::HistorySourceKind;
use crate::fs::{FileSystem as _, LocalFs};

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
            Self::UnknownAgent(agent) => write!(f, "未知的历史来源：{agent}"),
            Self::NotInHistory(_) => f.write_str("历史会话已不存在"),
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

pub fn detail(agent: &str, path: &str) -> Result<Option<HistoryDetail>, HistoryError> {
    let kind = source(agent)?;
    Ok(
        crate::session_history::load_agent_session_detail(kind, Path::new(path)).map(|detail| {
            HistoryDetail {
                turns: detail
                    .turns
                    .into_iter()
                    .map(|turn| HistoryTurn {
                        is_user: turn.is_user,
                        timestamp_ms: millis(turn.timestamp),
                        text: turn.text,
                        tools: turn.tools,
                        tool_paths: turn.tool_paths,
                    })
                    .collect(),
                model: detail.model,
            }
        }),
    )
}

/// 删除一份历史存档。先确认它确实在这个项目的历史列表里，再删；删完同步清掉自定义
/// 标题，否则同一个 resume id 被复用时会继承旧标题。Pi 的记录还带走它引用、而同一
/// 项目下别的记录（分叉、回退出来的）不再引用的子智能体子会话。
pub fn delete(params: &HistoryDeleteParams) -> Result<(), HistoryError> {
    let kind = source(&params.agent)?;
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
    let is_pi =
        kind == HistorySourceKind::Conversation(crate::agent_kind::ConversationAgentKind::Pi);
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
        return Err(HistoryError::Io("历史会话路径无效".into()));
    }
    let metadata = LocalFs
        .symlink_metadata(path)
        .ok_or_else(|| HistoryError::NotInHistory(path.display().to_string()))?;
    if metadata.is_symlink {
        return Err(HistoryError::Io("拒绝删除符号链接形式的历史会话".into()));
    }
    let result = if metadata.is_dir {
        LocalFs.remove_dir_all(path)
    } else {
        LocalFs.remove_file(path)
    };
    result.map_err(|error| HistoryError::Io(format!("删除会话存档失败：{error}")))
}

pub fn remember_agent(params: &HistoryRememberAgentParams) -> Result<(), HistoryError> {
    let kind = source(&params.agent)?;
    crate::session_metadata::remember_agent_definition(
        kind,
        params.profile_id.as_deref(),
        &params.resume_id,
        &params.agent_definition_id,
    )
    .map_err(HistoryError::Io)
}

pub fn titles() -> HistoryTitlesResult {
    HistoryTitlesResult {
        titles: crate::session_metadata::all_custom_titles()
            .into_iter()
            .map(|((agent, profile_id, resume_id), title)| HistoryTitle {
                agent,
                profile_id,
                resume_id,
                title,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_kind::ConversationAgentKind;
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
