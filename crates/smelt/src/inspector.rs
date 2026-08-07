//! inspector：面板内横向 tabs + 右侧面板（默认 320px，可整体隐藏）。
//! FILES / GIT / TASK / SKILL 四个 tab，点击切换或收合；面板头
//! 带「展开」把对应的旧全屏页盖到会话舞台上（stage_override），功能零删除。
//! TASK 面板只显示当前项目的任务；完整任务总览仍由 session_list.rs 的一级导航提供。
//!
//! 跟 file_tree.rs 同一个套路：`impl Workspace` 方法，字段仍在 main.rs。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::*;

use crate::tasks::TaskStore;
use crate::{MainView, Workspace, ui_theme, workspace_frame};

/// SKILLS 面板卡片的 hover group 名，同上一个套路（卡片 `.group()` + 操作条
/// `.group_hover()`）。
const SKILL_CARD_GROUP: &str = "insp-skill-card";
pub(crate) const MIN_FILE_TREE_WIDTH: f32 = 190.0;
/// 文件树是导航区而不是主内容区；限制最大值既能修正早期“按比例保存”留下的异常
/// 宽度，也能保证编辑器至少保有可读空间。该值只在恢复或拖树自身分隔条时变化。
pub(crate) const MAX_FILE_TREE_WIDTH: f32 = 320.0;

/// inspector 面板的四个 tab。
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InspectorTab {
    Files,
    Git,
    Tasks,
    Skills,
}

impl Default for InspectorTab {
    fn default() -> Self {
        Self::Files
    }
}

impl InspectorTab {
    fn label(self) -> &'static str {
        match self {
            Self::Files => "FILES",
            Self::Git => "GIT",
            Self::Tasks => "TASK",
            Self::Skills => "SKILL",
        }
    }

    /// 面板头「⤢ 展开」对应的舞台全宽视图。
    pub(crate) fn stage_view(self) -> Option<MainView> {
        match self {
            Self::Files => Some(MainView::Files),
            Self::Git => Some(MainView::Git),
            Self::Tasks => None,
            Self::Skills => Some(MainView::Skills),
        }
    }
}

/// 任务 cwd 与项目根相同或位于其下才属于该项目。路径边界必须完整匹配，
/// 避免 `/work/api-next` 被误归到 `/work/api`。
fn task_belongs_to_project(task_cwd: &str, project_root: &str) -> bool {
    let task_cwd = task_cwd.trim();
    let project_root = project_root.trim();
    if project_root == "/" {
        return task_cwd == "/" || task_cwd.starts_with('/');
    }
    let task_cwd = task_cwd.trim_end_matches('/');
    let project_root = project_root.trim_end_matches('/');
    if task_cwd.is_empty() || project_root.is_empty() {
        return false;
    }
    task_cwd == project_root || task_cwd.starts_with(&format!("{project_root}/"))
}

fn project_label(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(path)
        .to_string()
}

fn stage_matches_docked_tab(stage_override: Option<MainView>, docked_tab: InspectorTab) -> bool {
    stage_override.and_then(MainView::inspector_stage_tab) == Some(docked_tab)
}

/// 请求打开右侧 Git 时是否真的是一次新导航。中央已经显示 Git 时，右侧切回 Git
/// 只是合并面板，必须保留当前 diff。
pub(crate) fn should_reset_git_diff_on_dock_selection(
    docked_tab: InspectorTab,
    stage_override: Option<MainView>,
) -> bool {
    docked_tab != InspectorTab::Git && stage_override != Some(MainView::Git)
}

impl Workspace {
    /// Files 与 Git 右侧树列的固定像素分隔条。父 Inspector 改宽时，普通 flex 布局
    /// 只会让左侧内容区伸缩；树列的 `.w(file_tree_w).flex_none()` 不会被重新分配。
    /// 拖动条本身才修改这个会话保存的宽度。
    pub(crate) fn file_tree_resize_handle(
        &mut self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(id)
            .w(px(6.))
            .h_full()
            .flex_none()
            .cursor_col_resize()
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

                let up_view = view.clone();
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
        cx.notify();
    }

    /// 当前右侧 tab 是否已提升到舞台。只有两边是同一种内容时才隐藏右侧面板；
    /// 例如中央展开 Git 后在右侧打开完整文件，中央 Git 与右侧 Files 应同时保留。
    pub(crate) fn inspector_panel_promoted(&self) -> bool {
        stage_matches_docked_tab(self.stage_override, self.inspector_tab)
    }

    /// 唯一改 `inspector_open` 的入口：持久状态与可复用过渡状态同步更新。
    pub(crate) fn set_inspector_open(&mut self, open: bool) {
        self.inspector_transition.set_open(open);
        self.inspector_open = open;
    }

