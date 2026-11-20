// 归档：各机器上已归档的对话，按机器、项目分组，恢复或彻底删除。
//
// 一个视图：侧栏底下「设置」旁边的归档按钮，哪台机器上有归档才露出来，点开是一张弹出卡片列出全部归档。
// 不占启动栏的入口，也不开整页；布局把它摆在底下那条里。
//
// 归档是运行时工作区目录里对话的一个状态（archived_at），权威在运行时；这里只从宿主转来的目录现算分组，
// 不另存一份。恢复、删除发运行时的单条目录命令，改完运行时推整份目录，列表随推送变。恢复成功后用会话
// 插件的 smelt.sessions/activate 跳过去。
//
// 什么时候重算全靠事件：目录或会话变了、机器变了、换了语言。按钮露不露要看有没有归档，所以一直跟着算，
// 不等卡片打开。算好的分组留在模块里，render 只读它。

import { div, svg, View } from "gpui-kit";
import { DockArea } from "gpui-base";
import * as api from "smelt-plugin-api";
import {
  SmeltButton,
  SmeltDialog,
  SmeltListRow,
  SmeltPopover,
  SmeltRadio,
  SmeltTooltip,
  SmeltTruncatedLine,
} from "smelt-ui";

import { BUTTON, BUTTON_NAME } from "./contract.js";
import { busy_key, machine_group, same_state } from "./model.js";

// 约定：smelt.sessions 的 ACTIVATE，权威定义在 crates/plugins/sessions/src/contract.rs。在主窗口里露出
// 这个会话，参数 { machine, session_id, after? }：after 是恢复那次回的目录版本，目录推到那一版之前会话
// 插件等着，不会因为目录还没推到就找不到。
const ACTIVATE = "smelt.sessions/activate";

/** 卡片宽度：侧栏底下往上弹，标题和悬停露出的两个按钮放得下。 */
const POPOVER_WIDTH = 320;
/** 卡片里列表的最高高度，多了在里面滚。 */
const LIST_MAX_HEIGHT = 420;
/** 选工作树对话框里列表的最高高度。 */
const CHOICES_MAX_HEIGHT = 240;
/** 确认框宽度（smelt-ui 的 Confirm）。 */
const CONFIRM_WIDTH = 440;
/** 悬停一行才露出恢复、删除按钮。 */
const ROW_GROUP = "archive-row";

const path = {
  clean: api.path_clean,
  parent: api.path_parent,
  project_root_of: api.project_root_of,
  labels: api.project_labels,
};

/** 有归档的机器，各自的分组；一台都没有时按钮不露。 */
let machines = [];
/** 正在恢复、删除的（busy_key）：按钮先禁用。成了推送会把它从列表里拿走。 */
const busy = new Set();
/** 开着的视图，状态变了叫它们重画。 */
const views = new Set();

function redraw() {
  for (const view of [...views]) view.redraw();
}

/** 从各机器的工作区目录重算分组。目录是整份的，只在事件里做，不在 render 里。 */
function rebuild() {
  const now = Math.floor(Date.now() / 1000);
  const next = [];
  for (const machine of api.machines()) {
    const directory = api.workspace_directory(machine.machine);
    if (directory == null) continue;
    const group = machine_group(path, api.t, machine, api.path_style(machine.machine), directory, now);
    if (group != null) next.push(group);
  }
  if (!same_state(next, machines)) {
    machines = next;
    redraw();
  }
}

/** 套上 theme().type 里的一种字号：字号、行高、字重一起给。 */
function typed(el, style) {
  return el.text_size(style.text_size).line_height(style.line_height).font_weight(style.font_weight);
}

const message = (error) => error?.message ?? String(error);

/** 失败了报出来：没有落点（卡片已收起），发系统通知。 */
function notify_error(text) {
  console.error(`[archive] ${text}`);
  api.run("smelt.app/notify", { subtitle: api.t("notify.error"), body: text }).catch(() => {});
}

/** 发恢复；成了就跳到那段会话。target_workset_id 是原工作树已关时选的那个。 */
async function restore(machine, row, target_workset_id) {
  const key = busy_key(machine, row.id);
  if (busy.has(key)) return;
  busy.add(key);
  redraw();
  let revision;
  try {
    revision = await api.call(machine, "workspace.conversation.archive", {
      conversation_id: row.id,
      archived: false,
      target_workset_id,
    });
  } catch (error) {
    notify_error(api.t("restore.failed", { title: row.title, error: message(error) }));
    return;
  } finally {
    busy.delete(key);
    redraw();
  }
  // 终端还没起过的没有会话 id：恢复了就留在会话列表里，不跳。
  if (row.session_id != null) {
    api
      .run(ACTIVATE, { machine, session_id: row.session_id, after: revision })
      .catch((error) => notify_error(message(error)));
  }
}

async function remove(machine, row) {
  const key = busy_key(machine, row.id);
  if (busy.has(key)) return;
  busy.add(key);
  redraw();
  try {
    await api.call(machine, "workspace.conversation.delete", { conversation_id: row.id });
  } catch (error) {
    notify_error(api.t("delete.failed", { title: row.title, error: message(error) }));
  } finally {
    busy.delete(key);
    redraw();
  }
}

