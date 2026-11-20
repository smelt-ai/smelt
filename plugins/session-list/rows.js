// 会话列表的各行：「对话」一段、「项目」标题、机器、项目、按状态 / 时间分的组、工作树、会话，和
// 行上的小件。只读 ctx 里算好的东西（list.sidebar、选中、动画账、主题），不做 IO；点击、拖放、
// 菜单都交给 actions.js、list（state.js）、view（list_view.js）。
//
// ctx：
// - list：state.js 的 SessionList；view：列表视图（start_rename、start_search、ask、drop_at）。
// - t、theme（smelt-plugin-api 的 theme()）、now（unix 秒，行尾的相对时间按它算）。
// - selection、motion（motion.js 的 ListMotion）、avatar_view（行首头像用的视图 id，没有是 null）。
// - rename：正在改名的 { target, state }（target 见 list_view.js），search：搜索框的状态（没在搜是 null）。
// - menus：menus.js 给的菜单项（session、project、workset、machine、machines、open_project、grouping）。
// - new_session_button(id, tooltip, machine, path, hover_group)：项目、工作树行上的「+」。

import { div, svg } from "gpui-kit";
import {
  SmeltArrival,
  SmeltButton,
  SmeltContextMenu,
  SmeltDragItem,
  SmeltFlip,
  SmeltFold,
  SmeltFoldChevron,
  SmeltGlowIcon,
  SmeltListRow,
  SmeltMenuButton,
  SmeltScrollAnchor,
  SmeltStickySection,
  SmeltTextInput,
  SmeltTooltip,
  SmeltTruncatedLine,
  SmeltViewInstance,
  SmeltWash,
} from "smelt-ui";

import * as actions from "./actions.js";
import { bucket_label } from "./group.js";
import { machine_id, same_conversation, selected_session } from "./model.js";
import { fail_key } from "./state.js";
import {
  bucket_flip,
  CONVERSATIONS_KEY,
  machine_flip,
  project_flip,
  project_visible,
  row_tally,
  session_flip,
  workset_flip,
} from "./sidebar.js";
import { tally_is_empty } from "./offscreen.js";
import { relative_time, span } from "./time.js";
import { icon, typed } from "./util.js";

/** 每深一级缩进多少。 */
const INDENT = 16;
/** 第 0 级的行首离行左边多远。 */
const EDGE = 8;
/** 行首图标那一格。 */
const LEAD = 16;
/** 行首头像，和有头像时的行高。 */
const AVATAR = 36;
const AVATAR_ROW = 44;
/** 头像右下角的小标，和它那圈描边（错开多远、几份）。 */
const BADGE = 13;
const BADGE_HALO = 1.5;
const BADGE_HALO_COPIES = 12;
/** 输入框文字离框边的距离：搜索入口和搜索框往左探出这么多，字正好落在「项目」原来的位置。 */
const SEARCH_INSET = 8;
/** 行高（smelt-ui 的 height::sm、md）。 */
const ROW_SM = 28;
const ROW_MD = 32;

function row_muted(p, selected) {
  return selected ? p.muted_selected : p.muted;
}

function row_faint(p, selected) {
  return selected ? p.faint_selected : p.faint;
}

const LIGHT = { good: "ok", degraded: "need", down: "bad" };

function light_color(p, link) {
  return p[LIGHT[link.light]];
}

function dot_color(p, status) {
  return status === "needs_you" ? p.need : p.run;
}

function indent(level) {
  return EDGE + INDENT * level;
}

/** 「项目」下一片里第一级的层级：按机器分开时机器的标题行占了第 0 级。 */
function area_level(sidebar) {
  return sidebar.by_machine ? 1 : 0;
}

const stop = (_event, cx) => cx.stop_propagation();

/** 整个侧栏：「对话」一段（有的话）和「项目」一段，各是吸顶区段。 */
export function sidebar(ctx) {
  const sections = [];
  if (ctx.sidebar.conversations) sections.push(conversations(ctx, ctx.sidebar.conversations));
  sections.push(SmeltStickySection.new().child(projects_header(ctx)).child(column(projects(ctx))));
  return sections;
}

function column(rows) {
  return div().flex().flex_col().children(rows);
}

/**
 * 套一层重排时的滑动：先后变了，这一项从上一帧的位置滑到新位置；是刚拖着放下的那一项，就从手
 * 放下的地方落下去。key 是 Sidebar.flip_order 里的身份。
 */
function flip(ctx, key, child) {
  const landing = ctx.motion.landing_top(key);
  const slot = SmeltFlip.new(key, ctx.motion.generation);
  return (landing == null ? slot : slot.from_top(landing)).child(child);
}

/** 一组的行收成一块，好整块滑；折起来时它就是标题那一行，可视区外的提示盯它（key）。 */
function group_block(key, rows) {
  return SmeltScrollAnchor.new(key).child(div().flex().flex_col().children(rows));
}

/** 一组：标题行，下面是开合的子行。 */
function folding(key, collapsed, header, body) {
  return [header, SmeltFold.new(key, collapsed).child(div().flex().flex_col().children(body))];
}

// ---- 「对话」一段 ----

