//! Git 面板中 Workspace 的数据修改：暂存/提交、diff 生命周期。
//!
//! Git 数据模型和纯解析/行渲染函数留在父模块；这里集中放置依赖 Workspace
//! 状态的交互方法。页面渲染在 `view.rs`。字段仍由 main.rs 的 Workspace 统一持有。

use super::*;

/// 「全部更改」默认展开，只收起用户收起过的文件；单文件 diff 总是展开。
pub(super) fn apply_collapsed_files(
    state: &mut gpui_component::diff::DiffState,
    aggregate: bool,
    collapsed: &HashSet<String>,
    cx: &mut Context<gpui_component::diff::DiffState>,
) {
    let paths: Vec<String> = state
        .files()
        .iter()
        .map(|file| file.path().to_string())
        .collect();
    for path in paths {
        state.set_file_collapsed(&path, aggregate && collapsed.contains(&path), cx);
    }
}

fn parse_diff_files(text: &str) -> Result<Vec<gpui_component::diff::DiffFile>, String> {
    gpui_component::diff::DiffFile::parse(text)
        .map_err(|err| diff_parse_failure(text, &err.to_string()))
}

// ===================== Workspace 方法 =====================

impl Workspace {
    /// 文件右键「丢弃改动」：先弹确认。
    ///
    /// 同时记下用户此刻在列表里看到的这条改动（状态码 + 运行时算好的内容指纹），确认
    /// 时原样带过去：agent 在这之后又写了什么，运行时对不上就拒绝，不跟着一起丢。
    pub fn start_discard_file(&mut self, root: String, path: String, cx: &mut Context<Self>) {
        let Some(seen) = self
            .git_status
            .get(&root)
            .and_then(|(_, status)| status.seen_change(&path))
        else {
            self.report_background_error(format!("{path} 已不在改动列表里，请刷新后再丢弃。"));
            return;
        };
        self.discard_file_target = Some((root, seen));
        cx.notify();
    }

    /// 取消丢弃文件。
    pub fn cancel_discard_file(&mut self, cx: &mut Context<Self>) {
        self.discard_file_target = None;
        cx.notify();
    }

    /// 确认丢弃整个文件的改动。
    ///
    /// 已跟踪走 `git restore --staged --worktree`（暂存区和工作区一起还原，省得
    /// 用户为「已 add 过」的文件再点一次）；未跟踪的 git 管不着，直接删盘。
    pub fn confirm_discard_file(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self
            .discard_file_target
            .as_ref()
            .map(|(root, _)| root.clone())
        else {
            return;
        };
        if self.git_writes.contains_key(&root) {
            return;
        }
        let Some((root, seen)) = self.discard_file_target.take() else {
            return;
        };
        let git_root = root.clone();
        let git_seen = seen.clone();
        let done_root = root.clone();
        let done_path = seen.path.clone();
        let started = self.spawn_git(
            &root,
            GitWrite::DiscardFile,
            // 行自带仓库根，直接在它自己的仓库里丢弃。
            move || {
                git_call::<GitDiscardFileCall>(GitDiscardFileParams {
                    root: git_root,
                    seen: git_seen,
                })
                .map(drop)
            },
            move |this, result, cx| {
                // 被拒绝多半是列表已经过时，也要刷新，让用户看到现在的样子。
                this.invalidate_git_status(&done_root, cx);
                match result {
                    Ok(()) => {
                        // 丢弃的正是当前打开的那个文件时，diff 已经没意义了，关掉。
                        if this.git_diff.as_ref().is_some_and(|d| d.path == done_path) {
                            this.reset_git_diff_view();
                        }
                    }
                    Err(err) => this.report_background_error(err),
                }
            },
            cx,
        );
        if !started {
            self.discard_file_target = Some((root, seen));
        }
    }

    /// F7 / Shift+F7：跳到下一处 / 上一处变更。组件到头会绕回另一端。
    pub fn jump_hunk(&mut self, forward: bool, cx: &mut Context<Self>) {
        if self
            .git_diff
            .as_ref()
            .is_none_or(|diff| diff.patch.is_none())
        {
            return;
        }
        let Some(view) = self.diff_view.clone() else {
            return;
        };
        view.update(cx, |state, cx| {
            if forward {
                state.next_change(cx);
            } else {
                state.previous_change(cx);
            }
        });
    }

    /// 点「丢弃块」：先弹确认，不直接动文件。
    pub fn start_discard_hunk(&mut self, root: String, idx: usize, cx: &mut Context<Self>) {
        self.discard_hunk_target = Some((root, idx));
        cx.notify();
    }

    /// 确认丢弃：这个仓库还没有写入时才收起弹窗，并执行 reverse apply。
    ///
    /// 已经有写入就保持弹窗。diff 不能 apply、或占用没成功，把目标放回去。
    pub fn confirm_discard_hunk(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self
            .discard_hunk_target
            .as_ref()
            .map(|(root, _)| root.clone())
        else {
            return;
        };
        if self.git_writes.contains_key(&root) {
            return;
        }
        let Some((root, idx)) = self.discard_hunk_target.take() else {
            return;
        };
        if !self.discard_hunk(root.clone(), idx, cx) {
            self.discard_hunk_target = Some((root, idx));
            cx.notify();
        }
    }

    /// 取消丢弃：什么都不发生。
    pub fn cancel_discard_hunk(&mut self, cx: &mut Context<Self>) {
        self.discard_hunk_target = None;
        cx.notify();
    }

    /// 请运行时监听某个项目（文件在运行时那台机器上）：之后有真实文件变化，运行时发
    /// `project.changed`，见 [`Self::on_project_changed`]。每个 root 只登记一次；运行时
    /// 重连后监听随旧进程没了，清空重新登记。活动项目被选中时就装上，不必等打开 Git 页。
    pub fn ensure_git_watch(&mut self, root: String, cx: &mut Context<Self>) {
        if !self.watched_projects.insert(root.clone()) {
            return;
        }
        // 立刻拉一次初值：之后纯靠文件事件驱动，但首帧得先有数据，
        // 否则文件一直不变时侧栏 GIT 角标永远出不来。
        self.ensure_git_status(root.clone(), cx);
        cx.spawn(async move |this, cx| {
            let watch_root = root.clone();
            let watched = cx
                .background_executor()
                .spawn(async move {
                    crate::runtime_call::call::<
                        smelt_runtime_client::protocol::files::ProjectWatchCall,
                    >(
                        smelt_runtime_client::protocol::files::ProjectRootParams {
                            root: watch_root,
                        },
                        crate::runtime_call::RUNTIME_CALL_TIMEOUT,
                    )
                })
                .await;
            if let Err(error) = watched {
                eprintln!(
                    "[git] 请运行时监听 {root} 失败：{}",
                    crate::runtime_call::error_text(error)
                );
                // 下次刷新再试。
                let _ = this.update(cx, |this, _| this.watched_projects.remove(&root));
            }
        })
        .detach();
    }

    /// 运行时说项目里有文件变了：标脏并主动重拉 git status，角标 / Git 页在任何页面都
    /// 即时刷新。监听是递归的，子仓里的改动也会到这里——所以要把该项目下**所有**仓库
    /// 的 status 都标脏，只刷项目根会让子仓改动在面板里冻结。ensure_* 内部有 inflight
    /// 去重，重复通知不会叠并发。
    pub(crate) fn on_project_changed(&mut self, root: String, cx: &mut Context<Self>) {
        if !self.watched_projects.contains(&root) {
            return;
        }
        self.invalidate_project_git_status(&root, cx);
        self.ensure_git_repos(root, cx);
        cx.notify();
    }

