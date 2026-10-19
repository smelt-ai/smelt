//! 侧栏里一个运行时的那一份（设计文档 P2「按运行时分空间」）。
//!
//! 项目的身份是根路径，只在一个运行时里唯一，所以项目列表、置顶、折叠，以及和这个
//! 运行时那份工作区目录的同步、冷启动恢复的进度，都按运行时各放一份。会话仍在
//! `Workspace::sessions` 一个列表里，各自从视图读出所属运行时。

use std::collections::HashSet;

use crate::SessionState;
use crate::runtimes::{Runtime, RuntimeKey};
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
    /// 侧栏里这台机器的分段，用户这次运行里亲手折起（`Some(true)`）或展开过。不落盘：
    /// 下次启动回到默认，见 [`crate::Workspace::runtime_folded`]。
    pub(crate) folded: Option<bool>,
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
            folded: None,
            catalog_loaded: false,
            catalog: WorkspaceCatalogSync::default(),
            write_queue: WorkspaceSnapshotWriteQueue::default(),
            restore_pending: Vec::new(),
            restore_retry_attempt: 0,
            sessions_restored: false,
        }
    }
}

impl crate::Workspace {
    /// 侧栏里这台机器的分段折没折。用户这次点过就按他点的；没点过的，远程机器折起、本机
    /// 展开：日常干活在本机，别的机器有事时标题上挂着状态点，要看再点开。
    pub(crate) fn runtime_folded(&self, key: &crate::runtimes::RuntimeKey) -> bool {
        self.space(key)
            .and_then(|space| space.folded)
            .unwrap_or(*key != crate::runtimes::RuntimeKey::Local)
    }

    pub(crate) fn toggle_runtime_fold(
        &mut self,
        key: &crate::runtimes::RuntimeKey,
        cx: &mut gpui::Context<Self>,
    ) {
        let folded = !self.runtime_folded(key);
        if let Some(space) = self.space_mut(key) {
            space.folded = Some(folded);
        }
        cx.notify();
    }

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

impl crate::Workspace {
    /// 远程运行时的空间跟着 [`crate::runtimes::RuntimeHub`] 走：新添加的建空间、读目录；
    /// 移除的连同它的会话一起从侧栏摘掉（运行时上的进程不动，那台机器不归这台桌面管）。
    pub(crate) fn sync_remote_spaces(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(hub) = cx.try_global::<crate::runtimes::RuntimeHub>() else {
            return;
        };
        let remotes: Vec<Runtime> = hub
            .remotes
            .iter()
            .map(|remote| remote.runtime.clone())
            .collect();
        let removed: Vec<RuntimeKey> = self
            .remote_spaces
            .iter()
            .map(|space| space.runtime.key().clone())
            .filter(|key| !remotes.iter().any(|runtime| runtime.key() == key))
            .collect();
        let mut changed = !removed.is_empty();
        for key in removed {
            let mut ixs = self.space_members(&key, cx);
            // 降序移除：前面的下标不受后面 remove 影响。
            ixs.sort_unstable_by(|a, b| b.cmp(a));
            for ix in ixs {
                self.remove_session(ix, false, cx);
            }
            self.remote_spaces
                .retain(|space| space.runtime.key() != &key);
            if self
                .active_project
                .as_ref()
                .is_some_and(|project| project.runtime == key)
            {
                self.active_project = None;
            }
        }
        for runtime in remotes {
            if self.space(runtime.key()).is_some() {
                continue;
            }
            changed = true;
            self.remote_spaces.push(RuntimeSpace::new(runtime.clone()));
            self.start_space_catalog_load(runtime, window, cx);
        }
        if changed {
            self.save_state(cx);
        }
        // 在线状态变了也要重画运行时标题。
        cx.notify();
    }
}

impl crate::Workspace {
    /// 恢复回来的会话已经不要了：运行时整个被移除了。
    pub(crate) fn restore_cancelled(&self, key: &RuntimeKey) -> bool {
        self.space(key).is_none()
    }

    pub(crate) fn forget_pending_restore(&mut self, key: &RuntimeKey, original_index: usize) {
        if let Some(space) = self.space_mut(key) {
            space
                .restore_pending
                .retain(|(index, _)| *index != original_index);
        }
    }

    /// 恢复的会话插在哪：按存档顺序排在这个运行时自己的会话里。返回（在这个运行时会话
    /// 里的位置，在整个会话列表里的下标）。几个运行时同时恢复时各排各的，互不打乱。
    pub(crate) fn restore_insert_at(
        &self,
        key: &RuntimeKey,
        restored_order: &[usize],
        original_index: usize,
        order_intact: &mut bool,
        cx: &gpui::App,
    ) -> (usize, usize) {
        let members = self.space_members(key, cx);
        let position = if *order_intact {
            crate::planned_restore_insert_position(restored_order, original_index, members.len())
                .unwrap_or_else(|| {
                    *order_intact = false;
                    members.len()
                })
        } else {
            members.len()
        };
        let insert_at = members.get(position).copied().unwrap_or_else(|| {
            members
                .last()
                .map(|last| last + 1)
                .unwrap_or(self.sessions.len())
        });
        (position, insert_at)
    }
}