function conversations(ctx, section) {
  const { theme, t } = ctx;
  const collapsed = section.collapsed;
  const header = typed(
    SmeltListRow.new("sessions-conversations")
      .group("group-row")
      .h(ROW_SM)
      .mx_2()
      .mt_2()
      .px_2()
      .flex()
      .items_center()
      .gap_2(),
    theme.type.text_xsmall_medium,
  )
    .child(disclosure(ctx, icon("message-square"), CONVERSATIONS_KEY, collapsed, true))
    .child(div().flex_1().min_w_0().child(t("kind.conversation_section")))
    .when(collapsed, (row) => row.child(collapsed_summary(ctx, section.status, section.rows.length, false)))
    .on_click(() => ctx.list.set_collapsed({ kind: "conversations" }, !collapsed));
  const body = SmeltFold.new(CONVERSATIONS_KEY, collapsed).child(
    column(section.rows.map((row) => session(ctx, row, 0))),
  );
  return SmeltStickySection.new().child(SmeltScrollAnchor.new(CONVERSATIONS_KEY).child(header)).child(body);
}

// ---- 项目 ----

/**
 * 「项目」标题下面那些行。不按机器分开时所有机器的排在一起，最后是还没收到目录的机器，一台一行；
 * 分开时一台机器一片。
 */
function projects(ctx) {
  const { sidebar, t } = ctx;
  const rows = [];
  if (sidebar.by_machine) {
    for (const area of sidebar.areas) {
      const block = machine_area(ctx, area);
      if (block) rows.push(block);
    }
    return rows;
  }
  // 一台机器的目录都还没收到时不知道有没有项目，只画下面那几行机器。
  const received = sidebar.machines.some((machine) => machine.received);
  const area = sidebar.areas[0];
  if (received && sidebar.projects.length === 0) rows.push(empty_note(ctx, t("list.no_projects"), 0));
  else if (sidebar.filtering && area && area.projects.length === 0) {
    rows.push(empty_note(ctx, t("list.no_matching_projects"), 0));
  } else if (area) rows.push(...area_body(ctx, area));
  for (const machine of sidebar.machines) {
    if (!machine.received) rows.push(waiting_machine_row(ctx, machine));
  }
  return rows;
}

function area_body(ctx, area) {
  return ctx.sidebar.grouping === "project" ? project_groups(ctx, area) : buckets(ctx, area);
}

/** 按机器分开时一台机器那片：标题行，展开时下面是它的项目或组。目录还没收到的只有标题行。 */
function machine_area(ctx, area) {
  const machine = machine_info(ctx.sidebar, area.machine);
  if (!machine) return null;
  const key = machine_flip(machine.machine);
  const group = [machine_row(ctx, machine, area)];
  if (machine.received) {
    let body;
    if (area.projects.length === 0) {
      const note = ctx.sidebar.filtering ? "list.no_matching_projects" : "list.no_projects";
      body = [empty_note(ctx, ctx.t(note), 1)];
    } else body = area_body(ctx, area);
    group.push(SmeltFold.new(key, area.collapsed).child(column(body)));
  }
  return flip(ctx, key, group_block(key, group));
}

function buckets(ctx, area) {
  const level = area_level(ctx.sidebar);
  const rows = [];
  if (area.buckets.length === 0) rows.push(empty_note(ctx, ctx.t("list.no_sessions"), level));
  for (const bucket of area.buckets) {
    const key = bucket_flip(bucket.key);
    const group = folding(
      key,
      bucket.collapsed,
      bucket_row(ctx, bucket, level),
      bucket.rows.map((row) => session(ctx, row, level + 1)),
    );
    rows.push(flip(ctx, key, group_block(key, group)));
  }
  return rows;
}

function project_groups(ctx, area) {
  const { sidebar } = ctx;
  const level = area_level(sidebar);
  const rows = [];
  let hidden = 0;
  for (const project of sidebar.area_projects(area)) {
    const selected = is_selected_project(ctx, project);
    if (!project_visible(sidebar.hide_empty, project.items.length > 0, project.pinned, selected)) {
      hidden += 1;
      continue;
    }
    const key = project_flip(project.machine, project.root);
    const body = project.items.map((item) => {
      if (item.kind === "session") return session(ctx, item.row, level + 1);
      const workset = item.workset;
      const inner = workset_flip(workset.machine, workset.id);
      const group = folding(
        inner,
        workset.collapsed,
        workset_row(ctx, workset),
        workset.sessions.map((row) => session(ctx, row, level + 2)),
      );
      return flip(ctx, inner, group_block(inner, group));
    });
    const group = folding(key, project.collapsed, project_row(ctx, project), body);
    rows.push(flip(ctx, key, group_block(key, group)));
  }
  if (hidden > 0) rows.push(hidden_empty_row(ctx, hidden, level));
  return rows;
}

/**
 * 「项目」标题：悬停露出「筛选与分组」「添加项目」和机器菜单；机器菜单也接右键。挂着远程机器时
 * 「添加项目」先选机器。标题那一格本身是搜索入口，点了换成搜索框。
 */
