//! 文件树的数据：挂哪些根、目录列表、git 标记、监听、展开和定位、键盘导航、删除、搜索。
//!
//! 缓存都靠事件标旧（`project.changed`、重连、窗口回到前台、自己的写操作），视图露着时
//! [`FilesPanel::refresh`] 只重取被标旧的。

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::*;
use gpui_component::WindowExt as _;
use gpui_component::button::ButtonVariant;
use smelt_plugin_api::client::protocol::files::{
    ProjectDeleteEntryCall, ProjectDirParams, ProjectPathParams, ProjectRootParams,
    ProjectSearchCall, ProjectSearchParams, ProjectTreeCall, ProjectWatchCall,
};
use smelt_plugin_api::client::protocol::git::{GitRootParams, GitStatusCall};
use smelt_plugin_api::{self as api, PathRequest, RuntimeKey, sessions};

use crate::model::{
    DirCache, DirListing, FileTreeRow, GitMarks, SearchHit, SearchState, TreeRoots,
    collect_file_tree_rows,
};
use crate::rt::{RUNTIME_CALL_TIMEOUT, call_on, error_text, target};
use crate::{FilesPanel, ProjectRef, TreeActivate, TreeDown, TreeLeft, TreeRight, TreeUp};

impl FilesPanel {
    /// 树里挂出来的根，都在 [`Self::tree_runtime`] 那台机器上：当前项目在最前，后面是钉住
    /// 的本机目录（去空去重）。远程项目的树里不混进本机目录。
    pub(crate) fn tree_roots(&self) -> Vec<String> {
        let mut roots: Vec<String> = Vec::new();
        if let Some(project) = &self.project
            && !project.root.is_empty()
        {
            roots.push(project.root.clone());
        }
        if self
            .project
            .as_ref()
            .is_none_or(|project| project.runtime == RuntimeKey::Local)
        {
            for path in &self.pinned_roots {
                if !path.is_empty() && !roots.contains(path) {
                    roots.push(path.clone());
                }
            }
        }
        roots
    }

    /// 树里的一个目录的缓存 key。
    fn tree_dir(&self, dir: impl Into<String>) -> ProjectRef {
        ProjectRef {
            runtime: self.tree_runtime(),
            root: dir.into(),
        }
    }

    /// 当前可见树，下标和虚拟列表一致。只读内存。
    pub(crate) fn tree_rows(&self, cx: &App) -> Vec<FileTreeRow> {
        let runtime = self.tree_runtime();
        let roots = self.tree_roots();
        let empty = DirCache::new();
        collect_file_tree_rows(
            self.path_style(&runtime, cx),
            &TreeRoots {
                roots: &roots,
                pinned: &self.pinned_roots,
                collapsed: &self.collapsed_roots,
            },
            &self.expanded,
            self.dir_cache.get(&runtime).unwrap_or(&empty),
            &|root: &str| {
                self.git_marks
                    .get(&ProjectRef {
                        runtime: runtime.clone(),
                        root: root.to_string(),
                    })
                    .map(|cached| &cached.value)
            },
        )
    }

    // ===================== 监听和失效 =====================

    /// 请那台机器的运行时监听这个根，有变化就推 `project.changed`。
    ///
    /// 每个根只登记一次，失败了（连不上、旧运行时没有这个方法）也算登记过：那台机器重新
    /// 订阅上之前不再试，这段时间靠窗口回到前台兜底。
    fn ensure_watch(&mut self, root: ProjectRef, cx: &mut Context<Self>) {
        if !self.watches.insert(root.clone()) {
            return;
        }
        let Ok(client) = target(&root.runtime, cx) else {
            return;
        };
        cx.background_executor()
            .spawn(async move {
                let params = ProjectRootParams {
                    root: root.root.clone(),
                };
                if let Err(error) =
                    call_on::<ProjectWatchCall>(&client, params, RUNTIME_CALL_TIMEOUT)
                {
                    eprintln!(
                        "[files] 请运行时监听 {} 失败：{}",
                        root.root,
                        error_text(error)
                    );
                }
            })
            .detach();
    }

