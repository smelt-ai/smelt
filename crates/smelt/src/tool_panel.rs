//! Tool Panel：面板内横向 tabs + 右侧面板（默认 320px，可整体隐藏）。
//! Files / Git / History 三个内置 tab 加插件的 tab，点击切换或收合；面板头的
//! 全屏按钮只切换 Tool Panel 容器的展示状态，内容选择始终由当前 tab 决定。
//!
//! 从 main.rs 拆出的 `impl Workspace` 方法，字段仍在 main.rs。

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::*;

use crate::{StageCover, Workspace, ui_theme};

pub(crate) const MIN_FILE_TREE_WIDTH: f32 = 190.0;
/// 文件树是导航区而不是主内容区；限制最大值既能修正早期“按比例保存”留下的异常
/// 宽度，也能保证编辑器至少保有可读空间。该值只在恢复或拖树自身分隔条时变化。
pub(crate) const MAX_FILE_TREE_WIDTH: f32 = 320.0;

/// 任何无法解析的 tab 都降级到它。
const DEFAULT_PANEL_KEY: &str = "files";

/// Tool Panel 的 tab。前三个是内置的；`Plugin` 是插件（见 `plugins`），
/// 数量和标题都来自已安装插件的 manifest，宿主不认识它们的具体身份。
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(crate) enum ToolPanelTab {
    #[default]
    Files,
    Git,
    History,
    Plugin(crate::plugins::PluginTabSlot),
}

impl serde::Serialize for ToolPanelTab {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            // 存插件自己的稳定身份，不存槽位下标：下标会随插件增删平移。
            // 查不到 key（注册表还没建好）时降级成 files，不写坏存档。
            Self::Plugin(slot) => match crate::plugins::key_for(*slot) {
                Some(key) => {
                    use serde::ser::SerializeMap;
                    let mut map = serializer.serialize_map(Some(1))?;
                    map.serialize_entry("plugin", &key)?;
                    map.end()
                }
                None => serializer.serialize_str(DEFAULT_PANEL_KEY),
            },
            builtin => serializer.serialize_str(
                builtin_panel(*builtin).map_or(DEFAULT_PANEL_KEY, |panel| panel.key),
            ),
        }
    }
}

impl<'de> serde::Deserialize<'de> for ToolPanelTab {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // 不用 enum：内置面板的 key 住在注册表里，再拄一份就成了第二份真相。
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Builtin(String),
            Plugin { plugin: String },
        }
        Ok(
            match <Wire as serde::Deserialize>::deserialize(deserializer)? {
                Wire::Builtin(key) => Self::from_key(&key),
                // 插件被卸载后，存档里指向它的 tab 自动降级——
                // 这是"可拔插"在持久化层该有的样子，不是错误。
                Wire::Plugin { plugin } => Self::plugin_or_files(&plugin),
            },
        )
    }
}

impl ToolPanelTab {
    /// 存档里的内置 key → tab。未知 key 降级成默认面板。
    fn from_key(key: &str) -> Self {
        BUILTIN_PANELS
            .iter()
            .find(|panel| panel.key == key)
            .map_or(Self::Files, |panel| panel.tab)
    }

    /// 解析插件 tab 的稳定 key；插件不可用时降级成 Files。
    fn plugin_or_files(key: &str) -> Self {
        crate::plugins::slot_for_key(key).map_or(Self::Files, Self::Plugin)
    }

    /// 页签元素身份。插件增删会平移槽位下标，key 不会。
    fn element_key(self) -> String {
        match self {
            Self::Plugin(slot) => {
                crate::plugins::key_for(slot).unwrap_or_else(|| format!("plugin:{}", slot.0))
            }
            builtin => builtin_panel(builtin)
                .map(|panel| panel.key.to_string())
                .unwrap_or_else(|| DEFAULT_PANEL_KEY.to_string()),
        }
    }

    fn label(self) -> String {
        match self {
            // 插件面板的标题来自 manifest，只有运行时才知道。
            Self::Plugin(slot) => crate::plugins::title(slot),
            builtin => builtin_panel(builtin).map_or_else(String::new, |p| p.title.to_string()),
        }
    }
}

