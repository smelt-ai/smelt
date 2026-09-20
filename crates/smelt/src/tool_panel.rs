//! Tool Panel：面板内横向 tabs + 右侧面板（默认 320px，可整体隐藏）。
//! Files / Git / History 三个内置 tab 加插件贡献的 tab，点击切换或收合；面板头的
//! 全屏按钮只切换 Tool Panel 容器的展示状态，内容选择始终由当前 tab 决定。
//!
//! 从 main.rs 拆出的 `impl Workspace` 方法，字段仍在 main.rs。

use gpui::*;
use gpui_component::tab::{Tab, TabBar};
use gpui_component::*;

use crate::{StageCover, Workspace, resizable_split, ui_theme};

pub(crate) const MIN_FILE_TREE_WIDTH: f32 = 190.0;
/// 文件树是导航区而不是主内容区；限制最大值既能修正早期“按比例保存”留下的异常
/// 宽度，也能保证编辑器至少保有可读空间。该值只在恢复或拖树自身分隔条时变化。
pub(crate) const MAX_FILE_TREE_WIDTH: f32 = 320.0;

/// 「技能」曾经是内置 tab，现在由 skills 插件贡献。旧存档里的 `"skills"`
/// 一次性映射到这个 key；插件不在（被停用/卸载）就按常规降级成 Files。
const MIGRATED_SKILLS_TAB_KEY: &str = "com.smelt.skills/skills";
/// 旧存档里「技能」内置 tab 用过的 key。
const LEGACY_SKILLS_PANEL_KEY: &str = "skills";
/// 任何无法解析的 tab 都降级到它。
const DEFAULT_PANEL_KEY: &str = "files";

/// Tool Panel 的 tab。前三个是内置的；`Plugin` 是插件贡献的，
/// 数量和标题都来自已安装插件的 manifest，宿主不认识它们的具体身份。
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub(crate) enum ToolPanelTab {
    #[default]
    Files,
    Git,
    History,
    Plugin(crate::plugin_ui::PluginTabSlot),
}

impl serde::Serialize for ToolPanelTab {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            // 存插件自己的稳定身份，不存槽位下标：下标会随插件增删平移。
            // 查不到 key（注册表还没建好）时降级成 files，不写坏存档。
            Self::Plugin(slot) => match crate::plugin_ui::key_for(*slot) {
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
        // 旧存档可能存了已迁走的内置 tab（"skills"）或已删除的名字：
        // 反序列化时归一化，避免整个 UI 存档因未知变体反序列化失败被静默丢弃。
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
                // 插件被卸载或停用后，存档里指向它的 tab 自动降级——
                // 这是"可拔插"在持久化层该有的样子，不是错误。
                Wire::Plugin { plugin } => Self::plugin_or_files(&plugin),
            },
        )
    }
}

impl ToolPanelTab {
    /// 存档里的内置 key → tab。未知 key 降级成默认面板。
    fn from_key(key: &str) -> Self {
        if key == LEGACY_SKILLS_PANEL_KEY {
            return Self::migrated_skills();
        }
        BUILTIN_PANELS
            .iter()
            .find(|panel| panel.key == key)
            .map_or(Self::Files, |panel| panel.tab)
    }

    /// 解析插件 tab 的稳定 key；插件不可用时降级成 Files。
    fn plugin_or_files(key: &str) -> Self {
        crate::plugin_ui::slot_for_key(key).map_or(Self::Files, Self::Plugin)
    }

    /// 旧存档里的「技能」tab 现在指向 skills 插件。
    pub(crate) fn migrated_skills() -> Self {
        Self::plugin_or_files(MIGRATED_SKILLS_TAB_KEY)
    }