    /// 确保某 cwd 的当前分支缓存新鲜（>5s 或缺失就后台刷新），供侧栏显示分支名。跟
    /// git_status 不同，这个要对**所有**会话的 cwd 探测（侧栏一直显示全部项目分组，不止
    /// 当前打开的那个），所以单独走一套缓存，TTL 也放宽到 5s——分支不常变，没必要跟
    /// git status 一样 1.5s 就重跑。非 git 目录（比如临时终端的 $HOME）缓存 None，
    /// 同样不重复重试。
    pub fn ensure_repo_info(&mut self, cwd: String, cx: &mut Context<Self>) {
        if cwd.is_empty() {
            return;
        }
        let fresh = self
            .repo_info
            .get(&cwd)
            .is_some_and(|(t, _)| t.elapsed() < std::time::Duration::from_secs(5));
        if fresh || self.repo_info_inflight.contains(&cwd) {
            return;
        }
        self.repo_info_inflight.insert(cwd.clone());
        cx.spawn(async move |this, cx| {
            let c = cwd.clone();
            let info = cx
                .background_executor()
                .spawn(async move {
                    git_call::<GitRepoInfoCall>(root_params(&c))
                        .ok()
                        .and_then(|result| result.info)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.repo_info_inflight.remove(&cwd);
                this.repo_info.insert(cwd, (Instant::now(), info));
                cx.notify();
            });
        })
        .detach();
    }

    /// 标记某 root 的 git status 缓存过期并推进失效代数。
    ///
    /// 不直接 `.remove()`：旧数据留着继续显示，新数据到达后无缝替换。代数用于识别
    /// 请求执行期间是否又发生了变化：成功的中间快照可以先显示，但仍保持过期并立即
    /// 补拉当前代，不能把它写成“新鲜”后再等一轮 TTL。
    fn mark_git_status_dirty(&mut self, root: &str) {
        let generation = self
            .git_status_generation
            .entry(root.to_string())
            .or_default();
        *generation = generation.wrapping_add(1);
        if let Some((t, _)) = self.git_status.get_mut(root) {
            *t = Instant::now() - std::time::Duration::from_secs(3600);
        }
    }

    pub fn invalidate_git_status(&mut self, root: &str, cx: &mut Context<Self>) {
        self.mark_git_status_dirty(root);
        self.schedule_visible_refresh(cx);
    }

    /// 把一个项目下所有已发现仓库的 status 都标脏。
    ///
    /// 文件监听是整棵目录递归的，一次事件分不出是哪个仓库变了；全部标脏交给
    /// `ensure_git_status` 的 TTL 与 inflight 去重去抓实际开销。
    pub fn invalidate_project_git_status(&mut self, project_root: &str, cx: &mut Context<Self>) {
        let roots: Vec<String> = match self.git_repos.get(project_root) {
            Some((_, set)) if !set.repos.is_empty() => set.roots().map(str::to_string).collect(),
            _ => vec![project_root.to_string()],
        };
        for root in roots {
            self.mark_git_status_dirty(&root);
        }
        self.schedule_visible_refresh(cx);
    }

    /// 确保某项目根的仓库发现结果新鲜，并为每个发现到的仓库拉 status。
    ///
    /// 发现比 status 稳定得多（新增/删除一个仓库是低频事件），所以 TTL 取 15s，
    /// 而不是跟 status 一样 1.5s——它要扫目录还要跑多次 rev-parse，往返成本高。
    pub fn ensure_git_repos(&mut self, project_root: String, cx: &mut Context<Self>) {
        let fresh = self
            .git_repos
            .get(&project_root)
            .is_some_and(|(t, _)| t.elapsed() < std::time::Duration::from_secs(15));
        if !fresh && !self.git_repos_inflight.contains(&project_root) {
            self.git_repos_inflight.insert(project_root.clone());
            let root_for_task = project_root.clone();
            cx.spawn(async move |this, cx| {
                let r = root_for_task.clone();
                let found = cx
                    .background_executor()
                    .spawn(async move {
                        git_call::<GitDiscoverCall>(root_params(&r)).unwrap_or_default()
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.git_repos_inflight.remove(&root_for_task);
                    this.git_repos.insert(
                        root_for_task.clone(),
                        (
                            Instant::now(),
                            RepoSet {
                                repos: found.repos,
                                truncated: found.truncated,
                            },
                        ),
                    );
                    // 提交框按发现结果来建，而那一步不在 render 里。
                    this.schedule_visible_refresh(cx);
                    cx.notify();
                });
            })
            .detach();
        }

        // 每个已知仓库都要有自己的 status；发现还没回来时至少先拉项目根的，
        // 首帧不至于空白。
        let roots: Vec<String> = match self.git_repos.get(&project_root) {
            Some((_, set)) if !set.repos.is_empty() => set.roots().map(str::to_string).collect(),
            _ => vec![project_root.clone()],
        };
        for root in roots {
            self.ensure_git_status(root, cx);
        }
    }

