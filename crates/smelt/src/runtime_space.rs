//! 侧栏里一个运行时的那一份（设计文档 P2「按运行时分空间」）。
//!
//! 项目的身份是根路径，只在一个运行时里唯一，所以项目列表、置顶、折叠，以及和这个
//! 运行时那份工作区目录的同步、冷启动恢复的进度，都按运行时各放一份。会话仍在
//! `Workspace::sessions` 一个列表里，各自从视图读出所属运行时。

use std::collections::HashSet;

use crate::SessionState;
use crate::runtimes::Runtime;
use crate::workspace_persist::{WorkspaceCatalogSync, WorkspaceSnapshotWriteQueue};

pub(crate) struct RuntimeSpace {
    pub(crate) runtime: Runtime,
    /// 已打开的项目根目录（有序）。「打开项目」只往这里加一条、不建会话；项目下最后一个
    /// 会话关掉了，这一条也还在（侧栏显示 0 个会话的空项目）。要它消失只能显式「关闭
    /// 项目」。会话按 cwd 挂到项目下；挂不上的仍按自己的 cwd 自建隐式分组，不污染这份
    /// 列表。
    pub(crate) projects: Vec<String>,
    /// 侧栏里被折叠起来的项目（存 root 路径）。
    pub(crate) collapsed_projects: HashSet<String>,
    /// 侧栏被固定的项目根。固定后即使开了「隐藏无会话」也留在列表里。
    pub(crate) pinned_projects: HashSet<String>,
    /// 这个运行时那份工作区目录（项目、会话组）是否已经读到。读到之前不保存：内存里
    /// 的空列表不代表用户关光了会话。
    pub(crate) catalog_loaded: bool,
    /// 和运行时目录的版本同步，保存在后台线程推进。
    pub(crate) catalog: WorkspaceCatalogSync,
    /// 这份目录的唯一写入队列。退出也必须排到这里，不能旁路另起写者。
    pub(crate) write_queue: WorkspaceSnapshotWriteQueue,
    /// 冷启动时恢复失败的会话（运行时未就绪等）。仍写回目录，避免「恢复失败 → 写空
    /// 目录 → 会话永久蒸发」；当前进程会自动重试，用尽次数后下次冷启动再试。
    pub(crate) restore_pending: Vec<(usize, SessionState)>,
    /// 已经完成的失败恢复轮次。用来限制握手超时后的自动重试。
    pub(crate) restore_retry_attempt: u32,
    /// 用户在后台恢复期间删除的项目路径；尚未交货的恢复结果命中这些路径时直接丢弃。
    pub(crate) cancelled_restore_paths: Vec<String>,
    /// 冷启动的会话恢复是否已经跑完（没有待恢复会话时目录一到即为 true）。保存时的抹盘
    /// 安全阀靠它区分「还没恢复上来的空」和「用户真把会话全关了」。
    pub(crate) sessions_restored: bool,
}

impl RuntimeSpace {
    pub(crate) fn new(runtime: Runtime) -> Self {
        Self {
            runtime,
            projects: Vec::new(),
            collapsed_projects: HashSet::new(),
            pinned_projects: HashSet::new(),
            catalog_loaded: false,
            catalog: WorkspaceCatalogSync::default(),
            write_queue: WorkspaceSnapshotWriteQueue::default(),
            restore_pending: Vec::new(),
            restore_retry_attempt: 0,
            cancelled_restore_paths: Vec::new(),
            sessions_restored: false,
        }
    }
}

impl crate::Workspace {
    /// 某个运行时的空间。远程运行时被移除后就没有了。
    pub(crate) fn space(&self, key: &crate::runtimes::RuntimeKey) -> Option<&RuntimeSpace> {
        if *key == crate::runtimes::RuntimeKey::Local {
            return Some(&self.local);
        }
        self.remote_spaces
            .iter()
            .find(|space| space.runtime.key() == key)
    }

    pub(crate) fn space_mut(
        &mut self,
        key: &crate::runtimes::RuntimeKey,
    ) -> Option<&mut RuntimeSpace> {
        if *key == crate::runtimes::RuntimeKey::Local {
            return Some(&mut self.local);
        }
        self.remote_spaces
            .iter_mut()
            .find(|space| space.runtime.key() == key)
    }

    /// 本机在前，远程按登记顺序。
    pub(crate) fn space_keys(&self) -> Vec<crate::runtimes::RuntimeKey> {
        std::iter::once(&self.local)
            .chain(self.remote_spaces.iter())
            .map(|space| space.runtime.key().clone())
            .collect()
    }
}

/// 一个项目：所在运行时 + 根路径。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectRef {
    pub(crate) runtime: crate::runtimes::RuntimeKey,
    pub(crate) root: String,
}

impl crate::Workspace {
    /// 属于这个运行时的会话下标（升序）。
    pub(crate) fn space_members(
        &self,
        key: &crate::runtimes::RuntimeKey,
        cx: &gpui::App,
    ) -> Vec<usize> {
        self.sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| session.runtime(cx).key() == key)
            .map(|(ix, _)| ix)
            .collect()
    }

    /// 所有运行时的项目分组，按侧栏顺序（本机在前）铺平。
    pub(crate) fn all_project_groups(&self, cx: &gpui::App) -> Vec<crate::ProjectGroup> {
        self.space_keys()
            .iter()
            .flat_map(|key| self.project_groups(key, cx))
            .collect()
    }
}

impl crate::Workspace {
    /// 侧栏里会话从上到下的顺序：运行时依次铺开，每段内按当前分组方式。键盘切会话、
    /// Cmd+1~9 按它数，所见即所得。
    pub(crate) fn sidebar_session_order(&self, cx: &gpui::App) -> Vec<usize> {
        let statuses = self
            .sessions
            .iter()
            .map(|session| session.status(cx))
            .collect::<Vec<_>>();
        let last_updated_at = self
            .sessions
            .iter()
            .map(|session| session.effective_updated_at(cx))
            .collect::<Vec<_>>();
        self.space_keys()
            .iter()
            .flat_map(|key| {
                let members = self.space_members(key, cx);
                crate::sidebar_groups(
                    key,
                    self.sidebar_grouping,
                    self.project_groups(key, cx),
                    &statuses,
                    &last_updated_at,
                    &members,
                )
            })
            .flat_map(|group| group.sessions)
            .collect()
    }
}
