//! 宿主自己的对话框：退出、改名、中继、关闭确认、选目录、仓库、会话管理、重启确认。
//!
//! 和插件的对话框是同一套：kit 的窗口对话框层（`window.open_dialog`），同样的外观、
//! Esc / Enter、焦点陷阱和关掉后焦点回到原处，叠起来时也只有最上面那个能操作。
//!
//! 开没开着由 Workspace 上各自的状态说了算（`show_quit_confirm`、`close_confirm`……），
//! 对话框只是它的样子。改状态的地方不管对话框：不少是异步回来的、手上没有窗口。
//! Workspace 每次通知后 [`Workspace::sync_host_dialogs`] 对一遍，状态有了就开，没了
//! 就关。用户在对话框里关（Esc、取消、Enter 关危险确认）走 kit，`on_close` 再把状态清掉。
//!
//! kit 只会关最上面那个对话框，没有按 id 关的接口。所以状态没了的对话框要等它在
//! 宿主对话框里排到最上面才关，在那之前不画。插件的对话框若叠在宿主对话框上面，
//! 这时宿主那个的状态没了，关掉的会是插件那个；宿主对话框盖着整个窗口，插件只能从
//! 快捷键或后台任务开出对话框，暂且接受。

use gpui::{App, Context, Styled as _, Window};
use gpui_component::WindowExt as _;
use gpui_component::dialog::{Dialog, DialogEntrance};

use super::*;

/// 一种宿主对话框。同一种同时只开一个，内容跟着状态变。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HostDialog {
    Rename,
    RelayEdit,
    ProjectRepositories,
    SessionManager,
    Quit,
    DaemonRestart,
    CloseConfirm,
    UpdateInstall,
    #[cfg(target_os = "macos")]
    MoveApp,
    DirPicker,
}

impl HostDialog {
    /// 同一轮里几个一起要开时按这个次序，后开的叠在上面。选目录总是从别的对话框里
    /// 点出来的，放最后。
    const ALL: &[HostDialog] = &[
        Self::Rename,
        Self::RelayEdit,
        Self::ProjectRepositories,
        Self::SessionManager,
        Self::Quit,
        Self::DaemonRestart,
        Self::CloseConfirm,
        Self::UpdateInstall,
        #[cfg(target_os = "macos")]
        Self::MoveApp,
        Self::DirPicker,
    ];
}

impl Workspace {
    /// 这种对话框的状态在不在。
    fn host_dialog_live(&self, kind: HostDialog) -> bool {
        match kind {
            HostDialog::Quit => self.show_quit_confirm,
            #[cfg(target_os = "macos")]
            HostDialog::MoveApp => self.show_move_app_confirm,
            // 侧栏行上的改名画在那一行里（见 `RenameTarget::is_inline`），不是对话框。
            HostDialog::Rename => {
                self.rename_input.is_some()
                    && self
                        .rename_target
                        .as_ref()
                        .is_some_and(|target| !target.is_inline())
            }
            HostDialog::RelayEdit => self.relay_edit.is_some(),
            HostDialog::CloseConfirm => self.close_confirm.is_some(),
            HostDialog::ProjectRepositories => self.project_repositories.is_some(),
            HostDialog::DirPicker => self.dir_picker.is_some(),
            HostDialog::SessionManager => self.session_manager_open,
            HostDialog::DaemonRestart => self.show_daemon_restart_confirm,
            HostDialog::UpdateInstall => self.show_update_install_confirm,
        }
    }

    /// 现在能不能取消。手上的事撤不回时（正在退出、正在移动、正在关工作树）不能。
    fn host_dialog_cancellable(&self, kind: HostDialog) -> bool {
        match kind {
            HostDialog::Quit => !self.quit_requested,
            #[cfg(target_os = "macos")]
            HostDialog::MoveApp => !self.moving_app,
            HostDialog::CloseConfirm => !matches!(
                self.close_confirm,
                Some(
                    CloseConfirm::Workset { pending: true, .. }
                        | CloseConfirm::RestoreWorkset { pending: true, .. }
                )
            ),
            HostDialog::ProjectRepositories => !self.project_repositories_busy(),
            // 中继已经提交的照常做完，只是不再等结果。
            HostDialog::Rename
            | HostDialog::RelayEdit
            | HostDialog::DirPicker
            | HostDialog::SessionManager
            | HostDialog::DaemonRestart
            | HostDialog::UpdateInstall => true,
        }
    }

