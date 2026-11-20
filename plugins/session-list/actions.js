// 列表上各种行的动作：置顶、拖动换位置、关闭、归档、关工作树、打开项目（挑目录、从访达拖进来）。
// 菜单和行上的按钮都调这里。要确认的（会话还在跑、项目里还有会话、关工作树）在这里先问：ask 由
// 列表视图给，弹确认框，{ title, action, description, danger, confirm }，点了确认才调 confirm()。
//
// 不 import 宿主模块，在 Node 里就能测。list 是 state.js 的 SessionList。

import { is_pinned } from "./model.js";
import { merge_project_order, move_near, move_to_pin_boundary, splice_group } from "./sidebar.js";
import * as peers from "./peers.js";
import { describe, merge_of_result, WORKSET_GIT_TIMEOUT } from "./worksets.js";

const same_project = (a, b) => a.machine === b.machine && a.root === b.root;

// ---- 置顶 ----

/**
 * 置顶或取消置顶一个项目，并把它挪到置顶区和其余的交界（见 move_to_pin_boundary）。置顶区不分
 * 机器：所有机器的项目排在一份列表里。
 */
export function pin_project(list, machine, root, pinned) {
  const item = { machine, root };
  const shown = move_to_pin_boundary(
    list.sidebar.project_keys(),
    item,
    (other) => (same_project(other, item) ? pinned : is_pinned(list.ui, { kind: "project", ...other })),
    same_project,
  );
  const order = merge_project_order(list.ui.project_order, shown, same_project);
  list.set_pinned({ kind: "project", machine, root }, pinned);
  list.set_project_order(order);
}

/** 置顶或取消置顶一段会话。项目里的会话同样挪到交界；「对话」一段按最近动静排，不记手动顺序，只改置顶。 */
export function pin_session(list, key, group, pinned) {
  let order = null;
  if (group.kind === "project") {
    const ids = list.sidebar.group_rows(group).map((row) => row.key.id);
    const moved = move_to_pin_boundary(ids, key.id, (other) =>
      other === key.id ? pinned : is_pinned(list.ui, { kind: "conversation", machine: group.machine, id: other }),
    );
    order = splice_group(list.sidebar.conversation_ids(group.machine), moved);
  }
  list.set_pinned({ kind: "conversation", machine: key.machine, id: key.id }, pinned);
  if (order) list.set_conversation_order(key.machine, order);
}

export function pin_workset(list, machine, id, pinned) {
  list.set_pinned({ kind: "workset", machine, id }, pinned);
}

// ---- 拖动 ----

/**
 * 把项目 from 放到 to 前面或后面，哪台机器上的都行（所有机器的项目排在一份列表里）。跨过置顶区
 * 和其余的交界就跟着 to 置顶或取消置顶。
 */
export function drop_project(list, from, to, before) {
  const shown = list.sidebar.project_keys();
  if (!move_near(shown, from, to, before, same_project)) return;
  const pinned = is_pinned(list.ui, { kind: "project", ...to });
  const order = merge_project_order(list.ui.project_order, shown, same_project);
  list.set_pinned({ kind: "project", machine: from.machine, root: from.root }, pinned);
  list.set_project_order(order);
}

/** 把会话 from 放到同一组里 to 的前面或后面，规则同 drop_project。 */
export function drop_session(list, from, to, group, before) {
  if (group.kind !== "project") return;
  const ids = list.sidebar.group_rows(group).map((row) => row.key.id);
  if (!move_near(ids, from.id, to.id, before)) return;
  const order = splice_group(list.sidebar.conversation_ids(group.machine), ids);
  const pinned = is_pinned(list.ui, { kind: "conversation", machine: to.machine, id: to.id });
  list.set_pinned({ kind: "conversation", machine: from.machine, id: from.id }, pinned);
  list.set_conversation_order(group.machine, order);
}

// ---- 关闭、归档 ----

/** 关掉一段会话。还在跑、在等人的先问一句。 */
export function close_session(list, ask, key, title, status) {
  const t = list.host.t;
  const close = () =>
    list.report_row_failure({ kind: "conversation", key }, t("action.close_session"), list.close_conversation(key));
  if (status === "idle") return close();
  ask({
    title: t("close.title", { title }),
    action: t("close.confirm"),
    description: t("close.running_hint"),
    danger: true,
    confirm: close,
  });
}

/** 归档一段会话（去归档里还找得回来）。还在跑、在等人的先问一句。 */
export function archive_session(list, ask, key, title, status) {
  const t = list.host.t;
  const archive = () =>
    list.report_row_failure({ kind: "conversation", key }, t("action.archive_session"), list.archive_conversation(key));
  if (status === "idle") return archive();
  ask({
    title: t("archive.title", { title }),
    action: t("archive.confirm"),
    description: t("archive.running_hint"),
    danger: false,
    confirm: archive,
  });
}

/** 关掉项目，连同下面的会话。里面有会话的先问一句。 */
export function close_project(list, ask, machine, root, name, sessions) {
  const t = list.host.t;
  const close = () =>
    list.report_row_failure(
      { kind: "project", machine, root },
      t("action.close_project"),
      list.close_project(machine, root),
    );
  if (sessions === 0) return close();
  ask({
    title: t("close_project.title", { name }),
    action: t("close.confirm"),
    description:
      sessions === 1 ? t("close_project.sessions.one") : t("close_project.sessions.other", { count: sessions }),
    danger: true,
    confirm: close,
  });
}

