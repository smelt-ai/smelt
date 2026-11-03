//! 文件树的数据修改：展开、打开、保存、搜索、删除。
//!
//! 页面渲染在 `view.rs`。字段仍由 main.rs 的 Workspace 持有。

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui_component::input::InputEvent;
use smelt_runtime_client::protocol::control::{ControlClientError, ControlErrorCode};
use smelt_runtime_client::protocol::files::{
    FileContent, FileOpenCall, FileOpenResult, FilePathParams, FileSaveCall, FileSaveParams,
    ProjectDeleteEntryCall, ProjectDirParams, ProjectPathParams, ProjectSearchCall,
    ProjectSearchParams, ProjectTreeCall,
};

use crate::runtime_call::{RUNTIME_CALL_TIMEOUT, call_on, error_text, target};
use crate::{ProjectRef, RuntimeKey, Workspace};
use smelt_model::runtime_path::PathStyle;

use super::view::{FileTreeRow, collect_file_tree_rows, editor_language_for_path};
use super::*;
use smelt_runtime_client::protocol::files::is_previewable_image;

impl OpenFile {
    /// 把预览正文交给 HTML 预览；不是 HTML 或没开预览时什么也不做。
    fn sync_html_preview(&self, cx: &mut App) {
        if let Some(html) = &self.html_preview {
            let source = self.preview_source.clone();
            html.update(cx, |preview, cx| preview.set_html(&source, cx));
        }
    }
}

// ===================== Workspace 方法 =====================

impl Workspace {
    /// 文件内容面板右上角源码 / 预览切换（只对 [`PreviewKind`] 认得的文件生效）。
    pub(super) fn set_file_preview(
        &mut self,
        preview: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(of) = self.open_file.as_mut() {
            of.preview = preview;
            of.html_preview = (preview
                && PreviewKind::for_path(&of.path) == Some(PreviewKind::Html))
            .then(|| cx.new(|cx| html_preview::HtmlPreview::new(window, cx)));
        }
        if preview {
            self.refresh_preview_source(cx);
        }
        cx.notify();
    }

    fn refresh_preview_source(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.open_file.as_mut() else {
            return;
        };
        if !open.preview {
            return;
        }
        let next = open.editor.read(cx).value().to_string();
        if open.preview_source.as_ref() != next {
            open.preview_source = next.into();
        }
        open.sync_html_preview(cx);
    }

