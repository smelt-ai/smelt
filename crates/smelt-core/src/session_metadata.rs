//! Smelt-owned metadata for durable ACP history identity.
//!
//! Agent transcripts remain the authority for conversation content and their generated title.
//! This store overlays user-owned metadata that must survive closing the active workspace
//! projection：自定义标题，以及「这条历史属于哪个产品智能体」。

use crate::agent_kind::HistorySourceKind;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

static STORE_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct SessionMetadata {
    agent: String,
    #[serde(default)]
    profile_id: Option<String>,
    resume_id: String,
    custom_title: String,
    updated_at_ms: u64,
}


#[derive(Default, serde::Serialize, serde::Deserialize)]
struct SessionMetadataStore {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    sessions: Vec<SessionMetadata>,
}

fn store_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("SMELT_SESSION_METADATA_DIR") {
        return Some(PathBuf::from(dir).join(smelt_store::DATABASE_FILE_NAME));
    }
    smelt_paths::smelt_home().map(|home| home.join(smelt_store::DATABASE_FILE_NAME))
}

/// 标题覆盖落在主库 `smelt.sqlite3`。调用方若仍传旧 JSON 路径，解析到同目录主库，
/// 绝不把 JSON 文件本身当成 SQLite。
fn database_path(path: &Path) -> PathBuf {
    if path.file_name().and_then(|name| name.to_str()) == Some(smelt_store::DATABASE_FILE_NAME) {
        path.to_path_buf()
    } else {
        path.parent()
            .map(|parent| parent.join(smelt_store::DATABASE_FILE_NAME))
            .unwrap_or_else(|| path.to_path_buf())
    }
}

fn load(path: &Path) -> Result<SessionMetadataStore, String> {
    let store = crate::sqlite_state::open_sqlite_store(&database_path(path))?;
    match store.get_history_title_snapshot()? {
        Some(snapshot) => Ok(store_from_snapshot(snapshot)),
        None => Ok(SessionMetadataStore::default()),
    }
}

fn store_from_snapshot(snapshot: smelt_store::HistoryTitleSnapshot) -> SessionMetadataStore {
    SessionMetadataStore {
        schema_version: snapshot.schema_version,
        sessions: snapshot
            .sessions
            .into_iter()
            .map(|session| SessionMetadata {
                agent: session.agent,
                profile_id: if session.profile_id.is_empty() {
                    None
                } else {
                    Some(session.profile_id)
                },
                resume_id: session.resume_id,
                custom_title: session.custom_title,
                updated_at_ms: session.updated_at_ms.max(0) as u64,
            })
            .collect(),
    }
}

fn matches_identity(
    entry: &SessionMetadata,
    agent: HistorySourceKind,
    profile_id: Option<&str>,
    resume_id: &str,
) -> bool {
    entry.agent == agent.id()
        && entry.profile_id.as_deref() == profile_id
        && entry.resume_id == resume_id
}

fn custom_title_at(
    path: &Path,
    agent: HistorySourceKind,
    profile_id: Option<&str>,
    resume_id: &str,
) -> Result<Option<String>, String> {
    Ok(load(path)?
        .sessions
        .into_iter()
        .find(|entry| matches_identity(entry, agent, profile_id, resume_id))
        .map(|entry| entry.custom_title))
}

fn custom_titles_at(
    path: &Path,
    agent: HistorySourceKind,
    profile_id: Option<&str>,
) -> HashMap<String, String> {
    load(path)
        .map(|store| {
            store
                .sessions
                .into_iter()
                .filter(|entry| {
                    entry.agent == agent.id() && entry.profile_id.as_deref() == profile_id
                })
                .map(|entry| (entry.resume_id, entry.custom_title))
                .collect()
        })
        .unwrap_or_default()
}

/// Return the Smelt user title for one agent history session, if the user assigned one.
pub fn custom_title(
    agent: HistorySourceKind,
    profile_id: Option<&str>,
    resume_id: &str,
) -> Option<String> {
    let path = store_path()?;
    let _guard = STORE_LOCK.lock().unwrap();
    custom_title_at(&path, agent, profile_id, resume_id)
        .ok()
        .flatten()
}