function projects_header(ctx) {
  const { sidebar, theme, t, menus } = ctx;
  const p = theme.palette;
  const machines_menu = menus.machines();
  const open_project = sidebar.has_remote()
    ? SmeltMenuButton.new("open-project")
        .quiet()
        .small()
        .icon(icon("folder-plus"))
        .accessibility_label(t("list.add_project"))
        .tooltip(t("list.add_project"))
        .anchor("top_right")
        .items(menus.open_project())
    : SmeltButton.new("open-project")
        .quiet()
        .small()
        .icon(icon("folder-plus"))
        .accessibility_label(t("list.add_project"))
        .tooltip(t("list.add_project"))
        .on_click(() => actions.open_project(ctx.list, null));
  const grouping = SmeltMenuButton.new("session-grouping")
    .quiet()
    .small()
    .icon(icon("sliders-horizontal"))
    .accessibility_label(t("list.grouping_menu"))
    .tooltip(t("list.grouping_menu"))
    .anchor("top_right")
    .items(menus.grouping());
  const header = typed(
    div()
      .id("project-section-header")
      .group("projects-header")
      .h(ROW_SM)
      .mx_2()
      .mt_3()
      .px_2()
      .flex()
      .items_center()
      .gap_2(),
    theme.type.text_xsmall_medium,
  )
    .text_color(p.muted)
    .child(ctx.search ? search_field(ctx) : search_entry(ctx))
    .child(
      hover_only("projects-header")
        .flex()
        .items_center()
        .child(grouping)
        .child(open_project)
        .when(machines_menu.length > 0, (slot) =>
          slot.child(
            SmeltMenuButton.new("machines-more")
              .quiet()
              .small()
              .icon(icon("ellipsis"))
              .accessibility_label(t("list.machine"))
              .tooltip(t("list.machine"))
              .anchor("top_right")
              .items(machines_menu),
          ),
        ),
    );
  if (machines_menu.length === 0) return header;
  // 菜单项是元素，只能用一次：右键菜单另建一份，不和「…」按钮共用。
  return SmeltContextMenu.new("machines-context").items(menus.machines()).child(header);
}

/**
 * 按机器分开时机器的标题行，样子跟「对话」标题一样：系统图标按连接上色（悬停换成展开箭头），名字，
 * 用不了时写为什么，折起来时的摘要；悬停露出在这台机器上「添加项目」和机器菜单，菜单也接右键。
 */
function machine_row(ctx, machine, area) {
  const { theme, t, list } = ctx;
  const p = theme.palette;
  const id = machine_id(machine.machine);
  const key = machine_flip(machine.machine);
  const collapsed = area.collapsed;
  const collapsible = machine.received;
  const state = machine.link.unavailable ?? (machine.received ? null : t("machine.connecting"));
  const menu = ctx.menus.machine(machine.machine);
  const actions_slot = hover_only("machine-row")
    .flex()
    .items_center()
    // 按下不往行上传：点按钮不开合。
    .on_mouse_down("left", stop)
    .child(
      SmeltButton.new(`machine-open-project:${id}`)
        .quiet()
        .small()
        .icon(icon("folder-plus"))
        .accessibility_label(t("list.add_project"))
        .tooltip(t("list.add_project"))
        .disabled(machine.link.unavailable != null)
        .on_click(() => actions.open_project(list, machine.machine)),
    )
    .when(menu.length > 0, (slot) =>
      slot.child(
        SmeltMenuButton.new(`machine-more:${id}`)
          .quiet()
          .small()
          .icon(icon("ellipsis"))
          .accessibility_label(t("list.machine"))
          .tooltip(t("list.machine"))
          .anchor("top_right")
          .items(menu),
      ),
    );
  const os = machine_icon(ctx, `machine:${id}`, machine, 14);
  const row = typed(
    SmeltListRow.new(key).group("machine-row").h(ROW_SM).mx_2().mt_2().px_2().flex().items_center().gap_2(),
    theme.type.text_xsmall_medium,
  )
    .child(disclosure_slot(ctx, "machine-row", os, key, collapsed, collapsible))
    .child(
      div().flex_1().min_w_0().child(SmeltTruncatedLine.new(machine.name)),
    )
    .when(state != null, (row) =>
      row.child(typed(div().flex_shrink_0(), theme.type.text_xsmall).text_color(p.faint).child(state)),
    )
    .when(collapsed, (row) => row.child(collapsed_summary(ctx, area.status, area.sessions, false)))
    .child(actions_slot)
    .when(collapsible, (row) => row.on_click(() => list.set_machine_collapsed(machine.machine, !collapsed)));
  if (menu.length === 0) return row;
  // 菜单项是元素，只能用一次：右键菜单另建一份，不和行上的菜单按钮共用。
  return SmeltContextMenu.new(`machine-row-context:${id}`).items(ctx.menus.machine(machine.machine)).child(row);
}

/** 还没收到目录的机器：留一行说一声。系统图标按连接上色，悬停看连接说明，右键是这台机器的菜单。 */
function waiting_machine_row(ctx, machine) {
  const { theme, t } = ctx;
  const p = theme.palette;
  const id = machine_id(machine.machine);
  const row = typed(
    div()
      .id(`machine-waiting:${id}`)
      .h(ROW_MD)
      .mx_2()
      .pl(indent(0))
      .pr_2()
      .flex()
      .items_center()
      .gap_2(),
    theme.type.ui,
  )
    .text_color(p.faint)
    .child(lead_box().child(machine_icon(ctx, `machine-waiting:${id}`, machine, 14)))
    .child(div().flex_1().min_w_0().child(SmeltTruncatedLine.new(machine.name)))
    .child(
      typed(div().flex_shrink_0(), theme.type.text_xsmall).child(
        machine.link.unavailable ?? t("machine.connecting"),
      ),
    );
  const menu = ctx.menus.machine(machine.machine);
  if (menu.length === 0) return row;
  return SmeltContextMenu.new(`machine-context:${id}`).items(menu).child(row);
}