    fn schedule_preview_refresh(&mut self, cx: &mut Context<Self>) {
        self.preview_epoch = self.preview_epoch.wrapping_add(1);
        let epoch = self.preview_epoch;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(80))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.preview_epoch != epoch {
                    return;
                }
                this.refresh_preview_source(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 文件树：展开/收起一个文件夹。
    pub fn toggle_expand(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            // 运行时的监听不报 target、node_modules 这类目录里的变化，展开时重取一次
            // 这一层；取回来之前先显示缓存。
            self.mark_dir_stale(&path, cx);
            self.expanded.insert(path);
        }
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 折叠/展开文件树里的一个项目根（多根工作区才有的顶层标题行）。根默认展开，只有
    /// 落进 `collapsed_roots` 的才收起，所以这里是「在集合里就移除、不在就加入」。
    /// 折叠偏好持久化（save_state），重启后还记得哪些根是收起的。
    pub fn toggle_root_collapsed(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.collapsed_roots.remove(&path) {
            self.collapsed_roots.insert(path);
        }
        self.save_state(cx);
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 把一个项目根 pin 进文件树 / 从文件树移除（toggle）。侧栏项目右键「加到文件树 /
    /// 从文件树移除」走这里。当前活动项目根本来就在文件树里（workspace_roots 第一位），
    /// pin 它的意义是「即使切到别的项目，它也一直留着」。改完立即持久化。
    pub fn toggle_file_tree_root(&mut self, cwd: String, cx: &mut Context<Self>) {
        if let Some(pos) = self.pinned_roots.iter().position(|p| p == &cwd) {
            self.pinned_roots.remove(pos);
        } else {
            self.pinned_roots.push(cwd);
        }
        self.save_state(cx);
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 某个 cwd 当前是否已 pin 在文件树里（侧栏右键菜单据此显示「加到」还是「移除」）。
    pub fn is_file_tree_root_pinned(&self, cwd: &str) -> bool {
        self.pinned_roots.iter().any(|p| p == cwd)
    }

    /// 文件树显示的是哪台机器上的目录：跟着活动项目走，没有项目时是本机。
    pub(crate) fn file_tree_runtime(&self, cx: &App) -> RuntimeKey {
        self.active_project_ref(cx)
            .map(|project| project.runtime)
            .unwrap_or(RuntimeKey::Local)
    }

    /// 文件树里的路径按哪种系统的规则切。
    pub(crate) fn file_tree_path_style(&self, cx: &App) -> PathStyle {
        crate::runtimes::path_style(&self.file_tree_runtime(cx), cx)
    }

    /// 文件树要同时挂出来的根目录集合，都在 [`Self::file_tree_runtime`] 那台机器上：
    /// 按项目分组聚合（顺序跟侧栏项目列表一致），去空去重。单项目时就一个根；多项目时
    /// 文件树把这些根一起铺开，右侧文件不用再切项目来回换。
    pub fn workspace_roots(&self, cx: &App) -> Vec<String> {
        let mut roots: Vec<String> = Vec::new();
        // 当前选中项目根永远在第一位。跟侧栏点选走，不跟当前会话 cwd，
        // 否则点空项目/切项目时文件树还停在旧会话上。
        let project = self.active_project_ref(cx);
        if let Some(cur) = project.as_ref().map(|project| project.root.clone())
            && !cur.is_empty()
        {
            roots.push(cur);
        }
        // 用户从「+ 项目」主动 pin 进来的额外根（去重、跳过已是当前项目的那个）。
        // 只有本机项目能钉，远程项目的文件树里不混进本机目录。
        if project.is_none_or(|project| project.runtime == RuntimeKey::Local) {
            for p in &self.pinned_roots {
                if !p.is_empty() && !roots.contains(p) {
                    roots.push(p.clone());
                }
            }
        }
        roots
    }

    /// 文件树里一个目录的缓存 key。
    fn file_tree_dir(&self, dir: impl Into<String>, cx: &App) -> ProjectRef {
        ProjectRef {
            runtime: self.file_tree_runtime(cx),
            root: dir.into(),
        }
    }

    /// 在文件树中定位 path：展开所有祖先目录、选中、并排队滚动到该行。
    pub fn reveal_in_file_tree(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(project) = self.active_project_ref(cx) else {
            return;
        };
        let root = project.root.clone();
        // 自下而上展开祖先（不含文件自身）。路径按项目所在机器的规则切。
        let style = crate::runtimes::path_style(&project.runtime, cx);
        let mut ancestors = Vec::new();
        let mut current = style.parent(path);
        while let Some(ps) = current {
            if ps.is_empty() || ps == root {
                break;
            }
            if !style.starts_with(&ps, &root) {
                break;
            }
            current = style.parent(&ps);
            ancestors.push(ps);
        }
        // 先 ensure 近根的，再展开
        for dir in ancestors.iter().rev() {
            self.expanded.insert(dir.clone());
            self.ensure_dir_listing(project.at(dir.clone()), cx);
        }
        self.ensure_dir_listing(project, cx);
        self.file_tree_selected = Some(path.to_string());
        self.file_tree_pending_reveal = Some(path.to_string());
        self.try_flush_file_tree_reveal(cx);
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 点击路径面包屑某一段：打开文件树并定位到该目录/文件。
    /// 点目录还会展开它，方便接着看子项。
    pub fn reveal_from_breadcrumb(&mut self, path: String, is_dir: bool, cx: &mut Context<Self>) {
        if !self.file_tree_open {
            self.file_tree_open = true;
            self.save_state(cx);
        }
        self.reveal_in_file_tree(&path, cx);
        if is_dir {
            self.expanded.insert(path.clone());
            let dir = self.file_tree_dir(path, cx);
            self.ensure_dir_listing(dir, cx);
        }
    }

    /// 祖先目录缓存齐了就把树滚到 pending reveal 那一行。
    pub fn try_flush_file_tree_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.file_tree_pending_reveal.clone() else {
            return;
        };
        let rows = self.file_tree_rows(cx);
        if let Some(ix) = rows
            .iter()
            .position(|row| row.path() == Some(path.as_str()))
        {
            self.file_tree_scroll
                .scroll_to_item(ix, ScrollStrategy::Nearest);
            self.file_tree_pending_reveal = None;
        }
        let _ = cx;
    }

    /// 当前可见树，下标和虚拟列表一致。
    fn file_tree_rows(&self, cx: &App) -> Vec<super::view::FileTreeRow> {
        let runtime = self.file_tree_runtime(cx);
        let empty = DirCache::new();
        collect_file_tree_rows(
            crate::runtimes::path_style(&runtime, cx),
            &self.workspace_roots(cx),
            &self.expanded,
            &self.collapsed_roots,
            self.dir_cache.get(&runtime).unwrap_or(&empty),
            &|root: &str| {
                self.git_status
                    .get(&ProjectRef {
                        runtime: runtime.clone(),
                        root: root.to_string(),
                    })
                    .map(|(_, status)| status)
            },
        )
    }

    /// ↑↓ 移动键盘选中。跳过没有路径的加载占位行。
    pub fn file_tree_move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        let rows = self.file_tree_rows(cx);
        let selectable: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(ix, row)| row.path().map(|_| ix))
            .collect();
        if selectable.is_empty() {
            return;
        }
        let cur = self.file_tree_selected.as_ref().and_then(|path| {
            selectable
                .iter()
                .position(|&ix| rows[ix].path() == Some(path.as_str()))
        });
        let next_sel = match cur {
            Some(i) => (i as i32 + delta).clamp(0, selectable.len() as i32 - 1) as usize,
            None => {
                if delta >= 0 {
                    0
                } else {
                    selectable.len() - 1
                }
            }
        };
        let ix = selectable[next_sel];
        self.file_tree_selected = rows[ix].path().map(str::to_string);
        self.file_tree_scroll
            .scroll_to_item(ix, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// ←：目录已展开则收起；否则选中父目录。根标题行走 collapsed_roots，不进 expanded。
    pub fn file_tree_key_left(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.file_tree_selected.clone() else {
            return;
        };
        if matches!(
            self.file_tree_rows(cx)
                .iter()
                .find(|row| row.path() == Some(path.as_str())),
            Some(FileTreeRow::RootHeader { .. })
        ) {
            if !self.collapsed_roots.contains(&path) {
                self.toggle_root_collapsed(path, cx);
            }
            return;
        }
        if self.expanded.contains(&path) {
            self.expanded.remove(&path);
            cx.notify();
            return;
        }
        let Some(root) = self.active_project_ref(cx).map(|project| project.root) else {
            return;
        };
        let style = self.file_tree_path_style(cx);
        if let Some(ps) = style.parent(&path) {
            // 父目录在项目根之下（含根的直接子项的父 = root）
            if ps == root || style.starts_with(&ps, &root) {
                // 根本身不在列表里时，选中它没有对应行；只选非根父路径
                if ps != root {
                    self.file_tree_selected = Some(ps);
                    self.try_flush_file_tree_reveal_path(cx);
                    cx.notify();
                }
            }
        }
    }

    /// →：目录未展开则展开；已展开则进下一个有路径的行；文件则打开。
    pub fn file_tree_key_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.file_tree_selected.clone() else {
            return;
        };
        let rows = self.file_tree_rows(cx);
        let Some(ix) = rows
            .iter()
            .position(|row| row.path() == Some(path.as_str()))
        else {
            return;
        };
        match &rows[ix] {
            FileTreeRow::RootHeader { open: false, .. } => {
                self.toggle_root_collapsed(path, cx);
            }
            FileTreeRow::Entry { is_dir: true, .. } if !self.expanded.contains(&path) => {
                self.toggle_expand(path, cx);
            }
            FileTreeRow::Entry { is_dir: false, .. } => self.view_file(path, window, cx),
            FileTreeRow::RootHeader { open: true, .. } | FileTreeRow::Entry { .. } => {
                if let Some(child) = rows.get(ix + 1).and_then(|row| row.path()) {
                    self.file_tree_selected = Some(child.to_string());
                    self.file_tree_scroll
                        .scroll_to_item(ix + 1, ScrollStrategy::Nearest);
                    cx.notify();
                }
            }
            FileTreeRow::Loading { .. } => {}
        }
    }

    /// Enter：目录切换展开；文件打开。根标题行折叠/展开。
    pub fn file_tree_key_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.file_tree_selected.clone() else {
            return;
        };
        let rows = self.file_tree_rows(cx);
        let Some(row) = rows.iter().find(|row| row.path() == Some(path.as_str())) else {
            return;
        };
        match row {
            FileTreeRow::RootHeader { .. } => self.toggle_root_collapsed(path, cx),
            FileTreeRow::Entry { is_dir: true, .. } => self.toggle_expand(path, cx),
            FileTreeRow::Entry { is_dir: false, .. } => self.view_file(path, window, cx),
            FileTreeRow::Loading { .. } => {}
        }
    }

    fn try_flush_file_tree_reveal_path(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.file_tree_selected.clone() {
            let rows = self.file_tree_rows(cx);
            if let Some(ix) = rows
                .iter()
                .position(|row| row.path() == Some(path.as_str()))
            {
                self.file_tree_scroll
                    .scroll_to_item(ix, ScrollStrategy::Nearest);
            }
        }
    }

    /// HTML 文件在系统默认浏览器里打开。开的是磁盘上的文件，相对路径的资源能加载；
    /// 编辑器里没保存的改动不会带过去。
    pub(super) fn open_file_in_browser(&mut self, cx: &mut Context<Self>) {
        let Some(of) = &self.open_file else {
            return;
        };
        // 浏览器只能打开这台电脑上的文件。
        if of.runtime != RuntimeKey::Local {
            return;
        }
        match url::Url::from_file_path(&of.path) {
            Ok(url) => cx.open_url(url.as_str()),
            Err(()) => {
                if let Some(of) = self.open_file.as_mut() {
                    of.save_error = Some("这个路径转不成文件地址，打不开浏览器".to_string());
                }
                cx.notify();
            }
        }
    }

    /// 右键：在系统文件管理器中显示。
    pub fn reveal_path_in_finder(&mut self, path: String, _cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        {
            let _ = std::process::Command::new("open")
                .arg("-R")
                .arg(&path)
                .spawn();
        }
        #[cfg(not(target_os = "macos"))]
        {
            // Linux：尽量打开所在目录。路径按运行时的 `/` 取父目录。
            if let Some(parent) = smelt_model::runtime_path::PathStyle::LOCAL.parent(&path) {
                let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
            }
        }
    }

    /// 文件树：打开一个文件查看/编辑内容。当前文件有未保存改动时不直接切换——先弹
    /// 确认弹窗（见 pending_file_switch / render_unsaved_file_confirm），用户选了
    /// "不保存"或"保存并切换"才真正调用 open_file_now。
    /// 文件在文件树所在的那台机器上。
    pub fn view_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.view_file_at(path, None, window, cx);
    }

