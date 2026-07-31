//! PC 侧栏对外暴露的纯数据快照。
//!
//! GPUI 和 Flutter 不共享控件，但必须共享项目分组、会话类型、显示标题和顺序。
//! PC 在保存 workspace.json 时生成这份快照；守护网关读取后按会话类型过滤。

use serde::{Deserialize, Serialize};

pub const WORKSPACE_MENU_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceMenuSnapshot {
    #[serde(default)]
    pub version: u32,
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
            projects,
            sessions,
        }
    }

    pub fn acp_session(&self, id: &str) -> Option<&WorkspaceMenuSession> {
        self.sessions
            .iter()
            .find(|session| session.id == id && session.kind == WorkspaceMenuSessionKind::Acp)
    }
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
                    agent: Some("codex".into()),
                },
            ],
        );

        assert!(snapshot.acp_session("terminal-running-codex").is_none());
        assert!(snapshot.acp_session("any-stable-id").is_some());
    }
}
