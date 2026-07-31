//! 会话舞台的头部（44px：会话名 + 状态胶囊 + 次要信息 + split）
//! 终端与 ACP 会话共用这一层，避免在终端底部重复展示状态和快捷键。
//! 头栏里只有状态胶囊保留卡片底+边框，是唯一该抢视线的颜色；模型/token/
//! cwd/git 统一降级成 "·" 分隔的纯文字，避免所有信息一样重导致扫不出重点。
//!
//! 跟 file_tree.rs 同一个套路：`impl Workspace` 方法，字段仍在 main.rs。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::*;

use crate::inspector::InspectorTab;
use crate::{AgentStatus, MainView, SessionKind, Workspace, session_history, ui_theme};

/// 状态胶囊文案（与会话列表副标题同一套口径）。
fn phase_text(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::WaitingApproval => "等你批准",
        AgentStatus::NeedsAttention => "需要处理",
        AgentStatus::Running => "运行中",
        AgentStatus::Done => "已完成",
        AgentStatus::Idle => "空闲",
    }
}

impl Workspace {
    /// 钻取页顶部的返回条（32px）。任务 route 不经过这里；Files/Git/Skills
    /// 展开态也使用自己的收回操作，目前只有 History 需要返回会话。
    pub(crate) fn render_stage_back_bar(
        &self,
        v: MainView,
        corner_guard: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        // match 保持穷尽，非 History 变体若误走到这里应尽早暴露调用错误。
        let (label, back) = match v {
            MainView::History => ("历史会话", "‹ 返回会话"),
            MainView::Tasks | MainView::Files | MainView::Git | MainView::Skills => {
                unreachable!("一级页面或展开面板不应渲染返回条")
            }
        };
        let this = cx.entity();
        div()
            .h(px(32.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_2p5()
            // sidebar 收起时这条横条会变成贴着窗口左边缘那一块，真交通灯浮在
            // 它头上——跟 render_stage_header 同一个处理：不额外拿一整行 34px
            // 去撑高度，就地在左边多让出交通灯的宽度，留在同一行里。
            // 128px 不是随手拍的：main.rs 顶部拖拽层里的「切换左侧栏」图标固定
            // 绝对定位在 left(92px)、size_6（24px），92+24=116 再加一点间距——
            // 之前给 92px 只避开了交通灯，没避开这颗常驻图标，标题文字被它糊住。
            .when(corner_guard, |d| d.pl(px(128.)).pr_3())
            .when(!corner_guard, |d| d.px_3())
            // 跟 44px 舞台头统一用 bg_bar：这条返回条现在紧挨着同样刷成 bg_bar
            // 的顶部安全区，两截颜色对不上就又会露出一条突兀的分界。
            .bg(rgb(ui_theme::bg_bar()))
            .border_b_1()
            .border_color(rgb(ui_theme::border_dim()))
            .child(
                div()
                    .id("stage-back")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py(px(2.))
                    .rounded(px(6.))
                    .text_sm()
                    .text_color(rgb(ui_theme::text_mid()))
                    .cursor_pointer()
                    .hover(|d| {
                        d.bg(rgb(ui_theme::bg_hover()))
                            .text_color(rgb(ui_theme::text_bright()))
                    })
                    .child(back)
                    .on_click(move |_ev, window, cx| {
                        this.update(cx, |ws, cx| ws.set_stage_override(None, window, cx));
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .font_semibold()
                    .text_color(rgb(ui_theme::text_faint()))
                    .child(label),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(rgb(ui_theme::text_faint()))
                    .child("Esc"),
            )
    }

    /// 44px 舞台头。没有会话时返回 None（空态自带引导）。
    ///
    /// `corner_guard`：sidebar 收起时这块舞台会变成贴着窗口左边缘的那一块，
    /// 真交通灯浮在它上面——这时不额外拿一整条 34px 出来把头栏往下挤（那样
    /// 平白多出一整行空白，看着像布局错位），而是让头栏自己在左边多留出
    /// 交通灯的宽度，标题跟交通灯挤在同一行里，参考 Arc / VS Code 的处理。
    pub(crate) fn render_stage_header(
        &mut self,
        corner_guard: bool,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let ix = self.active_session;
        let sess = self.sessions.get(ix)?;
        let title = sess.title(cx);
        let is_term = matches!(sess.kind, SessionKind::Term { .. });
        // 状态胶囊：ACP 直接问视图要相位（它有自己的相位机，经五态映射会把
        // 「启动中 / 已结束」都塌成「空闲」）；终端仍走 AgentStatus 那套。
        let (phase_label, phase_color) = match &sess.kind {
            SessionKind::Acp(view) => {
                let (label, color) = view.read(cx).phase_label();
                (label, rgb(color))
            }
            SessionKind::Term { .. } => {
                let st = sess.status(cx);
                (phase_text(st), ui_theme::session_dot_color(st))
            }
        };
        // ACP 会话把当前模型也摆到舞台头上——「这轮对话用的哪个模型」是随时
        // 要能确认的事实，不该只藏在输入栏胶囊里。ACP 顺带把当前上下文用量
        // （精确 token 数，跟输入栏「上下文 %」胶囊同一个数据源）也取出来。
        let (model, acp_tokens) = match &sess.kind {
            SessionKind::Acp(view) => {
                let v = view.read(cx);
                (v.model_name(), v.context_tokens_used())
            }
            SessionKind::Term { .. } => (None, None),
        };
        let cwd = sess.cwd(cx);
        let cwd_tail = cwd
            .as_ref()
            .map(|c| crate::project_name_for_cwd(&c))
            .unwrap_or_default();
        let git_summary = cwd
            .as_ref()
            .and_then(|cwd| self.git_status.get(cwd))
            .and_then(|(_, status)| {
                let branch = status.branch_name();
                (!branch.is_empty()).then(|| (branch.to_string(), status.files.len()))
            });
        // 终端会话没有实时用量上报，退而求其次：复用历史会话缓存，取该项目下
        // Claude Code 最近一次活跃会话的累计 token 数（近似值——终端里跑的不一定
        // 是 Claude，也可能缓存还没扫到，两种情况都直接不显示，不瞎猜）。
        let token_count = if is_term {
            cwd.as_ref().and_then(|c| {
                self.ensure_session_list(
                    crate::settings::AcpAgentKind::Claude,
                    None,
                    c.clone(),
                    cx,
                );
                self.claude_session_tokens(c)
            })
        } else {
            acp_tokens
        };
        let this = cx.entity();

        // 次要信息（模型 / token / 项目目录）统一降级成纯文字，用 "·" 分隔，
        // 不再各自套跟状态胶囊一样的卡片边框——状态才是这条头栏唯一该抢视线的颜色，
        // 其余是「随时能查」但不需要抢戏的辅助信息。
        let mut info_segments: Vec<AnyElement> = Vec::new();
        if let Some(m) = model {
            info_segments.push(
                div()
                    .text_xs()
                    .text_color(rgb(ui_theme::text_mid()))
                    .child(m)
                    .into_any_element(),
            );
        }
        if let Some(n) = token_count {
            info_segments.push(
                div()
                    .id("stage-token-count")
                    .text_xs()
                    .font_family("monospace")
                    .text_color(rgb(ui_theme::text_mid()))
                    .child(format!("{} tok", session_history::format_count(n)))
                    .tooltip(|window, cx| {
                        gpui_component::tooltip::Tooltip::new(
                            "Claude 用量：ACP 会话为当前上下文占用，终端会话为最近一次 Claude Code 活动的累计用量（近似值）",
                        )
                        .build(window, cx)
                    })
                    .into_any_element(),
            );
        }
        if !cwd_tail.is_empty() {
            info_segments.push(
                div()
                    .text_xs()
                    .font_family("monospace")
                    .text_color(rgb(ui_theme::text_faint()))
                    .child(cwd_tail)
                    .into_any_element(),
            );
        }
        let mut info_cluster = div()
            .flex()
            .items_center()
            .gap_1p5()
            .min_w(px(0.))
            .flex_shrink(1.)
            .overflow_hidden();
        for (i, seg) in info_segments.into_iter().enumerate() {
            if i > 0 {
                info_cluster = info_cluster.child(
                    div()
                        .text_xs()
                        .text_color(rgb(ui_theme::text_faint()))
                        .child("·"),
                );
            }
            info_cluster = info_cluster.child(seg);
        }

        let e_split = this.clone();
        let e_git = this.clone();
        Some(
            div()
                .h(px(44.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_2p5()
                .when(corner_guard, |d| d.pl(px(128.)).pr_4())
                .when(!corner_guard, |d| d.px_4())
                // 舞台头独立成 bg_bar 表面：跟下面终端/消息流的 bg_panel 拉开层次，
                // 呼应参考应用「聊天头栏 vs 消息区」的色阶差，不再是同一块底上
                // 光靠一条描边分隔。
                .bg(rgb(ui_theme::bg_bar()))
                .border_b_1()
                .border_color(rgb(ui_theme::border_dim()))
                .child(
                    div()
                        // 收窄到极限也至少留够几个字——之前是 0，pane 一变窄就先塌成
                        // 纯省略号「…」，标题（这行最该保住的信息）反而完全看不见。
                        // 该让步的是下面 info_cluster 那串「随时能查」的次要信息。
                        .min_w(px(56.))
                        .flex_shrink(1.)
                        .overflow_hidden()
                        .text_sm()
                        .font_semibold()
                        .text_color(rgb(ui_theme::text_bright()))
                        .truncate()
                        .child(title),
                )
                .child(
                    // 状态胶囊：头栏里唯一保留卡片底+边框的元素，用颜色/边框把
                    // 「这个会话现在什么状态」跟其余辅助信息拉开一档视觉权重。
                    div()
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap_1p5()
                        .px_2()
                        .py(px(2.))
                        .rounded(px(6.))
                        .bg(rgb(ui_theme::bg_card()))
                        .border_1()
                        .border_color(rgb(ui_theme::border_mid()))
                        .text_xs()
                        .text_color(rgb(ui_theme::text_muted()))
                        .child(div().size(px(6.)).rounded_full().bg(phase_color))
                        .child(phase_label),
                )
                .child(info_cluster)
                .children(git_summary.map(|(branch, changes)| {
                    div()
                        .id("stage-git-status")
                        .flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap_1p5()
                        .px_2()
                        .py(px(2.))
                        .rounded(px(6.))
                        .text_xs()
                        .font_family("monospace")
                        .text_color(rgb(ui_theme::text_mid()))
                        .cursor_pointer()
                        .hover(|d| {
                            d.bg(rgb(ui_theme::bg_hover()))
                                .text_color(rgb(ui_theme::text_bright()))
                        })
                        .child(Icon::new(IconName::Github).size(px(12.)))
                        .child(branch)
                        .when(changes > 0, |d| {
                            d.child(
                                div()
                                    .min_w(px(16.))
                                    .h(px(16.))
                                    .px(px(4.))
                                    .rounded(px(8.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(rgb(ui_theme::accent()))
                                    .text_color(rgb(ui_theme::on_accent()))
                                    .text_size(px(9.))
                                    .font_semibold()
                                    .child(changes.to_string()),
                            )
                        })
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new("打开 Git 面板").build(window, cx)
                        })
                        .on_click(move |_ev, window, cx| {
                            e_git.update(cx, |ws, cx| {
                                if ws.inspector_panel_promoted() {
                                    ws.set_stage_override(None, window, cx);
                                }
                                ws.inspector_tab = InspectorTab::Git;
                                ws.set_inspector_open(true);
                                cx.notify();
                            });
                        })
                }))
                .child(div().flex_1())
                .children(is_term.then(|| {
                    div()
                        .id("stage-split")
                        .text_xs()
                        .font_family("monospace")
                        .text_color(rgb(ui_theme::text_faint()))
                        .cursor_pointer()
                        .hover(|d| d.text_color(rgb(ui_theme::text_mid())))
                        .child("split ⌘D")
                        .on_click(move |_ev, _window, cx| {
                            e_split.update(cx, |ws, cx| ws.split_active(Axis::Horizontal, cx));
                        })
                })),
        )
    }
}