    /// 切换提交目标仓库。
    ///
    /// 切仓库要重置提交框：提交信息是写给某个仓库的，把上一个仓库的描述
    /// 留在输入框里只会造成误提交。diff 预览同理，它属于旧仓库的文件。
    pub fn set_active_git_repo(&mut self, root: String, cx: &mut Context<Self>) {
        if self.active_git_repo.as_deref() == Some(root.as_str()) {
            return;
        }
        self.active_git_repo = Some(root);
        self.reset_git_diff_view();
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 项目里第一个有未提交改动的仓库（按发现顺序，项目根在前）。
    ///
    /// 只在用户还没亲自选过仓库时用。
    fn first_repo_with_changes(&self, project_root: &str) -> Option<String> {
        let (_, set) = self.git_repos.get(project_root)?;
        set.roots()
            .find(|root| {
                self.git_status
                    .get(*root)
                    .is_some_and(|(_, status)| status.ok && !status.files.is_empty())
            })
            .map(str::to_string)
    }

    /// 当前写操作（提交/推送/分支）的目标仓库根。没打开项目时为 None。
    /// 所有会改仓库状态的入口都走这里，不得直接用 `active_project_root`：
    /// 多仓工作区里项目根只是其中一个仓库，把它当成唯一写入点正是
    /// “子仓改动能暂存却提交不出去”的根因。
    pub fn git_write_target(&self, cx: &mut Context<Self>) -> Option<String> {
        let project_root = self.active_project_root(cx)?;
        Some(self.active_git_repo_root(&project_root))
    }

    /// 当前写操作（提交/推送/分支）的目标仓库根。
    ///
    /// 选中的仓库已经不在发现结果里（删了、或换了项目）就回退到项目根，
    /// 不能拿着一个不存在的路径去跑 git。
    pub fn active_git_repo_root(&self, project_root: &str) -> String {
        // 没选过就落在第一个有改动的仓库上。不把这个落点写进 `active_git_repo`：
        // 写进去等于替用户做了选择，那个仓库提交干净之后就再也不会挪窝，
        // 别的仓库有改动也看不到 diff 预览。
        let fallback = self.first_repo_with_changes(project_root);
        resolve_git_write_target(
            project_root,
            self.active_git_repo.as_deref(),
            self.git_repos.get(project_root).map(|(_, set)| set),
            fallback.as_deref(),
        )
    }

    /// Git 视图：查看某个改动文件的 diff。已跟踪文件用 `git diff HEAD`，
    /// 未跟踪文件（??）用 `git diff --no-index` 展示全文（整体当作新增）。
    /// 确保某 root 的 git status 缓存新鲜（>1.5s 或缺失就后台刷新；ensure_git_watch
    /// 建的监听命中时会主动标脏缓存，比 1.5s 轮询更快触发这里重新拉取）。
    /// 绝不阻塞 render：git status 在大仓要 ~90ms，同步跑就是掉帧元凶。
    pub fn ensure_git_status(&mut self, root: String, cx: &mut Context<Self>) {
        let fresh = self
            .git_status
            .get(&root)
            .is_some_and(|(t, _)| t.elapsed() < std::time::Duration::from_millis(1500));
        if fresh || self.git_status_inflight.contains(&root) {
            return;
        }
        let request_generation = self
            .git_status_generation
            .get(&root)
            .copied()
            .unwrap_or_default();
        self.git_status_inflight.insert(root.clone());
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let data = cx
                .background_executor()
                .spawn(
                    async move { git_call::<GitStatusCall>(root_params(&r)).unwrap_or_default() },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                let current_generation = this
                    .git_status_generation
                    .get(&root)
                    .copied()
                    .unwrap_or_default();
                match git_status_response_action(current_generation, request_generation, data.ok) {
                    GitStatusResponseAction::AcceptCurrent => {
                        this.git_status_inflight.remove(&root);
                        this.git_status_failures.remove(&root);
                        this.git_status.insert(root.clone(), (Instant::now(), data));
                        // 当前代数的请求一定在所有已完成 index 操作之后启动；运行中的
                        // 操作仍是 false，不能提前移除。
                        clear_confirmed_git_index_ops(&mut this.git_index_pending, &root);
                    }
                    GitStatusResponseAction::AcceptStaleAndRefresh => {
                        // 这份成功快照不是最终权威状态，但仍比一直保留旧缓存可靠；先
                        // 展示它并保持过期，再串行补拉当前代。持续有文件事件时也不会
                        // 因为每个回包都被丢弃而永远卡在启动时的空状态。
                        this.git_status_inflight.remove(&root);
                        this.git_status_failures.remove(&root);
                        this.git_status.insert(
                            root.clone(),
                            (Instant::now() - std::time::Duration::from_secs(3600), data),
                        );
                        this.ensure_git_status(root.clone(), cx);
                    }
                    GitStatusResponseAction::PreserveAndRetry => {
                        // 命令失败不等于工作区干净：保留上一次成功快照；首次加载就失败
                        // 时只缓存“读取失败”占位，渲染层不会把它解释成干净状态。
                        let failure_count = {
                            let failures =
                                this.git_status_failures.entry(root.clone()).or_default();
                            *failures = failures.saturating_add(1);
                            *failures
                        };
                        if let Some((fetched_at, _)) = this.git_status.get_mut(&root) {
                            *fetched_at = Instant::now();
                        } else {
                            this.git_status.insert(root.clone(), (Instant::now(), data));
                        }

                        if let Some(delay) = git_status_retry_delay(failure_count) {
                            // inflight 在退避期间继续占位，文件事件和 render 不会再起一条
                            // 并发请求；定时器到点后释放并串行重试。
                            let retry_root = root.clone();
                            cx.spawn(async move |this, cx| {
                                cx.background_executor().timer(delay).await;
                                let _ = this.update(cx, |this, cx| {
                                    this.git_status_inflight.remove(&retry_root);
                                    if let Some((fetched_at, _)) =
                                        this.git_status.get_mut(&retry_root)
                                    {
                                        *fetched_at =
                                            Instant::now() - std::time::Duration::from_secs(3600);
                                    }
                                    this.ensure_git_status(retry_root.clone(), cx);
                                    cx.notify();
                                });
                            })
                            .detach();
                        } else {
                            // 连续失败时停止自动重试，避免非 Git 目录永久轮询；后续文件
                            // 事件或界面交互超过 TTL 后仍会再尝试。
                            this.git_status_inflight.remove(&root);
                        }
                    }
                }
                // 聚合开关和提交框的占位跟着这份快照走，不能只等 2 秒保底。
                this.schedule_visible_refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 确保某 root 的分支列表缓存新鲜（>1.5s 或缺失就后台刷新），Git 页头部分支切换
    /// 下拉用。`for-each-ref` 一次传两个 pattern 拿全 `refs/heads` + `refs/remotes`，
    /// 靠 refname 前缀区分本地/远程，不用起两次 git 进程。
    pub fn ensure_branches(&mut self, root: String, cx: &mut Context<Self>) {
        let fresh = self
            .branches
            .get(&root)
            .is_some_and(|(t, _)| t.elapsed() < std::time::Duration::from_millis(1500));
        if fresh || self.branches_inflight.contains(&root) {
            return;
        }
        let request_generation = self.branches_generation.get(&root).copied().unwrap_or(0);
        self.branches_inflight.insert(root.clone());
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let list = cx
                .background_executor()
                .spawn(
                    async move { git_call::<GitBranchesCall>(root_params(&r)).unwrap_or_default() },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.branches_inflight.remove(&root);
                let current_generation = this.branches_generation.get(&root).copied().unwrap_or(0);
                // 删除分支发生在列举期间时，这一包还含着已删的分支。不写回，再排一次刷新。
                if current_generation == request_generation {
                    this.branches.insert(root, (Instant::now(), list));
                } else {
                    this.schedule_visible_refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 分支集合变了。在途的 `for-each-ref` 不得再把旧列表标成新鲜。
    fn bump_branches_generation(&mut self, root: &str) {
        let generation = self
            .branches_generation
            .entry(root.to_string())
            .or_default();
        *generation = generation.wrapping_add(1);
    }

    /// Git 页组页面前的数据准备：失效跨仓库 diff、默认打开全部改动、懒建输入框。
    /// 缓存刷新（status / branches / watch）由可见数据刷新统一调度。
    pub(crate) fn prepare_git_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project_root) = self.active_project_root(cx) else {
            return;
        };
        // 预览与历史都跟随当前提交目标仓库，而不是恒为项目根。
        let root = self.active_git_repo_root(&project_root);
        // diff 预览属于某个仓库的文件。它跟当前仓库对不上就得丢掉重开——
        // 切了项目、或提交完自动落到下一个有改动的仓库，都会走到这里；
        // 不丢的话右侧会一直停在上一个仓库的 diff 上。
        if self.git_diff.as_ref().is_some_and(|diff| diff.root != root) {
            self.reset_git_diff_view();
        }
        // 每个仓库一个提交框：提交信息属于仓库，不能跨仓库串味。
        // 同时回收已经不在发现结果里的仓库，避免关掉项目后输入框实体一直挂着。
        let live_roots: Vec<String> = match self.git_repos.get(&project_root) {
            Some((_, set)) if !set.repos.is_empty() => set.roots().map(str::to_string).collect(),
            _ => vec![project_root.clone()],
        };
        self.commit_msg_inputs
            .retain(|root, _| live_roots.iter().any(|live| live == root));
        for repo_root in live_roots {
            if self.commit_msg_inputs.contains_key(&repo_root) {
                continue;
            }
            use gpui_component::input::TextareaState;
            let branch = self
                .git_status
                .get(&repo_root)
                .map(|(_, d)| d.branch.clone())
                .unwrap_or_default();
            let placeholder = if branch.is_empty() {
                "Commit message（可多行）".to_string()
            } else {
                format!("消息（提交到 {branch}）")
            };
            let state = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(1, 6)
                    .placeholder(placeholder)
            });
            self.commit_msg_inputs.insert(repo_root, state);
        }
        if self.git_tab == GitTab::Changes {
            let status = self.git_status.get(&root).map(|(_, status)| status);
            let has_changes = status.is_some_and(|status| status.ok && !status.files.is_empty());
            if should_close_aggregate_diff(
                status,
                self.git_diff.as_ref().is_some_and(|diff| diff.aggregate),
            ) {
                self.reset_git_diff_view();
            } else if self.git_diff.is_none() && has_changes {
                self.open_all_diffs(root.clone(), cx);
            }
            if self.git_diff.is_some() && self.diff_comment_input.is_none() {
                use gpui_component::input::TextareaState;
                let state = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(2, 6)
                        .placeholder("给选中的行写评论，发送前可以再改改…")
                });
                self.diff_comment_input = Some(state);
            }
            if self.diff_view.is_some() {
                self.ensure_diff_body(cx);
            }
            // 定位发生在点击或补丁装上时。这里再补一帧，且放到 render 之外，
            // 避免准备阶段读 DiffState，把整窗订上它的刷新。
            if self.pending_diff_file.is_some() {
                let view = cx.entity();
                cx.defer(move |cx| {
                    view.update(cx, |this, cx| this.reveal_pending_diff_file(cx));
                });
            }
        } else if self.git_tab == GitTab::Log {
            self.ensure_git_log(root, cx);
        }
    }

