//! PC 侧栏对外暴露的纯数据快照。
//!
//! GPUI 和 Flutter 不共享控件，但必须共享项目分组、会话类型、显示标题和顺序。
//! 桌面经 smeltd `workspace_menu` op 发布；网关只消费 event_subscribe，不读工作区快照。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const WORKSPACE_MENU_VERSION: u32 = 2;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceMenuSnapshot {
    #[serde(default)]
    pub version: u32,
    /// daemon 发布代数；0 表示尚未发布。与 schema `version` 无关。
    #[serde(default)]
    pub revision: u64,
    /// 发布者实例 id。桌面端在一次运行期间固定；daemon 用它识别同一发布者的迟到重发。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source_id: String,
    /// 发布者自己的单调序号。与 daemon 分配的 `revision` 分离，重连后仍能拒绝旧 payload。
    #[serde(default)]
    pub source_revision: u64,
    #[serde(default)]
    pub projects: Vec<WorkspaceMenuProject>,
    #[serde(default)]
    pub sessions: Vec<WorkspaceMenuSession>,
}

impl WorkspaceMenuSnapshot {
    pub fn current(
        projects: Vec<WorkspaceMenuProject>,
        sessions: Vec<WorkspaceMenuSession>,
    ) -> Self {
        Self {
            version: WORKSPACE_MENU_VERSION,
            revision: 0,
            source_id: String::new(),
            source_revision: 0,
            projects,
            sessions,
        }
    }

    pub fn with_source(mut self, source_id: impl Into<String>, source_revision: u64) -> Self {
        self.source_id = source_id.into();
        self.source_revision = source_revision;
        self
    }

    pub fn acp_session(&self, id: &str) -> Option<&WorkspaceMenuSession> {
        self.session(id)
            .filter(|session| session.kind == WorkspaceMenuSessionKind::Acp)
    }

    pub fn session(&self, id: &str) -> Option<&WorkspaceMenuSession> {
        self.sessions.iter().find(|session| session.id == id)
    }
}

fn workspace_menu_store() -> Result<smelt_store::Store, String> {
    if let Some(dir) = std::env::var_os("SMELT_WORKSPACE_MENU_DIR") {
        return crate::sqlite_state::open_sqlite_store(
            &PathBuf::from(dir).join(smelt_store::DATABASE_FILE_NAME),
        );
    }
    crate::sqlite_state::default_sqlite_store()
}

/// smeltd 启动时加载守护自己发布的侧栏菜单。
pub fn load_published_workspace_menu() -> WorkspaceMenuSnapshot {
    let Ok(store) = workspace_menu_store() else {
        return WorkspaceMenuSnapshot::default();
    };
    match store.get_workspace_menu_snapshot() {
        Ok(Some(snapshot)) => menu_from_snapshot(snapshot)
            .ok()
            .filter(|menu| menu.version > 0)
            .unwrap_or_default(),
        Ok(None) | Err(_) => WorkspaceMenuSnapshot::default(),
    }
}

pub fn persist_published_workspace_menu(menu: &WorkspaceMenuSnapshot) -> Result<(), String> {
    let store = workspace_menu_store()?;
    store
        .put_workspace_menu_snapshot(&snapshot_from_menu(menu)?)
        .map_err(Into::into)
}

fn snapshot_from_menu(
    menu: &WorkspaceMenuSnapshot,
) -> Result<smelt_store::PublishedWorkspaceMenuSnapshot, String> {
    Ok(smelt_store::PublishedWorkspaceMenuSnapshot {
        version: menu.version,
        revision: menu.revision,
        source_id: menu.source_id.clone(),
        source_revision: menu.source_revision,
        projects_json: serde_json::to_vec(&menu.projects).map_err(|error| error.to_string())?,
        sessions_json: serde_json::to_vec(&menu.sessions).map_err(|error| error.to_string())?,
    })
}

fn menu_from_snapshot(
    snapshot: smelt_store::PublishedWorkspaceMenuSnapshot,
) -> Result<WorkspaceMenuSnapshot, String> {
    Ok(WorkspaceMenuSnapshot {
        version: snapshot.version,
        revision: snapshot.revision,
        source_id: snapshot.source_id,
        source_revision: snapshot.source_revision,
        projects: serde_json::from_slice(&snapshot.projects_json)
            .map_err(|error| error.to_string())?,
        sessions: serde_json::from_slice(&snapshot.sessions_json)
            .map_err(|error| error.to_string())?,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceMenuProject {
    pub root: String,
    pub title: String,
    pub order: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMenuSessionKind {
    Terminal,
    Acp,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceMenuSession {
    pub id: String,
    pub kind: WorkspaceMenuSessionKind,
    pub title: String,
    /// true 表示用户在 PC 侧手动重命名，网关不得用自动标题覆盖。
    #[serde(default)]
    pub custom_title: bool,
    pub cwd: Option<String>,
    pub project_root: Option<String>,
    pub project_title: Option<String>,
    pub project_order: u32,
    pub session_order: u32,
    /// Pane order within a split terminal session. ACP and single-pane sessions use zero.
    #[serde(default)]
    pub leaf_order: u32,
    #[serde(default)]
    pub agent: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acp_filter_uses_explicit_kind_instead_of_id_or_agent_name() {
        let snapshot = WorkspaceMenuSnapshot::current(
            vec![],
            vec![
                WorkspaceMenuSession {
                    id: "terminal-running-codex".into(),
                    kind: WorkspaceMenuSessionKind::Terminal,
                    title: "Codex CLI".into(),
                    custom_title: false,
                    cwd: None,
                    project_root: None,
                    project_title: None,
                    project_order: 0,
                    session_order: 0,
                    leaf_order: 0,
                    agent: Some("codex".into()),
                },
                WorkspaceMenuSession {
                    id: "any-stable-id".into(),
                    kind: WorkspaceMenuSessionKind::Acp,
                    title: "ACP conversation".into(),
                    custom_title: false,
                    cwd: None,
                    project_root: None,
                    project_title: None,
                    project_order: 0,
                    session_order: 1,
                    leaf_order: 0,
                    agent: Some("codex".into()),
                },
            ],
        );

        assert!(snapshot.acp_session("terminal-running-codex").is_none());
        assert!(snapshot.acp_session("any-stable-id").is_some());
        assert_eq!(
            snapshot.session("terminal-running-codex").map(|s| s.kind),
            Some(WorkspaceMenuSessionKind::Terminal)
        );
    }

    #[test]
    fn published_menu_roundtrips_through_dedicated_file() {
        let dir = std::env::temp_dir().join(format!(
            "smelt-workspace-menu-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        unsafe { std::env::set_var("SMELT_WORKSPACE_MENU_DIR", &dir) };
        let mut menu = WorkspaceMenuSnapshot::current(
            vec![WorkspaceMenuProject {
                root: "/repo".into(),
                title: "repo".into(),
                order: 0,
            }],
            vec![],
        );
        menu.revision = 3;
        persist_published_workspace_menu(&menu).unwrap();
        let loaded = load_published_workspace_menu();
        unsafe { std::env::remove_var("SMELT_WORKSPACE_MENU_DIR") };
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(loaded, menu);
    }
}
