//! 各种行怎么画：「对话」一段、机器、项目、工作树、会话。只读 [`Sidebar`] 和视图上的
//! 临时状态（[`RowCtx`]），不读库、不读文件。
//!
//! 层级靠缩进：「对话」标题、机器标题（或只有本机时的「项目」标题）在最外，项目、按状态
//! 和时间分的组、「对话」里的会话缩一级，项目和组里的会话、工作树再缩一级，工作树里的会话
//! 再缩一级。

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Transformation, Window, div, px, radians, rgb,
    svg,
};
use gpui_base::StyledExt as _;
use gpui_base::input::InputState;
use gpui_kit_assets::IconName;
use smelt_plugin_api::model::agent_status::AgentStatus;
use smelt_plugin_api::ui::menu::{Menu, MenuTrigger};
use smelt_plugin_api::ui::tooltip::Tooltip;
use smelt_plugin_api::ui::truncated_line::truncated_line;
use smelt_plugin_api::ui::ui_theme::{self, height};
use smelt_plugin_api::ui::{Button, IconAction as _, ListRow, TextInput};
use smelt_plugin_api::{self as api, RuntimeKey};

use super::machine::Light;
use super::view::{DropHint, DropTarget, ListView, RenameTarget};
use super::{
    BucketGroup, ConversationsSection, GroupKey, MachineSection, ProjectGroup, ProjectItem,
    RowIcon, RowOrigin, SessionRow, Sidebar, WorksetGroup, actions, menus, project_visible,
};
use crate::lifecycle;
use crate::model::{ConversationKey, Fold, Selection, Sessions, SidebarGrouping};
use crate::{contract, dialogs};

/// 每缩一级多少。
const INDENT: f32 = 16.;
/// 行首那一格（图标、展开箭头）的边长。
const LEAD: f32 = 16.;

/// 画一遍要的东西：算好的侧栏，加上视图上的临时状态。
pub(super) struct RowCtx {
    pub(super) sidebar: Rc<Sidebar>,
    pub(super) view: Entity<ListView>,
    pub(super) selection: Selection,
    /// 拖着时的落点；没在拖是 `None`。
    pub(super) drop: Option<DropHint>,
    pub(super) rename: Option<(RenameTarget, Entity<InputState>)>,
    /// 现在（unix 秒），行尾的相对时间按它算。
    pub(super) now: u64,
}

/// 整个列表从上到下的行。
pub(super) fn sidebar(ctx: &RowCtx, cx: &App) -> Vec<AnyElement> {
    let mut rows = Vec::new();
    if let Some(section) = &ctx.sidebar.conversations {
        conversations(section, ctx, &mut rows);
    }
    // 「筛选与分组」只有一个：不分段时在「项目」标题上，分段时在第一台机器的标题上。
    let mut grouping = Some(grouping_button(&ctx.sidebar));
    for (machine_index, section) in ctx.sidebar.machines.iter().enumerate() {
        machine(machine_index, section, grouping.take(), ctx, cx, &mut rows);
    }
    rows
}

/// 「筛选与分组」按钮：分组依据和显示哪些项目。
fn grouping_button(sidebar: &Sidebar) -> AnyElement {
    let (grouping, hide_empty) = (sidebar.grouping, sidebar.hide_empty);
    MenuTrigger::dropdown(
        "session-grouping-menu",
        Button::new("session-grouping")
            .quiet()
            .small()
            .icon(IconName::SlidersHorizontal)
            .icon_action("筛选与分组"),
        move |_, _| menus::grouping_menu(grouping, hide_empty),
    )
    .anchor(gpui::Anchor::TopRight)
    .into_any_element()
}

// ---- 「对话」一段 ----

fn conversations(section: &ConversationsSection, ctx: &RowCtx, rows: &mut Vec<AnyElement>) {
    let collapsed = section.collapsed;
    let header = ListRow::new("sessions-conversations")
        .group("group-row")
        .h(height::sm())
        .mx_2()
        .mt_2()
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .font_medium()
        .child(disclosure(IconName::MessageSquare, collapsed, true))
        .child(div().flex_1().min_w_0().child("对话"))
        .when(collapsed, |row| {
            row.child(collapsed_summary(section.status, section.rows.len(), false))
        })
        .on_click(move |_, _, cx| toggle_fold(Fold::Conversations, !collapsed, cx));
    rows.push(header.into_any_element());
    if !collapsed {
        rows.extend(section.rows.iter().map(|row| session(row, 1, ctx)));
    }
}

// ---- 机器 ----

