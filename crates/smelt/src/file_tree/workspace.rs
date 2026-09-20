//! 文件树的数据修改：展开、打开、保存、搜索、删除。
//!
//! 页面渲染在 `view.rs`。字段仍由 main.rs 的 Workspace 持有。

use std::path::Path;
use std::rc::Rc;
use std::time::Instant;

use gpui::*;
use gpui_component::input::InputEvent;

use crate::Workspace;

use super::view::{editor_language_for_path, is_previewable_image, walk_dir_cached};
use super::*;

// ===================== Workspace 方法 =====================

impl Workspace {
    /// 文件内容面板右上角源码 / 预览切换（仅 markdown 生效）。
    pub(super) fn set_file_preview(&mut self, preview: bool, cx: &mut Context<Self>) {
        if let Some(of) = self.open_file.as_mut() {
            of.preview = preview;
        }
        cx.notify();
    }

    /// 文件树：展开/收起一个文件夹。
    pub fn toggle_expand(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
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
        cx.notify();
    }

    /// 某个 cwd 当前是否已 pin 在文件树里（侧栏右键菜单据此显示「加到」还是「移除」）。
    pub fn is_file_tree_root_pinned(&self, cwd: &str) -> bool {
        self.pinned_roots.iter().any(|p| p == cwd)
    }

    /// 文件树要同时挂出来的根目录集合：按项目分组聚合（顺序跟侧栏项目列表一致），去空
    /// 去重。单项目时就一个根、行为跟以前一样；多项目时文件树把这些根一起铺开，右侧
    /// 文件不用再切项目来回换。
    pub fn workspace_roots(&self, cx: &App) -> Vec<String> {
        let mut roots: Vec<String> = Vec::new();
        // 当前选中项目根永远在第一位。跟侧栏点选走，不跟当前会话 cwd，
        // 否则点空项目/切项目时文件树还停在旧会话上。
        if let Some(cur) = self.active_project_root(cx)
            && !cur.is_empty()
        {
            roots.push(cur);
        }
        // 用户从「+ 项目」主动 pin 进来的额外根（去重、跳过已是当前项目的那个）。
        // 默认 pinned_roots 为空 → 就一个当前项目根，不显示根标题、不折腾。
        for p in &self.pinned_roots {
            if !p.is_empty() && !roots.contains(p) {
                roots.push(p.clone());
            }
        }
        roots
    }