    /// 图标条点击：已提升到舞台 → 收回停靠；同 tab 再点 → 收合/展开面板；
    /// 异 tab → 切过去并保证展开。
    pub(crate) fn toggle_inspector_tab(
        &mut self,
        tab: InspectorTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.inspector_tab == tab && self.inspector_panel_promoted() {
            // 点的就是当前占着舞台的那个 → 等价于 ⤡ 收回停靠
            self.set_stage_override(None, window, cx);
            self.set_inspector_open(true);
            self.save_state(cx);
            return;
        }
        if self.inspector_panel_promoted() {
            // 展开态下点了别的 tab：FILES / GIT 展开都跟横条共用一份停靠
            // UI（只是变宽，见 main.rs 舞台分派），能直接切过去、继续保持展开态；
            // TASK / SKILL 没有展开形态（stage_view() = None），这时才退回停靠。
            if let Some(view) = tab.stage_view() {
                self.set_stage_override(Some(view), window, cx);
                if tab == InspectorTab::Git {
                    self.reset_git_diff_view();
                }
                self.inspector_tab = tab;
                self.save_state(cx);
                cx.notify();
                return;
            }
            self.set_stage_override(None, window, cx);
            if tab == InspectorTab::Git {
                self.reset_git_diff_view();
            }
            self.inspector_tab = tab;
            self.set_inspector_open(true);
            self.save_state(cx);
            cx.notify();
            return;
        }
        if self.inspector_tab == tab && self.inspector_open {
            self.set_inspector_open(false);
        } else {
            if tab == InspectorTab::Git
                && should_reset_git_diff_on_dock_selection(self.inspector_tab, self.stage_override)
            {
                // 中央已经在看 Git 时，右侧切回 Git 只是合并两个面板，不能丢掉
                // 当前选中的 diff / 评论上下文。
                self.reset_git_diff_view();
            }
            self.inspector_tab = tab;
            self.set_inspector_open(true);
        }
        self.save_state(cx);
        cx.notify();
    }

    /// 中央展开页的 tab 操作与右侧面板独立。中央 Git 配合右侧 Files 时，收回 Git
    /// 只能影响中央区域，不能覆盖用户正在查看的文件。
    fn toggle_stage_inspector_tab(
        &mut self,
        stage_tab: InspectorTab,
        tab: InspectorTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        debug_assert!(
            self.stage_override.and_then(MainView::inspector_stage_tab) == Some(stage_tab)
        );

        if tab == stage_tab {
            self.set_stage_override(None, window, cx);
            // 右侧和中央原本是同一个面板时，收回后恢复为停靠态；若右侧已切到
            // 其他内容（如 Files），则保持用户当前的右侧上下文。
            if self.inspector_tab == stage_tab {
                self.set_inspector_open(true);
            }
        } else if let Some(view) = tab.stage_view() {
            if tab == InspectorTab::Git {
                self.reset_git_diff_view();
            }
            // 选择另一个可展开 tab 就把中央换成该内容；右侧相同的重复面板会由
            // inspector_panel_promoted 自动隐藏。
            self.inspector_tab = tab;
            self.set_stage_override(Some(view), window, cx);
        } else {
            // TASK 没有中央展开形态，切回会话并在右侧打开任务面板。
            self.set_stage_override(None, window, cx);
            self.inspector_tab = tab;
            self.set_inspector_open(true);
        }
        self.save_state(cx);
        cx.notify();
    }

    /// inspector 面板顶部的横向 tabs，避免常驻 56px 竖轨挤占会话宽度。
    ///
    /// `left_guard`：见 stage.rs::render_stage_header 同名参数——这条横条被
    /// Files/Git 展开态复用为舞台第一行时，sidebar 收起会让它变成贴着窗口左边
    /// 缘那块，真交通灯浮在它上面，需要在左边多让出交通灯宽度；平时停靠在右侧
    /// inspector 卡片里就永远不是最左边那块，传 0。宽度由调用方按全屏状态算好。
    pub(crate) fn render_inspector_rail(
        &mut self,
        left_guard: Pixels,
        right_edge: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        self.render_inspector_rail_for(self.inspector_tab, false, left_guard, right_edge, cx)
    }

    /// 中央展开页用独立的 rail：选中态和点击行为都属于中央内容，不能复用右侧
    /// 当前选中的 tab，否则 Git + Files 分栏时会把中央 Git 标成 FILES。
    pub(crate) fn render_stage_inspector_rail(
        &mut self,
        stage_tab: InspectorTab,
        left_guard: Pixels,
        right_edge: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        self.render_inspector_rail_for(stage_tab, true, left_guard, right_edge, cx)
    }