/** 机器的系统图标，按连接上色。悬停它看连接说明（走哪条路、多慢），远程机器顺便叫宿主重测一次。 */
function machine_icon(ctx, id, machine, size) {
  return SmeltTooltip.new(`${id}:icon-tip`, machine.link.detail)
    .flex_shrink_0()
    .child(
      div()
        .id(`${id}:icon`)
        .flex()
        .when(machine.machine != null, (icon) => icon.on_hover(refresh_connections(ctx)))
        .child(svg(machine.os_icon).size(size).text_color(light_color(ctx.theme.palette, machine.link))),
    );
}

/** 连接说明里的延迟只在有人看时采样：悬停远程机器的图标时叫宿主重测一次。 */
function refresh_connections(ctx) {
  return (hovered) => {
    if (!hovered) return;
    ctx.list.host
      .run("smelt.app/refresh-connections")
      .catch((error) => console.error(`[session-list] 重测连接失败：${error?.message ?? error}`));
  };
}

/** 按项目分组、「显示：有会话」时藏起来的空项目有几个；点一下回到「显示：全部」。 */
function hidden_empty_row(ctx, count, level) {
  const { theme, t } = ctx;
  return typed(
    SmeltListRow.new("hidden-empty").h(ROW_SM).mx_2().pl(indent(level)).pr_2().flex().items_center(),
    theme.type.text_xsmall,
  )
    .text_color(theme.palette.faint)
    .child(count === 1 ? t("list.hidden_projects.one") : t("list.hidden_projects.other", { count }))
    .on_click(() => ctx.list.set_hide_empty_projects(false));
}

function empty_note(ctx, text, level) {
  const { theme } = ctx;
  return typed(div().mx_2().px_2().pl(indent(level)).h(ROW_SM).flex().items_center(), theme.type.text_xsmall)
    .text_color(theme.palette.faint)
    .child(text);
}

/** 行上标的机器：系统图标按连接上色（悬停看连接说明），后面是灰字写的名字。`id` 在窗口里唯一。 */
function machine_tag(ctx, id, machine, text, selected) {
  const { theme } = ctx;
  return typed(div().flex_shrink_0().max_w(120).min_w_0().flex().items_center().gap_1(), theme.type.text_xsmall)
    .text_color(row_faint(theme.palette, selected))
    .when(machine != null, (tag) => tag.child(machine_icon(ctx, id, machine, 12)))
    .child(div().min_w_0().child(SmeltTruncatedLine.new(text)));
}

function machine_info(sidebar, machine) {
  return sidebar.machines.find((info) => info.machine === machine) ?? null;
}

/** 项目行上的机器标签：远程项目写机器名，机器用不了时再写为什么。 */
function project_machine_tag(ctx, id, project, selected) {
  const link = project.link;
  const name = project.machine_name;
  const why = link.unavailable;
  const text = name != null && why != null ? `${name} · ${why}` : (name ?? why);
  if (text == null) return null;
  return machine_tag(ctx, `project-machine:${id}`, machine_info(ctx.sidebar, project.machine), text, selected);
}

function project_id(project) {
  return `${machine_id(project.machine)}:${project.root}`;
}

function is_selected_project(ctx, project) {
  const selection = ctx.selection;
  return selection.kind === "project" && selection.machine === project.machine && selection.root === project.root;
}

/** 项目能拖去哪：按机器分开时只在自己那台机器的片里换位置。 */
function project_drag_kind(ctx, project) {
  return ctx.sidebar.by_machine ? `project:${machine_id(project.machine)}` : "project";
}

function project_row(ctx, project) {
  const { list, t } = ctx;
  const { machine, root } = project;
  const id = project_id(project);
  const key = project_flip(machine, root);
  const selected = is_selected_project(ctx, project);
  const collapsible = project.items.length > 0;
  const lead = disclosure(ctx, icon(collapsible && !project.collapsed ? "folder-open" : "folder"), key, project.collapsed, collapsible)
    .id(`project-fold:${id}`)
    .when(collapsible, (lead) =>
      lead.on_click((_event, cx) => {
        cx.stop_propagation();
        list.set_collapsed({ kind: "project", machine, root }, !project.collapsed);
      }),
    );
  const row = project_face(ctx, SmeltListRow.new(`project:${id}`).selected(selected).group("group-row").relative(), lead, project, selected)
    .children(failure_mark(ctx, id, { kind: "project", machine, root }))
    // 点名字选中项目（主区显示项目页）；折叠只靠行首的箭头。
    .on_click(() => {
      if (root === "") return;
      list.report_row_failure({ kind: "project", machine, root }, t("action.select_project"), list.select_project(machine, root));
    });
  const item = SmeltDragItem.new(key, project_drag_kind(ctx, project))
    .mx_2()
    .content(row)
    .on_drop((from, before, top) => {
      const dragged = list.sidebar.projects.find((other) => project_flip(other.machine, other.root) === from);
      if (!dragged) return;
      ctx.motion.drop_at(from, top);
      actions.drop_project(list, { machine: dragged.machine, root: dragged.root }, { machine, root }, before);
    });
  return SmeltContextMenu.new(`project-menu:${id}`).items(ctx.menus.project(project)).child(item);
}