fn machine(
    machine_index: usize,
    section: &MachineSection,
    grouping: Option<AnyElement>,
    ctx: &RowCtx,
    cx: &App,
    rows: &mut Vec<AnyElement>,
) {
    rows.push(if ctx.sidebar.segmented {
        machine_header(section, grouping, cx)
    } else {
        projects_header(section, grouping, cx)
    });
    if section.collapsed {
        return;
    }
    if !section.received {
        rows.push(empty_note("连接后显示项目"));
        return;
    }
    if section.projects.is_empty() {
        rows.push(empty_note("没有打开的项目"));
        return;
    }
    if ctx.sidebar.grouping != SidebarGrouping::Project {
        if section.buckets.is_empty() {
            rows.push(empty_note("没有会话"));
        }
        for bucket in &section.buckets {
            rows.push(bucket_row(&section.key, bucket));
            if !bucket.collapsed {
                rows.extend(bucket.rows.iter().map(|row| session(row, 2, ctx)));
            }
        }
        return;
    }
    let mut hidden = 0;
    for (project_index, project) in section.projects.iter().enumerate() {
        let selected = is_selected_project(ctx, project);
        let has_items = !project.items.is_empty();
        if !project_visible(ctx.sidebar.hide_empty, has_items, project.pinned, selected) {
            hidden += 1;
            continue;
        }
        rows.push(project_row(machine_index, project_index, project, ctx));
        if project.collapsed {
            continue;
        }
        for (item_index, item) in project.items.iter().enumerate() {
            match item {
                ProjectItem::Session(row) => rows.push(session(row, 2, ctx)),
                ProjectItem::Workset(workset) => {
                    let at = (machine_index, project_index, item_index);
                    rows.push(workset_row(at, workset, ctx));
                    if !workset.collapsed {
                        rows.extend(workset.sessions.iter().map(|row| session(row, 3, ctx)));
                    }
                }
            }
        }
    }
    if hidden > 0 {
        rows.push(hidden_empty_row(&section.key, hidden));
    }
}

/// 挂了远程机器时每台机器的段标题：系统图标按连接上色，悬停整行换成开合箭头、悬停名字看
/// 连接说明，点行开合；折起时行尾标状态点和会话数。能用时悬停露出「打开项目」，第一台还带
/// 「筛选与分组」。
fn machine_header(section: &MachineSection, grouping: Option<AnyElement>, cx: &App) -> AnyElement {
    let key = section.key.clone();
    let id = machine_id(&key).to_string();
    let local = matches!(key, RuntimeKey::Local);
    let light = match section.link.light {
        Light::Good => ui_theme::ok(),
        Light::Degraded => ui_theme::need(),
        Light::Down => ui_theme::bad(),
    };
    let detail: SharedString = section.link.detail.clone().into();
    let collapsed = section.collapsed;
    let icon = svg()
        .path(section.os_icon.clone())
        .size(px(13.))
        .text_color(rgb(light));
    let header = div()
        .id(eid(format!("machine:{id}")))
        .group("machine-row")
        .h(height::sm())
        .mx_2()
        .mt_3()
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .font_medium()
        .text_color(rgb(ui_theme::muted()))
        .child(disclosure_slot("machine-row", icon, collapsed, true))
        .child(
            div()
                .id(eid(format!("machine-name:{id}")))
                .flex_1()
                .min_w_0()
                .child(truncated_line(section.name.clone()))
                .tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx)),
        )
        .when(collapsed, |header| {
            let status = AgentStatus::highest(section.projects.iter().map(|p| p.status));
            let count = section.projects.iter().map(|p| p.sessions).sum();
            header.child(collapsed_summary(status, count, false))
        })
        .on_click({
            let key = key.clone();
            move |_, _, cx| {
                Sessions::global(cx).update(cx, |sessions, cx| {
                    sessions.set_machine_collapsed(&key, !collapsed, cx)
                })
            }
        })
        // 连接说明里的延迟只在有人看时采样：悬停远程机器时叫宿主重读一次。
        .when(!local, |header| {
            header.on_hover(|hovered, _, cx| {
                if *hovered {
                    api::REFRESH_CONNECTIONS.run(&(), cx).detach();
                }
            })
        })
        .child(
            hover_only("machine-row")
                .flex()
                .items_center()
                // 点按钮不开合这台机器。
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .children(grouping)
                // 连不上的机器挑不了目录，不给按钮。
                .when(section.link.unavailable.is_none(), |slot| {
                    let key = key.clone();
                    slot.child(
                        Button::new(eid(format!("machine-open:{id}")))
                            .quiet()
                            .small()
                            .icon(IconName::FolderPlus)
                            .icon_action("打开项目")
                            .on_click(move |_, _, cx| actions::pick_project(key.clone(), cx)),
                    )
                }),
        );
    with_machine_menu(&key, header, cx)
}

