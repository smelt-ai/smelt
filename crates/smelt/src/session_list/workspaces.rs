use super::*;
use gpui_base::actions::{Cancel, Confirm};
use gpui_component::popover::Popover;
use smelt_model::workspace_directory::WorkspaceWorkset;

fn project_worksets<'a>(
    worksets: &'a [WorkspaceWorkset],
    project_root: &str,
) -> Vec<&'a WorkspaceWorkset> {
    let mut result = worksets
        .iter()
        .filter(|workset| {
            crate::sidebar_order::same_project_root(&workset.project_root, project_root)
        })
        .collect::<Vec<_>>();
    result.sort_by(|a, b| a.position.cmp(&b.position).then_with(|| a.id.cmp(&b.id)));
    result
}

pub(super) enum ProjectSessionRow<'a> {
    Worktree(&'a WorkspaceWorkset),
    Session {
        index: usize,
        workset_id: Option<&'a str>,
    },
}

fn project_session_rows<'a>(
    worksets: &'a [WorkspaceWorkset],
    project_root: &str,
    members: &[(usize, Option<&str>)],
) -> Vec<ProjectSessionRow<'a>> {
    let worksets = project_worksets(worksets, project_root);
    let mut rows = members
        .iter()
        .filter(|(_, id)| {
            !worksets
                .iter()
                .any(|workset| Some(workset.id.as_str()) == *id)
        })
        .map(|(index, _)| ProjectSessionRow::Session {
            index: *index,
            workset_id: None,
        })
        .collect::<Vec<_>>();
    for workset in worksets {
        let members = members
            .iter()
            .filter(|(_, id)| *id == Some(workset.id.as_str()))
            .collect::<Vec<_>>();
        if members.is_empty() {
            continue;
        }
        rows.push(ProjectSessionRow::Worktree(workset));
        rows.extend(
            members
                .into_iter()
                .map(|(index, _)| ProjectSessionRow::Session {
                    index: *index,
                    workset_id: Some(workset.id.as_str()),
                }),
        );
    }
    rows
}

pub(super) fn rows_for_group<'a>(
    workspace: &'a Workspace,
    group: &crate::ProjectGroup,
) -> Vec<ProjectSessionRow<'a>> {
    let members = group
        .sessions
        .iter()
        .map(|&index| (index, workspace.sessions[index].workset_id.as_deref()))
        .collect::<Vec<_>>();
    if workspace.sidebar_grouping == SidebarGrouping::Project {
        project_session_rows(
            workspace
                .space(&group.runtime)
                .map(|space| space.worksets.as_slice())
                .unwrap_or_default(),
            &group.root,
            &members,
        )
    } else {
        members
            .into_iter()
            .map(|(index, _)| ProjectSessionRow::Session {
                index,
                workset_id: None,
            })
            .collect()
    }
}

impl Workspace {
    pub(crate) fn project_session_order(&self, group: &crate::ProjectGroup) -> Vec<usize> {
        rows_for_group(self, group)
            .into_iter()
            .filter_map(|row| match row {
                ProjectSessionRow::Session { index, .. } => Some(index),
                ProjectSessionRow::Worktree(_) => None,
            })
            .collect()
    }

    pub(crate) fn toggle_worktree_collapsed(
        &mut self,
        project: &ProjectRef,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(space) = self.space_mut(&project.runtime) {
            if !space.collapsed_worksets.remove(id) {
                space.collapsed_worksets.insert(id.to_string());
            }
        }
        cx.notify();
    }

    pub(crate) fn select_worktree(
        &mut self,
        project: &ProjectRef,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workset) = self.space(&project.runtime).and_then(|space| {
            space.worksets.iter().find(|workset| {
                workset.id == id
                    && crate::sidebar_order::same_project_root(&workset.project_root, &project.root)
            })
        }) else {
            self.report_background_error("工作树已不存在，请刷新后重试");
            return;
        };
        self.active_project =
            Some(project.at(workset.task_root(crate::runtimes::path_style(&project.runtime, cx))));
        self.sync_project_ui(window, cx);
        self.save_state(cx);
        self.schedule_visible_refresh(cx);
        cx.notify();
    }
}