/** 项目行的样子：行首、名字和附属信息、折起来时的摘要、悬停才露出的「新建会话」。 */
function project_face(ctx, el, lead, project, selected) {
  const { theme, t, sidebar } = ctx;
  const p = theme.palette;
  const id = project_id(project);
  const collapsible = project.items.length > 0;
  const name = div()
    .min_w_0()
    .when(selected, (name) => name.font_weight(theme.type.ui_medium.font_weight))
    // 没有会话的项目淡一些；连不上的更淡。
    .when(!collapsible, (name) => name.text_color(row_muted(p, selected)))
    .when(project.link.unavailable != null, (name) => name.text_color(row_faint(p, selected)))
    .child(SmeltTruncatedLine.new(project.name));
  const tag = sidebar.by_machine ? null : project_machine_tag(ctx, id, project, selected);
  return typed(el.h(ROW_MD).pl(indent(area_level(sidebar))).pr_1().flex().items_center().gap_2(), theme.type.ui)
    .child(lead)
    .child(
      div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_1p5()
        .child(name)
        .when(project.pinned, (name) => name.child(pin_marker(ctx)))
        .when(tag != null, (name) => name.child(tag))
        .when(project.branch != null, (name) =>
          name.child(
            typed(
              SmeltTooltip.new(`project-branch:${id}`, project.branch).flex_shrink_0().max_w(120),
              theme.type.text_xsmall,
            )
              .text_color(row_faint(p, selected))
              .child(SmeltTruncatedLine.new(project.branch)),
          ),
        )
        .when(project.repositories > 1, (name) =>
          name.child(
            typed(div().flex_shrink_0(), theme.type.text_xsmall)
              .text_color(row_faint(p, selected))
              .child(t("project.repositories.other", { count: project.repositories })),
          ),
        ),
    )
    .when(project.collapsed, (row) => row.child(collapsed_summary(ctx, project.status, project.sessions, selected)))
    .when(project.root !== "", (row) =>
      row.child(ctx.new_session_button(`project-new:${id}`, t("menu.new_session"), project.machine, project.root, "group-row")),
    );
}

// ---- 按状态、时间分的组 ----

/** 组的标题行：点一下开合。组是现算的，没有菜单、不能拖、不能在组里新建会话。 */
function bucket_row(ctx, group, level) {
  const { theme, t, list } = ctx;
  const key = bucket_flip(group.key);
  const collapsed = group.collapsed;
  return typed(
    SmeltListRow.new(key).group("group-row").h(ROW_MD).mx_2().pl(indent(level)).pr_1().flex().items_center().gap_2(),
    theme.type.ui,
  )
    .child(disclosure(ctx, icon(collapsed ? "folder" : "folder-open"), key, collapsed, true))
    .child(div().flex_1().min_w_0().child(SmeltTruncatedLine.new(bucket_label(group.key.bucket, t))))
    .when(collapsed, (row) => row.child(collapsed_summary(ctx, group.status, group.rows.length, false)))
    .on_click(() => list.set_bucket_collapsed(key, !collapsed));
}

// ---- 工作树 ----

function workset_row(ctx, workset) {
  const { theme, t, list } = ctx;
  const { machine } = workset;
  const id = `${machine_id(machine)}:${workset.id}`;
  const key = workset_flip(machine, workset.id);
  const collapsed = workset.collapsed;
  const collapsible = workset.sessions.length > 0;
  const renaming = rename_state(ctx, { kind: "workset", machine, id: workset.id });
  const title = renaming
    ? rename_field(ctx, renaming)
    : SmeltTooltip.new(`workset-name:${id}`, workset.tooltip)
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_1p5()
        .child(div().min_w_0().child(SmeltTruncatedLine.new(workset.task)))
        .when(workset.pinned, (name) => name.child(pin_marker(ctx)));
  const row = typed(
    SmeltListRow.new(`workset:${id}`)
      .group("group-row")
      .h(ROW_MD)
      .mx_2()
      .pl(indent(area_level(ctx.sidebar) + 1))
      .pr_1()
      .flex()
      .items_center()
      .gap_2(),
    theme.type.ui,
  )
    .child(disclosure(ctx, icon("git-branch"), key, collapsed, collapsible))
    .child(title)
    .when(collapsed, (row) => row.child(collapsed_summary(ctx, workset.status, workset.sessions.length, false)))
    // 分支都合进基线了：一直露着一个「关闭工作树」，合完 PR 回来点一下就收。关之前照样再查、再确认。
    .when(workset.merge === "merged", (row) =>
      row.child(
        SmeltButton.new(`workset-merged:${id}`)
          .quiet()
          .small()
          .icon(icon("circle-check"))
          .accessibility_label(t("list.finish_merged"))
          .tooltip(t("list.finish_merged"))
          .on_click((_event, cx) => {
            cx.stop_propagation();
            actions.finish_workset(list, ctx.view.ask, machine, workset.id, workset.task);
          }),
      ),
    )
    .child(ctx.new_session_button(`workset-new:${id}`, t("list.new_in_workset"), machine, workset.path, "group-row"))
    .children(failure_mark(ctx, id, { kind: "workset", machine, id: workset.id }))
    // 工作树没有自己的页：点一下就是开合（改名走菜单，免得双击先开合一次）。
    .on_click(() => {
      if (collapsible) list.set_collapsed({ kind: "workset", machine, id: workset.id }, !collapsed);
    });
  return SmeltContextMenu.new(`workset-menu:${id}`).items(ctx.menus.workset(workset)).child(row);
}

// ---- 会话 ----

function session_id(key) {
  return `${machine_id(key.machine)}:${key.id}`;
}

/** 会话能拖去哪：只有按项目分组时顺序是手动的，只在同一组里换位置。 */
function session_drag_kind(ctx, row) {
  const group = row.group;
  if (ctx.sidebar.grouping !== "project" || group.kind !== "project") return null;
  return `session:${machine_id(group.machine)}:${group.root}:${group.workset ?? ""}`;
}