/// 一个内置面板的声明。
///
/// 内置面板跟插件面板一样是「注册进来的一项」，而不是散在 tabs 列表、
/// `label()`、渲染分派和角标特判四处的 if-else。新增一个内置面板 = 表里加一行。
struct BuiltinPanel {
    tab: ToolPanelTab,
    /// 存档里的稳定身份。改它等于丢掉用户上次停留的 tab。
    key: &'static str,
    title: &'static str,
    render: fn(&mut Workspace, &mut Window, &mut Context<Workspace>) -> AnyElement,
    /// tab 上的计数角标（返回 0 就不显示）。None = 这个面板没角标。
    badge: Option<fn(&Workspace, &mut Context<Workspace>) -> usize>,
}

const BUILTIN_PANELS: &[BuiltinPanel] = &[
    BuiltinPanel {
        tab: ToolPanelTab::Files,
        key: DEFAULT_PANEL_KEY,
        title: "文件",
        render: |ws, window, cx| ws.render_tool_panel_files(window, cx),
        badge: None,
    },
    BuiltinPanel {
        tab: ToolPanelTab::Git,
        key: "git",
        title: "变更",
        render: |ws, window, cx| ws.render_tool_panel_git(window, cx),
        badge: Some(git_changes_badge),
    },
    BuiltinPanel {
        tab: ToolPanelTab::History,
        key: "history",
        title: "历史",
        render: |ws, _window, cx| ws.render_history_view(cx),
        badge: None,
    },
];

fn builtin_panel(tab: ToolPanelTab) -> Option<&'static BuiltinPanel> {
    BUILTIN_PANELS.iter().find(|panel| panel.tab == tab)
}

/// 变更角标：当前项目里**所有**仓库的改动文件总数。
///
/// 只数项目根一个仓库会让子仓里的改动在角标上隐形，而角标正是用户判断
/// “要不要点进去看”的唯一依据。
fn git_changes_badge(ws: &Workspace, cx: &mut Context<Workspace>) -> usize {
    let Some(cwd) = ws.cur().and_then(|s| s.cwd(cx)) else {
        return 0;
    };
    let roots: Vec<String> = match ws.git_repos.get(&cwd) {
        Some((_, set)) if !set.repos.is_empty() => set.roots().map(str::to_string).collect(),
        _ => vec![cwd],
    };
    roots
        .iter()
        .filter_map(|root| ws.git_status.get(root))
        .map(|(_, d)| d.files.len())
        .sum()
}

/// 请求打开右侧 Git 时是否真的是一次新导航。中央已经显示 Git 时，右侧切回 Git
/// 只是合并面板，必须保留当前 diff。
pub(crate) fn should_reset_git_diff_on_dock_selection(
    docked_tab: ToolPanelTab,
    stage_tab: Option<ToolPanelTab>,
) -> bool {
    docked_tab != ToolPanelTab::Git && stage_tab != Some(ToolPanelTab::Git)
}

impl Workspace {
    /// Files 与 Git 右侧树列的固定像素分隔条。父 Tool Panel 改宽时，普通 flex 布局
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

    /// Tool Panel 是否处于舞台全屏展示状态。它是容器级状态，不按内容 tab 分支。
    pub(crate) fn tool_panel_promoted(&self) -> bool {
        self.stage_cover
            .is_some_and(StageCover::is_tool_panel_cover)
    }

    /// 当前中央 Tool Panel 舞台正在展示的 tab。
    pub(crate) fn active_stage_tool_panel_tab(&self) -> Option<ToolPanelTab> {
        match self.stage_cover {
            Some(StageCover::ToolPanel) => Some(self.tool_panel_tab),
            Some(StageCover::Unknown) | None => None,
        }
    }

    pub(crate) fn tool_panel_stage_active(&self, tab: ToolPanelTab) -> bool {
        self.active_stage_tool_panel_tab() == Some(tab)
    }

