//! 各种行怎么画：「对话」一段、「项目」标题、项目、工作树、会话，和还没收到目录的机器。
//! 只读 [`Sidebar`] 和视图上的临时状态（[`RowCtx`]），不读库、不读文件。
//!
//! 层级靠缩进（[`indent`]）：段标题（「对话」「项目」）下的第一级跟标题齐平，不缩：项目、
//! 按状态和时间分的组、「对话」里的会话、等目录的机器；项目和组里的会话、工作树缩一级，工作树
//! 里的会话再缩一级（行首是头像时会话少缩一级，见 [`session`]）。机器默认不占一层：远程项目和
//! 会话行上标机器名。按机器分开时机器的标题行占第一级，它那片里的都往里缩一级（[`area_level`]）。

use rust_i18n::t;
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyView, App, AppContext as _, Context, ElementId, Entity, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Pixels, Render, Rgba, SharedString,
    StatefulInteractiveElement as _, Styled as _, Transformation, Window, div, px, radians, rgb,
    svg,
};
use gpui_base::input::InputState;
use smelt_plugin_api::model::agent_status::AgentStatus;
use smelt_plugin_api::ui::IconName;
use smelt_plugin_api::ui::menu::{Menu, MenuTrigger};
use smelt_plugin_api::ui::motion::flip_slot;
use smelt_plugin_api::ui::tooltip::Tooltip;
use smelt_plugin_api::ui::truncated_line::truncated_line;
use smelt_plugin_api::ui::ui_theme::{self, height};
use smelt_plugin_api::ui::{
    Button, IconAction as _, ListRow, ScrollAnchors, StickySection, TextInput,
};
use smelt_plugin_api::{self as api, RuntimeKey};

use super::motion::{FoldMotion, Frame, arrival_slot, wash};
use super::view::{DropHint, DropTarget, ListView, RenameTarget};
use super::{
    Area, BucketGroup, CONVERSATIONS_KEY, ConversationsSection, GroupKey, MachineInfo,
    ProjectGroup, ProjectItem, RowIcon, RowOrigin, SessionRow, Sidebar, WorksetGroup, actions,
    bucket_flip, machine_flip, machine_id, menus, project_flip, project_visible, session_flip,
    workset_flip,
};
use crate::dialogs;
use crate::ops;
use crate::model::{
    ConversationKey, FailTarget, Failure, Fold, ProjectKey, Selection, SessionList, SidebarGrouping,
};
use crate::worksets::{MergeSummary, WorksetKey};
use smelt_plugin_api::ui::kit::{RADIUS, TYPE, TypeStyled as _, elevation};

/// 每缩一级多少。
const INDENT: f32 = 16.;
/// 行首离列表边的距离（第 0 级，跟段标题齐平）。
const EDGE: f32 = 8.;
/// 行首那一格（图标、展开箭头）的边长。
const LEAD: f32 = 16.;
/// 会话行首头像的边长，和放得下它的会话行高。没人填头像插槽时行首是图标，行高照常。
const AVATAR: f32 = 36.;
const AVATAR_ROW: f32 = 44.;
/// 头像右下角标明哪家 CLI、哪个智能体的小圆标，和里面图标、首字的大小。
const BADGE: f32 = 18.;
const BADGE_GLYPH: f32 = 13.;

/// 画一遍要的东西：算好的侧栏，加上视图上的临时状态。
pub(super) struct RowCtx {
    pub(super) sidebar: Rc<Sidebar>,
    pub(super) view: Entity<ListView>,
    pub(super) selection: Selection,
    /// 拖着时的落点；没在拖是 `None`。
    pub(super) drop: Option<DropHint>,
    /// 拖着时拿起的那一项（`Sidebar::flip_order` 里的身份）；没在拖是 `None`。
    pub(super) lifted: Option<String>,
    pub(super) rename: Option<(RenameTarget, Entity<InputState>)>,
    /// 「项目」标题换成的搜索框；没在搜是 `None`。
    pub(super) search: Option<Entity<InputState>>,
    /// 现在（unix 秒），行尾的相对时间按它算。
    pub(super) now: u64,
    /// 重排滑动、新会话落进列表这一帧的取样。
    pub(super) motion: Frame,
    /// 各段会话行首的头像（填进 [`crate::contract::SESSION_AVATAR`] 的视图实例）。没人填是
    /// `None`，行首画图标。
    pub(super) avatars: Option<Rc<HashMap<ConversationKey, AnyView>>>,
    /// 给会话行、组、「对话」标题挂上 key（滑动身份），记下它们画在哪：可视区外的提示、
    /// 选中后滚过去用。
    pub(super) anchors: ScrollAnchors,
    /// 哪些行上的操作刚失败了（[`SessionList::failures`]）。
    pub(super) failures: Rc<HashMap<FailTarget, Failure>>,
}

/// 整个列表从上到下：「对话」「项目」两段，标题吸顶（滚过了停在列表顶上，下一段来了被
/// 顶走）。只吸这一层，项目、机器、按状态分的组不吸，免得顶上叠好几层。
pub(super) fn sidebar(ctx: &RowCtx, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    let mut sections = Vec::new();
    if let Some(section) = &ctx.sidebar.conversations {
        sections.push(conversations(section, ctx, window, cx));
    }
    let mut rows = Vec::new();
    projects(ctx, window, cx, &mut rows);
    sections.push(StickySection::new(projects_header(ctx, cx), column(rows)).into_any_element());
    sections
}

/// 一段里标题下面的行，竖着排。
fn column(rows: Vec<AnyElement>) -> gpui::Div {
    div().flex().flex_col().children(rows)
}

/// 拖着时被拿起的那一项在原处的不透明度。
const LIFTED_OPACITY: f32 = 0.4;
/// 拖着走的牌子的不透明度：认得出是哪一行，又透得出底下的行和落点线。
const DRAG_OPACITY: f32 = 0.55;

/// 套一层重排时的滑动：先后变了，这一项从上一帧画的位置滑到新位置；是刚拖着放下的那一项，
/// 就从手放下的地方落下去。`key` 是 `Sidebar::flip_order` 里的身份。一组（项目、工作树、
/// 按状态分的组）连标题带子行套一层，整组一起走；组里的行各有一层，在组里换位置时用。
fn flip(ctx: &RowCtx, key: String, child: impl IntoElement) -> AnyElement {
    let landing = ctx
        .motion
        .landing
        .as_ref()
        .filter(|(landed, _)| *landed == key)
        .map(|(_, top)| *top);
    // 拿起来的那一项在原处淡下去：牌子跟着手走，原处留个影子标出它是从哪拿起的。
    let child = if ctx.lifted.as_ref() == Some(&key) {
        div()
            .opacity(LIFTED_OPACITY)
            .child(child)
            .into_any_element()
    } else {
        child.into_any_element()
    };
    flip_slot(eid(format!("flip:{key}")), ctx.motion.reorder, child)
        .from_top(landing)
        .into_any_element()
}

/// 一组的行收成一块，好整块滑。折起来时它就是标题那一行，可视区外的提示盯它（`key`）。
fn group_block(ctx: &RowCtx, key: &str, rows: Vec<AnyElement>) -> gpui::Div {
    ctx.anchors
        .anchor(key.to_string(), div().flex().flex_col().children(rows))
}

/// 「筛选与分组」按钮：分组依据和显示哪些项目。
fn grouping_button(sidebar: &Sidebar) -> AnyElement {
    let (grouping, hide_empty) = (sidebar.grouping, sidebar.hide_empty);
    let by_machine = sidebar.has_remote().then_some(sidebar.by_machine);
    MenuTrigger::dropdown(
        "session-grouping-menu",
        Button::new("session-grouping")
            .quiet()
            .small()
            .icon(IconName::SlidersHorizontal)
            .icon_action(t!("list.grouping_menu")),
        move |_, _| menus::grouping_menu(grouping, hide_empty, by_machine),
    )
    .anchor(gpui::Anchor::TopRight)
    .into_any_element()
}

