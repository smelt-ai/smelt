//! Workspace-owned layers rendered in the main GPUI window.
//!
//! Plugin panels remain native AppKit child windows. Whenever a GPUI modal,
//! popover, or drag preview must be visible, the host temporarily hides
//! those WebViews instead of creating another GPUI window above them.

use gpui::{AnyElement, App, Context, IntoElement, ParentElement, Styled, Window, canvas, div};
use gpui_component::WindowExt;

use super::*;

/// 确认框的叠放顺序。同一扇窗里只有最上层做成 Dialog，底下的只留卡片，
/// 这样不会共用一个 `dialog-host` 和焦点句柄。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModalLayer {
    WorktreeList,
    NewWorktree,
    DeleteWorktree,
    Rename,
    Quit,
    CloseProject,
    DiscardHunk,
    DiscardFile,
    DiscardAll,
    DeleteBranch,
    DeleteFile,
    DeleteHistory,
    DeleteAgentMemory,
    DeleteAgent,
    DeleteAutomation,
    UnsavedFile,
    SessionManager,
    DaemonRestart,
}

impl ModalLayer {
    pub(crate) fn rank(self) -> u8 {
        match self {
            Self::WorktreeList => 1,
            Self::NewWorktree => 2,
            Self::DeleteWorktree => 3,
            Self::Rename => 4,
            Self::Quit => 5,
            Self::CloseProject => 6,
            Self::DiscardHunk => 7,
            Self::DiscardFile => 8,
            Self::DiscardAll => 9,
            Self::DeleteBranch => 10,
            Self::DeleteFile => 11,
            Self::DeleteHistory => 12,
            Self::DeleteAgentMemory => 13,
            Self::DeleteAgent => 14,
            Self::DeleteAutomation => 15,
            Self::UnsavedFile => 16,
            Self::SessionManager => 1,
            Self::DaemonRestart => 2,
        }
    }