/// 只挂着本机时最上面的「项目」标题（不画机器行）：悬停露出「筛选与分组」和「添加项目」；
/// 本机的菜单（别的插件登记的）也挂在这里，跟机器行一样。
fn projects_header(section: &MachineSection, grouping: Option<AnyElement>, cx: &App) -> AnyElement {
    let header = div()
        .id("project-section-header")
        .group("projects-header")
        .h(height::sm())
        .mx_2()
        .mt_3()
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .text_xs()
        .font_medium()
        .text_color(rgb(ui_theme::muted()))
        .child(div().flex_1().min_w_0().child("项目"))
        .child(
            hover_only("projects-header")
                .flex()
                .items_center()
                .children(grouping)
                .when(section.link.unavailable.is_none(), |slot| {
                    slot.child(
                        Button::new("open-project")
                            .quiet()
                            .small()
                            .icon(IconName::FolderPlus)
                            .icon_action("添加项目")
                            .on_click(|_, _, cx| {
                                let opened = contract::OPEN_PROJECT.run(&(), cx);
                                crate::report_failure("添加项目", opened, cx);
                            }),
                    )
                }),
        );
    with_machine_menu(&section.key, header, cx)
}

/// 机器有菜单（别的插件往 [`contract::MACHINE_MENU`] 登记了项）时，标题行尾加「⋯」，整行
/// 接右键。
fn with_machine_menu(key: &RuntimeKey, header: gpui::Stateful<gpui::Div>, cx: &App) -> AnyElement {
    if !menus::has_machine_menu(key, cx) {
        return header.into_any_element();
    }
    let id = machine_id(key).to_string();
    let header = header.child({
        let key = key.clone();
        MenuTrigger::dropdown(
            eid(format!("machine-menu:{id}")),
            Button::new(eid(format!("machine-more:{id}")))
                .quiet()
                .small()
                .icon(IconName::Ellipsis)
                .icon_action("更多"),
            move |_, cx| menus::machine_menu(&key, cx).unwrap_or_else(Menu::new),
        )
        .anchor(gpui::Anchor::TopRight)
    });
    let key = key.clone();
    MenuTrigger::context(
        eid(format!("machine-context:{id}")),
        header,
        move |_, cx| menus::machine_menu(&key, cx).unwrap_or_else(Menu::new),
    )
    .into_any_element()
}

/// 按项目分组、「显示：有会话」时，藏起来的空项目有几个；点一下回到「显示：全部」。
fn hidden_empty_row(machine: &RuntimeKey, count: usize) -> AnyElement {
    ListRow::new(eid(format!("hidden-empty:{}", machine_id(machine))))
        .h(height::sm())
        .mx_2()
        .pl(px(8. + INDENT))
        .pr_2()
        .flex()
        .items_center()
        .text_xs()
        .text_color(rgb(ui_theme::faint()))
        .child(format!("已隐藏 {count} 个无会话项目"))
        .on_click(|_, _, cx| {
            Sessions::global(cx).update(cx, |sessions, cx| {
                sessions.set_hide_empty_projects(false, cx)
            });
        })
        .into_any_element()
}

fn empty_note(text: &'static str) -> AnyElement {
    div()
        .mx_2()
        .px_2()
        .pl(px(8. + INDENT))
        .h(height::sm())
        .flex()
        .items_center()
        .text_xs()
        .text_color(rgb(ui_theme::faint()))
        .child(text)
        .into_any_element()
}

// ---- 项目 ----

#[derive(Clone)]
pub(super) struct ProjectDrag {
    machine: RuntimeKey,
    root: String,
    name: SharedString,
}