class ArchiveButton extends View {
  init() {
    this.instance = api.opened_view().instance;
    /** 卡片开着。 */
    this.popover_open = false;
    /**
     * 正在问的：{ kind: "delete", machine, row } 彻底删除先确认；{ kind: "restore", machine, row, picked }
     * 原工作树已关，选一个同一项目下还在的工作树（picked 是选中的 id）。
     */
    this.dialog = null;
    views.add(this);
  }

  redraw() {
    if (!api.notify_view(this.instance)) views.delete(this);
  }

  // ---- 动作 ----

  /** 恢复、删除前先收起卡片：恢复了要跳到那段会话，要确认的对话框也不该压在卡片上。 */
  request_restore(machine, row) {
    this.popover_open = false;
    if (row.closed_workset != null) {
      this.dialog = { kind: "restore", machine, row, picked: row.closed_workset.candidates[0]?.id ?? null };
    } else {
      restore(machine, row, null);
    }
    this.redraw();
  }

  request_delete(machine, row) {
    this.popover_open = false;
    this.dialog = { kind: "delete", machine, row };
    this.redraw();
  }

  close_dialog(dialog) {
    if (this.dialog !== dialog) return;
    this.dialog = null;
    this.redraw();
  }

  // ---- 画 ----

  render() {
    const look = api.theme();
    const bar = div().h_full().flex().items_center();
    if (machines.length === 0) return bar.when(this.dialog != null, (bar) => bar.child(this.dialog_view(look)));
    // 贴着窗口下面的角时给窗口圆角让位。
    const insets = api.safe_area(BUTTON).bar_insets;
    const title = api.t("title");
    return bar
      .pl(insets.left)
      .pr(insets.right)
      .child(
        SmeltPopover.new("archive-popover")
          .side("bottom_left")
          .w(POPOVER_WIDTH)
          .p_1()
          .open(this.popover_open)
          .on_open_change((open) => {
            this.popover_open = open;
            this.redraw();
          })
          .trigger(
            SmeltButton.new("archive-button")
              .quiet()
              .icon("icons/archive.svg")
              .accessibility_label(title)
              .tooltip(title),
          )
          .when(this.popover_open, (popover) => popover.content(this.card(look))),
      )
      .when(this.dialog != null, (bar) => bar.child(this.dialog_view(look)));
  }

  /** 卡片内容：标题，下面是能滚的列表。 */
  card(look) {
    const p = look.palette;
    // 只有一台机器有归档时不画机器那一层。
    const by_machine = machines.length > 1;
    let count = 0;
    for (const machine of machines) for (const project of machine.projects) count += project.rows.length;
    return div()
      .flex()
      .flex_col()
      .w_full()
      .child(
        div()
          .flex()
          .items_center()
          .gap_2()
          .px_2()
          .pt_1()
          .pb_1p5()
          .child(typed(div(), look.type.ui_medium).text_color(p.text).child(api.t("title")))
          .child(typed(div(), look.type.text_xsmall).text_color(p.muted).child(String(count))),
      )
      .child(
        div()
          .id("archive-scroll")
          .max_h(LIST_MAX_HEIGHT)
          .overflow_y_scroll()
          .child(
            div()
              .flex()
              .flex_col()
              .w_full()
              .gap_2()
              .children(machines.map((machine) => this.machine_section(look, machine, by_machine))),
          ),
      );
  }

  machine_section(look, machine, by_machine) {
    const p = look.palette;
    const header = div()
      .flex()
      .items_center()
      .gap_2()
      .px_2()
      .child(typed(div(), look.type.text_xsmall_medium).text_color(p.text).child(machine.name))
      .when(machine.unavailable != null, (row) =>
        row.child(typed(div(), look.type.text_xsmall).text_color(p.faint).child(machine.unavailable)),
      );
    return div()
      .flex()
      .flex_col()
      .w_full()
      .gap_2()
      .when(by_machine, (section) => section.child(header))
      .children(machine.projects.map((project) => this.project_group(look, machine.machine, project)));
  }

  project_group(look, machine, project) {
    const header = typed(
      SmeltTooltip.new(`archive-project-${machine ?? "local"}-${project.root}`, project.root),
      look.type.text_xsmall,
    )
      .px_2()
      .text_color(look.palette.muted)
      .overflow_hidden()
      .child(SmeltTruncatedLine.new(project.label));
    return div()
      .flex()
      .flex_col()
      .w_full()
      .gap(1)
      .child(header)
      .children(project.rows.map((row) => this.row(look, machine, row)));
  }