/**
 * depth 是这一行的层级（行首是图标时就缩这么多级）。行首是头像时少缩一级：头像左边对齐上一级的
 * 行首图标，标题自然比上一级的名字往右错开一截。
 */
function session(ctx, row, depth) {
  const { list, t, motion } = ctx;
  const key = row.key;
  const id = session_id(key);
  const flip_key = session_flip(key);
  const selected = same_conversation(selected_session(ctx.selection), key);
  const renaming = rename_state(ctx, { kind: "conversation", key });
  const wash = motion.wash_serial(flip_key);
  const close = SmeltButton.new(`session-close:${id}`)
    .dismiss()
    .small()
    .icon(icon("close"))
    .accessibility_label(t("close.confirm"))
    .tooltip(t("close.confirm"))
    .on_click((_event, cx) => {
      cx.stop_propagation();
      actions.close_session(list, ctx.view.ask, key, row.title, row.status);
    });
  const face = session_face(
    ctx,
    SmeltListRow.new(`session:${id}`)
      .selected(selected)
      .group("session-row")
      .relative()
      .when(wash != null, (item) => item.child(SmeltWash.new(flip_key, wash))),
    row,
    depth,
    selected,
    renaming,
    close,
  )
    .children(failure_mark(ctx, id, { kind: "conversation", key }))
    .tooltip(row.tooltip)
    .on_click((event) => {
      if (event.click_count === 2) {
        ctx.view.start_rename({ kind: "conversation", key }, row.title);
        return;
      }
      // 出口里可能正放着别的页（归档这些）：打开会话视图，露出来再选中。
      list.report_row_failure({ kind: "conversation", key }, t("action.open_session"), list.open_conversation(key));
    });
  const kind = session_drag_kind(ctx, row);
  const item =
    kind == null
      ? face.mx_2()
      : SmeltDragItem.new(flip_key, kind)
          .mx_2()
          .content(face)
          .on_drop((from, before, top) => {
            const dragged = list.sidebar.group_rows(row.group).find((other) => session_flip(other.key) === from);
            if (!dragged) return;
            motion.drop_at(from, top);
            actions.drop_session(list, dragged.key, key, row.group, before);
          });
  const anchored = SmeltScrollAnchor.new(flip_key).child(
    SmeltContextMenu.new(`session-menu:${id}`).items(ctx.menus.session(row)).child(item),
  );
  const arrival = motion.arrival_serial(flip_key);
  return flip(ctx, flip_key, arrival == null ? anchored : SmeltArrival.new(flip_key, arrival).child(anchored));
}

/** 会话行的样子：行首、标题、行尾。renaming 是顶替标题的输入框状态，close 是悬停时换到行尾的关闭键。 */
function session_face(ctx, el, row, depth, selected, renaming, close) {
  const { theme, sidebar } = ctx;
  const p = theme.palette;
  const key = row.key;
  const id = session_id(key);
  const avatar = ctx.avatar_view != null;
  const lead = avatar ? session_avatar(ctx, id, row) : session_icon(ctx, id, row, selected);
  const level = avatar ? Math.max(depth - 1, 0) : depth;
  // 标题亮不亮看要不要你看：需要你处理的、有没看过的提醒的亮且加重，在跑的、闲着的淡下去。选中的照常亮。
  const loud = !tally_is_empty(row_tally(row));
  const quiet = !selected && !loud;
  const title = renaming
    ? rename_field(ctx, renaming)
    : div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_1p5()
        .child(
          div()
            .min_w_0()
            .when(selected || loud, (title) => title.font_weight(theme.type.ui_medium.font_weight))
            .when(quiet, (title) => title.text_color(p.muted))
            .child(SmeltTruncatedLine.new(row.title)),
        )
        .when(row.pinned, (title) => title.child(pin_marker(ctx)))
        .when(row.machine_name != null, (title) =>
          title.child(machine_tag(ctx, `session-machine:${id}`, machine_info(sidebar, key.machine), row.machine_name, selected)),
        )
        .when(row.origin != null, (title) => title.children(origin_labels(ctx, id, row.origin, selected)));
  return typed(
    el
      .h(avatar ? AVATAR_ROW : ROW_MD)
      .pl(indent(level))
      .pr_1()
      .flex()
      .items_center()
      .gap_2(),
    theme.type.ui,
  )
    .child(lead)
    .child(title)
    .child(trailing(ctx, meta(ctx, row, selected), close));
}

/** 按状态、时间分组时标题后面补的来源：工作树的小标签，项目名和分支。 */
function origin_labels(ctx, id, origin, selected) {
  const { theme } = ctx;
  const p = theme.palette;
  const labels = [];
  if (origin.workset) {
    labels.push(
      typed(
        SmeltTooltip.new(`session-workset:${id}`, origin.workset.tooltip)
          .flex_shrink_0()
          .max_w(104)
          .min_w_0()
          .flex()
          .items_center()
          .gap_1()
          .px_1()
          .rounded(3)
          .bg(p.hair),
        theme.type.text_xsmall,
      )
        .text_color(row_muted(p, selected))
        .child(svg(icon("git-branch")).size(12).flex_shrink_0().text_color(row_muted(p, selected)))
        .child(div().min_w_0().child(SmeltTruncatedLine.new(origin.workset.task))),
    );
  }
  labels.push(
    typed(div().flex_shrink_0().max_w(160).min_w_0().flex().items_center().gap_1(), theme.type.text_xsmall)
      .text_color(row_muted(p, selected))
      .child(div().min_w_0().child(SmeltTruncatedLine.new(origin.project)))
      .when(origin.branch != null, (label) =>
        label.child(
          SmeltTooltip.new(`session-branch:${id}`, origin.branch)
            .min_w_0()
            .text_color(row_faint(p, selected))
            .child(SmeltTruncatedLine.new(origin.branch)),
        ),
      ),
  );
  return labels;
}