    /// Git 页分支切换下拉：checkout 目标分支（本地分支直接切；远程分支传短名，靠 git
    /// 内建 DWIM 自动建好跟踪分支）。
    /// 成功后清掉 status/分支缓存并刷新可见数据（文件、ahead/behind 全变了）。
    pub fn checkout_branch(&mut self, root: String, branch: String, cx: &mut Context<Self>) {
        let key = root.clone();
        let status_root = root.clone();
        self.spawn_git(
            &key,
            GitWrite::Checkout,
            move || git_call::<GitCheckoutCall>(GitBranchParams { root, branch }).map(drop),
            move |this, result, cx| match result {
                // 切分支后文件列表、ahead/behind 都变了，标脏再刷新；
                // 分支列表本身（有哪些分支）不受切换影响，不用跟着失效。
                Ok(()) => this.invalidate_git_status(&status_root, cx),
                Err(err) => this.report_background_error(err),
            },
            cx,
        );
    }

    /// 右键「删除分支」：先弹确认。`remote` 为真时删的是远端分支（更危险，别人也
    /// 会受影响），文案要分开写。
    pub fn start_delete_branch(
        &mut self,
        root: String,
        branch: String,
        remote: bool,
        cx: &mut Context<Self>,
    ) {
        self.delete_branch_target = Some((root, branch, remote));
        cx.notify();
    }

    /// 取消删除分支。
    pub fn cancel_delete_branch(&mut self, cx: &mut Context<Self>) {
        self.delete_branch_target = None;
        cx.notify();
    }

    /// 确认删除分支。
    ///
    /// 本地分支先试 `-d`（安全删，未合并会被 git 拒绝），被拒了不自作主张改 `-D`，
    /// 而是把 git 的原话报出来让人自己决定——分支删了没有 reflog 之外的退路。
    /// 远端分支走 `push origin --delete`。
    pub fn confirm_delete_branch(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self
            .delete_branch_target
            .as_ref()
            .map(|(root, _, _)| root.clone())
        else {
            return;
        };
        if self.git_writes.contains_key(&root) {
            return;
        }
        let Some((root, branch, remote)) = self.delete_branch_target.take() else {
            return;
        };
        let op = if remote {
            GitWrite::DeleteRemoteBranch
        } else {
            GitWrite::DeleteBranch
        };
        let git_root = root.clone();
        let git_branch = branch.clone();
        let done_root = root.clone();
        let started = self.spawn_git(
            &root,
            op,
            move || {
                git_network_call::<GitBranchDeleteCall>(GitBranchDeleteParams {
                    root: git_root,
                    branch: git_branch,
                    remote,
                })
                .map(drop)
            },
            move |this, result, cx| match result {
                Ok(()) => {
                    this.invalidate_git_status(&done_root, cx);
                    // 分支没了，列表和日志都得重拉。先推进代次，在途的旧列表不能写回来。
                    this.bump_branches_generation(&done_root);
                    this.branches.remove(&done_root);
                    this.reload_git_log(done_root, cx);
                }
                Err(err) => this.report_background_error(err),
            },
            cx,
        );
        if !started {
            self.delete_branch_target = Some((root, branch, remote));
        }
    }

    /// 把某个分支合并进当前分支（`git merge <branch>`）。
    ///
    /// 冲突时 git 会以非 0 退出并把冲突文件写进工作区——此时**不自动 abort**，
    /// 保留现场让人去解，只把提示报出来（自动回滚会让人措手不及）。
    pub fn merge_branch(&mut self, root: String, branch: String, cx: &mut Context<Self>) {
        let key = root.clone();
        let done_root = root.clone();
        self.spawn_git(
            &key,
            GitWrite::Merge,
            move || git_call::<GitMergeCall>(GitBranchParams { root, branch }).map(drop),
            move |this, result, cx| match result {
                Ok(()) => {
                    this.invalidate_git_status(&done_root, cx);
                    this.reload_git_log(done_root, cx);
                }
                Err(err) => this.report_background_error(err),
            },
            cx,
        );
    }

    /// Git 页文件列表勾选框：把某个改动文件加入暂存区（`git add --`，untracked/修改/
    /// 删除都适用）。纯本地索引改动、可逆，不像 commit/push 那样需要走"发到终端让人
    /// 确认"那一套，直接执行。成功后清 git_status 缓存并刷新状态。
    pub fn stage_file(&mut self, root: String, path: String, cx: &mut Context<Self>) {
        self.run_git_index_op(root, path, true, cx);
    }

    /// 文件列表取消勾选：把已暂存的改动移出暂存区（`git reset --`），不影响工作区
    /// 内容本身，随时能重新勾选加回去。
    pub fn unstage_file(&mut self, root: String, path: String, cx: &mut Context<Self>) {
        self.run_git_index_op(root, path, false, cx);
    }

    /// 整组批量暂存/撤出。一次 git 调用带上全部路径，不是逐个文件发命令——
    /// 几十个文件就是几十个进程，而且每个都会各自失效一次 status。
    pub fn stage_paths(
        &mut self,
        root: String,
        paths: Vec<String>,
        stage: bool,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let op = if stage {
            GitWrite::Stage
        } else {
            GitWrite::Unstage
        };
        let failure = if stage { "暂存" } else { "取消暂存" };
        let key = root.clone();
        let done_root = root.clone();
        self.spawn_git(
            &key,
            op,
            move || stage_or_unstage(root, paths, stage),
            move |this, result, cx| {
                if let Err(err) = result {
                    this.report_background_error(format!("{failure}失败：{err}"));
                }
                // 批量操作会一口气改掉一片文件的暂存状态，之前留下的单文件乐观状态
                // 全部作废；留着它们只会让复选标记跟真实 status 对不上。
                this.git_index_pending
                    .retain(|(pending_root, _), _| pending_root != &done_root);
                this.invalidate_git_status(&done_root, cx);
                this.ensure_git_status(done_root, cx);
            },
            cx,
        );
    }

    /// stage_file/unstage_file 共用的后台执行 + 缓存失效逻辑。
    fn run_git_index_op(
        &mut self,
        root: String,
        path: String,
        stage: bool,
        cx: &mut Context<Self>,
    ) {
        // 勾选框的乐观标记在占用成功之后才写。占用失败时格子保持原样，用户可再点。
        let key = root.clone();
        let pending_key = (root.clone(), path.clone());
        let git_path = path;
        let done_key = pending_key.clone();
        let done_root = root.clone();
        let started = self.spawn_git(
            &key,
            GitWrite::IndexFile,
            move || stage_or_unstage(root, vec![git_path], stage),
            move |this, result, cx| match result {
                Ok(()) => {
                    this.mark_git_status_dirty(&done_root);
                    if let Some(pending) = this.git_index_pending.get_mut(&done_key) {
                        pending.completed = true;
                    }
                    // 不依赖下一次 render 或 watcher；操作完成后立即发起权威刷新。
                    // 若已有旧请求在途，generation 会让它回包后自动补拉。
                    this.ensure_git_status(done_root, cx);
                }
                Err(err) => {
                    this.git_index_pending.remove(&done_key);
                    this.report_background_error(err);
                }
            },
            cx,
        );
        if started {
            self.git_index_pending
                .insert(pending_key, PendingGitIndexOp { completed: false });
            cx.notify();
        }
    }

