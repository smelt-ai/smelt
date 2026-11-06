//! 文件、变更两块面板的内容，和它们共用的小件（文件树分隔条、变更角标）。
//! 摆在哪、标签条怎么画归布局（`workspace_layout` / `workspace_shell`），这里只画内容。
//!
//! 从 main.rs 拆出的 `impl Workspace` 方法，字段仍在 main.rs。

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::*;

use crate::{Workspace, ui_theme};

pub(crate) const MIN_FILE_TREE_WIDTH: f32 = 190.0;
/// 文件树是导航区而不是主内容区；限制最大值既能修正早期“按比例保存”留下的异常
/// 宽度，也能保证编辑器至少保有可读空间。该值只在恢复或拖树自身分隔条时变化。
pub(crate) const MAX_FILE_TREE_WIDTH: f32 = 320.0;

/// 变更角标：当前项目里**所有**仓库的改动文件总数。
///
/// 只数项目根一个仓库会让子仓里的改动在角标上隐形，而角标正是用户判断
/// “要不要点进去看”的唯一依据。
pub(crate) fn git_changes_badge(ws: &Workspace, cx: &App) -> usize {
    let Some(cwd) = ws.current_cwd(cx) else {
        return 0;
    };
    let roots: Vec<crate::ProjectRef> = match ws.git_repos.get(&cwd) {
        Some((_, set)) if !set.repos.is_empty() => set.roots().map(|root| cwd.at(root)).collect(),
        _ => vec![cwd],
    };
    roots
        .iter()
        .filter_map(|root| ws.git_status.get(root))
        .map(|(_, d)| d.files.len())
        .sum()
}

