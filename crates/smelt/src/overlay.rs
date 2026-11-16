//! Workspace-owned layers rendered in the main GPUI window.

use gpui::{
    AnyElement, App, Context, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, canvas, div, relative,
};

pub(crate) use smelt_ui::Button;
use smelt_ui::IconAction as _;

use super::*;

impl Workspace {
    /// 主窗口里有 Workspace 自己的遮罩层开着（宿主对话框、命令面板、大图预览）。原生网页视图
    /// 会盖住这些层，开着时文件面板的 HTML 预览让开。
    pub(crate) fn main_overlay_open(&self) -> bool {
        self.main_host_dialog_open() || self.palette.is_some() || self.acp_image_preview.is_some()
    }

    /// Esc 关掉最上面那层 Workspace 自己画的东西。宿主对话框归 kit 的对话框层管，不在这里。
    /// 返回按键用掉了没有，用掉了主键位处理就不再切面板。
    pub(crate) fn dismiss_workspace_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.acp_image_preview.take().is_some() {
            cx.notify();
            true
        } else if self.palette.is_some() {
            self.close_palette(window, cx);
            true
        } else {
            false
        }
    }

    /// Render all Workspace-owned overlays in the main GPUI window.
    pub(crate) fn render_overlay_layers(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let palette_overlay = self.palette.as_ref().map(|state| {
            let (commands, sessions) = self.palette_groups(cx);
            let command_ids: Vec<SharedString> =
                commands.iter().map(|command| command.id.clone()).collect();
            let session_ixs: Vec<usize> = sessions.iter().map(|(_, ix)| *ix).collect();
            let host = cx.entity().downgrade();
            let confirm_host = host.clone();
            let command_group = gpui_component::command::CommandGroup::new()
                .label("命令")
                .items(commands.into_iter().map(|command| {
                    gpui_component::command::CommandItem::new()
                        .label(command.title)
                        .keywords([command.group, command.id])
                }));
            let session_group =
                gpui_component::command::CommandGroup::new()
                    .label("切换会话")
                    .items(sessions.into_iter().map(|(label, _)| {
                        gpui_component::command::CommandItem::new().label(label)
                    }));
            div()
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .pt(px(80.))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_palette(window, cx)),
                )
                .child(
                    div()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w(px(520.))
                        .h(px(360.))
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .bg(gpui::rgb(ui_theme::surface()))
                        .border_1()
                        .border_color(ui_theme::overlay(0x22))
                        .rounded(ui_theme::composer_radius())
                        .shadow_lg()
                        .child(
                            gpui_component::command::Command::new(state)
                                .group(command_group)
                                .group(session_group)
                                .placeholder("想做什么？")
                                .bordered(false)
                                .max_h(px(360.))
                                .size_full()
                                .on_confirm(move |index, window, cx| {
                                    // 回调被推迟到这次更新结束后。用打开这一帧的快照，
                                    // 避免中途重建把下标对到另一条命令上。
                                    let Some(host) = confirm_host.upgrade() else {
                                        return;
                                    };
                                    match index.section {
                                        0 => {
                                            if let Some(id) = command_ids.get(index.row) {
                                                crate::workspace_palette::run_palette_command(
                                                    &host, id, window, cx,
                                                );
                                            }
                                        }
                                        1 => {
                                            if let Some(&ix) = session_ixs.get(index.row) {
                                                host.update(cx, |this, cx| {
                                                    this.palette_switch_session(ix, window, cx)
                                                });
                                            }
                                        }
                                        _ => {}
                                    }
                                })
                                .on_cancel(move |window, cx| {
                                    let Some(host) = host.upgrade() else {
                                        return;
                                    };
                                    host.update(cx, |this, cx| this.close_palette(window, cx));
                                }),
                        ),
                )
        });

        let preview_render = self
            .acp_image_preview
            .as_ref()
            .and_then(|image| smelt_ui::image::cached(image));
        let preview_canvas = canvas(
            move |bounds, window, _cx| {
                window.insert_hitbox(bounds, HitboxBehavior::Normal);
            },
            move |bounds, _prepaint, window, _cx| {
                smelt_ui::image::paint_contain(bounds, window, preview_render.as_ref(), 4.0);
            },
        )
        .size_full();
        let image_preview_overlay = self.acp_image_preview.as_ref().map(|_image| {
            div()
                .id("workspace-image-preview-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x000000d9))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.acp_image_preview = None;
                    cx.notify();
                }))
                .child(
                    div()
                        .id("workspace-image-preview-content")
                        .w(relative(0.86))
                        .h(relative(0.84))
                        .overflow_hidden()
                        .cursor_default()
                        .on_click(|_ev, _window, cx| cx.stop_propagation())
                        .child(preview_canvas),
                )
                .child(
                    Button::new("workspace-image-preview-close")
                        .floating()
                        .icon(IconName::Close)
                        .icon_action("关闭预览")
                        .absolute()
                        .top(px(48.))
                        .right(px(48.))
                        .on_click(cx.listener(|this, _ev, _window, cx| {
                            this.acp_image_preview = None;
                            cx.notify();
                        })),
                )
        });

        div()
            .absolute()
            .inset_0()
            .children(palette_overlay)
            .children(self.debug_hud.then(|| debug_hud(window, cx)))
            .children(image_preview_overlay)
            .into_any_element()
    }
}

/// 调试 HUD（Cmd+Shift+F）：gpui-fps 的帧耗时、慢帧、掉帧和 GPU/CPU/内存。
/// 让出顶部标题栏，免得盖住窗口按钮。
pub(crate) fn debug_hud(window: &mut Window, cx: &mut App) -> Div {
    div()
        .absolute()
        .top(px(40.))
        .left_0()
        .right_0()
        .bottom_0()
        .child(gpui_fps::fps_monitor(window, cx))
}