    /// 切换 diff 视图（全部 / 已暂存 / 未暂存），并按新视图重拉当前文件的 diff。
    pub fn set_diff_scope(&mut self, scope: DiffScope, cx: &mut Context<Self>) {
        if self.diff_scope == scope {
            return;
        }
        self.diff_scope = scope;
        // 当前开着某个文件就按新视图重拉；没开就只记住选择。
        if let Some((root, path, aggregate)) = self
            .git_diff
            .as_ref()
            .map(|diff| (diff.path.clone(), diff.aggregate))
            .and_then(|(path, aggregate)| {
                self.current_local_cwd(cx)
                    .map(|root| (root, path, aggregate))
            })
        {
            if aggregate {
                self.open_all_diffs(root, cx);
            } else {
                self.open_diff(root, path, false, cx);
            }
        } else {
            cx.notify();
        }
    }

    /// 右侧文件树是聚合 diff 的导航，不应把主面板切成只能看到一个文件的视图。
    /// 聚合 diff 尚未返回时先记住目标，下一帧在文件标题处定位。
    pub fn open_aggregate_file(&mut self, root: String, path: String, cx: &mut Context<Self>) {
        // 点哪个仓库的文件，提交目标就跟到哪个仓库：diff 预览、提交框、头部分支
        // 必须指向同一个仓库，否则看着 A 的 diff 却提交到了 B。
        self.set_active_git_repo(root.clone(), cx);
        self.pending_diff_file = Some(path.clone());
        let header_ready = self
            .git_diff
            .as_ref()
            .is_some_and(|diff| aggregate_diff_ready_for_file(diff, &root, self.diff_scope, &path));
        // 标题行按空白切开，带空格或重命名的路径会对不上。组件里已经有这个文件
        // 就直接滚，不要整份重拉。重拉会先闪「正在读取…」，滚动位置也没了。
        let in_view = self.loaded_aggregate_has_file(&root, &path, cx);
        let aggregate_loading = self.git_diff.as_ref().is_some_and(|diff| {
            diff.root == root
                && diff.aggregate
                && diff.scope == self.diff_scope
                && diff.patch.is_none()
        });
        if !header_ready && !in_view && !aggregate_loading {
            self.open_all_diffs_at(root, cx);
        }
        self.reveal_pending_diff_file(cx);
        cx.notify();
    }

    /// 清掉当前 Git diff 及其交互状态。进入新的 Git 改动页或切换仓库时必须失效
    /// 正在加载的旧结果，否则异步回调可能把旧文件重新放回主面板。
    pub(crate) fn reset_git_diff_view(&mut self) {
        self.diff_gen = self.diff_gen.wrapping_add(1);
        self.git_diff = None;
        self.pending_diff_file = None;
        self.drop_diff_view();
    }

    fn drop_diff_view(&mut self) {
        self.diff_view = None;
        self._diff_view_sub = None;
        self.diff_view_patch.clear();
        self.diff_parse_error = None;
        self.diff_comment_range = None;
        self.diff_comment_open = false;
    }

    /// 开始读一份新补丁。旧正文先藏起来，但原文没变时不拆掉 Diff，滚动才留得住。
    fn begin_diff_load(&mut self, cx: &mut Context<Self>) {
        self.diff_comment_open = false;
        self.diff_comment_range = None;
        if let Some(view) = self.diff_view.clone() {
            view.update(cx, |state, cx| state.set_selected_lines(None, cx));
        }
        self.publish_diff_body(cx);
    }

    pub(crate) fn current_diff_mode(&self) -> gpui_component::diff::DiffMode {
        if self.diff_split {
            gpui_component::diff::DiffMode::Split
        } else {
            gpui_component::diff::DiffMode::Unified
        }
    }

    fn subscribe_diff_view(
        &mut self,
        view: &Entity<gpui_component::diff::DiffState>,
        cx: &mut Context<Self>,
    ) {
        // 折叠事件只改集合。DiffState 自己会通知正文视图；这里再 notify Workspace
        // 会把右侧文件列表整列重排。
        self._diff_view_sub = Some(cx.subscribe(view, |this, _, event, _cx| match event {
            gpui_component::diff::DiffEvent::FileCollapsed(path) => {
                this.git_collapsed_diff_files.insert(path.to_string());
            }
            gpui_component::diff::DiffEvent::FileExpanded(path) => {
                this.git_collapsed_diff_files.remove(path.as_ref());
            }
            _ => {}
        }));
    }

    fn ensure_diff_body(&mut self, cx: &mut Context<Self>) {
        if self.diff_body.is_some() {
            return;
        }
        let host = cx.entity().downgrade();
        self.diff_body = Some(cx.new(|_| GitDiffBody::new(host)));
    }

    /// 把补丁装进 Diff。原文相同就只同步统一/并排，不调用 `set_files`。
    fn install_diff_view(
        &mut self,
        patch: &str,
        files: Result<Vec<gpui_component::diff::DiffFile>, String>,
        cx: &mut Context<Self>,
    ) {
        if self.diff_view_patch == patch {
            if let Some(view) = self.diff_view.clone() {
                let mode = self.current_diff_mode();
                view.update(cx, |state, cx| state.set_mode(mode, cx));
                self.ensure_diff_body(cx);
                self.publish_diff_body(cx);
            }
            self.reveal_pending_diff_file(cx);
            return;
        }
        self.diff_comment_open = false;
        self.diff_comment_range = None;
        self.diff_view_patch = patch.to_string();
        let files = match files {
            Ok(files) => files,
            Err(err) => {
                self.diff_view = None;
                self._diff_view_sub = None;
                self.diff_parse_error = Some(err);
                return;
            }
        };
        self.diff_parse_error = None;
        let mode = self.current_diff_mode();
        let aggregate = self.git_diff.as_ref().is_some_and(|diff| diff.aggregate);
        let collapsed = self.git_collapsed_diff_files.clone();
        if let Some(view) = self.diff_view.clone() {
            view.update(cx, |state, cx| {
                state.set_files(files, cx);
                state.set_mode(mode, cx);
                apply_collapsed_files(state, aggregate, &collapsed, cx);
            });
        } else {
            let view = cx.new(|cx| gpui_component::diff::DiffState::new(files, cx).with_mode(mode));
            view.update(cx, |state, cx| {
                apply_collapsed_files(state, aggregate, &collapsed, cx)
            });
            self.subscribe_diff_view(&view, cx);
            self.diff_view = Some(view);
        }
        self.ensure_diff_body(cx);
        self.publish_diff_body(cx);
        self.reveal_pending_diff_file(cx);
    }

    /// 把第 `idx` 个 hunk 单独加入暂存区（`git apply --cached`）。
    ///
    /// 只暂存一块、其余留在工作区，是 agent 写的代码「对一半」时最需要的动作：挑出
    /// 对的先存下来，剩下的继续让它改。
    pub fn stage_hunk(&mut self, root: String, idx: usize, cx: &mut Context<Self>) {
        self.apply_hunk(root, idx, GitPatchAction::Stage, GitWrite::StageHunk, cx);
    }

    /// 把第 `idx` 个 hunk 撤出暂存区（`git apply --cached --reverse`）。
    ///
    /// 只在「已暂存」视图下给：那时 diff 就是索引相对 HEAD 的差异，reverse 回去
    /// 正好把这一块退回工作区，文件内容不受影响。
    pub fn unstage_hunk(&mut self, root: String, idx: usize, cx: &mut Context<Self>) {
        self.apply_hunk(
            root,
            idx,
            GitPatchAction::Unstage,
            GitWrite::UnstageHunk,
            cx,
        );
    }

