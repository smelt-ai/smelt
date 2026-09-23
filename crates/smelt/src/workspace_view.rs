//! 工作台页面：分屏、确认弹窗、舞台覆盖页。
//!
//! 只组 UI。打开/关闭项目、升级守护、改名等写状态的方法留在 main.rs。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::Input;
use gpui_component::*;

use crate::resizable_split::{h_resizable, v_resizable};
use crate::session_history::{HistoryListState, HistoryViewParams, history_view};
use crate::{
    Pane, RenameTarget, UpdateInstallTrigger, Workspace, request_app_quit, ui_theme, updater,
};

impl Workspace {
    /// 「关闭项目」确认弹窗：说清会连带关掉几个会话（视觉同删 Worktree 那套危险配色）。
    pub(crate) fn render_close_project_confirm(&self, cx: &mut Context<Self>) -> Div {
        let Some((label, _, n)) = self.close_project_target.clone() else {
            return div();
        };
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);

        let content = v_flex()
            .child(Self::modal_title(fg, "确定关闭这个项目吗？"))
            .child(div().text_sm().text_color(muted).child(format!(
                "「{label}」下的 {n} 个会话会被一起关掉，终端里正在跑的东西会被终止。项目本身只是从工作台移走，磁盘上的目录不动。"
            )))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-close-project",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_close_project(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-close-project",
                        "关闭项目",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_close_project(cx),
                        cx,
                    )),
            );
        Self::modal_shell(380., true, content, cx)
    }

    /// 「会话管理」弹窗：列出守护进程持有的全部会话，标出哪些是游离会话（没有任何
    /// 侧栏在追踪），逐个/批量清理。入口和弹层都在设置窗口里。
    pub(crate) fn render_session_manager(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted, border) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground, t.border)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);
        let tracked = self.tracked_session_ids(cx);

        let body: AnyElement = match &self.session_manager_list {
            None => div()
                .text_sm()
                .text_color(muted)
                .child("查询中…")
                .into_any_element(),
            Some(list) if list.is_empty() => div()
                .text_sm()
                .text_color(muted)
                .child("守护进程当前没有任何会话。")
                .into_any_element(),
            Some(list) => {
                let detached_count = list.iter().filter(|s| !tracked.contains(&s.id)).count();
                let mut rows = v_flex()
                    .id("session-manager-list")
                    .gap_1()
                    .max_h(px(360.))
                    .overflow_y_scroll();
                for (row_index, s) in list.iter().enumerate() {
                    let is_detached = !tracked.contains(&s.id);
                    let is_acp = s.id.starts_with("acp-");
                    let label = s
                        .cwd
                        .clone()
                        .or_else(|| s.title.clone())
                        .unwrap_or_else(|| s.id.clone());
                    let id_for_kill = s.id.clone();
                    rows = rows.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .justify_between()
                            .py_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .min_w_0()
                                    .child(div().size_2().rounded_full().bg(if is_detached {
                                        rgb(ui_theme::red())
                                    } else {
                                        rgb(ui_theme::green())
                                    }))
                                    .child(
                                        div()
                                            .text_xs()
                                            .flex_shrink_0()
                                            .text_color(muted)
                                            .child(if is_acp { "对话" } else { "终端" }),
                                    )
                                    .child(
                                        div()
                                            .id(("session-manager-label", row_index))
                                            .flex_1()
                                            .min_w_0()
                                            .text_sm()
                                            .text_color(fg)
                                            .truncate()
                                            .tooltip({
                                                let tip: SharedString = label.clone().into();
                                                move |window, cx| {
                                                    gpui_component::tooltip::Tooltip::new(
                                                        tip.clone(),
                                                    )
                                                    .build(window, cx)
                                                }
                                            })
                                            .child(label),
                                    )
                                    .children(is_detached.then(|| {
                                        div()
                                            .text_xs()
                                            .flex_shrink_0()
                                            .text_color(rgb(ui_theme::red()))
                                            .child("游离会话（无侧栏追踪）")
                                    })),
                            )
                            .child(Self::modal_button(
                                "kill-session-in-manager",
                                "关闭",
                                neutral_bg,
                                neutral_hover,
                                fg,
                                move |this, _, _, cx| {
                                    this.kill_session_in_manager(id_for_kill.clone(), cx);
                                },
                                cx,
                            )),
                    );
                }
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("共 {} 个，{detached_count} 个游离会话", list.len())),
                    )
                    .child(rows)
                    .into_any_element()
            }
        };

        let has_detached = self
            .session_manager_list
            .as_ref()
            .map(|l| l.iter().any(|s| !tracked.contains(&s.id)))
            .unwrap_or(false);

        let content = v_flex()
            .child(Self::modal_title(fg, "会话管理"))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("守护进程持有的全部会话；游离会话是没有被任何窗口侧栏追踪的（测试跑出来的、忘了关的临时会话），清理它们不影响正常使用中的会话。"),
            )
            .child(div().border_t_1().border_color(border).pt_3().child(body))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "close-session-manager",
                        "关闭",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| {
                            this.session_manager_open = false;
                            this.session_manager_tombstones.clear();
                            cx.notify();
                        },
                        cx,
                    ))
                    .when(has_detached, |el| {
                        el.child(Self::modal_button(
                            "kill-all-detached",
                            "清理全部游离会话",
                            tint,
                            hover,
                            accent_text,
                            |this, _, _, cx| {
                                this.kill_all_detached_in_manager(cx);
                            },
                            cx,
                        ))
                    }),
            );
        Self::modal_shell(420., true, content, cx)
    }

    /// 弹窗标题：半粗、跟正文同色阶，不要 `font_bold` + `text_lg` 喊出来。
    pub(crate) fn modal_title(fg: Hsla, label: impl Into<SharedString>) -> Div {
        div().font_semibold().text_color(fg).child(label.into())
    }

    /// 弹窗遮罩 + 居中卡片壳。Grok Bot：12px 圆角矩形、轻阴影、轻遮罩；
    /// 胶囊只给按钮，不给整张卡。
    ///
    /// `heavy` 只比轻遮罩深一档（退出/删除仍用 true），不要把舞台压成纯黑。
    pub(crate) fn modal_shell(
        width: f32,
        heavy: bool,
        content: Div,
        _cx: &mut Context<Self>,
    ) -> Div {
        let backdrop = ui_theme::glass_scrim(heavy);
        div()
            .absolute()
            .inset_0()
            .bg(backdrop)
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                content
                    .w(px(width))
                    .p_5()
                    .bg(ui_theme::glass_floating())
                    .border_1()
                    .border_color(ui_theme::overlay(0x22))
                    .rounded(ui_theme::card_radius())
                    .shadow_sm()
                    .gap_4(),
            )
    }

    /// 弹窗按钮的中性/强调配色：(中性底色, 中性 hover, 强调底色, 强调 hover, 强调文字色)。
    /// `danger=true` 强调色用红（危险操作，如删除/重启），`false` 用蓝（普通确认）。
    pub(crate) fn modal_accent_colors(danger: bool) -> (Hsla, Hsla, Hsla, Hsla, Hsla) {
        let neutral_bg: Hsla = ui_theme::overlay(0x0a).into();
        let neutral_hover: Hsla = ui_theme::overlay(0x1f).into();
        if danger {
            (
                neutral_bg,
                neutral_hover,
                ui_theme::tint(ui_theme::red(), 0x24).into(),
                ui_theme::tint(ui_theme::red(), 0x40).into(),
                Hsla::from(rgb(ui_theme::red())),
            )
        } else {
            // 主操作跟 Grok Bot 发送钮同一套：近白/近黑实心胶囊，不用强调蓝。
            // danger 仍是红薄底，克制警示。
            (
                neutral_bg,
                neutral_hover,
                Hsla::from(rgb(ui_theme::action_fill())),
                ui_theme::tint(ui_theme::action_fill(), 0xe6).into(),
                Hsla::from(rgb(ui_theme::action_on())),
            )
        }
    }

    /// 弹窗按钮的基础样式（尺寸/圆角/字号/底色/文字色/label），不含点击行为——大部分
    /// 调用方直接用 [`Self::modal_button`]；`render_delete_worktree_confirm` 的
    /// 「检查中…」禁用态需要条件性挂 hover/on_click，才会单独调这个再自己 `.when(...)`。
    pub(crate) fn modal_button_base(
        id: &'static str,
        label: impl Into<SharedString>,
        bg: Hsla,
        text_color: Hsla,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .h(px(32.))
            .px_4()
            .flex()
            .items_center()
            .rounded(ui_theme::row_radius())
            .bg(bg)
            .text_sm()
            .font_medium()
            .text_color(text_color)
            .child(label.into())
    }

    /// 弹窗按钮：基础样式 + hover 变色 + 点击行为，覆盖绝大多数弹窗按钮的用法。
    pub(crate) fn modal_button(
        id: &'static str,
        label: &'static str,
        bg: Hsla,
        hover_bg: Hsla,
        text_color: Hsla,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        Self::modal_button_base(id, label, bg, text_color)
            .cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .on_click(cx.listener(on_click))
    }

    /// 渲染无条件退出确认弹层：轻遮罩 + 确认退出/取消按钮。
    pub(crate) fn render_quit_confirm(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            Self::modal_accent_colors(false);
        let update_is_applying =
            matches!(self.update_status, updater::UpdateStatus::Applying { .. });

        let content = v_flex()
            .child(Self::modal_title(fg, "退出 Smelt？"))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(if update_is_applying {
                        "正在安全应用更新，完成后会自动重启。"
                    } else {
                        "后台会话将继续运行，当前连接会断开。"
                    }),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-quit",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| {
                            if this.quit_requested {
                                return;
                            }
                            this.show_quit_confirm = false;
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-quit",
                        if self.quit_requested {
                            "正在退出…"
                        } else if update_is_applying {
                            "正在更新…"
                        } else {
                            "退出"
                        },
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| {
                            if this.quit_requested {
                                return;
                            }
                            // Applying 表示 helper 已派生且当前 GUI 正在退出，不能再启动
                            // 第二条退出或安装流程。
                            if matches!(this.update_status, updater::UpdateStatus::Applying { .. })
                            {
                                return;
                            }
                            this.quit_requested = true;
                            cx.notify();
                            // 退出只是统一安装驱动的一种触发策略：helper 派生成功后退出，
                            // 并在本进程完全消失后才提交同一份事务。
                            if let Some(update) = this.update_status.ready_update().cloned() {
                                this.start_update_install(update, UpdateInstallTrigger::Quit, cx);
                                return;
                            }
                            request_app_quit(cx);
                        },
                        cx,
                    )),
            );
        Self::modal_shell(320., true, content, cx)
    }

    /// 侧栏「重命名」弹层：与 render_quit_confirm 同款视觉（居中卡片 + 半透明遮罩），
    /// 正文换成预填当前标题的文本框。仅在 self.rename_input 就绪时被调用（见
    /// start_rename/上面 .children(self.rename_target.is_some()...)）。
    pub(crate) fn render_rename_session(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            Self::modal_accent_colors(false);
        let Some(input) = self.rename_input.as_ref() else {
            return div();
        };
        // 会话行和分屏子行共用这个弹窗，标题得说清改的是哪个。
        let heading = match self.rename_target {
            Some(RenameTarget::Pane(_)) => "重命名终端",
            Some(RenameTarget::History { .. }) => "重命名历史会话",
            Some(RenameTarget::WorkspaceSurface(_)) => "重命名",
            _ => "重命名会话",
        };

        let content = v_flex()
            .child(Self::modal_title(fg, heading))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("留空则恢复自动识别的标题。"),
            )
            .child(Input::new(input))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-rename",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| this.cancel_rename(cx),
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-rename",
                        "确定",
                        tint,
                        hover,
                        accent_text,
                        |this, _, window, cx| this.confirm_rename(window, cx),
                        cx,
                    )),
            );
        Self::modal_shell(320., false, content, cx)
    }

    /// 「重启守护进程」二次确认弹窗：明确告知会断开所有当前终端会话。与
    /// render_quit_confirm 同款视觉（居中卡片 + 半透明遮罩）。
    ///
    /// 入口只在设置窗「更新」页；弹层挂在设置窗上（见 `SettingsWindow::render`），
    /// 不再画到主窗口，避免「按钮在设置、确认框跑到主界面」的割裂感。
    pub(crate) fn render_daemon_restart_confirm(&self, cx: &mut Context<Self>) -> Div {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) = Self::modal_accent_colors(true);

        let content = v_flex()
            .child(Self::modal_title(fg, "确定重启守护进程吗？"))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("守护进程升级后才会生效新版本。重启会立即断开并终止当前所有终端会话（包括正在跑的 agent），且无法恢复。"),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "cancel-daemon-restart",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| {
                            this.show_daemon_restart_confirm = false;
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(Self::modal_button(
                        "confirm-daemon-restart",
                        "确定重启",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| this.confirm_restart_daemon(cx),
                        cx,
                    )),
            );
        Self::modal_shell(320., true, content, cx)
    }

    /// 当前文件有未保存改动、又点了别的文件时弹的确认弹窗：取消 / 不保存直接切换 /
    /// 保存并切换。与 render_quit_confirm 同款视觉（居中卡片 + 半透明遮罩）。
    pub(crate) fn render_unsaved_file_confirm(
        &self,
        target: String,
        cx: &mut Context<Self>,
    ) -> Div {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (neutral_bg, neutral_hover, tint, hover, accent_text) =
            Self::modal_accent_colors(false);
        let cur_name = self
            .open_file
            .as_ref()
            .map(|of| {
                of.path
                    .rsplit('/')
                    .next()
                    .unwrap_or(of.path.as_str())
                    .to_string()
            })
            .unwrap_or_default();
        let target_name = target
            .rsplit('/')
            .next()
            .unwrap_or(target.as_str())
            .to_string();

        let content = v_flex()
            .child(Self::modal_title(
                fg,
                format!("「{cur_name}」有未保存的改动"),
            ))
            .child(div().text_sm().text_color(muted).child(format!(
                "要切换到「{target_name}」了，这些改动还没保存，要怎么处理？"
            )))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_button(
                        "unsaved-cancel",
                        "取消",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, _, cx| {
                            this.pending_file_switch = None;
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(Self::modal_button(
                        "unsaved-discard",
                        "不保存，直接切换",
                        neutral_bg,
                        neutral_hover,
                        fg,
                        |this, _, window, cx| {
                            if let Some(target) = this.pending_file_switch.take() {
                                this.open_file_now(target, None, window, cx);
                            }
                        },
                        cx,
                    ))
                    .child(Self::modal_button(
                        "unsaved-save-switch",
                        "保存并切换",
                        tint,
                        hover,
                        accent_text,
                        |this, _, _, cx| {
                            if let Some(target) = this.pending_file_switch.take() {
                                this.pending_switch_after_save = Some(target);
                                this.save_open_file(cx);
                            }
                        },
                        cx,
                    )),
            );
        Self::modal_shell(360., true, content, cx)
    }

    /// 递归渲染分屏布局树：Leaf 渲染一个终端（活动 pane 描边 + 点击聚焦），
    /// Split 用 h/v_resizable 把子节点排成可拖拽的并排 / 堆叠。
    pub(crate) fn render_pane(
        &self,
        pane: &Pane,
        path: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match pane {
            Pane::Leaf(t) => {
                let active = self.cur().is_some_and(|s| s.anchor_id() == t.entity_id());
                // 不给任何 pane 描边（iTerm2 也不描，之前的蓝框提醒也拿掉了）：分屏时靠
                // 「压暗非活动 pane」区分谁是活动的就够了；单 pane 时压根没有别的 pane
                // 可比，不需要任何叠加层。
                let multi_pane = self.cur().is_some_and(|s| s.pane_count() > 1);
                let overlay = if !multi_pane || active {
                    div().absolute().inset_0()
                } else {
                    div().absolute().inset_0().bg(hsla(0., 0., 0., 0.28))
                };
                let te = t.clone();
                div()
                    .id(SharedString::from(path.to_string()))
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_hidden()
                    // 点击 pane 即设为当前会话的活动 pane（终端自身也会抢焦点，二者一致）。
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _ev, window, cx| {
                            this.activate_pane(&te, window, cx)
                        }),
                    )
                    .child(t.clone())
                    .child(overlay)
                    .into_any_element()
            }
            Pane::Split {
                axis,
                state,
                children,
                init_sizes,
            } => {
                let id = SharedString::from(path.to_string());
                let mut group = if matches!(axis, Axis::Horizontal) {
                    h_resizable(id)
                } else {
                    v_resizable(id)
                }
                .with_state(state);
                for (i, c) in children.iter().enumerate() {
                    let el = self.render_pane(c, &format!("{path}-{i}"), cx);
                    // 存档尺寸只作 initial_size：拖过之后 panel 自己的 size 会盖过它
                    //（见 Pane::Split::init_sizes 注释），所以每帧原样传是安全的。
                    let mut panel = resizable_panel().child(el);
                    if let Some(s) = init_sizes.get(i).copied().filter(|s| *s > 0.) {
                        panel = panel.size(px(s));
                    }
                    group = group.child(panel);
                }
                group.into_any_element()
            }
        }
    }

    /// 智能体的 space 里只可能有底层引擎自己的历史，别家的在这个目录下不可能
    /// 存在。返回 `Some(kind)` 表示当前处在某个智能体的上下文里。
    pub(crate) fn history_engine_override(
        &self,
        cwd: Option<&str>,
        cx: &App,
    ) -> Option<crate::settings::ConversationAgentKind> {
        let id = smelt_core::agent_definition_store::agent_definition_id_for_space(
            std::path::Path::new(cwd?),
        )?;
        cx.global::<crate::settings::AgentHostState>()
            .agents
            .iter()
            .find(|agent| agent.id == id)
            .and_then(|agent| agent.engine_kind())
    }

    /// 历史列表要按哪个引擎、哪份 profile 去扫。渲染和后台扫描必须用同一个答案，
    /// 否则扫的是 A 的数据、显示的是 B 的 tab，列表会永远空着。
    pub(crate) fn history_source(
        &self,
        cwd: Option<&str>,
        cx: &App,
    ) -> (crate::settings::HistorySourceKind, Option<String>) {
        match self.history_engine_override(cwd, cx) {
            Some(kind) => (kind.into(), None),
            None => (self.history_agent, self.history_profile.clone()),
        }
    }

    /// 历史内容由 Tool Panel 统一承载；停靠和全屏只改变外层容器尺寸，
    /// 不在历史视图内部做特殊布局分支。
    pub(crate) fn render_history_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let cwd = self.active_project_root(cx);
        let restrict_to_kind = self.history_engine_override(cwd.as_deref(), cx);
        let (agent, profile_id) = self.history_source(cwd.as_deref(), cx);
        let list_key = cwd
            .as_ref()
            .map(|c| crate::session_history::session_list_key(agent, profile_id.as_deref(), c));
        let sessions = list_key
            .as_ref()
            .and_then(|k| self.session_list.get(k).map(|(_, d)| d.clone()));
        let list_state = match (cwd.as_ref(), sessions) {
            (None, _) => HistoryListState::NoProject,
            (Some(_), None) => HistoryListState::Loading,
            (Some(_), Some(s)) if s.is_empty() => HistoryListState::Empty,
            (Some(_), Some(s)) => HistoryListState::Ready(s),
        };
        let (source_tabs, launch_override, profile_label, history_agent_ids) = {
            let config = cx.global::<crate::settings::AgentHostState>();
            let source_tabs = crate::session_history::history_source_tabs(
                restrict_to_kind,
                config.all_profiles().cloned(),
            );
            let launch_override = profile_id.as_deref().and_then(|id| {
                config
                    .find_profile(id)
                    .and_then(|profile| config.profile_launch_spec(profile).ok())
            });
            let profile_label = profile_id
                .as_deref()
                .and_then(|id| config.find_profile(id).map(|profile| profile.label.clone()));
            let known: std::collections::HashSet<&str> = config
                .agents
                .iter()
                .map(|agent| agent.id.as_str())
                .collect();
            let mut history_agent_ids =
                smelt_core::session_metadata::agent_definition_ids(agent, profile_id.as_deref());
            history_agent_ids.retain(|_, id| known.contains(id.as_str()));
            (
                source_tabs,
                launch_override,
                profile_label,
                std::rc::Rc::new(history_agent_ids),
            )
        };
        history_view(
            HistoryViewParams {
                agent,
                profile_id,
                cwd,
                list: list_state,
                detail: &self.session_detail,
                detail_list_state: self.history_detail_list_state.clone(),
                filter: self.history_filter.clone(),
                restrict_to_kind,
                source_tabs,
                launch_override,
                profile_label,
                history_agent_ids,
            },
            cx,
        )
        .into_any_element()
    }
}
