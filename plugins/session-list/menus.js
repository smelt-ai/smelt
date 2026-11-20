// 列表上的菜单：会话、项目、工作树、机器（挂在「项目」标题上，按机器分开时也在机器的标题行上），
// 「添加项目」选机器，和「筛选与分组」。会话、项目、机器菜单里还有别的插件登记进来的项
// （contract 的 SESSION_MENU、PROJECT_MENU、MACHINE_MENU，如「在 IDE 里打开」、机器的重连、改名），
// 点了按登记的命令执行，参数宿主已并上菜单的上下文。
//
// 菜单是每次画都建好的元素：登记的项在画的时候现取（context_menu_items 只读宿主内存）。

import { context_menu_items } from "smelt-plugin-api";
import { SmeltMenuItem, SmeltMenuLabel, SmeltMenuSeparator } from "smelt-ui";

import * as actions from "./actions.js";
import { MACHINE_MENU, PROJECT_MENU, SESSION_MENU } from "./contract.js";
import { icon, message } from "./util.js";

const LIGHT = { good: "ok", degraded: "need", down: "bad" };

/** 机器连接灯的颜色。 */
function light(theme, link) {
  return theme.palette[LIGHT[link.light]];
}

function separator() {
  return SmeltMenuSeparator.new();
}

/** 用分隔线把几段接起来，空的段不要。 */
function sections(...parts) {
  const entries = [];
  for (const part of parts) {
    if (part.length === 0) continue;
    if (entries.length > 0) entries.push(separator());
    entries.push(...part);
  }
  return entries;
}

/**
 * ctx：list、view（start_rename、ask）、t、theme、sidebar。
 * 返回 rows.js 要的各个菜单，每个给元素数组（空数组是没有菜单）。
 */