    fn render_inspector_rail_for(
        &mut self,
        active_tab: InspectorTab,
        stage_rail: bool,
        left_guard: Pixels,
        right_edge: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        // GIT 角标：当前项目改动文件数（读 git status 缓存，没有就不显示）。
        let git_changes = self
            .cur()
            .and_then(|s| s.cwd(cx))
            .and_then(|cwd| self.git_status.get(&cwd))
            .map(|(_, d)| d.files.len())
            .unwrap_or(0);
        let task_count = self
            .active_project_root(cx)
            .map(|root| {
                TaskStore::load()
                    .tasks
                    .iter()
                    .filter(|task| task_belongs_to_project(&task.project_cwd, &root))
                    .count()
            })
            .unwrap_or(0);

        const TABS: [InspectorTab; 4] = [
            InspectorTab::Files,
            InspectorTab::Git,
            InspectorTab::Tasks,
            InspectorTab::Skills,
        ];
        // 停靠 rail 收起期间不保留高亮；中央 rail 本身正在显示内容，始终高亮自己的
        // tab。两者能同时出现，因此不能共享选中状态。
        let selected_index = (stage_rail || self.inspector_open)
            .then(|| TABS.iter().position(|t| *t == active_tab))
            .flatten();

        let tab = |t: InspectorTab, badge: usize| {
            let mut b = Tab::new().label(t.label());
            if badge > 0 {
                b = b.suffix(
                    div()
                        .px(px(4.))
                        .rounded(px(8.))
                        .bg(rgb(ui_theme::accent()))
                        .text_size(px(8.))
                        .font_semibold()
                        .text_color(rgb(ui_theme::on_accent()))
                        .child(badge.to_string()),
                );
            }
            b
        };

        workspace_frame::top_bar()
            .w_full()
            .h(px(34.))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            // left_guard 由调用方按全屏状态算好（见 main.rs 的注释）：非全屏
            // 128px 要避开的不只是交通灯，还有 main.rs 顶部拖拽层里常驻绝对
            // 定位的「切换左侧栏」图标（left 92px + 24px 宽）；全屏时红绿灯
            // 隐藏、按钮移到 left(18px)，让位宽度缩小到 48px。
            .when(left_guard > px(0.), |d| d.pl(left_guard))
            // 跟 stage.rs 头栏同一档默认左边距——Underline 变体本身内边距是 0
            // （TabVariant::inner_paddings 里专门给它清零了，指望 tab 之间紧贴），
            // 之前没给非 left_guard（left_guard 为 0）分支补左边距，FILES 直接贴着
            // 卡片左边缘。
            .when(left_guard == px(0.), |d| d.pl_4())
            // 停靠态 / 展开态都贴着窗口右边缘，右上角浮着全屏/终端抽屉/
            // 侧边面板 3 颗图标（main.rs 那个 h_flex），tab 横条不留够空间
            // FILES/GIT/SKILL 和下划线会直接怼上图标，见 render_stage_header
            // 同款注释。
            .when(right_edge, |d| d.pr(px(100.)))
            .child({
                let mut bar = TabBar::new(if stage_rail {
                    "stage-inspector-rail"
                } else {
                    "inspector-rail"
                })
                .underline()
                // Underline 默认（Medium）内建行高 36px、字号 text_sm(14px)，
                // 比这一行固定的 34px 容器高，还跟旁边侧栏/搜索框的次级文字
                // 比显得偏大。XSmall 的行高是 26px（塞进 34px 绰绰有余），
                // 字号也降到 text_xs(12px)，跟 FILES/GIT/SKILL 该有的「次级
                // 导航」分量更配。
                .with_size(gpui_component::Size::XSmall)
                .flex_1()
                .on_click(cx.listener(move |ws, ix: &usize, window, cx| {
                    if let Some(tab) = TABS.get(*ix).copied() {
                        if stage_rail {
                            ws.toggle_stage_inspector_tab(active_tab, tab, window, cx);
                        } else {
                            ws.toggle_inspector_tab(tab, window, cx);
                        }
                    }
                }));
                if let Some(ix) = selected_index {
                    bar = bar.selected_index(ix);
                }
                bar.child(tab(InspectorTab::Files, 0))
                    .child(tab(InspectorTab::Git, git_changes))
                    .child(tab(InspectorTab::Tasks, task_count))
                    .child(tab(InspectorTab::Skills, 0))
            })
    }

    /// 面板统一头：36px，标题 + 自定义右侧内容。全屏入口统一放在窗口标题栏，
    /// 避免停靠/全屏时面板内部再出现一颗重复按钮。
    pub(crate) fn inspector_header(&self, title: &'static str, _cx: &mut Context<Self>) -> Div {
        div()
            .h(px(36.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            // 同 render_inspector_rail：面板头独立表面，跟面板体分层。
            .bg(rgb(ui_theme::bg_bar()))
            .border_b_1()
            .border_color(rgb(ui_theme::border_dim()))
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(ui_theme::text_muted()))
                    .child(title),
            )
    }