pub(super) fn worktree_row(
    workspace: &Workspace,
    entity: Entity<Workspace>,
    workset: &WorkspaceWorkset,
    project: &ProjectRef,
    scroll: ScrollHandle,
    focused_key: Option<u64>,
    cx: &mut App,
) -> AnyElement {
    let collapsed = workspace
        .space(&project.runtime)
        .is_some_and(|space| space.collapsed_worksets.contains(&workset.id));
    let key = keyboard::SidebarItem::Worktree(project.clone(), workset.id.clone()).key();
    let click_entity = entity.clone();
    let click_project = project.clone();
    let click_id = workset.id.clone();
    let count = workspace
        .sessions
        .iter()
        .filter(|session| {
            session.runtime(cx).key() == &project.runtime
                && session.workset_id.as_deref() == Some(workset.id.as_str())
        })
        .count();
    let runtime = workspace
        .space(&project.runtime)
        .map(|space| space.runtime.clone());
    let tooltip = format!(
        "工作树：{}\n分支：{}\n{}",
        workset.task,
        workset.branch,
        repository_summary(workset)
    );
    ListRow::new(workset_element_id(
        "task-worktree-row",
        project,
        &workset.id,
    ))
    .relative()
    .flex()
    .items_center()
    .gap_1p5()
    .px_2()
    .h(ui_theme::height::md())
    .rounded(ui_theme::row_radius())
    .when(focused_key == Some(key), |row| row.child(focus_ring()))
    .child(reveal_anchor(
        workspace.sidebar_reveal.clone(),
        key,
        scroll,
        entity.clone(),
    ))
    .child(
        Icon::new(project_disclosure_icon(collapsed))
            .size(px(12.))
            .flex_shrink_0(),
    )
    .child(
        Icon::empty()
            .path("smelt-icons/git-branch.svg")
            .size(px(16.))
            .flex_shrink_0(),
    )
    .child(
        div()
            .flex_1()
            .min_w_0()
            .text_sm()
            .text_color(rgb(ui_theme::text_muted()))
            .child(truncated_line(workset.task.clone())),
    )
    .child(
        div()
            .text_xs()
            .text_color(rgb(ui_theme::text_faint()))
            .child(count.to_string()),
    )
    .children(runtime.map(|runtime| {
        new_session::workset_session_picker(entity, runtime, workset.clone()).icon_trigger(
            workset_element_id("task-worktree-new", project, &workset.id),
        )
    }))
    .tooltip(move |window, cx| {
        gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
    })
    .on_click(move |_, window, cx| {
        cx.stop_propagation();
        click_entity.update(cx, |workspace, cx| {
            workspace.select_worktree(&click_project, &click_id, window, cx);
            workspace.toggle_worktree_collapsed(&click_project, &click_id, cx);
        });
    })
    .into_any_element()
}

