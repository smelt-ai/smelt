//! Smelt-owned metadata for durable ACP history identity.
//!
//! Agent transcripts remain the authority for conversation content and their generated title.
//! This store only overlays user-owned metadata that must survive closing the active workspace
//! projection.

use crate::agent_kind::AcpAgentKind;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
    sessions: Vec<SessionMetadata>,
}

fn store_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".smelt").join("session_metadata.json"))
}

fn load(path: &Path) -> SessionMetadataStore {
    crate::json_store::load_json(Some(path.to_path_buf()))
}

fn matches_identity(
    entry: &SessionMetadata,
    agent: AcpAgentKind,
    profile_id: Option<&str>,
    resume_id: &str,
) -> bool {
    entry.agent == agent.id()
        && entry.profile_id.as_deref() == profile_id
        && entry.resume_id == resume_id
}

fn custom_title_at(
    path: &Path,
    agent: AcpAgentKind,
    profile_id: Option<&str>,
    resume_id: &str,
) -> Option<String> {
    load(path)
        .sessions
        .into_iter()
        .find(|entry| matches_identity(entry, agent, profile_id, resume_id))
        .map(|entry| entry.custom_title)
}

fn custom_titles_at(
    path: &Path,
    agent: AcpAgentKind,
    profile_id: Option<&str>,
) -> HashMap<String, String> {
    load(path)
        .sessions
        .into_iter()
        .filter(|entry| entry.agent == agent.id() && entry.profile_id.as_deref() == profile_id)
        .map(|entry| (entry.resume_id, entry.custom_title))
        .collect()
}

/// Return the Smelt user title for one agent history session, if the user assigned one.
pub fn custom_title(
    agent: AcpAgentKind,
    profile_id: Option<&str>,
    resume_id: &str,
) -> Option<String> {
    let path = store_path()?;
    let _guard = STORE_LOCK.lock().unwrap();
    custom_title_at(&path, agent, profile_id, resume_id)
}

/// Load all Smelt user titles for one agent/profile history namespace in one disk read.
pub fn custom_titles(agent: AcpAgentKind, profile_id: Option<&str>) -> HashMap<String, String> {
    let Some(path) = store_path() else {
        return HashMap::new();
    };
    let _guard = STORE_LOCK.lock().unwrap();
    custom_titles_at(&path, agent, profile_id)
}

fn set_custom_title_at(
    path: &Path,
    agent: AcpAgentKind,
    profile_id: Option<&str>,
    resume_id: &str,
    custom_title: Option<&str>,
) -> Result<(), String> {
    let mut store = load(path);
    store
        .sessions
        .retain(|entry| !matches_identity(entry, agent, profile_id, resume_id));
    if let Some(custom_title) = custom_title.filter(|title| !title.trim().is_empty()) {
        let updated_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as u64);
        store.sessions.push(SessionMetadata {
            agent: agent.id().to_string(),
            profile_id: profile_id.map(String::from),
            resume_id: resume_id.to_string(),
            custom_title: custom_title.trim().to_string(),
            updated_at_ms,
        });
    }
    crate::json_store::save_json_atomic(Some(path.to_path_buf()), &store)
}

/// Set or clear the Smelt user title for one agent history session.
pub fn set_custom_title(
    agent: AcpAgentKind,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "smelt-session-metadata-{name}-{}.json",
            std::process::id()
        ))
    }

    #[test]
    fn titles_are_scoped_by_agent_profile_and_resume_id() {
        let path = temp_path("identity");
        let _ = std::fs::remove_file(&path);
        set_custom_title_at(
            &path,
            AcpAgentKind::Codex,
            Some("work"),
            "session-1",
            Some("人工名称"),
        )
        .unwrap();

        assert_eq!(
            custom_title_at(&path, AcpAgentKind::Codex, Some("work"), "session-1").as_deref(),
            Some("人工名称")
        );
        assert_eq!(
            custom_title_at(&path, AcpAgentKind::Codex, None, "session-1"),
            None
        );
        assert_eq!(
            custom_title_at(&path, AcpAgentKind::Claude, Some("work"), "session-1"),
            None
        );
        assert_eq!(
            custom_titles_at(&path, AcpAgentKind::Codex, Some("work"))
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
            AcpAgentKind::Claude,
            None,
            "session-1",
            Some("旧名称"),
        )
        .unwrap();
        set_custom_title_at(&path, AcpAgentKind::Claude, None, "session-1", None).unwrap();

        assert_eq!(
            custom_title_at(&path, AcpAgentKind::Claude, None, "session-1"),
            None
        );
        let _ = std::fs::remove_file(path);
    }
}