    /// 在文件树中定位 path：展开所有祖先目录、选中、并排队滚动到该行。
    pub fn reveal_in_file_tree(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(root) = self.active_project_root(cx) else {
            return;
        };
        let path_buf = Path::new(path);
        // 自下而上展开祖先（不含文件自身）。
        let mut ancestors = Vec::new();
        let mut p = path_buf.parent();
        while let Some(parent) = p {
            let ps = parent.to_string_lossy().to_string();
            if ps.is_empty() || ps == root {
                // 根目录本身也要有 listing
                self.ensure_dir_listing(root.clone(), cx);
                break;
            }
            if !ps.starts_with(&root) {
                break;
            }
            ancestors.push(ps);
            p = parent.parent();
        }
        // 先 ensure 近根的，再展开
        for dir in ancestors.iter().rev() {
            self.expanded.insert(dir.clone());
            self.ensure_dir_listing(dir.clone(), cx);
        }
        self.ensure_dir_listing(root, cx);
        self.file_tree_selected = Some(path.to_string());
        self.file_tree_pending_reveal = Some(path.to_string());
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
            self.ensure_dir_listing(path, cx);
        }
    }

    /// 祖先目录缓存齐了就把树滚到 pending reveal 那一行。
    pub fn try_flush_file_tree_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.file_tree_pending_reveal.clone() else {
            return;
        };
        let flat = self.file_tree_flat(cx);
        if let Some(ix) = flat.iter().position(|(_, _, p)| p == &path) {
            self.file_tree_scroll.scroll_to_item(ix);
            self.file_tree_pending_reveal = None;
            // 不 notify：本帧正在 render，scroll 在 prepaint 生效即可
        }
        let _ = cx;
    }

    /// 当前扁平可见树条目：(is_dir, name, path)。
    fn file_tree_flat(&self, cx: &App) -> Vec<(bool, String, String)> {
        let Some(root) = self.active_project_root(cx) else {
            return Vec::new();
        };
        let mut raw: Vec<(usize, String, bool, String, bool)> = Vec::new();
        walk_dir_cached(&root, &self.dir_cache, &self.expanded, 0, &mut raw);
        raw.into_iter()
            .map(|(_, name, is_dir, path, _)| (is_dir, name, path))
            .collect()
    }

    /// ↑↓ 移动键盘选中。
    pub fn file_tree_move_selection(&mut self, delta: i32, cx: &mut Context<Self>) {
        let flat = self.file_tree_flat(cx);
        if flat.is_empty() {
            return;
        }
        let cur = self
            .file_tree_selected
            .as_ref()
            .and_then(|p| flat.iter().position(|(_, _, path)| path == p));
        let next = match cur {
            Some(i) => (i as i32 + delta).clamp(0, flat.len() as i32 - 1) as usize,
            None => {
                if delta >= 0 {
                    0
                } else {
                    flat.len() - 1
                }
            }
        };
        self.file_tree_selected = Some(flat[next].2.clone());
        self.file_tree_scroll.scroll_to_item(next);
        cx.notify();
    }

    /// ←：目录已展开则收起；否则选中父目录。
    pub fn file_tree_key_left(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.file_tree_selected.clone() else {
            return;
        };
        if self.expanded.contains(&path) {
            self.expanded.remove(&path);
            cx.notify();
            return;
        }
        let Some(root) = self.active_project_root(cx) else {
            return;
        };
        if let Some(parent) = Path::new(&path).parent() {
            let ps = parent.to_string_lossy().to_string();
            // 父目录在项目根之下（含根的直接子项的父 = root）
            if ps == root || (ps.starts_with(&root) && ps.len() > root.len()) {
                // 根本身不在 flat 列表里时，选中第一项的兄弟无意义；只选非根父路径
                if ps != root {
                    self.file_tree_selected = Some(ps);
                    self.try_flush_file_tree_reveal_path(cx);
                    cx.notify();
                }
            }
        }
    }

    /// →：目录未展开则展开；已展开则进第一个子项；文件则打开。
    pub fn file_tree_key_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.file_tree_selected.clone() else {
            return;
        };
        let flat = self.file_tree_flat(cx);
        let Some((is_dir, _, _)) = flat.iter().find(|(_, _, p)| p == &path) else {
            return;
        };
        if *is_dir {
            if !self.expanded.contains(&path) {
                self.expanded.insert(path.clone());
                self.ensure_dir_listing(path, cx);
                cx.notify();
            } else if let Some(ix) = flat.iter().position(|(_, _, p)| p == &path)
                && let Some((_, _, child)) = flat.get(ix + 1)
            {
                // 下一行若是更深的子项才进去
                self.file_tree_selected = Some(child.clone());
                self.file_tree_scroll.scroll_to_item(ix + 1);
                cx.notify();
            }
        } else {
            self.view_file(path, window, cx);
        }
    }

    /// Enter：目录切换展开；文件打开。
    pub fn file_tree_key_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.file_tree_selected.clone() else {
            return;
        };
        let flat = self.file_tree_flat(cx);
        let Some((is_dir, _, _)) = flat.iter().find(|(_, _, p)| p == &path) else {
            return;
        };
        if *is_dir {
            self.toggle_expand(path, cx);
        } else {
            self.view_file(path, window, cx);
        }
    }

    fn try_flush_file_tree_reveal_path(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.file_tree_selected.clone() {
            let flat = self.file_tree_flat(cx);
            if let Some(ix) = flat.iter().position(|(_, _, p)| p == &path) {
                self.file_tree_scroll.scroll_to_item(ix);
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
            // Linux：尽量打开所在目录
            if let Some(parent) = Path::new(&path).parent() {
                let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
            }
        }
    }

    /// 文件树：打开一个文件查看/编辑内容。当前文件有未保存改动时不直接切换——先弹
    /// 确认弹窗（见 pending_file_switch / render_unsaved_file_confirm），用户选了
    /// "不保存"或"保存并切换"才真正调用 open_file_now。
    pub fn view_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.view_file_at(path, None, window, cx);
    }

    /// 打开文件并可跳到指定行（1 基，搜索命中用）。
    pub fn view_file_at(
        &mut self,
        path: String,
        line: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dirty = self.open_file.as_ref().is_some_and(|of| {
            of.readable && of.editor.read(cx).value().as_ref() != of.saved_content.as_str()
        });
        if dirty {
            // 脏切换暂不带行号（确认后再 open 整文件即可）
            self.pending_file_switch = Some(path);
            cx.notify();
            return;
        }
        self.open_file_now(path, line, window, cx);
    }

    /// 实际打开文件：用 gpui-component 的 Editor（InputState 的 code_editor 模式）：
    /// tree-sitter 语法高亮 + 行号 + 搜索，直接可编辑，Cmd+S（见 save_open_file）能
    /// 存回磁盘。读文件本身放到后台线程跑（大文件不卡 UI），读完回主线程灌进编辑器；
    /// 用自增 file_gen 丢弃过期结果（期间又切了别的文件）。
    /// `goto_line`：1 基行号，读完后定位光标。
    pub fn open_file_now(
        &mut self,
        path: String,
        goto_line: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::input::{EditorState, Position};

        self.reveal_in_file_tree(&path, cx);
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
        let is_markdown = language == "md";
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .searchable(true)
                // 超长行横向滚动而不是自动换行——代码这种东西换行会破坏缩进对齐，
                // 多行输入默认开软换行，这里显式关掉。
                .soft_wrap(false)
        });
        self.open_file = Some(OpenFile {
            path: path.clone(),
            editor: editor.clone(),
            saved_content: Rc::new(String::new()),
            save_error: None,
            readable: false, // 读完确认是文本才翻真，防止读取完成前误按 Cmd+S
            conflict_pending: false,
            preview: is_markdown,
        });
        // InputState 自己会刷新编辑器，但预览和文件头属于 Workspace，必须订阅变更
        // 才能在编辑时同步更新 Markdown 预览与未保存标记，而不依赖切换 tab 触发 render。
        self._file_editor_sub = Some(cx.subscribe(&editor, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        }));
        cx.notify();

        // 图片由 GPUI 的 img 元素直接从路径异步解码；不要再走 read_to_string，
        // 否则会短暂或永久显示“可能是二进制文件”的错误占位。
        if is_previewable_image(&path) {
            return;
        }

        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let read = cx
                .background_executor()
                .spawn(async move { std::fs::read_to_string(&p) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                // 只有当前仍是这次打开的文件才写入，避免旧任务覆盖新文件。
                if this.file_gen != r#gen {
                    return;
                }
                let Some(of) = this.open_file.as_mut() else {
                    return;
                };
                match read {
                    Ok(content) => {
                        editor.update(cx, |state, cx| {
                            state.set_value(content.clone(), window, cx);
                            if let Some(line) = goto_line {
                                // 搜索命中是 1 基；Position 是 0 基。
                                let line0 = line.saturating_sub(1) as u32;
                                state.set_cursor_position(Position::new(line0, 0), window, cx);
                            }
                        });
                        of.saved_content = Rc::new(content);
                        of.readable = true;
                    }
                    Err(_) => {
                        editor.update(cx, |state, cx| {
                            state.set_value("（无法以文本方式读取：可能是二进制文件）", window, cx);
                        });
                        of.readable = false;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Cmd+S：把当前打开文件的编辑器内容写回磁盘（仅 Files 页触发，见 on_key_down）。
    /// 写之前先读一次磁盘现状跟 saved_content 比对——不一样说明文件被外部改过，
    /// 这次先不写、把 conflict_pending 置位提示用户；用户再按一次 Cmd+S 就当作
    /// 已确认覆盖，跳过这次检查直接写。写文件本身放后台线程；成功后把
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
        let content = of.editor.read(cx).value().to_string();
        // Rc<String> 不是 Send，进不了 background_executor；克隆成普通 String 再带过去。
        let expected_on_disk = (*of.saved_content).clone();
        let force = of.conflict_pending;
        let r#gen = self.file_gen;

        cx.spawn(async move |this, cx| {
            let check_path = path.clone();
            let write_content = content.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    if !force
                        && let Ok(current) = std::fs::read_to_string(&check_path)
                        && current != expected_on_disk
                    {
                        return SaveOutcome::Conflict;
                    }
                    match std::fs::write(&check_path, write_content) {
                        Ok(()) => SaveOutcome::Saved,
                        Err(e) => SaveOutcome::Error(e.to_string()),
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
                    SaveOutcome::Saved => {
                        of.saved_content = Rc::new(content);
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

    /// 文件树搜索：按 query 匹配文件名 + 文件内容，后台遍历项目、命中写回 search_results。
    /// 与 view_file 同款「background_executor + 自增 gen 丢弃过期结果」模式，绝不阻塞 render。
    /// query 未变（已有对应结果或正在跑同一 query）就跳过，避免每帧重扫。
    pub fn ensure_search(&mut self, root: String, query: String, cx: &mut Context<Self>) {
        // 已有本 query 的结果、或正有一次针对本 query 的遍历在跑，就不重复触发。
        if self
            .search_results
            .as_ref()
            .is_some_and(|s| s.query == query)
        {
            return;
        }
        self.search_gen = self.search_gen.wrapping_add(1);
        let r#gen = self.search_gen;
        // 先占位：done=false 让列表顶部显示「搜索中…」，遍历完成后替换。
        self.search_results = Some(SearchState {
            query: query.clone(),
            done: false,
            hits: Vec::new(),
            truncated: false,
        });
        cx.notify();

        cx.spawn(async move |this, cx| {
            let (r, q) = (root.clone(), query.clone());
            let (hits, truncated) = cx
                .background_executor()
                .spawn(async move { search_project(&r, &q) })
                .await;
            let _ = this.update(cx, |this, cx| {
                // 只有仍是最新一次搜索才写入，丢弃期间被新查询取代的过期结果。
                if this.search_gen == r#gen {
                    this.search_results = Some(SearchState {
                        query,
                        done: true,
                        hits,
                        truncated,
                    });
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// 确保某目录的直接子项列表缓存新鲜（>2s 或缺失就后台刷新）。
    /// 绝不阻塞 render：此前 file_tree 在 render 里同步 fs::read_dir，大目录会
    /// 像 git status 那样掉帧，这里挪到后台执行器 + 缓存，render 只读。
    pub fn ensure_dir_listing(&mut self, dir: String, cx: &mut Context<Self>) {
        let fresh = self
            .dir_cache
            .get(&dir)
            .is_some_and(|(t, _)| t.elapsed() < std::time::Duration::from_millis(2000));
        if fresh || self.dir_inflight.contains(&dir) {
            return;
        }
        self.dir_inflight.insert(dir.clone());
        cx.spawn(async move |this, cx| {
            let d = dir.clone();
            let entries = cx
                .background_executor()
                .spawn(async move {
                    // 排序与噪音目录过滤住在 fs 接缝的 Consumer 里，本地面板
                    // 与将来的远程 worktree 共用同一份规则，不会各自漂移。
                    smelt_core::fs::list_dir(&smelt_core::fs::LocalFs, std::path::Path::new(&d))
                        .into_iter()
                        .map(|entry| (entry.name, entry.is_dir))
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.dir_inflight.remove(&dir);
                this.dir_cache
                    .insert(dir, (Instant::now(), Rc::new(entries)));
                cx.notify();
            });
        })
        .detach();
    }

    /// 文件树右键「复制文件路径」：把绝对路径写入系统剪贴板。
    pub fn copy_file_path_to_clipboard(&mut self, path: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(path));
    }

    /// 文件树右键「删除文件」：先弹二次确认，用户点确定后才真正删盘。
    pub fn start_delete_file(&mut self, path: String, is_dir: bool, cx: &mut Context<Self>) {
        let label = Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
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

    /// 真正删除磁盘上的文件或目录，并刷新文件树缓存。
    fn perform_delete_file(&mut self, path: String, is_dir: bool, cx: &mut Context<Self>) {
        let ok = if is_dir {
            std::fs::remove_dir_all(&path).is_ok()
        } else {
            std::fs::remove_file(&path).is_ok()
        };
        if !ok {
            return;
        }

        let under = |base: &str, candidate: &str| {
            candidate == base || candidate.starts_with(&format!("{base}/"))
        };

        if self
            .open_file
            .as_ref()
            .is_some_and(|of| under(&path, &of.path))
        {
            self.open_file = None;
        }
        if self
            .pending_file_switch
            .as_ref()
            .is_some_and(|p| under(&path, p))
        {
            self.pending_file_switch = None;
        }

        if is_dir {
            self.expanded.retain(|p| !under(&path, p));
            self.dir_cache.retain(|p, _| !under(&path, p));
        } else {
            self.expanded.remove(&path);
        }

        if let Some(parent) = Path::new(&path).parent().and_then(|p| p.to_str()) {
            self.dir_cache.remove(parent);
        }
        cx.notify();
    }

    /// 文件树右键「发送到终端」：把路径转成相对当前 cwd 的 @提及，写进当前激活终端
    /// 的 PTY（不带回车，同 send_diff_comments 的做法）。
    pub fn send_path_to_terminal(&mut self, path: String, cx: &mut Context<Self>) {
        let root = self.active_project_root(cx);
        let rel = root
            .and_then(|root| {
                Path::new(&path)
                    .strip_prefix(&root)
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| path.clone());
        let msg = format!("@{rel} ");
        if let Some(view) = self.cur().and_then(|s| s.active_term().cloned()) {
            view.update(cx, |tv, cx| tv.send_text(&msg, cx));
        }
    }

    /// 文件内容框选右键「发送选中内容到终端」：带上文件名 + 选中文字，写进当前激活
    /// 终端的 PTY（不带回车）。
    pub fn send_open_file_selection(&mut self, cx: &mut Context<Self>) {
        let Some(of) = &self.open_file else { return };
        let selected = of.editor.read(cx).selected_value().to_string();
        if selected.trim().is_empty() {
            return;
        }
        let root = self.active_project_root(cx);
        let rel = root
            .and_then(|root| {
                Path::new(&of.path)
                    .strip_prefix(&root)
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| of.path.clone());
        let msg = format!("{rel} 里选中的这段：\n```\n{selected}\n```\n");
        if let Some(view) = self.cur().and_then(|s| s.active_term().cloned()) {
            view.update(cx, |tv, cx| tv.send_text(&msg, cx));
        }
    }
}