// ---- 「对话」一段 ----

fn conversations(
    section: &ConversationsSection,
    ctx: &RowCtx,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let collapsed = section.collapsed;
    let fold = FoldMotion::new("conversations", collapsed, window, cx);
    let header = ListRow::new("sessions-conversations")
        .group("group-row")
        .h(height::sm())
        .mx_2()
        .mt_2()
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .type_style(TYPE.text_xsmall_medium)
        .child(disclosure(IconName::MessageSquare, fold.angle(), true))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(t!("kind.conversation_section").into_owned()),
        )
        .when(collapsed, |row| {
            row.child(collapsed_summary(section.status, section.rows.len(), false))
        })
        .on_click(move |_, _, cx| toggle_fold(Fold::Conversations, !collapsed, cx));
    let header = ctx.anchors.anchor(CONVERSATIONS_KEY, header);
    let mut rows = Vec::new();
    fold.body(&mut rows, window, cx, |rows, _, _| {
        rows.extend(section.rows.iter().map(|row| session(row, 0, ctx)));
    });
    StickySection::new(header, column(rows)).into_any_element()
}

// ---- 项目 ----

/// 「项目」标题下面那些行：项目（或按状态、时间分的组）和里面的会话。不按机器分开时所有
/// 机器的排在一起，最后是还没收到目录的机器，一台一行；分开时一台机器一片，见 [`machine_area`]。
fn projects(ctx: &RowCtx, window: &mut Window, cx: &mut App, rows: &mut Vec<AnyElement>) {
    let sidebar = &ctx.sidebar;
    if sidebar.by_machine {
        for area in &sidebar.areas {
            machine_area(area, ctx, window, cx, rows);
        }
        return;
    }
    // 一台机器的目录都还没收到时不知道有没有项目，只画下面那几行机器。
    let received = sidebar.machines.iter().any(|machine| machine.received);
    let area = sidebar.areas.first();
    if received && sidebar.projects.is_empty() {
        rows.push(empty_note(t!("list.no_projects"), 0));
    } else if sidebar.filtering && area.is_some_and(|area| area.projects.is_empty()) {
        rows.push(empty_note(t!("list.no_matching_projects"), 0));
    } else if let Some(area) = area {
        area_body(area, ctx, window, cx, rows);
    }
    for machine in sidebar.machines.iter().filter(|machine| !machine.received) {
        rows.push(waiting_machine_row(machine, cx));
    }
}

/// 一片里的项目或按状态、时间分的组。
fn area_body(
    area: &Area,
    ctx: &RowCtx,
    window: &mut Window,
    cx: &mut App,
    rows: &mut Vec<AnyElement>,
) {
    if ctx.sidebar.grouping == SidebarGrouping::Project {
        project_groups(area, ctx, window, cx, rows);
    } else {
        buckets(area, ctx, window, cx, rows);
    }
}

/// 按机器分开时一台机器那片：机器的标题行，展开时下面是它的项目或组。目录还没收到的机器
/// 只有标题行（写着在连、离线这些），没有可折的。
fn machine_area(
    area: &Area,
    ctx: &RowCtx,
    window: &mut Window,
    cx: &mut App,
    rows: &mut Vec<AnyElement>,
) {
    let Some(machine) = area
        .machine
        .as_ref()
        .and_then(|key| machine_info(&ctx.sidebar, key))
    else {
        return;
    };
    let key = machine_flip(&machine.key);
    let fold = FoldMotion::new(&key, area.collapsed, window, cx);
    let mut group = vec![machine_row(machine, area, fold.angle(), cx)];
    if machine.received {
        fold.body(&mut group, window, cx, |rows, window, cx| {
            if area.projects.is_empty() {
                let note = if ctx.sidebar.filtering {
                    t!("list.no_matching_projects")
                } else {
                    t!("list.no_projects")
                };
                rows.push(empty_note(note, 1));
            } else {
                area_body(area, ctx, window, cx, rows);
            }
        });
    }
    rows.push(flip(ctx, key.clone(), group_block(ctx, &key, group)));
}

fn buckets(
    area: &Area,
    ctx: &RowCtx,
    window: &mut Window,
    cx: &mut App,
    rows: &mut Vec<AnyElement>,
) {
    if area.buckets.is_empty() {
        rows.push(empty_note(t!("list.no_sessions"), area_level(&ctx.sidebar)));
    }
    for bucket in &area.buckets {
        let key = bucket_flip(&bucket.key);
        let fold = FoldMotion::new(&key, bucket.collapsed, window, cx);
        let level = area_level(&ctx.sidebar);
        let mut group = vec![bucket_row(bucket, fold.angle(), level)];
        fold.body(&mut group, window, cx, |rows, _, _| {
            rows.extend(bucket.rows.iter().map(|row| session(row, level + 1, ctx)));
        });
        rows.push(flip(ctx, key.clone(), group_block(ctx, &key, group)));
    }
}

fn project_groups(
    area: &Area,
    ctx: &RowCtx,
    window: &mut Window,
    cx: &mut App,
    rows: &mut Vec<AnyElement>,
) {
    let level = area_level(&ctx.sidebar);
    let mut hidden = 0;
    for (project_index, project) in ctx.sidebar.area_projects(area) {
        let selected = is_selected_project(ctx, project);
        let has_items = !project.items.is_empty();
        if !project_visible(ctx.sidebar.hide_empty, has_items, project.pinned, selected) {
            hidden += 1;
            continue;
        }
        let key = project_flip(&project.machine, &project.root);
        let fold = FoldMotion::new(&key, project.collapsed, window, cx);
        let mut group = vec![project_row(project_index, project, fold.angle(), ctx)];
        fold.body(&mut group, window, cx, |rows, window, cx| {
            for (item_index, item) in project.items.iter().enumerate() {
                match item {
                    ProjectItem::Session(row) => rows.push(session(row, level + 1, ctx)),
                    ProjectItem::Workset(workset) => {
                        let key = workset_flip(&workset.machine, &workset.id);
                        let fold = FoldMotion::new(&key, workset.collapsed, window, cx);
                        let at = (project_index, item_index);
                        let mut group = vec![workset_row(at, workset, fold.angle(), ctx)];
                        fold.body(&mut group, window, cx, |rows, _, _| {
                            let inner = workset.sessions.iter();
                            rows.extend(inner.map(|row| session(row, level + 2, ctx)));
                        });
                        rows.push(flip(ctx, key.clone(), group_block(ctx, &key, group)));
                    }
                }
            }
        });
        rows.push(flip(ctx, key.clone(), group_block(ctx, &key, group)));
    }
    if hidden > 0 {
        rows.push(hidden_empty_row(hidden, level));
    }
}