fn repository_summary(workset: &WorkspaceWorkset) -> String {
    let mut repositories = workset.repositories.iter().collect::<Vec<_>>();
    repositories.sort_by(|a, b| {
        a.position
            .cmp(&b.position)
            .then_with(|| a.name.cmp(&b.name))
    });
    let names = repositories
        .iter()
        .map(|repository| repository.name.as_str())
        .collect::<Vec<_>>()
        .join("、");
    if names.is_empty() {
        "0 个仓库".into()
    } else {
        format!("{} 个仓库：{names}", repositories.len())
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct OverviewVisibility {
    open: bool,
    pinned: bool,
    trigger_hovered: bool,
    content_hovered: bool,
    focused: bool,
}

impl OverviewVisibility {
    fn trigger_hover(&mut self, hovered: bool) {
        self.trigger_hovered = hovered;
        if hovered {
            self.open = true;
        }
    }

    fn content_hover(&mut self, hovered: bool) {
        self.content_hovered = hovered;
    }

    fn pin(&mut self) {
        self.open = true;
        self.pinned = true;
    }

    fn close_if_idle(&mut self) {
        if !self.pinned && !self.trigger_hovered && !self.content_hovered && !self.focused {
            self.open = false;
        }
    }

    fn dismiss(&mut self) {
        *self = Self::default();
    }
}

fn status_summary(
    workspace: &Workspace,
    project: &ProjectRef,
    workset: &WorkspaceWorkset,
) -> String {
    let statuses = workset
        .repositories
        .iter()
        .filter_map(|repository| {
            workspace
                .git_status
                .get(&ProjectRef {
                    runtime: project.runtime.clone(),
                    root: repository.worktree.clone(),
                })
                .map(|(_, status)| status)
        })
        .collect::<Vec<_>>();
    if statuses.is_empty()
        || statuses.len() != workset.repositories.len()
        || statuses.iter().any(|status| !status.ok)
    {
        return "状态未知".into();
    }
    let mut label = if statuses.iter().any(|status| !status.files.is_empty()) {
        "脏".to_string()
    } else {
        "净".to_string()
    };
    let ahead = statuses.iter().map(|status| status.ahead).sum::<u32>();
    let behind = statuses.iter().map(|status| status.behind).sum::<u32>();
    if ahead > 0 {
        label.push_str(&format!(" ↑{ahead}"));
    }
    if behind > 0 {
        label.push_str(&format!(" ↓{behind}"));
    }
    label
}

#[derive(IntoElement)]
pub(super) struct WorkspaceBadge {
    workspace: Entity<Workspace>,
    project: ProjectRef,
}

pub(super) fn workspace_badge(workspace: Entity<Workspace>, project: ProjectRef) -> WorkspaceBadge {
    WorkspaceBadge { workspace, project }
}

impl RenderOnce for WorkspaceBadge {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = project_element_id("workspace-overview-state", &self.project);
        overview_view(id, window, cx, move |cx| {
            WorkspaceOverview::new(self.workspace, self.project, cx)
        })
    }
}

fn overview_view<V: Render>(
    id: ElementId,
    window: &mut Window,
    cx: &mut App,
    init: impl FnOnce(&mut Context<V>) -> V,
) -> Entity<V> {
    // The view observes Workspace; use_keyed_state would forward its notifications back to Workspace.
    window.with_global_id(id, |global_id, window| {
        window.with_element_state(global_id, |state: Option<Entity<V>>, _| {
            let view = state.unwrap_or_else(|| cx.new(init));
            (view.clone(), view)
        })
    })
}

struct WorkspaceOverview {
    workspace: WeakEntity<Workspace>,
    project: ProjectRef,
    visibility: OverviewVisibility,
    trigger_focus: FocusHandle,
    content_focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    close_task: Option<Task<()>>,
    open_menu: Option<String>,
    nested_picker: Option<String>,
    _workspace_refresh: Subscription,
}

impl WorkspaceOverview {
    fn new(workspace: Entity<Workspace>, project: ProjectRef, cx: &mut Context<Self>) -> Self {
        Self {
            workspace: workspace.downgrade(),
            project,
            visibility: OverviewVisibility::default(),
            trigger_focus: cx.focus_handle().tab_stop(true),
            content_focus: cx.focus_handle(),
            previous_focus: None,
            close_task: None,
            open_menu: None,
            nested_picker: None,
            _workspace_refresh: cx.observe(&workspace, |this, workspace, cx| {
                if this.open_menu.as_ref().is_some_and(|id| {
                    !workspace
                        .read(cx)
                        .space(&this.project.runtime)
                        .is_some_and(|space| space.worksets.iter().any(|workset| workset.id == *id))
                }) {
                    this.open_menu = None;
                    this.nested_picker = None;
                    this.visibility.dismiss();
                    this.close_task = None;
                }
                cx.notify();
            }),
        }
    }

    fn hover(&mut self, trigger: bool, hovered: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.close_task = None;
        if trigger {
            self.visibility.trigger_hover(hovered);
        } else {
            self.visibility.content_hover(hovered);
        }
        if !hovered {
            self.close_task = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(300))
                    .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.visibility.focused = this.content_focus.contains_focused(window, cx);
                    this.visibility.close_if_idle();
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }

    fn pin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_task = None;
        if !self.visibility.pinned {
            self.previous_focus = window.focused(cx);
            self.content_focus.focus(window, cx);
        }
        self.visibility.pin();
        cx.notify();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_task = None;
        self.visibility.dismiss();
        self.open_menu = None;
        self.nested_picker = None;
        if self.content_focus.contains_focused(window, cx) {
            if let Some(previous) = self.previous_focus.take() {
                previous.focus(window, cx);
            } else {
                self.trigger_focus.focus(window, cx);
            }
        } else {
            self.previous_focus = None;
        }
        cx.notify();
    }

    fn manage(
        &mut self,
        workset_id: String,
        finish: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let project = self.project.clone();
        self.dismiss(window, cx);
        workspace.update(cx, |workspace, cx| {
            if let Some(label) = crate::runtimes::runtime_unavailable_label(&project.runtime, cx) {
                crate::status_item::notify_error(format!("工作树操作不可用：{label}"));
                return;
            }
            let Some(runtime) = workspace.space(&project.runtime).and_then(|space| {
                space
                    .worksets
                    .iter()
                    .any(|workset| {
                        workset.id == workset_id
                            && crate::sidebar_order::same_project_root(
                                &workset.project_root,
                                &project.root,
                            )
                    })
                    .then(|| space.runtime.clone())
            }) else {
                crate::status_item::notify_error("工作树已不存在，请刷新后重试");
                return;
            };
            if finish {
                workspace.request_workset_finish(
                    runtime,
                    smelt_model::workspace_directory::WorkspaceWorksetFinishParams { workset_id },
                    cx,
                );
            } else {
                workspace.start_rename(RenameTarget::Workset(workset_id), window, cx);
            }
        });
    }
}