    /// 唯一改 `tool_panel_open` 的入口。
    pub(crate) fn set_tool_panel_open(&mut self, open: bool, cx: &mut Context<Self>) {
        let changed = self.tool_panel_open != open;
        self.tool_panel_open = open;
        if changed {
            self.schedule_visible_refresh(cx);
        }
    }

    /// 切换 Tool Panel 显隐（或全屏退出），与右上角按钮逻辑对齐。
    pub(crate) fn toggle_tool_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tool_panel_promoted() {
            self.set_stage_cover(None, window, cx);
            self.set_tool_panel_open(false, cx);
            self.focus_active_stage(window, cx);
        } else {
            let next_open = !self.tool_panel_open;
            self.set_tool_panel_open(next_open, cx);
            if !next_open {
                self.focus_active_stage(window, cx);
            }
        }
        self.save_state(cx);
        cx.notify();
    }

    /// Tool Panel tab 点击：停靠态同 tab 可收合面板；全屏态只切换内容，不改变
    /// 全屏展示状态。全屏的进入/退出由容器按钮（或 Esc）负责。
    pub(crate) fn toggle_tool_panel_tab(&mut self, tab: ToolPanelTab, cx: &mut Context<Self>) {
        if self.tool_panel_promoted() {
            // 全屏态切换内容仍用当前 Tool Panel tab；点击当前 tab
            // 也保持全屏，避免内容导航反向改变容器展示状态。
            if self.tool_panel_tab != tab {
                if tab == ToolPanelTab::Git {
                    self.reset_git_diff_view();
                }
                self.tool_panel_tab = tab;
            }
            self.save_state(cx);
            self.schedule_visible_refresh(cx);
            cx.notify();
            return;
        }
        if self.tool_panel_tab == tab && self.tool_panel_open {
            self.set_tool_panel_open(false, cx);
        } else {
            if tab == ToolPanelTab::Git
                && should_reset_git_diff_on_dock_selection(
                    self.tool_panel_tab,
                    self.active_stage_tool_panel_tab(),
                )
            {
                // 中央已经在看 Git 时，右侧切回 Git 只是合并两个面板，不能丢掉
                // 当前选中的 diff / 评论上下文。
                self.reset_git_diff_view();
            }
            self.tool_panel_tab = tab;
            self.set_tool_panel_open(true, cx);
        }
        self.save_state(cx);
        self.schedule_visible_refresh(cx);
        cx.notify();
    }

    /// 文件 / 变更 / 技能 / 历史。这一条就是交通灯带，不再在它下面另垫标题。
    pub(crate) fn render_tool_panel_tabs(
        &mut self,
        active_tab: ToolPanelTab,
        stage_rail: bool,
        left_guard: Pixels,
        cx: &mut Context<Self>,
    ) -> Div {
        // 内置面板来自注册表，插件面板追在后面；两者在这一层同等。
        let mut tabs: Vec<ToolPanelTab> = BUILTIN_PANELS.iter().map(|panel| panel.tab).collect();
        tabs.extend(
            crate::plugins::slots()
                .into_iter()
                .map(ToolPanelTab::Plugin),
        );
        // 停靠 rail 收起期间不保留高亮；全屏 rail 始终高亮当前 Tool Panel tab。
        let selected_index = (stage_rail || self.tool_panel_open)
            .then(|| tabs.iter().position(|t| *t == active_tab))
            .flatten();

        // 下划线跟着选中页签滑过去，不是在两个页签之间瞬移。
        let mut track = smelt_ui::segmented::segmented_track(
            if stage_rail {
                "stage-tool-panel-rail"
            } else {
                "tool-panel-rail"
            },
            smelt_ui::segmented::SegmentedVariant::Underline,
            selected_index,
        );
        for (ix, entry) in tabs.iter().copied().enumerate() {
            // 角标由面板自己声明，不在这里特判某个 tab。
            let badge = builtin_panel(entry)
                .and_then(|panel| panel.badge)
                .map_or(0, |badge| badge(self, cx));
            let tab_key = entry.element_key();
            let tab = smelt_ui::segmented::underline_segment(
                (ElementId::from("tool-panel-tab"), tab_key),
                entry.label(),
                selected_index == Some(ix),
                cx,
            )
            // 字格默认 20px，下划线画在轨道底、格的外面。铺满轨道后，点字和下划线都落在这一格上。
            .h_full()
            // 按下就切换。这一行在标题栏里，松手经常到不了，不能等 click。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, _, _, cx| {
                    cx.stop_propagation();
                    ws.toggle_tool_panel_tab(entry, cx);
                }),
            )
            .when(badge > 0, |tab| {
                tab.child(
                    div()
                        .px(px(5.))
                        .rounded_full()
                        .bg(rgb(ui_theme::action_fill()))
                        .text_size(px(8.))
                        .font_semibold()
                        .text_color(rgb(ui_theme::action_on()))
                        .child(badge.to_string()),
                )
            });
            track = track.child(tab);
        }
        // 页签按内容宽，放不下再缩。空白留给后面的拖动手柄，不要把整行都包进拖窗口。
        let bar_h = crate::workspace_frame::TOP_BAR_HEIGHT;
        let tab_bar = div()
            .id(if stage_rail {
                "stage-tool-panel-tabs"
            } else {
                "tool-panel-tabs"
            })
            .flex_initial()
            .min_w_0()
            .h(bar_h)
            .flex()
            .items_center()
            .overflow_x_scroll()
            .child(track);
        // 分隔线铺满这一行。页签条自己不再画一条，否则缩成内容宽后线会断开。
        div()
            .w_full()
            .h(bar_h)
            .flex_none()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(ui_theme::hairline())
            .child(crate::workspace_frame::with_window_drag(
                // 铺满舞台且侧栏收起时，页签从交通灯右边开始。停靠右栏时 left_guard 是 0。
                div().w(left_guard + px(16.)).h(bar_h).flex_none(),
            ))
            .child(tab_bar)
            .child(crate::workspace_frame::with_window_drag(
                div().flex_1().min_w_0().h(bar_h),
            ))
            // 窗口开关浮在这一行右上角，tab 收到开关左边。
            .child(crate::workspace_frame::with_window_drag(
                div()
                    .w(px(crate::stage::CHROME_TOGGLE_RESERVE))
                    .h(bar_h)
                    .flex_none(),
            ))
    }

    /// 停靠右栏：tab 在抽屉顶，内容在下面。窗口顶栏不放这些 tab。
    pub(crate) fn render_tool_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let tabs = self.render_tool_panel_tabs(self.tool_panel_tab, false, px(0.), cx);
        let body = self.render_tool_panel_content(self.tool_panel_tab, window, cx);
        div()
            .w_full()
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .min_h_0()
            .bg(gpui::transparent_black())
            .child(tabs)
            .child(body)
    }

    /// Tool Panel 全屏：tab 仍在抽屉内容顶，不进共用窗口顶栏。
    pub(crate) fn render_tool_panel_stage(
        &mut self,
        left_guard: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tab = self
            .active_stage_tool_panel_tab()
            .unwrap_or(self.tool_panel_tab);
        v_flex()
            .flex_1()
            .min_h_0()
            .child(self.render_tool_panel_tabs(tab, true, left_guard, cx))
            .child(self.render_tool_panel_content(tab, window, cx))
            .into_any_element()
    }

    /// Tool Panel 的内容渲染与停靠/全屏展示方式无关。
    pub(crate) fn render_tool_panel_content(
        &mut self,
        tab: ToolPanelTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match tab {
            ToolPanelTab::Plugin(slot) => {
                // 插件经 smelt-workspace.context() 读当前项目；这里只写内存。
                crate::plugins::set_project_root(self.active_project_root(cx));
                crate::plugins::render(slot, cx)
            }
            builtin => match builtin_panel(builtin) {
                Some(panel) => (panel.render)(self, window, cx),
                // 注册表里没有就不渲染：这是声明与实现脱节，不应该 panic 拖垮整个窗口。
                None => div().flex_1().min_h_0().into_any_element(),
            },
        }
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
        // 多根工作区：Tool Panel 的 EXPLORER 也把所有项目根一起挂出来（跟全屏 Files 页
        // 同一套 workspace_roots / collapsed_roots，行为一致）。
        let roots = self.workspace_roots(cx);
        // 文件树宽度是独立的、按会话保存的用户偏好。不能跟 Tool Panel 总宽度按比例
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
                // ensure_search 已在打开文件页或改查询时置位，通常到不了这里。
                None => div().flex_1().into_any_element(),
            }
        } else {
            crate::file_tree::file_tree(
                crate::file_tree::FileTreeParams {
                    roots: &roots,
                    expanded: &self.expanded,
                    collapsed_roots: &self.collapsed_roots,
                    dir_cache: &self.dir_cache,
                    scroll: &self.file_tree_scroll,
                    open_path,
                    selected_path: selected,
                    panel_w: tree_w,
                    git_status: &self.git_status,
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
                .child(self.file_tree_resize_handle("tool-panel-files-split", cx))
                .child(
                    div()
                        .w(px(tree_w))
                        .flex_none()
                        .min_w_0()
                        .min_h_0()
                        .overflow_hidden()
                        .flex()
                        .bg(rgb(ui_theme::bg_stage()))
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
    fn render_tool_panel_git(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.git_narrow_panel(window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::{BUILTIN_PANELS, ToolPanelTab, should_reset_git_diff_on_dock_selection};
    use crate::StageCover;

    /// 每个内置面板都要能存进存档再原样读回来。
    ///
    /// key 是用户上次停留位置的唯一凭据：注册表里加了面板却忘了处理持久化，
    /// 表现就是「重启后 tab 莫名其妙跳回文件」，而且不会有任何报错。
    #[test]
    fn every_builtin_panel_round_trips_through_the_archive() {
        for panel in BUILTIN_PANELS {
            let json = serde_json::to_string(&panel.tab).unwrap();
            let back: ToolPanelTab = serde_json::from_str(&json).unwrap();
            assert_eq!(back, panel.tab, "面板 {} 存档往返不一致", panel.key);
        }
    }

    /// key 撞车会让两个面板互相顶掉对方的存档位置。
    #[test]
    fn builtin_panel_keys_are_unique() {
        let mut keys: Vec<&str> = BUILTIN_PANELS.iter().map(|panel| panel.key).collect();
        keys.sort_unstable();
        let total = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), total, "内置面板 key 必须唯一");
    }

    #[test]
    fn unknown_builtin_tab_deserializes_to_files() {
        let tab: ToolPanelTab = serde_json::from_str("\"retired-tab\"").unwrap();
        assert_eq!(tab, ToolPanelTab::Files);
        let tab: ToolPanelTab = serde_json::from_str("\"git\"").unwrap();
        assert_eq!(tab, ToolPanelTab::Git);
        let tab: ToolPanelTab = serde_json::from_str("\"history\"").unwrap();
        assert_eq!(tab, ToolPanelTab::History);
        // 插件不在（这里注册表为空）时降级成 Files。
        let tab: ToolPanelTab = serde_json::from_str(r#"{"plugin":"com.smelt.skills"}"#).unwrap();
        assert_eq!(tab, ToolPanelTab::Files);
    }

    #[test]
    fn tool_panel_fullscreen_is_panel_level() {
        assert!(StageCover::ToolPanel.is_tool_panel_cover());
        assert!(!StageCover::Unknown.is_tool_panel_cover());
    }

    #[test]
    fn returning_to_center_git_keeps_the_existing_diff() {
        assert!(!should_reset_git_diff_on_dock_selection(
            ToolPanelTab::Files,
            Some(ToolPanelTab::Git)
        ));
        assert!(should_reset_git_diff_on_dock_selection(
            ToolPanelTab::Files,
            None
        ));
        assert!(!should_reset_git_diff_on_dock_selection(
            ToolPanelTab::Git,
            None
        ));
    }
}
