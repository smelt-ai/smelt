//! 桌面发布给运行时（再转给移动端）的侧栏菜单：项目和会话的展示投影。存取在运行时
//! （smelt-core）。

use serde::{Deserialize, Serialize};

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