/// 「项目」标题：悬停露出「筛选与分组」「添加项目」和机器菜单（别的插件给机器登记的项，如
/// 重新连接、改名、远程桌面）；机器菜单也接右键。挂着远程机器时「添加项目」先选机器。
/// 标题那一格本身是搜索入口（[`search_entry`]），点了换成搜索框（[`ListView::start_search`]）。
fn projects_header(ctx: &RowCtx, cx: &App) -> AnyElement {
    let sidebar = &ctx.sidebar;
    let machines_menu = menus::has_machines_menu(sidebar, cx).then(|| {
        let sidebar = sidebar.clone();
        move |_: &mut Window, cx: &mut App| {
            menus::machines_menu(&sidebar, cx).unwrap_or_else(Menu::new)
        }
    });
    let open_project = if sidebar.has_remote() {
        let sidebar = sidebar.clone();
        MenuTrigger::dropdown(
            "open-project-menu",
            Button::new("open-project")
                .quiet()
                .small()
                .icon(IconName::FolderPlus)
                .icon_action(t!("list.add_project")),
            move |_, _| menus::open_project_menu(&sidebar),
        )
        .anchor(gpui::Anchor::TopRight)
        .into_any_element()
    } else {
        Button::new("open-project")
            .quiet()
            .small()
            .icon(IconName::FolderPlus)
            .icon_action(t!("list.add_project"))
            .on_click(|_, _, cx| actions::open_project(RuntimeKey::Local, cx))
            .into_any_element()
    };
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
        .type_style(TYPE.text_xsmall_medium)
        .text_color(rgb(ui_theme::muted()))
        .child(match &ctx.search {
            Some(input) => search_field(input.clone()),
            None => search_entry(ctx.view.clone()),
        })
        .child(
            hover_only("projects-header")
                .flex()
                .items_center()
                .child(grouping_button(sidebar))
                .child(open_project)
                .when_some(machines_menu.clone(), |slot, menu| {
                    slot.child(
                        MenuTrigger::dropdown(
                            "machines-menu",
                            Button::new("machines-more")
                                .quiet()
                                .small()
                                .icon(IconName::Ellipsis)
                                .icon_action(t!("list.machine")),
                            menu,
                        )
                        .anchor(gpui::Anchor::TopRight),
                    )
                }),
        );
    match machines_menu {
        Some(menu) => MenuTrigger::context("machines-context", header, menu).into_any_element(),
        None => header.into_any_element(),
    }
}

/// 按机器分开时机器的标题行，占「项目」下的第 0 级，样子跟「对话」标题一样：系统图标按连接上色（悬停那一行换成
/// 展开箭头），名字，用不了时写为什么，折起来时的摘要；悬停露出在这台机器上「添加项目」和
/// 这台机器的菜单，菜单也接右键。点一下开合；悬停名字看连接说明。
fn machine_row(machine: &MachineInfo, area: &Area, angle: f32, cx: &App) -> AnyElement {
    let key = machine.key.clone();
    let id = machine_id(&key).to_string();
    let collapsed = area.collapsed;
    let collapsible = machine.received;
    let detail: SharedString = machine.link.detail.clone().into();
    let remote = !matches!(key, RuntimeKey::Local);
    let state = match (machine.received, machine.link.unavailable.clone()) {
        (_, Some(why)) => Some(why),
        (false, None) => Some(SharedString::from(t!("machine.connecting"))),
        (true, None) => None,
    };
    let icon = svg()
        .path(machine.os_icon.clone())
        .size(px(14.))
        .text_color(rgb(machine.link.light.color()));
    let add_project = {
        let key = key.clone();
        Button::new(eid(format!("machine-open-project:{id}")))
            .quiet()
            .small()
            .icon(IconName::FolderPlus)
            .icon_action(t!("list.add_project"))
            .disabled(machine.link.unavailable.is_some())
            .on_click(move |_, _, cx| actions::open_project(key.clone(), cx))
    };
    let has_menu = menus::has_machine_menu(&key, cx);
    let actions = hover_only("machine-row")
        .flex()
        .items_center()
        // 按下不往行上传：点按钮不开合。
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(add_project)
        .when(has_menu, |slot| {
            let key = key.clone();
            slot.child(
                MenuTrigger::dropdown(
                    eid(format!("machine-menu:{id}")),
                    Button::new(eid(format!("machine-more:{id}")))
                        .quiet()
                        .small()
                        .icon(IconName::Ellipsis)
                        .icon_action(t!("list.machine")),
                    move |_, cx| menus::machine_menu(&key, cx).unwrap_or_else(Menu::new),
                )
                .anchor(gpui::Anchor::TopRight),
            )
        });
    let row = ListRow::new(eid(machine_flip(&key)))
        .group("machine-row")
        .h(height::sm())
        .mx_2()
        .mt_2()
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .type_style(TYPE.text_xsmall_medium)
        .child(disclosure_slot("machine-row", icon, angle, collapsible))
        .child(
            div()
                .id(eid(format!("machine-name:{id}")))
                .flex_1()
                .min_w_0()
                .child(truncated_line(machine.name.clone()))
                .tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx))
                .when(remote, |name| name.on_hover(refresh_connections)),
        )
        .when_some(state, |row, state| {
            row.child(
                div()
                    .flex_shrink_0()
                    .type_style(TYPE.text_xsmall)
                    .text_color(rgb(ui_theme::faint()))
                    .child(state),
            )
        })
        .when(collapsed, |row| {
            row.child(collapsed_summary(area.status, area.sessions, false))
        })
        .child(actions)
        .when(collapsible, |row| {
            let key = key.clone();
            row.on_click(move |_, _, cx| {
                SessionList::global(cx).update(cx, |sessions, cx| {
                    sessions.set_machine_collapsed(&key, !collapsed, cx)
                });
            })
        });
    if !has_menu {
        return row.into_any_element();
    }
    MenuTrigger::context(
        eid(format!("machine-row-context:{id}")),
        row,
        move |_, cx| menus::machine_menu(&key, cx).unwrap_or_else(Menu::new),
    )
    .into_any_element()
}

/// 还没收到目录的机器（刚启动在连、离线、版本不兼容）：它有哪些项目还不知道，留一行说一声。
/// 系统图标按连接上色，悬停看连接说明，右键是这台机器的菜单（重新连接等）。
fn waiting_machine_row(machine: &MachineInfo, cx: &App) -> AnyElement {
    let key = machine.key.clone();
    let id = machine_id(&key).to_string();
    let detail: SharedString = machine.link.detail.clone().into();
    let row = div()
        .id(eid(format!("machine-waiting:{id}")))
        .h(height::md())
        .mx_2()
        .pl(indent(0))
        .pr_2()
        .flex()
        .items_center()
        .gap_2()
        .type_style(TYPE.ui)
        .text_color(rgb(ui_theme::faint()))
        .child(
            lead_box().child(
                svg()
                    .path(machine.os_icon.clone())
                    .size(px(14.))
                    .text_color(rgb(machine.link.light.color())),
            ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(truncated_line(machine.name.clone())),
        )
        .child(
            div().flex_shrink_0().type_style(TYPE.text_xsmall).child(
                machine
                    .link
                    .unavailable
                    .clone()
                    .unwrap_or_else(|| t!("machine.connecting").into()),
            ),
        )
        .tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx))
        .when(!matches!(key, RuntimeKey::Local), |row| {
            row.on_hover(refresh_connections)
        });
    if !menus::has_machine_menu(&key, cx) {
        return row.into_any_element();
    }
    MenuTrigger::context(eid(format!("machine-context:{id}")), row, move |_, cx| {
        menus::machine_menu(&key, cx).unwrap_or_else(Menu::new)
    })
    .into_any_element()
}

/// 连接说明里的延迟只在有人看时采样：悬停远程机器的名字时叫宿主重测一次。
fn refresh_connections(hovered: &bool, _: &mut Window, cx: &mut App) {
    if *hovered {
        api::REFRESH_CONNECTIONS.run(&(), cx).detach();
    }
}

/// 第 `level` 级的行首离行左边多远。
fn indent(level: usize) -> Pixels {
    px(EDGE + INDENT * level as f32)
}

