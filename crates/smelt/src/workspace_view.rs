//! 工作台页面：分屏、确认弹窗、舞台覆盖页。
//!
//! 只组 UI。打开/关闭项目、升级守护、改名等写状态的方法留在 main.rs。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::input::Input;
use gpui_component::*;

use crate::session_history::{HistoryListState, HistoryViewParams, history_view};
use crate::{CloseConfirm, RenameTarget, Workspace, request_app_quit, ui_theme};
#[cfg(target_os = "macos")]
use crate::{UpdateInstallTrigger, updater};

enum ModalRole {
    Normal,
    Commit,
    Danger,
}

impl Workspace {
    /// 关闭确认弹窗：关项目说清会连带关掉几个会话；关正在跑的会话说清这一轮会被终止。
    /// 用危险配色。
    pub(crate) fn render_close_confirm(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(target) = self.close_confirm.clone() else {
            return div().into_any_element();
        };
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let (title, body, confirm_label) = match &target {
            CloseConfirm::Project {
                label, sessions, ..
            } => (
                "确定关闭这个项目吗？",
                format!(
                    "「{label}」下的 {sessions} 个会话会被一起关掉，终端里正在跑的东西会被终止。项目本身只是从工作台移走，磁盘上的目录不动。"
                ),
                "关闭项目",
            ),
            CloseConfirm::Runtime { name, .. } => (
                "确定移除这台机器吗？",
                format!(
                    "这台电脑不再连「{name}」，侧栏里它的项目和会话会一起移走。那台机器上的会话照常运行，重新配对后还能回来。"
                ),
                "移除",
            ),
            CloseConfirm::Session { title, pane, .. } => (
                if pane.is_some() {
                    "这个分屏还在运行，确定关闭吗？"
                } else {
                    "这个会话还在运行，确定关闭吗？"
                },
                format!("「{title}」正在跑的这一轮会被终止，关掉之后找不回来。"),
                if pane.is_some() {
                    "关闭分屏"
                } else {
                    "关闭会话"
                },
            ),
        };

        let content = v_flex()
            .child(Self::modal_title(fg, title))
            .child(div().text_sm().text_color(muted).child(body))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_normal_button(
                        "cancel-close-confirm",
                        "取消",
                        |this, _, _, cx| this.cancel_close_confirm(cx),
                        cx,
                    ))
                    .child(Self::modal_danger_button(
                        "confirm-close-confirm",
                        confirm_label,
                        |this, _, window, cx| this.confirm_close(window, cx),
                        cx,
                    )),
            );
        self.modal_shell(
            380.,
            true,
            content,
            crate::overlay::ModalLayer::CloseConfirm,
            cx,
        )
    }

    /// 「会话管理」弹窗：列出守护进程持有的全部会话，标出哪些是游离会话（没有任何
    /// 侧栏在追踪），逐个/批量清理。入口和弹层都在设置窗口里。
    pub(crate) fn render_session_manager(&self, cx: &mut Context<Self>) -> AnyElement {
        let (fg, muted, border) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground, t.border)
        };
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
                for s in list {
                    let is_detached = !tracked.contains(&s.id);
                    let is_acp = s.id.starts_with("acp-");
                    let label: SharedString = s
                        .cwd
                        .clone()
                        .or_else(|| s.title.clone())
                        .unwrap_or_else(|| s.id.clone())
                        .into();
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
                                            .id((
                                                ElementId::from("session-manager-label"),
                                                id_for_kill.clone(),
                                            ))
                                            .flex_1()
                                            .min_w_0()
                                            .text_sm()
                                            .text_color(fg)
                                            .truncate()
                                            .tooltip({
                                                let tip = label.clone();
                                                move |window, cx| {
                                                    gpui_component::tooltip::Tooltip::new(
                                                        tip.clone(),
                                                    )
                                                    .build(window, cx)
                                                }
                                            })
                                            .child(label.clone()),
                                    )
                                    .children(is_detached.then(|| {
                                        div()
                                            .text_xs()
                                            .flex_shrink_0()
                                            .text_color(rgb(ui_theme::red()))
                                            .child("游离会话（无侧栏追踪）")
                                    })),
                            )
                            .child(Self::modal_normal_button(
                                "kill-session-in-manager",
                                "关闭",
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
                    .child(Self::modal_normal_button(
                        "close-session-manager",
                        "关闭",
                        |this, _, _, cx| {
                            this.session_manager_open = false;
                            this.session_manager_tombstones.clear();
                            cx.notify();
                        },
                        cx,
                    ))
                    .when(has_detached, |el| {
                        el.child(Self::modal_danger_button(
                            "kill-all-detached",
                            "清理全部游离会话",
                            |this, _, _, cx| {
                                this.kill_all_detached_in_manager(cx);
                            },
                            cx,
                        ))
                    }),
            );
        self.modal_shell(
            420.,
            true,
            content,
            crate::overlay::ModalLayer::SessionManager,
            cx,
        )
    }

    /// 弹窗标题：半粗、跟正文同色阶，不要 `font_bold` + `text_lg` 喊出来。
    pub(crate) fn modal_title(fg: Hsla, label: impl Into<SharedString>) -> Div {
        div().font_semibold().text_color(fg).child(label.into())
    }

    /// 弹窗遮罩 + 居中卡片壳。沙色卡片和遮罩还是原来的；外壳换成
    /// `gpui_base::Dialog`，只为了焦点陷阱。不走组件库那套 Dialog 皮肤。
    ///
    /// Esc 仍由 Workspace 捕获关掉。Dialog 的 Enter=Confirm 会无视当前按钮直接
    /// 跑确定，危险操作不能这样，所以键盘上下文关掉。Tab 到按钮后，按钮自己的
    /// Enter / Space 才生效。点遮罩不关闭。
    ///
    /// `heavy` 只比轻遮罩深一档（退出/删除仍用 true），不要把舞台压成纯黑。
    pub(crate) fn modal_shell(
        &self,
        width: f32,
        heavy: bool,
        content: Div,
        layer: crate::overlay::ModalLayer,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let card = content
            .occlude()
            .w(px(width))
            .p_5()
            .bg(ui_theme::glass_floating())
            .border_1()
            .border_color(ui_theme::overlay(0x22))
            .rounded(ui_theme::card_radius())
            .shadow_sm()
            .gap_4();
        // 底下那层若也是 Dialog，会和最上层共用 dialog-host 与焦点句柄，
        // 后画的列表会盖住确认框。底下只留卡片，焦点陷阱只挂最上层。
        if !self.modal_is_top(layer) {
            return div()
                .absolute()
                .inset_0()
                .bg(ui_theme::glass_scrim(heavy))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(card)
                .into_any_element();
        }
        let backdrop = ui_theme::glass_scrim(heavy);
        gpui_base::Dialog::new(cx)
            .focus_handle(self.modal_focus.clone())
            .close_on_escape(false)
            .close_on_backdrop_press(false)
            .layer(layer.rank() as usize, true)
            .backdrop(
                div()
                    .size_full()
                    .bg(backdrop)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()),
            )
            .popup(card)
            .into_any_element()
    }

    /// 弹窗里的普通按钮（取消）。
    pub(crate) fn modal_normal_button(
        id: &'static str,
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> smelt_ui::Button {
        Self::modal_role_button(id, label, ModalRole::Normal, on_click, cx)
    }

    /// 这块确认里唯一的默认提交（退出、改名、未保存、选目录）。
    pub(crate) fn modal_commit_button(
        id: &'static str,
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> smelt_ui::Button {
        Self::modal_role_button(id, label, ModalRole::Commit, on_click, cx)
    }

    /// 不可逆确认：丢弃、删除、重启守护、关闭仍在跑的会话。
    pub(crate) fn modal_danger_button(
        id: &'static str,
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> smelt_ui::Button {
        Self::modal_role_button(id, label, ModalRole::Danger, on_click, cx)
    }

    fn modal_role_button(
        id: &'static str,
        label: impl Into<SharedString>,
        role: ModalRole,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> smelt_ui::Button {
        let host = cx.entity().downgrade();
        let label = label.into();
        let button = smelt_ui::Button::new(id)
            .label(label.clone())
            .accessibility_label(label);
        let button = match role {
            ModalRole::Normal => button.normal(),
            ModalRole::Commit => button.commit(),
            ModalRole::Danger => button.danger(),
        };
        button.on_click(move |event, window, cx| {
            let Some(host) = host.upgrade() else {
                return;
            };
            host.update(cx, |this, cx| on_click(this, event, window, cx));
        })
    }

    /// 渲染无条件退出确认弹层：轻遮罩 + 确认退出/取消按钮。
    pub(crate) fn render_quit_confirm(&self, cx: &mut Context<Self>) -> AnyElement {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        #[cfg(target_os = "macos")]
        let update_is_applying =
            matches!(self.update_status, updater::UpdateStatus::Applying { .. });
        #[cfg(not(target_os = "macos"))]
        let update_is_applying = false;

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
                    .child(Self::modal_normal_button(
                        "cancel-quit",
                        "取消",
                        |this, _, _, cx| {
                            if this.quit_requested {
                                return;
                            }
                            this.show_quit_confirm = false;
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(Self::modal_commit_button(
                        "confirm-quit",
                        if self.quit_requested {
                            "正在退出…"
                        } else if update_is_applying {
                            "正在更新…"
                        } else {
                            "退出"
                        },
                        |this, _, _, cx| {
                            if this.quit_requested {
                                return;
                            }
                            // Applying 表示 helper 已派生且当前 GUI 正在退出，不能再启动
                            // 第二条退出或安装流程。更新事务只在 macOS 上存在。
                            #[cfg(target_os = "macos")]
                            if matches!(this.update_status, updater::UpdateStatus::Applying { .. })
                            {
                                return;
                            }
                            this.quit_requested = true;
                            cx.notify();
                            // 退出只是统一安装驱动的一种触发策略：helper 派生成功后退出，
                            // 并在本进程完全消失后才提交同一份事务。
                            #[cfg(target_os = "macos")]
                            if let Some(update) = this.update_status.ready_update().cloned() {
                                this.start_update_install(update, UpdateInstallTrigger::Quit, cx);
                                return;
                            }
                            request_app_quit(cx);
                        },
                        cx,
                    )),
            );
        self.modal_shell(320., true, content, crate::overlay::ModalLayer::Quit, cx)
    }

    /// 启动时提议把 App 移到「应用程序」：被系统转移或放在只读位置时更新装不上。
    /// 「以后再说」只关这一次，设置页的更新栏里一直留着同一个按钮。
    /// 只在 macOS 上存在（设计文档 D4）。
    #[cfg(target_os = "macos")]
    pub(crate) fn render_move_app_confirm(&self, cx: &mut Context<Self>) -> AnyElement {
        let (fg, muted, danger) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground, t.danger)
        };
        let body = match self.app_location_issue {
            Some(smelt_client::app_location::LocationIssue::ReadOnly) => {
                "Smelt 正从只读位置（比如安装盘映像）运行，自动更新装不上。移过去后会自动重新打开，后台会话不受影响。"
            }
            _ => {
                "Smelt 没放进「应用程序」就打开了，macOS 让它在一个临时的只读位置运行，自动更新因此装不上。移过去后会自动重新打开，后台会话不受影响。"
            }
        };
        let content = v_flex()
            .child(Self::modal_title(fg, "把 Smelt 移到「应用程序」？"))
            .child(div().text_sm().text_color(muted).child(body))
            .when_some(self.move_app_error.clone(), |content, error| {
                content.child(div().text_sm().text_color(danger).child(error))
            })
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_normal_button(
                        "move-app-later",
                        "以后再说",
                        |this, _, _, cx| {
                            if this.moving_app {
                                return;
                            }
                            this.show_move_app_confirm = false;
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(Self::modal_commit_button(
                        "move-app-confirm",
                        if self.moving_app {
                            "正在移动…"
                        } else {
                            "移过去并重新打开"
                        },
                        |this, _, _, cx| this.move_app_to_applications(cx),
                        cx,
                    )),
            );
        self.modal_shell(380., true, content, crate::overlay::ModalLayer::MoveApp, cx)
    }

    /// 侧栏「重命名」弹层：与 render_quit_confirm 同款视觉（居中卡片 + 半透明遮罩），
    /// 正文换成预填当前标题的文本框。仅在 self.rename_input 就绪时被调用（见
    /// start_rename/上面 .children(self.rename_target.is_some()...)）。
    pub(crate) fn render_rename_session(&self, cx: &mut Context<Self>) -> AnyElement {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
        let Some(input) = self.rename_input.as_ref() else {
            return div().into_any_element();
        };
        // 会话行和分屏子行共用这个弹窗，标题得说清改的是哪个。
        let (heading, hint) = match self.rename_target {
            Some(RenameTarget::Pane(_)) => ("重命名终端", "留空则恢复自动识别的标题。"),
            Some(RenameTarget::History { .. }) => ("重命名历史会话", "留空则恢复自动识别的标题。"),
            Some(RenameTarget::Runtime(_)) => (
                "重命名机器",
                "侧栏和机器面板里显示这个名字，只在这台电脑上生效。",
            ),
            _ => ("重命名会话", "留空则恢复自动识别的标题。"),
        };

        let content = v_flex()
            .child(Self::modal_title(fg, heading))
            .child(div().text_sm().text_color(muted).child(hint))
            .child(Input::new(input))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_normal_button(
                        "cancel-rename",
                        "取消",
                        |this, _, _, cx| this.cancel_rename(cx),
                        cx,
                    ))
                    .child(Self::modal_commit_button(
                        "confirm-rename",
                        "确定",
                        |this, _, window, cx| this.confirm_rename(window, cx),
                        cx,
                    )),
            );
        self.modal_shell(320., false, content, crate::overlay::ModalLayer::Rename, cx)
    }

    /// 机器菜单「中继…」弹层：和改名同款，多一行没改成的原因。在线和离线改的东西不一样，
    /// 说明里讲清楚。
    pub(crate) fn render_relay_edit(&self, cx: &mut Context<Self>) -> AnyElement {
        let (fg, muted, danger) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground, t.danger)
        };
        let Some(edit) = self.relay_edit.as_ref() else {
            return div().into_any_element();
        };
        let key = crate::RuntimeKey::Remote(edit.runtime_id.clone());
        let hint = if crate::runtimes::runtime_online(&key, cx) {
            "那台机器会改用这个中继，这台电脑也跟着改。填自建 iroh relay 的域名、IP 或 URL；\
             留空用 n0 的公共 relay。"
        } else {
            "那台机器现在连不上，只改这台电脑这边：那台机器要已经用 smeltd relay 改成同一个。"
        };
        let content = v_flex()
            .child(Self::modal_title(
                fg,
                format!("{} 的中继", crate::runtimes::runtime_name(&key, cx)),
            ))
            .child(div().text_sm().text_color(muted).child(hint))
            .child(Input::new(&edit.input).disabled(edit.saving))
            .children(
                edit.error
                    .clone()
                    .map(|error| div().text_sm().text_color(danger).child(error)),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(Self::modal_normal_button(
                        "cancel-relay-edit",
                        "取消",
                        |this, _, _, cx| this.cancel_relay_edit(cx),
                        cx,
                    ))
                    .child(Self::modal_commit_button(
                        "confirm-relay-edit",
                        if edit.saving {
                            "正在保存…"
                        } else {
                            "保存"
                        },
                        |this, _, _, cx| this.confirm_relay_edit(cx),
                        cx,
                    )),
            );
        self.modal_shell(
            380.,
            false,
            content,
            crate::overlay::ModalLayer::RelayEdit,
            cx,
        )
    }

    /// 「重启守护进程」二次确认弹窗：明确告知会断开所有当前终端会话。与
    /// render_quit_confirm 同款视觉（居中卡片 + 半透明遮罩）。
    ///
    /// 入口只在设置窗「更新」页；弹层挂在设置窗上（见 `SettingsWindow::render`），
    /// 不再画到主窗口，避免「按钮在设置、确认框跑到主界面」的割裂感。
    pub(crate) fn render_daemon_restart_confirm(&self, cx: &mut Context<Self>) -> AnyElement {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };

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
                    .child(Self::modal_normal_button(
                        "cancel-daemon-restart",
                        "取消",
                        |this, _, _, cx| {
                            this.show_daemon_restart_confirm = false;
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(Self::modal_danger_button(
"confirm-daemon-restart",
"确定重启",
|this, _, _, cx| {
                            this.confirm_restart_daemon(cx);
                            #[cfg(not(unix))]
                            {
                                let _ = (this, cx);
                            }
                        },
cx,
)),
            );
        self.modal_shell(
            320.,
            true,
            content,
            crate::overlay::ModalLayer::DaemonRestart,
            cx,
        )
    }

    /// 当前文件有未保存改动、又点了别的文件时弹的确认弹窗：取消 / 不保存直接切换 /
    /// 保存并切换。与 render_quit_confirm 同款视觉（居中卡片 + 半透明遮罩）。
    pub(crate) fn render_unsaved_file_confirm(
        &self,
        target: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };
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
                    .child(Self::modal_normal_button(
                        "unsaved-cancel",
                        "取消",
                        |this, _, _, cx| {
                            this.pending_file_switch = None;
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(Self::modal_normal_button(
                        "unsaved-discard",
                        "不保存，直接切换",
                        |this, _, window, cx| {
                            if let Some(target) = this.pending_file_switch.take() {
                                this.open_file_now(target, None, window, cx);
                            }
                        },
                        cx,
                    ))
                    .child(Self::modal_commit_button(
                        "unsaved-save-switch",
                        "保存并切换",
                        |this, _, _, cx| {
                            if let Some(target) = this.pending_file_switch.take() {
                                this.pending_switch_after_save = Some(target);
                                this.save_open_file(cx);
                            }
                        },
                        cx,
                    )),
            );
        self.modal_shell(
            360.,
            true,
            content,
            crate::overlay::ModalLayer::UnsavedFile,
            cx,
        )
    }

    /// 智能体的 space 里只可能有底层引擎自己的历史，别家的在这个目录下不可能
    /// 存在。返回 `Some(kind)` 表示当前处在某个智能体的上下文里。
    pub(crate) fn history_engine_override(
        &self,
        cwd: Option<&str>,
        cx: &App,
    ) -> Option<crate::settings::ConversationAgentKind> {
        let id =
            smelt_model::agent_paths::agent_definition_id_for_space(std::path::Path::new(cwd?))?;
        // 历史只在本机。
        crate::settings::agent_hosts(cx)?
            .agent(&crate::RuntimeKey::Local, &id)
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
        let (sessions, row_text, row_keys) =
            match list_key.as_ref().and_then(|key| self.session_list.get(key)) {
                Some((_, list)) => (
                    Some(list.sessions.clone()),
                    list.row_text.clone(),
                    list.row_keys.clone(),
                ),
                None => (
                    None,
                    std::rc::Rc::new(Vec::new()),
                    std::rc::Rc::new(Vec::new()),
                ),
            };
        let list_state = match (cwd.as_ref(), sessions) {
            (None, _) => match self.remote_project_notice(cx) {
                Some(notice) => HistoryListState::RemoteProject(notice),
                None => HistoryListState::NoProject,
            },
            (Some(_), None) => HistoryListState::Loading,
            (Some(_), Some(s)) if s.is_empty() => HistoryListState::Empty,
            (Some(_), Some(s)) => HistoryListState::Ready(s),
        };
        let (source_tabs, launch_override, profile_label, history_agent_ids) = {
            // 历史只在本机，profile 和智能体定义也按本机的认。
            let config = crate::settings::agent_host_settings(&crate::RuntimeKey::Local, cx);
            let source_tabs = crate::session_history::tabs_with_sessions(
                crate::session_history::history_source_tabs(
                    restrict_to_kind,
                    config.profiles.iter().cloned(),
                ),
                (agent, profile_id.as_deref()),
                |tab| {
                    cwd.as_deref().is_some_and(|cwd| {
                        let key = crate::session_history::session_list_key(
                            tab.kind,
                            tab.profile_id.as_deref(),
                            cwd,
                        );
                        self.session_list
                            .get(&key)
                            .is_some_and(|(_, list)| !list.sessions.is_empty())
                    })
                },
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
            let mut history_agent_ids = list_key
                .as_ref()
                .and_then(|key| self.history_agent_bindings.get(key))
                .map(|bindings| bindings.as_ref().clone())
                .unwrap_or_default();
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
                row_text,
                row_keys,
            },
            cx,
        )
        .into_any_element()
    }
}