function icon_path(row_icon) {
  switch (row_icon.kind) {
    case "provider":
      return row_icon.path;
    case "terminal":
      return icon("terminal");
    default:
      return icon("message-square");
  }
}

/**
 * 行首的头像：填进插槽的视图给这段会话的实例。右下角的小标标明是哪家 CLI，跟着状态上色：在跑、
 * 要你用状态色，闲着用正文色。小标像贴纸一样贴在小人身上：同一个图标换成浮起面的颜色、往四周
 * 错开画几份垫在下面，拼出一圈描边（svg 不能描边）。
 */
function session_avatar(ctx, id, row) {
  const p = ctx.theme.palette;
  const path = icon_path(row.icon);
  const layer = (x, y, color) =>
    div()
      .absolute()
      .left(x)
      .top(y)
      .size(BADGE)
      .flex()
      .items_center()
      .justify_center()
      .child(svg(path).size(BADGE).text_color(color));
  const halo = [];
  for (let k = 0; k < BADGE_HALO_COPIES; k++) {
    const angle = (k * 2 * Math.PI) / BADGE_HALO_COPIES;
    halo.push(layer(BADGE_HALO * Math.cos(angle), BADGE_HALO * Math.sin(angle), p.surface));
  }
  const badge = div()
    .absolute()
    .right(-2)
    .bottom(-1)
    .size(BADGE)
    .children(halo)
    .child(layer(0, 0, row.status === "idle" ? p.text : dot_color(p, row.status)));
  const figure = SmeltViewInstance.new(ctx.avatar_view)
    .id(`avatar:${id}`)
    .size_full()
    .args(JSON.stringify({ machine: row.key.machine, conversation: row.key.id }));
  return (row.icon.kind === "provider" ? SmeltTooltip.new(`session-avatar:${id}`, row.icon.label) : div())
    .relative()
    .size(AVATAR)
    .flex_shrink_0()
    .child(figure)
    .child(badge);
}

/** 没人填头像插槽时行首的图标：空闲随行的次要字色，在跑、要人管的用状态色；刚开始跑时亮一下。 */
function session_icon(ctx, id, row, selected) {
  const p = ctx.theme.palette;
  const path = icon_path(row.icon);
  const flip_key = session_flip(row.key);
  const glow = row.status === "running" ? ctx.motion.glow_serial(flip_key) : null;
  const glyph =
    glow != null
      ? SmeltGlowIcon.new(path, p.run, flip_key, glow).size(LEAD)
      : svg(path)
          .size(LEAD)
          .text_color(row.status === "idle" ? row_muted(p, selected) : dot_color(p, row.status));
  const inner =
    row.icon.kind === "provider" ? SmeltTooltip.new(`session-provider:${id}`, row.icon.label).child(glyph) : glyph;
  return lead_box().child(inner);
}

/**
 * 行尾的字，状态先说话：用不了的机器写为什么，要人管的写「需要你」，在跑的写这一轮跑了多久，别的
 * 写最近一次动静在多久前。不为它定时重画，每次重画按当时算。
 */
function meta(ctx, row, selected) {
  const { theme, t, now } = ctx;
  const p = theme.palette;
  const unread = row_tally(row).unread > 0;
  if (row.unavailable != null) return { text: row.unavailable, color: row_faint(p, selected), unread };
  if (row.status === "needs_you") return { text: t("bucket.needs_you"), color: p.need, unread };
  if (row.status === "running" && row.turn_started_at > 0) {
    return { text: span(Math.max(now - row.turn_started_at, 0), t), color: p.run, unread };
  }
  return { text: relative_time(row.activity, now, t), color: row_faint(p, selected), unread };
}

/**
 * 行尾一格：平时写字，悬停这一行时同一格换成关闭键。两样叠着放，换的时候这一格不变宽。按下不往
 * 行上传，免得点关闭先选中了会话。
 */
function trailing(ctx, meta, close) {
  const { theme } = ctx;
  return div()
    .relative()
    .flex_shrink_0()
    .min_w(20)
    .h(20)
    .flex()
    .items_center()
    .justify_end()
    .child(
      typed(div().flex().items_center().gap_1(), theme.type.text_xsmall)
        .text_color(meta.color)
        .group_hover("session-row", (style) => style.opacity(0))
        .when(meta.unread, (label) =>
          label.child(div().size(6).flex_shrink_0().rounded_full().bg(theme.palette.link)),
        )
        .child(meta.text),
    )
    .child(
      div()
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .flex()
        .items_center()
        // 悬停样式挂在这层 div 上：gpui-shell 只把普通样式交给注册组件，按钮自己的 group_hover 会被丢掉。
        .opacity(0)
        .group_hover("session-row", (style) => style.opacity(1))
        .on_mouse_down("left", stop)
        .child(close),
    );
}

// ---- 共用的小件 ----

/**
 * 这一行上刚失败的操作：行尾一个 bad 色的叹号，悬停看为什么，点一下收起。按下不往行上传，免得
 * 收起时先选中、打开了这一行。
 */
