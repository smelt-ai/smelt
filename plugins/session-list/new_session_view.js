// 新建会话菜单的样子（状态和规则在 new_session.js）：项目行、工作树行上的「+」弹层，和 ⌘N 打开、
// 钉在窗口上方的那张（插件的根画它，见 main.js）。
//
// 行高、面板内边距和组件库的菜单一致。

import { div, svg } from "gpui-kit";
import { new_session_icon, t, theme } from "smelt-plugin-api";
import {
  SmeltButton,
  SmeltDialog,
  SmeltKeybind,
  SmeltMenuButton,
  SmeltMenuItem,
  SmeltMenuLabel,
  SmeltMenuSeparator,
  SmeltPopover,
  SmeltScrollAnchor,
  SmeltScrollView,
} from "smelt-ui";

import { OPEN_PROJECT } from "./contract.js";
import { digit_key, initial_location, NewSession, pinned_by_digit } from "./new_session.js";
import { icon, typed } from "./util.js";

/** 菜单宽度。从命令打开的那张顶上多一行「机器 · 项目 / 工作树」，宽一些。 */
const WIDTH = 280;
const PALETTE_WIDTH = 360;
/** 列表最高多高，再多就在里面滚。 */
const LIST_HEIGHT = 420;
/** 从命令打开时卡片顶边离窗口顶，和命令面板一样。 */
const PALETTE_TOP = 80;
const ROW_HEIGHT = 28;
const PADDING = 4;

/**
 * 项目行、工作树行的「+」：点开贴着它弹出新建菜单，位置就是 path（项目根或工作树路径）。给了悬停组的
 * 平时隐着、悬停那一行才出现；菜单开着时一直显示，鼠标移进菜单、离开那一行也不消失。
 *
 * 开着的是哪一个记在列表视图上（view.new_session：{ id, menu, focus }），每次打开新建一份菜单状态：
 * 每次打开都重读目录。
 */
export function new_session_button(list, view, id, tooltip, machine, path, hover_group) {
  const open = view.new_session?.id === id;
  const close = (menu) => {
    menu.dispose();
    if (view.new_session?.menu !== menu) return;
    view.new_session = null;
    view.redraw();
  };
  let popover = SmeltPopover.new(id)
    .w(WIDTH)
    .p(PADDING)
    .open(open)
    .on_open_change((opened, cx) => {
      if (!opened) {
        if (view.new_session?.id === id) close(view.new_session.menu);
        return;
      }
      if (view.new_session) view.new_session.menu.dispose();
      const owner = { palette: false, redraw: () => view.redraw(), close: () => close(menu) };
      const menu = new NewSession(list, initial_location(list.host, machine, path), owner);
      // 弹层开好时焦点在它自己身上：交给菜单，上下键、数字键才收得到。
      const focus = cx.focus_handle();
      view.new_session = { id, menu, focus };
      focus.focus();
      view.redraw();
    })
    .trigger(SmeltButton.new(`${id}:trigger`).quiet().small().icon(icon("plus")).accessibility_label(tooltip).tooltip(tooltip));
  // SmeltPopover 一定要有 content（只在打开时才画）：关着时给个空的占位，打开后才建菜单。
  popover = popover.content(open ? menu_body(view.new_session.menu, view.new_session.focus) : div());
  return div()
    .flex_shrink_0()
    .when(hover_group != null && !open, (slot) => slot.opacity(0).group_hover(hover_group, (style) => style.opacity(1)))
    .child(popover);
}

/**
 * ⌘N 打开的那张：钉在窗口上方，带位置行。list.palette 是开着的那份菜单，focus 是插件的根给的焦点
 * （对话框开好时交给菜单）。Enter 在对话框层是确认动作，先于按键监听，走 on_confirm。
 */
export function palette_dialog(list, focus) {
  const menu = list.palette;
  return SmeltDialog.new("new-session-palette")
    .open(true)
    .pin_top(PALETTE_TOP)
    .w(PALETTE_WIDTH)
    .p(PADDING)
    .gap_0()
    .content(menu_body(menu, focus))
    .on_open(() => focus.focus())
    .on_confirm(() => {
      if (menu.selected != null) menu.launch(menu.selected);
    })
    .on_dismiss(() => menu.owner.close());
}