    /// 用户把它关了，或者它开着的窗口没了：清掉状态，什么都不做。
    fn dismiss_host_dialog(&mut self, kind: HostDialog, cx: &mut Context<Self>) {
        match kind {
            HostDialog::Quit => self.show_quit_confirm = false,
            #[cfg(target_os = "macos")]
            HostDialog::MoveApp => self.show_move_app_confirm = false,
            HostDialog::Rename => self.cancel_rename(cx),
            HostDialog::RelayEdit => self.cancel_relay_edit(cx),
            HostDialog::CloseConfirm => self.close_confirm = None,
            HostDialog::ProjectRepositories => self.close_project_repositories(cx),
            HostDialog::DirPicker => self.cancel_dir_picker(cx),
            HostDialog::SessionManager => {
                self.session_manager_open = false;
                self.session_manager_tombstones.clear();
            }
            HostDialog::DaemonRestart => self.show_daemon_restart_confirm = false,
            HostDialog::UpdateInstall => self.show_update_install_confirm = false,
        }
        cx.notify();
    }

    /// Enter（焦点不在输入框、按钮上时）。普通确认执行默认按钮，关不关看状态；危险确认
    /// 不执行，Enter 等于取消（macOS 的惯例，和插件的 `smelt_ui::dialog::Confirm` 一样）。
    /// 返回 true 让 kit 当取消关掉。
    fn host_dialog_enter(
        &mut self,
        kind: HostDialog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match kind {
            HostDialog::Quit if self.quit_confirm_is_danger() => {
                return self.host_dialog_cancellable(kind);
            }
            HostDialog::Quit => self.confirm_quit(cx),
            #[cfg(target_os = "macos")]
            HostDialog::MoveApp => self.move_app_to_applications(cx),
            HostDialog::Rename => self.confirm_rename(window, cx),
            HostDialog::RelayEdit => self.confirm_relay_edit(cx),
            HostDialog::DirPicker => self.dir_picker_confirm(cx),
            // 恢复归档要在列表里点一个工作树，Enter 没有默认的那个。
            HostDialog::CloseConfirm
                if matches!(
                    self.close_confirm,
                    Some(CloseConfirm::RestoreWorkset { .. })
                ) => {}
            HostDialog::CloseConfirm | HostDialog::DaemonRestart | HostDialog::UpdateInstall => {
                return self.host_dialog_cancellable(kind);
            }
            // 表单和列表，没有一个说得清的默认操作。
            HostDialog::ProjectRepositories | HostDialog::SessionManager => {}
        }
        false
    }

    /// 宿主对话框跟着状态开关。Workspace 每次通知后跑（见 `note_overlay`）。
    ///
    /// 在 observer 里跑，这时没有窗口被借着，可以按句柄进窗口开关对话框。
    pub(crate) fn sync_host_dialogs(&mut self, cx: &mut Context<Self>) {
        if self.host_dialogs.is_empty()
            && !HostDialog::ALL
                .iter()
                .any(|&kind| self.host_dialog_live(kind))
        {
            return;
        }

        // 状态没了的，从上往下关。kit 只关最上面那个，所以只关最上面的宿主对话框，压在
        // 底下的等上面的关掉再说（这期间不画，见 `build_host_dialog`）。
        let window = self.window_handle;
        while let Some(&top) = self.host_dialogs.last() {
            if self.host_dialog_live(top) {
                break;
            }
            self.host_dialogs.pop();
            let _ = window.update(cx, |_, window, cx| window.close_dialog(cx));
        }

        for &kind in HostDialog::ALL {
            if !self.host_dialog_live(kind) || self.host_dialogs.contains(&kind) {
                continue;
            }
            let workspace = cx.weak_entity();
            let opened = window
                .update(cx, |_, window, cx| {
                    window.open_dialog(cx, move |dialog, _, cx| {
                        let Some(workspace) = workspace.upgrade() else {
                            return dialog.entrance(DialogEntrance::None).invisible();
                        };
                        workspace.update(cx, |workspace, cx| {
                            workspace.build_host_dialog(kind, dialog, cx)
                        })
                    });
                    self.focus_host_dialog(kind, window, cx);
                })
                .is_ok();
            if opened {
                self.host_dialogs.push(kind);
            } else {
                // 主窗口没了：当作没开过。
                self.dismiss_host_dialog(kind, cx);
            }
        }
    }