/// 「项目」下一片里第一级（项目、组）的层级：按机器分开时机器的标题行占了第 0 级，片里的
/// 往里缩一级。
fn area_level(sidebar: &Sidebar) -> usize {
    usize::from(sidebar.by_machine)
}

/// 按项目分组、「显示：有会话」时，藏起来的空项目有几个；点一下回到「显示：全部」。
fn hidden_empty_row(count: usize, level: usize) -> AnyElement {
    ListRow::new("hidden-empty")
        .h(height::sm())
        .mx_2()
        .pl(indent(level))
        .pr_2()
        .flex()
        .items_center()
        .type_style(TYPE.text_xsmall)
        .text_color(rgb(ui_theme::faint()))
        .child(SharedString::from(if count == 1 {
            t!("list.hidden_projects.one")
        } else {
            t!("list.hidden_projects.other", count = count)
        }))
        .on_click(|_, _, cx| {
            SessionList::global(cx).update(cx, |sessions, cx| {
                sessions.set_hide_empty_projects(false, cx)
            });
        })
        .into_any_element()
}

fn empty_note(text: Cow<'static, str>, level: usize) -> AnyElement {
    let text = SharedString::from(text);
    div()
        .mx_2()
        .px_2()
        .pl(indent(level))
        .h(height::sm())
        .flex()
        .items_center()
        .type_style(TYPE.text_xsmall)
        .text_color(rgb(ui_theme::faint()))
        .child(text)
        .into_any_element()
}

/// 行上标的机器：系统图标按连接上色，后面是灰字写的名字。不加底色，靠图标跟纯文字的分支名
/// 分开；连接的好坏只看图标的颜色，字不跟着变。
fn machine_tag(machine: Option<&MachineInfo>, text: SharedString, selected: bool) -> gpui::Div {
    div()
        .flex_shrink_0()
        .max_w(px(120.))
        .min_w_0()
        .flex()
        .items_center()
        .gap_1()
        .type_style(TYPE.text_xsmall)
        .text_color(rgb(ui_theme::row_faint(selected)))
        .when_some(machine, |tag, machine| {
            tag.child(
                svg()
                    .path(machine.os_icon.clone())
                    .flex_shrink_0()
                    .size(px(12.))
                    .text_color(rgb(machine.link.light.color())),
            )
        })
        .child(div().min_w_0().child(truncated_line(text)))
}

/// 机器表里的这台机器。
fn machine_info<'a>(sidebar: &'a Sidebar, key: &RuntimeKey) -> Option<&'a MachineInfo> {
    sidebar.machines.iter().find(|machine| machine.key == *key)
}

/// 项目行上的机器标签：远程项目写机器名，机器用不了时再写为什么（本机用不了只写为什么）。
/// 悬停看完整的连接说明。
fn project_machine_tag(
    id: &str,
    project: &ProjectGroup,
    sidebar: &Sidebar,
    selected: bool,
) -> Option<AnyElement> {
    let link = &project.link;
    let text = match (&project.machine_name, &link.unavailable) {
        (Some(name), Some(why)) => format!("{name} · {why}"),
        (Some(name), None) => name.to_string(),
        (None, Some(why)) => why.to_string(),
        (None, None) => return None,
    };
    let detail: SharedString = link.detail.clone().into();
    let remote = project.machine_name.is_some();
    Some(
        machine_tag(
            machine_info(sidebar, &project.machine),
            text.into(),
            selected,
        )
        .id(eid(format!("project-machine:{id}")))
        .tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx))
        .when(remote, |tag| tag.on_hover(refresh_connections))
        .into_any_element(),
    )
}

#[derive(Clone)]
pub(super) struct ProjectDrag {
    machine: RuntimeKey,
    root: String,
}

fn project_row(
    project_index: usize,
    project: &ProjectGroup,
    angle: f32,
    ctx: &RowCtx,
) -> AnyElement {
    let machine = project.machine.clone();
    let root = project.root.clone();
    let id = project_id(project);
    let selected = is_selected_project(ctx, project);
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
    };

    let row = ListRow::new(eid(format!("project:{id}")))
        .selected(selected)
        .group("group-row")
        .relative()
        .mx_2();
    let lead = fold_toggle(
        eid(format!("project-fold:{id}")),
        disclosure(
            folder_icon(collapsible && !project.collapsed),
            angle,
            collapsible,
        ),
        collapsible.then_some((fold, project.collapsed)),
    );
    let row = project_face(row, lead, project, &ctx.sidebar, selected)
        .children(failure_mark(
            ctx,
            &id,
            FailTarget::Project {
                machine: machine.clone(),
                root: root.clone(),
            },
        ))
        .when_some(hint, |row, before| row.child(drop_line(before)))
        // 点名字选中项目（主区显示项目页）；折叠只靠行首的箭头。
        .on_click({
            let (machine, root) = (machine.clone(), root.clone());
            move |_, _, cx| {
                if !root.is_empty() {
                    let selected = ops::select_project(machine.clone(), root.clone(), cx);
                    let target = FailTarget::Project {
                        machine: machine.clone(),
                        root: root.clone(),
                    };
                    crate::report_row_failure(target, t!("action.select_project"), selected, cx);
                }
            }
        })
        .on_drag(drag, {
            let (view, sidebar) = (view.clone(), ctx.sidebar.clone());
            move |drag, grab, _, cx| {
                start_drag(&view, grab.y, cx);
                let sidebar = sidebar.clone();
                let lifted = project_flip(&drag.machine, &drag.root);
                preview(&view, lifted, selected, cx, move |face| {
                    let project = &sidebar.projects[project_index];
                    let collapsible = !project.items.is_empty();
                    let open = collapsible && !project.collapsed;
                    let lead = disclosure(folder_icon(open), angle, collapsible);
                    project_face(face, lead, project, &sidebar, selected)
                })
            }
        })
        .on_drag_move::<ProjectDrag>({
            let (view, machine, root) = (view.clone(), machine.clone(), root.clone());
            let by_machine = ctx.sidebar.by_machine;
            move |event, _, cx| {
                if !event.bounds.contains(&event.event.position) {
                    return;
                }
                let dragged = event.drag(cx);
                let same = dragged.machine == machine && dragged.root == root;
                // 按机器分开时项目只在自己那台机器的片里换位置。
                let across = by_machine && dragged.machine != machine;
                let hint = (!same && !across).then(|| DropHint {
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
            move |dragged, window, cx| {
                let target = DropTarget::Project {
                    machine: machine.clone(),
                    root: root.clone(),
                };
                let Some(before) = take_drop(&view, &target, cx) else {
                    return;
                };
                let from = ProjectKey {
                    machine: dragged.machine.clone(),
                    root: dragged.root.clone(),
                };
                let to = ProjectKey {
                    machine: machine.clone(),
                    root: root.clone(),
                };
                drop_at(&view, project_flip(&from.machine, &from.root), window, cx);
                actions::drop_project(&from, &to, before, cx);
            }
        });

    let sidebar = ctx.sidebar.clone();
    MenuTrigger::context(eid(format!("project-menu:{id}")), row, move |_, cx| {
        menus::project_menu(&sidebar.projects[project_index], cx)
    })
    .into_any_element()
}

/// 项目行里各个小件 id 的前缀。
fn project_id(project: &ProjectGroup) -> String {
    format!("{}:{}", machine_id(&project.machine), project.root)
}

/// 项目行的样子，画在 `el` 上：`lead`（行首的图标和展开箭头）、名字和附属信息、折起来时的
/// 摘要、悬停才露出的「新建会话」。不带点击、拖拽、菜单，列表里那一行和拖着走的牌子都用它画。
fn project_face<E: gpui::Styled + gpui::ParentElement + IntoElement>(
    el: E,
    lead: impl IntoElement,
    project: &ProjectGroup,
    sidebar: &Sidebar,
    selected: bool,
) -> E {
    let id = project_id(project);
    let collapsible = !project.items.is_empty();
    el.h(height::md())
        .pl(indent(area_level(sidebar)))
        .pr_1()
        .flex()
        .items_center()
        .gap_2()
        .type_style(TYPE.ui)
        .child(lead)
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
                        .when(selected, |name| name.font_weight(TYPE.ui_medium.weight))
                        // 没有会话的项目淡一些，和有会话的一眼分开；连不上的更淡，在下一条。
                        .when(!collapsible, |name| {
                            name.text_color(rgb(ui_theme::row_muted(selected)))
                        })
                        .when(project.link.unavailable.is_some(), |name| {
                            name.text_color(rgb(ui_theme::row_faint(selected)))
                        })
                        .child(truncated_line(project.name.clone())),
                )
                .when(project.pinned, |name| name.child(pin_marker()))
                // 按机器分开时机器的标题行已经写了是哪台、连得怎样。
                .children(
                    (!sidebar.by_machine)
                        .then(|| project_machine_tag(&id, project, sidebar, selected))
                        .flatten(),
                )
                .when_some(project.branch.clone(), |name, branch| {
                    // 窄侧栏里分支名常被截掉后半段，悬停看全名。
                    let full = branch.clone();
                    name.child(
                        div()
                            .id(eid(format!("project-branch:{id}")))
                            .tooltip(move |window, cx| Tooltip::new(full.clone()).build(window, cx))
                            .flex_shrink_0()
                            .max_w(px(120.))
                            .type_style(TYPE.text_xsmall)
                            .text_color(rgb(ui_theme::row_faint(selected)))
                            .child(truncated_line(branch)),
                    )
                })
                .when(project.repositories > 1, |name| {
                    name.child(
                        div()
                            .flex_shrink_0()
                            .type_style(TYPE.text_xsmall)
                            .text_color(rgb(ui_theme::row_faint(selected)))
                            .child(SharedString::from(if project.repositories == 1 {
            t!("project.repositories.one")
        } else {
            t!("project.repositories.other", count = project.repositories)
        })),
                    )
                }),
        )
        .when(project.collapsed, |row| {
            row.child(collapsed_summary(project.status, project.sessions, selected))
        })
        // 平时透明但占着位置；牌子上也画，名字和摘要才和列表里那一行对得齐。
        .when(!project.root.is_empty(), |row| {
            row.child(
                dialogs::NewSessionButton::new(
                    format!("project-new:{id}"),
                    t!("menu.new_session"),
                    project.machine.clone(),
                    project.root.clone(),
                )
                .revealed_on_hover("group-row"),
            )
        })
}

