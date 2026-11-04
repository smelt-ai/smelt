use super::*;
use crate::workspace_persist::ConversationOp;
use smelt_model::workspace_directory::WorkspaceConversation;
use smelt_runtime_client::protocol::control::ControlMethod;

const ARCHIVE_HEADER_GROUP: &str = "archive-header-hover";

pub(super) fn organization_menu(
    menu: PopupMenu,
    workspace: Entity<Workspace>,
    ui_id: u64,
    cx: &App,
) -> PopupMenu {
    let ws = workspace.read(cx);
    let Some(session) = ws.sessions.iter().find(|s| s.ui_id == ui_id) else {
        return menu;
    };
    let runtime = session.runtime(cx);
    let key = runtime.key();
    let Some(space) = ws.space(key) else {
        return menu;
    };
    let pinned = space.directory.is_pinned(&session.conversation_id);
    let pin_supported = !space
        .unsupported_conversation_methods
        .contains(&ControlMethod::WorkspaceConversationPin);
    let archive_supported = !space
        .unsupported_conversation_methods
        .contains(&ControlMethod::WorkspaceConversationArchive)
        && session.automation_id.is_none();
    let e_pin = workspace.clone();
    menu.item(
        PopupMenuItem::new(if pinned { "取消置顶" } else { "置顶" })
            .icon(Icon::new(gpui_kit_assets::IconName::Pin))
            .disabled(!pin_supported)
            .on_click(move |_, _, cx| {
                e_pin.update(cx, |ws, cx| ws.toggle_conversation_pin(ui_id, cx));
            }),
    )
    .item(
        PopupMenuItem::new("归档")
            .disabled(!archive_supported)
            .on_click(move |_, _, cx| {
                workspace.update(cx, |ws, cx| ws.request_archive_conversation_ui(ui_id, cx));
            }),
    )
    .separator()
}

fn archive_title(conversation: &WorkspaceConversation) -> String {
    conversation
        .custom_title
        .as_deref()
        .or_else(|| {
            conversation
                .acp
                .as_ref()
                .and_then(|acp| acp.session_title.as_deref())
        })
        .or_else(|| {
            conversation
                .terminals
                .first()
                .and_then(|t| t.custom_title.as_deref().or(t.launch_label.as_deref()))
        })
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(if conversation.acp.is_some() {
            "未命名对话"
        } else {
            "终端"
        })
        .to_string()
}

impl Workspace {
    pub(crate) fn toggle_conversation_pin(&mut self, ui_id: u64, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.iter().find(|s| s.ui_id == ui_id) else {
            return;
        };
        let key = session.runtime(cx).key().clone();
        let id = session.conversation_id.clone();
        let pinned = !self.space(&key).is_some_and(|s| s.directory.is_pinned(&id));
        self.request_conversation_op(&key, ConversationOp::Pin { id, pinned }, cx);
    }

    pub(crate) fn request_archive_conversation_ui(&mut self, ui_id: u64, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.iter().find(|s| s.ui_id == ui_id) else {
            return;
        };
        if session.status(cx) != AgentStatus::Idle
            || session
                .term_leaves()
                .iter()
                .any(|pane| pane_status(pane, cx) != AgentStatus::Idle)
        {
            self.close_confirm = Some(crate::CloseConfirm::Archive {
                ui_id,
                title: session.title(cx),
            });
            cx.notify();
        } else {
            self.archive_conversation_ui(ui_id, cx);
        }
    }

    pub(crate) fn archive_conversation_ui(&mut self, ui_id: u64, cx: &mut Context<Self>) {
        let Some(session) = self.sessions.iter().find(|s| s.ui_id == ui_id) else {
            return;
        };
        let key = session.runtime(cx).key().clone();
        let id = session.conversation_id.clone();
        // 不先关闭本地视图：失败时用户仍能操作，成功由目录推送走非删除的移除路径。
        self.request_conversation_op(&key, ConversationOp::Archive { id, archived: true }, cx);
    }

    fn request_delete_archived(
        &mut self,
        runtime: RuntimeKey,
        id: String,
        title: String,
        cx: &mut Context<Self>,
    ) {
        self.close_confirm = Some(crate::CloseConfirm::DeleteArchived { runtime, id, title });
        cx.notify();
    }