fn project_row(
    machine_index: usize,
    project_index: usize,
    project: &ProjectGroup,
    ctx: &RowCtx,
) -> AnyElement {
    let machine = project.machine.clone();
    let root = project.root.clone();
    let id = format!("{}:{}", machine_id(&machine), root);
    let selected = is_selected_project(ctx, project);
    let collapsed = project.collapsed;
    let collapsible = !project.items.is_empty();
    let fold = Fold::Project {
        machine: machine.clone(),
        root: root.clone(),
    };
    let hint = ctx.drop.as_ref().and_then(|hint| match &hint.target {
        DropTarget::Project {
            machine: m,
            root: r,
        } if *m == machine && *r == root => Some(hint.before),
        _ => None,
    });
    let view = ctx.view.clone();
    let drag = ProjectDrag {
        machine: machine.clone(),
        root: root.clone(),
        name: project.name.clone(),
    };

    let row = ListRow::new(eid(format!("project:{id}")))
        .selected(selected)
        .group("group-row")
        .relative()
        .h(height::md())
        .mx_2()
        .pl(px(8. + INDENT))
        .pr_1()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .child(fold_toggle(
            eid(format!("project-fold:{id}")),
            disclosure(IconName::Folder, collapsed, collapsible),
            collapsible.then_some((fold, collapsed)),
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_1p5()
                .child(
                    div()
                        .min_w_0()
                        .when(selected, |name| name.font_medium())
                        .child(truncated_line(project.name.clone())),
                )
                .when(project.pinned, |name| name.child(pin_marker()))
                .when_some(project.branch.clone(), |name, branch| {
                    // 窄侧栏里分支名常被截掉后半段，悬停看全名。
                    let full = branch.clone();
                    name.child(
                        div()
                            .id(eid(format!("project-branch:{id}")))
                            .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                            .flex_shrink_0()
                            .max_w(px(120.))
                            .text_xs()
                            .text_color(rgb(ui_theme::row_faint(selected)))
                            .child(truncated_line(branch)),
                    )
                })
                .when(project.repositories > 1, |name| {
                    name.child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(rgb(ui_theme::row_faint(selected)))
                            .child(format!("{} 仓库", project.repositories)),
                    )
                }),
        )
        .when(collapsed, |row| {
            row.child(collapsed_summary(project.status, project.sessions, selected))
        })
        .when(!root.is_empty(), |row| {
            let (machine, root) = (machine.clone(), root.clone());
            row.child(
                hover_only("group-row").child(
                    Button::new(eid(format!("project-new:{id}")))
                        .quiet()
                        .small()
                        .icon(IconName::Plus)
                        .icon_action("新建会话")
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            dialogs::open_new_session(
                                Some(machine.clone()),
                                Some(root.clone()),
                                window,
                                cx,
                            )
                        }),
                ),
            )
        })
        .when_some(hint, |row, before| row.child(drop_line(before)))
        // 点名字选中项目（主区显示项目页）；折叠只靠行首的箭头。
        .on_click({
            let (machine, root) = (machine.clone(), root.clone());
            move |_, _, cx| {
                if !root.is_empty() {
                    lifecycle::select_project(machine.clone(), root.clone(), cx);
                }
            }
        })
        .on_drag(drag, {
            let view = view.clone();
            move |drag, _, window, cx| {
                clear_drop(&view, cx);
                preview(drag.name.clone(), window, cx)
            }
        })
        .on_drag_move::<ProjectDrag>({
            let (view, machine, root) = (view.clone(), machine.clone(), root.clone());
            move |event, _, cx| {
                if !event.bounds.contains(&event.event.position) {
                    return;
                }
                let dragged = event.drag(cx);
                let hint = (dragged.machine == machine && dragged.root != root).then(|| DropHint {
                    target: DropTarget::Project {
                        machine: machine.clone(),
                        root: root.clone(),
                    },
                    before: event.event.position.y < event.bounds.center().y,
                });
                view.update(cx, |view, cx| view.set_drop(hint, cx));
            }
        })
        .on_drop::<ProjectDrag>({
            let (view, machine, root) = (view.clone(), machine.clone(), root.clone());
            move |dragged, _, cx| {
                let target = DropTarget::Project {
                    machine: machine.clone(),
                    root: root.clone(),
                };
                let Some(before) = take_drop(&view, &target, cx) else {
                    return;
                };
                if dragged.machine == machine {
                    actions::drop_project(&machine, &dragged.root, &root, before, cx);
                }
            }
        });

    let sidebar = ctx.sidebar.clone();
    MenuTrigger::context(eid(format!("project-menu:{id}")), row, move |_, cx| {
        let project = &sidebar.machines[machine_index].projects[project_index];
        menus::project_menu(project, cx)
    })
    .into_any_element()
}

fn is_selected_project(ctx: &RowCtx, project: &ProjectGroup) -> bool {
    ctx.selection
        .project()
        .is_some_and(|(machine, root)| *machine == project.machine && root == project.root)
}

// ---- 按状态、时间分的组 ----

/// 组的标题行：点一下开合。组是现算的，没有菜单、不能拖、不能在组里新建会话。
fn bucket_row(machine: &RuntimeKey, group: &BucketGroup) -> AnyElement {
    let (bucket, collapsed) = (group.bucket, group.collapsed);
    let machine = machine.clone();
    ListRow::new(eid(format!("bucket:{}:{bucket:?}", machine_id(&machine))))
        .group("group-row")
        .h(height::md())
        .mx_2()
        .pl(px(8. + INDENT))
        .pr_1()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .child(disclosure(IconName::Folder, collapsed, true))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(truncated_line(bucket.label())),
        )
        .when(collapsed, |row| {
            row.child(collapsed_summary(group.status, group.rows.len(), false))
        })
        .on_click(move |_, _, cx| {
            Sessions::global(cx).update(cx, |sessions, cx| {
                sessions.set_bucket_collapsed(&machine, bucket, !collapsed, cx)
            });
        })
        .into_any_element()
}

// ---- 工作树 ----