/**
 * 关掉工作树：先问运行时各仓库的情况（会 fetch 基线），再按结果确认。都合进去了是普通的确认；有
 * 判断不了是否已合并的，说清楚哪个仓库、让人决定要不要仍然关闭；有未提交改动、确实没合并的直接
 * 说为什么关不了（挂在工作树那一行上）。运行时移除目录，下面的会话回到项目里。
 */
export async function finish_workset(list, ask, machine, id, task) {
  const t = list.host.t;
  const target = { kind: "workset", machine, id };
  list.set_failure(target, null);
  let result = null;
  try {
    result = await list.inspect(machine, id);
  } catch {
    // 查不了（运行时还没有这条命令、Git 登记对不上……）：照旧问一句，关的时候运行时会再查。
  }
  const moves = t("workset.finish_moves");
  const summary = result ? merge_of_result(result) : "unknown";
  const lines = result ? describe(result, t).join("\n") : "";
  const finish = (allow_unconfirmed_merge) => () => finish_now(list, machine, id, allow_unconfirmed_merge);
  if (summary === "pending") {
    list.fail_row(target, t("action.finish_workset"), lines);
  } else if (summary === "unconfirmed") {
    ask({
      title: t("workset.finish_anyway_title", { task }),
      action: t("workset.finish_anyway"),
      description: `${lines}\n\n${t("workset.unconfirmed_hint")} ${moves}`,
      danger: true,
      confirm: finish(true),
    });
  } else {
    ask({
      title: t("workset.finish_title", { task }),
      action: t("close.confirm"),
      description: lines === "" ? moves : `${lines}\n\n${moves}`,
      danger: true,
      confirm: finish(false),
    });
  }
}

/** 真的关：运行时留下了分支、目录里有没删掉的东西时发一条通知说一声。 */
export async function finish_now(list, machine, id, allow_unconfirmed_merge) {
  const { t, call, run } = list.host;
  let result;
  try {
    result = await call(
      machine,
      "workspace.workset.finish",
      { workset_id: id, allow_unconfirmed_merge },
      WORKSET_GIT_TIMEOUT,
    );
  } catch (error) {
    list.fail_row({ kind: "workset", machine, id }, t("action.finish_workset"), message(error));
    return;
  }
  const notes = [];
  const separator = t("list.separator");
  if (result.retained_branches.length > 0) {
    notes.push(t("workset.retained_branches", { branches: result.retained_branches.join(separator) }));
  }
  if (result.leftover_entries.length > 0) {
    notes.push(t("workset.leftover_entries", { entries: result.leftover_entries.join(separator) }));
  }
  if (notes.length > 0) {
    run("smelt.app/notify", { subtitle: t("workset.finished"), body: notes.join("\n") }).catch((error) =>
      console.error(`[session-list] 发通知失败：${message(error)}`),
    );
  }
}

/** 菜单里的「检查合并状态」：马上问一次，结果画在工作树行上；查失败了也挂在那一行上。 */
export function check_workset(list, machine, id) {
  list.report_row_failure({ kind: "workset", machine, id }, list.host.t("action.check_merge"), list.inspect(machine, id));
}

/** 工作树里某个仓库的新建 PR 页，交给变更插件开（它认得远端是哪家托管）。 */
export function open_pull_request(list, machine, id, root) {
  list.report_row_failure(
    { kind: "workset", machine, id },
    list.host.t("action.create_pr"),
    list.host.run(peers.OPEN_PULL_REQUEST, { machine, root }),
  );
}

// ---- 打开项目 ----

/**
 * 在那台机器上打开项目：本机是系统选择框（能多选），远程机器在宿主画的浏览框里挑一个目录。
 */
export function open_project(list, machine) {
  const t = list.host.t;
  if (machine == null) {
    list.report_failure(t("action.add_project"), list.pick_projects());
    return;
  }
  pick_project(list, machine);
}

async function pick_project(list, machine) {
  const { t, run } = list.host;
  let picked;
  try {
    picked = await run("smelt.app/pick-paths", {
      machine,
      title: t("pick.title"),
      confirm: t("pick.confirm"),
      files: false,
      multiple: false,
    });
  } catch (error) {
    list.notify_error(list.failed(t("action.open_project"), message(error)));
    return;
  }
  const root = picked?.[0];
  if (root == null) return;
  list.report_failure(t("action.add_project"), list.open_project(machine, root));
}

/**
 * 从访达拖进列表的路径：目录就是项目，文件取它所在的目录，去重后在本机逐个打开。一个能用的都
 * 没有时报给人看。
 */
export async function open_dropped_paths(list, paths) {
  const host = list.host;
  const roots = [];
  for (const path of paths) {
    const kind = await host.path_kind(path).catch(() => null);
    const dir = kind === "dir" ? path : kind === "file" ? host.parent_dir(path) : null;
    if (dir != null && !roots.includes(dir)) roots.push(dir);
  }
  if (roots.length === 0) {
    list.notify_error(host.t("error.drop_not_folder"));
    return;
  }
  for (const root of roots) list.report_failure(host.t("action.add_project"), list.open_project(null, root));
}

function message(error) {
  return error?.message ?? String(error);
}