fn is_selected_project(ctx: &RowCtx, project: &ProjectGroup) -> bool {
    ctx.selection
        .project()
        .is_some_and(|(machine, root)| *machine == project.machine && root == project.root)
}

// ---- 按状态、时间分的组 ----

/// 组的标题行：点一下开合。组是现算的，没有菜单、不能拖、不能在组里新建会话。
fn bucket_row(group: &BucketGroup, angle: f32, level: usize) -> AnyElement {
    let (bucket, collapsed) = (group.key.clone(), group.collapsed);
    ListRow::new(eid(bucket_flip(&bucket)))
        .group("group-row")
        .h(height::md())
        .mx_2()
        .pl(indent(level))
        .pr_1()
        .flex()
        .items_center()
        .gap_2()
        .type_style(TYPE.ui)
        .child(disclosure(folder_icon(!collapsed), angle, true))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(truncated_line(bucket.bucket.label())),
        )
        .when(collapsed, |row| {
            row.child(collapsed_summary(group.status, group.rows.len(), false))
        })
        .on_click(move |_, _, cx| {
            SessionList::global(cx).update(cx, |sessions, cx| {
                sessions.set_bucket_collapsed(bucket.clone(), !collapsed, cx)
            });
        })
        .into_any_element()
}

// ---- 工作树 ----

fn workset_row(
    (project_index, item_index): (usize, usize),
    workset: &WorksetGroup,
    angle: f32,
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
        .pl(indent(area_level(&ctx.sidebar) + 1))
        .pr_1()
        .flex()
        .items_center()
        .gap_2()
        .type_style(TYPE.ui)
        .child(disclosure(IconName::GitBranch, angle, collapsible))
        .child(title)
        .when(collapsed, |row| {
            row.child(collapsed_summary(workset.status, workset.sessions.len(), false))
        })
        // 分支都合进基线了：一直露着一个「关闭工作树」，合完 PR 回来点一下就收。关之前照样
        // 再查一次、确认一次。
        .when(workset.merge == MergeSummary::Merged, |row| {
            let (machine, id, task) = (machine.clone(), workset_id.clone(), workset.task.clone());
            row.child(
                Button::new(eid(format!("workset-merged:{id}")))
                    .quiet()
                    .small()
                    .icon(IconName::CircleCheck)
                    .icon_action(t!("list.finish_merged"))
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        actions::finish_workset(
                            machine.clone(),
                            id.clone(),
                            task.clone(),
                            window,
                            cx,
                        )
                    }),
            )
        })
        .child(
            dialogs::NewSessionButton::new(
                format!("workset-new:{id}"),
                t!("list.new_in_workset"),
                machine.clone(),
                workset.path.clone(),
            )
            .revealed_on_hover("group-row"),
        )
        .children(failure_mark(
            ctx,
            &id,
            FailTarget::Workset(WorksetKey {
                machine: machine.clone(),
                id: workset_id.clone(),
            }),
        ))
        // 工作树没有自己的页：点一下就是开合（改名走菜单，免得双击先开合一次）。
        .on_click(move |_, _, cx| {
            if collapsible {
                toggle_fold(fold.clone(), !collapsed, cx);
            }
        });

    let sidebar = ctx.sidebar.clone();
    MenuTrigger::context(
        eid(format!("workset-menu:{id}")),
        row,
        move |_, _| match &sidebar.projects[project_index].items[item_index] {
            ProjectItem::Workset(workset) => menus::workset_menu(workset, view.clone()),
            ProjectItem::Session(_) => Menu::new(),
        },
    )
    .into_any_element()
}

// ---- 会话 ----

#[derive(Clone)]
pub(super) struct SessionDrag {
    key: ConversationKey,
    group: GroupKey,
}

/// 会话行首放什么。
#[derive(Clone)]
enum SessionLead {
    /// 有人填头像插槽：这段会话的头像实例（还没建好是 `None`，空着）。
    Avatar(Option<AnyView>),
    /// 没人填：行首画图标。
    Icon,
}

impl SessionLead {
    /// 头像比图标高，行跟着高。
    fn row_height(&self) -> Pixels {
        match self {
            Self::Avatar(_) => px(AVATAR_ROW),
            Self::Icon => height::md(),
        }
    }
}

/// 会话行画成什么样要的状态（数据在 [`SessionRow`] 里）。拖拽牌子在开始拖时拷一份。
#[derive(Clone)]
struct SessionLook {
    lead: SessionLead,
    depth: usize,
    selected: bool,
    /// 刚开始跑时行首亮到哪（[`status_color`]）。
    glow: Option<f32>,
    /// 现在（unix 秒），行尾的相对时间按它算。
    now: u64,
}