    fn label(self) -> String {
        match self {
            // 插件面板的标题来自 manifest，只有运行时才知道。
            Self::Plugin(slot) => crate::plugin_ui::title(slot),
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
                    .group_hover(id, |line| {
                        line.bg(resizable_split::resize_handle_hover_color())
                    }),
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
        cx.notify();
    }

    /// Tool Panel 是否处于舞台全屏展示状态。它是容器级状态，不按内容 tab 分支。
    pub(crate) fn tool_panel_promoted(&self) -> bool {
        self.stage_cover
            .is_some_and(StageCover::is_tool_panel_cover)
    }

    /// 当前中央 Tool Panel 舞台正在展示的 tab。新状态由 Tool Panel 自己的 tab 保存；
    /// 运行时若暂时读到旧具体 StageCover 变体，则从变体本身推导。
    pub(crate) fn active_stage_tool_panel_tab(&self) -> Option<ToolPanelTab> {
        match self.stage_cover {
            Some(StageCover::ToolPanel) => Some(self.tool_panel_tab),
            Some(StageCover::Unknown) | None => None,
            Some(view) => view.legacy_tool_panel_tab(),
        }
    }

    pub(crate) fn tool_panel_stage_active(&self, tab: ToolPanelTab) -> bool {
        self.active_stage_tool_panel_tab() == Some(tab)
    }

    /// 唯一改 `tool_panel_open` 的入口：持久状态与可复用过渡状态同步更新。
    pub(crate) fn set_tool_panel_open(&mut self, open: bool) {
        self.tool_panel_transition.set_open(open);
        self.tool_panel_open = open;
    }

    /// 切换 Tool Panel 显隐（或全屏退出），与右上角按钮逻辑对齐。
    pub(crate) fn toggle_tool_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tool_panel_promoted() {
            self.set_stage_cover(None, window, cx);
            self.set_tool_panel_open(false);
            self.focus_active_stage(window, cx);
        } else {
            let next_open = !self.tool_panel_open;
            self.set_tool_panel_open(next_open);
            if !next_open {
                self.focus_active_stage(window, cx);
            }
        }
        self.save_state(cx);
        cx.notify();
    }

    /// Tool Panel tab 点击：停靠态同 tab 可收合面板；全屏态只切换内容，不改变
    /// 全屏展示状态。全屏的进入/退出由容器按钮（或 Esc）负责。
    pub(crate) fn toggle_tool_panel_tab(
        &mut self,
        tab: ToolPanelTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tool_panel_promoted() {
            // 全屏态切换内容仍用当前 Tool Panel tab；点击当前 tab
            // 也保持全屏，避免内容导航反向改变容器展示状态。
            let active_tab = self
                .active_stage_tool_panel_tab()
                .unwrap_or(self.tool_panel_tab);
            if active_tab != tab {
                if tab == ToolPanelTab::Git {
                    self.reset_git_diff_view();
                }
                self.tool_panel_tab = tab;
            } else if self.tool_panel_tab != tab {
                // 旧具体 StageCover 变体运行时兼容：统一当前内容字段。
                self.tool_panel_tab = tab;
            }
            if self.stage_cover != Some(StageCover::ToolPanel) {
                self.set_stage_cover(Some(StageCover::ToolPanel), window, cx);
            }
            self.save_state(cx);
            cx.notify();
            return;
        }
        if self.tool_panel_tab == tab && self.tool_panel_open {
            self.set_tool_panel_open(false);
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
            self.set_tool_panel_open(true);
        }
        self.save_state(cx);
        cx.notify();
    }

