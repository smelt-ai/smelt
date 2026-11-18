//! 宿主对话框的内容：退出确认，和对话框共用的按钮。
//!
//! 只组 UI，什么时候开关见 `host_dialog`。

use gpui::*;
use rust_i18n::t;
use smelt_ui::dialog::Card;

use crate::{Workspace, request_app_quit};

enum ModalRole {
    Quiet,
    Primary,
}

impl Workspace {
    /// 对话框里退出这一步的按钮：取消、返回、关闭、以后再说。
    pub(crate) fn modal_cancel_button(
        id: &'static str,
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> smelt_ui::Button {
        Self::modal_role_button(id, label, ModalRole::Quiet, on_click, cx)
    }

    /// 这块确认里唯一的默认提交（退出、选目录）。
    pub(crate) fn modal_primary_button(
        id: &'static str,
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Self, &ClickEvent, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> smelt_ui::Button {
        Self::modal_role_button(id, label, ModalRole::Primary, on_click, cx)
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
            .small()
            .label(label.clone())
            .accessibility_label(label);
        let button = match role {
            ModalRole::Quiet => button.quiet(),
            ModalRole::Primary => button.primary(),
        };
        button.on_click(move |event, window, cx| {
            let Some(host) = host.upgrade() else {
                return;
            };
            host.update(cx, |this, cx| on_click(this, event, window, cx));
        })
    }

    /// 确认退出。后台守护和里面的会话照常跑着，只是这个窗口断开。
    pub(crate) fn confirm_quit(&mut self, cx: &mut Context<Self>) {
        if self.quit_requested {
            return;
        }
        self.quit_requested = true;
        cx.notify();
        request_app_quit(cx);
    }

    /// 退出确认。退出时要做别的事的插件（比如装更新）经退出守卫接手，不走这里。
    pub(crate) fn render_quit_confirm(&self, card: Card, cx: &mut Context<Self>) -> Card {
        let confirm = Self::modal_primary_button(
            "confirm-quit",
            if self.quit_requested {
                t!("quit.quitting")
            } else {
                t!("quit.quit")
            },
            |this, _, _, cx| this.confirm_quit(cx),
            cx,
        );
        let footer = smelt_ui::dialog::footer()
            .child(
                Self::modal_cancel_button(
                    "cancel-quit",
                    t!("action.cancel"),
                    |this, _, _, cx| {
                        this.show_quit_confirm = false;
                        cx.notify();
                    },
                    cx,
                )
                .disabled(self.quit_requested),
            )
            .child(confirm);
        card.w(px(smelt_ui::dialog::CONFIRM_WIDTH))
            .child(smelt_ui::dialog::content(
                t!("quit.title"),
                Some(t!("quit.body").into_owned().into()),
            ))
            .footer(footer)
    }
}
