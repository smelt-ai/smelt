//! inspector：面板内横向 tabs + 右侧面板（默认 344px，可整体隐藏）。
//! FILES / GIT / SKILL 三个 tab，点击切换或收合；面板头
//! 带「展开」把对应的旧全屏页盖到会话舞台上（stage_override），功能零删除。
//! （TASKS 已经升格成一级导航，见 session_list.rs 的「任务」入口，不再是这里的 tab。）
//!
//! 跟 file_tree.rs 同一个套路：`impl Workspace` 方法，字段仍在 main.rs。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::resizable::{h_resizable, resizable_panel};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::*;

use crate::{MainView, Workspace, ui_theme};

/// SKILLS 面板卡片的 hover group 名，同上一个套路（卡片 `.group()` + 操作条
/// `.group_hover()`）。
const SKILL_CARD_GROUP: &str = "insp-skill-card";

/// inspector 面板的四个 tab。
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InspectorTab {
    Files,
    Git,
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
            Self::Skills => "SKILL",
        }
    }

    /// 面板头「⤢ 展开」对应的舞台全宽视图。
    pub(crate) fn stage_view(self) -> Option<MainView> {
        match self {
            Self::Files => Some(MainView::Files),
            Self::Git => Some(MainView::Git),
            Self::Skills => Some(MainView::Skills),
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
            self.set_inspector_open(true);
            self.save_state(cx);
            cx.notify();
            return;
        }
        if self.inspector_tab == tab && self.inspector_open {
            self.set_inspector_open(false);
        } else {
            self.inspector_tab = tab;
            self.set_inspector_open(true);
        }
        self.save_state(cx);
        cx.notify();
    }

    /// inspector 面板顶部的横向 tabs，避免常驻 56px 竖轨挤占会话宽度。
    ///
    /// `corner_guard`：见 stage.rs::render_stage_header 同名参数——这条横条被
    /// Files/Git 展开态复用为舞台第一行时，sidebar 收起会让它变成贴着窗口左边
    /// 缘那块，真交通灯浮在它上面，需要在左边多让出交通灯宽度；平时停靠在右侧
    /// inspector 卡片里就永远不是最左边那块，传 `false`。
    pub(crate) fn render_inspector_rail(
        &mut self,
        corner_guard: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        // GIT 角标：当前项目改动文件数（读 git status 缓存，没有就不显示）。
        let git_changes = self
            .cur()
            .and_then(|s| s.cwd(cx))
            .and_then(|cwd| self.git_status.get(&cwd))
            .map(|(_, d)| d.files.len())
            .unwrap_or(0);

        const TABS: [InspectorTab; 3] = [
            InspectorTab::Files,
            InspectorTab::Git,
            InspectorTab::Skills,
        ];
        let cur = self.inspector_tab;
        let open = self.inspector_open;
        // 面板已展开且落在这个 tab 上才算「选中」——跟旧实现一致：收合时无高亮。
        let selected_index = open
            .then(|| TABS.iter().position(|t| *t == cur))
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

        div()
            .w_full()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            // tab 横条独立成 bg_bar 表面：跟下面的列表/面板体（bg_elev）拉开一档，
            // 读起来像一条工具栏而不是列表的第一行。
            .bg(rgb(ui_theme::bg_bar()))
            .border_b_1()
            .border_color(rgb(ui_theme::border_dim()))
            // 128px：同 stage.rs corner_guard 注释——要避开的不只是交通灯，
            // 还有 main.rs 顶部拖拽层里常驻绝对定位的「切换左侧栏」图标
            // （left 92px + 24px 宽）。
            .when(corner_guard, |d| d.pl(px(128.)))
            .child(
                {
                    let mut bar = TabBar::new("inspector-rail")
                        .underline()
                        .flex_1()
                        .on_click(cx.listener(move |ws, ix: &usize, window, cx| {
                            if let Some(tab) = TABS.get(*ix).copied() {
                                ws.toggle_inspector_tab(tab, window, cx);
                            }
                        }));
                    if let Some(ix) = selected_index {
                        bar = bar.selected_index(ix);
                    }
                    bar.child(tab(InspectorTab::Files, 0))
                        .child(tab(InspectorTab::Git, git_changes))
                        .child(tab(InspectorTab::Skills, 0))
                },
            )
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
        // 停靠 Inspector 永远在窗口右侧，不会碰到左上角交通灯。
        let tabs = self.render_inspector_rail(false, cx);
        let body: AnyElement = match self.inspector_tab {
            InspectorTab::Files => self.render_inspector_files(window, cx),
            InspectorTab::Git => self.render_inspector_git(window, cx),
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