impl Workspace {
    /// Files 与 Git 右侧树列的固定像素分隔条。面板改宽时，普通 flex 布局
    /// 只会让左侧内容区伸缩；树列的 `.w(file_tree_w).flex_none()` 不会被重新分配。
    /// 拖动条本身才修改这个会话保存的宽度。
    pub(crate) fn file_tree_resize_handle(
        &mut self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(id)
            .relative()
            .w(px(6.))
            .h_full()
            .flex_none()
            .cursor_col_resize()
            .group(id)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(3.))
                    .w(px(1.))
                    .h_full()
                    .bg(rgb(ui_theme::border_dim()))
                    .group_hover(id, |line| line.bg(rgb(ui_theme::text_faint()))),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    this.file_tree_drag_start = Some((
                        f32::from(event.position.x),
                        this.file_tree_w
                            .clamp(MIN_FILE_TREE_WIDTH, MAX_FILE_TREE_WIDTH),
                    ));
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    /// 窗口级监听保证拖动指针越过 6px 分隔条后仍持续生效；组件级 mouse move
    /// 只在命中元素时触发，不能用于 resize。
    pub(crate) fn file_tree_resize_listener(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        canvas(
            |_, _, _| {},
            move |_bounds, _, window, _cx| {
                let move_view = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                    if !phase.bubble() || event.pressed_button != Some(MouseButton::Left) {
                        return;
                    }
                    move_view.update(cx, |this, cx| {
                        let Some((start_x, start_w)) = this.file_tree_drag_start else {
                            return;
                        };
                        let width = (start_w + start_x - f32::from(event.position.x))
                            .clamp(MIN_FILE_TREE_WIDTH, MAX_FILE_TREE_WIDTH);
                        if (this.file_tree_w - width).abs() > 0.5 {
                            this.file_tree_w = width;
                            cx.notify();
                        }
                    });
                });

                let up_view = view;
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _window, cx| {
                    if !phase.bubble() {
                        return;
                    }
                    up_view.update(cx, |this, cx| {
                        if this.file_tree_drag_start.take().is_some() {
                            this.save_state(cx);
                            cx.notify();
                        }
                    });
                });
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }

    /// Files 内容区右上角切换文件树显隐；停靠态和舞台展开态共用这份状态。
    pub(crate) fn toggle_file_tree(&mut self, cx: &mut Context<Self>) {
        self.file_tree_open = !self.file_tree_open;
        self.save_state(cx);
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// FILES 面板：文件树（复用全屏页的 file_tree 组件）。点文件不再提升到舞台
    /// （见 open_file_now），而是本面板自己分左右两栏：内容在左、树常驻右侧，
    /// 参考 Codex App 的「开启档案」面板——中间舞台的终端/ACP 对话完全不受影响。
    /// 停靠态和舞台全屏态共用这份 UI，见 main.rs。
    pub(crate) fn render_tool_panel_files(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 「EXPLORER」标题行去掉：上面已经有 FILES tab 高亮，这行纯属多余的重复标签。
        // 有查询串 → 显示搜索结果；否则显示文件树（跟旧全屏页行为一致，
        // file_filter/search_results 在打开文件页时懒创建 + 刷新）。
        let has_query = self
            .file_filter
            .as_ref()
            .is_some_and(|s| !s.read(cx).value().trim().is_empty());
        let open_path = self.open_file.as_ref().map(|of| of.path.as_str());
        let selected = self.file_tree_selected.as_deref();
        // 多根工作区：文件面板 也把所有项目根一起挂出来（跟全屏 Files 页
        // 同一套 workspace_roots / collapsed_roots，行为一致）。
        let roots = self.workspace_roots(cx);
        // 文件树宽度是独立的、按会话保存的用户偏好。不能跟面板总宽度按比例
        // 重算，否则拖右侧面板时文件树会在没有拖自身分隔线的情况下改变宽度。
        let tree_w = self
            .file_tree_w
            .clamp(MIN_FILE_TREE_WIDTH, MAX_FILE_TREE_WIDTH);
        let tree_open = self.file_tree_open;
        let runtime = self.file_tree_runtime(cx);
        let empty_dirs = crate::file_tree::DirCache::new();
        let git_status = |root: &str| {
            self.git_status
                .get(&crate::ProjectRef {
                    runtime: runtime.clone(),
                    root: root.to_string(),
                })
                .map(|(_, status)| status)
        };
        let tree = if has_query {
            match &self.search_results {
                Some(state) => {
                    crate::file_tree::search_results_view(state, &self.file_tree_scroll, cx)
                }
                // ensure_search 已在打开文件页或改查询时置位，通常到不了这里。
                None => div().flex_1().into_any_element(),
            }
        } else {
            crate::file_tree::file_tree(
                crate::file_tree::FileTreeParams {
                    roots: &roots,
                    expanded: &self.expanded,
                    collapsed_roots: &self.collapsed_roots,
                    dir_cache: self.dir_cache.get(&runtime).unwrap_or(&empty_dirs),
                    scroll: &self.file_tree_scroll,
                    open_path,
                    selected_path: selected,
                    panel_w: tree_w,
                    style: crate::runtimes::path_style(&runtime, cx),
                    local: runtime == crate::RuntimeKey::Local,
                    git_status: &git_status,
                    focus_handle: &self.file_tree_focus_handle,
                },
                cx,
            )
        };
        // 顶部搜索框（file_filter 在打开文件页时懒创建）。
        let search_box = self.file_filter.as_ref().map(|state| {
            div()
                .px_2()
                .py(px(6.))
                .border_b_1()
                .border_color(rgb(ui_theme::border_dim()))
                .child(gpui_component::input::Input::new(state).small())
        });
        let tree = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .children(search_box)
            .child(tree)
            .into_any_element();
        // 路径栏横跨整个面板；只有它下方才开始左右分栏。这样路径不被左侧内容区
        // 裁掉，树内搜索也不会与面包屑抢同一行，结构对齐常见编辑器的文件视图。
        let overlay_open = self.main_overlay_open();
        let content_parts = crate::file_tree::file_content_parts(
            &self.open_file,
            &roots,
            tree_open,
            overlay_open,
            cx,
        );
        let content_header = content_parts.header;
        let content = content_parts.body;
        let resize_listener = tree_open.then(|| self.file_tree_resize_listener(cx));
        // 所在区域有布局背景时文件树不铺舞台色，让图透出来。
        let backdrop = self
            .shell_layout
            .backdrop_of(&smelt_client::layout::Component::Files)
            .is_some();
        let tree_side = tree_open.then(|| {
            div()
                .h_full()
                .flex()
                .flex_none()
                .child(self.file_tree_resize_handle("tool-panel-files-split", cx))
                .child(
                    div()
                        .w(px(tree_w))
                        .flex_none()
                        .min_w_0()
                        .min_h_0()
                        .overflow_hidden()
                        .flex()
                        .when(!backdrop, |this| this.bg(rgb(ui_theme::bg_stage())))
                        .child(div().size_full().min_h_0().flex().flex_col().child(tree)),
                )
                .into_any_element()
        });
        let body = div()
            .flex_1()
            .relative()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .children(resize_listener)
            .child(
                div()
                    .size_full()
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .flex()
                            .child(content),
                    )
                    .children(tree_side),
            )
            .into_any_element();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .children(content_header)
            .child(body)
            .into_any_element()
    }

    /// GIT 面板：窄版 SOURCE CONTROL（实现见 `git_panel/view.rs` 的 git_narrow_panel，
    /// 需要访问 GitStatusData / DiffLine 的模块内私有字段）。
    pub(crate) fn render_tool_panel_git(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.git_narrow_panel(window, cx)
    }
}