fn workset_row(
    (machine_index, project_index, item_index): (usize, usize, usize),
    workset: &WorksetGroup,
    ctx: &RowCtx,
) -> AnyElement {
    let machine = workset.machine.clone();
    let workset_id = workset.id.clone();
    let id = format!("{}:{}", machine_id(&machine), workset_id);
    let collapsed = workset.collapsed;
    let collapsible = !workset.sessions.is_empty();
    let fold = Fold::Workset {
        machine: machine.clone(),
        id: workset_id.clone(),
    };
    let renaming = rename_input(
        ctx,
        &RenameTarget::Workset {
            machine: machine.clone(),
            id: workset_id.clone(),
        },
    );
    let tooltip = workset.tooltip.clone();
    let view = ctx.view.clone();

    let title = match renaming {
        Some(input) => rename_field(input),
        None => div()
            .id(eid(format!("workset-name:{id}")))
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1p5()
            .child(div().min_w_0().child(truncated_line(workset.task.clone())))
            .when(workset.pinned, |name| name.child(pin_marker()))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .into_any_element(),
    };
    let row = ListRow::new(eid(format!("workset:{id}")))
        .group("group-row")
        .h(height::md())
        .mx_2()
        .pl(px(8. + INDENT * 2.))
        .pr_1()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .child(disclosure(IconName::GitBranch, collapsed, collapsible))
        .child(title)
        .when(collapsed, |row| {
            row.child(collapsed_summary(workset.status, workset.sessions.len(), false))
        })
        .child(
            hover_only("group-row").child({
                let (machine, path) = (machine.clone(), workset.path.clone());
                Button::new(eid(format!("workset-new:{id}")))
                    .quiet()
                    .small()
                    .icon(IconName::Plus)
                    .icon_action("在工作树里新建会话")
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        dialogs::open_new_session(
                            Some(machine.clone()),
                            Some(path.clone()),
                            window,
                            cx,
                        )
                    })
            }),
        )
        // 工作树没有自己的页：点一下就是开合（改名走菜单，免得双击先开合一次）。
        .on_click(move |_, _, cx| {
            if collapsible {
                toggle_fold(fold.clone(), !collapsed, cx);
            }
        });

    let sidebar = ctx.sidebar.clone();
    MenuTrigger::context(eid(format!("workset-menu:{id}")), row, move |_, _| {
        let project = &sidebar.machines[machine_index].projects[project_index];
        match &project.items[item_index] {
            ProjectItem::Workset(workset) => menus::workset_menu(workset, view.clone()),
            ProjectItem::Session(_) => Menu::new(),
        }
    })
    .into_any_element()
}

// ---- 会话 ----

#[derive(Clone)]
pub(super) struct SessionDrag {
    key: ConversationKey,
    group: GroupKey,
    title: SharedString,
}

fn session(row: &SessionRow, depth: usize, ctx: &RowCtx) -> AnyElement {
    let key = row.key.clone();
    let id = format!("{}:{}", machine_id(&key.machine), key.id);
    let selected = ctx.selection.session() == Some(&key);
    // 只有按项目分组时顺序是手动的；按状态、时间分组的顺序是算出来的，拖了也对不上。
    let draggable = ctx.sidebar.grouping == SidebarGrouping::Project
        && matches!(row.group, GroupKey::Project { .. });
    let target = RenameTarget::Conversation(key.clone());
    let renaming = rename_input(ctx, &target);
    let hint = ctx.drop.as_ref().and_then(|hint| match &hint.target {
        DropTarget::Session(target) if *target == key => Some(hint.before),
        _ => None,
    });
    let view = ctx.view.clone();

    let title = match renaming {
        Some(input) => rename_field(input),
        None => div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1p5()
            .child(
                div()
                    .min_w_0()
                    .when(selected, |title| title.font_medium())
                    .child(truncated_line(row.title.clone())),
            )
            .when(row.pinned, |title| title.child(pin_marker()))
            .when_some(row.origin.as_ref(), |title, origin| {
                title.children(origin_labels(&id, origin, selected))
            })
            .into_any_element(),
    };
    let close = {
        let (key, title, status) = (key.clone(), row.title.clone(), row.status);
        Button::new(eid(format!("session-close:{id}")))
            .dismiss()
            .small()
            .icon(IconName::Close)
            .icon_action("关闭")
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                actions::close_session(key.clone(), title.clone(), status, window, cx)
            })
    };
    let item = ListRow::new(eid(format!("session:{id}")))
        .selected(selected)
        .group("session-row")
        .relative()
        .h(height::md())
        .mx_2()
        .pl(px(8. + INDENT * depth as f32))
        .pr_1()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .child(session_icon(&id, &row.icon, row.status, selected))
        .child(title)
        .child(trailing(meta(row, selected, ctx.now), close))
        .tooltip({
            let tooltip = row.tooltip.clone();
            move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)
        })
        .when_some(hint, |item, before| item.child(drop_line(before)))
        .on_click({
            let (key, title, view) = (key.clone(), row.title.to_string(), view.clone());
            move |event, window, cx| {
                if event.click_count() == 2 {
                    let target = RenameTarget::Conversation(key.clone());
                    let title = title.clone();
                    view.update(cx, |view, cx| view.start_rename(target, title, window, cx));
                } else {
                    // 出口里可能正放着别的页（归档这些）：打开会话视图，露出来再选中。
                    let opened = lifecycle::open_conversation(key.clone(), cx);
                    crate::report_failure("打开会话", opened, cx);
                }
            }
        })
        .when(draggable, |item| {
            let drag = SessionDrag {
                key: key.clone(),
                group: row.group.clone(),
                title: row.title.clone(),
            };
            let group = row.group.clone();
            item.on_drag(drag, {
                let view = view.clone();
                move |drag, _, window, cx| {
                    clear_drop(&view, cx);
                    preview(drag.title.clone(), window, cx)
                }
            })
            .on_drag_move::<SessionDrag>({
                let (view, key, group) = (view.clone(), key.clone(), group.clone());
                move |event, _, cx| {
                    if !event.bounds.contains(&event.event.position) {
                        return;
                    }
                    let dragged = event.drag(cx);
                    let hint = (dragged.group == group && dragged.key != key).then(|| DropHint {
                        target: DropTarget::Session(key.clone()),
                        before: event.event.position.y < event.bounds.center().y,
                    });
                    view.update(cx, |view, cx| view.set_drop(hint, cx));
                }
            })
            .on_drop::<SessionDrag>({
                let (view, key) = (view.clone(), key.clone());
                move |dragged, _, cx| {
                    let target = DropTarget::Session(key.clone());
                    let Some(before) = take_drop(&view, &target, cx) else {
                        return;
                    };
                    if dragged.group == group {
                        actions::drop_session(&dragged.key, &key, &group, before, cx);
                    }
                }
            })
        });

    let row = row.clone();
    MenuTrigger::context(eid(format!("session-menu:{id}")), item, move |_, _| {
        menus::session_menu(&row, view.clone())
    })
    .into_any_element()
}