/// `depth` 是这一行的层级（行首是图标时就缩这么多级）。行首是头像时少缩一级，头像左边对齐
/// 上一级的行首图标：头像比行首图标宽，标题自然比上一级的名字往右错开一截，看着就是缩了
/// 一级；再多缩一级会把窄侧栏里的标题挤没。段标题下的第一级（「对话」里的会话）本来就不缩。
fn session(row: &SessionRow, depth: usize, ctx: &RowCtx) -> AnyElement {
    let key = row.key.clone();
    let id = session_id(&key);
    let selected = ctx.selection.session() == Some(&key);
    // 只有按项目分组时顺序是手动的；按状态、时间分组的顺序是算出来的，拖了也对不上。
    let draggable = ctx.sidebar.grouping == SidebarGrouping::Project
        && matches!(row.group, GroupKey::Project { .. });
    let target = RenameTarget::Conversation(key.clone());
    let renaming = rename_input(ctx, &target).map(rename_field);
    let hint = ctx.drop.as_ref().and_then(|hint| match &hint.target {
        DropTarget::Session(target) if *target == key => Some(hint.before),
        _ => None,
    });
    let view = ctx.view.clone();
    let arrival = ctx
        .motion
        .arrival
        .as_ref()
        .filter(|arrival| arrival.key == key);
    let look = SessionLook {
        lead: match &ctx.avatars {
            Some(avatars) => SessionLead::Avatar(avatars.get(&key).cloned()),
            None => SessionLead::Icon,
        },
        depth,
        selected,
        glow: ctx.motion.glows.get(&key).copied(),
        now: ctx.now,
    };
    let row_height = look.lead.row_height();
    // 落进来的、点标题找出来的，都在行底下铺一层底色。
    let wash_alpha = arrival.map(|arrival| arrival.wash).or_else(|| {
        ctx.motion
            .located
            .as_ref()
            .filter(|(located, _)| *located == key)
            .map(|(_, alpha)| *alpha)
    });
    let close = {
        let (key, title, status) = (key.clone(), row.title.clone(), row.status);
        Button::new(eid(format!("session-close:{id}")))
            .dismiss()
            .small()
            .icon(IconName::Close)
            .icon_action(t!("close.confirm"))
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                actions::close_session(key.clone(), title.clone(), status, window, cx)
            })
    };
    let flip_key = session_flip(&key);
    let item = ListRow::new(eid(format!("session:{id}")))
        .selected(selected)
        .group("session-row")
        .relative()
        .mx_2()
        .when_some(wash_alpha, |item, alpha| item.child(wash(alpha)));
    let item = session_face(item, row, &look, &ctx.sidebar, renaming, Some(close))
        .children(failure_mark(
            ctx,
            &id,
            FailTarget::Conversation(key.clone()),
        ))
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
                    let opened = ops::open_conversation(key.clone(), cx);
                    let target = FailTarget::Conversation(key.clone());
                    crate::report_row_failure(target, t!("action.open_session"), opened, cx);
                }
            }
        })
        .when(draggable, |item| {
            let drag = SessionDrag {
                key: key.clone(),
                group: row.group.clone(),
            };
            let group = row.group.clone();
            item.on_drag(drag, {
                let (view, row, look) = (view.clone(), row.clone(), look.clone());
                let sidebar = ctx.sidebar.clone();
                move |drag, grab, _, cx| {
                    start_drag(&view, grab.y, cx);
                    let (row, look, sidebar) = (row.clone(), look.clone(), sidebar.clone());
                    let lifted = session_flip(&drag.key);
                    preview(&view, lifted, look.selected, cx, move |face| {
                        session_face(face, &row, &look, &sidebar, None, None)
                    })
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
                move |dragged, window, cx| {
                    let target = DropTarget::Session(key.clone());
                    let Some(before) = take_drop(&view, &target, cx) else {
                        return;
                    };
                    if dragged.group == group {
                        drop_at(&view, session_flip(&dragged.key), window, cx);
                        actions::drop_session(&dragged.key, &key, &group, before, cx);
                    }
                }
            })
        });
    let item = ctx.anchors.anchor(flip_key.clone(), item);

    let row = row.clone();
    let item = MenuTrigger::context(eid(format!("session-menu:{id}")), item, move |_, cx| {
        menus::session_menu(&row, view.clone(), cx)
    })
    .into_any_element();
    flip(ctx, flip_key, arrival_slot(arrival, row_height, item))
}

/// 会话行里各个小件 id 的前缀。
fn session_id(key: &ConversationKey) -> String {
    format!("{}:{}", machine_id(&key.machine), key.id)
}

/// 会话行的样子，画在 `el` 上：行首、标题、行尾，不带点击、拖拽、菜单。列表里那一行和拖着
/// 走的牌子都用它画，拖起来的牌子就和那一行一个样。`renaming` 是正在改名时顶替标题的输入框，
/// `close` 是悬停时换到行尾的关闭键；牌子上两样都没有。
fn session_face<E: gpui::Styled + gpui::ParentElement + IntoElement>(
    el: E,
    row: &SessionRow,
    look: &SessionLook,
    sidebar: &Sidebar,
    renaming: Option<AnyElement>,
    close: Option<Button>,
) -> E {
    let key = &row.key;
    let id = session_id(key);
    let selected = look.selected;
    // 有头像时少缩一级（见 [`session`]）。
    let (lead, depth) = match &look.lead {
        SessionLead::Avatar(figure) => (
            session_avatar(
                &id,
                figure.clone(),
                &row.icon,
                row.status,
                selected,
                look.glow,
            ),
            look.depth.saturating_sub(1),
        ),
        SessionLead::Icon => (
            session_icon(&id, &row.icon, row.status, selected, look.glow),
            look.depth,
        ),
    };

    // 标题亮不亮看要不要你看：需要你处理的、有没看过的提醒的亮且加重（未读另在行尾带个
    // 点），在跑的、闲着的淡下去（在跑看行首和行尾）。选中的那行照常亮。
    let tally = row.tally();
    let quiet = !selected && tally.is_empty();
    let title = match renaming {
        Some(input) => input,
        None => div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1p5()
            .child(
                div()
                    .min_w_0()
                    .when(selected || !tally.is_empty(), |title| {
                        title.font_weight(TYPE.ui_medium.weight)
                    })
                    .when(quiet, |title| title.text_color(rgb(ui_theme::muted())))
                    .child(truncated_line(row.title.clone())),
            )
            .when(row.pinned, |title| title.child(pin_marker()))
            .when_some(row.machine_name.clone(), |title, name| {
                title.child(machine_tag(
                    machine_info(sidebar, &key.machine),
                    name,
                    selected,
                ))
            })
            .when_some(row.origin.as_ref(), |title, origin| {
                title.children(origin_labels(&id, origin, selected))
            })
            .into_any_element(),
    };
    el.h(look.lead.row_height())
        .pl(indent(depth))
        .pr_1()
        .flex()
        .items_center()
        .gap_2()
        .type_style(TYPE.ui)
        .child(lead)
        .child(title)
        .child(trailing(meta(row, selected, look.now), close))
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
                .type_style(TYPE.text_xsmall)
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
            .type_style(TYPE.text_xsmall)
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

/// 行首图标、头像小圆标的颜色：空闲随行的次要字色，在跑、要人管的用状态色。`glow` 是刚开始
/// 跑时亮一下的进度：只提亮度不淡出，还要认得出是哪家。
fn status_color(status: AgentStatus, selected: bool, glow: Option<f32>) -> Rgba {
    match (status, glow) {
        (AgentStatus::Idle, _) => rgb(ui_theme::row_muted(selected)),
        (AgentStatus::Running, Some(phase)) => ui_theme::running_glow_color(phase),
        _ => ui_theme::session_dot_color(status),
    }
}

/// 行首的头像：`figure` 是填进插槽的视图给这段会话的实例（还没建好是 `None`，空着）。右下角的
/// 小标标明是哪家 CLI（智能体对话是名字的第一个字）。
///
/// 小标垫一块实心圆底，把它从头像上隔开：直接压在彩色像素上，细线条图标和小人的轮廓搅在
/// 一起认不出。底不取行底色：行底是叠在窗口材质上的半透明色，选中、悬停各不一样，实心色
/// 对不上；用浮起面的颜色，各种行底上都是同一枚小牌子。图标一律用正文色，它要回答的是
/// 「哪家」，状态色（尤其闲着的灰、某些主题偏暗的在跑色）压在圆底上对比度不够；状态改由
/// 圆圈的颜色说（[`status_color`]），闲着只描一圈细边。
fn session_avatar(
    id: &str,
    figure: Option<AnyView>,
    icon: &RowIcon,
    status: AgentStatus,
    selected: bool,
    glow: Option<f32>,
) -> AnyElement {
    let color = rgb(ui_theme::text());
    let glyph = |path: SharedString| {
        svg()
            .path(path)
            .size(px(BADGE_GLYPH))
            .text_color(color)
            .into_any_element()
    };
    let (mark, tip): (AnyElement, Option<SharedString>) = match icon {
        RowIcon::Agent { mark, name } => (
            div()
                .text_size(px(BADGE_GLYPH))
                .line_height(px(BADGE))
                .font_weight(TYPE.ui_medium.weight)
                .text_color(color)
                .child(mark.clone())
                .into_any_element(),
            Some(name.clone()),
        ),
        RowIcon::Provider { path, label } => (glyph(path.clone()), Some(label.clone())),
        RowIcon::Terminal => (glyph(IconName::Terminal.path()), None),
        RowIcon::Conversation => (glyph(IconName::MessageSquare.path()), None),
    };
    let badge = div()
        .absolute()
        .right(px(-3.))
        .bottom(px(-2.))
        .size(px(BADGE))
        .rounded_full()
        .bg(rgb(ui_theme::surface()))
        .map(|badge| match status {
            AgentStatus::Idle => badge.border_1().border_color(ui_theme::hair()),
            _ => badge
                .border(px(1.5))
                .border_color(status_color(status, selected, glow)),
        })
        .flex()
        .items_center()
        .justify_center()
        .child(mark);
    div()
        .id(eid(format!("session-avatar:{id}")))
        .relative()
        .size(px(AVATAR))
        .flex_shrink_0()
        .children(figure)
        .child(badge)
        .when_some(tip, |avatar, tip| {
            avatar.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        })
        .into_any_element()
}

/// 没人填头像插槽时行首的图标，按状态上色（[`status_color`]）。
fn session_icon(
    id: &str,
    icon: &RowIcon,
    status: AgentStatus,
    selected: bool,
    glow: Option<f32>,
) -> AnyElement {
    let color = status_color(status, selected, glow);
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
                .type_style(TYPE.small_medium)
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
        RowIcon::Terminal => glyph(IconName::Terminal.path()),
        RowIcon::Conversation => glyph(IconName::MessageSquare.path()),
    };
    lead_box().child(inner).into_any_element()
}

