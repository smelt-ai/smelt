//! Git 面板页面：确认弹窗、窄版 SOURCE CONTROL、日志页。
//!
//! 只组 UI。写缓存、跑 git、改 Workspace 字段的方法留在 `workspace.rs`，
//! 由按钮 listener 调用。

use super::*;

impl Workspace {
    /// 「删除 Worktree」确认弹窗：dirty 探测完之前（None）按钮禁用显示"检查中…"；
    /// 探测出有未提交改动就红字警告 + 按钮仍可点（--force 由确认后的调用方处理）。
    /// 视觉同 render_daemon_restart_confirm 一套（红色危险操作配色）。
    pub fn render_delete_worktree_confirm(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        let Some(target) = self.delete_worktree_target.as_ref() else {
            return div();
        };

        let (body_text, warn) = match target.dirty {
            None => ("正在检查有没有未提交的更改…".to_string(), false),
            Some(true) => (
                format!(
                    "分支「{}」的这个 worktree 还有未提交的更改，删除后会永久丢失，且其下所有终端会话都会被关闭。",
                    target.branch
                ),
                true,
            ),
            Some(false) => (
                format!(
                    "删除分支「{}」的这个 worktree，其下所有终端会话都会被关闭。",
                    target.branch
                ),
                false,
            ),
        };
        let ready = target.dirty.is_some();
        let fg = cx.theme().foreground;

        let content = v_flex()
            .child(Self::modal_title(fg, "确定删除这个 Worktree 吗？"))
            .child(
                div()
                    .text_sm()
                    .text_color(if warn { accent_text } else { muted })
                    .child(body_text),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-delete-worktree",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_delete_worktree(cx),
                        cx,
                    ))
                    .child(
                        Self::modal_button_base(
                            "confirm-delete-worktree",
                            if ready {
                                "确定删除"
                            } else {
                                "检查中…"
                            },
                            tint,
                            accent_text,
                        )
                        .when(ready, |el| {
                            el.cursor_pointer()
                                .hover(move |s| s.bg(hover))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_delete_worktree(cx);
                                }))
                        }),
                    ),
            );
        Self::modal_shell(360., true, content, cx)
    }

    /// 「关联 Worktree」弹窗：列出这个仓库的所有 worktree。主工作树不可删；其余
    /// 条目「删除」走 [`Self::start_delete_worktree`] 的确认流程（确认弹窗叠在本层
    /// 之上）；已失效（prunable）条目只能「清理」（`git worktree prune`）；底部另有
    /// 「清理失效项」把失效记录一次性清掉并刷新列表。
    pub fn render_worktree_list(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let (neutral_bg, neutral_hover, _, _, _) = Self::modal_accent_colors(true);
        let Some(state) = self.worktree_list.as_ref() else {
            return div();
        };

        let repo_name = Path::new(&state.root)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(state.root.as_str())
            .to_string();

        let body: AnyElement = if let Some(error) = state.error.as_ref() {
            div()
                .text_sm()
                .text_color(rgb(ui_theme::red()))
                .child(error.clone())
                .into_any_element()
        } else if let Some(entries) = state.entries.as_ref() {
            if entries.is_empty() {
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("这个仓库没有可显示的 worktree。")
                    .into_any_element()
            } else {
                let mut rows = v_flex()
                    .id("worktree-list")
                    .gap_1()
                    .max_h(px(380.))
                    .overflow_y_scroll();
                for (i, entry) in entries.iter().enumerate() {
                    let path = entry.path.clone();
                    let branch = entry.branch.clone();
                    let main_root = state.main_root.clone();
                    let is_main = entry.is_main;
                    let prunable = entry.prunable;
                    let locked = entry.locked;
                    // 徽标：主工作树 / 已失效 / 已锁定，普通条目显示分支名。
                    // 用拥有所有权的 String，避免借用 branch 导致与下方删除按钮的
                    // move 冲突（badge 要活过整轮渲染，删除按钮闭包也要拿 branch）。
                    let badge = if is_main {
                        Some(("主工作树".to_string(), rgb(ui_theme::accent())))
                    } else if prunable {
                        Some(("已失效".to_string(), rgb(ui_theme::red())))
                    } else if locked {
                        Some(("已锁定".to_string(), rgb(ui_theme::yellow())))
                    } else {
                        branch.clone().map(|b| (b, rgb(ui_theme::text_faint())))
                    };
                    let row = h_flex()
                        .gap_2()
                        .items_center()
                        .justify_between()
                        .py_1()
                        .px_2()
                        .rounded(ui_theme::row_radius())
                        .hover(|d| d.bg(rgb(ui_theme::bg_row_hover())))
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .min_w_0()
                                .flex_1()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(12.))
                                        .font_family(crate::terminal_view::font_family())
                                        .text_color(if is_main { fg } else { muted })
                                        .child(path.clone()),
                                )
                                .children(badge.map(|(text, color)| {
                                    div()
                                        .flex_shrink_0()
                                        .px(px(4.))
                                        .py(px(1.))
                                        .rounded(px(4.))
                                        .bg(ui_theme::overlay(0x14))
                                        .text_size(px(10.))
                                        .text_color(color)
                                        .child(text)
                                })),
                        )
                        .child(
                            // 操作列：主工作树不可删；已失效只能清理；其余走删除确认。
                            // 三分支类型不同，统一收成 AnyElement。
                            if is_main {
                                div().size(px(0.)).into_any_element()
                            } else if prunable {
                                Button::new(("wt-prune", i))
                                    .small()
                                    .label("清理")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.prune_stale_worktrees(cx);
                                    }))
                                    .into_any_element()
                            } else {
                                Button::new(("wt-del", i))
                                    .small()
                                    .label("删除")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let branch_label = branch
                                            .clone()
                                            .unwrap_or_else(|| "detached HEAD".to_string());
                                        this.start_delete_worktree(
                                            path.clone(),
                                            main_root.clone(),
                                            branch_label,
                                            cx,
                                        );
                                    }))
                                    .into_any_element()
                            },
                        );
                    rows = rows.child(row);
                }
                rows.into_any_element()
            }
        } else {
            div()
                .text_sm()
                .text_color(muted)
                .child("正在列出 worktree…")
                .into_any_element()
        };

        let content = v_flex()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Icon::new(IconName::HardDrive).size(px(15.)).text_color(fg))
                            .child(Self::modal_title(fg, format!("Worktree · {repo_name}"))),
                    )
                    .child(
                        Button::new("wt-close-x")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_worktree_list(cx);
                            })),
                    ),
            )
            .child(body)
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        Button::new("wt-prune-all")
                            .small()
                            .label("清理失效项")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.prune_stale_worktrees(cx);
                            })),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new("wt-refresh")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Redo)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.refresh_worktree_list(cx);
                                    })),
                            )
                            .child(Self::modal_button(
                                "wt-close",
                                "关闭",
                                neutral_bg,
                                neutral_hover,
                                fg,
                                |this, _, _, cx| this.close_worktree_list(cx),
                                cx,
                            )),
                    ),
            );

        Self::modal_shell(560., true, content, cx)
    }

    /// 「新建 Worktree」弹窗：分支名（留空 = detached）+ 检出目录两个输入框，
    /// 底部取消/创建。创建中按钮置灰；失败红字显示在输入框下方。
    pub fn render_new_worktree(&self, cx: &mut Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            Self::modal_accent_colors(false);
        let Some(state) = self.new_worktree.as_ref() else {
            return div();
        };

        let base_hint = if state.base_branch.is_empty() {
            format!("基于 {} 当前 HEAD 创建", state.main_root)
        } else {
            format!("基于 {} 的「{}」分支", state.main_root, state.base_branch)
        };

        let content = v_flex()
            .child(
                h_flex()
                    .items_center()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Icon::new(IconName::HardDrive).size(px(15.)).text_color(fg))
                            .child(Self::modal_title(fg, "新建 Worktree")),
                    )
                    .child(
                        Button::new("nwt-close-x")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cancel_new_worktree(cx);
                            })),
                    ),
            )
            .child(div().text_sm().text_color(muted).child(base_hint))
            .child(div().text_xs().text_color(muted).child("分支名"))
            .child(Input::new(&state.branch_input).small())
            .child(div().text_xs().text_color(muted).child("检出目录"))
            .child(Input::new(&state.path_input).small())
            .children(state.error.as_ref().map(|err| {
                div()
                    .text_sm()
                    .text_color(rgb(ui_theme::red()))
                    .child(err.clone())
            }))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-new-worktree",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_new_worktree(cx),
                        cx,
                    ))
                    .child(
                        Self::modal_button_base(
                            "confirm-new-worktree",
                            if state.busy { "创建中…" } else { "创建" },
                            tint,
                            accent_text,
                        )
                        .when(!state.busy, |el| {
                            el.cursor_pointer()
                                .hover(move |s| s.bg(hover))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_new_worktree(cx);
                                }))
                        }),
                    ),
            );

        Self::modal_shell(420., false, content, cx)
    }

    /// 「丢弃文件改动」确认弹窗。未跟踪文件是直接删盘，措辞要比 restore 更重。
    pub fn render_discard_file_confirm(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        let Some((_, path, untracked)) = self.discard_file_target.as_ref() else {
            return div();
        };
        let untracked = *untracked;
        let (title, body) = if untracked {
            (
                "确定删除这个新文件吗？",
                format!("{path} 从未被 git 跟踪过，删了就是彻底删除。"),
            )
        } else {
            (
                "确定丢弃这个文件的更改吗？",
                format!("{path} 会被还原成 HEAD 的样子，已暂存的部分一并还原。"),
            )
        };

        let content = v_flex()
            .child(Self::modal_title(fg, title))
            .child(div().text_sm().text_color(muted).child(body))
            .child(
                div()
                    .text_sm()
                    .text_color(accent_text)
                    .child("不进 reflog，找不回来。"),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-discard-file",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_discard_file(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-discard-file",
                        if untracked {
                            "删除文件"
                        } else {
                            "丢弃更改"
                        },
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_discard_file(cx),
                        cx,
                    )),
            );
        Self::modal_shell(380., true, content, cx)
    }

    /// 「丢弃这一块」确认弹窗。用危险配色，文案点明不可恢复——这个操作直接覆写
    /// 工作区文件，既不进索引也不进 reflog，点完就真没了。
    pub fn render_discard_hunk_confirm(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        let Some((_, idx)) = self.discard_hunk_target.as_ref() else {
            return div();
        };
        let file = self
            .git_diff
            .as_ref()
            .map(|d| d.path.clone())
            .unwrap_or_default();

        let content = v_flex()
            .child(Self::modal_title(fg, "确定丢弃这一块更改吗？"))
            .child(div().text_sm().text_color(muted).child(format!(
                "{file} 的第 {} 块更改会被还原成更改前的样子。",
                idx + 1
            )))
            .child(
                div()
                    .text_sm()
                    .text_color(accent_text)
                    .child("直接改工作区文件，不进暂存区也不进 reflog——丢了就找不回来。"),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-discard-hunk",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_discard_hunk(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-discard-hunk",
                        "丢弃这一块",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_discard_hunk(cx),
                        cx,
                    )),
            );
        Self::modal_shell(380., true, content, cx)
    }

    /// 「删除分支」确认弹窗。
    pub fn render_delete_branch_confirm(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        let Some((_, branch, remote)) = self.delete_branch_target.as_ref() else {
            return div();
        };
        let remote = *remote;

        let content = v_flex()
            .child(Self::modal_title(
                fg,
                if remote {
                    "确定删除这个远端分支吗？"
                } else {
                    "确定删除这个分支吗？"
                },
            ))
            .child(div().text_sm().text_color(muted).child(if remote {
                format!("{branch} 会从远端仓库删除，其他人拉取后本地也会跟着消失。")
            } else {
                format!("{branch} 只删本地引用，工作区文件不受影响。")
            }))
            .child(div().text_sm().text_color(accent_text).child(if remote {
                "影响所有协作者，删之前确认没人在用。"
            } else {
                "有未合并的提交时 git 会拒绝删除，不会强删。"
            }))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-delete-branch",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_delete_branch(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-delete-branch",
                        "删除",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_delete_branch(cx),
                        cx,
                    )),
            );
        Self::modal_shell(380., true, content, cx)
    }

    /// 窗口级 mouse-up 兜底：diff 行被虚拟列表重建、指针移到正文或空白区时，元素级
    /// mouse-up 都可能收不到。窗口事件不依赖命中元素，单击后不会留下悬挂的拖拽态。
    fn diff_selection_listener(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        canvas(
            |_, _, _| {},
            move |_bounds, _, window, _cx| {
                let finish_view = view;
                window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                    if !phase.bubble() {
                        return;
                    }
                    finish_view.update(cx, |this, cx| {
                        this.finish_diff_selection(window, cx);
                    });
                });
            },
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }

    /// 「丢弃全部改动」确认弹窗（照 render_discard_file_confirm，措辞更重——连未
    /// 跟踪文件一起删）。
    pub fn render_discard_all_confirm(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        if self.discard_all_target.is_none() {
            return div();
        }
        let content = v_flex()
            .child(Self::modal_title(fg, "丢弃工作区全部更改？"))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("已跟踪文件还原成 HEAD，未暂存 / 未跟踪的新文件会被删除。"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(accent_text)
                    .child("不进 reflog，找不回来。"),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-discard-all",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_discard_all(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-discard-all",
                        "全部丢弃",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_discard_all(cx),
                        cx,
                    )),
            );
        Self::modal_shell(400., true, content, cx)
    }

    /// Tool Panel 的窄版 GIT 面板：变更（STAGED / CHANGES）+ 操作
    ///（提交/推送）。打开全部改动后切到 DIFF 预览子视图
    ///（聚合行 + 文件级暂存按钮）。
    pub(crate) fn git_narrow_panel(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> AnyElement {
        use crate::ui_theme;

        let Some(root) = self.active_project_root(cx) else {
            let header = self.tool_panel_header("变更", cx);
            return v_flex()
                .flex_1()
                .min_h_0()
                .child(header)
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(
                            div()
                                .text_lg()
                                .font_semibold()
                                .text_color(rgb(ui_theme::text_bright()))
                                .child("没有项目"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(ui_theme::text_faint()))
                                .child("打开一个项目后再看更改"),
                        ),
                )
                .into_any_element();
        };

        // ---- 列表 ----
        // 头部（分支、↑N↓M、stash、同步菜单）展示的是**当前提交目标仓库**的状态，
        // 不是项目根的。否则在子仓上提交时，头部会显示另一个仓库的分支与领先/落后。
        let active_repo = self.active_git_repo_root(&root);
        let status = self.git_status.get(&active_repo).map(|(_, d)| d.clone());
        let status_refreshing = self.git_status_inflight.contains(&active_repo);
        let status_failure_count = self
            .git_status_failures
            .get(&active_repo)
            .copied()
            .unwrap_or_default();
        let (ahead, behind) = status
            .as_ref()
            .map(|d| (d.ahead, d.behind))
            .unwrap_or((0, 0));
        let stash_n = status.as_ref().map(|d| d.stash_count).unwrap_or(0);
        let has_changes = status.as_ref().is_some_and(|d| !d.files.is_empty());
        let ws_ops = cx.entity();
        let root_ops = root.clone();
        // 进行中反馈：点了拉取/获取几秒内没动静会以为没反应，op 跑着时按钮直接显示
        // 「拉取中…」。
        let busy = self.git_op;
        // 同步入口 = 把本来就显眼的绿色 ↑N↓M 直接做成可点下拉（带 ▾ 提示），
        // 替代之前那个不起眼的「⇅」。点开是获取/拉取/暂存/恢复/丢弃全部。
        let sync_label = match busy {
            Some(op) => format!("{op}中…"),
            None => format!("↑{ahead} ↓{behind} ▾"),
        };
        let sync_color = if busy.is_some() {
            rgb(ui_theme::yellow())
        } else if ahead + behind > 0 {
            rgb(ui_theme::green())
        } else {
            rgb(ui_theme::text_faint())
        };
        let (tab_fg, tab_muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let tab_view = cx.entity();
        // 仓库集合要在头部之前算好：历史页的仓库选择器和改动页的分组都用它。
        let repo_set = self.git_repos.get(&root).map(|(_, set)| set.clone());
        let repos: Vec<smelt_git::discovery::DiscoveredRepo> = match &repo_set {
            Some(set) if !set.repos.is_empty() => set.repos.clone(),
            // 发现还没回来：先当单仓渲染，首帧不至于空白。
            _ => vec![smelt_git::discovery::DiscoveredRepo {
                root: std::path::PathBuf::from(&root),
                rel_path: String::new(),
                kind: smelt_git::discovery::RepoKind::Root,
            }],
        };
        let show_repo_headers = repos.len() > 1;
        // 仓库选择器的选项：(显示名, 仓库根)。
        let repo_label_of = |repo: &smelt_git::discovery::DiscoveredRepo| -> String {
            if repo.rel_path.is_empty() {
                std::path::Path::new(&root)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| root.clone())
            } else {
                repo.rel_path.clone()
            }
        };
        let repo_choices: Vec<(String, String)> = repos
            .iter()
            .map(|repo| (repo_label_of(repo), repo.root.to_string_lossy().to_string()))
            .collect();
        let active_repo_label = repos
            .iter()
            .find(|repo| repo.root.to_string_lossy() == active_repo)
            .map(repo_label_of)
            .unwrap_or_else(|| active_repo.clone());
        let ws_log_repo = cx.entity();

        let view_tabs = h_flex().gap(px(2.)).children(
            [(GitTab::Changes, "更改"), (GitTab::Log, "历史")]
                .into_iter()
                .enumerate()
                .map(|(ix, (tab, label))| {
                    let selected = self.git_tab == tab;
                    let view = tab_view.clone();
                    div()
                        .id(("git-view", ix))
                        .px_2p5()
                        .py(px(4.))
                        .rounded_full()
                        .text_xs()
                        .cursor_pointer()
                        .text_color(if selected { tab_fg } else { tab_muted })
                        .when(selected, |d| d.bg(rgb(ui_theme::bg_hover())))
                        .hover(|d| d.bg(rgb(ui_theme::bg_hover())))
                        .child(label)
                        .on_click(move |_event, _window, cx| {
                            view.update(cx, |workspace, cx| {
                                if workspace.git_tab != tab {
                                    workspace.git_tab = tab;
                                    if tab == GitTab::Changes {
                                        workspace.reset_git_diff_view();
                                    }
                                    cx.notify();
                                }
                            });
                        })
                }),
        );
        let scope_view = cx.entity();
        let scope_control = Button::new("git-diff-scope")
            .ghost()
            .xsmall()
            .label(format!("{} ▾", self.diff_scope.label()))
            .text_color(rgb(ui_theme::text_muted()))
            .dropdown_menu(move |mut menu, _window, _cx| {
                for scope in [DiffScope::All, DiffScope::Staged, DiffScope::Unstaged] {
                    let view = scope_view.clone();
                    menu = menu.item(PopupMenuItem::new(scope.label()).on_click(
                        move |_event, _window, cx| {
                            view.update(cx, |workspace, cx| {
                                workspace.set_diff_scope(scope, cx);
                            });
                        },
                    ));
                }
                menu
            });
        // 改动 / 历史是长期可见的同级视图，不属于操作菜单；与标题合并到同一行，
        // 避免旧的第二条 tab 栏挤压本就有限的 Git 内容高度。
        let header = div()
            .h(px(36.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .border_b_1()
            .border_color(ui_theme::hairline())
            .child(view_tabs)
            .when(self.git_tab == GitTab::Changes, |bar| {
                bar.child(scope_control)
            })
            // 历史是"某个仓库的历史"。多仓工作区里不写明是谁的，看到的提交
            // 就完全没法对应到项目里的哪一块。
            .when(self.git_tab == GitTab::Log && repo_choices.len() > 1, |bar| {
                bar.child(
                    Button::new("git-log-repo")
                        .ghost()
                        .xsmall()
                        .label(format!("{active_repo_label} ▾"))
                        .text_color(rgb(ui_theme::text_muted()))
                        .dropdown_menu(move |mut menu, _window, _cx| {
                            for (label, repo_root) in repo_choices.clone() {
                                let ws = ws_log_repo.clone();
                                menu = menu.item(PopupMenuItem::new(label).on_click(
                                    move |_ev, _window, cx| {
                                        let root = repo_root.clone();
                                        ws.update(cx, |workspace, cx| {
                                            workspace.set_active_git_repo(root, cx);
                                        });
                                    },
                                ));
                            }
                            menu
                        }),
                )
            })
            .child(div().flex_1())
            // 历史页还没有分仓概念，仍用当前目标仓库的操作菜单；改动页的操作
            // 已经下放到每个仓库自己那一行，这里不再摆一个"作用于谁不明确"的全局按钮。
            .when(self.git_tab == GitTab::Log, |bar| {
                bar.child(
                    Button::new("git-sync-menu")
                        .ghost()
                        .xsmall()
                        .label(sync_label)
                        .font_family(crate::terminal_view::font_family())
                        .text_color(sync_color)
                        .dropdown_menu(move |menu, _window, _cx| {
                            git_ops_menu_items(
                                menu,
                                ws_ops.clone(),
                                root_ops.clone(),
                                ahead,
                                behind,
                                stash_n,
                                has_changes,
                            )
                        }),
                )
            });

        if self.git_tab == GitTab::Log {
            return v_flex()
                .flex_1()
                .min_h_0()
                .child(header)
                .child(self.render_git_log_tab(active_repo, cx))
                .into_any_element();
        }

        // 多仓工作区：逐仓拿自己的 status，各自分组。文件不再被摊平成“项目根的文件”，
        // 因此暂存和提交总是发生在同一个仓库里。

        let ws = cx.entity();
        // 一组改动：复用全屏页的 build_git_tree（JetBrains 式目录树 + 单链压缩），
        // 折叠状态也共用同一份 git_tree_collapsed，两处视图折叠同步。
        // 目录行 = caret + 目录名（淡）；文件行 = 前缀符号（staged「+」绿 /
        // 已跟踪改动「~」橙 / 未跟踪「?」绿）+ 文件名 + 右侧状态字母。
        let collapsed = self.git_tree_collapsed.clone();
        // uniform_list 的渲染闭包必须持有数据快照。按仓库嵌套投影，行渲染时先用 &str
        // 取内层 map 再查 path，不用每行拼 tuple key 分配两个 String。
        let mut index_pending: HashMap<String, HashMap<String, PendingGitIndexOp>> = HashMap::new();
        for ((pending_root, path), pending) in &self.git_index_pending {
            index_pending
                .entry(pending_root.clone())
                .or_default()
                .insert(path.clone(), *pending);
        }

        // 变更行统一高度，交给 GPUI 的 uniform_list 只构造可视区附近的元素。
        // 旧实现把每个文件的 Checkbox、右键菜单和文本树一次性挂进 flex 树，176
        // 个文件时会让 Taffy 在每帧重排整棵树。
        // 哪些改动条目其实是子模块指针。
        //
        // 发现器只解析了项目根的 `.gitmodules`，所以这份路径集合是**相对项目根**的，
        // 只能用来标注项目根自己的 status；拿去匹配子仓内的文件会张冠李戴。
        let root_gitlinks: HashSet<String> = repos
            .iter()
            .filter(|r| matches!(r.kind, smelt_git::discovery::RepoKind::Submodule))
            .map(|r| r.rel_path.clone())
            .collect();
        let no_gitlinks: HashSet<String> = HashSet::new();

        let mut narrow_rows = Vec::new();
        let mut total_changes = 0usize;
        // 有没有任何值得让列表存在的事：改动、待推送、待拉取、压着的 stash。
        let mut anything_to_do = false;
        for repo in &repos {
            let repo_root = repo.root.to_string_lossy().to_string();
            let repo_status = self.git_status.get(&repo_root).map(|(_, d)| d);
            let (staged, changed) = match repo_status {
                Some(d) => split_staged_and_changed(&d.files),
                None => (Vec::new(), Vec::new()),
            };
            total_changes += staged.len() + changed.len();

            // 干净的仓库不占列表空间，但当前提交目标除外——否则用户看不到
            // 自己正在往哪个仓库提交。
            let is_active = repo_root == active_repo;
            let has_changes = !staged.is_empty() || !changed.is_empty();
            anything_to_do |= repo_status.is_some_and(|d| {
                repo_needs_attention(has_changes, d.ahead, d.behind, d.stash_count)
            });

            let repo_root: Rc<str> = Rc::from(repo_root.as_str());
            let repo_collapsed = self.git_repo_collapsed.contains(repo_root.as_ref());
            if show_repo_headers {
                narrow_rows.push(NarrowGitRow::RepoHeader {
                    root: repo_root.clone(),
                    label: if repo.rel_path.is_empty() {
                        std::path::Path::new(&root)
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| root.clone())
                    } else {
                        repo.rel_path.clone()
                    },
                    branch: repo_status
                        .map(|d| d.branch.clone())
                        .unwrap_or_else(|| "…".to_string()),
                    kind: repo.kind,
                    active: is_active,
                    collapsed: repo_collapsed,
                    ahead: repo_status.map(|d| d.ahead).unwrap_or(0),
                    behind: repo_status.map(|d| d.behind).unwrap_or(0),
                    stash_n: repo_status.map(|d| d.stash_count).unwrap_or(0),
                    has_changes,
                });
            }
            // 折起来的仓库只留标题行。
            if show_repo_headers && repo_collapsed {
                continue;
            }
            // 干净的仓库不摆提交区——没东西可提交。但仓库行本身一定留着：
            // 「没有改动」不等于「没事可做」，本地攒着待推送的提交、远端有待拉取的
            // 提交、栈里压着 stash，都只能从那一行的操作菜单进去。
            // 之前整段跳过干净仓库，结果提交完最后一笔改动，仓库连同推送入口一起消失。
            let show_commit_box = has_changes || is_active;
            if let Some(input) = self
                .commit_msg_inputs
                .get(repo_root.as_ref())
                .filter(|_| show_commit_box)
            {
                narrow_rows.push(NarrowGitRow::CommitBox {
                    root: repo_root.clone(),
                    input: input.clone(),
                    branch: repo_status.map(|d| d.branch.clone()).unwrap_or_default(),
                    has_text: !input.read(cx).value().trim().is_empty(),
                    has_staged: !staged.is_empty(),
                    pushing: self.pushing,
                    ahead: repo_status.map(|d| d.ahead).unwrap_or(0),
                    error: self.commit_errors.get(repo_root.as_ref()).cloned(),
                    depth: usize::from(show_repo_headers),
                });
            }
            // 多仓时文件比仓库行缩进一级，层级才看得出来；单仓不白白浪费横向空间。
            let depth = usize::from(show_repo_headers);
            let gitlinks = if repo.rel_path.is_empty() {
                &root_gitlinks
            } else {
                &no_gitlinks
            };
            if !staged.is_empty() {
                append_narrow_git_group(
                    &mut narrow_rows,
                    NarrowGitGroup {
                        title: format!("暂存的更改 · {}", staged.len()),
                        files: &staged,
                        key: "narrow-staged",
                        is_staged_group: true,
                        root: repo_root.clone(),
                        depth,
                        gitlinks,
                    },
                    &collapsed,
                );
            }
            if !changed.is_empty() {
                append_narrow_git_group(
                    &mut narrow_rows,
                    NarrowGitGroup {
                        title: format!("更改 · {}", changed.len()),
                        files: &changed,
                        key: "narrow-changed",
                        is_staged_group: false,
                        root: repo_root.clone(),
                        depth,
                        gitlinks,
                    },
                    &collapsed,
                );
            }
        }
        // 真的无事可做时才当空列表处理（显示"工作区干净"）。只看改动数是不够的：
        // 仓库可能干净但攒着待推送的提交，那一行的操作菜单是唯一的推送入口。
        if total_changes == 0 && !anything_to_do {
            narrow_rows.clear();
        }
        let list: AnyElement = if narrow_rows.is_empty() {
            let message =
                git_empty_list_message(status.as_ref(), status_refreshing, status_failure_count);
            div()
                .id("narrow-git-list-empty")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .text_sm()
                .text_color(rgb(ui_theme::text_faint()))
                .child(
                    div()
                        .text_lg()
                        .font_semibold()
                        .text_color(rgb(ui_theme::text_bright()))
                        .child(if message == "工作区干净" {
                            "工作区干净"
                        } else {
                            message
                        }),
                )
                .when(message == "工作区干净", |col| {
                    col.child(
                        div()
                            .text_xs()
                            .text_color(rgb(ui_theme::text_faint()))
                            .child("没有要提交的更改"),
                    )
                })
                .into_any_element()
        } else {
            // 列表里混着仓库行、内联提交区和文件行，高度不一，用不了等高的
            // uniform_list。改成常规滚动容器 + 每仓渲染上限：超大仓库不至于把
            // 整棵布局树拖垮，而上限本身也比"静默卡顿"诚实。
            let ws_for_rows = ws;
            let collapsed_for_rows = collapsed;
            v_flex()
                .id("narrow-git-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.git_list_scroll)
                .children(narrow_rows.iter().enumerate().map(|(ix, row)| {
                    render_narrow_git_row(
                        row,
                        ix,
                        &ws_for_rows,
                        &collapsed_for_rows,
                        &index_pending,
                    )
                }))
                .into_any_element()
        };
        // 发现被上限截断时必须说清楚。默不作声会让用户以为没列出来的仓库
        // 没改动，而事实是根本没看。
        let list: AnyElement = if repo_set.as_ref().is_some_and(|set| set.truncated) {
            v_flex()
                .flex_1()
                .min_h_0()
                .child(list)
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .text_size(px(10.))
                        .text_color(rgb(ui_theme::text_faint()))
                        .child("仓库太多，只列出了一部分"),
                )
                .into_any_element()
        } else {
            list
        };

        let list_and_commit = v_flex()
            .flex_1()
            .min_h_0()
            .pt_2()
            .child(
                div()
                    .px_3()
                    .pb_1()
                    .text_size(px(10.))
                    .font_semibold()
                    .text_color(rgb(ui_theme::text_faint()))
                    .child("变更"),
            )
            .child(list);
        // 有打开的 diff：内嵌显示（diff 在左占主宽度，改动列表 + commit 框收窄到
        // 右边），跟 Files 面板「内容左、树右」同一个视觉套路——点了改动不再
        // 提升到舞台另开一页（见 open_diff 的改动）。
        // Git 改动树和 Files 文件树共用一份已持久化的侧树宽度。拖外层 Tool Panel
        // 不能按比例重算它；只有拖这份树自身的分隔线时才会改变。
        let list_w = self.file_tree_w.clamp(
            crate::tool_panel::MIN_FILE_TREE_WIDTH,
            crate::tool_panel::MAX_FILE_TREE_WIDTH,
        );
        // diff 栏恒在：选中干净仓库时把它整块撤掉，会让面板布局在切仓库时跳来跳去，
        // 还让人以为预览功能坏了。没有可显示的 diff 就画成空态。
        let changes_body: AnyElement = {
            // 派生数据（度量 + 展开行）走缓存；diff 内容不变时每帧 O(1) 复用。
            let derived = self.diff_derived_for_render();
            let empty_hint = if total_changes > 0 {
                "← 选择文件查看更改"
            } else {
                "没有文件更改"
            };
            let diff_pane = git_diff_pane(
                GitDiffPaneParams {
                    root: &root,
                    git_diff: &self.git_diff,
                    derived: derived.as_ref(),
                    empty_hint,
                    collapsed_diff_files: &self.git_collapsed_diff_files,
                    diff_selected: &self.diff_selected,
                    diff_selection_cursor: self.diff_selection_cursor,
                    diff_selection_dragging: self.diff_selection_dragging,
                    diff_comment_open: self.diff_comment_open,
                    diff_comment_input: self.diff_comment_input.as_ref(),
                    diff_scroll: &self.diff_scroll,
                    diff_code_scroll: &self.diff_code_scroll,
                    active_hunk: self.active_hunk,
                },
                cx,
            );
            div()
                .flex_1()
                .relative()
                .min_h_0()
                .overflow_hidden()
                .child(self.file_tree_resize_listener(cx))
                .child(self.diff_selection_listener(cx))
                .flex()
                .child(
                    div()
                        .size_full()
                        .flex()
                        .child(div().flex_1().min_w_0().min_h_0().flex().child(diff_pane))
                        .child(self.file_tree_resize_handle("git-narrow-diff-split", cx))
                        .child(
                            div()
                                .w(px(list_w))
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
                                        .border_color(ui_theme::hairline())
                                        .child(list_and_commit),
                                ),
                        ),
                )
                .into_any_element()
        };

        v_flex()
            .flex_1()
            .min_h_0()
            .child(header)
            .child(changes_body)
            .into_any_element()
    }

    /// GIT 面板「日志」子标签：提交历史 + 分支图三栏（分支树 / 图+列表 / 详情），
    /// 停靠 / 舞台展开态共用（root 由调用方保证非空）。
    pub(crate) fn render_git_log_tab(
        &mut self,
        root: String,
        cx: &mut Context<Workspace>,
    ) -> AnyElement {
        // `root` 现在是当前选中的仓库根（不是项目根），分支与 HEAD 都读它自己的缓存。
        let branches = self.branches.get(&root).map(|(_, b)| b);
        // 当前检出的分支：日志默认看它，分支树里也要标出来。复用 Git 页已有的
        // status 缓存，不另跑一次 git。
        let head_branch = self
            .git_status
            .get(&root)
            .map(|(_, d)| d.branch_name().to_string())
            .filter(|b| !b.is_empty());
        let border = rgb(crate::ui_theme::border_dim());
        // 三栏都可拖拽：窗口窄的时候能自己腾地方，写死宽度的话中间的提交
        // 列表会被挤得没法看。
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .child(
                h_resizable("git-log-split")
                    .with_state(&self.git_log_resize)
                    // 左：分支树
                    .child(
                        resizable_panel()
                            .size(px(200.))
                            .size_range(px(140.)..Pixels::MAX)
                            .child(
                                div()
                                    .size_full()
                                    .min_h_0()
                                    .border_r_1()
                                    .border_color(border)
                                    .child(crate::git_log_view::branch_tree(
                                        Some(root.clone()),
                                        branches,
                                        &self.git_log.scope,
                                        head_branch.clone(),
                                        cx,
                                    )),
                            ),
                    )
                    // 中：分支图 + 提交列表
                    // wrapper 必须 .flex()：div 默认 Block，里面 flex_1 根节点会
                    // 高度塌 0（同 Git 改动页 diff 面板的坑）。
                    .child(
                        resizable_panel().child(
                            div()
                                .size_full()
                                .flex()
                                .min_w_0()
                                .min_h_0()
                                .border_r_1()
                                .border_color(border)
                                .child(crate::git_log_view::git_log_view(
                                    Some(root.clone()),
                                    &self.git_log,
                                    head_branch,
                                    cx,
                                )),
                        ),
                    )
                    // 右：提交详情
                    .child(
                        resizable_panel()
                            .size(px(380.))
                            .size_range(px(240.)..Pixels::MAX)
                            .child(div().size_full().flex().min_w_0().min_h_0().child(
                                crate::git_log_view::commit_detail_pane(
                                    Some(root),
                                    &self.git_log,
                                    cx,
                                ),
                            )),
                    ),
            )
            .into_any_element()
    }
}