    /// 丢弃第 `idx` 个 hunk（`git apply --reverse`，作用于工作区文件）。
    ///
    /// **会真的改用户的文件且不进 reflog**，调用方必须先让用户确认过。
    ///
    /// 返回 false 表示没有占住这个仓库：确认弹窗要留着。
    fn discard_hunk(&mut self, root: String, idx: usize, cx: &mut Context<Self>) -> bool {
        self.apply_hunk(
            root,
            idx,
            GitPatchAction::Discard,
            GitWrite::DiscardHunk,
            cx,
        )
    }

    /// stage_hunk / discard_hunk 共用：拼 patch → 后台 `git apply` → 刷新状态并重开 diff。
    ///
    /// 成功后必须重新拉一次 diff：apply 之后剩余 hunk 的行号和分段都变了，接着用旧的
    /// 下标去点第二块，改的就是别的地方（`-U0` 那次错位是同一类问题）。
    fn apply_hunk(
        &mut self,
        root: String,
        idx: usize,
        action: GitPatchAction,
        op: GitWrite,
        cx: &mut Context<Self>,
    ) -> bool {
        let (patch, path) = {
            let Some(d) = self.git_diff.as_ref() else {
                return false;
            };
            if !d.patchable {
                self.report_background_error(
                    "这个 diff 不支持按块操作（子模块 / 未跟踪文件），请用整文件的勾选框",
                );
                cx.notify();
                return false;
            }
            let Some(hunk) = d.hunks.get(idx) else {
                return false;
            };
            (hunk_patch(&d.header, hunk), d.path.clone())
        };
        let key = root.clone();
        let done_root = root.clone();
        self.spawn_git(
            &key,
            op,
            move || {
                let Err(raw) = git_call::<GitApplyPatchCall>(GitApplyPatchParams {
                    root,
                    action,
                    patch,
                }) else {
                    return Ok(());
                };
                // apply 失败最常见的原因是这个文件已经部分暂存过：当前 diff 是
                // `git diff HEAD`（暂存+未暂存合起来），其中已进索引的那部分再
                // apply --cached 就会 "already exists"/"does not apply"。把原因
                // 说清楚，别只甩 git 的英文原文。
                Err(format!(
                    "按块操作失败：{raw}\n\
                     （这个文件若已部分暂存，当前视图混着暂存与未暂存的改动，\
                     按块操作会对不上号；先用勾选框整文件取消暂存再试）"
                ))
            },
            move |this, result, cx| match result {
                Ok(()) => {
                    this.invalidate_git_status(&done_root, cx);
                    // 行号已变，重新解析一份，避免下一次点击打在错的位置上。
                    this.open_diff(done_root, path, false, cx);
                }
                Err(err) => this.report_background_error(err),
            },
            cx,
        )
    }