/// 行尾那一格平时写的字。
struct Meta {
    text: String,
    color: u32,
    /// 字前面带未读点：有没看过的提醒（跑完了、失败了你还没看）。只亮标题和闲着的行分不
    /// 开，点在行尾和时间挨着，扫一眼列表就看得出哪几行。
    unread: bool,
}

/// 行尾的字，状态先说话：用不了的机器写为什么，要人管的写「需要你」，在跑的写这一轮跑了
/// 多久（用在跑的颜色，和闲着的分开），别的写最近一次动静在多久前。不为它定时重画，每次
/// 重画按当时算。
fn meta(row: &SessionRow, selected: bool, now: u64) -> Meta {
    let unread = row.tally().unread > 0;
    let plain = |text: String, color: u32| Meta {
        text,
        color,
        unread,
    };
    if let Some(label) = &row.unavailable {
        return plain(label.to_string(), ui_theme::row_faint(selected));
    }
    match row.status {
        AgentStatus::NeedsYou => plain(t!("bucket.needs_you").into_owned(), ui_theme::need()),
        AgentStatus::Running if row.turn_started_at > 0 => Meta {
            text: span(now.saturating_sub(row.turn_started_at)),
            color: ui_theme::run(),
            unread,
        },
        _ => plain(
            relative_time(row.activity, now),
            ui_theme::row_faint(selected),
        ),
    }
}

/// 行尾一格：平时写字，悬停这一行时同一格换成关闭键（`close`，拖拽牌子上没有）。两样叠着
/// 放，换的时候这一格不变宽，标题不跟着伸缩。按下不往行上传，免得点关闭先选中了会话。
fn trailing(meta: Meta, close: Option<Button>) -> impl IntoElement {
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
                .flex()
                .items_center()
                .gap_1()
                .type_style(TYPE.text_xsmall)
                .text_color(rgb(meta.color))
                .group_hover("session-row", |style| style.opacity(0.0))
                .when(meta.unread, |label| {
                    label.child(
                        div()
                            .size(px(6.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(rgb(ui_theme::link())),
                    )
                })
                .child(meta.text),
        )
        .when_some(close, |cell, close| {
            cell.child(
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
        })
}

// ---- 共用的小件 ----

/// 这一行上刚失败的操作：行尾一个 `bad` 色的叹号，悬停看为什么，点一下收起（同一行再做
/// 一次也会收起）。没失败是 `None`。按下不往行上传，免得收起时先选中、打开了这一行。
fn failure_mark(ctx: &RowCtx, id: &str, target: FailTarget) -> Option<AnyElement> {
    let failure = ctx.failures.get(&target)?;
    Some(
        div()
            .flex_shrink_0()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new(eid(format!("failure:{id}")))
                    .danger_quiet()
                    .small()
                    .icon(IconName::CircleAlert)
                    .icon_action(failure.message())
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        let target = target.clone();
                        SessionList::global(cx)
                            .update(cx, |sessions, cx| sessions.set_failure(target, None, cx));
                    }),
            )
            .into_any_element(),
    )
}

/// 行首那一格。
fn lead_box() -> gpui::Div {
    div()
        .size(px(LEAD))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
}

/// 项目行、分组行平时的图标：展开是打开的文件夹，收起或没东西可折是合上的。箭头只在悬停时
/// 露出来，不悬停也得看得出哪组开着。
fn folder_icon(open: bool) -> IconName {
    if open {
        IconName::FolderOpen
    } else {
        IconName::Folder
    }
}

/// 分组行开头那一格：平时是分组自己的图标，悬停整行时换成展开 / 收起箭头（收起朝右，
/// 展开朝下）。没东西可折的只有图标。两样叠在同一格里只换透明度，名字不跟着动。`angle` 是
/// 箭头转到的角度（[`FoldMotion::angle`]），开合时跟着转。
fn disclosure(icon: IconName, angle: f32, collapsible: bool) -> gpui::Div {
    let icon = svg()
        .path(icon.path())
        .size(px(14.))
        .text_color(rgb(ui_theme::muted()));
    disclosure_slot("group-row", icon, angle, collapsible)
}