/// 按状态、时间分组时标题后面补的来源：工作树的小标签，项目名和分支。
fn origin_labels(id: &str, origin: &RowOrigin, selected: bool) -> Vec<AnyElement> {
    let mut labels = Vec::new();
    if let Some((task, tooltip)) = origin.workset.clone() {
        labels.push(
            div()
                .id(eid(format!("session-workset:{id}")))
                .flex_shrink_0()
                .max_w(px(104.))
                .min_w_0()
                .flex()
                .items_center()
                .gap_1()
                .px_1()
                .rounded(px(3.))
                .bg(ui_theme::hair())
                .text_xs()
                .text_color(rgb(ui_theme::row_muted(selected)))
                .child(
                    svg()
                        .path(IconName::GitBranch.path())
                        .size(px(12.))
                        .flex_shrink_0()
                        .text_color(rgb(ui_theme::row_muted(selected))),
                )
                .child(div().min_w_0().child(truncated_line(task)))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .into_any_element(),
        );
    }
    labels.push(
        div()
            .flex_shrink_0()
            .max_w(px(160.))
            .min_w_0()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(rgb(ui_theme::row_muted(selected)))
            .child(
                div()
                    .min_w_0()
                    .child(truncated_line(origin.project.clone())),
            )
            .when_some(origin.branch.clone(), |label, branch| {
                let full = branch.clone();
                label.child(
                    div()
                        .id(eid(format!("session-branch:{id}")))
                        .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                        .min_w_0()
                        .text_color(rgb(ui_theme::row_faint(selected)))
                        .child(truncated_line(branch)),
                )
            })
            .into_any_element(),
    );
    labels
}

/// 行首的图标，按状态上色：空闲随行的次要字色，在跑、要人管的用状态色。
fn session_icon(id: &str, icon: &RowIcon, status: AgentStatus, selected: bool) -> AnyElement {
    let color = if status == AgentStatus::Idle {
        rgb(ui_theme::row_muted(selected))
    } else {
        ui_theme::session_dot_color(status)
    };
    let glyph = |path: SharedString| {
        svg()
            .path(path)
            .size(px(14.))
            .text_color(color)
            .into_any_element()
    };
    let inner = match icon {
        RowIcon::Agent { mark, name } => {
            let name = name.clone();
            div()
                .id(eid(format!("session-agent:{id}")))
                .size(px(LEAD))
                .rounded_full()
                .bg(rgb(ui_theme::soft_hover()))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(10.))
                .font_medium()
                .text_color(color)
                .child(mark.clone())
                .tooltip(move |window, cx| Tooltip::new(name.clone()).build(window, cx))
                .into_any_element()
        }
        RowIcon::Provider { path, label } => {
            let label = label.clone();
            div()
                .id(eid(format!("session-provider:{id}")))
                .child(glyph(path.clone()))
                .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                .into_any_element()
        }
        RowIcon::Terminal => glyph(IconName::SquareTerminal.path()),
        RowIcon::Conversation => glyph(IconName::MessageSquare.path()),
    };
    lead_box().child(inner).into_any_element()
}