/// Load all Smelt user titles for one agent/profile history namespace in one disk read.
pub fn custom_titles(
    agent: HistorySourceKind,
    profile_id: Option<&str>,
) -> HashMap<String, String> {
    let Some(path) = store_path() else {
        return HashMap::new();
    };
    let _guard = STORE_LOCK.lock().unwrap();
    custom_titles_at(&path, agent, profile_id)
}

/// Load all Smelt user titles across every agent/profile namespace in one disk read.
///
/// 侧栏要按「当前对话」显示名字，而一个窗口里同时可能挂着多家 provider 的终端；
/// 按 (agent, profile) 逐个读会把一次渲染放大成多次存储访问，这里一次读完由
/// 调用方缓存。
pub fn all_custom_titles() -> HashMap<(String, Option<String>, String), String> {
    let Some(path) = store_path() else {
        return HashMap::new();
    };
    let _guard = STORE_LOCK.lock().unwrap();
    all_custom_titles_at(&path)
}

fn all_custom_titles_at(path: &Path) -> HashMap<(String, Option<String>, String), String> {
    load(path)
        .map(|store| {
            store
                .sessions
                .into_iter()
                .map(|entry| {
                    (
                        (entry.agent, entry.profile_id, entry.resume_id),
                        entry.custom_title,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn set_custom_title_at(
    path: &Path,
    agent: HistorySourceKind,
    profile_id: Option<&str>,
    resume_id: &str,
    custom_title: Option<&str>,
) -> Result<(), String> {
    // 只改这一条，和工作区目录同一个事务推进版本号：侧栏里这段对话的名字按它现算。
    crate::sqlite_state::open_sqlite_store(&database_path(path))?
        .set_workspace_history_title(agent.id(), profile_id, resume_id, custom_title)?;
    Ok(())
}

/// Set or clear the Smelt user title for one agent history session.
pub fn set_custom_title(
    agent: HistorySourceKind,
    profile_id: Option<&str>,
    resume_id: &str,
    custom_title: Option<&str>,
) -> Result<(), String> {
    let Some(path) = store_path() else {
        return Err("找不到用户目录".into());
    };
    let _guard = STORE_LOCK.lock().unwrap();
    set_custom_title_at(&path, agent, profile_id, resume_id, custom_title)
}

fn history_agent_store(path: &Path) -> Result<smelt_store::Store, String> {
    crate::sqlite_state::open_sqlite_store(&database_path(path))
}

fn remember_agent_definition_at(
    path: &Path,
    agent: HistorySourceKind,
    profile_id: Option<&str>,
    resume_id: &str,
    agent_definition_id: &str,
) -> Result<(), String> {
    let resume_id = resume_id.trim();
    let agent_definition_id = agent_definition_id.trim();
    if resume_id.is_empty() || agent_definition_id.is_empty() {
        return Ok(());
    }
    history_agent_store(path)?
        .put_history_agent_binding(
            agent.id(),
            profile_id.unwrap_or(""),
            resume_id,
            agent_definition_id,
        )
        .map_err(Into::into)
}

fn agent_definition_id_at(
    path: &Path,
    agent: HistorySourceKind,
    profile_id: Option<&str>,
    resume_id: &str,
) -> Result<Option<String>, String> {
    let resume_id = resume_id.trim();
    if resume_id.is_empty() {
        return Ok(None);
    }
    Ok(history_agent_store(path)?
        .history_agent_binding(agent.id(), profile_id.unwrap_or(""), resume_id)?
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty()))
}

fn agent_definition_ids_at(
    path: &Path,
    agent: HistorySourceKind,
    profile_id: Option<&str>,
) -> HashMap<String, String> {
    let Ok(store) = history_agent_store(path) else {
        return HashMap::new();
    };
    store
        .history_agent_bindings(agent.id(), profile_id.unwrap_or(""))
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, id)| !id.trim().is_empty())
        .collect()
}

/// 把「这条历史会话属于哪个产品智能体」记在 `resume_id` 上。
///
/// 工作区快照只保活体会话；关掉后历史页只拿得到引擎的 session id。
/// 不记这份绑定，项目里用智能体开的对话就会被当成裸引擎续接，插件与人设都丢。
pub fn remember_agent_definition(
    agent: HistorySourceKind,
    profile_id: Option<&str>,
    resume_id: &str,
    agent_definition_id: &str,
) -> Result<(), String> {
    let Some(path) = store_path() else {
        return Err("找不到用户目录".into());
    };
    let _guard = STORE_LOCK.lock().unwrap();
    remember_agent_definition_at(&path, agent, profile_id, resume_id, agent_definition_id)
}

/// 查这条历史会话是否绑过产品智能体。
pub fn agent_definition_id(
    agent: HistorySourceKind,
    profile_id: Option<&str>,
    resume_id: &str,
) -> Option<String> {
    let path = store_path()?;
    let _guard = STORE_LOCK.lock().unwrap();
    agent_definition_id_at(&path, agent, profile_id, resume_id)
        .ok()
        .flatten()
}

/// 一次读出某引擎/某 profile 下全部历史会话的智能体绑定，供历史列表贴标签。
pub fn agent_definition_ids(
    agent: HistorySourceKind,
    profile_id: Option<&str>,
) -> HashMap<String, String> {
    let Some(path) = store_path() else {
        return HashMap::new();
    };
    let _guard = STORE_LOCK.lock().unwrap();
    agent_definition_ids_at(&path, agent, profile_id)
}

/// 历史续接要用的智能体 id：space 目录优先，否则查 `resume_id` 绑定。
pub fn resume_agent_definition_id(
    cwd: &Path,
    agent: HistorySourceKind,
    profile_id: Option<&str>,
    resume_id: &str,
) -> Option<String> {
    crate::agent_definition_store::agent_definition_id_for_space(cwd)
        .or_else(|| agent_definition_id(agent, profile_id, resume_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// 测试沿用 ACP 种类书写，这里取它们对应的历史来源身份。
    use crate::agent_kind::ConversationAgentKind as Acp;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smelt-session-metadata-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("session_metadata.json")
    }

    #[test]
    fn titles_are_scoped_by_agent_profile_and_resume_id() {
        let path = temp_path("identity");
        let _ = std::fs::remove_file(&path);
        set_custom_title_at(
            &path,
            HistorySourceKind::from(Acp::Codex),
            Some("work"),
            "session-1",
            Some("人工名称"),
        )
        .unwrap();

        assert_eq!(
            custom_title_at(
                &path,
                HistorySourceKind::from(Acp::Codex),
                Some("work"),
                "session-1"
            )
            .unwrap()
            .as_deref(),
            Some("人工名称")
        );
        assert_eq!(
            custom_title_at(
                &path,
                HistorySourceKind::from(Acp::Codex),
                None,
                "session-1"
            )
            .unwrap(),
            None
        );
        assert_eq!(
            custom_title_at(
                &path,
                HistorySourceKind::from(Acp::Claude),
                Some("work"),
                "session-1"
            )
            .unwrap(),
            None
        );
        assert_eq!(
            custom_titles_at(&path, HistorySourceKind::from(Acp::Codex), Some("work"))
                .get("session-1")
                .map(String::as_str),
            Some("人工名称")
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn clearing_a_title_removes_the_overlay() {
        let path = temp_path("clear");
        let _ = std::fs::remove_file(&path);
        set_custom_title_at(
            &path,
            HistorySourceKind::from(Acp::Claude),
            None,
            "session-1",
            Some("旧名称"),
        )
        .unwrap();
        set_custom_title_at(
            &path,
            HistorySourceKind::from(Acp::Claude),
            None,
            "session-1",
            None,
        )
        .unwrap();

        assert_eq!(
            custom_title_at(
                &path,
                HistorySourceKind::from(Acp::Claude),
                None,
                "session-1"
            )
            .unwrap(),
            None
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn bulk_read_returns_every_namespace_and_survives_a_corrupt_store() {
        let path = temp_path("bulk");
        let _ = std::fs::remove_file(&path);
        assert!(
            all_custom_titles_at(&path).is_empty(),
            "没有覆盖层时必须给空表，而不是失败"
        );

        set_custom_title_at(
            &path,
            HistorySourceKind::from(Acp::Codex),
            Some("work"),
            "s1",
            Some("甲"),
        )
        .unwrap();
        set_custom_title_at(
            &path,
            HistorySourceKind::from(Acp::Claude),
            None,
            "s1",
            Some("乙"),
        )
        .unwrap();

        let all = all_custom_titles_at(&path);
        assert_eq!(all.len(), 2, "同一个 resume id 在不同 agent 下必须各自成键");
        assert_eq!(
            all.get(&(
                HistorySourceKind::from(Acp::Codex).id().to_string(),
                Some("work".to_string()),
                "s1".to_string()
            ))
            .map(String::as_str),
            Some("甲")
        );
        assert_eq!(
            all.get(&(
                HistorySourceKind::from(Acp::Claude).id().to_string(),
                None,
                "s1".to_string()
            ))
            .map(String::as_str),
            Some("乙")
        );

        std::fs::write(&path, "{ not json").unwrap();
        let all = all_custom_titles_at(&path);
        assert_eq!(all.len(), 2, "SQLite 已有值时残留坏 JSON 不能覆盖主库");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn corrupt_store_does_not_get_overwritten() {
        let path = temp_path("corrupt");
        let _ = std::fs::remove_file(&path);
        let database = database_path(&path);
        std::fs::write(&database, "{ not json").unwrap();
        let original = std::fs::read_to_string(&database).unwrap();
        let error = set_custom_title_at(
            &path,
            HistorySourceKind::from(Acp::Claude),
            None,
            "session-1",
            Some("不该写进去"),
        )
        .expect_err("损坏文件必须拒绝写入");
        assert!(!error.is_empty());
        assert_eq!(std::fs::read_to_string(&database).unwrap(), original);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn agent_bindings_are_scoped_by_agent_profile_and_resume_id() {
        let path = temp_path("agent-binding");
        remember_agent_definition_at(
            &path,
            HistorySourceKind::from(Acp::Pi),
            None,
            "session-1",
            "writer",
        )
        .unwrap();
        remember_agent_definition_at(
            &path,
            HistorySourceKind::from(Acp::Pi),
            Some("work"),
            "session-1",
            "quant",
        )
        .unwrap();

        assert_eq!(
            agent_definition_id_at(&path, HistorySourceKind::from(Acp::Pi), None, "session-1")
                .unwrap(),
            Some("writer".to_string())
        );
        assert_eq!(
            agent_definition_id_at(
                &path,
                HistorySourceKind::from(Acp::Pi),
                Some("work"),
                "session-1"
            )
            .unwrap(),
            Some("quant".to_string())
        );
        assert_eq!(
            agent_definition_id_at(
                &path,
                HistorySourceKind::from(Acp::Claude),
                None,
                "session-1"
            )
            .unwrap(),
            None
        );
        assert_eq!(
            agent_definition_ids_at(&path, HistorySourceKind::from(Acp::Pi), None).get("session-1"),
            Some(&"writer".to_string())
        );
        assert_eq!(
            agent_definition_ids_at(&path, HistorySourceKind::from(Acp::Pi), Some("work"))
                .get("session-1"),
            Some(&"quant".to_string())
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn blank_agent_bindings_are_ignored() {
        let path = temp_path("blank-agent-binding");
        remember_agent_definition_at(&path, HistorySourceKind::from(Acp::Pi), None, "", "writer")
            .unwrap();
        remember_agent_definition_at(
            &path,
            HistorySourceKind::from(Acp::Pi),
            None,
            "session-1",
            "  ",
        )
        .unwrap();
        assert!(agent_definition_ids_at(&path, HistorySourceKind::from(Acp::Pi), None).is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
