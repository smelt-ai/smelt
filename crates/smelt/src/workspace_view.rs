//! 宿主对话框的内容：关闭确认、会话管理、退出、改名、中继、重启确认。
//!
//! 只组 UI，什么时候开关见 `host_dialog`。打开/关闭项目、升级守护、改名等写状态的方法
//! 不在这里。

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::dialog::Dialog;
use gpui_component::input::Input;
use gpui_component::*;
use smelt_ui::truncated_line::truncated_line;

use crate::{CloseConfirm, RenameTarget, Workspace, request_app_quit, ui_theme};
#[cfg(any(target_os = "macos", windows))]
use crate::{UpdateInstallTrigger, updater};

enum ModalRole {
    Quiet,
    Secondary,
    Primary,
    Danger,
}

/// 退出确认要说的话。普通退出仍保留后台守护；待装更新的退出会停掉守护。
struct QuitPrompt {
    title: SharedString,
    body: SharedString,
    confirm: SharedString,
    /// 停守护会打断进行中的会话：危险按钮，Enter 不执行。
    danger: bool,
    width: f32,
}

impl Workspace {
    fn render_restore_workset(
        &self,
        dialog: Dialog,
        runtime: &crate::RuntimeKey,
        old_root: &str,
        pending: bool,
        error: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Dialog {
        let style = crate::runtimes::path_style(runtime, cx);
        let project_root = style.parent(old_root).and_then(|path| style.parent(&path));
        let worksets = self
            .space(runtime)
            .into_iter()
            .flat_map(|space| &space.worksets)
            .filter(|workset| {
                project_root
                    .as_ref()
                    .is_some_and(|root| style.clean(root) == style.clean(&workset.project_root))
            })
            .cloned()
            .collect::<Vec<_>>();
        let muted = cx.theme().muted_foreground;
        let mut content = smelt_ui::dialog::content(
            "原工作树已关闭",
            Some(format!("原目录：{old_root}").into()),
        )
        .child(div().text_sm().child(
            "请选择原项目的工作树。恢复后终端和智能体的工作目录将改为所选工作树根目录，工作树外已打开的文件及固定目录不会恢复；智能体能否续接取决于其服务商。"
        ));
        if worksets.is_empty() {
            content = content.child(div().text_sm().child("没有可用工作树，请先新建。"));
        }
        let mut list = div()
            .id("restore-workset-list")
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(240.))
            .overflow_y_scroll();
        for workset in worksets {
            let workset_id = workset.id.clone();
            let label = format!("{} · {}", workset.task, workset.branch);
            list = list.child(
                smelt_ui::Button::new(SharedString::from(format!(
                    "restore-workset-{}",
                    workset.id
                )))
                .label(label)
                .disabled(pending)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.restore_archived_into_workset(workset_id.clone(), cx);
                })),
            );
        }
        content = content.child(list);
        if let Some(message) = error {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(message.to_string()),
            );
        }
        content = content.child(
            div()
                .text_xs()
                .text_color(muted)
                .child("新建完成后，请再次点击归档会话的恢复按钮。"),
        );
        let runtime = runtime.clone();
        let footer = smelt_ui::dialog::footer()
            .child(
                Self::modal_cancel_button(
                    "restore-workset-cancel",
                    "取消",
                    |this, _, _, cx| this.cancel_close_confirm(cx),
                    cx,
                )
                .disabled(pending),
            )
            .child(
                Self::modal_secondary_button(
                    "restore-workset-create",
                    "新建工作树…",
                    move |this, _, window, cx| {
                        let Some(root) = project_root.clone() else {
                            if let Some(CloseConfirm::RestoreWorkset { error, .. }) =
                                &mut this.close_confirm
                            {
                                *error = Some("无法识别原项目目录".into());
                            }
                            cx.notify();
                            return;
                        };
                        let Some(target_runtime) =
                            this.space(&runtime).map(|space| space.runtime.clone())
                        else {
                            if let Some(CloseConfirm::RestoreWorkset { error, .. }) =
                                &mut this.close_confirm
                            {
                                *error = Some("运行时已断开，无法新建工作树".into());
                            }
                            cx.notify();
                            return;
                        };
                        this.close_confirm = None;
                        this.open_project_repositories(target_runtime, root, true, window, cx);
                    },
                    cx,
                )
                .disabled(pending),
            );
        dialog.w(px(460.)).child(content).footer(footer)
    }

    /// 关闭确认：关项目说清会连带关掉几个会话；关正在跑的会话说清这一轮会被终止。
    /// 用危险配色。
    pub(crate) fn render_close_confirm(&self, dialog: Dialog, cx: &mut Context<Self>) -> Dialog {
        let Some(target) = self.close_confirm.clone() else {
            return dialog;
        };
        if let CloseConfirm::RestoreWorkset {
            runtime,
            old_root,
            pending,
            error,
            ..
        } = &target
        {
            return self.render_restore_workset(
                dialog,
                runtime,
                old_root,
                *pending,
                error.as_deref(),
                cx,
            );
        }
        let muted = cx.theme().muted_foreground;
        let (title, body, confirm_label) = match &target {
            CloseConfirm::Archive { title, .. } => (
                "确定归档这个会话吗？",
                format!(
                    "「{title}」正在运行或等待处理。归档会停止进程，对话记录和启动参数会保留；恢复终端将启动新 shell，不会恢复原进程。"
                ),
                "归档",
            ),
            CloseConfirm::DeleteArchived { title, .. } => (
                "确定永久删除归档会话吗？",
                format!(
                    "「{title}」的 Smelt 归档记录和续接身份将被永久删除，无法再从归档恢复。不会删除项目文件或 Copilot、Claude 等工具自行保存的历史。"
                ),
                "永久删除",
            ),
            CloseConfirm::Project {
                label, sessions, ..
            } => (
                "确定关闭这个项目吗？",
                format!(
                    "「{label}」下的 {sessions} 个会话会被一起关掉，终端里正在跑的东西会被终止。项目本身只是从工作台移走，磁盘上的目录不动。"
                ),
                "关闭项目",
            ),
            CloseConfirm::Workset { title, .. } => (
                "确定关闭这个工作树吗？",
                format!(
                    "将关闭并删除「{title}」的工作树目录及其创建的已合并分支，复用的分支会保留。Git 忽略的文件也可能被删除。未归档会话、未提交修改或未合并分支会阻止关闭。归档会话将保留，恢复时需选择工作树。"
                ),
                "关闭工作树",
            ),
            CloseConfirm::RestoreWorkset { .. } => unreachable!(),
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

        let pending = matches!(&target, CloseConfirm::Workset { pending: true, .. });
        let error = match &target {
            CloseConfirm::Workset { error, .. } => error.as_ref(),
            _ => None,
        };
        let content = smelt_ui::dialog::content(title, Some(body.into()))
            .children(error.map(|error| {
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error.clone())
            }))
            .children(error.map(|_| {
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("请处理问题后取消并重新打开此弹窗，再尝试关闭。")
            }));
        let footer = smelt_ui::dialog::footer()
            .child(
                Self::modal_cancel_button(
                    "cancel-close-confirm",
                    "取消",
                    |this, _, _, cx| this.cancel_close_confirm(cx),
                    cx,
                )
                .disabled(pending),
            )
            .child(
                Self::modal_danger_button(
                    "confirm-close-confirm",
                    if pending {
                        "关闭中…"
                    } else {
                        confirm_label
                    },
                    |this, _, window, cx| this.confirm_close(window, cx),
                    cx,
                )
                .disabled(target.confirmation_blocked()),
            );
        dialog
            .w(px(smelt_ui::dialog::CONFIRM_WIDTH))
            .child(content)
            .footer(footer)
    }

    /// 「会话管理」：列出守护进程持有的全部会话，标出哪些是游离会话（没有任何
    /// 侧栏在追踪），逐个/批量清理。入口和对话框都在设置窗口里。
    pub(crate) fn render_session_manager(&self, dialog: Dialog, cx: &mut Context<Self>) -> Dialog {
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
                                        rgb(ui_theme::bad())
                                    } else {
                                        rgb(ui_theme::ok())
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
                                            .overflow_hidden()
                                            .tooltip({
                                                let tip = label.clone();
                                                move |window, cx| {
                                                    gpui_component::tooltip::Tooltip::new(
                                                        tip.clone(),
                                                    )
                                                    .build(window, cx)
                                                }
                                            })
                                            .child(truncated_line(label.clone())),
                                    )
                                    .children(is_detached.then(|| {
                                        div()
                                            .text_xs()
                                            .flex_shrink_0()
                                            .text_color(rgb(ui_theme::bad()))
                                            .child("游离会话（无侧栏追踪）")
                                    })),
                            )
                            .child(Self::modal_secondary_button(
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

        let content = smelt_ui::dialog::content(
            "会话管理",
            Some("守护进程持有的全部会话；游离会话是没有被任何窗口侧栏追踪的（测试跑出来的、忘了关的临时会话），清理它们不影响正常使用中的会话。".into()),
        )
        .child(div().border_t_1().border_color(border).pt_3().child(body));
        let footer = smelt_ui::dialog::footer()
            .child(Self::modal_cancel_button(
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
            });
        dialog.w(px(520.)).child(content).footer(footer)
    }

    /// 对话框里退出这一步的按钮：取消、返回、关闭、以后再说。
    pub(crate) fn modal_cancel_button(
        id: &'static str,
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> smelt_ui::Button {
        Self::modal_role_button(id, label, ModalRole::Quiet, on_click, cx)
    }

    /// 对话框里和主操作并列的其它选择。
    pub(crate) fn modal_secondary_button(
        id: &'static str,
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> smelt_ui::Button {
        Self::modal_role_button(id, label, ModalRole::Secondary, on_click, cx)
    }

    /// 这块确认里唯一的默认提交（退出、改名、选目录）。
    pub(crate) fn modal_primary_button(
        id: &'static str,
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> smelt_ui::Button {
        Self::modal_role_button(id, label, ModalRole::Primary, on_click, cx)
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

    /// 按钮只改 Workspace 上的状态；对话框跟着状态关，见 `host_dialog`。
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
            .large()
            .label(label.clone())
            .accessibility_label(label);
        let button = match role {
            ModalRole::Quiet => button.quiet(),
            ModalRole::Secondary => button.secondary(),
            ModalRole::Primary => button.primary(),
            ModalRole::Danger => button.danger(),
        };
        button.on_click(move |event, window, cx| {
            let Some(host) = host.upgrade() else {
                return;
            };
            host.update(cx, |this, cx| on_click(this, event, window, cx));
        })
    }

    fn quit_prompt(&self) -> QuitPrompt {
        #[cfg(any(target_os = "macos", windows))]
        let update_is_applying =
            matches!(self.update_status, updater::UpdateStatus::Applying { .. });
        #[cfg(not(any(target_os = "macos", windows)))]
        let update_is_applying = false;
        // 「退出并安装」的提示讲的是停守护；不停守护的平台照普通退出说。
        #[cfg(any(target_os = "macos", windows))]
        let installing = updater::INSTALL_STOPS_LOCAL_DAEMON
            && !update_is_applying
            && self.update_status.ready_update().is_some();
        #[cfg(not(any(target_os = "macos", windows)))]
        let installing = false;
        let prompt = super::daemon_restart_prompt::daemon_stop_prompt(
            super::daemon_restart_prompt::DaemonStopReason::QuitToInstall,
            &self.stop_prompt_names,
        );
        QuitPrompt {
            title: if installing {
                prompt.title.into()
            } else {
                "退出 Smelt？".into()
            },
            body: if update_is_applying {
                "正在安全应用更新，完成后会自动重启。".into()
            } else if installing {
                prompt.body.into()
            } else {
                "后台会话将继续运行，当前连接会断开。".into()
            },
            confirm: if self.quit_requested {
                "正在退出…".into()
            } else if update_is_applying {
                "正在更新…".into()
            } else if installing {
                prompt.confirm.into()
            } else {
                "退出".into()
            },
            danger: installing && prompt.danger,
            width: if installing {
                420.
            } else {
                smelt_ui::dialog::CONFIRM_WIDTH
            },
        }
    }

    /// 退出确认里的危险版本（停守护打断会话）Enter 不执行。
    pub(crate) fn quit_confirm_is_danger(&self) -> bool {
        self.quit_prompt().danger
    }

    /// 确认退出。待装更新时退出只是统一安装驱动的一种触发策略：helper 派生成功后退出，
    /// 并在本进程完全消失后才提交同一份事务。
    pub(crate) fn confirm_quit(&mut self, cx: &mut Context<Self>) {
        if self.quit_requested {
            return;
        }
        // Applying 表示 helper 已派生且当前 GUI 正在退出，不能再启动第二条退出或安装流程。
        #[cfg(any(target_os = "macos", windows))]
        if matches!(self.update_status, updater::UpdateStatus::Applying { .. }) {
            return;
        }
        self.quit_requested = true;
        cx.notify();
        #[cfg(any(target_os = "macos", windows))]
        if let Some(update) = self.update_status.ready_update().cloned() {
            self.start_update_install(update, UpdateInstallTrigger::Quit, cx);
            return;
        }
        request_app_quit(cx);
    }

    /// 退出确认。普通退出仍保留后台守护；待装更新的退出会停掉守护，
    /// 只有存在进行中的会话时才走到这里。
    pub(crate) fn render_quit_confirm(&self, dialog: Dialog, cx: &mut Context<Self>) -> Dialog {
        let prompt = self.quit_prompt();
        let on_confirm =
            |this: &mut Self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                this.confirm_quit(cx)
            };
        let confirm = if prompt.danger {
            Self::modal_danger_button("confirm-quit", prompt.confirm, on_confirm, cx)
        } else {
            Self::modal_primary_button("confirm-quit", prompt.confirm, on_confirm, cx)
        };
        let footer = smelt_ui::dialog::footer()
            .child(
                Self::modal_cancel_button(
                    "cancel-quit",
                    "取消",
                    |this, _, _, cx| {
                        this.show_quit_confirm = false;
                        cx.notify();
                    },
                    cx,
                )
                .disabled(self.quit_requested),
            )
            .child(confirm);
        dialog
            .w(px(prompt.width))
            .child(smelt_ui::dialog::content(prompt.title, Some(prompt.body)))
            .footer(footer)
    }

    /// 启动时提议把 App 移到「应用程序」：被系统转移或放在只读位置时更新装不上。
    /// 「以后再说」只关这一次，设置页的更新栏里一直留着同一个按钮。
    /// 只在 macOS 上存在（设计文档 D4）。
    #[cfg(target_os = "macos")]
    pub(crate) fn render_move_app_confirm(&self, dialog: Dialog, cx: &mut Context<Self>) -> Dialog {
        let danger = cx.theme().danger;
        let body = match self.app_location_issue {
            Some(smelt_client::app_location::LocationIssue::ReadOnly) => {
                "Smelt 正从只读位置（比如安装盘映像）运行，自动更新装不上。移过去后会自动重新打开，后台会话不受影响。"
            }
            _ => {
                "Smelt 没放进「应用程序」就打开了，macOS 让它在一个临时的只读位置运行，自动更新因此装不上。移过去后会自动重新打开，后台会话不受影响。"
            }
        };
        let content = smelt_ui::dialog::content("把 Smelt 移到「应用程序」？", Some(body.into()))
            .when_some(self.move_app_error.clone(), |content, error| {
                content.child(div().text_sm().text_color(danger).child(error))
            });
        let footer = smelt_ui::dialog::footer()
            .child(
                Self::modal_cancel_button(
                    "move-app-later",
                    "以后再说",
                    |this, _, _, cx| {
                        this.show_move_app_confirm = false;
                        cx.notify();
                    },
                    cx,
                )
                .disabled(self.moving_app),
            )
            .child(Self::modal_primary_button(
                "move-app-confirm",
                if self.moving_app {
                    "正在移动…"
                } else {
                    "移过去并重新打开"
                },
                |this, _, _, cx| this.move_app_to_applications(cx),
                cx,
            ));
        dialog
            .w(px(smelt_ui::dialog::CONFIRM_WIDTH))
            .child(content)
            .footer(footer)
    }

    /// 改名对话框：工作树和机器。会话行、分屏子行的改名画在侧栏那一行里。
    pub(crate) fn render_rename(&self, dialog: Dialog, cx: &mut Context<Self>) -> Dialog {
        let Some(input) = self.rename_input.as_ref() else {
            return dialog;
        };
        let (heading, hint) = match self.rename_target {
            Some(RenameTarget::Runtime(_)) => (
                "重命名机器",
                "侧栏和设置里显示这个名字，只在这台电脑上生效。",
            ),
            Some(RenameTarget::Workset(_)) => {
                ("重命名工作树", "会同步重命名工作树目录，分支名保持不变。")
            }
            Some(RenameTarget::Pane(_)) => ("重命名终端", "留空则恢复自动识别的标题。"),
            _ => ("重命名会话", "留空则恢复自动识别的标题。"),
        };
        let footer = smelt_ui::dialog::footer()
            .child(Self::modal_cancel_button(
                "cancel-rename",
                "取消",
                |this, _, _, cx| this.cancel_rename(cx),
                cx,
            ))
            .child(Self::modal_primary_button(
                "confirm-rename",
                "重命名",
                |this, _, window, cx| this.confirm_rename(window, cx),
                cx,
            ));
        dialog
            .w(px(smelt_ui::dialog::CONFIRM_WIDTH))
            .child(smelt_ui::dialog::content(heading, Some(hint.into())).child(Input::new(input)))
            .footer(footer)
    }

    /// 机器菜单「中继…」：和改名同款，多一行没改成的原因。在线和离线改的东西不一样，
    /// 说明里讲清楚。
    pub(crate) fn render_relay_edit(&self, dialog: Dialog, cx: &mut Context<Self>) -> Dialog {
        let danger = cx.theme().danger;
        let Some(edit) = self.relay_edit.as_ref() else {
            return dialog;
        };
        let key = crate::RuntimeKey::Remote(edit.runtime_id.clone());
        let hint = if crate::runtimes::runtime_online(&key, cx) {
            "那台机器会改用这个中继，这台电脑也跟着改。填自建 iroh relay 的域名、IP 或 URL；\
             留空用 n0 的公共 relay。"
        } else {
            "那台机器现在连不上，只改这台电脑这边：那台机器要已经用 smeltd relay 改成同一个。"
        };
        let content = smelt_ui::dialog::content(
            format!("{} 的中继", crate::runtimes::runtime_name(&key, cx)),
            Some(hint.into()),
        )
        .child(Input::new(&edit.input).disabled(edit.saving))
        .children(
            edit.error
                .clone()
                .map(|error| div().text_sm().text_color(danger).child(error)),
        );
        let footer = smelt_ui::dialog::footer()
            .child(Self::modal_cancel_button(
                "cancel-relay-edit",
                "取消",
                |this, _, _, cx| this.cancel_relay_edit(cx),
                cx,
            ))
            .child(Self::modal_primary_button(
                "confirm-relay-edit",
                if edit.saving {
                    "正在保存…"
                } else {
                    "保存"
                },
                |this, _, _, cx| this.confirm_relay_edit(cx),
                cx,
            ));
        dialog
            .w(px(smelt_ui::dialog::CONFIRM_WIDTH))
            .child(content)
            .footer(footer)
    }

    /// 「重启守护进程」确认。只在有进行中的会话时打开，在设置窗口里。
    pub(crate) fn render_daemon_restart_confirm(
        &self,
        dialog: Dialog,
        cx: &mut Context<Self>,
    ) -> Dialog {
        let prompt = super::daemon_restart_prompt::daemon_stop_prompt(
            super::daemon_restart_prompt::DaemonStopReason::ExplicitRestart,
            &self.stop_prompt_names,
        );
        let footer = smelt_ui::dialog::footer()
            .child(Self::modal_cancel_button(
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
                prompt.confirm,
                |this, _, _, cx| this.confirm_restart_daemon(cx),
                cx,
            ));
        dialog
            .w(px(smelt_ui::dialog::CONFIRM_WIDTH))
            .child(smelt_ui::dialog::content(
                prompt.title,
                Some(prompt.body.into()),
            ))
            .footer(footer)
    }

    /// 「立即重启更新」确认。有进行中的会话时打开，在设置窗口里。
    pub(crate) fn render_update_install_confirm(
        &self,
        dialog: Dialog,
        cx: &mut Context<Self>,
    ) -> Dialog {
        let prompt = super::daemon_restart_prompt::daemon_stop_prompt(
            super::daemon_restart_prompt::DaemonStopReason::UpdateInstall,
            &self.stop_prompt_names,
        );
        let footer = smelt_ui::dialog::footer()
            .child(Self::modal_cancel_button(
                "cancel-update-install",
                "取消",
                |this, _, _, cx| {
                    this.show_update_install_confirm = false;
                    cx.notify();
                },
                cx,
            ))
            .child(Self::modal_danger_button(
                "confirm-update-install",
                prompt.confirm,
                |this, _, _, cx| this.confirm_update_install(cx),
                cx,
            ));
        dialog
            .w(px(smelt_ui::dialog::CONFIRM_WIDTH))
            .child(smelt_ui::dialog::content(
                prompt.title,
                Some(prompt.body.into()),
            ))
            .footer(footer)
    }
}

/// 项目是不是某个智能体的 space，是的话返回智能体 id。space 在那台机器的 `SMELT_HOME`
/// 下面，远程项目要拿那台机器的主目录去比。
pub(crate) fn space_agent_id(project: &crate::ProjectRef, cx: &App) -> Option<String> {
    match &project.runtime {
        crate::RuntimeKey::Local => smelt_model::agent_paths::agent_definition_id_for_space(
            std::path::Path::new(&project.root),
        ),
        remote => smelt_model::agent_paths::agent_definition_id_for_space_in(
            &crate::runtimes::smelt_home(remote, cx)?,
            &project.root,
        ),
    }
}
