use super::*;
use gpui_base::actions::{Cancel, Confirm};
use smelt_model::workspace_directory::WorkspaceWorkset;
use std::collections::HashSet;

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
    pinned: &HashSet<String>,
) -> Vec<ProjectSessionRow<'a>> {
    let mut worksets = project_worksets(worksets, project_root);
    worksets.sort_by_key(|workset| !pinned.contains(&workset.id));
    let ordinary = members
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
    let mut rows = Vec::new();
    let mut ordinary = Some(ordinary);
    for workset in worksets {
        if !pinned.contains(&workset.id) {
            if let Some(ordinary) = ordinary.take() {
                rows.extend(ordinary);
            }
        }
        let members = members
            .iter()
            .filter(|(_, id)| *id == Some(workset.id.as_str()))
            .collect::<Vec<_>>();
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
    if let Some(ordinary) = ordinary {
        rows.extend(ordinary);
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
        if let Some(space) = workspace.space(&group.runtime) {
            project_session_rows(
                &space.worksets,
                &group.root,
                &members,
                &space.pinned_worksets,
            )
        } else {
            project_session_rows(&[], &group.root, &members, &HashSet::new())
        }
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
    fn toggle_workset_pinned(&mut self, project: &ProjectRef, id: &str, cx: &mut Context<Self>) {
        let Some(space) = self.space(&project.runtime) else {
            self.report_background_error("运行时已断开，无法修改工作树置顶状态");
            return;
        };
        if !space.worksets.iter().any(|workset| {
            workset.id == id
                && crate::sidebar_order::same_project_root(&workset.project_root, &project.root)
        }) {
            self.report_background_error("工作树已不存在，请刷新后重试");
            return;
        }
        let pinned = !space.pinned_worksets.contains(id);
        let runtime_id = match &project.runtime {
            RuntimeKey::Local => None,
            RuntimeKey::Remote(id) => Some(id.as_str()),
        };
        if let Err(error) = smelt_client_store::client_store()
            .and_then(|store| store.set_workset_pinned(runtime_id, id, pinned))
        {
            self.report_background_error(format!("保存工作树置顶状态失败：{error}"));
            return;
        }
        if let Some(space) = self.space_mut(&project.runtime) {
            if pinned {
                space.pinned_worksets.insert(id.to_string());
            } else {
                space.pinned_worksets.remove(id);
            }
        }
        cx.notify();
    }

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
    let pinned = workspace
        .space(&project.runtime)
        .is_some_and(|space| space.pinned_worksets.contains(&workset.id));
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
    let menu_entity = entity.clone();
    let menu_project = project.clone();
    let menu_workset = workset.clone();
    let menu_runtime = runtime.clone();
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
    .group("task-worktree-row")
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
    .child(row::group_disclosure(
        workset_element_id("task-worktree-disclosure", project, &workset.id),
        "task-worktree-row",
        Icon::empty()
            .path("smelt-icons/git-branch.svg")
            .size(px(16.))
            .flex_shrink_0(),
        collapsed,
        count > 0,
        None,
    ))
    .child(
        div()
            .flex_1()
            .min_w_0()
            .text_sm()
            .text_color(rgb(ui_theme::text_muted()))
            .child(truncated_line(workset.task.clone())),
    )
    .children(pinned.then(|| {
        Icon::new(gpui_kit_assets::IconName::Pin)
            .size(px(12.))
            .text_color(rgb(ui_theme::text_faint()))
    }))
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
            if count > 0 {
                workspace.toggle_worktree_collapsed(&click_project, &click_id, cx);
            }
        });
    })
    .context_menu(move |menu, window, cx| {
        let pin_entity = menu_entity.clone();
        let pin_project = menu_project.clone();
        let pin_id = menu_workset.id.clone();
        let rename_entity = menu_entity.clone();
        let rename_id = menu_workset.id.clone();
        let close_entity = menu_entity.clone();
        let close_project = menu_project.clone();
        let close_id = menu_workset.id.clone();
        let mut menu = menu;
        if let Some(runtime) = menu_runtime.clone() {
            menu = new_session::workset_session_submenu(
                menu,
                menu_entity.clone(),
                runtime,
                menu_workset.clone(),
                window,
                cx,
            );
        }
        menu.item(
            PopupMenuItem::new(if pinned { "取消置顶" } else { "置顶" })
                .icon(Icon::new(gpui_kit_assets::IconName::Pin))
                .on_click(move |_, _, cx| {
                    pin_entity.update(cx, |ws, cx| {
                        ws.toggle_workset_pinned(&pin_project, &pin_id, cx)
                    });
                }),
        )
        .item(
            PopupMenuItem::new("重命名工作树…").on_click(move |_, window, cx| {
                rename_entity.update(cx, |ws, cx| {
                    ws.start_rename(RenameTarget::Workset(rename_id.clone()), window, cx)
                });
            }),
        )
        .separator()
        .item(
            PopupMenuItem::new("关闭工作树…")
                .icon(Icon::empty().path("smelt-icons/trash.svg"))
                .on_click(move |_, _, cx| {
                    close_entity.update(cx, |ws, cx| {
                        ws.start_close_workset(close_project.clone(), close_id.clone(), cx)
                    });
                }),
        )
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

#[derive(Debug, PartialEq, Eq)]
struct WorktreeStatusSummary {
    changed: bool,
    ahead: u32,
    behind: u32,
}

fn summarize_statuses(
    statuses: &[&crate::GitStatusData],
    repository_count: usize,
) -> Option<WorktreeStatusSummary> {
    if statuses.is_empty()
        || statuses.len() != repository_count
        || statuses.iter().any(|status| !status.ok)
    {
        return None;
    }
    Some(WorktreeStatusSummary {
        changed: statuses.iter().any(|status| !status.files.is_empty()),
        ahead: statuses.iter().map(|status| status.ahead).sum(),
        behind: statuses.iter().map(|status| status.behind).sum(),
    })
}

fn status_summary(
    workspace: &Workspace,
    project: &ProjectRef,
    workset: &WorkspaceWorkset,
) -> Option<WorktreeStatusSummary> {
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
    summarize_statuses(&statuses, workset.repositories.len())
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
            _workspace_refresh: cx.observe(&workspace, |_this, _workspace, cx| {
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

    fn manage(&mut self, workset_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let project = self.project.clone();
        self.dismiss(window, cx);
        workspace.update(cx, |workspace, cx| {
            workspace.start_close_workset(project, workset_id, cx);
        });
    }
}

impl Render for WorkspaceOverview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(workspace) = self.workspace.upgrade() else {
            return div().into_any_element();
        };
        let (rows, count, loaded) = {
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
            (rows, count, space.directory.loaded())
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
            .anchor(Anchor::TopLeft)
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
                        if !this.visibility.trigger_hovered {
                            this.dismiss(window, cx);
                        }
                    }))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .on_action(cx.listener(|this, _: &Cancel, window, cx| {
                        this.dismiss(window, cx);
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
                let finish_view = view.clone();
                let finish_id = workset.id.clone();
                let online = unavailable.is_none() && loaded;
                let finish = Button::new(workset_element_id(
                    "finish-workspace",
                    &self.project,
                    &workset.id,
                ))
                .quiet()
                .icon_size()
                .tab_stop(true)
                .icon(Icon::empty().path("smelt-icons/trash.svg"))
                .icon_action("关闭工作树")
                .disabled(!online)
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    finish_view.update(cx, |view, cx| view.manage(finish_id.clone(), window, cx));
                });
                let summary = repository_summary(&workset);
                let full_summary = summary.clone();
                let branch = workset.branch.clone();
                let status = match status {
                    Some(status) => {
                        let (icon, description) = if status.changed {
                            (
                                Icon::empty().path("smelt-icons/square-pen.svg"),
                                "有未提交修改或未跟踪文件",
                            )
                        } else {
                            (Icon::new(IconName::CircleCheck), "没有未提交修改")
                        };
                        div()
                            .id("workspace-git-status")
                            .flex()
                            .items_center()
                            .gap_1()
                            .aria_label(description)
                            .tooltip(move |window, cx| {
                                gpui_component::tooltip::Tooltip::new(description).build(window, cx)
                            })
                            .child(icon.size(px(13.)).flex_shrink_0())
                            .children((status.ahead > 0).then(|| format!("↑{}", status.ahead)))
                            .children((status.behind > 0).then(|| format!("↓{}", status.behind)))
                            .into_any_element()
                    }
                    None => div().child("状态未知").into_any_element(),
                };
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
                                .child(finish),
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
                                .child(
                                    Icon::empty()
                                        .path("smelt-icons/git-branch.svg")
                                        .size(px(12.))
                                        .flex_shrink_0(),
                                )
                                .child(
                                    div()
                                        .id("workspace-branch-label")
                                        .flex_1()
                                        .min_w_0()
                                        .tooltip(move |window, cx| {
                                            gpui_component::tooltip::Tooltip::new(branch.clone())
                                                .build(window, cx)
                                        })
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
        OverviewVisibility, ProjectSessionRow, WorktreeStatusSummary, overview_view,
        project_session_rows, project_worksets, repository_summary, summarize_statuses,
    };
    use gpui::{App, Context, Entity, IntoElement, Render, RenderOnce, Subscription, Window, div};
    use smelt_model::workspace_directory::{WorkspaceWorkset, WorkspaceWorksetRepository};
    use std::collections::HashSet;

    #[test]
    fn worktree_status_summarizes_changes_and_preserves_sync_counts() {
        let clean = crate::GitStatusData {
            ok: true,
            ahead: 2,
            ..Default::default()
        };
        let changed = crate::GitStatusData {
            ok: true,
            behind: 3,
            files: vec![("??".into(), "new-file".into())],
            ..Default::default()
        };
        assert_eq!(
            summarize_statuses(&[&clean], 1),
            Some(WorktreeStatusSummary {
                changed: false,
                ahead: 2,
                behind: 0,
            })
        );
        assert_eq!(
            summarize_statuses(&[&clean, &changed], 2),
            Some(WorktreeStatusSummary {
                changed: true,
                ahead: 2,
                behind: 3,
            })
        );
        assert_eq!(summarize_statuses(&[], 0), None);
        assert_eq!(summarize_statuses(&[&clean], 2), None);
        assert_eq!(
            summarize_statuses(&[&crate::GitStatusData::default()], 1),
            None
        );
    }

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
        let rows = project_session_rows(&worksets, "/project", &members, &HashSet::new());
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
                "session-3",
                "empty"
            ]
        );
    }

    #[test]
    fn worktree_row_survives_removal_of_its_last_session() {
        let worksets = vec![workset("task", "/project", 0)];
        let before =
            project_session_rows(&worksets, "/project", &[(0, Some("task"))], &HashSet::new());
        assert_eq!(before.len(), 2);
        assert!(matches!(before[0], ProjectSessionRow::Worktree(_)));
        assert!(matches!(before[1], ProjectSessionRow::Session { .. }));

        let after = project_session_rows(&worksets, "/project", &[], &HashSet::new());
        assert_eq!(after.len(), 1);
        assert!(matches!(&after[0], ProjectSessionRow::Worktree(workset) if workset.id == "task"));
        assert!(project_session_rows(&[], "/project", &[], &HashSet::new()).is_empty());
    }

    #[test]
    fn pinned_worktrees_precede_other_worktrees_and_keep_their_sessions() {
        let worksets = vec![
            workset("first", "/project", 0),
            workset("pinned", "/project", 1),
        ];
        let pinned = HashSet::from(["pinned".to_string()]);
        let rows = project_session_rows(
            &worksets,
            "/project",
            &[(0, Some("first")), (1, Some("pinned")), (2, None)],
            &pinned,
        );
        assert!(matches!(&rows[0], ProjectSessionRow::Worktree(workset) if workset.id == "pinned"));
        assert!(matches!(
            &rows[1],
            ProjectSessionRow::Session { index: 1, .. }
        ));
        assert!(matches!(
            &rows[2],
            ProjectSessionRow::Session { index: 2, .. }
        ));
        assert!(matches!(&rows[3], ProjectSessionRow::Worktree(workset) if workset.id == "first"));
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
