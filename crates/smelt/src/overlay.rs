//! Workspace-owned layers rendered in the main GPUI window.
//!
//! Plugin panels remain native AppKit child windows. Whenever a GPUI modal,
//! popover, or drag preview must be visible, the host temporarily hides
//! those WebViews instead of creating another GPUI window above them.

use gpui::{AnyElement, App, Context, IntoElement, ParentElement, Styled, Window, canvas, div};
use gpui_component::WindowExt;

use super::*;

impl Workspace {
    /// Mark a library ContextMenu as active. ContextMenu stores its open bit in
    /// element-local state, so it cannot be observed through GlobalState.
    pub(crate) fn begin_context_menu_suppression(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu_suppressed = true;
        self.context_menu_generation = self.context_menu_generation.wrapping_add(1);
        let generation = self.context_menu_generation;
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                // ContextMenu does not emit a host-visible state transition. Keep the
                // lease long enough for a user to choose an item; input events and
                // Escape release it immediately, while this is only a lost-event guard.
                .timer(std::time::Duration::from_secs(30))
                .await;
            let _ = this.update_in(cx, |workspace, _window, cx| {
                if workspace.context_menu_generation == generation {
                    workspace.context_menu_suppressed = false;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// A click outside or a handled Escape is enough to release the bounded
    /// ContextMenu lease immediately. The timeout above is only a lost-event guard.
    pub(crate) fn end_context_menu_suppression(&mut self, cx: &mut Context<Self>) {
        if self.context_menu_suppressed {
            self.context_menu_suppressed = false;
            self.context_menu_generation = self.context_menu_generation.wrapping_add(1);
            cx.notify();
        }
    }

    /// Schedule one post-event pass after deferred popovers and context menus have
    /// updated their element state. The boolean keeps high-frequency pointer events
    /// from installing an unbounded callback list.
    pub(crate) fn schedule_plugin_surface_sync(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.plugin_surface_sync_scheduled {
            return;
        }
        self.plugin_surface_sync_scheduled = true;
        cx.on_next_frame(window, |workspace, window, cx| {
            workspace.plugin_surface_sync_scheduled = false;
            workspace.sync_plugin_panels(window, cx);
        });
    }

    fn has_workspace_overlay(&self) -> bool {
        self.palette.is_some()
            || self.show_quit_confirm
            || self.rename_target.is_some()
            || self.delete_worktree_target.is_some()
            || self.worktree_list.is_some()
            || self.new_worktree.is_some()
            || self.close_project_target.is_some()
            || self.discard_hunk_target.is_some()
            || self.discard_file_target.is_some()
            || self.discard_all_target.is_some()
            || self.delete_branch_target.is_some()
            || self.delete_file_target.is_some()
            || self.delete_history_target.is_some()
            || self.pending_file_switch.is_some()
            || self.acp_image_preview.is_some()
    }

    /// Dismiss the topmost Workspace-owned surface on Escape. Returns whether the
    /// key was consumed so the main key handler does not also change the active pane.
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
        } else if self.rename_target.is_some() {
            self.cancel_rename(cx);
            true
        } else if self.show_quit_confirm {
            self.show_quit_confirm = false;
            cx.notify();
            true
        } else if self.delete_worktree_target.is_some()
            || self.worktree_list.is_some()
            || self.new_worktree.is_some()
        {
            self.delete_worktree_target = None;
            self.worktree_list = None;
            self.new_worktree = None;
            cx.notify();
            true
        } else if self.close_project_target.is_some()
            || self.discard_hunk_target.is_some()
            || self.discard_file_target.is_some()
            || self.discard_all_target.is_some()
            || self.delete_branch_target.is_some()
            || self.delete_file_target.is_some()
            || self.delete_history_target.is_some()
            || self.pending_file_switch.is_some()
        {
            self.close_project_target = None;
            self.discard_hunk_target = None;
            self.discard_file_target = None;
            self.discard_all_target = None;
            self.delete_branch_target = None;
            self.delete_file_target = None;
            self.delete_history_target = None;
            self.pending_file_switch = None;
            cx.notify();
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
                        .bg(ui_theme::glass_floating())
                        .border_1()
                        .border_color(ui_theme::overlay(0x22))
                        .rounded(ui_theme::composer_radius())
                        .shadow_lg()
                        .child(List::new(state).search_placeholder("想做什么？")),
                )
        });

        let preview_viewport = window.viewport_size();
        let preview_box_w = preview_viewport.width * 0.86;
        let preview_box_h = preview_viewport.height * 0.84;
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
                .bg(rgba(0x000000d9))
                .cursor_pointer()
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.acp_image_preview = None;
                    cx.notify();
                }))
                .child(
                    div()
                        .id("workspace-image-preview-content")
                        .absolute()
                        .top((preview_viewport.height - preview_box_h) / 2.0)
                        .left((preview_viewport.width - preview_box_w) / 2.0)
                        .w(preview_box_w)
                        .h(preview_box_h)
                        .overflow_hidden()
                        .cursor_default()
                        .on_click(|_ev, _window, cx| cx.stop_propagation())
                        .child(preview_canvas),
                )
                .child(
                    div()
                        .id("workspace-image-preview-close")
                        .absolute()
                        .top(px(48.))
                        .right(px(48.))
                        .size(px(34.))
                        .rounded_full()
                        .border_1()
                        .border_color(rgba(0xffffff33))
                        .bg(rgba(0x181818ee))
                        .text_color(rgb(0xffffff))
                        .text_lg()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|d| d.bg(rgba(0x303030ff)))
                        .child("×")
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
            .children(self.show_quit_confirm.then(|| self.render_quit_confirm(cx)))
            .children(
                self.rename_target
                    .is_some()
                    .then(|| self.render_rename_session(cx)),
            )
            .children(
                self.delete_worktree_target
                    .is_some()
                    .then(|| self.render_delete_worktree_confirm(cx)),
            )
            .children(
                self.worktree_list
                    .is_some()
                    .then(|| self.render_worktree_list(cx)),
            )
            .children(
                self.new_worktree
                    .is_some()
                    .then(|| self.render_new_worktree(cx)),
            )
            .children(
                self.close_project_target
                    .is_some()
                    .then(|| self.render_close_project_confirm(cx)),
            )
            .children(
                self.discard_hunk_target
                    .is_some()
                    .then(|| self.render_discard_hunk_confirm(cx)),
            )
            .children(
                self.discard_file_target
                    .is_some()
                    .then(|| self.render_discard_file_confirm(cx)),
            )
            .children(
                self.discard_all_target
                    .is_some()
                    .then(|| self.render_discard_all_confirm(cx)),
            )
            .children(
                self.delete_branch_target
                    .is_some()
                    .then(|| self.render_delete_branch_confirm(cx)),
            )
            .children(
                self.delete_file_target
                    .is_some()
                    .then(|| self.render_delete_file_confirm(cx)),
            )
            .children(
                self.delete_history_target
                    .is_some()
                    .then(|| self.render_delete_history_confirm(cx)),
            )
            .children(
                self.pending_file_switch
                    .clone()
                    .map(|target| self.render_unsaved_file_confirm(target, cx)),
            )
            .children(self.debug_hud.then(|| {
                let fps = self.fps_ema;
                let ms = if fps > 0.0 { 1000.0 / fps } else { 0.0 };
                let mem = self
                    .debug_mem_rss
                    .map(mem_usage::format_rss)
                    .unwrap_or_else(|| "—".into());
                let color = if fps >= 55.0 {
                    rgb(ui_theme::green())
                } else if fps >= 30.0 {
                    rgb(ui_theme::yellow())
                } else {
                    rgb(ui_theme::red())
                };
                div()
                    .absolute()
                    .top(px(40.))
                    .right(px(12.))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(ui_theme::tint(ui_theme::bg_card(), 0xcc))
                    .border_1()
                    .border_color(ui_theme::overlay(0x22))
                    .font_family(terminal_view::font_family())
                    .text_xs()
                    .text_color(color)
                    .child(format!("{fps:.0} FPS · {ms:.1} ms · RSS {mem}"))
            }))
            .children(image_preview_overlay)
            .into_any_element()
    }
}

/// Whether GPUI content currently needs plugin WebViews removed from the native
/// child-window stack. The policy never examines a plugin identity.
pub(crate) fn should_suppress_plugin_content(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut App,
) -> bool {
    let workspace_overlay = workspace.has_workspace_overlay();
    let root_overlay = window.has_active_sheet(cx) || window.has_active_dialog(cx);
    let deferred_popup = cx.has_global::<gpui_component::GlobalState>()
        && gpui_component::GlobalState::is_in_deferred_context(cx);
    let stage_cover = workspace.stage_cover.is_some();
    let drag = cx.has_active_drag();
    let context_menu = workspace.context_menu_suppressed;

    // WebViews are AppKit child windows and therefore outrank the parent's Metal
    // surface. Hide them while any GPUI layer must paint or receive input above them.
    workspace_overlay
        || workspace.debug_hud
        || root_overlay
        || deferred_popup
        || stage_cover
        || drag
        || context_menu
}