/// [`disclosure`] 的通用版：`icon` 是平时那个图标（机器行的是按连接上色的系统图标），悬停
/// `group` 那一行时换成箭头。
fn disclosure_slot(
    group: &'static str,
    icon: gpui::Svg,
    angle: f32,
    collapsible: bool,
) -> gpui::Div {
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
        .type_style(TYPE.text_xsmall)
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

/// 输入框文字离框边的距离（`TextInput` 的左内边距）。入口和搜索框都往左探出这么多，框里的
/// 字正好落在标题「项目」原来的位置，点开前后字不跳。
const SEARCH_INSET: f32 = 8.;

/// 「项目」标题那一格兼当搜索入口：平时只是标题；悬停显出输入框的样子（底色、细边、末尾
/// 放大镜），点一下换成真的搜索框。不另放放大镜按钮：入口就在标题上，少一个要找的图标。
fn search_entry(view: Entity<ListView>) -> AnyElement {
    div()
        .id("search-projects")
        .group("search-projects")
        .flex_1()
        .min_w_0()
        .h_full()
        .ml(px(-SEARCH_INSET))
        .px(px(SEARCH_INSET))
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(RADIUS.md))
        .cursor_text()
        .hover(|entry| entry.bg(rgb(ui_theme::bg())).shadow(elevation::input()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(t!("grouping.project").into_owned()),
        )
        .child(
            svg()
                .path(IconName::Search.path())
                .size(px(12.))
                .flex_shrink_0()
                .text_color(rgb(ui_theme::muted()))
                .opacity(0.0)
                .group_hover("search-projects", |icon| icon.opacity(1.0)),
        )
        .tooltip(|window, cx| Tooltip::new(t!("list.search_projects")).build(window, cx))
        .on_click(move |_, window, cx| view.update(cx, |view, cx| view.start_search(window, cx)))
        .into_any_element()
}

/// 「项目」标题上的搜索框，占标题那一格。按下不往标题上传，免得点进框里就弹机器菜单之类。
/// 字号跟标题一样（输入框默认的 13px 比标题大一号，点开字就跳），只换成常规字重。
fn search_field(input: Entity<InputState>) -> AnyElement {
    div()
        .flex_1()
        .min_w_0()
        .ml(px(-SEARCH_INSET))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            TextInput::new(&input)
                .small()
                .text_size(px(TYPE.text_xsmall.size)),
        )
        .into_any_element()
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
    SessionList::global(cx).update(cx, |sessions, cx| {
        sessions.set_collapsed(fold, collapsed, cx)
    });
}

/// 开始拖：清掉上一次没放下就松手留下的落点，记下指针按在行上多高的地方（`grab`）。
fn start_drag(view: &Entity<ListView>, grab: Pixels, cx: &mut App) {
    view.update(cx, |view, cx| {
        view.motion.start_drag(grab);
        view.set_drop(None, cx);
    });
}

/// 放下了：被拖的那一项（`Sidebar::flip_order` 里的身份）从拖拽牌子这时的高度落到新位置。
fn drop_at(view: &Entity<ListView>, key: String, window: &Window, cx: &mut App) {
    let pointer = window.mouse_position().y;
    view.update(cx, |view, _| view.motion.drop_at(key, pointer));
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

/// 拖着走的牌子：被拖的那一行浮起来。`face` 就是画列表里那一行的函数（[`session_face`]、
/// [`project_face`]），用开始拖时拷下的数据画，所以和那一行一个样；拖着时数据变了不跟。
///
/// 宽度要给死：GPUI 按最小内容宽度排拖拽牌子，能截短的单行字最窄是 0，不给宽度标题就只剩
/// 一个省略号。底先铺实色，免得行底那层半透明叠出两层字；整块再按 [`DRAG_OPACITY`] 淡下去，
/// 透出底下的行和落点线，看得清要放到哪。
struct DragPreview {
    width: Pixels,
    selected: bool,
    face: Box<dyn Fn(gpui::Div) -> gpui::Div>,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let face = div()
            .rounded(ui_theme::row_radius())
            .when(self.selected, |face| face.bg(ui_theme::row_selected()));
        div()
            .w(self.width)
            .rounded(ui_theme::row_radius())
            .bg(rgb(ui_theme::surface()))
            .shadow_lg()
            .opacity(DRAG_OPACITY)
            .text_color(rgb(ui_theme::text()))
            .child((self.face)(face))
    }
}

/// 拿起 `lifted` 那一项（`Sidebar::flip_order` 里的身份），给它做牌子。原处淡到牌子放掉为止：
/// 松手、放下、拖到窗口外取消，GPUI 都会扔掉牌子，那时清掉，免得之后拖别的东西（比如标签页）
/// 时这一项又淡下去。
fn preview(
    view: &Entity<ListView>,
    lifted: String,
    selected: bool,
    cx: &mut App,
    face: impl Fn(gpui::Div) -> gpui::Div + 'static,
) -> Entity<DragPreview> {
    let width = view.read(cx).row_width();
    let preview = cx.new(|_| DragPreview {
        width,
        selected,
        face: Box::new(face),
    });
    view.update(cx, |view, cx| {
        view.lifted = Some(lifted.clone());
        cx.observe_release(&preview, move |view, _, cx| {
            if view.lifted.as_ref() == Some(&lifted) {
                view.lifted = None;
                cx.notify();
            }
        })
        .detach();
    });
    preview
}

fn eid(id: String) -> ElementId {
    ElementId::Name(id.into())
}

// ---- 时间 ----

/// 一段时长：只到最大的那一级（「6 分钟」「1 小时」「3 天」），向下取整，不到 1 分钟写
/// 「刚刚」。在跑的「跑了多久」和闲着的「多久之前」共用这一套，两样只差前面的计时图标、
/// 颜色和后面的「前」字。到分钟为止，列表不为它每秒重画。
fn span(seconds: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if seconds < MINUTE {
        t!("span.now")
    } else if seconds < HOUR {
        t!("span.minutes", count = seconds / MINUTE)
    } else if seconds < DAY {
        t!("span.hours", count = seconds / HOUR)
    } else if seconds < 30 * DAY {
        t!("span.days", count = seconds / DAY)
    } else if seconds < 365 * DAY {
        t!("span.months", count = seconds / (30 * DAY))
    } else {
        t!("span.years", count = seconds / (365 * DAY))
    }
    .into_owned()
}

/// 「N 分钟前」。`at` 比 `now` 还晚（远程机器的时钟比本机快）时写「刚刚」，不写成负数。
/// 不知道是什么时候（0）写「未知」。
fn relative_time(at: u64, now: u64) -> String {
    if at == 0 {
        return t!("span.unknown").into_owned();
    }
    match now.saturating_sub(at) {
        0..60 => span(0),
        delta => t!("span.ago", span = span(delta)).into_owned(),
    }
}

#[cfg(all(test, target_os = "macos"))]
mod snapshots;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_counts_back_from_now() {
        assert_eq!(relative_time(0, 100), "Unknown");
        assert_eq!(relative_time(100, 100), "now");
        assert_eq!(relative_time(100, 130), "now");
        assert_eq!(relative_time(100, 100 + 3 * 60), "3m ago");
        assert_eq!(relative_time(100, 100 + 5 * 3600), "5h ago");
        assert_eq!(relative_time(100, 100 + 2 * 86400), "2d ago");
        assert_eq!(relative_time(100, 100 + 40 * 86400), "1mo ago");
        assert_eq!(relative_time(100, 100 + 400 * 86400), "1y ago");
        // 远程机器的时钟可能比本机快，不写成负数。
        assert_eq!(relative_time(100 + 3600, 100), "now");
    }

    #[test]
    fn span_keeps_the_largest_unit() {
        assert_eq!(span(0), "now");
        assert_eq!(span(59), "now");
        assert_eq!(span(60), "1m");
        assert_eq!(span(6 * 60 + 59), "6m");
        assert_eq!(span(3600 - 1), "59m");
        assert_eq!(span(3600), "1h");
        assert_eq!(span(3600 + 50 * 60), "1h");
        assert_eq!(span(3 * 86400), "3d");
    }
}