    /// 刚开出来时焦点放哪。kit 已经把焦点交给对话框本身，有输入框的再往里放一层。
    /// 输入框要等对话框开好再聚焦：kit 记的「打开前的焦点」是关掉后回去的地方，不能是它。
    fn focus_host_dialog(&self, kind: HostDialog, window: &mut Window, cx: &mut App) {
        match kind {
            HostDialog::Rename => {
                if let Some(input) = &self.rename_input {
                    // 名字整段选中：直接打字就是换掉，按方向键才是在原名上改。
                    input.update(cx, |input, cx| {
                        input.focus(window, cx);
                        input.select_all(window, cx);
                    });
                }
            }
            HostDialog::RelayEdit => {
                if let Some(edit) = &self.relay_edit {
                    edit.input.update(cx, |input, cx| input.focus(window, cx));
                }
            }
            _ => {}
        }
    }

    /// 每帧由 kit 调：按状态把对话框画出来。
    fn build_host_dialog(
        &mut self,
        kind: HostDialog,
        dialog: Dialog,
        cx: &mut Context<Self>,
    ) -> Dialog {
        // 状态没了：正在关（没有内容可演退场，当场消失），或压在底下等着关。
        if !self.host_dialog_live(kind) {
            return dialog.entrance(DialogEntrance::None).invisible();
        }
        let workspace = cx.weak_entity();
        let on_ok = workspace.clone();
        let on_cancel = workspace.clone();
        let dialog = smelt_ui::dialog::present(dialog)
            .close_button(false)
            .overlay_closable(false)
            .on_ok(move |_, window, cx| {
                on_ok
                    .update(cx, |this, cx| this.host_dialog_enter(kind, window, cx))
                    .unwrap_or(true)
            })
            .on_cancel(move |_, _, cx| {
                on_cancel
                    .read_with(cx, |this, _| this.host_dialog_cancellable(kind))
                    .unwrap_or(true)
            })
            .on_close(move |_, _, cx| {
                let _ = workspace.update(cx, |this, cx| {
                    // kit 已经关了它，同步时不能再关一次（会关掉底下那个）。
                    this.host_dialogs.retain(|&open| open != kind);
                    this.dismiss_host_dialog(kind, cx);
                });
            });
        match kind {
            HostDialog::Quit => self.render_quit_confirm(dialog, cx),
            #[cfg(target_os = "macos")]
            HostDialog::MoveApp => self.render_move_app_confirm(dialog, cx),
            HostDialog::Rename => self.render_rename(dialog, cx),
            HostDialog::RelayEdit => self.render_relay_edit(dialog, cx),
            HostDialog::CloseConfirm => self.render_close_confirm(dialog, cx),
            HostDialog::ProjectRepositories => self.render_project_repositories(dialog, cx),
            HostDialog::DirPicker => self.render_dir_picker(dialog, cx),
            HostDialog::SessionManager => self.render_session_manager(dialog, cx),
            HostDialog::DaemonRestart => self.render_daemon_restart_confirm(dialog, cx),
            HostDialog::UpdateInstall => self.render_update_install_confirm(dialog, cx),
        }
    }

    /// 主窗口里开着宿主对话框。
    pub(crate) fn main_host_dialog_open(&self) -> bool {
        !self.host_dialogs.is_empty()
    }
}