    /// 344px 面板本体：按当前 tab 分派。
    pub(crate) fn render_inspector_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        // 停靠 Inspector 永远在窗口右侧，不会碰到左上角交通灯；但它自己就是
        // 贴着窗口右边缘的那张卡，得让 tab 横条给右上角浮着的图标留位置。
        let tabs = self.render_inspector_rail(px(0.), true, cx);
        let body: AnyElement = match self.inspector_tab {
            InspectorTab::Files => self.render_inspector_files(window, cx),
            InspectorTab::Git => self.render_inspector_git(window, cx),
            InspectorTab::Tasks => self.render_inspector_tasks(window, cx),
            InspectorTab::Skills => self.render_inspector_skills(cx),
        };
        div()
            .w_full()
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .min_h_0()
            // 面板的玻璃底和外描边由 Workspace 统一提供，避免左右边线叠成 2px。
            .bg(gpui::transparent_black())
            .child(tabs)
            .child(body)
    }

    /// FILES 面板：文件树（复用全屏页的 file_tree 组件）。点文件不再提升到舞台
    /// （见 open_file_now），而是本面板自己分左右两栏：内容在左、树常驻右侧，
    /// 参考 Codex App 的「开启档案」面板——中间舞台的终端/ACP 对话完全不受影响。
    /// `pub(crate)`：舞台展开态（`MainView::Files`）跟停靠态共用这份 UI，见 main.rs。
    pub(crate) fn render_inspector_files(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // 「EXPLORER」标题行去掉：上面已经有 FILES tab 高亮，这行纯属多余的重复标签。
        // 有查询串 → 显示搜索结果；否则显示文件树（跟旧全屏页行为一致，
        // file_filter/search_results 已在 main.rs 的 render 顶部懒创建 + 刷新）。
        let has_query = self
            .file_filter
            .as_ref()
            .is_some_and(|s| !s.read(cx).value().trim().is_empty());
        if !has_query {
            self.try_flush_file_tree_reveal(cx);
        }
        let open_path = self.open_file.as_ref().map(|of| of.path.as_str());
        let selected = self.file_tree_selected.as_deref();
        // 多根工作区：inspector 的 EXPLORER 也把所有项目根一起挂出来（跟全屏 Files 页
        // 同一套 workspace_roots / collapsed_roots，行为一致）。
        let roots = self.workspace_roots(cx);
        // 文件树宽度是独立的、按会话保存的用户偏好。不能跟 Inspector 总宽度按比例
        // 重算，否则拖右侧面板时文件树会在没有拖自身分隔线的情况下改变宽度。
        let tree_w = self
            .file_tree_w
            .clamp(MIN_FILE_TREE_WIDTH, MAX_FILE_TREE_WIDTH);
        let tree_open = self.file_tree_open;
        let tree = if has_query {
            match &self.search_results {
                Some(state) => {
                    crate::file_tree::search_results_view(state, &self.file_tree_scroll, cx)
                }
                // ensure_search 已在 render 顶部同步置位，通常到不了这里。
                None => div().flex_1().into_any_element(),
            }
        } else {
            crate::file_tree::file_tree(
                &roots,
                &self.expanded,
                &self.collapsed_roots,
                &self.dir_cache,
                &self.file_tree_scroll,
                open_path,
                selected,
                tree_w,
                &self.git_status,
                cx,
            )
        };
        // 顶部搜索框（file_filter 已在 render 顶部懒创建）。
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
        let content_parts =
            crate::file_tree::file_content_parts(&self.open_file, &roots, tree_open, cx);
        let content_header = content_parts.header;
        let content = content_parts.body;
        let resize_listener = tree_open.then(|| self.file_tree_resize_listener(cx));
        let tree_side = tree_open.then(|| {
            div()
                .h_full()
                .flex()
                .flex_none()
                .child(self.file_tree_resize_handle("inspector-files-split", cx))
                .child(
                    div()
                        .w(px(tree_w))
                        .flex_none()
                        .min_w_0()
                        .min_h_0()
                        .overflow_hidden()
                        .flex()
                        .bg(rgb(ui_theme::bg_panel()))
                        .child(
                            div()
                                .size_full()
                                .min_h_0()
                                .flex()
                                .flex_col()
                                .border_l_1()
                                .border_color(rgb(ui_theme::border_dim()))
                                .child(tree),
                        ),
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

    /// GIT 面板：窄版 SOURCE CONTROL（实现见 git_panel.rs 的 git_narrow_panel，
    /// 需要访问 GitStatusData / DiffLine 的模块内私有字段）。
    fn render_inspector_git(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.git_narrow_panel(window, cx)
    }

    /// TASK 面板：只列当前活动项目根下的任务。完整任务总览仍保留在左侧一级导航，
    /// 这里提供和 Git / Skill 一样的项目上下文快捷入口。
    fn render_inspector_tasks(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let project_root = self.active_project_root(cx);
        let project_name = project_root.as_deref().map(project_label);
        let mut tasks = TaskStore::load().tasks;
        if let Some(root) = project_root.as_deref() {
            tasks.retain(|task| task_belongs_to_project(&task.project_cwd, root));
            tasks.sort_by(|a, b| {
                a.column
                    .sidebar_rank()
                    .cmp(&b.column.sidebar_rank())
                    .then_with(|| b.updated_at.cmp(&a.updated_at))
            });
        } else {
            tasks.clear();
        }

        let project_for_new = project_root.clone();
        let task_count = tasks.len();
        let header = div()
            .h(px(36.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_3()
            .border_b_1()
            .border_color(rgb(ui_theme::border_dim()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(rgb(ui_theme::text_muted()))
                            .child("TASKS"),
                    )
                    .children(project_name.map(|name| {
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(10.))
                            .text_color(rgb(ui_theme::text_faint()))
                            .child(name)
                    }))
                    .when(project_root.is_some(), |d| {
                        d.child(
                            div()
                                .rounded_full()
                                .px_1()
                                .bg(rgb(ui_theme::bg_elev()))
                                .text_size(px(10.))
                                .text_color(rgb(ui_theme::text_faint()))
                                .child(task_count.to_string()),
                        )
                    }),
            )
            .when(project_for_new.is_some(), |d| {
                d.child(
                    Button::new("inspector-tasks-new")
                        .icon(IconName::Plus)
                        .xsmall()
                        .ghost()
                        .tooltip("新建当前项目任务")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(root) = project_for_new.clone() {
                                this.open_new_task_for_project(root, window, cx);
                            }
                        })),
                )
            });

        let mut list = div()
            .id("inspector-task-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1p5()
            .p_2p5();

        if project_root.is_none() {
            list = list.child(
                div()
                    .pt_8()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .text_color(rgb(ui_theme::text_faint()))
                    .child(div().text_sm().child("还没有打开项目"))
                    .child(div().text_xs().child("打开或切换一个项目后显示其任务")),
            );
        } else if tasks.is_empty() {
            list = list.child(
                div()
                    .pt_8()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .text_color(rgb(ui_theme::text_faint()))
                    .child(div().text_sm().child("当前项目还没有任务"))
                    .child(div().text_xs().child("点右上角 + 新建一条任务")),
            );
        } else {
            let acp_targets = self.idle_acp_task_targets(cx);
            for task in tasks {
                let task_id = task.id.clone();
                let action_id = task_id.clone();
                let acp_task_id = task_id.clone();
                let acp_targets = acp_targets.clone();
                let can_run_in_acp =
                    task.column.is_todo() || task.column == crate::tasks::TaskColumn::Failed;
                let acp_runner = cx.entity().clone();
                let status_color = rgb(task.column.color());
                let status_label = task.column.label();
                let body = {
                    let body = task.body.trim();
                    if body.chars().count() > 80 {
                        format!("{}…", body.chars().take(80).collect::<String>())
                    } else {
                        body.to_string()
                    }
                };
                let action = if task.column.is_todo() {
                    Some("终端")
                } else if task.column == crate::tasks::TaskColumn::Failed {
                    Some("重试")
                } else if task.session_id.is_some() {
                    Some("打开")
                } else if task.column.is_active() {
                    Some("终端")
                } else {
                    None
                };

                list = list.child(
                    div()
                        .id(SharedString::from(format!("inspector-task-{task_id}")))
                        .rounded(ui_theme::card_radius())
                        .border_1()
                        .border_color(rgb(ui_theme::border_mid()))
                        .bg(ui_theme::glass_card())
                        .px_2p5()
                        .py_2()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .hover(|d| d.border_color(rgb(ui_theme::border_focus())))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .min_w_0()
                                .child(
                                    div()
                                        .size(px(6.))
                                        .rounded_xs()
                                        .bg(status_color)
                                        .flex_shrink_0(),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .font_semibold()
                                        .text_color(rgb(ui_theme::text_bright()))
                                        .child(task.title),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .rounded_xs()
                                        .px_1()
                                        .bg(crate::ui_theme::tint(task.column.color(), 0x20))
                                        .text_size(px(9.))
                                        .text_color(status_color)
                                        .child(status_label),
                                ),
                        )
                        .when(!body.is_empty(), |d| {
                            d.child(
                                div()
                                    .text_size(px(10.))
                                    .line_height(px(14.))
                                    .text_color(rgb(ui_theme::text_muted()))
                                    .line_clamp(2)
                                    .child(body),
                            )
                        })
                        .children(action.map(|label| {
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "inspector-task-action-{task_id}"
                                    )))
                                    .label(label)
                                    .xsmall()
                                    .ghost()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.primary_task_action(&action_id, window, cx);
                                    })),
                                )
                                .when(can_run_in_acp, |d| {
                                    d.child(
                                        Button::new(SharedString::from(format!(
                                            "inspector-task-acp-{task_id}"
                                        )))
                                        .label("对话")
                                        .xsmall()
                                        .ghost()
                                        .tooltip("在原生对话中执行任务，可选择或复用 Agent 对话")
                                        .dropdown_menu(move |menu, _window, _cx| {
                                            let mut menu = menu;
                                            if acp_targets.is_empty() {
                                                menu = menu.item(
                                                    PopupMenuItem::new(
                                                        "没有空闲的 ACP 对话",
                                                    )
                                                    .disabled(true),
                                                );
                                            } else {
                                                menu = menu.item(
                                                    PopupMenuItem::new(
                                                        "发送到空闲的已打开 ACP 对话",
                                                    )
                                                    .disabled(true),
                                                );
                                                for target in &acp_targets {
                                                    let task_id = acp_task_id.clone();
                                                    let session_id = target.session_id.clone();
                                                    let label = target.label.clone();
                                                    let runner = acp_runner.clone();
                                                    menu = menu.item(
                                                        PopupMenuItem::new(label).on_click(
                                                            move |_, window, cx| {
                                                                let task_id = task_id.clone();
                                                                let session_id =
                                                                    session_id.clone();
                                                                runner.update(cx, |ws, cx| {
                                                                    ws.run_task_in_open_acp(
                                                                        &task_id,
                                                                        &session_id,
                                                                        window,
                                                                        cx,
                                                                    );
                                                                });
                                                            },
                                                        ),
                                                    );
                                                }
                                            }
                                            menu = menu
                                                .separator()
                                                .item(
                                                    PopupMenuItem::new("新建 ACP 对话")
                                                        .disabled(true),
                                                );
                                            for agent in crate::settings::AcpAgentKind::ALL {
                                                let task_id = acp_task_id.clone();
                                                let runner = acp_runner.clone();
                                                menu = menu.item(
                                                    PopupMenuItem::new(format!(
                                                        "{} ACP 对话",
                                                        agent.label()
                                                    ))
                                                    .on_click(move |_, window, cx| {
                                                        let task_id = task_id.clone();
                                                        runner.update(cx, |ws, cx| {
                                                            ws.run_task_in_acp(
                                                                &task_id, agent, window, cx,
                                                            );
                                                        });
                                                    }),
                                                );
                                            }
                                            menu
                                        }),
                                    )
                                })
                        })),
                );
            }
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .child(list)
            .into_any_element()
    }

    /// SKILLS 面板：列出用户级 / 项目级 skill（`~/.claude/skills`、
    /// `<项目>/.claude/skills`），点一条把 `/name` 填进当前会话。
    ///
    /// 不做「启用/停用」开关：Claude Code 侧没有对应机制（settings.json 的
    /// `enabledPlugins` 管插件不管 skill），拨了不生效的开关比没有更糟。
    pub(crate) fn render_inspector_skills(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let cwd = self.cur().and_then(|s| s.cwd(cx));
        self.ensure_skills(cwd.clone(), cx);
        let this = cx.entity();
        let skills = self.skills_cache.as_ref().map(|(_, d)| d.clone());
        let has_project = cwd.is_some();

        let e_import = this.clone();
        let header = div()
            .h(px(36.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            .border_b_1()
            .border_color(rgb(ui_theme::border_dim()))
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(ui_theme::text_muted()))
                    .child("SKILLS"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(skills.as_ref().map(|s| {
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(ui_theme::text_faint()))
                            .child(s.len().to_string())
                    }))
                    .child(
                        div()
                            .id("inspector-skill-import")
                            .text_xs()
                            .font_semibold()
                            .text_color(rgb(ui_theme::accent()))
                            .cursor_pointer()
                            .hover(|d| d.opacity(0.8))
                            .child("导入")
                            .on_click(move |_ev, _window, cx| {
                                e_import.update(cx, |ws, cx| {
                                    ws.import_skill_from_folder(has_project, cx)
                                });
                            }),
                    ),
            );

        let mut list = div()
            .id("inspector-skill-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1p5()
            .p_2p5();
        match skills {
            None => {
                list = list.child(
                    div()
                        .pt_8()
                        .flex()
                        .justify_center()
                        .text_sm()
                        .text_color(rgb(ui_theme::text_faint()))
                        .child("加载中…"),
                );
            }
            Some(items) if items.is_empty() => {
                list = list.child(
                    div()
                        .pt_8()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .text_color(rgb(ui_theme::text_faint()))
                        .child(div().text_sm().child("还没有 skill"))
                        .child(
                            div()
                                .text_xs()
                                .font_family("monospace")
                                .child("~/.claude/skills/<名字>/SKILL.md"),
                        ),
                );
            }
            Some(items) => {
                let mut last_scope: Option<bool> = None;
                for (six, sk) in items.iter().enumerate() {
                    // 分组小标题：项目级 / 用户级（scan_skills 已按 scope 排好序）。
                    if last_scope != Some(sk.project_scope) {
                        last_scope = Some(sk.project_scope);
                        list = list.child(
                            div()
                                .px_1()
                                .pt_1p5()
                                .text_size(px(10.))
                                .font_semibold()
                                .text_color(rgb(ui_theme::text_faint()))
                                .child(if sk.project_scope {
                                    "项目级"
                                } else {
                                    "用户级"
                                }),
                        );
                    }
                    let dot = if sk.project_scope {
                        rgb(ui_theme::accent())
                    } else {
                        rgb(ui_theme::blue())
                    };
                    let e_use = this.clone();
                    let cmd = format!("/{}", sk.name);
                    let e_edit = this.clone();
                    let e_del = this.clone();
                    let e_reveal = this.clone();
                    let e_adopt = this.clone();
                    let e_resolve = this.clone();
                    let sk_edit = sk.clone();
                    let sk_del = sk.clone();
                    let sk_adopt = sk.clone();
                    let sk_resolve = sk.clone();
                    let reveal_path = sk.dir.to_string_lossy().into_owned();
                    let hover_bar = div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .flex_shrink_0()
                        .opacity(0.0)
                        .group_hover(SKILL_CARD_GROUP, |s| s.opacity(1.0))
                        .when(sk.managed, |d| {
                            d.child(
                                div()
                                    .id(("inspector-skill-edit", six))
                                    .px_1()
                                    .text_xs()
                                    .cursor_pointer()
                                    .text_color(rgb(ui_theme::text_faint()))
                                    .hover(|s| s.text_color(rgb(ui_theme::accent())))
                                    .child("编辑")
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(move |_ev, window, cx| {
                                        let sk = sk_edit.clone();
                                        e_edit.update(cx, |ws, cx| {
                                            ws.open_edit_skill_modal(&sk, window, cx)
                                        });
                                    }),
                            )
                        })
                        .when(!sk.managed, |d| {
                            // 非托管 skill 的 SKILL.md 格式我们不一定完全吃得
                            // 准（可能带我们解析不了的复杂 frontmatter），贸然
                            // 用我们的编辑器重写容易把人家的内容写坏——只给个
                            // 「访达打开」，让用户自己去看/改源文件。
                            d.child(
                                div()
                                    .id(("inspector-skill-reveal", six))
                                    .px_1()
                                    .text_xs()
                                    .cursor_pointer()
                                    .text_color(rgb(ui_theme::text_faint()))
                                    .hover(|s| s.text_color(rgb(ui_theme::accent())))
                                    .child("访达打开")
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(move |_ev, _window, cx| {
                                        let path = reveal_path.clone();
                                        e_reveal.update(cx, |ws, cx| {
                                            ws.reveal_path_in_finder(path, cx)
                                        });
                                    }),
                            )
                            .when(sk.duplicates.is_empty(), |d| {
                                d.child(
                                    div()
                                        .id(("inspector-skill-adopt", six))
                                        .px_1()
                                        .text_xs()
                                        .cursor_pointer()
                                        .text_color(rgb(ui_theme::text_faint()))
                                        .hover(|s| s.text_color(rgb(ui_theme::accent())))
                                        .child("迁移到 .smelt")
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .on_click(move |_ev, _window, cx| {
                                            let sk = sk_adopt.clone();
                                            e_adopt.update(cx, |ws, cx| {
                                                ws.open_skill_link_modal(&sk, cx)
                                            });
                                        }),
                                )
                            })
                        })
                        .when(!sk.duplicates.is_empty(), |d| {
                            d.child(
                                div()
                                    .id(("inspector-skill-resolve", six))
                                    .px_1()
                                    .text_xs()
                                    .cursor_pointer()
                                    .text_color(rgb(ui_theme::yellow()))
                                    .hover(|s| s.opacity(0.8))
                                    .child(if sk.managed {
                                        "一键清理"
                                    } else {
                                        "处理副本"
                                    })
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_click(move |_ev, _window, cx| {
                                        let sk = sk_resolve.clone();
                                        e_resolve.update(cx, |ws, cx| {
                                            if sk.managed {
                                                ws.cleanup_managed_skill_duplicates(&sk, cx)
                                            } else {
                                                ws.open_skill_link_modal(&sk, cx)
                                            }
                                        });
                                    }),
                            )
                        })
                        .child(
                            div()
                                .id(("inspector-skill-del", six))
                                .px_1()
                                .text_xs()
                                .cursor_pointer()
                                .text_color(rgb(ui_theme::text_faint()))
                                .hover(|s| s.text_color(rgb(ui_theme::red())))
                                .child("删除")
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .on_click(move |_ev, _window, cx| {
                                    let sk = sk_del.clone();
                                    e_del.update(cx, |ws, cx| ws.request_delete_skill(&sk, cx));
                                }),
                        );
                    list = list.child(
                        div()
                            .id(("inspector-skill", six))
                            .group(SKILL_CARD_GROUP)
                            .rounded(ui_theme::card_radius())
                            .border_1()
                            .border_color(rgb(ui_theme::border_mid()))
                            .bg(ui_theme::glass_card())
                            .px_2p5()
                            .py_2()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .cursor_pointer()
                            .hover(|d| d.border_color(rgb(ui_theme::border_focus())))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .child(div().flex_shrink_0().size(px(6.)).rounded_xs().bg(dot))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_xs()
                                            .font_semibold()
                                            .font_family("monospace")
                                            .text_color(rgb(ui_theme::text_bright()))
                                            .truncate()
                                            .child(sk.name.clone()),
                                    )
                                    .when(!sk.duplicates.is_empty(), |d| {
                                        d.child(
                                            div()
                                                .flex_shrink_0()
                                                .px_1()
                                                .rounded_xs()
                                                .bg(crate::ui_theme::tint(ui_theme::yellow(), 0x20))
                                                .text_size(px(9.))
                                                .text_color(rgb(ui_theme::yellow()))
                                                .child(format!(
                                                    "{} 个副本",
                                                    sk.duplicates.len() + 1
                                                )),
                                        )
                                    })
                                    .child(hover_bar),
                            )
                            .when(!sk.description.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_size(px(10.))
                                        .line_height(px(14.))
                                        .text_color(rgb(ui_theme::text_muted()))
                                        // 描述常是一长段触发条件，卡片里只留两行的量。
                                        .max_h(px(28.))
                                        .overflow_hidden()
                                        .child(sk.description.clone()),
                                )
                            })
                            .when(!sk.managed, |d| {
                                // 非托管 skill：不用笼统的「旧」，直接标出它实际躺在哪个
                                // agent 的目录里。挪到底部跟托管卡片的工具行同一个位置，
                                // 免得「工具标签有的在标题行、有的在底部」看着不是一套
                                // 卡片规范——标题行只留名字本身，工具归属统一放底部。
                                d.child(
                                    div()
                                        .flex_shrink_0()
                                        .px_1()
                                        .rounded_xs()
                                        .border_1()
                                        .border_color(rgb(ui_theme::border_dim()))
                                        .text_size(px(9.))
                                        .text_color(rgb(ui_theme::text_faint()))
                                        .child(sk.source_agent.unwrap_or("旧")),
                                )
                            })
                            .when(sk.managed, |d| {
                                d.child(div().flex().items_center().gap_1().children(
                                    crate::skills::AGENT_TARGETS.iter().map(|t| {
                                        let linked = sk.linked_agents.contains(&t.label);
                                        let e_toggle = this.clone();
                                        let sk_toggle = sk.clone();
                                        let label = t.label;
                                        div()
                                            .id(format!("inspector-skill-link-chip-{six}-{label}"))
                                            .px_1()
                                            .rounded_xs()
                                            .text_size(px(9.))
                                            .cursor_pointer()
                                            .when(linked, |d| {
                                                d.text_color(rgb(ui_theme::text_bright()))
                                                    .bg(rgb(ui_theme::bg_elev()))
                                            })
                                            .when(!linked, |d| {
                                                d.text_color(rgb(ui_theme::text_faint()))
                                                    .opacity(0.5)
                                                    .hover(|d| d.opacity(0.85))
                                            })
                                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                                cx.stop_propagation()
                                            })
                                            .on_click(move |_ev, _window, cx| {
                                                e_toggle.update(cx, |ws, cx| {
                                                    ws.toggle_managed_skill_link(
                                                        &sk_toggle, label, cx,
                                                    )
                                                });
                                            })
                                            .child(label)
                                    }),
                                ))
                            })
                            .on_click(move |_ev, window, cx| {
                                let cmd = cmd.clone();
                                e_use.update(cx, |ws, cx| {
                                    ws.send_skill_to_session(&cmd, window, cx)
                                });
                            }),
                    );
                }
            }

        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(header)
            .child(list)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        InspectorTab, should_reset_git_diff_on_dock_selection, stage_matches_docked_tab,
        task_belongs_to_project,
    };
    use crate::MainView;

    #[test]
    fn task_project_match_respects_path_boundaries() {
        assert!(task_belongs_to_project("/work/api", "/work/api"));
        assert!(task_belongs_to_project("/work/api/src", "/work/api/"));
        assert!(task_belongs_to_project("/", "/"));
        assert!(task_belongs_to_project("/work/api", "/"));
        assert!(!task_belongs_to_project("/work/api-next", "/work/api"));
        assert!(!task_belongs_to_project("/work/other", "/work/api"));
    }

    #[test]
    fn stage_and_dock_only_merge_when_tabs_match() {
        assert!(stage_matches_docked_tab(
            Some(MainView::Git),
            InspectorTab::Git
        ));
        assert!(!stage_matches_docked_tab(
            Some(MainView::Git),
            InspectorTab::Files
        ));
        assert!(!stage_matches_docked_tab(
            Some(MainView::History),
            InspectorTab::Files
        ));
    }

    #[test]
    fn returning_to_center_git_keeps_the_existing_diff() {
        assert!(!should_reset_git_diff_on_dock_selection(
            InspectorTab::Files,
            Some(MainView::Git)
        ));
        assert!(should_reset_git_diff_on_dock_selection(
            InspectorTab::Files,
            None
        ));
        assert!(!should_reset_git_diff_on_dock_selection(
            InspectorTab::Git,
            None
        ));
    }
}