  row(look, machine, row) {
    const p = look.palette;
    const key = `archive-${machine ?? "local"}-${row.id}`;
    const is_busy = busy.has(busy_key(machine, row.id));
    const restore_label = api.t("action.restore");
    const delete_label = api.t("action.delete");
    return SmeltListRow.new(key)
      .group(ROW_GROUP)
      .w_full()
      .px_2()
      .py_1()
      .rounded(look.radius.row)
      .flex()
      .items_center()
      .gap_2()
      .disabled(is_busy)
      .child(
        div()
          .flex_shrink_0()
          .child(
            svg(row.is_conversation ? "icons/message-square.svg" : "icons/terminal.svg")
              .size(15)
              .text_color(p.muted),
          ),
      )
      .child(
        div()
          .flex_1()
          .min_w_0()
          .overflow_hidden()
          .child(typed(div(), look.type.ui).child(SmeltTruncatedLine.new(row.title)))
          .when(row.when !== "", (column) =>
            column.child(
              typed(div(), look.type.text_xsmall).text_color(p.muted).child(SmeltTruncatedLine.new(row.when)),
            ),
          ),
      )
      .child(
        // 只改透明度（不改布局），套在外层的 div 上：组件自己不认悬停样式。
        div()
          .flex()
          .items_center()
          .flex_shrink_0()
          .gap_1()
          .opacity(0)
          .group_hover(ROW_GROUP, (style) => style.opacity(1))
          .child(
            SmeltButton.new(`${key}-restore`)
              .quiet()
              .small()
              .icon("icons/restore.svg")
              .accessibility_label(restore_label)
              .tooltip(restore_label)
              .disabled(is_busy)
              .on_click(() => this.request_restore(machine, row)),
          )
          .child(
            SmeltButton.new(`${key}-delete`)
              .dismiss()
              .icon("icons/trash.svg")
              .accessibility_label(delete_label)
              .tooltip(delete_label)
              .disabled(is_busy)
              .on_click(() => this.request_delete(machine, row)),
          ),
      );
  }

  /** 正在问的对话框：取消在左，动作在右。 */
  dialog_view(look) {
    const dialog = this.dialog;
    const close = () => this.close_dialog(dialog);
    const danger = dialog.kind === "delete";
    // 危险的确认 Enter 只关框不执行，要点按钮；选工作树的没选中不关。
    const confirm = () => {
      if (dialog.kind === "delete") {
        close();
        remove(dialog.machine, dialog.row);
      } else if (dialog.picked != null) {
        close();
        restore(dialog.machine, dialog.row, dialog.picked);
      }
    };
    const action = SmeltButton.new("dialog-confirm")
      .label(api.t(danger ? "action.delete" : "action.restore"))
      .on_click(confirm);
    const card = SmeltDialog.new("archive-dialog")
      .open(true)
      .w(CONFIRM_WIDTH)
      .close_on_backdrop(false)
      .footer(
        div()
          .w_full()
          .flex()
          .justify_end()
          .gap_2()
          .child(SmeltButton.new("dialog-cancel").quiet().label(api.t("action.cancel")).on_click(close))
          .child(danger ? action.danger() : action.primary()),
      )
      .on_confirm(() => (danger ? close() : confirm()))
      .on_dismiss(close);
    if (danger) {
      return card.title(api.t("delete.confirm")).description(api.t("delete.description", { title: dialog.row.title }));
    }
    return card
      .title(api.t("restore.closed_title"))
      .description(api.t("restore.closed_description", { root: dialog.row.closed_workset.old_root }))
      .content(this.workset_choices(look, dialog));
  }

  /** 选工作树对话框的正文：同一项目下还在的工作树，单选。没有就说清楚去哪建。 */
  workset_choices(look, dialog) {
    const candidates = dialog.row.closed_workset.candidates;
    if (candidates.length === 0) {
      return typed(div(), look.type.ui).text_color(look.palette.muted).child(api.t("restore.no_worksets"));
    }
    return div()
      .id("archive-workset-choices")
      .flex()
      .flex_col()
      .gap_2()
      .max_h(CHOICES_MAX_HEIGHT)
      .overflow_y_scroll()
      .children(
        candidates.map((candidate) =>
          SmeltRadio.new(`archive-workset-${candidate.id}`)
            .label(candidate.label)
            .checked(dialog.picked === candidate.id)
            .on_select(() => {
              dialog.picked = candidate.id;
              this.redraw();
            }),
        ),
      );
  }
}

/** 要据此重算的事件：行上的标题、时间是按当时的语言算好的。 */
const AFFECTS = new Set(["sessions_changed", "machines_changed", "locale_changed"]);

async function follow_events() {
  for (;;) {
    const event = await api.next_event();
    try {
      if (AFFECTS.has(event.type)) rebuild();
      // 两头的边距在 render 里现读 safe_area。
      else if (event.type === "safe_area_changed") redraw();
    } catch (error) {
      console.error(`[archive] 处理宿主事件 ${event.type} 失败：`, error);
    }
  }
}

export default class Root extends View {
  init() {
    DockArea.register_panel(BUTTON_NAME, ArchiveButton);
    // 侧栏底下那条里的一个按钮，不要外壳的标题带。
    api.register_views([
      { name: BUTTON_NAME, title: api.text("view.title"), icon: "icons/archive.svg", draws_top_bar: true },
    ]);
    rebuild();
    follow_events();
  }

  render() {
    return div();
  }
}