/** 菜单本身：焦点挂在整块上（列表还没读到时也在），Esc 照样冒到弹出层、对话框。 */
function menu_body(menu, focus) {
  const look = theme();
  return div()
    .flex()
    .flex_col()
    .track_focus(focus)
    .on_key_down((event, cx) => {
      const modifiers = event.modifiers;
      if (modifiers.control || modifiers.alt || modifiers.shift || modifiers.platform || modifiers.function) return;
      switch (event.key) {
        case "up":
          menu.step(-1);
          break;
        case "down":
          menu.step(1);
          break;
        // 弹出层里 Enter 不是动作，落到这里；对话框里它先被对话框层接走（on_confirm）。
        case "enter":
          if (menu.selected == null) return;
          menu.launch(menu.selected);
          break;
        default: {
          const index = pinned_by_digit(menu.rows, event.key);
          if (index == null) return;
          menu.launch(index);
        }
      }
      cx.stop_propagation();
    })
    .when(menu.owner.palette, (body) => body.child(location_row(menu)).child(separator(look)))
    .child(body(menu, look))
    .when(menu.error != null, (body) =>
      body.child(typed(div().px(12).py_1(), look.type.ui).text_color(look.palette.bad).child(menu.error)),
    );
}

/** 分隔线：上下各 2，横向顶到面板边，和组件库的菜单一致。 */
function separator(look) {
  return div().flex_none().h(1).my(2).mx(-PADDING).bg(look.palette.hair);
}

function body(menu, look) {
  const notice = (text) => typed(div().px(12).py_2(), look.type.ui).text_color(look.palette.muted).child(text);
  if (menu.root == null) {
    return div()
      .flex()
      .flex_col()
      .items_start()
      .child(notice(t("new.no_projects", { machine: menu.machine_name })))
      .child(
        div()
          .px(6)
          .pb(4)
          .child(
            SmeltButton.new("new-session-open-project")
              .small()
              .label(t("command.open_project"))
              .on_click(() => {
                menu.dispose();
                menu.owner.close();
                menu.list.report_failure(t("action.open_project"), menu.host.run(OPEN_PROJECT));
              }),
          ),
      );
  }
  switch (menu.catalog.state) {
    case "loading":
      return notice(t("new.loading_tools", { machine: menu.machine_name }));
    case "failed":
      return notice(t("new.unreachable", { machine: menu.machine_name, error: menu.catalog.error }));
  }
  if (menu.rows.length === 0) return notice(t("new.no_options"));
  let list = SmeltScrollView.new("new-session-actions")
    .max_h(LIST_HEIGHT)
    .child(
      div()
        .flex()
        .flex_col()
        .children(
          menu.slots.map((slot) => {
            switch (slot.kind) {
              case "separator":
                return separator(look);
              case "header":
                return typed(div().h(ROW_HEIGHT).flex().items_center().px(12), look.type.small_medium)
                  .text_color(look.palette.faint)
                  .child(t(slot.section === 1 ? "kind.terminal" : "kind.conversation"));
              case "row":
                return row(menu, slot.index, look);
            }
          }),
        ),
    );
  if (menu.selected != null && menu.reveal_serial > 0) list = list.reveal(row_key(menu.selected), menu.reveal_serial);
  return list;
}

const row_key = (index) => `new-session-row:${index}`;