    /// 文件 / 变更 / 技能 / 历史。只出 tab 条，外层 34px 铬由共用顶栏包。
    pub(crate) fn render_tool_panel_tabs(
        &mut self,
        active_tab: ToolPanelTab,
        stage_rail: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        // 内置面板来自注册表，插件面板追在后面；两者在这一层同等。
        let mut tabs: Vec<ToolPanelTab> = BUILTIN_PANELS.iter().map(|panel| panel.tab).collect();
        tabs.extend(
            crate::plugin_ui::slots()
                .into_iter()
                .map(ToolPanelTab::Plugin),
        );
        let click_tabs = tabs.clone();
        // 停靠 rail 收起期间不保留高亮；全屏 rail 始终高亮当前 Tool Panel tab。
        let selected_index = (stage_rail || self.tool_panel_open)
            .then(|| tabs.iter().position(|t| *t == active_tab))
            .flatten();

        let tab = |t: ToolPanelTab, badge: usize| {
            let mut b = Tab::new()
                .label(t.label())
                // Tab 自己是交互控件，只拦住它的命中区域；不要再由包住整行的
                // 容器拦截，否则 Tab 后面的空白标题栏也收不到双击。
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
            if badge > 0 {
                b = b.suffix(
                    div()
                        .px(px(5.))
                        .rounded_full()
                        .bg(rgb(ui_theme::action_fill()))
                        .text_size(px(8.))
                        .font_semibold()
                        .text_color(rgb(ui_theme::action_on()))
                        .child(badge.to_string()),
                );
            }
            b
        };

        let mut tab_bar = TabBar::new(if stage_rail {
            "stage-tool-panel-rail"
        } else {
            "tool-panel-rail"
        })
        .underline()
        .with_size(gpui_component::Size::XSmall)
        .flex_1()
        .on_click(cx.listener(move |ws, ix: &usize, window, cx| {
            if let Some(tab) = click_tabs.get(*ix).copied() {
                ws.toggle_tool_panel_tab(tab, window, cx);
            }
        }));
        if let Some(ix) = selected_index {
            tab_bar = tab_bar.selected_index(ix);
        }
        for entry in &tabs {
            // 角标由面板自己声明，不在这里特判某个 tab。
            let badge = builtin_panel(*entry)
                .and_then(|panel| panel.badge)
                .map_or(0, |badge| badge(self, cx));
            tab_bar = tab_bar.child(tab(*entry, badge));
        }

        div()
            .w_full()
            .h(px(34.))
            .flex_none()
            .flex()
            .items_center()
            .pl_4()
            .border_b_1()
            .border_color(ui_theme::hairline())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(tab_bar),
            )
    }

    /// 面板统一头：36px，标题 + 自定义右侧内容。窗口开关在浮层，不在这里重复画。
    pub(crate) fn tool_panel_header(&self, title: &'static str, _cx: &mut Context<Self>) -> Div {
        div()
            .h(px(36.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .px_3()
            // 面板头不再刷一块 bg_bar：那是工具栏叠工具栏的旧层次。
            // 用弱分隔切开标题和内容即可。
            .border_b_1()
            .border_color(ui_theme::hairline())
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(ui_theme::text_muted()))
                    .child(title),
            )
    }

    /// 停靠右栏：tab 在抽屉顶，内容在下面。窗口顶栏不放这些 tab。
    pub(crate) fn render_tool_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        // 打开动画期间面板宽度逐帧变化，整个 body 每帧重建；Git 等内容构建
        // 成本高（改动树、diff），动画那 ~180ms 里逐帧全量重建会掉帧。打开
        // 动画期间先渲染轻量骨架占位，动画结束帧再挂真实内容；关闭动画保留
        // 真实内容，避免面板滑出前内容提前消失。
        let tabs = self.render_tool_panel_tabs(self.tool_panel_tab, false, cx);
        let opening = self.tool_panel_transition.is_opening();
        let body: AnyElement = if opening {
            div().flex_1().min_h_0().into_any_element()
        } else {
            self.render_tool_panel_content(self.tool_panel_tab, window, cx)
        };
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
        _left_guard: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tab = self
            .active_stage_tool_panel_tab()
            .unwrap_or(self.tool_panel_tab);
        v_flex()
            .flex_1()
            .min_h_0()
            .child(self.render_tool_panel_tabs(tab, true, cx))
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
            ToolPanelTab::Plugin(slot) => self.render_plugin_tab(slot, window, cx),
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
        // file_filter/search_results 已在 prepare_frame 懒创建 + 刷新）。
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
                // ensure_search 已在 prepare_frame 置位，通常到不了这里。
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
        // 顶部搜索框（file_filter 已在 prepare_frame 懒创建）。
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
        // 「技能」已迁成插件 tab：插件不可用（这里注册表为空）时降级成 Files，
        // 而不是让整份 UI 存档反序列化失败。
        let tab: ToolPanelTab = serde_json::from_str("\"skills\"").unwrap();
        assert_eq!(tab, ToolPanelTab::Files);
    }

    #[test]
    fn tool_panel_fullscreen_is_panel_level() {
        assert!(StageCover::ToolPanel.is_tool_panel_cover());
        assert!(StageCover::Files.is_tool_panel_cover());
        assert!(StageCover::Git.is_tool_panel_cover());
        assert!(StageCover::Skills.is_tool_panel_cover());
        assert!(StageCover::History.is_tool_panel_cover());
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