function failure_mark(ctx, id, target) {
  const failure = ctx.list.failures.get(fail_key(target));
  if (!failure) return [];
  const text = ctx.t("error.failed", { what: failure.what, error: failure.error });
  return [
    div()
      .flex_shrink_0()
      .on_mouse_down("left", stop)
      .child(
        SmeltButton.new(`failure:${id}`)
          .danger_quiet()
          .small()
          .icon(icon("circle-alert"))
          .accessibility_label(text)
          .tooltip(text)
          .on_click((_event, cx) => {
            cx.stop_propagation();
            ctx.list.set_failure(target, null);
          }),
      ),
  ];
}

function lead_box() {
  return div().size(LEAD).flex_shrink_0().flex().items_center().justify_center();
}

/** 分组行开头那一格：平时是分组自己的图标，悬停整行时换成展开 / 收起箭头。 */
function disclosure(ctx, path, key, collapsed, collapsible) {
  const muted = ctx.theme.palette.muted;
  return disclosure_slot(ctx, "group-row", svg(path).size(14).text_color(muted), key, collapsed, collapsible);
}

/**
 * disclosure 的通用版：icon 是平时那个图标（机器行的是按连接上色的系统图标），悬停 group 那一行时
 * 换成箭头。两样叠在同一格里只换透明度，名字不跟着动；箭头跟着那一组的开合转。
 */
function disclosure_slot(ctx, group, glyph, key, collapsed, collapsible) {
  const muted = ctx.theme.palette.muted;
  return lead_box()
    .relative()
    .text_color(muted)
    // 悬停样式挂在包图标的 div 上：gpui-shell 画 svg 时只接普通样式，svg 上的 group_hover 会被丢掉。
    .child(collapsible ? div().flex().child(glyph).group_hover(group, (style) => style.opacity(0)) : glyph)
    .when(collapsible, (lead) =>
      lead.child(
        div()
          .absolute()
          .inset_0()
          .flex()
          .items_center()
          .justify_center()
          .opacity(0)
          .group_hover(group, (style) => style.opacity(1))
          .child(SmeltFoldChevron.new(key, collapsed).size(14).text_color(muted)),
      ),
    );
}

/** 折起来时标题行尾：有要人管、在跑的点一个状态点，再写下面有几段。 */
function collapsed_summary(ctx, status, count, selected) {
  const { theme } = ctx;
  const p = theme.palette;
  return typed(div().flex_shrink_0().flex().items_center().gap_1p5(), theme.type.text_xsmall)
    .text_color(row_faint(p, selected))
    .when(status !== "idle", (summary) => summary.child(div().size(6).rounded_full().bg(dot_color(p, status))))
    .when(count > 0, (summary) => summary.child(String(count)));
}

function pin_marker(ctx) {
  return svg(icon("pin")).size(11).flex_shrink_0().text_color(ctx.theme.palette.faint);
}

/** 只在悬停那一行（group）时露出来的按钮位。 */
function hover_only(group) {
  return div()
    .flex_shrink_0()
    .opacity(0)
    .group_hover(group, (style) => style.opacity(1));
}

/**
 * 「项目」标题那一格兼当搜索入口：平时只是标题；悬停显出输入框的样子（底色、投影、末尾放大镜），
 * 点一下换成真的搜索框。
 */
function search_entry(ctx) {
  const { theme, t } = ctx;
  const p = theme.palette;
  return SmeltTooltip.new("search-projects-tip", t("list.search_projects"))
    .flex_1()
    .min_w_0()
    .h_full()
    .ml(-SEARCH_INSET)
    .child(
      div()
        .id("search-projects")
        .group("search-projects")
        .size_full()
        .px(SEARCH_INSET)
        .flex()
        .items_center()
        .gap_2()
        .rounded(theme.radius.md)
        .cursor_text()
        .hover((entry) => entry.bg(p.bg).shadow(theme.elevation.input))
        .child(div().flex_1().min_w_0().child(t("grouping.project")))
        .child(
          // 悬停样式挂在外层 div 上，理由同 disclosure_slot。
          div()
            .flex()
            .flex_shrink_0()
            .opacity(0)
            .group_hover("search-projects", (style) => style.opacity(1))
            .child(svg(icon("search")).size(12).text_color(p.muted)),
        )
        .on_click(() => ctx.view.start_search()),
    );
}

/** 「项目」标题上的搜索框，占标题那一格。字号跟标题一样，只换成常规字重。 */
function search_field(ctx) {
  return div()
    .flex_1()
    .min_w_0()
    .ml(-SEARCH_INSET)
    .on_mouse_down("left", stop)
    .child(
      SmeltTextInput.new(ctx.search)
        .small()
        .text_size(ctx.theme.type.text_xsmall.text_size)
        .on_escape(() => ctx.view.escape()),
    );
}

/** 行内改名的输入框，占标题那一格。按下不往行上传，免得点进输入框就起拖拽或切会话。 */
function rename_field(ctx, state) {
  return div()
    .flex_1()
    .min_w_0()
    .on_mouse_down("left", stop)
    .child(
      SmeltTextInput.new(state)
        .small()
        .on_escape(() => ctx.view.escape()),
    );
}

function rename_state(ctx, target) {
  const rename = ctx.rename;
  if (!rename || rename.target.kind !== target.kind) return null;
  if (target.kind === "conversation") return same_conversation(rename.target.key, target.key) ? rename.state : null;
  return rename.target.machine === target.machine && rename.target.id === target.id ? rename.state : null;
}