/// 行尾平时写的字：用不了的机器写为什么，要人管的写「需要你」，在跑的写这一轮跑了多久，
/// 别的写最近一次动静在多久前。不为它定时重画，每次重画按当时算。
fn meta(row: &SessionRow, selected: bool, now: u64) -> (String, u32) {
    if let Some(label) = row.unavailable {
        return (label.to_string(), ui_theme::row_faint(selected));
    }
    match row.status {
        AgentStatus::NeedsYou => ("需要你".to_string(), ui_theme::need()),
        AgentStatus::Running if row.turn_started_at > 0 => (
            turn_text(row.turn_started_at, now),
            ui_theme::row_muted(selected),
        ),
        _ => (
            relative_time(row.activity, now),
            ui_theme::row_faint(selected),
        ),
    }
}

/// 行尾一格：平时写字，悬停这一行时同一格换成关闭键。两样叠着放，换的时候这一格不变宽，
/// 标题不跟着伸缩。按下不往行上传，免得点关闭先选中了会话。
fn trailing((text, color): (String, u32), close: Button) -> impl IntoElement {
    div()
        .relative()
        .flex_shrink_0()
        .min_w(px(20.))
        .h(px(20.))
        .flex()
        .items_center()
        .justify_end()
        .child(
            div()
                .text_xs()
                .text_color(rgb(color))
                .group_hover("session-row", |style| style.opacity(0.0))
                .child(text),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_center()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    close
                        .opacity(0.0)
                        .group_hover("session-row", |style| style.opacity(1.0)),
                ),
        )
}

// ---- 共用的小件 ----

/// 行首那一格。
fn lead_box() -> gpui::Div {
    div()
        .size(px(LEAD))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
}

/// 分组行开头那一格：平时是分组自己的图标，悬停整行时换成展开 / 收起箭头（收起朝右，
/// 展开朝下）。没东西可折的只有图标。两样叠在同一格里只换透明度，名字不跟着动。
fn disclosure(icon: IconName, collapsed: bool, collapsible: bool) -> gpui::Div {
    let icon = svg()
        .path(icon.path())
        .size(px(14.))
        .text_color(rgb(ui_theme::muted()));
    disclosure_slot("group-row", icon, collapsed, collapsible)
}

/// [`disclosure`] 的通用版：`icon` 是平时那个图标（机器行的是按连接上色的系统图标），悬停
/// `group` 那一行时换成箭头。
fn disclosure_slot(
    group: &'static str,
    icon: gpui::Svg,
    collapsed: bool,
    collapsible: bool,
) -> gpui::Div {
    let angle: f32 = if collapsed { 0. } else { 90. };
    lead_box()
        .relative()
        .text_color(rgb(ui_theme::muted()))
        .child(icon.when(collapsible, |icon| {
            icon.group_hover(group, |style| style.opacity(0.0))
        }))
        .when(collapsible, |lead| {
            lead.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .opacity(0.0)
                    .group_hover(group, |style| style.opacity(1.0))
                    .child(
                        svg()
                            .path(IconName::ChevronRight.path())
                            .size(px(14.))
                            .text_color(rgb(ui_theme::muted()))
                            .with_transformation(Transformation::rotate(radians(
                                angle.to_radians(),
                            ))),
                    ),
            )
        })
}

/// 项目行的行首：点它开合，不选中项目（点击不往行上传）。`fold` 为 `None`（没东西可折）
/// 时只是图标，点了跟点行一样。
fn fold_toggle(id: ElementId, lead: gpui::Div, fold: Option<(Fold, bool)>) -> impl IntoElement {
    let lead = lead.id(id);
    match fold {
        Some((fold, collapsed)) => lead.on_click(move |_, _, cx| {
            cx.stop_propagation();
            toggle_fold(fold.clone(), !collapsed, cx);
        }),
        None => lead,
    }
}

/// 折起来时标题行尾：有要人管、在跑的点一个状态点，再写下面有几段。
fn collapsed_summary(status: AgentStatus, count: usize, selected: bool) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap_1p5()
        .text_xs()
        .text_color(rgb(ui_theme::row_faint(selected)))
        .when(status != AgentStatus::Idle, |summary| {
            summary.child(
                div()
                    .size(px(6.))
                    .rounded_full()
                    .bg(ui_theme::session_dot_color(status)),
            )
        })
        .when(count > 0, |summary| summary.child(count.to_string()))
}

fn pin_marker() -> impl IntoElement {
    svg()
        .path(IconName::Pin.path())
        .size(px(11.))
        .flex_shrink_0()
        .text_color(rgb(ui_theme::faint()))
}

/// 只在悬停那一行（`group`）时露出来的按钮位。
fn hover_only(group: &'static str) -> gpui::Div {
    div()
        .flex_shrink_0()
        .opacity(0.0)
        .group_hover(group, |style| style.opacity(1.0))
}

/// 行内改名的输入框，占标题那一格。按下不往行上传，免得点进输入框就起拖拽或切会话。
/// Esc 由视图根上接住（输入框自己用不上 Esc 时往上传）。
fn rename_field(input: Entity<InputState>) -> AnyElement {
    div()
        .flex_1()
        .min_w_0()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(TextInput::new(&input).small())
        .into_any_element()
}