    fn is_settings(self) -> bool {
        matches!(self, Self::SessionManager | Self::DaemonRestart)
    }
}

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

    /// 当前最上层主窗口确认框。数字大的盖住数字小的。
    /// Worktree 列表是底，新建在其上，删除再在其上。
    pub(crate) fn main_modal_rank(&self) -> Option<u8> {
        let mut rank = None;
        let raise = |rank: &mut Option<u8>, open: bool, value: u8| {
            if open {
                *rank = Some(rank.unwrap_or(0).max(value));
            }
        };
        raise(&mut rank, self.worktree_list.is_some(), ModalLayer::WorktreeList.rank());
        raise(&mut rank, self.new_worktree.is_some(), ModalLayer::NewWorktree.rank());
        raise(
            &mut rank,
            self.delete_worktree_target.is_some(),
            ModalLayer::DeleteWorktree.rank(),
        );
        raise(&mut rank, self.rename_target.is_some(), ModalLayer::Rename.rank());
        raise(&mut rank, self.show_quit_confirm, ModalLayer::Quit.rank());
        raise(
            &mut rank,
            self.close_project_target.is_some(),
            ModalLayer::CloseProject.rank(),
        );
        raise(
            &mut rank,
            self.discard_hunk_target.is_some(),
            ModalLayer::DiscardHunk.rank(),
        );
        raise(
            &mut rank,
            self.discard_file_target.is_some(),
            ModalLayer::DiscardFile.rank(),
        );
        raise(
            &mut rank,
            self.discard_all_target.is_some(),
            ModalLayer::DiscardAll.rank(),
        );
        raise(
            &mut rank,
            self.delete_branch_target.is_some(),
            ModalLayer::DeleteBranch.rank(),
        );
        raise(
            &mut rank,
            self.delete_file_target.is_some(),
            ModalLayer::DeleteFile.rank(),
        );
        raise(
            &mut rank,
            self.delete_history_target.is_some(),
            ModalLayer::DeleteHistory.rank(),
        );
        raise(
            &mut rank,
            self.agent_surface.pending_memory_delete.is_some(),
            ModalLayer::DeleteAgentMemory.rank(),
        );
        raise(
            &mut rank,
            self.agent_surface.pending_delete.is_some(),
            ModalLayer::DeleteAgent.rank(),
        );
        raise(
            &mut rank,
            self.automation_surface.pending_delete.is_some(),
            ModalLayer::DeleteAutomation.rank(),
        );
        raise(
            &mut rank,
            self.pending_file_switch.is_some(),
            ModalLayer::UnsavedFile.rank(),
        );
        rank
    }

    pub(crate) fn settings_modal_rank(&self) -> Option<u8> {
        let mut rank = None;
        if self.session_manager_open {
            rank = Some(ModalLayer::SessionManager.rank());
        }
        if self.show_daemon_restart_confirm {
            rank = Some(rank.unwrap_or(0).max(ModalLayer::DaemonRestart.rank()));
        }
        rank
    }

    pub(crate) fn modal_is_top(&self, layer: ModalLayer) -> bool {
        let current = if layer.is_settings() {
            self.settings_modal_rank()
        } else {
            self.main_modal_rank()
        };
        current == Some(layer.rank())
    }

    /// 在改名框、新建 worktree 的输入框拿到焦点之前调用。
    /// 渲染时再记焦点会把那个输入框当成「打开前的焦点」，关弹窗时又抢回来。
    /// `hold` 让紧接着的 sync 不要因为层级变化把焦点从输入框抢回对话框。
    pub(crate) fn begin_main_modal_focus(&mut self, window: &mut Window, cx: &mut App) {
        self.arm_modal_focus(false, window, cx);
        self.main_modal_hold = true;
    }

    pub(crate) fn sync_main_modal_focus(&mut self, window: &mut Window, cx: &mut App) {
        self.sync_modal_rank(false, self.main_modal_rank(), window, cx);
    }

    pub(crate) fn sync_settings_modal_focus(&mut self, window: &mut Window, cx: &mut App) {
        self.sync_modal_rank(true, self.settings_modal_rank(), window, cx);
    }

    fn sync_modal_rank(
        &mut self,
        settings: bool,
        rank: Option<u8>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (guard, hold, current) = if settings {
            (
                self.settings_modal_guard,
                false,
                self.settings_modal_rank,
            )
        } else {
            (
                self.main_modal_guard,
                self.main_modal_hold,
                self.main_modal_rank,
            )
        };
        if let Some(rank) = rank {
            if !guard {
                self.arm_modal_focus(settings, window, cx);
            } else if !hold && current != Some(rank) {
                self.modal_focus.focus(window, cx);
            }
            if settings {
                self.settings_modal_rank = Some(rank);
            } else {
                self.main_modal_rank = Some(rank);
                self.main_modal_hold = false;
            }
        } else {
            if settings {
                self.settings_modal_rank = None;
            } else {
                self.main_modal_rank = None;
                self.main_modal_hold = false;
            }
            self.release_modal_focus(settings, window, cx);
        }
    }

    fn arm_modal_focus(&mut self, settings: bool, window: &mut Window, cx: &mut App) {
        let armed = if settings {
            self.settings_modal_guard
        } else {
            self.main_modal_guard
        };
        if armed {
            return;
        }
        let previous = window.focused(cx).map(|handle| handle.downgrade());
        if settings {
            self.settings_modal_restore = previous;
            self.settings_modal_guard = true;
        } else {
            self.main_modal_restore = previous;
            self.main_modal_guard = true;
        }
        self.modal_focus.focus(window, cx);
    }

    fn release_modal_focus(&mut self, settings: bool, window: &mut Window, cx: &mut App) {
        let armed = if settings {
            self.settings_modal_guard
        } else {
            self.main_modal_guard
        };
        if !armed {
            return;
        }
        if settings {
            self.settings_modal_guard = false;
        } else {
            self.main_modal_guard = false;
        }
        let previous = if settings {
            self.settings_modal_restore.take()
        } else {
            self.main_modal_restore.take()
        };
        if let Some(handle) = previous.and_then(|handle| handle.upgrade()) {
            handle.focus(window, cx);
        }
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
            || self.agent_surface.pending_delete.is_some()
            || self.agent_surface.pending_memory_delete.is_some()
            || self.automation_surface.pending_delete.is_some()
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
            || self.agent_surface.pending_delete.is_some()
            || self.agent_surface.pending_memory_delete.is_some()
            || self.automation_surface.pending_delete.is_some()
            || self.pending_file_switch.is_some()
        {
            self.close_project_target = None;
            self.discard_hunk_target = None;
            self.discard_file_target = None;
            self.discard_all_target = None;
            self.delete_branch_target = None;
            self.delete_file_target = None;
            self.delete_history_target = None;
            self.agent_surface.pending_delete = None;
            self.agent_surface.pending_memory_delete = None;
            self.automation_surface.pending_delete = None;
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
        self.sync_main_modal_focus(window, cx);
        let palette_overlay = self.palette.as_ref().map(|state| {
            let [commands, sessions] = self.palette_groups(cx);
            let command_cmds: Vec<Cmd> = commands.iter().map(|(_, cmd)| cmd.clone()).collect();
            let session_cmds: Vec<Cmd> = sessions.iter().map(|(_, cmd)| cmd.clone()).collect();
            let host = cx.entity().downgrade();
            let confirm_host = host.clone();
            let command_group = gpui_component::command::CommandGroup::new()
                .label("命令")
                .items(
                    commands
                        .into_iter()
                        .map(|(label, _)| gpui_component::command::CommandItem::new().label(label)),
                );
            let session_group = gpui_component::command::CommandGroup::new()
                .label("切换会话")
                .items(
                    sessions
                        .into_iter()
                        .map(|(label, _)| gpui_component::command::CommandItem::new().label(label)),
                );
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
                        .bg(ui_theme::glass_floating())
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
                                    let cmd = match index.section {
                                        0 => command_cmds.get(index.row).cloned(),
                                        1 => session_cmds.get(index.row).cloned(),
                                        _ => None,
                                    };
                                    let Some(cmd) = cmd else {
                                        return;
                                    };
                                    let Some(host) = confirm_host.upgrade() else {
                                        return;
                                    };
                                    host.update(cx, |this, cx| this.exec_cmd(cmd, window, cx));
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
                        .border_color(ui_theme::card_stroke())
                        .bg(rgb(ui_theme::bg_card()))
                        .text_color(rgb(ui_theme::text()))
                        .text_lg()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(ui_theme::bg_hover())))
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
                self.agent_surface
                    .pending_delete
                    .is_some()
                    .then(|| self.render_delete_agent_confirm(cx)),
            )
            .children(
                self.agent_surface
                    .pending_memory_delete
                    .is_some()
                    .then(|| self.render_delete_agent_memory_confirm(cx)),
            )
            .children(
                self.automation_surface
                    .pending_delete
                    .is_some()
                    .then(|| self.render_delete_automation_confirm(cx)),
            )
            .children(
                self.pending_file_switch
                    .clone()
                    .map(|target| self.render_unsaved_file_confirm(target, cx)),
            )
            .children(self.debug_hud.then(|| debug_hud(window, cx)))
            .children(image_preview_overlay)
            .into_any_element()
    }
}

/// 调试 HUD（Cmd+Shift+F）：gpui-fps 的帧耗时、慢帧、掉帧和 GPU/CPU/内存。
/// 让出顶部标题栏，免得盖住窗口按钮。主窗口和设置窗口共用，每个窗口各一份监视器。
pub(crate) fn debug_hud(window: &mut Window, cx: &mut App) -> Div {
    div()
        .absolute()
        .top(px(40.))
        .left_0()
        .right_0()
        .bottom_0()
        .child(gpui_fps::fps_monitor(window, cx))
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
