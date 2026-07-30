//! inspector：面板内横向 tabs + 右侧面板（默认 344px，可整体隐藏）。
//! FILES / GIT / TASKS / SKILL 四个 tab，点击切换或收合；面板头
//! 带「展开」把对应的旧全屏页盖到会话舞台上（stage_override），功能零删除。
//!
//! 跟 file_tree.rs 同一个套路：`impl Workspace` 方法，字段仍在 main.rs。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::*;

use crate::tasks::TaskStore;
use crate::{MainView, Workspace, ui_theme};

/// 侧栏任务卡片的 hover group 名：卡片 `.group()` + 操作条 `.group_hover()` 配对，
/// 鼠标移到卡片才显形「编辑 / 删除」。名字全卡共享，靠 DOM 祖先关系就近生效。
const TASK_CARD_GROUP: &str = "insp-task-card";

/// SKILLS 面板卡片的 hover group 名，同上一个套路（卡片 `.group()` + 操作条
/// `.group_hover()`）。
const SKILL_CARD_GROUP: &str = "insp-skill-card";

/// inspector 面板的四个 tab。
#[derive(Clone, Copy, PartialEq)]
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
            Self::Tasks => "TASKS",
            Self::Skills => "SKILL",
        }
    }

    /// 面板头「⤢ 展开」对应的舞台全宽视图；None = 头上不放展开按钮。
    /// Files → 「文件树 + 内容」双栏，Git → 「变更列表 + diff」双栏。
    fn stage_view(self) -> Option<MainView> {
        match self {
            Self::Files => Some(MainView::Files),
            Self::Git => Some(MainView::Git),
            Self::Tasks => Some(MainView::Tasks),
            Self::Skills => None,
        }
    }
}

