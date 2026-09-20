//! Git 面板中 Workspace 的数据修改：worktree、暂存/提交、diff 生命周期。
//!
//! Git 数据模型和纯解析/行渲染函数留在父模块；这里集中放置依赖 Workspace
//! 状态的交互方法。页面渲染在 `view.rs`。字段仍由 main.rs 的 Workspace 统一持有。

use super::*;

// ===================== Workspace 方法 =====================

impl Workspace {
    /// 项目行右键「新增 Worktree」：打开弹窗（分支名 + 检出目录两个输入框）。
    /// main_root/base_branch 由 session_list 用已缓存的 repo_info 算好传进来，
    /// 不重复跑 git。默认检出路径：主仓库同级目录 `<仓库名>-<分支>`。
    pub fn open_new_worktree(
        &mut self,
        main_root: String,
        base_branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::input::{InputEvent, InputState};
        let repo_name = Path::new(&main_root)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("worktree");
        let suffix = if base_branch.is_empty() {
            "wt".to_string()
        } else {
            base_branch.clone()
        };
        let default_path = Path::new(&main_root)
            .parent()
            .map(|p| {
                p.join(format!("{repo_name}-{suffix}"))
                    .display()
                    .to_string()
            })
            .unwrap_or_else(|| format!("{main_root}-{suffix}"));

        let branch_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("分支名（留空 = detached HEAD）"));
        let path_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("worktree 检出目录")
                .default_value(default_path)
        });
        // 路径框回车 = 直接创建（主表单只有一个动作，提交体验顺一点）。
        let mut subs = Vec::new();
        subs.push(
            cx.subscribe(&path_input, move |this, _s, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.confirm_new_worktree(cx);
                }
            }),
        );

        path_input.update(cx, |input, cx| input.focus(window, cx));
        self.new_worktree = Some(NewWorktreeState {
            main_root,
            base_branch,
            branch_input,
            path_input,
            busy: false,
            error: None,
            _subs: subs,
        });
        cx.notify();
    }

    /// 取消新建 worktree：不落地任何改动，输入框随 state drop。
    pub fn cancel_new_worktree(&mut self, cx: &mut Context<Self>) {
        self.new_worktree = None;
        cx.notify();
    }

    /// 确认新建：后台跑 `git worktree add`，成功后在新建目录打开一个终端会话
    /// （建 worktree 就是要进去干活），并刷新「关联 Worktree」列表（若开着）。
    pub fn confirm_new_worktree(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.new_worktree.as_mut() else {
            return;
        };
        if state.busy {
            return;
        }
        let main_root = state.main_root.clone();
        let branch = state.branch_input.read(cx).value().to_string();
        let path = state.path_input.read(cx).value().to_string();
        let path = path.trim().to_string();
        if path.is_empty() {
            state.error = Some("请填写 worktree 检出目录".to_string());
            cx.notify();
            return;
        }
        state.busy = true;
        state.error = None;
        cx.notify();
        let branch_opt = (!branch.trim().is_empty()).then(|| branch.trim().to_string());
        cx.spawn(async move |this, cx| {
            let b = branch_opt.clone();
            let p = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { create_worktree(&main_root, b.as_deref(), &p) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.new_worktree.as_mut() else {
                    return;
                };
                match result {
                    Ok(created) => {
                        this.new_worktree = None;
                        // 列表弹窗还开着就刷新，让新 worktree 立刻出现。
                        if this.worktree_list.is_some() {
                            this.refresh_worktree_list(cx);
                        }
                        // 建完直接在新目录开一个终端会话。
                        this.add_session(Some(created), cx);
                    }
                    Err(err) => {
                        state.busy = false;
                        state.error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 项目行右键「关联 Worktree」：打开弹窗并后台列出这个仓库的全部 worktree。
    pub fn open_worktree_list(&mut self, root: String, cx: &mut Context<Self>) {
        self.worktree_list = Some(WorktreeListState {
            root,
            main_root: String::new(),
            entries: None,
            error: None,
        });
        cx.notify();
        self.load_worktree_list(cx);
    }

    /// 关闭「关联 Worktree」弹窗。
    pub fn close_worktree_list(&mut self, cx: &mut Context<Self>) {
        self.worktree_list = None;
        cx.notify();
    }

    /// 重新加载 worktree 列表（删除/清理成功后刷新用）。弹窗没开着就什么都不做。
    pub fn refresh_worktree_list(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.worktree_list.as_mut() else {
            return;
        };
        state.entries = None;
        state.error = None;
        cx.notify();
        self.load_worktree_list(cx);
    }

    fn load_worktree_list(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.worktree_list.as_mut() else {
            return;
        };
        let root = state.root.clone();
        cx.spawn(async move |this, cx| {
            let probe_root = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move { list_worktrees(&probe_root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.worktree_list.as_mut() else {
                    return;
                };
                // 弹窗期间用户可能又打开了别的仓库，只在还是同一个 root 时写回。
                if state.root != root {
                    return;
                }
                match result {
                    Ok(data) => {
                        state.main_root = data.main_root;
                        state.entries = Some(data.entries);
                        state.error = None;
                    }
                    Err(err) => {
                        state.entries = Some(Vec::new());
                        state.error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 「清理失效项」：后台 `git worktree prune` + 隔离工作区注册表对账，完成后刷新列表。
    pub fn prune_stale_worktrees(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.worktree_list.as_mut() else {
            return;
        };
        let main_root = if state.main_root.is_empty() {
            state.root.clone()
        } else {
            state.main_root.clone()
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { prune_stale_worktrees(&main_root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.worktree_list.as_mut() else {
                    return;
                };
                match result {
                    Err(err) => state.error = Some(err),
                    Ok(()) => {
                        state.entries = None;
                        state.error = None;
                        this.load_worktree_list(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 项目行右键「删除 Worktree」（仅 worktree 分组显示，见 render 里 is_worktree_group）：
    /// 先弹窗（dirty=None，显示"检查中…"），后台探测有没有未提交改动，探测完再把
    /// dirty 写回去驱动弹窗文案/是否要红色警告。main_root 是同仓库下的主仓库根目录，
    /// 真正执行删除时 `git worktree remove` 要从那跑（不能从待删目录自己发起）。
    pub fn start_delete_worktree(
        &mut self,
        path: String,
        main_root: String,
        branch: String,
        cx: &mut Context<Self>,
    ) {
        self.delete_worktree_target = Some(DeleteWorktreeTarget {
            path: path.clone(),
            main_root,
            branch,
            dirty: None,
        });
        cx.notify();
        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let dirty = cx
                .background_executor()
                .spawn(async move {
                    run_git(&p, &["status", "--porcelain"])
                        .ok()
                        .is_some_and(|o| o.success() && !o.stdout.is_empty())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // 弹窗期间用户可能已经取消/又点了别的 worktree，只在还是同一个目标时写回。
                if this
                    .delete_worktree_target
                    .as_ref()
                    .is_some_and(|t| t.path == path)
                {
                    if let Some(t) = this.delete_worktree_target.as_mut() {
                        t.dirty = Some(dirty);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 确认删除：先关掉这个 worktree 下的所有会话，再后台跑 `git worktree remove`
    /// （探测出有未提交改动就带 --force——用户已经在弹窗里看到红色警告并主动点了
    /// 确定）。失败写 background_error，交给 render 顶部弹通知。
    pub fn confirm_delete_worktree(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.delete_worktree_target.take() else {
            return;
        };
        cx.notify();
        self.close_sessions_under(&target.path, cx);
        let force = target.dirty.unwrap_or(false);
        cx.spawn(async move |this, cx| {
            let path = target.path.clone();
            let main_root = target.main_root.clone();
            let git_path = path.clone();
            let git_result = cx
                .background_executor()
                .spawn(async move { remove_worktree(&main_root, &git_path, force) })
                .await;
            match git_result {
                Err(err) => {
                    let _ = this.update(cx, |this, cx| {
                        this.background_error = Some(err);
                        cx.notify();
                    });
                }
                Ok(()) => {
                    // 「关联 Worktree」弹窗还开着就刷新，让刚删掉的条目从列表消失。
                    let _ = this.update(cx, |this, cx| {
                        if this.worktree_list.is_some() {
                            this.refresh_worktree_list(cx);
                        }
                    });
                }
            }
        })
        .detach();
    }

    /// 取消删除 worktree：不落地任何改动。
    pub fn cancel_delete_worktree(&mut self, cx: &mut Context<Self>) {
        self.delete_worktree_target = None;
        cx.notify();
    }

    /// 文件右键「丢弃改动」：先弹确认。untracked 标记决定是删盘还是 restore。
    pub fn start_discard_file(
        &mut self,
        root: String,
        path: String,
        untracked: bool,
        cx: &mut Context<Self>,
    ) {
        self.discard_file_target = Some((root, path, untracked));
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
        let Some((root, path, untracked)) = self.discard_file_target.take() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let (r, p) = (root.clone(), path.clone());
            let result = cx
                .background_executor()
                .spawn(async move {
                    if untracked {
                        let full = Path::new(&r).join(&p);
                        return LocalFs
                            .remove_file(&full)
                            .map_err(|e| format!("删除 {p} 失败：{e}"));
                    }
                    // 行自带仓库根，直接在它自己的仓库里丢弃。
                    let out = run_git(&r, &["restore", "--staged", "--worktree", "--", &p])
                        .map_err(|e| e.to_string())?;
                    if out.success() {
                        Ok(())
                    } else {
                        Err(git_err(&out, "git restore 失败"))
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.invalidate_git_status(&root);
                        // 丢弃的正是当前打开的那个文件时，diff 已经没意义了，关掉。
                        if this.git_diff.as_ref().is_some_and(|d| d.path == path) {
                            this.git_diff = None;
                            this.active_hunk = None;
                        }
                    }
                    Err(err) => this.background_error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// F7 / Shift+F7：跳到下一个 / 上一个改动块并滚到视野里。
    ///
    /// 首次按跳第一块；到头就停在两端，不回绕——回绕会让人以为还有更多改动。
    pub fn jump_hunk(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(d) = self.git_diff.as_ref() else {
            return;
        };
        let n = d.hunks.len();
        if n == 0 {
            return;
        }
        let next = match self.active_hunk {
            None => 0,
            Some(i) if forward => (i + 1).min(n - 1),
            Some(i) => i.saturating_sub(1),
        };
        let line = d.hunks[next].range.start;
        // 并排视图里 uniform_list 的 item 是 SplitRow，下标和 lines 的下标不是一回事，
        // 直接拿行号去滚会滚到别的位置——先翻译成 row 下标。
        let item = if self.diff_split {
            build_split_rows(&d.lines)
                .iter()
                .position(|r| matches!(r, SplitRow::Full(i) if *i == line))
                .unwrap_or(line)
        } else {
            line
        };
        self.active_hunk = Some(next);
        self.diff_scroll
            .scroll_to_item(item, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    /// 点「丢弃块」：先弹确认，不直接动文件。
    pub fn start_discard_hunk(&mut self, root: String, idx: usize, cx: &mut Context<Self>) {
        self.discard_hunk_target = Some((root, idx));
        cx.notify();
    }

    /// 确认丢弃：关弹窗并真正执行 reverse apply。
    pub fn confirm_discard_hunk(&mut self, cx: &mut Context<Self>) {
        let Some((root, idx)) = self.discard_hunk_target.take() else {
            return;
        };
        self.discard_hunk(root, idx, cx);
    }

    /// 取消丢弃：什么都不发生。
    pub fn cancel_discard_hunk(&mut self, cx: &mut Context<Self>) {
        self.discard_hunk_target = None;
        cx.notify();
    }

    /// 给某个 root 建一次性的文件监听（notify crate，macOS 走 FSEvents）：文件系统
    /// 回调通过有界通道唤醒 GPUI 任务，git 页只在真实文件事件到达时刷新。
    ///
    /// 递归监听整棵目录树（含 .git/ 内部），**但过滤高频噪音路径**（构建产物/依赖
    /// 目录）——不滤的话，rust-analyzer/cargo 持续写 target/、node_modules 的每次
    /// 触碰都会触发一次 git status + 整窗重绘，空闲 CPU 高、耗电的主要来源。
    /// 只在这个 root 第一次进 Git 页时建一次；watcher 存进 git_watchers 常驻到应用退出
    /// （必须持有 watcher 不被 drop，否则会停止收事件）。
    pub fn ensure_git_watch(&mut self, root: String, cx: &mut Context<Self>) {
        if self.git_watchers.contains_key(&root) {
            return;
        }
        // 通道容量为 1：一批文件事件只需一次刷新唤醒，避免保存/构建产生的事件
        // 在队列里无限堆积。GPUI 任务真正消费唤醒后再读取最新工作区状态。
        let (dirty_tx, dirty_rx) = smol::channel::bounded::<()>(1);
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                // 噪音过滤：构建产物 / 依赖目录变化极频繁，监听它们只会让
                // git status 反复空跑。只有真实源码/配置文件变化才标脏。
                if event.paths.iter().any(|p| is_git_noise_path(p)) {
                    return;
                }
                // 回调运行在 notify 的线程上，不能直接触碰 GPUI；只发一个唤醒信号。
                let _ = dirty_tx.try_send(());
            }
        });
        let Ok(mut watcher) = watcher else { return };
        if watcher
            .watch(Path::new(&root), RecursiveMode::Recursive)
            .is_err()
        {
            return;
        }
        self.git_watchers.insert(root.clone(), watcher);
        // 立刻拉一次初值：之后纯靠文件事件驱动，但首帧得先有数据，
        // 否则文件一直不变时侧栏 GIT 角标永远出不来。
        self.ensure_git_status(root.clone(), cx);

        cx.spawn(async move |this, cx| {
            while dirty_rx.recv().await.is_ok() {
                // 文件变了：标脏并主动重拉 git_status，角标 / Git 页在任何页面都即时刷新，
                // 不依赖「当前正停在 Files/Git 页、render 每帧才调 ensure_git_status」。
                // ensure_git_status 内部有 inflight 去重，重复唤醒不会叠并发。
                let r = this.update(cx, |this, cx| {
                    // 监听是递归的，子仓里的改动也会到这里——所以要把该项目下**所有**
                    // 仓库的 status 都标脏，只刷项目根会让子仓改动在面板里冻结。
                    this.invalidate_project_git_status(&root);
                    this.ensure_git_repos(root.clone(), cx);
                    cx.notify();
                });
                if r.is_err() {
                    break; // Workspace 已销毁
                }
            }
        })
        .detach();
    }

    /// 确保某 cwd 的仓库身份缓存新鲜（>5s 或缺失就后台刷新）：探测它是不是 worktree
    /// 检出、当前分支是什么，供侧栏分组用（见 group_info_for_cwd）。跟 git_status
    /// 不同，这个要对**所有**会话的 cwd 探测（侧栏一直显示全部项目分组，不止当前
    /// 打开的那个），所以单独走一套缓存，TTL 也放宽到 5s——身份和分支不常变，没必要
    /// 跟 git status 一样 1.5s 就重跑。非 git 目录（比如临时终端的 $HOME）缓存
    /// None，同样不重复重试。
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
                .spawn(async move { load_repo_info(&c) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.repo_info_inflight.remove(&cwd);
                this.repo_info.insert(cwd, (Instant::now(), info));
                cx.notify();
            });
        })
        .detach();
    }

    /// cwd → 侧栏分组显示名 + 聚簇 key。是 worktree 检出（git-dir ≠ common-dir）就
    /// 显示「仓库名 · 分支名」，聚簇 key 用 common-dir（worktree 和主仓库共享同一个
    /// 值，project_groups 靠它把同仓库的组排在一起）；非 git 目录 / 身份缓存还没到位
    /// 就退回旧的纯目录末段名（跟改动前完全一致，不会闪烁成别的样子）。
    pub fn group_info_for_cwd(&self, cwd: &str) -> (String, Option<String>) {
        let base_name = crate::project_name_for_cwd(cwd);
        match self.repo_info.get(cwd).and_then(|(_, info)| info.as_ref()) {
            Some(info) if info.is_worktree() => {
                let repo_label = repo_label_from_common_dir(&info.common_dir).unwrap_or(base_name);
                (
                    format!("{repo_label} · {}", info.branch),
                    Some(info.common_dir.clone()),
                )
            }
            Some(info) => (base_name, Some(info.common_dir.clone())),
            None => (base_name, None),
        }
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

    pub fn invalidate_git_status(&mut self, root: &str) {
        self.mark_git_status_dirty(root);
    }

    /// 把一个项目下所有已发现仓库的 status 都标脏。
    ///
    /// 文件监听是整棵目录递归的，一次事件分不出是哪个仓库变了；全部标脏交给
    /// `ensure_git_status` 的 TTL 与 inflight 去重去抓实际开销。
    pub fn invalidate_project_git_status(&mut self, project_root: &str) {
        let roots: Vec<String> = match self.git_repos.get(project_root) {
            Some((_, set)) if !set.repos.is_empty() => set.roots().map(str::to_string).collect(),
            _ => vec![project_root.to_string()],
        };
        for root in roots {
            self.mark_git_status_dirty(&root);
        }
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
                        smelt_git::discovery::discover(
                            &LocalFs,
                            &smelt_git::LocalSubprocess,
                            Path::new(&r),
                            &smelt_git::discovery::DiscoveryOptions::default(),
                        )
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
                .spawn(async move { load_git_status(&r) })
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
        self.branches_inflight.insert(root.clone());
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let list = cx
                .background_executor()
                .spawn(async move { load_branches(&r) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.branches_inflight.remove(&root);
                this.branches.insert(root, (Instant::now(), list));
                cx.notify();
            });
        })
        .detach();
    }

    /// Git 页组页面前的数据准备：失效跨仓库 diff、默认打开全部改动、懒建输入框。
    /// 缓存刷新（status / branches / watch）仍由 `prepare_frame` 统一调度。
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
            self.reveal_pending_diff_file();
        } else if self.git_tab == GitTab::Log {
            self.ensure_git_log(root, cx);
        }
    }

    /// Git 页分支切换下拉：checkout 目标分支（本地分支直接切；远程分支传短名，靠 git
    /// 内建 DWIM 自动建好跟踪分支——跟 create_worktree 判断分支存不存在同一个逻辑）。
    /// 成功后清掉 status/分支缓存强制下一帧重新拉（文件、ahead/behind 全变了）。
    pub fn checkout_branch(&mut self, root: String, branch: String, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let b = branch.clone();
            let result = cx
                .background_executor()
                .spawn(async move { checkout_git_branch(&r, &b) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    // 切分支后文件列表、ahead/behind 都变了，标脏逼下一帧重新拉取；
                    // 分支列表本身（有哪些分支）不受切换影响，不用跟着失效。
                    Ok(()) => this.invalidate_git_status(&root),
                    Err(err) => this.background_error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
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
        let Some((root, branch, remote)) = self.delete_branch_target.take() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let (r, b) = (root.clone(), branch.clone());
            let result = cx
                .background_executor()
                .spawn(async move { delete_git_branch(&r, &b, remote) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.invalidate_git_status(&root);
                        // 分支没了，列表和日志都得重拉。
                        this.branches.remove(&root);
                        this.reload_git_log(root.clone(), cx);
                    }
                    Err(err) => this.background_error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 把某个分支合并进当前分支（`git merge <branch>`）。
    ///
    /// 冲突时 git 会以非 0 退出并把冲突文件写进工作区——此时**不自动 abort**，
    /// 保留现场让人去解，只把提示报出来（自动回滚会让人措手不及）。
    pub fn merge_branch(&mut self, root: String, branch: String, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (r, b) = (root.clone(), branch.clone());
            let result = cx
                .background_executor()
                .spawn(async move { merge_git_branch(&r, &b) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.invalidate_git_status(&root);
                        this.reload_git_log(root.clone(), cx);
                    }
                    Err(err) => this.background_error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Git 页文件列表勾选框：把某个改动文件加入暂存区（`git add --`，untracked/修改/
    /// 删除都适用）。纯本地索引改动、可逆，不像 commit/push 那样需要走"发到终端让人
    /// 确认"那一套，直接执行。成功后清 git_status 缓存强制下一帧重新拉状态。
    pub fn stage_file(&mut self, root: String, path: String, cx: &mut Context<Self>) {
        self.run_git_index_op(root, path, stage_git_file, cx);
    }

    /// 文件列表取消勾选：把已暂存的改动移出暂存区（`git reset --`），不影响工作区
    /// 内容本身，随时能重新勾选加回去。
    pub fn unstage_file(&mut self, root: String, path: String, cx: &mut Context<Self>) {
        self.run_git_index_op(root, path, unstage_git_file, cx);
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
        let op_name = if stage { "暂存" } else { "取消暂存" };
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let args: Vec<&str> = if stage {
                        std::iter::once("add")
                            .chain(std::iter::once("--"))
                            .collect()
                    } else {
                        ["reset", "--"].into_iter().collect()
                    };
                    let mut full: Vec<&str> = args;
                    full.extend(paths.iter().map(String::as_str));
                    run_git(&r, &full)
                        .map_err(|error| error.to_string())
                        .and_then(|out| {
                            if out.success() {
                                Ok(())
                            } else {
                                Err(git_err(&out, "git 操作失败"))
                            }
                        })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(err) = result {
                    this.background_error = Some(format!("{op_name}失败：{err}"));
                }
                // 批量操作会一口气改掉一片文件的暂存状态，之前留下的单文件乐观状态
                // 全部作废；留着它们只会让复选标记跟真实 status 对不上。
                this.git_index_pending
                    .retain(|(pending_root, _), _| pending_root != &root);
                this.invalidate_git_status(&root);
                this.ensure_git_status(root.clone(), cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// stage_file/unstage_file 共用的后台执行 + 缓存失效逻辑：`git <args.. > -- <path>`。
    fn run_git_index_op(
        &mut self,
        root: String,
        path: String,
        op: fn(&str, &str) -> Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        let pending_key = (root.clone(), path.clone());
        if git_index_op_in_flight(self.git_index_pending.get(&pending_key)) {
            return;
        }
        self.git_index_pending
            .insert(pending_key.clone(), PendingGitIndexOp { completed: false });
        // 立刻把这一行标成"操作进行中"，不等 git 进程和 status 回包。
        cx.notify();

        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let p = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { op(&r, &p) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.mark_git_status_dirty(&root);
                        if let Some(pending) = this.git_index_pending.get_mut(&pending_key) {
                            pending.completed = true;
                        }
                        // 不依赖下一次 render 或 watcher；操作完成后立即发起权威刷新。
                        // 若已有旧请求在途，generation 会让它回包后自动补拉。
                        this.ensure_git_status(root.clone(), cx);
                    }
                    Err(err) => {
                        this.git_index_pending.remove(&pending_key);
                        this.background_error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
                self.cur()
                    .and_then(|session| session.cwd(cx))
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
        let aggregate_ready = self
            .git_diff
            .as_ref()
            .is_some_and(|diff| aggregate_diff_ready_for_file(diff, &root, self.diff_scope, &path));
        let aggregate_loading = self.git_diff.as_ref().is_some_and(|diff| {
            diff.root == root
                && diff.aggregate
                && diff.scope == self.diff_scope
                && diff.lines.is_empty()
        });
        if !aggregate_ready && !aggregate_loading {
            self.open_all_diffs_at(root, cx);
        }
        cx.notify();
    }

    /// 清掉当前 Git diff 及其交互状态。进入新的 Git 改动页或切换仓库时必须失效
    /// 正在加载的旧结果，否则异步回调可能把旧文件重新放回主面板。
    pub(crate) fn reset_git_diff_view(&mut self) {
        self.diff_gen = self.diff_gen.wrapping_add(1);
        self.git_diff = None;
        self.pending_diff_file = None;
        self.diff_selected.clear();
        self.diff_selection_anchor = None;
        self.diff_selection_cursor = None;
        self.diff_selection_dragging = false;
        self.diff_comment_open = false;
        self.active_hunk = None;
    }

    /// 把第 `idx` 个 hunk 单独加入暂存区（`git apply --cached`）。
    ///
    /// 只暂存一块、其余留在工作区，是 agent 写的代码「对一半」时最需要的动作：挑出
    /// 对的先存下来，剩下的继续让它改。
    pub fn stage_hunk(&mut self, root: String, idx: usize, cx: &mut Context<Self>) {
        self.apply_hunk(root, idx, &["apply", "--cached", "-"], cx);
    }

    /// 把第 `idx` 个 hunk 撤出暂存区（`git apply --cached --reverse`）。
    ///
    /// 只在「已暂存」视图下给：那时 diff 就是索引相对 HEAD 的差异，reverse 回去
    /// 正好把这一块退回工作区，文件内容不受影响。
    pub fn unstage_hunk(&mut self, root: String, idx: usize, cx: &mut Context<Self>) {
        self.apply_hunk(root, idx, &["apply", "--cached", "--reverse", "-"], cx);
    }

    /// 丢弃第 `idx` 个 hunk（`git apply --reverse`，作用于工作区文件）。
    ///
    /// **会真的改用户的文件且不进 reflog**，调用方必须先让用户确认过。
    pub fn discard_hunk(&mut self, root: String, idx: usize, cx: &mut Context<Self>) {
        self.apply_hunk(root, idx, &["apply", "--reverse", "-"], cx);
    }

    /// stage_hunk / discard_hunk 共用：拼 patch → 后台 `git apply` → 刷新状态并重开 diff。
    ///
    /// 成功后必须重新拉一次 diff：apply 之后剩余 hunk 的行号和分段都变了，接着用旧的
    /// 下标去点第二块，改的就是别的地方（`-U0` 那次错位是同一类问题）。
    fn apply_hunk(
        &mut self,
        root: String,
        idx: usize,
        args: &'static [&'static str],
        cx: &mut Context<Self>,
    ) {
        let Some(d) = self.git_diff.as_ref() else {
            return;
        };
        if !d.patchable {
            self.background_error =
                Some("这个 diff 不支持按块操作（子模块 / 未跟踪文件），请用整文件的勾选框".into());
            cx.notify();
            return;
        }
        let Some(hunk) = d.hunks.get(idx) else { return };
        let patch = hunk_patch(&d.header, hunk);
        let path = d.path.clone();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let Err(raw) = apply_git_patch(&r, args, &patch) else {
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
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.invalidate_git_status(&root);
                        // 行号已变，重新解析一份，避免下一次点击打在错的位置上。
                        this.open_diff(root.clone(), path, false, cx);
                    }
                    Err(err) => this.background_error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
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
        });
        self.diff_selected.clear(); // 换文件/重开 diff：旧的行选区不再对应新内容
        self.diff_selection_anchor = None;
        self.diff_selection_cursor = None;
        self.diff_selection_dragging = false;
        self.diff_comment_open = false;
        self.active_hunk = None; // 块下标同理，换了文件就不指向原来那块了
        // diff 内嵌显示在改动页里（停靠或展开态用同一份 UI，见 git_narrow_panel），
        // 不再提升到单独的舞台页——跟 Files 点文件不提升到舞台是同一个道理。
        self.git_tab = crate::GitTab::Changes;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let (r, p) = (root.clone(), path.clone());
            let parsed = cx
                .background_executor()
                .spawn(async move {
                    // 文件已经带着自己的仓库根进来，不用再按路径猜它属于哪个仓库。
                    let (cwd, rel) = (r.clone(), p.clone());
                    let has_staged = !untracked
                        && run_git(&cwd, &["diff", "--staged", "--quiet", "--", &rel])
                            .map(|o| !o.success())
                            .unwrap_or(false);
                    let text = if untracked {
                        match run_git(&r, &["diff", "--no-index", "--", "/dev/null", &p]) {
                            Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
                            Err(err) => format!("无法执行 git diff：{err}"),
                        }
                    } else {
                        let mut args: Vec<&str> = scope.args().to_vec();
                        args.push(&rel);
                        match run_git(&cwd, &args) {
                            Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
                            Err(err) => format!("无法执行 git diff：{err}"),
                        }
                    };
                    let is_gitlink = text.lines().any(|l| l.starts_with("Submodule "));
                    let worktree_file = full_file_path(&r, &p, false)
                        .filter(|full_path| is_regular_worktree_file(Path::new(full_path)));
                    (
                        parse_diff(&text),
                        !untracked && !is_gitlink,
                        has_staged,
                        worktree_file,
                    )
                })
                .await;
            let (parsed, patchable, has_staged, worktree_file) = parsed;
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
                    });
                    // 异步结果替换了 diff 内容；同一 diff_gen 下缓存键不变，
                    // 必须主动失效，否则会继续复用加载前的空行列表。
                    this.diff_derived = None;
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
        // 普通打开“全部改动”从顶部开始；文件树导航触发的重载则必须保留 pending，
        // 否则这里排队的 deferred scroll-to-0 可能在异步结果回来后覆盖目标定位。
        if self.pending_diff_file.is_none() {
            self.diff_scroll
                .scroll_to_item(0, gpui::ScrollStrategy::Top);
        }
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
        });
        self.diff_selected.clear();
        self.diff_selection_anchor = None;
        self.diff_selection_cursor = None;
        self.diff_selection_dragging = false;
        self.diff_comment_open = false;
        self.active_hunk = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut text = run_git(&r, scope.args())
                        .map(|out| String::from_utf8_lossy(&out.stdout).to_string())
                        .unwrap_or_else(|err| format!("无法执行 git diff：{err}"));
                    // 不再把子仓的文件 diff 拼进父仓：子仓有自己的分组和自己的提交入口。
                    // 父仓这边只展示 gitlink 指针变化（它确实是父仓待提交的改动）。
                    // 旧行为把子仓里**已提交**的内容也摊成几十个文件，与变更列表对不上。
                    for path in untracked_paths {
                        let chunk = run_git(&r, &["diff", "--no-index", "--", "/dev/null", &path])
                            .ok()
                            .filter(|out| !out.stdout.is_empty())
                            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
                            .unwrap_or_else(|| smelt_git::untracked_entry_diff(&path));
                        if chunk.is_empty() {
                            continue;
                        }
                        if !text.is_empty() && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        text.push_str(&chunk);
                    }
                    parse_diff_with_file_headers(&text)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.diff_gen == r#gen {
                    this.git_diff = Some(GitDiff {
                        root: root.clone(),
                        path: "全部更改".into(),
                        aggregate: true,
                        worktree_file: None,
                        lines: Rc::new(result.lines),
                        header: String::new(),
                        hunks: Rc::new(Vec::new()),
                        patchable: false,
                        scope,
                        has_staged: false,
                    });
                    // 聚合 diff 的异步结果与占位对象共用同一代数，不能只依赖
                    // diff_gen 判断缓存是否过期。
                    this.diff_derived = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn reveal_pending_diff_file(&mut self) {
        let Some(path) = self.pending_diff_file.clone() else {
            return;
        };
        let comment_after_line = self
            .diff_comment_open
            .then(|| self.diff_selected.iter().max().copied())
            .flatten();
        let (loaded, scroll_top) = self
            .git_diff
            .as_ref()
            .and_then(|diff| {
                diff.aggregate.then(|| {
                    (
                        !diff.lines.is_empty(),
                        aggregate_file_scroll_top(
                            &diff.lines,
                            &path,
                            &self.git_collapsed_diff_files,
                            self.diff_split,
                            comment_after_line,
                        ),
                    )
                })
            })
            .unwrap_or((false, None));
        if !loaded {
            return;
        }
        self.pending_diff_file = None;
        if let Some(scroll_top) = scroll_top {
            let current_offset = self.diff_scroll.offset();
            self.diff_scroll
                .set_offset(point(current_offset.x, px(-scroll_top)));
        }
    }

    /// 开始一次代码审查式拖选。每次新的按下都替换旧选区，语义稳定且不会留下
    /// 分散的“勾选行”；多段评论可以逐段发送。
    pub(super) fn begin_diff_selection(&mut self, i: usize, cx: &mut Context<Self>) {
        self.diff_selection_anchor = Some(i);
        self.diff_selection_cursor = Some(i);
        self.diff_selection_dragging = true;
        self.diff_selected.clear();
        self.diff_selected.insert(i);
        self.diff_comment_open = false;
        cx.notify();
    }

    /// 指针拖过新行时，把选区重建为从锚点到当前行的连续范围。只收集可评论的
    /// 代码行，避免把 @@ hunk 标题或文件元信息混进反馈正文。
    pub(super) fn extend_diff_selection(&mut self, i: usize, cx: &mut Context<Self>) {
        if !self.diff_selection_dragging {
            return;
        }
        let Some(anchor) = self.diff_selection_anchor else {
            return;
        };
        let Some(diff) = self.git_diff.as_ref() else {
            return;
        };
        let (start, end) = if anchor <= i {
            (anchor, i)
        } else {
            (i, anchor)
        };
        let selected = (start..=end)
            .filter(|&ix| diff.lines.get(ix).is_some_and(is_commentable_diff_line))
            .collect();
        let cursor_is_commentable = diff.lines.get(i).is_some_and(is_commentable_diff_line);
        self.diff_selected = selected;
        if cursor_is_commentable {
            self.diff_selection_cursor = Some(i);
        }
        cx.notify();
    }

    /// 松开鼠标只完成范围；评论器由锚点 `+` 单独触发，拖拽期间列表不会变高。
    pub(super) fn finish_diff_selection(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.diff_selection_dragging {
            return;
        }
        self.diff_selection_dragging = false;
        cx.notify();
    }

    /// 点击选区起点的 `+` 后才展开评论卡并聚焦输入框。
    pub(super) fn open_diff_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.diff_selected.is_empty() {
            return;
        }
        self.diff_comment_open = true;
        if let Some(state) = self.diff_comment_input.clone() {
            state.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    pub(super) fn clear_diff_comment_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.diff_selection_anchor = None;
        self.diff_selection_cursor = None;
        self.diff_selection_dragging = false;
        self.diff_selected.clear();
        self.diff_comment_open = false;
        if let Some(state) = self.diff_comment_input.clone() {
            state.update(cx, |state, cx| state.set_value("", window, cx));
        }
        cx.notify();
    }

    /// 把选中的 diff 行 + 评论输入框内容拼成一段文本，写进当前激活终端的 PTY
    /// （不带回车，留给用户自己看一眼再发送）。
    pub(super) fn send_diff_comments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.diff_selected.is_empty() {
            return;
        }
        let Some(diff) = &self.git_diff else { return };
        let comment = self
            .diff_comment_input
            .as_ref()
            .map(|s| s.read(cx).value().trim().to_string())
            .unwrap_or_default();

        let mut selected: Vec<usize> = self.diff_selected.iter().copied().collect();
        selected.sort_unstable();
        let mut msg = format!("对 {} 的这几行有反馈：\n", diff.path);
        for i in selected {
            if let Some(l) = diff.lines.get(i) {
                let ln = l
                    .new_ln
                    .or(l.old_ln)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".into());
                let marker = match l.kind {
                    DiffKind::Add => "+",
                    DiffKind::Del => "-",
                    _ => " ",
                };
                msg.push_str(&format!("  L{ln} {marker} {}\n", l.text));
            }
        }
        if !comment.is_empty() {
            msg.push_str(&format!("\n{comment}\n"));
        }

        let target = self.cur().and_then(|s| s.active_term().cloned());
        if let Some(view) = target {
            view.update(cx, |tv, cx| tv.send_text(&msg, cx));
        }
        self.diff_selected.clear();
        self.diff_selection_anchor = None;
        self.diff_selection_cursor = None;
        self.diff_selection_dragging = false;
        self.diff_comment_open = false;
        if let Some(state) = self.diff_comment_input.clone() {
            state.update(cx, |s, cx| s.set_value("", window, cx));
        }
        cx.notify();
    }

    /// Git 页「提交」/「提交并推送」共用入口：直接执行（不再走"发到终端"那套）——
    /// 要提交的内容已经在暂存区里明明白白摆着（部分暂存那套勾选框），跟 stage/
    /// checkout 一样属于本地可控操作，不需要再让人去终端里确认一遍回车；真正没法
    /// 回头的风险点在 push 影响远程共享状态，但 WebStorm 等主流 git 客户端也是
    /// 「提交并推送」一键做的，这里跟随这个惯例。
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
        self.pushing = true;
        // 上一次的失败原因已经不再描述当前这次尝试。
        self.commit_errors.remove(&root);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (r, b) = (root.clone(), branch.clone());
            let result = cx
                .background_executor()
                .spawn(async move { push_current(&r, &b) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.pushing = false;
                match result {
                    Ok(()) => this.invalidate_git_status(&root),
                    Err(err) => {
                        this.commit_errors.insert(root.clone(), err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 后台跑一条 git 操作，回来刷新状态 / 报错。fetch·pull·stash 共用这个骨架
    /// （照 push_only）。`name` 是进行中显示的操作名（「拉取」等）；`op` 在后台
    /// 线程里执行，返回 `Result<(),String>`；`reload_log` = 操作会改历史（pull）
    /// 时顺带重拉 log；`silent` = 失败不弹顶部错误通知（自动 fetch 用，避免离线/
    /// 无凭据时反复刷屏）。
    fn run_git_op<F>(
        &mut self,
        name: &'static str,
        reload_log: bool,
        silent: bool,
        op: F,
        cx: &mut Context<Self>,
    ) where
        F: FnOnce(&str) -> Result<(), String> + Send + 'static,
    {
        // 一次只跑一个：避免连点 fetch/pull 起一堆并发 git 抢 index.lock。
        if self.git_op.is_some() {
            return;
        }
        let Some(root) = self.git_write_target(cx) else {
            return;
        };
        self.git_op = Some(name);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let result = cx.background_executor().spawn(async move { op(&r) }).await;
            let _ = this.update(cx, |this, cx| {
                this.git_op = None;
                match result {
                    Ok(()) => {
                        this.invalidate_git_status(&root);
                        if reload_log {
                            this.reload_git_log(root.clone(), cx);
                        }
                    }
                    // silent（自动 fetch）：失败不弹顶部错误通知，避免离线 / 无凭据时
                    // 每次进 Git 页刷屏；只有手动操作失败才提示。
                    Err(err) => {
                        if !silent {
                            this.background_error = Some(err);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `git fetch --all --prune`：更新远端追踪，刷新 ahead/behind。手动触发，失败弹错误。
    pub fn git_fetch(&mut self, cx: &mut Context<Self>) {
        self.run_git_op("获取", false, false, fetch_remote, cx);
    }

    /// 同 git_fetch，但失败静默：进 Git 页自动 fetch 用，离线 / 无凭据时不刷屏报错。
    pub fn git_fetch_silent(&mut self, cx: &mut Context<Self>) {
        self.run_git_op("获取", false, true, fetch_remote, cx);
    }

    /// `git pull --rebase`：拉取并 rebase 本地提交（历史变了 → 重拉 log）。
    pub fn git_pull(&mut self, cx: &mut Context<Self>) {
        self.run_git_op("拉取", true, false, pull_rebase, cx);
    }

    /// `git stash push -u`：储藏全部更改（含未跟踪）。
    ///
    /// 名字是「储藏」不是「暂存」：暂存已经归 staging area 了，两个概念共用一个
    /// 词，用户看到「暂存中…」根本分不清东西进了索引还是 stash 栈。
    pub fn git_stash_push(&mut self, cx: &mut Context<Self>) {
        self.run_git_op("储藏", false, false, stash_push, cx);
    }

    /// `git stash pop`：弹出最近一条 stash。
    pub fn git_stash_pop(&mut self, cx: &mut Context<Self>) {
        self.run_git_op("恢复储藏", false, false, stash_pop, cx);
    }

    /// 点「丢弃全部更改」：先弹确认，不直接动文件（照 start_discard_file）。
    pub fn start_discard_all(&mut self, root: String, cx: &mut Context<Self>) {
        self.discard_all_target = Some(root);
        cx.notify();
    }

    /// 取消丢弃全部。
    pub fn cancel_discard_all(&mut self, cx: &mut Context<Self>) {
        self.discard_all_target = None;
        cx.notify();
    }

    /// 确认丢弃工作区全部改动（restore + clean）。
    pub fn confirm_discard_all(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.discard_all_target.take() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let result = cx
                .background_executor()
                .spawn(async move { discard_all(&r) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.invalidate_git_status(&root);
                        // 丢弃后当前打开的 diff 大概率没了，关掉免得看空。
                        this.git_diff = None;
                        this.active_hunk = None;
                    }
                    Err(err) => this.background_error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn commit(&mut self, push: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.git_write_target(cx) else {
            return;
        };
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
        // 新的一次提交开始，旧错误先清——否则用户分不清面板上那条是这次的还是上次的。
        self.commit_errors.remove(&root);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let r = root.clone();
            let msg = message.clone();
            let b = branch.clone();
            let result = cx
                .background_executor()
                .spawn(async move { commit_and_maybe_push(&r, &msg, push, &b) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => {
                        this.invalidate_git_status(&root);
                        if let Some(input) = this.commit_msg_inputs.get(&root).cloned() {
                            input.update(cx, |s, cx| s.set_value("", window, cx));
                        }
                    }
                    Err(err) => {
                        this.commit_errors.insert(root.clone(), err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 全部改动：一份 diff 里所有文件一起展开或收起。
    pub fn toggle_all_aggregate_diff_files(&mut self, cx: &mut Context<Self>) {
        let Some(diff) = self.git_diff.as_ref() else {
            return;
        };
        if !diff.aggregate {
            return;
        }
        let paths = aggregate_file_header_paths(&diff.lines);
        toggle_aggregate_collapse_all(&paths, &mut self.git_collapsed_diff_files);
        self.git_collapsed_diff_files_gen = self.git_collapsed_diff_files_gen.wrapping_add(1);
        cx.notify();
    }
}

impl Workspace {
    /// diff 视图的派生数据（gutter 宽 / 内容宽 / 展开行）：diff 身份（diff_gen +
    /// root + path）、并排/统一、折叠状态任一变化才重算，否则每帧复用缓存——
    /// 大 diff 下每帧 O(n) 重算这些度量是「变更」页持续低帧率的主因。
    /// 返回 None 表示当前没有已打开的 diff。
    pub(crate) fn diff_derived_for_render(&mut self) -> Option<DiffDerivedCache> {
        // 命中路径只做 O(1) 的身份/版本比较（root/path 是很短的路径字符串）。
        let (root, path, split, diff_gen_v, collapsed_gen) = {
            let d = self.git_diff.as_ref()?;
            (
                d.root.clone(),
                d.path.clone(),
                self.diff_split,
                self.diff_gen,
                self.git_collapsed_diff_files_gen,
            )
        };
        if let Some(c) = &self.diff_derived
            && c.diff_gen == diff_gen_v
            && c.root == root
            && c.path == path
            && c.split == split
            && c.collapsed_gen == collapsed_gen
        {
            return Some(c.clone());
        }
        // 未命中才克隆 lines / 折叠集合重算。
        let (aggregate, lines, collapsed) = {
            let d = self.git_diff.as_ref().expect("git_diff 存在才进重算分支");
            (
                d.aggregate,
                d.lines.clone(),
                self.git_collapsed_diff_files.clone(),
            )
        };
        let gutter_w = gutter_width(&lines);
        let content_w = diff_content_width(&lines, gutter_w);
        let (rows, split_rows, sticky_headers) = if split {
            let split_rows = Rc::new(build_split_rows(&lines));
            let sticky_headers = Rc::new(if aggregate {
                aggregate_sticky_headers(&[], &split_rows, &lines, true)
            } else {
                Vec::new()
            });
            (Rc::new(Vec::new()), split_rows, sticky_headers)
        } else {
            let rows: Rc<Vec<DiffReviewRow>> = Rc::new(if aggregate {
                aggregate_diff_rows(&lines, &collapsed)
                    .into_iter()
                    .map(|row| match row {
                        AggregateDiffRow::Header { path, adds, dels } => {
                            DiffReviewRow::Header { path, adds, dels }
                        }
                        AggregateDiffRow::Line(index) => DiffReviewRow::Line(index),
                    })
                    .collect::<Vec<_>>()
            } else {
                (0..lines.len())
                    .map(DiffReviewRow::Line)
                    .collect::<Vec<_>>()
            });
            let sticky_headers = Rc::new(if aggregate {
                aggregate_sticky_headers(&rows, &[], &lines, false)
            } else {
                Vec::new()
            });
            (rows, Rc::new(Vec::new()), sticky_headers)
        };
        let cache = DiffDerivedCache {
            diff_gen: diff_gen_v,
            root,
            path,
            split,
            collapsed_gen,
            gutter_w,
            content_w,
            rows,
            split_rows,
            sticky_headers,
        };
        self.diff_derived = Some(cache.clone());
        Some(cache)
    }
}