    /// 打开文件树那台机器上的文件，并可跳到指定行（1 基，搜索命中用）。
    pub fn view_file_at(
        &mut self,
        path: String,
        line: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let file = FileRef {
            runtime: self.file_tree_runtime(cx),
            path,
        };
        self.view_file_on(file, line, window, cx);
    }

    /// 打开某台机器上的文件（对话里的文件链接可能指向别的机器）。
    pub fn view_file_on(
        &mut self,
        file: FileRef,
        line: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dirty = self.open_file.as_ref().is_some_and(|of| {
            of.readable && of.editor.read(cx).value().as_ref() != of.saved_content.as_str()
        });
        if dirty {
            // 脏切换暂不带行号（确认后再 open 整文件即可）
            self.begin_main_modal_focus(window, cx);
            self.pending_file_switch = Some(file);
            cx.notify();
            return;
        }
        self.open_file_now(file, line, window, cx);
    }

    /// 实际打开文件：用 gpui-component 的 Editor（InputState 的 code_editor 模式）：
    /// tree-sitter 语法高亮 + 行号 + 搜索，直接可编辑，Cmd+S（见 save_open_file）能
    /// 存回磁盘。读文件本身放到后台线程跑（大文件不卡 UI），读完回主线程灌进编辑器；
    /// 用自增 file_gen 丢弃过期结果（期间又切了别的文件）。
    /// `goto_line`：1 基行号，读完后定位光标。
    pub fn open_file_now(
        &mut self,
        file: FileRef,
        goto_line: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::input::{EditorState, Position};

        let FileRef { runtime, path } = file;
        // 文件树只显示活动项目那台机器的目录，别的机器上的文件定位不到。
        if runtime == self.file_tree_runtime(cx) {
            self.reveal_in_file_tree(&path, cx);
        }
        // 点文件不再抢占舞台：Tool Panel 的 FILES tab 自己就能分
        // 左右两栏显示内容 + 树，中间舞台的终端/ACP 对话完全不受影响。只有
        // 已经把 Files 提升到舞台全宽（双栏）时，才保持那个双栏视图跟着切换
        // 显示的文件（不触碰 stage_cover，本来就是 Some(Files) 不用改）。
        //
        // 停靠面板默认 344px 分给树 + 内容太挤，第一次在这个面板里开文件时
        // 顺手把面板拉宽一些（参考 Codex App 点文件自动展开面板），后续用户
        // 拖宽/拖窄的手动结果不再覆盖。
        if self.tool_panel_open
            && matches!(self.tool_panel_tab, crate::tool_panel::ToolPanelTab::Files)
            && !self.tool_panel_promoted()
            && self.tool_panel_w < 640.
        {
            self.tool_panel_w = 640.;
        }

        self.file_gen = self.file_gen.wrapping_add(1);
        let r#gen = self.file_gen;

        let language = editor_language_for_path(&path);
        let preview_kind = PreviewKind::for_path(&path);
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .searchable(true)
                // 超长行横向滚动而不是自动换行——代码这种东西换行会破坏缩进对齐，
                // 多行输入默认开软换行，这里显式关掉。
                .soft_wrap(false)
        });
        let style = crate::runtimes::path_style(&runtime, cx);
        let client = target(&runtime, cx);
        self.open_file = Some(OpenFile {
            runtime,
            path: path.clone(),
            editor: editor.clone(),
            saved_content: Rc::new(String::new()),
            saved_version: None,
            image: None,
            save_error: None,
            readable: false, // 读完确认是文本才翻真，防止读取完成前误按 Cmd+S
            conflict_pending: false,
            reload_inflight: false,
            reload_again: false,
            preview: preview_kind.is_some(),
            preview_source: SharedString::default(),
            html_preview: (preview_kind == Some(PreviewKind::Html))
                .then(|| cx.new(|cx| html_preview::HtmlPreview::new(window, cx))),
        });
        // 编辑器自己会刷新。预览和未保存标记属于 Workspace：按键先记下世代，
        // 停手后再把正文交给 Markdown，避免每个字符都重解析。
        self._file_editor_sub = Some(cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.schedule_preview_refresh(cx);
            }
        }));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let read = cx
                .background_executor()
                .spawn(async move {
                    let client = client?;
                    call_on::<FileOpenCall>(
                        &client,
                        FilePathParams { path: p },
                        RUNTIME_CALL_TIMEOUT,
                    )
                    .map_err(error_text)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                // 只有当前仍是这次打开的文件才写入，避免旧任务覆盖新文件。
                if this.file_gen != r#gen {
                    return;
                }
                let Some(of) = this.open_file.as_mut() else {
                    return;
                };
                let unreadable = match read {
                    Ok(FileOpenResult {
                        content:
                            FileContent::Text {
                                text: content,
                                version,
                            },
                    }) => {
                        editor.update(cx, |state, cx| {
                            state.set_value(content.clone(), window, cx);
                            if let Some(line) = goto_line {
                                // 搜索命中是 1 基；Position 是 0 基。
                                let line0 = line.saturating_sub(1) as u32;
                                state.set_cursor_position(Position::new(line0, 0), window, cx);
                            }
                        });
                        of.saved_content = Rc::new(content.clone());
                        of.saved_version = Some(version);
                        of.preview_source = content.into();
                        of.sync_html_preview(cx);
                        of.readable = true;
                        None
                    }
                    Ok(FileOpenResult {
                        content: FileContent::Image { data },
                    }) => match style
                        .extension(&of.path)
                        .and_then(smelt_ui::image::format_for_extension)
                    {
                        Some(format) => {
                            of.image = Some(Arc::new(gpui::Image::from_bytes(format, data.0)));
                            None
                        }
                        None => Some("（无法预览这种图片格式）".to_string()),
                    },
                    Ok(FileOpenResult {
                        content: FileContent::Binary,
                    }) => Some("（无法以文本方式读取：可能是二进制文件）".to_string()),
                    Ok(FileOpenResult {
                        content: FileContent::TooLarge { size, limit },
                    }) => Some(format!(
                        "（文件过大：{:.1} MB，查看上限 {} MB）",
                        size as f64 / (1024.0 * 1024.0),
                        limit / (1024 * 1024)
                    )),
                    Err(error) => Some(format!("（无法读取：{error}）")),
                };
                if let Some(placeholder) = unreadable {
                    editor.update(cx, |state, cx| state.set_value(placeholder, window, cx));
                    of.readable = false;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Cmd+S：把当前打开文件的编辑器内容交给运行时写回（仅 Files 页触发，见 SaveFile）。
    /// 带上打开时的版本：运行时发现磁盘内容已经不是那一版（文件被外部改过），就不写、
    /// 回冲突，这里把 conflict_pending 置位提示用户；用户再按一次 Cmd+S 就当作已确认
    /// 覆盖，不带版本直接写。写文件本身放后台线程；成功后把
    /// saved_content 同步成刚写的内容（清掉"未保存"标记 + 错误提示），并且如果这
    /// 次保存是「保存并切换」触发的，顺带打开 pending_switch_after_save 里存的目标
    /// 文件；保存失败或起冲突则放弃这次切换，留在当前文件上让用户处理。
    pub fn save_open_file(&mut self, cx: &mut Context<Self>) {
        let Some(of) = &self.open_file else { return };
        if is_previewable_image(&of.path) {
            return;
        }
        if !of.readable {
            if let Some(of) = self.open_file.as_mut() {
                of.save_error = Some("此文件未能正常读取为文本，不支持保存".to_string());
            }
            self.pending_switch_after_save = None;
            cx.notify();
            return;
        }
        let path = of.path.clone();
        let client = target(&of.runtime, cx);
        let content = of.editor.read(cx).value().to_string();
        let force = of.conflict_pending;
        let base_version = if force {
            None
        } else {
            of.saved_version.clone()
        };
        let r#gen = self.file_gen;

        cx.spawn(async move |this, cx| {
            let check_path = path.clone();
            let write_content = content.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let client = match client {
                        Ok(client) => client,
                        Err(error) => return SaveOutcome::Error(error),
                    };
                    let params = FileSaveParams {
                        path: check_path,
                        text: write_content,
                        base_version,
                    };
                    match call_on::<FileSaveCall>(&client, params, RUNTIME_CALL_TIMEOUT) {
                        Ok(saved) => SaveOutcome::Saved(saved.version),
                        Err(ControlClientError::Protocol(error))
                            if error.code == ControlErrorCode::Conflict =>
                        {
                            SaveOutcome::Conflict
                        }
                        Err(error) => SaveOutcome::Error(error_text(error)),
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.file_gen != r#gen {
                    return; // 写盘期间又切了别的文件，这次结果不再相关
                }
                let switch_target = this.pending_switch_after_save.take();
                let Some(of) = this.open_file.as_mut() else {
                    return;
                };
                match outcome {
                    SaveOutcome::Saved(version) => {
                        of.saved_content = Rc::new(content);
                        of.saved_version = Some(version);
                        of.save_error = None;
                        of.conflict_pending = false;
                        if let Some(target) = switch_target {
                            this.open_file_now(target, None, window, cx);
                        }
                    }
                    SaveOutcome::Conflict => {
                        of.conflict_pending = true;
                        of.save_error = Some(
                            "文件已被外部修改；再按一次 Cmd+S 会强制覆盖磁盘上的改动".to_string(),
                        );
                    }
                    SaveOutcome::Error(e) => of.save_error = Some(format!("保存失败：{e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 运行时报项目里有文件变了。打开着的文件在这个项目里就重读一次：没改过就换成新内容，
    /// 有未保存的改动就提示，保存时照常走冲突检测。事件只带项目根，分不出是不是这个文件，
    /// 内容版本没变时什么也不动。
    pub(crate) fn on_open_file_project_changed(
        &mut self,
        project: &ProjectRef,
        cx: &mut Context<Self>,
    ) {
        let style = crate::runtimes::path_style(&project.runtime, cx);
        let Some(of) = self.open_file.as_mut() else {
            return;
        };
        if of.runtime != project.runtime
            || !of.readable
            || !style.starts_with(&of.path, &project.root)
        {
            return;
        }
        if of.reload_inflight {
            of.reload_again = true;
            return;
        }
        self.reload_open_file(cx);
    }

    fn reload_open_file(&mut self, cx: &mut Context<Self>) {
        let Some(of) = self.open_file.as_mut() else {
            return;
        };
        let Ok(client) = target(&of.runtime, cx) else {
            return;
        };
        of.reload_inflight = true;
        of.reload_again = false;
        let path = of.path.clone();
        let r#gen = self.file_gen;
        let handle = self.window_handle;
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move {
                    call_on::<FileOpenCall>(&client, FilePathParams { path }, RUNTIME_CALL_TIMEOUT)
                })
                .await;
            let _ = cx.update_window(handle, |_root, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    if this.file_gen != r#gen {
                        return;
                    }
                    let Some(of) = this.open_file.as_mut() else {
                        return;
                    };
                    of.reload_inflight = false;
                    let again = std::mem::take(&mut of.reload_again);
                    if let Ok(FileOpenResult {
                        content: FileContent::Text { text, version },
                    }) = read
                        && of.saved_version.as_deref() != Some(version.as_str())
                    {
                        let dirty =
                            of.editor.read(cx).value().as_ref() != of.saved_content.as_str();
                        if dirty {
                            of.save_error =
                                Some("这个文件已在别处被改过；保存时会提示冲突".to_string());
                        } else {
                            of.editor
                                .update(cx, |state, cx| state.set_value(text.clone(), window, cx));
                            of.saved_content = Rc::new(text.clone());
                            of.saved_version = Some(version);
                            of.preview_source = text.into();
                            of.sync_html_preview(cx);
                        }
                        cx.notify();
                    }
                    if again {
                        this.reload_open_file(cx);
                    }
                });
            });
        })
        .detach();
    }

    /// 文件树搜索：按 query 匹配文件名 + 文件内容，后台遍历项目、命中写回 search_results。
    /// 与 view_file 同款「background_executor + 自增 gen 丢弃过期结果」模式，绝不阻塞 render。
    /// query 未变（已有对应结果或正在跑同一 query）就跳过，避免每帧重扫。
    ///
    /// 换查询时替换 `search_task`，并在遍历的每个文件边界看 `search_cancel`。
    /// 只递增代次会让旧扫描继续占着 worker。
    pub fn ensure_search(&mut self, root: ProjectRef, query: String, cx: &mut Context<Self>) {
        // 已有本 query 的结果、或正有一次针对本 query 的遍历在跑，就不重复触发。
        if self
            .search_results
            .as_ref()
            .is_some_and(|s| s.query == query)
        {
            return;
        }
        self.cancel_file_search();
        let client = target(&root.runtime, cx);
        self.search_gen = self.search_gen.wrapping_add(1);
        let generation = self.search_gen;
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_apply = Arc::clone(&cancel);
        self.search_cancel = Some(cancel);
        // 先占位：done=false 让列表顶部显示「搜索中…」，遍历完成后替换。
        self.search_results = Some(SearchState {
            query: query.clone(),
            done: false,
            hits: Vec::new(),
            truncated: false,
        });
        cx.notify();

        self.search_task = Some(cx.spawn(async move |this, cx| {
            let (r, q) = (root.root, query.clone());
            let walked = cx
                .background_executor()
                .spawn(async move {
                    let Ok(client) = client else {
                        return Default::default();
                    };
                    call_on::<ProjectSearchCall>(
                        &client,
                        ProjectSearchParams { root: r, query: q },
                        RUNTIME_CALL_TIMEOUT,
                    )
                    .unwrap_or_default()
                })
                .await;
            if cancel_for_apply.load(Ordering::Acquire) {
                return;
            }
            let truncated = walked.truncated;
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
                // 只有仍是最新一次搜索才写入，丢弃期间被新查询取代的过期结果。
                if this.search_gen != generation {
                    return;
                }
                this.search_results = Some(SearchState {
                    query,
                    done: true,
                    hits,
                    truncated,
                });
                cx.notify();
            });
        }));
    }

    /// 停掉正在跑的文件搜索。清空输入和换查询都走这里。
    pub(crate) fn cancel_file_search(&mut self) {
        if let Some(cancel) = self.search_cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        self.search_task.take();
    }

    /// 确保某目录的直接子项列表在缓存里且没被标旧，否则后台重取。
    /// 绝不阻塞 render：读目录交给运行时，render 只读缓存。
    pub fn ensure_dir_listing(&mut self, dir: ProjectRef, cx: &mut Context<Self>) {
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
            .file_tree_root_for(&dir, cx)
            .unwrap_or_else(|| dir.root.clone());
        cx.spawn(async move |this, cx| {
            let d = dir.root.clone();
            let entries = cx
                .background_executor()
                .spawn(async move {
                    // 排序与噪音目录过滤由运行时做，桌面和手机看到的是同一份规则。
                    call_on::<ProjectTreeCall>(
                        &client,
                        ProjectDirParams { root, dir: d },
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
                    // 删除发生在列目录期间时，这一包是旧快照。不写回，再排一次刷新去读新代次。
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
                    // 连不上或目录没了：有旧列表就留着（仍标旧，下次刷新再试），
                    // 没有就记一份空的，免得一直转「加载中」。
                    Err(error) => {
                        eprintln!("[files] 列目录 {} 失败：{}", dir.root, error_text(error));
                        this.dir_cache
                            .entry(dir.runtime)
                            .or_default()
                            .entry(dir.root)
                            .or_insert_with(|| DirListing {
                                entries: Rc::new(Vec::new()),
                                stale: false,
                            });
                    }
                }
                this.try_flush_file_tree_reveal(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 标旧一个目录的列表，下一次可见数据刷新重取。
    fn mark_dir_stale(&mut self, dir: &str, cx: &App) {
        let runtime = self.file_tree_runtime(cx);
        if let Some(listing) = self
            .dir_cache
            .get_mut(&runtime)
            .and_then(|cache| cache.get_mut(dir))
        {
            listing.stale = true;
        }
    }

    /// 运行时说项目里有文件变了：把这台机器上这个项目下的目录列表都标旧。事件只带项目
    /// 根，分不出是哪个目录。只有界面上看得到的目录会在下一次刷新时重取。
    pub(crate) fn mark_project_dirs_stale(&mut self, project: &ProjectRef, cx: &App) {
        let style = crate::runtimes::path_style(&project.runtime, cx);
        let Some(cache) = self.dir_cache.get_mut(&project.runtime) else {
            return;
        };
        for (dir, listing) in cache.iter_mut() {
            if style.starts_with(dir, &project.root) {
                listing.stale = true;
            }
        }
    }

    /// `dir` 归哪个文件树根：取包含它的最长根。运行时按这个根限定范围。
    fn file_tree_root_for(&self, dir: &ProjectRef, cx: &App) -> Option<String> {
        if dir.runtime != self.file_tree_runtime(cx) {
            return None;
        }
        let style = crate::runtimes::path_style(&dir.runtime, cx);
        self.workspace_roots(cx)
            .into_iter()
            .filter(|root| style.starts_with(&dir.root, root))
            .max_by_key(String::len)
    }

    /// 目录列表失效。在途的那一次读完后不得再把旧条目标成新鲜。
    fn bump_dir_generation(&mut self, dir: &ProjectRef) {
        let generation = self.dir_generation.entry(dir.clone()).or_default();
        *generation = generation.wrapping_add(1);
    }

    /// 文件树右键「复制文件路径」：把绝对路径写入系统剪贴板。
    pub fn copy_file_path_to_clipboard(&mut self, path: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(path));
    }

    /// 文件树右键「删除文件」：先弹二次确认，用户点确定后才真正删盘。
    pub fn start_delete_file(&mut self, path: String, is_dir: bool, cx: &mut Context<Self>) {
        let label = self
            .file_tree_path_style(cx)
            .file_name(&path)
            .unwrap_or(&path)
            .to_string();
        self.delete_file_target = Some(DeleteFileTarget {
            path,
            is_dir,
            label,
        });
        cx.notify();
    }

    /// 确认删除文件/文件夹。
    pub fn confirm_delete_file(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.delete_file_target.take() else {
            return;
        };
        cx.notify();
        self.perform_delete_file(target.path, target.is_dir, cx);
    }

    /// 取消删除。
    pub fn cancel_delete_file(&mut self, cx: &mut Context<Self>) {
        self.delete_file_target = None;
        cx.notify();
    }

    /// 交给运行时删除文件或目录，成功后刷新文件树缓存。
    fn perform_delete_file(&mut self, path: String, is_dir: bool, cx: &mut Context<Self>) {
        let entry = self.file_tree_dir(path.clone(), cx);
        let Some(root) = self
            .file_tree_root_for(&entry, cx)
            .or_else(|| self.file_tree_path_style(cx).parent(&path))
        else {
            return;
        };
        let client = match target(&entry.runtime, cx) {
            Ok(client) => client,
            Err(error) => {
                self.report_background_error(format!("删除失败：{error}"));
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let target = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    call_on::<ProjectDeleteEntryCall>(
                        &client,
                        ProjectPathParams { root, path: target },
                        RUNTIME_CALL_TIMEOUT,
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(_) => this.forget_deleted_entry(path, is_dir, cx),
                Err(error) => {
                    this.report_background_error(format!("删除失败：{}", error_text(error)));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 删除成功后清掉指向它的打开文件、展开状态和目录缓存。
    fn forget_deleted_entry(&mut self, path: String, is_dir: bool, cx: &mut Context<Self>) {
        let runtime = self.file_tree_runtime(cx);
        let style = crate::runtimes::path_style(&runtime, cx);
        let under = |base: &str, candidate: &str| style.starts_with(candidate, base);

        if self
            .open_file
            .as_ref()
            .is_some_and(|of| of.runtime == runtime && under(&path, &of.path))
        {
            self.open_file = None;
        }
        if self
            .pending_file_switch
            .as_ref()
            .is_some_and(|p| p.runtime == runtime && under(&path, &p.path))
        {
            self.pending_file_switch = None;
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
        } else {
            self.expanded.remove(&path);
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
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 文件树右键「发送到会话」：把路径转成相对项目根的 @提及，交给这个项目里正在看的
    /// 会话（不带回车，同 send_diff_comments 的做法）。
    pub fn send_path_to_session(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let style = self.file_tree_path_style(cx);
        let rel = self
            .active_project_ref(cx)
            .and_then(|project| style.strip_prefix(&path, &project.root))
            .unwrap_or_else(|| path.clone());
        let msg = format!("@{rel} ");
        self.send_to_panel_session(&msg, window, cx);
    }

    /// 文件内容框选右键「发送选中内容到会话」：带上文件名 + 选中文字，交给这个项目里
    /// 正在看的会话（不带回车）。
    pub fn send_open_file_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(of) = &self.open_file else { return };
        let selected = of.editor.read(cx).selected_value().to_string();
        if selected.trim().is_empty() {
            return;
        }
        let style = crate::runtimes::path_style(&of.runtime, cx);
        let rel = self
            .active_project_ref(cx)
            .filter(|project| project.runtime == of.runtime)
            .and_then(|project| style.strip_prefix(&of.path, &project.root))
            .unwrap_or_else(|| of.path.clone());
        let msg = format!("{rel} 里选中的这段：\n```\n{selected}\n```\n");
        self.send_to_panel_session(&msg, window, cx);
    }
}