fn rename_input(ctx: &RowCtx, target: &RenameTarget) -> Option<Entity<InputState>> {
    ctx.rename
        .as_ref()
        .filter(|(renaming, _)| renaming == target)
        .map(|(_, input)| input.clone())
}

/// 拖着走时的落点：行的上沿或下沿一条线。
fn drop_line(before: bool) -> impl IntoElement {
    div()
        .absolute()
        .left_2()
        .right_2()
        .h(px(2.))
        .rounded_full()
        .bg(ui_theme::focus())
        .map(|line| {
            if before {
                line.top(px(-1.))
            } else {
                line.bottom(px(-1.))
            }
        })
}

fn toggle_fold(fold: Fold, collapsed: bool, cx: &mut App) {
    Sessions::global(cx).update(cx, |sessions, cx| {
        sessions.set_collapsed(fold, collapsed, cx)
    });
}

/// 开始拖：清掉上一次没放下就松手留下的落点。
fn clear_drop(view: &Entity<ListView>, cx: &mut App) {
    view.update(cx, |view, cx| view.set_drop(None, cx));
}

/// 放下时取走落点：只认落点就在放下的这一行上（`target`）。
fn take_drop(view: &Entity<ListView>, target: &DropTarget, cx: &mut App) -> Option<bool> {
    view.update(cx, |view, cx| {
        let before = view
            .drop
            .as_ref()
            .filter(|hint| hint.target == *target)
            .map(|hint| hint.before);
        view.set_drop(None, cx);
        before
    })
}

/// 拖着走的那块小牌子。
struct DragPreview(SharedString);

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .max_w(px(240.))
            .rounded(ui_theme::row_radius())
            .bg(rgb(ui_theme::surface()))
            .border_1()
            .border_color(ui_theme::hair_strong())
            .shadow_md()
            .text_sm()
            .text_color(rgb(ui_theme::text()))
            .child(truncated_line(self.0.clone()))
    }
}

fn preview(label: SharedString, _: &mut Window, cx: &mut App) -> Entity<DragPreview> {
    cx.new(|_| DragPreview(label))
}

fn machine_id(machine: &RuntimeKey) -> &str {
    match machine {
        RuntimeKey::Local => "@local",
        RuntimeKey::Remote(id) => id,
    }
}

fn eid(id: String) -> ElementId {
    ElementId::Name(id.into())
}

// ---- 时间 ----

/// 「N 分钟前」。`at` 比 `now` 还晚（远程机器的时钟比本机快）时写「刚刚」，不写成负数。
/// 不知道是什么时候（0）写「未知」。
fn relative_time(at: u64, now: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if at == 0 {
        return "未知".to_string();
    }
    let delta = now.saturating_sub(at);
    if delta < MINUTE {
        "刚刚".to_string()
    } else if delta < HOUR {
        format!("{} 分钟前", delta / MINUTE)
    } else if delta < DAY {
        format!("{} 小时前", delta / HOUR)
    } else if delta < 30 * DAY {
        format!("{} 天前", delta / DAY)
    } else if delta < 365 * DAY {
        format!("{} 个月前", delta / (30 * DAY))
    } else {
        format!("{} 年前", delta / (365 * DAY))
    }
}

/// 这一轮跑了多久，到分钟（列表不为它每秒重画）。
fn turn_text(started_at: u64, now: u64) -> String {
    let minutes = now.saturating_sub(started_at) / 60;
    match minutes {
        0 => "不到 1 分钟".to_string(),
        1..=59 => format!("{minutes} 分钟"),
        _ => format!("{} 小时 {} 分", minutes / 60, minutes % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_counts_back_from_now() {
        assert_eq!(relative_time(0, 100), "未知");
        assert_eq!(relative_time(100, 100), "刚刚");
        assert_eq!(relative_time(100, 130), "刚刚");
        assert_eq!(relative_time(100, 100 + 3 * 60), "3 分钟前");
        assert_eq!(relative_time(100, 100 + 5 * 3600), "5 小时前");
        assert_eq!(relative_time(100, 100 + 2 * 86400), "2 天前");
        assert_eq!(relative_time(100, 100 + 40 * 86400), "1 个月前");
        assert_eq!(relative_time(100, 100 + 400 * 86400), "1 年前");
        // 远程机器的时钟可能比本机快，不写成负数。
        assert_eq!(relative_time(100 + 3600, 100), "刚刚");
    }

    #[test]
    fn turn_text_counts_whole_minutes() {
        let now = 10_000;
        assert_eq!(turn_text(now - 59, now), "不到 1 分钟");
        assert_eq!(turn_text(now - 60, now), "1 分钟");
        assert_eq!(turn_text(now - (59 * 60 + 59), now), "59 分钟");
        assert_eq!(turn_text(now - 65 * 60, now), "1 小时 5 分");
        // 时钟回拨，起点比现在还晚：按刚开始算。
        assert_eq!(turn_text(now + 30, now), "不到 1 分钟");
    }
}