impl Render for WorkspaceOverview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(workspace) = self.workspace.upgrade() else {
            return div().into_any_element();
        };
        let (rows, count, runtime, loaded) = {
            let ws = workspace.read(cx);
            let Some(space) = ws.space(&self.project.runtime) else {
                return div().into_any_element();
            };
            let active = ws.sessions.get(ws.active_session).filter(|session| {
                ws.active_tab().is_session() && session.runtime(cx).key() == &self.project.runtime
            });
            let worksets = project_worksets(&space.worksets, &self.project.root);
            let count = worksets.len();
            let rows = if self.visibility.open {
                worksets
                    .into_iter()
                    .map(|workset| {
                        (
                            workset.clone(),
                            status_summary(ws, &self.project, workset),
                            active.is_some_and(|session| {
                                session.workset_id.as_deref() == Some(workset.id.as_str())
                            }),
                        )
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            (rows, count, space.runtime.clone(), space.directory.loaded())
        };
        let unavailable = crate::runtimes::runtime_unavailable_label(&self.project.runtime, cx);
        let label = if loaded {
            format!("工作树·{count}")
        } else {
            "工作树·—".into()
        };
        let trigger = div()
            .id("workspace-count-trigger")
            .key_context("Popover")
            .track_focus(&self.trigger_focus)
            .role(gpui::Role::Button)
            .aria_label(label.clone())
            .on_hover(
                cx.listener(|this, hovered, window, cx| this.hover(true, *hovered, window, cx)),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_action(cx.listener(|this, _: &Confirm, window, cx| this.pin(window, cx)))
            .on_action(cx.listener(|this, _: &Cancel, window, cx| this.dismiss(window, cx)))
            .child(
                Button::new("workspace-count")
                    .normal()
                    .compact()
                    .capsule()
                    .tab_stop(false)
                    .accessibility_label(label.clone())
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(rgb(ui_theme::text_faint()))
                            .child(label),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        if this.visibility.pinned {
                            this.dismiss(window, cx);
                        } else {
                            this.pin(window, cx);
                        }
                    })),
            );
        let mut popup = gpui_base::Popup::new("workspace-overview-popup", trigger)
            .flex_shrink_0()
            .anchor(Anchor::TopRight)
            .offset(px(4.));
        if self.visibility.open {
            let view = cx.entity();
            let width = px(340.).min((window.viewport_size().width - px(16.)).max(px(0.)));
            let height = px(360.).min((window.viewport_size().height - px(32.)).max(px(0.)));
            let mut content =
                div()
                    .id("workspace-overview-content")
                    .key_context("PopoverContent")
                    .track_focus(&self.content_focus)
                    .w(width)
                    .max_h(height)
                    .overflow_y_scroll()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .rounded(ui_theme::row_radius())
                    .bg(ui_theme::glass_floating())
                    .border_1()
                    .border_color(ui_theme::card_stroke())
                    .text_color(rgb(ui_theme::text()))
                    .on_hover(cx.listener(|this, hovered, window, cx| {
                        this.hover(false, *hovered, window, cx)
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            if !this.visibility.pinned {
                                this.pin(window, cx);
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                        if this.open_menu.is_none() && !this.visibility.trigger_hovered {
                            this.dismiss(window, cx);
                        }
                    }))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                        if this.open_menu.is_none() {
                            this.dismiss(window, cx);
                        }
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_sm()
                            .child(if loaded {
                                format!("工作树 · {count}")
                            } else {
                                "工作树".into()
                            })
                            .child(
                                Button::new("close-workspace-overview")
                                    .quiet()
                                    .icon_size()
                                    .tab_stop(true)
                                    .icon(IconName::Close)
                                    .icon_action("关闭工作树总览")
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.dismiss(window, cx)),
                                    ),
                            ),
                    );
            if let Some(label) = unavailable {
                content = content.child(
                    div()
                        .text_xs()
                        .text_color(rgb(ui_theme::text_muted()))
                        .child(format!("{label}，显示最后已知信息，操作暂不可用")),
                );
            }
            if !loaded || rows.is_empty() {
                content = content.child(div().py_2().text_xs().child(if !loaded {
                    "工作树目录尚未同步"
                } else {
                    "暂无工作树，可通过项目右侧＋新建"
                }));
            }
            for (workset, status, active) in rows {
                let menu_view = view.clone();
                let menu_workspace = workspace.clone();
                let menu_runtime = runtime.clone();
                let menu_workset = workset.clone();
                let open_menu_id = workset.id.clone();
                let menu_id = workset_element_id("workspace-actions", &self.project, &workset.id);
                let online = unavailable.is_none() && loaded;
                let menu = Popover::new(menu_id)
                    .anchor(Anchor::TopRight)
                    .overlay_closable(self.nested_picker.as_deref() != Some(workset.id.as_str()))
                    .trigger(
                        Button::new("workspace-more")
                            .quiet()
                            .icon_size()
                            .tab_stop(true)
                            .icon(IconName::Ellipsis)
                            .icon_action("工作树操作")
                            .disabled(!online),
                    )
                    .on_open_change(move |open, window, cx| {
                        menu_view.update(cx, |view, cx| {
                            if *open {
                                view.open_menu = Some(open_menu_id.clone());
                                view.close_task = None;
                                view.visibility.pin();
                                cx.notify();
                            } else if view.open_menu.as_deref() == Some(open_menu_id.as_str()) {
                                view.open_menu = None;
                                view.nested_picker = None;
                                if !view.visibility.trigger_hovered
                                    && !view.visibility.content_hovered
                                {
                                    view.dismiss(window, cx);
                                } else {
                                    cx.notify();
                                }
                            }
                        });
                    })
                    .content({
                        let view = view.clone();
                        move |_, _, cx| {
                            let rename_view = view.clone();
                            let finish_view = view.clone();
                            let rename_id = menu_workset.id.clone();
                            let finish_id = menu_workset.id.clone();
                            let menu = cx.entity();
                            let rename_menu = menu.clone();
                            let finish_menu = menu;
                            let picker_view = view.clone();
                            let picker_id = menu_workset.id.clone();
                            div()
                                .p_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    new_session::workset_session_picker(
                                        menu_workspace.clone(),
                                        menu_runtime.clone(),
                                        menu_workset.clone(),
                                    )
                                    .on_open_change(
                                        move |open, _, cx| {
                                            picker_view.update(cx, |view, cx| {
                                                if *open {
                                                    view.nested_picker = Some(picker_id.clone());
                                                } else if view.nested_picker.as_deref()
                                                    == Some(picker_id.as_str())
                                                {
                                                    view.nested_picker = None;
                                                }
                                                cx.notify();
                                            });
                                        },
                                    ),
                                )
                                .child(
                                    Button::new("rename-workspace")
                                        .quiet()
                                        .tab_stop(true)
                                        .label("重命名工作树…")
                                        .on_click(move |_, window, cx| {
                                            rename_menu
                                                .update(cx, |menu, cx| menu.dismiss(window, cx));
                                            rename_view.update(cx, |view, cx| {
                                                view.manage(rename_id.clone(), false, window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("finish-workspace")
                                        .quiet()
                                        .tab_stop(true)
                                        .label("收工并删除工作树…")
                                        .on_click(move |_, window, cx| {
                                            finish_menu
                                                .update(cx, |menu, cx| menu.dismiss(window, cx));
                                            finish_view.update(cx, |view, cx| {
                                                view.manage(finish_id.clone(), true, window, cx)
                                            });
                                        }),
                                )
                        }
                    });
                let summary = repository_summary(&workset);
                let full_summary = summary.clone();
                let branch = workset.branch.clone();
                content = content.child(
                    div()
                        .id(workset_element_id(
                            "workspace-overview-item",
                            &self.project,
                            &workset.id,
                        ))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_2()
                        .border_1()
                        .border_color(ui_theme::hairline())
                        .rounded(ui_theme::row_radius())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(div().flex_1().min_w_0().child(truncated_line(workset.task)))
                                .children(active.then(|| div().text_xs().child("当前会话")))
                                .child(menu),
                        )
                        .child(
                            div()
                                .id("workspace-repository-summary")
                                .text_xs()
                                .text_color(rgb(ui_theme::text_muted()))
                                .tooltip(move |window, cx| {
                                    gpui_component::tooltip::Tooltip::new(full_summary.clone())
                                        .build(window, cx)
                                })
                                .child(truncated_line(summary)),
                        )
                        .child(
                            div()
                                .id("workspace-binding-branch")
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_xs()
                                .text_color(rgb(ui_theme::text_muted()))
                                .tooltip(move |window, cx| {
                                    gpui_component::tooltip::Tooltip::new(branch.clone())
                                        .build(window, cx)
                                })
                                .child(
                                    Icon::empty()
                                        .path("smelt-icons/git-branch.svg")
                                        .size(px(12.))
                                        .flex_shrink_0(),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .child(truncated_line(workset.branch)),
                                )
                                .child(status),
                        ),
                );
            }
            popup = popup.content(content);
        }
        popup.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        OverviewVisibility, ProjectSessionRow, overview_view, project_session_rows,
        project_worksets, repository_summary,
    };
    use gpui::{App, Context, Entity, IntoElement, Render, RenderOnce, Subscription, Window, div};
    use smelt_model::workspace_directory::{WorkspaceWorkset, WorkspaceWorksetRepository};

    struct RefreshHost {
        refreshes: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl Render for RefreshHost {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            RefreshBadge {
                host: cx.entity(),
                refreshes: self.refreshes.clone(),
            }
        }
    }

    #[derive(IntoElement)]
    struct RefreshBadge {
        host: Entity<RefreshHost>,
        refreshes: std::rc::Rc<std::cell::Cell<usize>>,
    }

    impl RenderOnce for RefreshBadge {
        fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
            overview_view("refresh-regression".into(), window, cx, |cx| RefreshChild {
                _refresh: cx.observe(&self.host, move |_, _, cx| {
                    let count = self.refreshes.get() + 1;
                    self.refreshes.set(count);
                    // Bound the old notification cycle so the regression fails instead of hanging.
                    if count < 8 {
                        cx.notify();
                    }
                }),
            })
        }
    }

    struct RefreshChild {
        _refresh: Subscription,
    }

    impl Render for RefreshChild {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[gpui::test]
    fn overview_refresh_does_not_notify_its_parent_again(cx: &mut gpui::TestAppContext) {
        let refreshes = std::rc::Rc::new(std::cell::Cell::new(0));
        let (host, cx) = cx.add_window_view({
            let refreshes = refreshes.clone();
            move |_, _| RefreshHost { refreshes }
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        refreshes.set(0);

        host.update(cx, |_, cx| cx.notify());
        assert_eq!(
            refreshes.get(),
            1,
            "one parent change must settle after one refresh"
        );

        cx.update(|window, cx| window.draw(cx).clear(cx));
        host.update(cx, |_, cx| cx.notify());
        assert_eq!(
            refreshes.get(),
            2,
            "redraw must preserve a single subscription"
        );
    }

    fn workset(id: &str, root: &str, position: u32) -> WorkspaceWorkset {
        WorkspaceWorkset {
            id: id.into(),
            project_root: root.into(),
            task: id.into(),
            branch: format!("feat/{id}"),
            position,
            repositories: vec![
                WorkspaceWorksetRepository {
                    name: "tools".into(),
                    position: 1,
                    ..Default::default()
                },
                WorkspaceWorksetRepository {
                    name: "smelt".into(),
                    position: 0,
                    ..Default::default()
                },
            ],
        }
    }

    #[test]
    fn shared_worktrees_group_sessions_without_absorbing_ordinary_tasks() {
        let worksets = vec![
            workset("later", "/project", 1),
            workset("first", "/project", 0),
            workset("empty", "/project", 2),
            workset("foreign", "/elsewhere", 0),
        ];
        let members = [
            (0, Some("later")),
            (1, None),
            (2, Some("first")),
            (3, Some("later")),
            (4, Some("missing")),
            (5, Some("foreign")),
        ];
        let rows = project_session_rows(&worksets, "/project", &members);
        let labels = rows
            .iter()
            .map(|row| match row {
                ProjectSessionRow::Worktree(workset) => workset.task.clone(),
                ProjectSessionRow::Session { index, .. } => format!("session-{index}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                "session-1",
                "session-4",
                "session-5",
                "first",
                "session-2",
                "later",
                "session-0",
                "session-3"
            ]
        );
    }

    #[test]
    fn overview_filters_exact_project_and_keeps_configured_order() {
        let rows = vec![
            workset("later", "/project", 1),
            workset("other", "/project-other", 0),
            workset("first", "/project", 0),
        ];
        let selected = project_worksets(&rows, "/project/");
        assert_eq!(
            selected
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "later"]
        );
        assert_eq!(repository_summary(selected[0]), "2 个仓库：smelt、tools");
    }

    #[test]
    fn crossing_into_content_cancels_hover_close_and_click_pins() {
        let mut visibility = OverviewVisibility::default();
        visibility.trigger_hover(true);
        visibility.trigger_hover(false);
        assert!(visibility.open);
        visibility.content_hover(true);
        visibility.close_if_idle();
        assert!(visibility.open);
        visibility.pin();
        visibility.content_hover(false);
        visibility.close_if_idle();
        assert!(visibility.open);
        visibility.dismiss();
        assert!(!visibility.open);
    }

    #[test]
    fn leaving_unpinned_overview_closes_only_after_the_hover_grace_period() {
        let mut visibility = OverviewVisibility::default();
        visibility.trigger_hover(true);
        visibility.trigger_hover(false);
        assert!(visibility.open);
        visibility.close_if_idle();
        assert!(!visibility.open);
    }
}