    /// 跑 git + 着色放后台，用 file_gen 丢弃过期结果。
    pub fn open_diff(
        &mut self,
        root: String,
        path: String,
        untracked: bool,
        cx: &mut Context<Self>,
    ) {
        // 同 open_aggregate_file：看谁的 diff，提交目标就是谁。
        self.set_active_git_repo(root.clone(), cx);
        let scope = self.diff_scope;
        self.pending_diff_file = None;
        self.diff_gen = self.diff_gen.wrapping_add(1);
        let r#gen = self.diff_gen;
        self.git_diff = Some(GitDiff {
            root: root.clone(),
            path: path.clone(),
            aggregate: false,
            worktree_file: None,
            lines: Rc::new(Vec::new()),
            header: String::new(),
            hunks: Rc::new(Vec::new()),
            patchable: false,
            scope,
            has_staged: false,
            patch: None,
        });
        self.begin_diff_load(cx);
        // diff 内嵌显示在改动页里（停靠或展开态用同一份 UI，见 git_narrow_panel），
        // 不再提升到单独的舞台页——跟 Files 点文件不提升到舞台是同一个道理。
        self.git_tab = crate::GitTab::Changes;
        // 仓库没变时 set_active_git_repo 不会排队。评论框仍要这次准备才能建出来。
        self.schedule_visible_refresh(cx);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let (r, p) = (root.clone(), path.clone());
            let parsed = cx
                .background_executor()
                .spawn(async move {
                    // 文件已经带着自己的仓库根进来，不用再按路径猜它属于哪个仓库。
                    let GitFileDiffResult {
                        text,
                        has_staged,
                        is_gitlink,
                        worktree_file,
                    } = git_call::<GitFileDiffCall>(GitFileDiffParams {
                        root: r,
                        path: p,
                        untracked,
                        scope,
                    })
                    .unwrap_or_else(|err| GitFileDiffResult {
                        text: format!("无法执行 git diff：{err}"),
                        ..Default::default()
                    });
                    let files = parse_diff_files(&text);
                    let parsed = parse_diff(&text);
                    (
                        text,
                        parsed,
                        files,
                        !untracked && !is_gitlink,
                        has_staged,
                        worktree_file,
                    )
                })
                .await;
            let (text, parsed, files, patchable, has_staged, worktree_file) = parsed;
            let _ = this.update(cx, |this, cx| {
                if this.diff_gen == r#gen {
                    // 没解析出文件头就拼不出合法 patch（比如 diff 为空、或 git 报错
                    // 的文案），这时也不能让按块按钮亮着。
                    let patchable = patchable && !parsed.header.is_empty();
                    this.git_diff = Some(GitDiff {
                        root: root.clone(),
                        path,
                        aggregate: false,
                        worktree_file,
                        lines: Rc::new(parsed.lines),
                        header: parsed.header,
                        hunks: Rc::new(parsed.hunks),
                        patchable,
                        scope,
                        has_staged,
                        patch: Some(text.clone()),
                    });
                    this.install_diff_view(&text, files, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 默认预览工作区的全部改动。聚合视图只读，避免把跨文件的 hunk 误用于
    /// stage/discard；右侧文件树只负责在聚合列表中定位。
    pub(super) fn open_all_diffs(&mut self, root: String, cx: &mut Context<Self>) {
        self.pending_diff_file = None;
        self.open_all_diffs_at(root, cx);
    }

    fn open_all_diffs_at(&mut self, root: String, cx: &mut Context<Self>) {
        let scope = self.diff_scope;
        let status_files = self
            .git_status
            .get(&root)
            .map(|(_, status)| status.files.clone())
            .unwrap_or_default();
        let untracked_paths = if matches!(scope, DiffScope::All | DiffScope::Unstaged) {
            status_files
                .iter()
                .filter(|(code, _)| code == "??")
                .map(|(_, path)| path.clone())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        self.diff_gen = self.diff_gen.wrapping_add(1);
        let r#gen = self.diff_gen;
        self.git_diff = Some(GitDiff {
            root: root.clone(),
            path: "全部更改".into(),
            aggregate: true,
            worktree_file: None,
            lines: Rc::new(Vec::new()),
            header: String::new(),
            hunks: Rc::new(Vec::new()),
            patchable: false,
            scope,
            has_staged: false,
            patch: None,
        });
        self.begin_diff_load(cx);
        // 从准备流程里调用时，正在跑的刷新会把这次排队丢掉，并在同一次准备里建评论框。
        self.schedule_visible_refresh(cx);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let text = git_call::<GitAllDiffCall>(GitAllDiffParams {
                        root: r,
                        scope,
                        untracked_paths,
                    })
                    .map(|result| result.text)
                    .unwrap_or_else(|err| format!("无法执行 git diff：{err}"));
                    let parsed = parse_diff_with_file_headers(&text);
                    let files = parse_diff_files(&text);
                    (text, parsed, files)
                })
                .await;
            let (text, parsed, files) = result;
            let _ = this.update(cx, |this, cx| {
                if this.diff_gen == r#gen {
                    this.git_diff = Some(GitDiff {
                        root: root.clone(),
                        path: "全部更改".into(),
                        aggregate: true,
                        worktree_file: None,
                        lines: Rc::new(parsed.lines),
                        header: String::new(),
                        hunks: Rc::new(Vec::new()),
                        patchable: false,
                        scope,
                        has_staged: false,
                        patch: Some(text.clone()),
                    });
                    this.install_diff_view(&text, files, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn reveal_pending_diff_file(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.pending_diff_file.clone() else {
            return;
        };
        if self.diff_parse_error.is_some() {
            self.pending_diff_file = None;
            return;
        }
        let Some(root) = self.git_diff.as_ref().map(|diff| diff.root.clone()) else {
            return;
        };
        let ready =
            self.git_diff.as_ref().is_some_and(|diff| {
                aggregate_diff_ready_for_file(diff, &root, self.diff_scope, &path)
            }) || self.loaded_aggregate_has_file(&root, &path, cx);
        if !ready {
            return;
        }
        let Some(view) = self.diff_view.clone() else {
            return;
        };
        let target = view.update(cx, |state, cx| {
            let target = diff_scroll_target(state.files(), &path)
                .unwrap_or(path.as_str())
                .to_string();
            // 点文件是为了看到正文。`scroll_to_file` 停在标题上，不会把折叠展开。
            state.set_file_collapsed(&target, false, cx);
            state.scroll_to_file(&target, cx);
            target
        });
        self.git_collapsed_diff_files.remove(&target);
        self.pending_diff_file = None;
    }

    /// 当前这份已经读完的聚合 diff 里，组件是否认这个状态路径。
    ///
    /// 读取中的视图还是上一份补丁，不能算数，否则会把待定位清掉，新补丁装上后不再滚。
    fn loaded_aggregate_has_file(&self, root: &str, path: &str, cx: &App) -> bool {
        let Some(diff) = self.git_diff.as_ref() else {
            return false;
        };
        if diff.root != root
            || !diff.aggregate
            || diff.scope != self.diff_scope
            || diff.patch.is_none()
        {
            return false;
        }
        self.diff_view
            .as_ref()
            .is_some_and(|view| diff_scroll_target(view.read(cx).files(), path).is_some())
    }

    pub(super) fn clear_diff_comment_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff_comment_range = None;
        self.diff_comment_open = false;
        if let Some(view) = self.diff_view.clone() {
            view.update(cx, |state, cx| state.set_selected_lines(None, cx));
        }
        if let Some(state) = self.diff_comment_input.clone() {
            state.update(cx, |state, cx| state.set_value("", window, cx));
        }
        self.publish_diff_body(cx);
    }

    /// 把选中的源码和评论拼成一段文本，交给这个项目里正在看的会话：终端写进 PTY，
    /// 对话放进输入框（不带回车，留给用户自己看一眼再发送）。没送到时选区和评论都留着。
    pub(super) fn send_diff_comments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = self.diff_comment_range.clone() else {
            return;
        };
        let selected = self
            .diff_view
            .as_ref()
            .map(|view| view.read(cx).selected_text(cx))
            .unwrap_or_default();
        let comment = self
            .diff_comment_input
            .as_ref()
            .map(|state| state.read(cx).value().trim().to_string())
            .unwrap_or_default();
        // 选区带的是文件路径。聚合 diff 的 `git_diff.path` 是「全部更改」。
        let path = range.path().to_string();
        let msg = diff_feedback_text(&path, range.start(), range.end(), &selected, &comment);
        if self.send_to_panel_session(&msg, window, cx) {
            self.clear_diff_comment_selection(window, cx);
        }
    }

    /// 在 `root` 上跑一条 git 写入，回来在 UI 线程收尾。
    ///
    /// 占用和释放都在这里。调用点只提供命令和成功/失败后要改的状态，
    /// 不会忘记放开这个仓库，也不会用字符串对上一次的操作名。
    fn spawn_git<T, Make, Done>(
        &mut self,
        root: &str,
        op: GitWrite,
        make: Make,
        done: Done,
        cx: &mut Context<Self>,
    ) -> bool
    where
        T: Send + 'static,
        Make: FnOnce() -> T + Send + 'static,
        Done: FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    {
        if !reserve_git_write(&mut self.git_writes, root, op) {
            return false;
        }
        cx.notify();
        let root_key = root.to_string();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { make() }).await;
            let _ = this.update(cx, |this, cx| {
                release_git_write(&mut this.git_writes, &root_key, op);
                done(this, result, cx);
                cx.notify();
            });
        })
        .detach();
        true
    }

    /// 同 [`Self::spawn_git`]，收尾时还拿得到点按钮的那个窗口（清提交框要用）。
    fn spawn_git_in<T, Make, Done>(
        &mut self,
        root: &str,
        op: GitWrite,
        window: &mut Window,
        make: Make,
        done: Done,
        cx: &mut Context<Self>,
    ) -> bool
    where
        T: Send + 'static,
        Make: FnOnce() -> T + Send + 'static,
        Done: FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    {
        if !reserve_git_write(&mut self.git_writes, root, op) {
            return false;
        }
        cx.notify();
        let root_key = root.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { make() }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                release_git_write(&mut this.git_writes, &root_key, op);
                done(this, result, window, cx);
                cx.notify();
            });
        })
        .detach();
        true
    }

    /// 只推送，不提交。本地攒了几个 commit 想推上去时用——以前只有「提交并推送」，
    /// 而它要求先写 commit message，于是「没有新改动、只想把已有提交推上去」这条
    /// 最常见的路径反而没有入口。
    pub fn push_only(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.git_write_target(cx) else {
            return;
        };
        let branch = self
            .git_status
            .get(&root)
            .map(|(_, d)| d.branch.clone())
            .unwrap_or_default();
        let key = root.clone();
        let done_root = root.clone();
        if self.spawn_git(
            &key,
            GitWrite::Push,
            move || git_network_call::<GitPushCall>(GitBranchParams { root, branch }).map(drop),
            move |this, result, cx| match result {
                Ok(()) => this.invalidate_git_status(&done_root, cx),
                Err(err) => {
                    this.commit_errors.insert(done_root, err);
                }
            },
            cx,
        ) {
            // 上一次的失败原因已经不再描述当前这次尝试。
            self.commit_errors.remove(&key);
        }
    }

    /// 后台跑一条 git 操作，回来刷新状态 / 报错。fetch·pull·stash 共用这个骨架。
    /// `reload_log` = 操作会改历史（pull）时顺带重拉 log；`silent` = 失败不弹顶部
    /// 错误通知（自动 fetch 用，避免离线/无凭据时反复刷屏）。
    fn run_git_op<F>(
        &mut self,
        op: GitWrite,
        reload_log: bool,
        silent: bool,
        work: F,
        cx: &mut Context<Self>,
    ) where
        F: FnOnce(&str) -> Result<(), String> + Send + 'static,
    {
        let Some(root) = self.git_write_target(cx) else {
            return;
        };
        let key = root.clone();
        let done_root = root.clone();
        self.spawn_git(
            &key,
            op,
            move || work(&root),
            move |this, result, cx| match result {
                Ok(()) => {
                    this.invalidate_git_status(&done_root, cx);
                    if reload_log {
                        this.reload_git_log(done_root, cx);
                    }
                }
                // silent（自动 fetch）：失败不弹顶部错误通知，避免离线 / 无凭据时
                // 每次进 Git 页刷屏；只有手动操作失败才提示。
                Err(err) => {
                    if !silent {
                        this.report_background_error(err);
                    }
                }
            },
            cx,
        );
    }

    /// `git fetch --all --prune`：更新远端追踪，刷新 ahead/behind。手动触发，失败弹错误。
    pub fn git_fetch(&mut self, cx: &mut Context<Self>) {
        self.run_git_op(GitWrite::Fetch, false, false, fetch_remote, cx);
    }

    /// 同 git_fetch，但失败静默：进 Git 页自动 fetch 用，离线 / 无凭据时不刷屏报错。
    pub fn git_fetch_silent(&mut self, cx: &mut Context<Self>) {
        self.run_git_op(GitWrite::Fetch, false, true, fetch_remote, cx);
    }

    /// `git pull --rebase`：拉取并 rebase 本地提交（历史变了 → 重拉 log）。
    pub fn git_pull(&mut self, cx: &mut Context<Self>) {
        self.run_git_op(GitWrite::Pull, true, false, pull_rebase, cx);
    }

    /// `git stash push -u`：储藏全部更改（含未跟踪）。
    ///
    /// 名字是「储藏」不是「暂存」：暂存已经归 staging area 了，两个概念共用一个
    /// 词，用户看到「暂存中…」根本分不清东西进了索引还是 stash 栈。
    pub fn git_stash_push(&mut self, cx: &mut Context<Self>) {
        self.run_git_op(GitWrite::Stash, false, false, stash_push, cx);
    }

    /// `git stash pop`：弹出最近一条 stash。
    pub fn git_stash_pop(&mut self, cx: &mut Context<Self>) {
        self.run_git_op(GitWrite::StashPop, false, false, stash_pop, cx);
    }

    /// 点「丢弃全部更改」：先弹确认，不直接动文件（照 start_discard_file）。
    ///
    /// 同时记下用户此刻看到的整份改动列表；运行时发现多了没见过的新文件、或哪个文件
    /// 内容又变了，就整体拒绝。
    pub fn start_discard_all(&mut self, root: String, cx: &mut Context<Self>) {
        let Some(seen) = self
            .git_status
            .get(&root)
            .map(|(_, status)| status.seen_changes())
        else {
            self.report_background_error("改动列表还没加载出来，请刷新后再丢弃。");
            return;
        };
        self.discard_all_target = Some((root, seen));
        cx.notify();
    }

    /// 取消丢弃全部。
    pub fn cancel_discard_all(&mut self, cx: &mut Context<Self>) {
        self.discard_all_target = None;
        cx.notify();
    }

    /// 确认丢弃工作区全部改动（restore + clean）。
    pub fn confirm_discard_all(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self
            .discard_all_target
            .as_ref()
            .map(|(root, _)| root.clone())
        else {
            return;
        };
        if self.git_writes.contains_key(&root) {
            return;
        }
        let Some((root, seen)) = self.discard_all_target.take() else {
            return;
        };
        let key = root.clone();
        let params = GitDiscardAllParams {
            root: root.clone(),
            seen: seen.clone(),
        };
        let done_root = root.clone();
        let started = self.spawn_git(
            &key,
            GitWrite::DiscardChanges,
            move || git_call::<GitDiscardAllCall>(params).map(drop),
            move |this, result, cx| {
                // 被拒绝多半是列表已经过时，也要刷新，让用户看到现在的样子。
                this.invalidate_git_status(&done_root, cx);
                match result {
                    Ok(()) => {
                        // 丢弃后当前打开的 diff 大概率没了，关掉免得看空。
                        this.reset_git_diff_view();
                    }
                    Err(err) => this.report_background_error(err),
                }
            },
            cx,
        );
        if !started {
            self.discard_all_target = Some((root, seen));
        }
    }

    /// Git 页「提交」/「提交并推送」共用入口：直接执行（不再走"发到终端"那套）——
    /// 要提交的内容已经在暂存区里明明白白摆着（部分暂存那套勾选框），跟 stage/
    /// checkout 一样属于本地可控操作，不需要再让人去终端里确认一遍回车；真正没法
    /// 回头的风险点在 push 影响远程共享状态，但 WebStorm 等主流 git 客户端也是
    /// 「提交并推送」一键做的，这里跟随这个惯例。
    ///
    /// 校验没过不会占这个仓库的写入格。
    pub(super) fn commit(&mut self, push: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.git_write_target(cx) else {
            return;
        };
        if self.git_writes.contains_key(&root) {
            return;
        }
        let Some(message) = self
            .commit_msg_inputs
            .get(&root)
            .map(|s| s.read(cx).value().trim().to_string())
        else {
            return;
        };
        if message.is_empty() {
            return;
        }
        let branch = self
            .git_status
            .get(&root)
            .map(|(_, d)| d.branch.clone())
            .unwrap_or_default();
        let op = if push {
            GitWrite::CommitAndPush
        } else {
            GitWrite::Commit
        };
        let key = root.clone();
        let done_root = root.clone();
        if self.spawn_git_in(
            &key,
            op,
            window,
            move || {
                git_network_call::<GitCommitCall>(GitCommitParams {
                    root,
                    message,
                    push,
                    branch,
                })
                .map(drop)
            },
            move |this, result, window, cx| match result {
                Ok(()) => {
                    this.invalidate_git_status(&done_root, cx);
                    if let Some(input) = this.commit_msg_inputs.get(&done_root).cloned() {
                        input.update(cx, |s, cx| s.set_value("", window, cx));
                    }
                }
                Err(err) => {
                    this.commit_errors.insert(done_root, err);
                }
            },
            cx,
        ) {
            // 新的一次提交开始，旧错误先清——否则用户分不清面板上那条是这次的还是上次的。
            self.commit_errors.remove(&key);
        }
    }

    /// 全部改动：一份 diff 里所有文件一起展开或收起。
    ///
    /// `set_file_collapsed` 不发折叠事件，折叠集合要在这里自己写。
    pub fn toggle_all_aggregate_diff_files(&mut self, cx: &mut Context<Self>) {
        let Some(diff) = self.git_diff.as_ref() else {
            return;
        };
        if !diff.aggregate {
            return;
        }
        let Some(view) = self.diff_view.clone() else {
            return;
        };
        let paths: Vec<String> = view
            .read(cx)
            .files()
            .iter()
            .map(|file| file.path().to_string())
            .collect();
        if paths.is_empty() {
            return;
        }
        // 还有展开着的就全部收起，否则全部展开。
        let collapse = paths
            .iter()
            .any(|path| !self.git_collapsed_diff_files.contains(path));
        view.update(cx, |state, cx| {
            for path in &paths {
                state.set_file_collapsed(path, collapse, cx);
            }
        });
        if collapse {
            self.git_collapsed_diff_files.extend(paths);
        } else {
            for path in &paths {
                self.git_collapsed_diff_files.remove(path);
            }
        }
    }
}

fn stage_or_unstage(root: String, paths: Vec<String>, stage: bool) -> Result<(), String> {
    let params = GitPathsParams { root, paths };
    if stage {
        git_call::<GitStageCall>(params).map(drop)
    } else {
        git_call::<GitUnstageCall>(params).map(drop)
    }
}

fn fetch_remote(root: &str) -> Result<(), String> {
    git_network_call::<GitFetchCall>(root_params(root)).map(drop)
}

fn pull_rebase(root: &str) -> Result<(), String> {
    git_network_call::<GitPullCall>(root_params(root)).map(drop)
}

fn stash_push(root: &str) -> Result<(), String> {
    git_call::<GitStashPushCall>(root_params(root)).map(drop)
}

fn stash_pop(root: &str) -> Result<(), String> {
    git_call::<GitStashPopCall>(root_params(root)).map(drop)
}