    pub(super) fn render_archive_section(&self, this: Entity<Self>, cx: &App) -> Option<Div> {
        let mut archived = self
            .spaces
            .iter()
            .flat_map(|space| {
                space
                    .directory
                    .archived()
                    .iter()
                    .map(move |c| (space.runtime.key(), c))
            })
            .collect::<Vec<_>>();
        if archived.is_empty() {
            return None;
        }
        archived.sort_by(|(_, a), (_, b)| b.archived_at.cmp(&a.archived_at));
        let e_toggle = this.clone();
        let header = ListRow::new("archive-section-header")
            .group(ARCHIVE_HEADER_GROUP)
            .mx_2()
            .mt_3()
            .px(px(PROJECT_HEADER_PAD_X))
            .h(ui_theme::height::sm())
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .font_medium()
            .text_color(rgb(ui_theme::text_muted()))
            .child(group_disclosure(
                "archive-disclosure",
                ARCHIVE_HEADER_GROUP,
                Icon::new(gpui_kit_assets::IconName::Archive)
                    .size(px(PROJECT_DISCLOSURE_ICON_SIZE)),
                !self.archive_expanded,
                true,
            ))
            .child(format!("归档 ({})", archived.len()))
            .on_click(move |_, _, cx| {
                e_toggle.update(cx, |ws, cx| {
                    ws.archive_expanded = !ws.archive_expanded;
                    ws.save_state(cx);
                    cx.notify();
                });
            });
        let mut section = div().flex().flex_col().child(header);
        if !self.archive_expanded {
            return Some(section);
        }
        for (key, conversation) in archived {
            let title = archive_title(conversation);
            let cwd = conversation
                .acp
                .as_ref()
                .and_then(|a| a.cwd.as_deref())
                .or_else(|| {
                    conversation
                        .terminals
                        .first()
                        .and_then(|t| t.cwd.as_deref())
                })
                .unwrap_or("");
            let project = self
                .space(key)
                .and_then(|space| {
                    smelt_model::workspace_directory::project_root_of(
                        crate::runtimes::path_style(key, cx),
                        &space.projects,
                        cwd,
                    )
                })
                .unwrap_or(cwd);
            let label = format!("{} · {}", crate::runtimes::runtime_name(key, cx), project);
            let element_id = SharedString::from(format!("archive-{:?}-{}", key, conversation.id));
            let restore_key = key.clone();
            let restore_id = conversation.id.clone();
            let delete_key = key.clone();
            let delete_id = conversation.id.clone();
            let delete_title = title.clone();
            let menu_key = key.clone();
            let menu_id = conversation.id.clone();
            let menu_title = title.clone();
            let e_restore = this.clone();
            let e_delete = this.clone();
            let e_menu = this.clone();
            let supported = self.space(key).is_some_and(|space| {
                !space
                    .unsupported_conversation_methods
                    .contains(&ControlMethod::WorkspaceConversationArchive)
            });
            let row = ListRow::new(element_id.clone())
                .mx_2()
                .px_2()
                .py_1()
                .rounded(ui_theme::row_radius())
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_color(rgb(ui_theme::text_muted()))
                        .child(div().text_sm().child(truncated_line(title)))
                        .child(div().text_xs().child(truncated_line(label))),
                )
                .child(
                    Button::new(SharedString::from(format!("{element_id}-restore")))
                        .quiet()
                        .icon_size()
                        .icon(Icon::new(gpui_kit_assets::IconName::RotateCw))
                        .icon_action("恢复")
                        .disabled(!supported)
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            e_restore.update(cx, |ws, cx| {
                                ws.request_conversation_op(
                                    &restore_key,
                                    ConversationOp::Archive {
                                        id: restore_id.clone(),
                                        archived: false,
                                    },
                                    cx,
                                )
                            });
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("{element_id}-delete")))
                        .dismiss()
                        .icon(crate::trash_icon())
                        .icon_action("永久删除")
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            e_delete.update(cx, |ws, cx| {
                                ws.request_delete_archived(
                                    delete_key.clone(),
                                    delete_id.clone(),
                                    delete_title.clone(),
                                    cx,
                                );
                            });
                        }),
                )
                .context_menu(move |menu, _, _| {
                    let e_restore = e_menu.clone();
                    let restore_key = menu_key.clone();
                    let restore_id = menu_id.clone();
                    let e_delete = e_menu.clone();
                    let delete_key = menu_key.clone();
                    let delete_id = menu_id.clone();
                    let delete_title = menu_title.clone();
                    menu.item(PopupMenuItem::new("恢复").disabled(!supported).on_click(
                        move |_, _, cx| {
                            e_restore.update(cx, |ws, cx| {
                                ws.request_conversation_op(
                                    &restore_key,
                                    ConversationOp::Archive {
                                        id: restore_id.clone(),
                                        archived: false,
                                    },
                                    cx,
                                )
                            });
                        },
                    ))
                    .item(
                        PopupMenuItem::new("永久删除")
                            .icon(crate::trash_icon())
                            .on_click(move |_, _, cx| {
                                e_delete.update(cx, |ws, cx| {
                                    ws.request_delete_archived(
                                        delete_key.clone(),
                                        delete_id.clone(),
                                        delete_title.clone(),
                                        cx,
                                    );
                                });
                            }),
                    )
                });
            section = section.child(row);
        }
        Some(section)
    }
}
