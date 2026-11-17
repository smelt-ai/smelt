//! 宿主自己的对话框：退出确认、选目录。
//!
//! 和插件的对话框同在 `smelt_ui::dialog` 那一层，叠成一摞：Esc / Enter、焦点陷阱、关掉后
//! 焦点回到原处都由那一层管，叠起来时只有最上面那个能操作。
//!
//! 开没开着由 Workspace 上各自的状态说了算（`show_quit_confirm`、`dir_picker`），
//! 对话框只是它的样子。改状态的地方不管对话框：有的是异步回来的、手上没有窗口。
//! Workspace 每次通知后 [`Workspace::sync_host_dialogs`] 对一遍，状态有了就开，没了
//! 就按 id 关掉那一张。用户在对话框里关（Esc、取消）走对话框层，`on_close` 再把状态清掉。

use gpui::Context;
use smelt_ui::dialog::{self, Card, DialogId};

use super::*;

/// 一种宿主对话框。同一种同时只开一个，内容跟着状态变。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HostDialog {
    Quit,
    DirPicker,
}

impl HostDialog {
    /// 同一轮里几个一起要开时按这个次序，后开的叠在上面。
    const ALL: &[HostDialog] = &[Self::Quit, Self::DirPicker];
}

impl Workspace {
    /// 这种对话框的状态在不在。
    fn host_dialog_live(&self, kind: HostDialog) -> bool {
        match kind {
            HostDialog::Quit => self.show_quit_confirm,
            HostDialog::DirPicker => self.dir_picker.is_some(),
        }
    }

    /// 现在能不能取消。正在退出时撤不回，不能。
    fn host_dialog_cancellable(&self, kind: HostDialog) -> bool {
        match kind {
            HostDialog::Quit => !self.quit_requested,
            HostDialog::DirPicker => true,
        }
    }

    /// 用户把它关了，或者它开着的窗口没了：清掉状态，什么都不做。
    fn dismiss_host_dialog(&mut self, kind: HostDialog, cx: &mut Context<Self>) {
        match kind {
            HostDialog::Quit => self.show_quit_confirm = false,
            HostDialog::DirPicker => self.cancel_dir_picker(cx),
        }
        cx.notify();
    }

    /// Enter（焦点不在输入框、按钮上时）执行默认按钮。总是返回 false：对话框不当场关，
    /// 状态没了由 [`Self::sync_host_dialogs`] 关。
    fn host_dialog_enter(&mut self, kind: HostDialog, cx: &mut Context<Self>) -> bool {
        match kind {
            HostDialog::Quit => self.confirm_quit(cx),
            HostDialog::DirPicker => self.dir_picker_confirm(cx),
        }
        false
    }

    /// 宿主对话框跟着状态开关。Workspace 每次通知后跑（见 `note_overlay`）。
    ///
    /// 在 observer 里跑，这时没有窗口被借着，可以按句柄进窗口开关对话框。
    pub(crate) fn sync_host_dialogs(&mut self, cx: &mut Context<Self>) {
        let window = self.window_handle;

        // 状态没了的关掉。先从记录里拿掉，它的 `on_close` 看到不在记录里就不再清状态。
        let stale: Vec<DialogId> = self
            .host_dialogs
            .iter()
            .filter(|&&(kind, _)| !self.host_dialog_live(kind))
            .map(|&(_, id)| id)
            .collect();
        if !stale.is_empty() {
            self.host_dialogs.retain(|(_, id)| !stale.contains(id));
            let _ = window.update(cx, |_, window, cx| {
                for id in stale {
                    id.close(window, cx);
                }
            });
        }

        for &kind in HostDialog::ALL {
            if !self.host_dialog_live(kind)
                || self.host_dialogs.iter().any(|&(open, _)| open == kind)
            {
                continue;
            }
            let workspace = cx.weak_entity();
            let opened = window
                .update(cx, |_, window, cx| {
                    dialog::open(window, cx, move |card, _, cx| match workspace.upgrade() {
                        Some(workspace) => workspace.update(cx, |workspace, cx| {
                            workspace.build_host_dialog(kind, card, cx)
                        }),
                        None => card,
                    })
                })
                .ok()
                .flatten();
            match opened {
                Some(id) => self.host_dialogs.push((kind, id)),
                // 主窗口没了：当作没开过。
                None => self.dismiss_host_dialog(kind, cx),
            }
        }
    }

    /// 每帧由对话框层调：按状态把卡片搭出来。
    fn build_host_dialog(&mut self, kind: HostDialog, card: Card, cx: &mut Context<Self>) -> Card {
        // 搭卡片时它已在记录里；`on_close` 按这个 id 认自己，不误清随后重开的同类对话框。
        let id = self
            .host_dialogs
            .iter()
            .find(|&&(open, _)| open == kind)
            .map(|&(_, id)| id);
        let workspace = cx.weak_entity();
        let on_ok = workspace.clone();
        let on_cancel = workspace.clone();
        let card = card
            .close_on_backdrop(false)
            .on_ok(move |_, cx| {
                on_ok
                    .update(cx, |this, cx| this.host_dialog_enter(kind, cx))
                    .unwrap_or(true)
            })
            .on_cancel(move |_, cx| {
                on_cancel
                    .read_with(cx, |this, _| this.host_dialog_cancellable(kind))
                    .unwrap_or(true)
            })
            .on_close(move |_, cx| {
                let _ = workspace.update(cx, |this, cx| {
                    // 同步时按状态关的已经不在记录里，状态也早清了。
                    let before = this.host_dialogs.len();
                    this.host_dialogs.retain(|&(_, open)| Some(open) != id);
                    if this.host_dialogs.len() != before {
                        this.dismiss_host_dialog(kind, cx);
                    }
                });
            });
        match kind {
            HostDialog::Quit => self.render_quit_confirm(card, cx),
            HostDialog::DirPicker => self.render_dir_picker(card, cx),
        }
    }

    /// 主窗口里开着宿主对话框。
    pub(crate) fn main_host_dialog_open(&self) -> bool {
        !self.host_dialogs.is_empty()
    }
}