/** 一行：图标、名字；常用行再标类型和数字键；选中的那行最右是星标。 */
function row(menu, index, look) {
  const { section, action } = menu.rows[index];
  const selected = menu.selected === index;
  const common = section === 0;
  const p = look.palette;
  const digit = digit_key(menu.rows[index], index);
  const line = div()
    .id(`new-session-action:${index}`)
    .h(ROW_HEIGHT)
    .flex_none()
    .pl(12)
    .pr(4)
    .rounded(look.radius.sm)
    .flex()
    .items_center()
    .gap(8)
    .when(selected, (line) => line.bg(p.soft_hover))
    // 鼠标和键盘共用一个高亮：移上去就选中，星标跟着出现。
    .on_mouse_move(() => menu.hover(index))
    .on_click(() => menu.launch(index))
    .child(svg(new_session_icon(action)).flex_none().size(16).text_color(p.muted))
    .child(typed(div().flex_1().min_w_0().truncate(), look.type.ui).child(action.label))
    .when(common, (line) =>
      line.child(
        typed(div().flex_none(), look.type.text_xsmall)
          .text_color(p.muted)
          .child(t(action.kind === "terminal" ? "kind.terminal" : "kind.conversation")),
      ),
    )
    .when(digit != null, (line) =>
      line.child(SmeltKeybind.new(digit).backdrop(selected ? p.soft_hover : p.surface)),
    )
    .child(
      div()
        .flex_none()
        .w(20)
        .when(selected, (slot) =>
          slot.child(
            SmeltButton.new(`new-session-pin:${index}`)
              .quiet()
              .small()
              .icon(icon(action.pinned ? "star-fill" : "star"))
              .accessibility_label(action.pinned ? t("new.unfavorite") : t("new.favorite"))
              .tooltip(action.pinned ? t("new.unfavorite") : t("new.favorite"))
              .on_click((_event, cx) => {
                // 星标在行里，别让这一下也算点了行。
                cx.stop_propagation();
                menu.toggle_pin(action.key);
              }),
          ),
        ),
    );
  return SmeltScrollAnchor.new(row_key(index)).child(line);
}

/** 命令打开时顶上那行：机器 · 位置，点开换。 */
function location_row(menu) {
  const location = menu.location ?? t("new.choose_location");
  return div()
    .flex()
    .items_center()
    .px(6)
    .py(4)
    .child(
      SmeltMenuButton.new("new-session-location-menu")
        .quiet()
        .small()
        .label(`${menu.machine_name} · ${location}`)
        .icon(icon(menu.in_workset ? "git-branch" : "folder"))
        .dropdown_caret()
        .items(location_menu(menu)),
    );
}

/** 位置下拉：每台机器上打开的项目和它们的工作树，最后是在选中的项目里新建工作树。 */
function location_menu(menu) {
  const host = menu.host;
  const machines = host.machines();
  const labelled = machines.length > 1;
  const entries = [];
  for (const info of machines) {
    const machine = info.machine;
    if (labelled) entries.push(SmeltMenuLabel.new(info.name));
    const directory = host.workspace_directory(machine);
    if (!directory) {
      entries.push(SmeltMenuItem.new(t("new.not_connected")).disabled(true));
      continue;
    }
    if (directory.projects.length === 0) {
      entries.push(SmeltMenuItem.new(t("list.no_projects")).disabled(true));
      continue;
    }
    const labels = host.project_labels(host.path_style(machine), directory.projects);
    const here = (machine ?? "") === (menu.machine ?? "");
    directory.projects.forEach((root, ix) => {
      const label = labels[ix];
      entries.push(
        SmeltMenuItem.new(label)
          .icon(icon("folder"))
          .checked(here && menu.root === root && menu.workset == null)
          .on_click(() => menu.set_location(machine, root, null)),
      );
      for (const workset of directory.worksets.filter((workset) => workset.project_root === root)) {
        entries.push(
          SmeltMenuItem.new(`${label} / ${workset.task}`)
            .icon(icon("git-branch"))
            .checked(here && menu.workset === workset.id)
            .on_click(() => menu.set_location(machine, root, workset.id)),
        );
      }
    });
  }
  entries.push(SmeltMenuSeparator.new());
  entries.push(
    SmeltMenuItem.new(t("menu.new_workset"))
      .icon(icon("git-branch"))
      .disabled(menu.root == null)
      .on_click(() => {
        // 在选中的项目里建工作树：仓库对话框叠在上面，建好了回到这里并选上它。
        const root = menu.root;
        if (root == null) return;
        menu.list.open_repositories(menu.machine, root, "create", (task) => menu.await_workset(root, task));
      }),
  );
  return entries;
}