    /// 运行时报 `project.changed{root}`：这个根下的目录列表、git 标记标旧，打开着的文件
    /// 没改过就重读。监听是递归的，事件只带根。不是本插件登记的监听不理。
    pub(crate) fn on_project_changed(&mut self, project: ProjectRef, cx: &mut Context<Self>) {
        if !self.watches.contains(&project) {
            return;
        }
        let style = self.path_style(&project.runtime, cx);
        self.mark_dirs_stale_where(&|runtime, dir| {
            runtime == &project.runtime && style.starts_with(dir, &project.root)
        });
        self.on_open_file_project_changed(&project, cx);
        self.mark_git_marks_stale(&|key| {
            key.runtime == project.runtime && style.starts_with(&key.root, &project.root)
        });
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 某台机器重新订阅上了。它替我们建的监听可能随旧进程没了，断开期间的变化也收不到：
    /// 清掉登记记录，缓存全部标旧，下一次刷新重新登记、重取。
    pub(crate) fn on_runtime_resubscribed(&mut self, runtime: &RuntimeKey, cx: &mut Context<Self>) {
        self.mark_everything_stale(Some(runtime), cx);
        self.watches.retain(|root| &root.runtime != runtime);
        self.schedule_visible_refresh(cx);
    }

    /// 窗口回到前台。监听收不到的变化（inotify 用完、登记失败）在这里补上。
    pub(crate) fn on_window_refocused(&mut self, cx: &mut Context<Self>) {
        self.mark_everything_stale(None, cx);
        self.schedule_visible_refresh(cx);
    }

    /// 不知道哪里变了：`runtime` 那台机器（`None` 是所有机器）上的缓存全部标旧，打开着的
    /// 文件按登记过监听的根各查一次。
    fn mark_everything_stale(&mut self, runtime: Option<&RuntimeKey>, cx: &mut Context<Self>) {
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
        self.mark_dirs_stale_where(&|dir_runtime, _| runtime.is_none_or(|r| r == dir_runtime));
    }

    // ===================== git 标记 =====================

    /// 选中的根的 git 标记标旧，正在取的那次回来也不算新。
    fn mark_git_marks_stale(&mut self, matches: &dyn Fn(&ProjectRef) -> bool) {
        let roots: HashSet<ProjectRef> = self
            .git_marks
            .keys()
            .chain(self.git_inflight.iter())
            .filter(|root| matches(root))
            .cloned()
            .collect();
        for root in roots {
            let generation = self.git_generation.entry(root.clone()).or_default();
            *generation = generation.wrapping_add(1);
            if let Some(cached) = self.git_marks.get_mut(&root) {
                cached.stale = true;
            }
        }
    }

    /// 这个根的 git 标记还没有或者旧了，就去取。
    ///
    /// 读失败（不是 git 仓库、git 报错）不等于干净：留着上一次成功的标记，等下一个事件再
    /// 试，不自己重试。取的途中又被标旧，回来的这份先显示，再接着取一次。
    fn ensure_git_status(&mut self, root: ProjectRef, cx: &mut Context<Self>) {
        if self
            .git_marks
            .get(&root)
            .is_some_and(|cached| !cached.stale)
            || self.git_inflight.contains(&root)
        {
            return;
        }
        let Ok(client) = target(&root.runtime, cx) else {
            return;
        };
        let generation = self.git_generation.get(&root).copied().unwrap_or_default();
        self.git_inflight.insert(root.clone());
        cx.spawn(async move |this, cx| {
            let params = GitRootParams {
                root: root.root.clone(),
            };
            let data = cx
                .background_executor()
                .spawn(async move {
                    call_on::<GitStatusCall>(&client, params, RUNTIME_CALL_TIMEOUT)
                        .unwrap_or_default()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.git_inflight.remove(&root);
                let changed =
                    this.git_generation.get(&root).copied().unwrap_or_default() != generation;
                match this.git_marks.get_mut(&root) {
                    Some(cached) if !data.ok => cached.stale = changed,
                    _ => {
                        let marks = GitMarks {
                            value: data,
                            stale: changed,
                        };
                        this.git_marks.insert(root.clone(), marks);
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

    // ===================== 目录列表 =====================

    /// 某目录的直接子项不在缓存里或被标旧了，就交给运行时去列。render 只读缓存。
    fn ensure_dir_listing(&mut self, dir: ProjectRef, cx: &mut Context<Self>) {
        let current = self
            .dir_cache
            .get(&dir.runtime)
            .and_then(|cache| cache.get(&dir.root))
            .is_some_and(|listing| !listing.stale);
        if current || self.dir_inflight.contains(&dir) {
            return;
        }
        let Ok(client) = target(&dir.runtime, cx) else {
            return;
        };
        let request_generation = self.dir_generation.get(&dir).copied().unwrap_or(0);
        self.dir_inflight.insert(dir.clone());
        // 根之外的目录（面包屑点到项目外）以它自己为范围。
        let root = self
            .tree_root_for(&dir, cx)
            .unwrap_or_else(|| dir.root.clone());
        cx.spawn(async move |this, cx| {
            let listed = dir.root.clone();
            let entries = cx
                .background_executor()
                .spawn(async move {
                    // 排序与噪音目录过滤由运行时做，桌面和手机看到的是同一份规则。
                    call_on::<ProjectTreeCall>(
                        &client,
                        ProjectDirParams { root, dir: listed },
                        RUNTIME_CALL_TIMEOUT,
                    )
                    .map(|tree| {
                        tree.entries
                            .into_iter()
                            .map(|entry| (entry.name, entry.is_dir))
                            .collect::<Vec<_>>()
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.dir_inflight.remove(&dir);
                let current_generation = this.dir_generation.get(&dir).copied().unwrap_or(0);
                match entries {
                    // 列目录期间被标旧或删了：这一包是旧快照，不写回，再排一次刷新去读新的。
                    Ok(_) if current_generation != request_generation => {
                        this.schedule_visible_refresh(cx);
                    }
                    Ok(entries) => {
                        this.dir_cache.entry(dir.runtime).or_default().insert(
                            dir.root,
                            DirListing {
                                entries: Rc::new(entries),
                                stale: false,
                            },
                        );
                    }
                    // 连不上或目录没了：有旧列表就留着，没有就记一份空的，免得一直转「加载中」。
                    // 都算取过了，下一个事件（变化、重连、窗口回到前台）再试。
                    Err(error) => {
                        eprintln!("[files] 列目录 {} 失败：{}", dir.root, error_text(error));
                        this.dir_cache
                            .entry(dir.runtime)
                            .or_default()
                            .entry(dir.root)
                            .or_insert_with(|| DirListing {
                                entries: Rc::new(Vec::new()),
                                stale: false,
                            })
                            .stale = false;
                    }
                }
                this.try_flush_reveal(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 选中的目录列表标旧；正在读的那次回来也不算新。
    fn mark_dirs_stale_where(&mut self, matches: &dyn Fn(&RuntimeKey, &str) -> bool) {
        for (runtime, cache) in self.dir_cache.iter_mut() {
            for (dir, listing) in cache.iter_mut() {
                if matches(runtime, dir) {
                    listing.stale = true;
                }
            }
        }
        let inflight: Vec<ProjectRef> = self
            .dir_inflight
            .iter()
            .filter(|dir| matches(&dir.runtime, &dir.root))
            .cloned()
            .collect();
        for dir in inflight {
            self.bump_dir_generation(&dir);
        }
    }

    /// `dir` 归哪个根：取包含它的最长根。运行时按这个根限定范围。
    fn tree_root_for(&self, dir: &ProjectRef, cx: &App) -> Option<String> {
        if dir.runtime != self.tree_runtime() {
            return None;
        }
        let style = self.path_style(&dir.runtime, cx);
        self.tree_roots()
            .into_iter()
            .filter(|root| style.starts_with(&dir.root, root))
            .max_by_key(String::len)
    }

    fn bump_dir_generation(&mut self, dir: &ProjectRef) {
        let generation = self.dir_generation.entry(dir.clone()).or_default();
        *generation = generation.wrapping_add(1);
    }

    /// 视图露着时把挂出来的根补齐：监听、git 标记、目录列表，以及展开的目录。
    pub(crate) fn refresh_tree(&mut self, cx: &mut Context<Self>) {
        let runtime = self.tree_runtime();
        for root in self.tree_roots() {
            let root = ProjectRef {
                runtime: runtime.clone(),
                root,
            };
            // 目录列表靠运行时的变化通知失效，每个挂出来的根都要有监听。
            self.ensure_watch(root.clone(), cx);
            self.ensure_git_status(root.clone(), cx);
            self.ensure_dir_listing(root, cx);
        }
        for dir in self.expanded.clone() {
            self.ensure_dir_listing(self.tree_dir(dir), cx);
        }
        self.try_flush_reveal(cx);
    }

    // ===================== 展开、钉住、定位 =====================

    /// 展开 / 收起一个目录。
    pub(crate) fn toggle_expand(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            // 运行时的监听不报 target、node_modules 这类目录里的变化，展开时重取一次这一
            // 层；取回来之前先显示缓存。
            let runtime = self.tree_runtime();
            if let Some(listing) = self
                .dir_cache
                .get_mut(&runtime)
                .and_then(|cache| cache.get_mut(&path))
            {
                listing.stale = true;
            }
            self.expanded.insert(path);
        }
        self.persist_project();
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 收起 / 展开多根时的一个根。根默认展开，只有在 `collapsed_roots` 里的才收起。
    pub(crate) fn toggle_root_collapsed(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.collapsed_roots.remove(&path) {
            self.collapsed_roots.insert(path);
        }
        self.persist_project();
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 选本机目录钉进树里：切到别的本机项目也一直挂着。
    pub(crate) fn pick_pinned_roots(&mut self, cx: &mut Context<Self>) {
        let picked = api::PICK_PATHS.run(
            &PathRequest {
                machine: RuntimeKey::Local,
                title: "选择要加到文件树的目录".into(),
                confirm: "加入".into(),
                files: false,
                multiple: true,
            },
            cx,
        );
        cx.spawn(async move |panel, cx| {
            let paths = match picked.await {
                Ok(paths) => paths.unwrap_or_default(),
                Err(error) => {
                    cx.update(|cx| crate::notify_error(error, cx));
                    return;
                }
            };
            let _ = panel.update(cx, |this, cx| {
                for path in paths {
                    if !path.is_empty() && !this.pinned_roots.contains(&path) {
                        this.pinned_roots.push(path);
                    }
                }
                this.persist_pinned();
                this.schedule_visible_refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 根标题行菜单「从文件树移除」。
    pub(crate) fn unpin_root(&mut self, path: &str, cx: &mut Context<Self>) {
        self.pinned_roots.retain(|pinned| pinned != path);
        self.collapsed_roots.remove(path);
        self.persist_pinned();
        self.persist_project();
        cx.notify();
    }

    /// 在树里定位 path：展开所有祖先目录、选中，等祖先列出来后滚到那一行。不在任何根下的
    /// 只选中。
    pub(crate) fn reveal_in_tree(&mut self, path: &str, cx: &mut Context<Self>) {
        let runtime = self.tree_runtime();
        let style = self.path_style(&runtime, cx);
        if let Some(root) = self.tree_root_for(&self.tree_dir(path), cx) {
            // 自下而上收集祖先（不含文件自身和根）。
            let mut ancestors = Vec::new();
            let mut current = style.parent(path);
            while let Some(dir) = current {
                if dir.is_empty() || dir == root || !style.starts_with(&dir, &root) {
                    break;
                }
                current = style.parent(&dir);
                ancestors.push(dir);
            }
            for dir in ancestors.iter().rev() {
                self.expanded.insert(dir.clone());
                self.ensure_dir_listing(self.tree_dir(dir.clone()), cx);
            }
            // 多根时根可能收起着。
            self.collapsed_roots.remove(&root);
            self.ensure_dir_listing(self.tree_dir(root), cx);
        }
        self.selected = Some(path.to_string());
        self.pending_reveal = Some(path.to_string());
        self.try_flush_reveal(cx);
        self.persist_project();
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 点路径面包屑某一段：打开树并定位到它。点目录还会展开它，方便接着看子项。
    pub(crate) fn reveal_from_breadcrumb(
        &mut self,
        path: String,
        is_dir: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.tree_open {
            self.tree_open = true;
            self.persist_tree_layout();
        }
        if is_dir {
            self.expanded.insert(path.clone());
        }
        self.reveal_in_tree(&path, cx);
    }

    /// 祖先目录列齐了就把树滚到等着定位的那一行。
    pub(crate) fn try_flush_reveal(&mut self, cx: &App) {
        let Some(path) = self.pending_reveal.clone() else {
            return;
        };
        if let Some(ix) = self
            .tree_rows(cx)
            .iter()
            .position(|row| row.path() == Some(path.as_str()))
        {
            self.tree_scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            self.pending_reveal = None;
        }
    }

    // ===================== 键盘 =====================

    fn select_row(&mut self, rows: &[FileTreeRow], ix: usize, cx: &mut Context<Self>) {
        self.selected = rows[ix].path().map(str::to_string);
        self.tree_scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        self.persist_project();
        cx.notify();
    }

    fn selected_ix(&self, rows: &[FileTreeRow]) -> Option<usize> {
        let path = self.selected.as_deref()?;
        rows.iter().position(|row| row.path() == Some(path))
    }

    /// ↑↓ 移动选中，跳过没有路径的加载占位行。
    fn move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        let rows = self.tree_rows(cx);
        let selectable: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(ix, row)| row.path().map(|_| ix))
            .collect();
        if selectable.is_empty() {
            return;
        }
        let current = self
            .selected_ix(&rows)
            .and_then(|ix| selectable.iter().position(|&s| s == ix));
        let next = match current {
            Some(i) => (i as i32 + delta).clamp(0, selectable.len() as i32 - 1) as usize,
            None if delta >= 0 => 0,
            None => selectable.len() - 1,
        };
        self.select_row(&rows, selectable[next], cx);
    }

    pub(crate) fn on_tree_up(&mut self, _: &TreeUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, cx);
    }

    pub(crate) fn on_tree_down(&mut self, _: &TreeDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, cx);
    }

    /// ←：根或目录展开着就收起；否则选中上级（上级是根时选根标题行，单根时没有这一行）。
    pub(crate) fn on_tree_left(&mut self, _: &TreeLeft, _: &mut Window, cx: &mut Context<Self>) {
        let rows = self.tree_rows(cx);
        let Some(ix) = self.selected_ix(&rows) else {
            return;
        };
        let path = rows[ix].path().unwrap_or_default().to_string();
        match &rows[ix] {
            FileTreeRow::RootHeader { open: true, .. } => {
                self.toggle_root_collapsed(path, cx);
                return;
            }
            FileTreeRow::RootHeader { open: false, .. } => return,
            FileTreeRow::Entry { expanded: true, .. } => {
                self.toggle_expand(path, cx);
                return;
            }
            FileTreeRow::Entry { .. } | FileTreeRow::Loading { .. } => {}
        }
        let style = self.path_style(&self.tree_runtime(), cx);
        let Some(parent) = style.parent(&path) else {
            return;
        };
        if let Some(parent_ix) = rows
            .iter()
            .position(|row| row.path() == Some(parent.as_str()))
        {
            self.select_row(&rows, parent_ix, cx);
        }
    }

    /// →：收起的根或目录就展开；展开着的进到下一行；文件就打开。
    pub(crate) fn on_tree_right(
        &mut self,
        _: &TreeRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.tree_rows(cx);
        let Some(ix) = self.selected_ix(&rows) else {
            return;
        };
        let path = rows[ix].path().unwrap_or_default().to_string();
        match &rows[ix] {
            FileTreeRow::RootHeader { open: false, .. } => self.toggle_root_collapsed(path, cx),
            FileTreeRow::Entry {
                is_dir: true,
                expanded: false,
                ..
            } => self.toggle_expand(path, cx),
            FileTreeRow::Entry { is_dir: false, .. } => self.view_file(path, window, cx),
            FileTreeRow::RootHeader { open: true, .. } | FileTreeRow::Entry { .. } => {
                if rows.get(ix + 1).and_then(|row| row.path()).is_some() {
                    self.select_row(&rows, ix + 1, cx);
                }
            }
            FileTreeRow::Loading { .. } => {}
        }
    }

    /// Enter：根和目录切换展开；文件打开。
    pub(crate) fn on_tree_activate(
        &mut self,
        _: &TreeActivate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows = self.tree_rows(cx);
        let Some(ix) = self.selected_ix(&rows) else {
            return;
        };
        let path = rows[ix].path().unwrap_or_default().to_string();
        match &rows[ix] {
            FileTreeRow::RootHeader { .. } => self.toggle_root_collapsed(path, cx),
            FileTreeRow::Entry { is_dir: true, .. } => self.toggle_expand(path, cx),
            FileTreeRow::Entry { is_dir: false, .. } => self.view_file(path, window, cx),
            FileTreeRow::Loading { .. } => {}
        }
    }

    // ===================== 右键菜单 =====================

    /// 在系统文件管理器里显示（只对本机文件）。
    pub(crate) fn reveal_path_in_finder(path: &str) {
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open")
                .arg("-R")
                .arg(path)
                .spawn();
        }
        #[cfg(not(target_os = "macos"))]
        {
            // 其它系统打开所在目录。
            if let Some(parent) =
                smelt_plugin_api::model::runtime_path::PathStyle::LOCAL.parent(path)
            {
                let _ = std::process::Command::new(if cfg!(windows) {
                    "explorer"
                } else {
                    "xdg-open"
                })
                .arg(parent)
                .spawn();
            }
        }
    }

    /// 「发送到会话」：路径转成相对它所在根的 @提及，交给当前会话（不替用户发出）。
    pub(crate) fn send_path_to_session(&mut self, path: &str, cx: &mut Context<Self>) {
        let style = self.path_style(&self.tree_runtime(), cx);
        let rel = self
            .tree_root_for(&self.tree_dir(path), cx)
            .and_then(|root| style.strip_prefix(path, &root))
            .filter(|rel| !rel.is_empty())
            .unwrap_or_else(|| path.to_string());
        self.send_to_session(format!("@{rel} "), cx);
    }

    pub(crate) fn send_to_session(&self, text: String, cx: &mut App) {
        let sent = sessions::SEND_TEXT.run(&sessions::SendText { text }, cx);
        cx.spawn(async move |cx| {
            if let Err(error) = sent.await {
                cx.update(|cx| crate::notify_error(format!("发送到会话失败：{error}"), cx));
            }
        })
        .detach();
    }

    /// 「删除」：先确认，确定了才交给运行时删。
    pub(crate) fn start_delete(
        &mut self,
        path: String,
        is_dir: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let style = self.path_style(&self.tree_runtime(), cx);
        let label = style.file_name(&path).unwrap_or(&path).to_string();
        let (title, description) = if is_dir {
            (
                "确定删除这个文件夹吗？",
                format!("将永久删除「{label}」及其全部内容，此操作不可撤销。"),
            )
        } else {
            (
                "确定删除这个文件吗？",
                format!("将永久删除「{label}」，此操作不可撤销。"),
            )
        };
        let panel = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let ok_panel = panel.clone();
            let close_panel = panel.clone();
            let path = path.clone();
            alert
                .confirm()
                .title(title)
                .description(description.clone())
                .ok_text("确定删除")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    ok_panel.update(cx, |this, cx| this.perform_delete(path.clone(), is_dir, cx));
                    true
                })
                .on_close(move |_, _, cx| {
                    close_panel.update(cx, |this, cx| this.dialog_toggled(cx))
                })
        });
        self.dialog_toggled(cx);
    }

    fn perform_delete(&mut self, path: String, is_dir: bool, cx: &mut Context<Self>) {
        let entry = self.tree_dir(path.clone());
        let Some(root) = self
            .tree_root_for(&entry, cx)
            .or_else(|| self.path_style(&entry.runtime, cx).parent(&path))
        else {
            return;
        };
        let client = match target(&entry.runtime, cx) {
            Ok(client) => client,
            Err(error) => {
                self.report_background_error(format!("删除失败：{error}"), cx);
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let deleted = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    call_on::<ProjectDeleteEntryCall>(
                        &client,
                        ProjectPathParams {
                            root,
                            path: deleted,
                        },
                        RUNTIME_CALL_TIMEOUT,
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => this.forget_deleted_entry(entry, is_dir, cx),
                Err(error) => {
                    this.report_background_error(format!("删除失败：{}", error_text(error)), cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 删除成功后清掉指向它的打开文件、展开状态和目录缓存。
    fn forget_deleted_entry(&mut self, entry: ProjectRef, is_dir: bool, cx: &mut Context<Self>) {
        let ProjectRef {
            runtime,
            root: path,
        } = entry;
        let style = self.path_style(&runtime, cx);
        let under = |base: &str, candidate: &str| style.starts_with(candidate, base);

        if self
            .open_file
            .as_ref()
            .is_some_and(|of| of.runtime == runtime && under(&path, &of.path))
        {
            self.close_open_file();
        }
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| under(&path, selected))
        {
            self.selected = None;
        }

        let mut stale_dirs: Vec<String> = Vec::new();
        if is_dir {
            self.expanded.retain(|p| !under(&path, p));
            if let Some(cache) = self.dir_cache.get(&runtime) {
                stale_dirs.extend(
                    cache
                        .keys()
                        .filter(|candidate| under(&path, candidate))
                        .cloned(),
                );
            }
            stale_dirs.push(path.clone());
        }
        if let Some(parent) = style.parent(&path) {
            stale_dirs.push(parent);
        }
        stale_dirs.sort();
        stale_dirs.dedup();
        for dir in stale_dirs {
            let key = ProjectRef {
                runtime: runtime.clone(),
                root: dir,
            };
            self.bump_dir_generation(&key);
            if let Some(cache) = self.dir_cache.get_mut(&runtime) {
                cache.remove(&key.root);
            }
        }
        self.persist_project();
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    // ===================== 搜索 =====================

    /// 在项目里按文件名和内容搜。后台跑，换查询时停掉上一次；同一项目同一查询不重搜。
    pub(crate) fn ensure_search(
        &mut self,
        root: ProjectRef,
        query: String,
        cx: &mut Context<Self>,
    ) {
        if self
            .search
            .as_ref()
            .is_some_and(|search| search.root == root && search.query == query)
        {
            return;
        }
        self.cancel_file_search();
        let client = target(&root.runtime, cx);
        self.search_gen = self.search_gen.wrapping_add(1);
        let generation = self.search_gen;
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&cancel);
        self.search_cancel = Some(cancel);
        // 先占位：顶上显示「搜索中…」，跑完替换。
        self.search = Some(SearchState {
            root: root.clone(),
            query: query.clone(),
            done: false,
            hits: Vec::new(),
            truncated: false,
        });
        cx.notify();

        self.search_task = Some(cx.spawn(async move |this, cx| {
            let params = ProjectSearchParams {
                root: root.root.clone(),
                query: query.clone(),
            };
            let walked = cx
                .background_executor()
                .spawn(async move {
                    let client = client?;
                    call_on::<ProjectSearchCall>(&client, params, RUNTIME_CALL_TIMEOUT)
                        .map_err(error_text)
                })
                .await;
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            let walked = walked.unwrap_or_else(|error| {
                eprintln!("[files] 在 {} 里搜索失败：{error}", root.root);
                Default::default()
            });
            let hits = walked
                .hits
                .into_iter()
                .map(|hit| SearchHit {
                    path: hit.path,
                    rel: hit.rel,
                    line: hit.line,
                })
                .collect();
            let _ = this.update(cx, |this, cx| {
                if this.search_gen != generation {
                    return;
                }
                this.search = Some(SearchState {
                    root,
                    query,
                    done: true,
                    hits,
                    truncated: walked.truncated,
                });
                cx.notify();
            });
        }));
    }

    /// 停掉正在跑的搜索。清空输入、换查询、换项目都走这里。
    pub(crate) fn cancel_file_search(&mut self) {
        if let Some(cancel) = self.search_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.search_task.take();
    }
}