impl Workspace {
    /// 当前 tab 是不是已经「提升到舞台」（⤢ 展开）。
    /// 提升后本体在舞台上，右侧就别再停靠一份——否则同一个文件树 / 变更列表
    /// 左右各渲染一遍，看着像出了两个面板。
    pub(crate) fn inspector_panel_promoted(&self) -> bool {
        self.stage_override.is_some() && self.inspector_tab.stage_view() == self.stage_override
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
            self.inspector_open = true;
            self.save_state(cx);
            return;
        }
        if self.inspector_panel_promoted() {
            // 展开态下点了别的 tab：FILES / GIT / TASKS 展开都跟横条共用一份停靠
            // UI（只是变宽，见 main.rs 舞台分派），能直接切过去、继续保持展开态；
            // 只有 SKILL 没有展开形态（stage_view() = None），这时才退回停靠。
            if let Some(view) = tab.stage_view() {
                self.set_stage_override(Some(view), window, cx);
                self.inspector_tab = tab;
                self.save_state(cx);
                cx.notify();
                return;
            }
            self.set_stage_override(None, window, cx);
            self.inspector_tab = tab;
            self.inspector_open = true;
            self.save_state(cx);
            cx.notify();
            return;
        }
        if self.inspector_tab == tab && self.inspector_open {
            self.inspector_open = false;
        } else {
            self.inspector_tab = tab;
            self.inspector_open = true;
        }
        self.save_state(cx);
        cx.notify();
    }

    /// inspector 面板顶部的横向 tabs，避免常驻 56px 竖轨挤占会话宽度。
    pub(crate) fn render_inspector_rail(&mut self, cx: &mut Context<Self>) -> Div {
        let this = cx.entity();
        // GIT 角标：当前项目改动文件数（读 git status 缓存，没有就不显示）。
        let git_changes = self
            .cur()
            .and_then(|s| s.cwd(cx))
            .and_then(|cwd| self.git_status.get(&cwd))
            .map(|(_, d)| d.files.len())
            .unwrap_or(0);

        let item = |tab: InspectorTab, badge: usize, active: bool, this: Entity<Workspace>| {
            div()
                .id(tab.label())
                .relative()
                .h_full()
                .px_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .map(|d| {
                    if active {
                        d.bg(ui_theme::tint(ui_theme::accent(), 0x14))
                            .border_b_2()
                            .border_color(rgb(ui_theme::accent()))
                    } else {
                        d.border_b_2()
                            .border_color(gpui::transparent_black())
                            .hover(|d| d.bg(rgb(ui_theme::bg_hover())))
                    }
                })
                .child(div().size(px(7.)).rounded_xs().bg(if active {
                    rgb(ui_theme::accent())
                } else {
                    rgb(ui_theme::border_focus())
                }))
                .child(
                    div()
                        .text_size(px(9.))
                        .font_semibold()
                        .text_color(if active {
                            rgb(ui_theme::text_bright())
                        } else {
                            rgb(ui_theme::text_faint())
                        })
                        .child(tab.label()),
                )
                .when(badge > 0, |d| {
                    d.child(
                        div()
                            .px(px(4.))
                            .rounded(px(8.))
                            .bg(rgb(ui_theme::accent()))
                            .text_size(px(8.))
                            .font_semibold()
                            .text_color(rgb(ui_theme::on_accent()))
                            .child(badge.to_string()),
                    )
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_ev, window, cx| {
                    this.update(cx, |ws, cx| ws.toggle_inspector_tab(tab, window, cx));
                })
        };

        let cur = self.inspector_tab;
        let open = self.inspector_open;
        div()
            .w_full()
            .h(px(34.))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .border_b_1()
            .border_color(rgb(ui_theme::border_dim()))
            .child(item(
                InspectorTab::Files,
                0,
                open && cur == InspectorTab::Files,
                this.clone(),
            ))
            .child(item(
                InspectorTab::Git,
                git_changes,
                open && cur == InspectorTab::Git,
                this.clone(),
            ))
            .child(item(
                InspectorTab::Tasks,
                0,
                open && cur == InspectorTab::Tasks,
                this.clone(),
            ))
            .child(item(
                InspectorTab::Skills,
                0,
                open && cur == InspectorTab::Skills,
                this.clone(),
            ))
            .child(div().flex_1())
            .children(open.then(|| cur.stage_view()).flatten().map(|view| {
                // 已经展开到舞台（这条横条本身正是展开态的头）→ 图标/文案/点击
                // 都换成「收起」，回到停靠态；没展开就是平时的「展开」。
                let promoted = self.inspector_panel_promoted();
                div()
                    .id("inspector-expand")
                    .mr_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size_6()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(rgb(ui_theme::text_faint()))
                    .hover(|d| {
                        d.bg(rgb(ui_theme::bg_hover()))
                            .text_color(rgb(ui_theme::text_mid()))
                    })
                    .child(
                        Icon::new(if promoted {
                            IconName::Minimize
                        } else {
                            IconName::Maximize
                        })
                        .size(px(13.)),
                    )
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(if promoted {
                            "收起面板"
                        } else {
                            "展开面板"
                        })
                        .build(window, cx)
                    })
                    .on_click(move |_ev, window, cx| {
                        this.update(cx, |ws, cx| {
                            if promoted {
                                ws.set_stage_override(None, window, cx);
                                ws.inspector_open = true;
                            } else {
                                ws.set_stage_override(Some(view), window, cx);
                            }
                        });
                    })
            }))
    }

    /// 面板统一头：36px，标题 + 自定义右侧内容。「展开」（盖到舞台）按钮已挪到
    /// 上面的 tab 横条右端，四个面板共用同一个图标按钮，不必每个面板头各摆一份。
    pub(crate) fn inspector_header(&self, title: &'static str, _cx: &mut Context<Self>) -> Div {
        div()
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
                    .child(title),
            )
    }

    /// 344px 面板本体：按当前 tab 分派。
    pub(crate) fn render_inspector_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let tabs = self.render_inspector_rail(cx);
        let body: AnyElement = match self.inspector_tab {
            InspectorTab::Files => self.render_inspector_files(window, cx),
            InspectorTab::Git => self.render_inspector_git(window, cx),
            InspectorTab::Tasks => self.render_inspector_tasks(cx),
            InspectorTab::Skills => self.render_inspector_skills(cx),
        };
        div()
            .w_full()
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .min_h_0()
            .bg(rgb(ui_theme::bg_elev()))
            .border_l_1()
            .border_color(rgb(ui_theme::border_dim()))
            .child(tabs)
            .child(body)
    }

    /// TASKS 面板：任务卡片列表（复用 TaskStore；卡片行动 = focus_or_run_task）。
    fn render_inspector_tasks(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity();
        let mut tasks = TaskStore::load().tasks;
        tasks.sort_by_key(|t| t.column.sidebar_rank());
        let count = tasks.len();

        let e_new = this.clone();
        let header = self.inspector_header("TASKS", cx).child(
            div()
                .id("inspector-task-new")
                .text_xs()
                .font_semibold()
                .text_color(rgb(ui_theme::accent()))
                .cursor_pointer()
                .hover(|d| d.opacity(0.8))
                .child(format!("+ 新建 · {count}"))
                .on_click(move |_ev, window, cx| {
                    e_new.update(cx, |ws, cx| ws.open_new_task_modal(window, cx));
                }),
        );

        let mut list = div()
            .id("inspector-task-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .p_2p5();
        if tasks.is_empty() {
            list = list.child(
                div()
                    .pt_8()
                    .flex()
                    .justify_center()
                    .text_sm()
                    .text_color(rgb(ui_theme::text_faint()))
                    .child("还没有任务"),
            );
        }
        for (tix, t) in tasks.into_iter().enumerate() {
            let done = t.column == crate::tasks::TaskColumn::Done;
            let color = rgb(t.column.color());
            let has_session = t.session_id.is_some();
            let action_label = if has_session {
                "打开 →"
            } else if t.column.is_todo() {
                "运行 →"
            } else {
                ""
            };
            let tid = t.id.clone();
            let e_act = this.clone();
            // 平时透明、鼠标移到卡片才显形的操作条（编辑 / 删除）。stop_propagation
            // 拦住 mouse_down，避免同时触发整卡的 focus_or_run。group 名见卡片 `.group()`。
            let e_edit = this.clone();
            let e_del = this.clone();
            let tid_edit = t.id.clone();
            let tid_del = t.id.clone();
            let hover_bar = div()
                .flex()
                .items_center()
                .gap_1()
                .flex_shrink_0()
                .opacity(0.0)
                .group_hover(TASK_CARD_GROUP, |s| s.opacity(1.0))
                .child(
                    div()
                        .id(("inspector-task-edit", tix))
                        .px_1()
                        .text_xs()
                        .cursor_pointer()
                        .text_color(rgb(ui_theme::text_faint()))
                        .hover(|s| s.text_color(rgb(ui_theme::accent())))
                        .child("编辑")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_ev, window, cx| {
                            let tid = tid_edit.clone();
                            e_edit.update(cx, |ws, cx| ws.open_edit_task_modal(&tid, window, cx));
                        }),
                )
                .child(
                    div()
                        .id(("inspector-task-del", tix))
                        .px_1()
                        .text_xs()
                        .cursor_pointer()
                        .text_color(rgb(ui_theme::text_faint()))
                        .hover(|s| s.text_color(rgb(ui_theme::red())))
                        .child("删除")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_ev, _window, cx| {
                            let tid = tid_del.clone();
                            e_del.update(cx, |ws, cx| ws.delete_task(&tid, cx));
                        }),
                );
            // 结构：外层横排 = 左侧 3px 状态色竖条 + 内容列（GPUI 边框色是单值，
            // 左边框异色做不到，用嵌套竖条实现设计稿的左色条）。
            let card = div()
                .id(("inspector-task", tix))
                .group(TASK_CARD_GROUP)
                .rounded(px(9.))
                .border_1()
                .border_color(rgb(ui_theme::border_mid()))
                .bg(if done {
                    rgb(ui_theme::bg_panel())
                } else {
                    rgb(ui_theme::bg_card())
                })
                .when(done, |d| d.opacity(0.55))
                .overflow_hidden()
                .flex()
                .cursor_pointer()
                .child(div().w(px(3.)).flex_shrink_0().bg(color))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .font_semibold()
                                        .text_color(rgb(ui_theme::text_bright()))
                                        .child(if t.title.is_empty() {
                                            "（未命名任务）".to_string()
                                        } else {
                                            t.title.clone()
                                        }),
                                )
                                .child(hover_bar),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().size(px(6.)).rounded_full().bg(color))
                                .child(div().text_xs().text_color(color).child(t.column.label()))
                                .child(div().flex_1())
                                .when(!action_label.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .text_xs()
                                            .font_semibold()
                                            .text_color(if has_session {
                                                rgb(ui_theme::purple())
                                            } else {
                                                rgb(ui_theme::green())
                                            })
                                            .child(action_label),
                                    )
                                }),
                        ),
                )
                .on_click(move |_ev, window, cx| {
                    let tid = tid.clone();
                    e_act.update(cx, |ws, cx| ws.focus_or_run_task(&tid, window, cx));
                });
            list = list.child(card);
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
        let has_open_file = self.open_file.is_some();
        // 打开文件后的目录宽度随 inspector 一起缩放，避免外层面板较窄时
        // “详情最小宽度 + 固定目录宽度”超过容器并向右溢出。
        let tree_w = if has_open_file {
            (self.inspector_w * 0.36).clamp(120., 240.)
        } else {
            344.
        };
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
        let body: AnyElement = if has_open_file {
            let content = crate::file_tree::file_content_pane(&self.open_file, &roots, cx);
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_hidden()
                .child(
                    h_resizable("inspector-files-split")
                        .child(
                            resizable_panel()
                                .size_range(px(140.)..Pixels::MAX)
                                .min_w_0()
                                .min_h_0()
                                .flex()
                                .child(content),
                        )
                        .child(
                            resizable_panel()
                                .size(px(tree_w))
                                .size_range(px(120.)..Pixels::MAX)
                                .flex_none()
                                .min_w_0()
                                .min_h_0()
                                .flex()
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
                        ),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(tree)
                .into_any_element()
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(body)
            .into_any_element()
    }

    /// GIT 面板：窄版 SOURCE CONTROL（实现见 git_panel.rs 的 git_narrow_panel，
    /// 需要访问 GitStatusData / DiffLine 的模块内私有字段）。
    fn render_inspector_git(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.git_narrow_panel(window, cx)
    }

    /// SKILLS 面板：列出用户级 / 项目级 skill（`~/.claude/skills`、
    /// `<项目>/.claude/skills`），点一条把 `/name` 填进当前会话。
    ///
    /// 不做「启用/停用」开关：Claude Code 侧没有对应机制（settings.json 的
    /// `enabledPlugins` 管插件不管 skill），拨了不生效的开关比没有更糟。
    fn render_inspector_skills(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
                                        .child("应用到其他工具")
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
                            .rounded(px(8.))
                            .border_1()
                            .border_color(rgb(ui_theme::border_mid()))
                            .bg(rgb(ui_theme::bg_card()))
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
                                    .when(!sk.managed, |d| {
                                        // 非托管 skill：不用笼统的「旧」，直接
                                        // 标出它实际躺在哪个 agent 的目录里，
                                        // 免得用户看着一堆卡片分不清归属。
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
