//! 文件树上的 git 标记（改过、新增、未跟踪），和请运行时监听项目目录。
//!
//! 「变更」是插件，自己取自己的 status；文件树要标记就自己取一份，只取挂出来的根。
//! 缓存靠事件标旧（`project.changed`、重连、窗口回到前台），看得见时才重取。

use std::collections::HashSet;

use gpui::*;
use smelt_runtime_client::protocol::files::{ProjectRootParams, ProjectWatchCall};
use smelt_runtime_client::protocol::git::{GitRootParams, GitStatusCall, GitStatusData};

use crate::{ProjectRef, Workspace};

/// 一个根的 git status。取回来之前旧值照常显示。
pub(crate) struct GitMarks {
    pub(crate) value: GitStatusData,
    pub(crate) stale: bool,
}

impl Workspace {
    /// 请那台机器的运行时监听这个项目，有变化就推 `project.changed`。
    ///
    /// 每个项目只登记一次，失败了（连不上、旧运行时没有这个方法）也算登记过：那台机器
    /// 重新订阅上（[`Self::on_runtime_resubscribed_files`]）之前不再试。收不到事件的那段
    /// 时间靠窗口回到前台兜底（[`Self::on_window_refocused`]）。
    pub(crate) fn ensure_watch(&mut self, root: ProjectRef, cx: &mut Context<Self>) {
        if !self.watches.insert(root.clone()) {
            return;
        }
        let Ok(client) = crate::runtime_call::target(&root.runtime, cx) else {
            return;
        };
        cx.background_executor()
            .spawn(async move {
                let params = ProjectRootParams {
                    root: root.root.clone(),
                };
                let watched = crate::runtime_call::call_on::<ProjectWatchCall>(
                    &client,
                    params,
                    crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                );
                if let Err(error) = watched {
                    eprintln!(
                        "[files] 请运行时监听 {} 失败：{}",
                        root.root,
                        crate::runtime_call::error_text(error)
                    );
                }
            })
            .detach();
    }

    /// 运行时报 `project.changed{root}`：这个项目下的目录列表、git 标记标旧，打开着的文件
    /// 没改过就重读。监听是递归的，事件只带项目根。没登记过的（旧连接的遗留）不理。
    pub(crate) fn on_project_changed(&mut self, project: ProjectRef, cx: &mut Context<Self>) {
        if !self.watches.contains(&project) {
            return;
        }
        self.mark_project_dirs_stale(&project, cx);
        self.on_open_file_project_changed(&project, cx);
        let style = crate::runtimes::path_style(&project.runtime, cx);
        self.mark_git_marks_stale(&|key| {
            key.runtime == project.runtime && style.starts_with(&key.root, &project.root)
        });
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 某台机器重新订阅上了（新连上、重连、换了进程）。它替我们建的监听可能随旧进程没了，
    /// 断开期间的变化也收不到：清掉登记记录，缓存全部标旧，下一次可见刷新重新登记、重取。
    pub(crate) fn on_runtime_resubscribed_files(
        &mut self,
        runtime: &crate::RuntimeKey,
        cx: &mut Context<Self>,
    ) {
        self.mark_everything_stale(Some(runtime), cx);
        self.watches.retain(|root| &root.runtime != runtime);
        self.schedule_visible_refresh(cx);
    }

    /// 窗口回到前台。监听收不到的变化（inotify 数量用完、登记失败）在这里补上。
    pub(crate) fn on_window_refocused(&mut self, cx: &mut Context<Self>) {
        self.mark_everything_stale(None, cx);
        self.schedule_visible_refresh(cx);
    }

    /// 不知道哪里变了：`runtime` 那台机器（`None` 是所有机器）上的缓存全部标旧，打开着的
    /// 文件按登记过监听的项目各查一次。
    fn mark_everything_stale(
        &mut self,
        runtime: Option<&crate::RuntimeKey>,
        cx: &mut Context<Self>,
    ) {
        let on_runtime = |key: &ProjectRef| runtime.is_none_or(|runtime| &key.runtime == runtime);
        let projects: Vec<ProjectRef> = self
            .watches
            .iter()
            .filter(|root| on_runtime(root))
            .cloned()
            .collect();
        for project in &projects {
            self.on_open_file_project_changed(project, cx);
        }
        self.mark_git_marks_stale(&on_runtime);
        self.mark_runtime_dirs_stale(runtime);
    }

    /// 选中的根的 git 标记标旧，正在取的那次回来也不算新。
    fn mark_git_marks_stale(&mut self, matches: &dyn Fn(&ProjectRef) -> bool) {
        let roots: HashSet<ProjectRef> = self
            .git_status
            .keys()
            .chain(self.git_status_inflight.iter())
            .filter(|root| matches(root))
            .cloned()
            .collect();
        for root in roots {
            let generation = self.git_status_generation.entry(root.clone()).or_default();
            *generation = generation.wrapping_add(1);
            if let Some(cached) = self.git_status.get_mut(&root) {
                cached.stale = true;
            }
        }
    }

    /// 这个根的 git 标记还没有或者旧了，就去取。
    ///
    /// 读失败（不是 git 仓库、git 报错）不等于干净：留着上一次成功的标记，等下一个事件再试，
    /// 不自己重试。取的途中又被标旧，回来的这份先显示，再接着取一次。
    pub(crate) fn ensure_git_status(&mut self, root: ProjectRef, cx: &mut Context<Self>) {
        if self
            .git_status
            .get(&root)
            .is_some_and(|cached| !cached.stale)
            || self.git_status_inflight.contains(&root)
        {
            return;
        }
        let Ok(client) = crate::runtime_call::target(&root.runtime, cx) else {
            return;
        };
        let generation = self
            .git_status_generation
            .get(&root)
            .copied()
            .unwrap_or_default();
        self.git_status_inflight.insert(root.clone());
        cx.spawn(async move |this, cx| {
            let params = GitRootParams {
                root: root.root.clone(),
            };
            let data = cx
                .background_executor()
                .spawn(async move {
                    crate::runtime_call::call_on::<GitStatusCall>(
                        &client,
                        params,
                        crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                    )
                    .unwrap_or_default()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.git_status_inflight.remove(&root);
                let changed = this
                    .git_status_generation
                    .get(&root)
                    .copied()
                    .unwrap_or_default()
                    != generation;
                match this.git_status.get_mut(&root) {
                    Some(cached) if !data.ok => cached.stale = changed,
                    _ => {
                        let marks = GitMarks {
                            value: data,
                            stale: changed,
                        };
                        this.git_status.insert(root.clone(), marks);
                    }
                }
                if changed {
                    this.ensure_git_status(root, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}