export function menus(ctx) {
  const { list, view, t, theme, sidebar } = ctx;

  /** 别的插件往 menu 里登记的项。命令失败挂在 target 那一行上，没有就发系统通知。 */
  function plugin_items(menu, machine, context, target) {
    return context_menu_items(menu, machine, context).map((entry) => plugin_item(entry, target));
  }

  function plugin_item(entry, target) {
    let item = SmeltMenuItem.new(entry.label);
    if (entry.icon != null) item = item.icon(entry.icon);
    if (entry.image != null) item = item.image(entry.image);
    if ("submenu" in entry) return item.items(entry.submenu.map((inner) => plugin_item(inner, target)));
    const { command, args } = entry;
    return item.on_click(() => {
      const ran = list.host.run(command, args);
      if (target) {
        list.report_row_failure(target, t("action.run_command", { command }), ran);
        return;
      }
      ran.catch((error) => list.notify_error(t("error.command_failed", { command, error: message(error) })));
    });
  }

  function pin_item(pinned, toggle) {
    return SmeltMenuItem.new(pinned ? t("menu.unpin") : t("menu.pin"))
      .icon(icon(pinned ? "pin-off" : "pin"))
      .on_click(() => toggle(!pinned));
  }

  function copy_item(label, glyph, text) {
    return SmeltMenuItem.new(label)
      .icon(icon(glyph))
      .on_click((_event, cx) => cx.write_to_clipboard(text));
  }

  /** 一台机器的菜单：全是别的插件登记的。机器标题只在按机器分组时才有，失败没有稳定的行可挂。 */
  function machine(key) {
    return plugin_items(MACHINE_MENU, key, { machine: key }, null);
  }

  function session(row) {
    const key = row.key;
    const target = { kind: "conversation", key };
    const pin = pin_item(row.pinned, (pinned) => actions.pin_session(list, key, row.group, pinned));
    const rename = SmeltMenuItem.new(t("menu.rename"))
      .icon(icon("pencil"))
      .on_click(() => view.start_rename(target, row.title));
    const archive = SmeltMenuItem.new(t("archive.confirm"))
      .icon(icon("archive"))
      .on_click(() => actions.archive_session(list, view.ask, key, row.title, row.status));
    const copy_id = SmeltMenuItem.new(t("menu.copy_session_id"))
      .icon(icon("copy"))
      .disabled(row.session_id == null)
      .on_click((_event, cx) => {
        if (row.session_id != null) cx.write_to_clipboard(list.session_id_to_copy(key.machine, row.session_id));
      });
    const close = SmeltMenuItem.new(t("close.confirm"))
      .icon(icon("close"))
      .on_click(() => actions.close_session(list, view.ask, key, row.title, row.status));
    const context = { machine: key.machine, conversation: key.id, session_id: row.session_id };
    return sections(
      [pin, rename],
      plugin_items(SESSION_MENU, key.machine, context, target),
      [archive, copy_id],
      [close],
    );
  }

  /** 项目行的菜单。root 为空的那组（没有工作目录的会话）只有置顶。 */
  function project(group) {
    const { machine: key, root } = group;
    const parts = [];
    if (root !== "") {
      parts.push([
        SmeltMenuItem.new(t("menu.new_workset"))
          .icon(icon("git-branch"))
          .on_click(() => list.open_repositories(key, root, "create")),
        SmeltMenuItem.new(t("menu.manage_repositories"))
          .icon(icon("repositories"))
          .on_click(() => list.open_repositories(key, root, "manage")),
      ]);
      parts.push(plugin_items(PROJECT_MENU, key, { machine: key, path: root }, { kind: "project", machine: key, root }));
      const copies = [copy_item(t("menu.copy_project_path"), "copy", root)];
      if (group.branch != null) copies.push(copy_item(t("menu.copy_branch"), "git-branch", group.branch));
      parts.push(copies);
    }
    // 远程项目带上那台机器的菜单：机器连不上时它的项目变灰，右键就能重新连接。
    if (group.machine_name != null) {
      const items = machine(key);
      if (items.length > 0) parts.push([SmeltMenuItem.new(t("menu.machine", { name: group.machine_name })).items(items)]);
    }
    parts.push([pin_item(group.pinned, (pinned) => actions.pin_project(list, key, root, pinned))]);
    if (root !== "") {
      parts.push([
        SmeltMenuItem.new(group.opened ? t("menu.close_project") : t("menu.close_group"))
          .icon(icon("close"))
          .on_click(() => actions.close_project(list, view.ask, key, root, group.name, group.sessions)),
      ]);
    }
    return sections(...parts);
  }

  function workset(group) {
    const { machine: key, id } = group;
    const pin = pin_item(group.pinned, (pinned) => actions.pin_workset(list, key, id, pinned));
    const rename = SmeltMenuItem.new(t("menu.rename_workset"))
      .icon(icon("pencil"))
      .on_click(() => view.start_rename({ kind: "workset", machine: key, id }, group.task));
    let pull_request = SmeltMenuItem.new(t("menu.create_pr")).icon(icon("external-link"));
    const open = (root) => () => actions.open_pull_request(list, key, id, root);
    const repositories = group.repositories;
    if (repositories.length === 0) pull_request = pull_request.disabled(true);
    else if (repositories.length === 1) pull_request = pull_request.on_click(open(repositories[0].worktree));
    else {
      pull_request = pull_request.items(
        repositories.map((repository) => SmeltMenuItem.new(repository.name).on_click(open(repository.worktree))),
      );
    }
    const check = SmeltMenuItem.new(group.checking ? t("menu.checking_merge") : t("menu.check_merge"))
      .icon(icon("refresh"))
      .disabled(group.checking)
      .on_click(() => actions.check_workset(list, key, id));
    const finish = SmeltMenuItem.new(t("menu.finish_workset"))
      .icon(icon("close"))
      .on_click(() => actions.finish_workset(list, view.ask, key, id, group.task));
    return sections([pin, rename], [pull_request, check, finish]);
  }

  /**
   * 「筛选与分组」：分组依据（项目、状态、时间）、显示哪些项目（全部、有会话），挂着远程机器时
   * 再加「按机器分开」；当前的打勾。
   */
  function grouping() {
    const group = (mode, label) =>
      SmeltMenuItem.new(label)
        .checked(sidebar.grouping === mode)
        .on_click(() => list.set_grouping(mode));
    const show = (hide, label) =>
      SmeltMenuItem.new(label)
        .checked(sidebar.hide_empty === hide)
        .on_click(() => list.set_hide_empty_projects(hide));
    const entries = [
      SmeltMenuLabel.new(t("grouping.title")),
      group("project", t("grouping.project")),
      group("status", t("grouping.status")),
      group("last_updated", t("grouping.time")),
      separator(),
      SmeltMenuLabel.new(t("grouping.show")),
      show(false, t("grouping.all")),
      show(true, t("grouping.with_sessions")),
    ];
    if (!sidebar.has_remote()) return entries;
    const by_machine = sidebar.by_machine;
    return [
      ...entries,
      separator(),
      SmeltMenuItem.new(t("grouping.by_machine"))
        .checked(by_machine)
        .on_click(() => list.set_by_machine(!by_machine)),
    ];
  }

  /**
   * 「项目」标题上的机器菜单。挂着远程机器时每台机器一个子菜单（没有项的不列）；只有本机时直接是
   * 本机的项。
   */
  function machines() {
    if (!sidebar.has_remote()) return machine(null);
    return sidebar.machines.flatMap((info) => {
      const items = machine(info.machine);
      if (items.length === 0) return [];
      return [SmeltMenuItem.new(info.name).icon(info.os_icon).icon_color(light(theme, info.link)).items(items)];
    });
  }

  /** 挂着远程机器时「添加项目」先选在哪台机器上打开。连不上的机器挑不了目录，灰着。 */
  function open_project() {
    return [
      SmeltMenuLabel.new(t("menu.open_on_machine")),
      ...sidebar.machines.map((info) =>
        SmeltMenuItem.new(info.name)
          .icon(info.os_icon)
          .icon_color(light(theme, info.link))
          .disabled(info.link.unavailable != null)
          .on_click(() => actions.open_project(list, info.machine)),
      ),
    ];
  }

  return { session, project, workset, machine, machines, open_project, grouping };
}
