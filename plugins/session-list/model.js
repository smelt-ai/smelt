// 会话列表的界面状态（置顶、顺序、折叠、分组）和从工作区目录算东西的纯函数。不 import 宿主
// 模块，在 Node 里就能测。
//
// 权威在哪：
// - 有哪些会话、项目、工作集：那台机器的工作区目录（workspace_directory），运行时是唯一写入方。
//   这里不另存，用的时候现取。会话摆在哪（placement）、状态、名字是能力层算好的派生事实
//   （conversation_facts）。
// - 置顶、手动顺序、折叠、行首头像用哪个：本插件的库（store.js）。SidebarUi 是它的内存缓存，
//   启动时读一次，改的时候同一事件里写库。分组依据、隐藏空项目、按机器分开只在内存里。
// - 选中哪段会话或哪个项目：会话插件（smelt.sessions），这里是它的只读缓存。

/** 机器在 Map、Set 里的键：本机是 ""，远程是 runtime id（同库里的写法）。 */
export function machine_key(machine) {
  return machine ?? "";
}

/** 键回到机器（本机 null）。 */
export function machine_of_key(key) {
  return key === "" ? null : key;
}

/** 机器在滑动身份里的写法。 */
export function machine_id(machine) {
  return machine ?? "@local";
}

/** 同一段会话（机器 + 目录里的对话 id）。 */
export function same_conversation(a, b) {
  return !!a && !!b && a.machine === b.machine && a.id === b.id;
}

// ---- 侧栏的界面状态 ----

/** 一台机器上的侧栏状态。键都指着那台机器的工作区目录。 */
export function new_machine_ui() {
  return {
    /** 置顶的会话（目录里的对话 id），按置顶的先后。 */
    pinned_conversations: [],
    /** 置顶的项目（根目录），按置顶的先后。 */
    pinned_projects: [],
    /** 置顶的工作集，在项目里排前面。 */
    pinned_worksets: new Set(),
    /** 用户拖出来的会话顺序；不在里面的按默认顺序接在后面。 */
    conversation_order: [],
    collapsed_projects: new Set(),
    collapsed_worksets: new Set(),
  };
}

/**
 * 侧栏的置顶、顺序、折叠，所有机器的；再加上「筛选与分组」里选的那几样。
 * - grouping：项目里的会话按什么分组，"project" | "status" | "last_updated"。只管项目里的
 *   会话，「对话」一段不受它影响。
 * - hide_empty_projects：按项目分组时藏起没有会话、也没有工作树的项目（置顶的、选中的照常显示）。
 * - collapsed_buckets：按状态、时间分组时折起来的组（bucket_flip 的写法）。只在内存里。
 * - by_machine：挂着远程机器时一台机器一片。只在内存里。
 * - collapsed_machines：按机器分开时折起来的机器（machine_key）。只在内存里。
 */
export function new_sidebar_ui() {
  return {
    machines: new Map(),
    /** 用户拖出来的项目顺序，所有机器一份：[{ machine, root }]。 */
    project_order: [],
    conversations_collapsed: false,
    grouping: "project",
    hide_empty_projects: false,
    collapsed_buckets: new Set(),
    by_machine: false,
    collapsed_machines: new Set(),
  };
}

/** 那台机器的状态；没动过是 undefined（按全默认画）。 */
export function machine_ui(ui, machine) {
  return ui.machines.get(machine_key(machine));
}

function machine_entry(ui, machine) {
  const key = machine_key(machine);
  let entry = ui.machines.get(key);
  if (!entry) {
    entry = new_machine_ui();
    ui.machines.set(key, entry);
  }
  return entry;
}

/**
 * 能置顶的东西：
 * { kind: "conversation", machine, id } | { kind: "project", machine, root } | { kind: "workset", machine, id }
 */
export function is_pinned(ui, pin) {
  const entry = machine_ui(ui, pin.machine);
  if (!entry) return false;
  switch (pin.kind) {
    case "conversation":
      return entry.pinned_conversations.includes(pin.id);
    case "project":
      return entry.pinned_projects.includes(pin.root);
    case "workset":
      return entry.pinned_worksets.has(pin.id);
  }
  return false;
}

/**
 * 能折起来的东西：
 * { kind: "conversations" } | { kind: "project", machine, root } | { kind: "workset", machine, id }
 */
export function is_collapsed(ui, fold) {
  if (fold.kind === "conversations") return ui.conversations_collapsed;
  const entry = machine_ui(ui, fold.machine);
  if (!entry) return false;
  return fold.kind === "project"
    ? entry.collapsed_projects.has(fold.root)
    : entry.collapsed_worksets.has(fold.id);
}

/** 改内存里这份，规则同库（新置顶的排最后）。没变是 false。 */
export function set_pinned(ui, pin, pinned) {
  if (is_pinned(ui, pin) === pinned) return false;
  const entry = machine_entry(ui, pin.machine);
  switch (pin.kind) {
    case "conversation":
      entry.pinned_conversations = toggle_listed(entry.pinned_conversations, pin.id, pinned);
      break;
    case "project":
      entry.pinned_projects = toggle_listed(entry.pinned_projects, pin.root, pinned);
      break;
    case "workset":
      toggle_set(entry.pinned_worksets, pin.id, pinned);
      break;
  }
  return true;
}

export function set_collapsed(ui, fold, collapsed) {
  if (is_collapsed(ui, fold) === collapsed) return false;
  if (fold.kind === "conversations") {
    ui.conversations_collapsed = collapsed;
    return true;
  }
  const entry = machine_entry(ui, fold.machine);
  toggle_set(
    fold.kind === "project" ? entry.collapsed_projects : entry.collapsed_worksets,
    fold.kind === "project" ? fold.root : fold.id,
    collapsed,
  );
  return true;
}

/** 换掉那台机器上会话的手动顺序（整份）。没变是 false。 */
export function set_conversation_order(ui, machine, ids) {
  const entry = machine_entry(ui, machine);
  if (same_list(entry.conversation_order, ids)) return false;
  entry.conversation_order = [...ids];
  return true;
}

/** 换掉项目的手动顺序（所有机器一份）。没变是 false。 */
export function set_project_order(ui, order) {
  const same =
    ui.project_order.length === order.length &&
    ui.project_order.every((key, ix) => key.machine === order[ix].machine && key.root === order[ix].root);
  if (same) return false;
  ui.project_order = order.map(({ machine, root }) => ({ machine, root }));
  return true;
}

/** 清掉指着目录里已经没有的东西的状态（known 见 known_ids）。 */
export function prune_ui(ui, machine, known) {
  ui.project_order = ui.project_order.filter(
    (key) => key.machine !== machine || known.roots.has(key.root),
  );
  const entry = machine_ui(ui, machine);
  if (!entry) return;
  entry.pinned_conversations = entry.pinned_conversations.filter((id) => known.conversations.has(id));
  entry.conversation_order = entry.conversation_order.filter((id) => known.conversations.has(id));
  entry.pinned_projects = entry.pinned_projects.filter((root) => known.roots.has(root));
  retain(entry.collapsed_projects, (root) => known.roots.has(root));
  retain(entry.pinned_worksets, (id) => known.worksets.has(id));
  retain(entry.collapsed_worksets, (id) => known.worksets.has(id));
}

/** 机器移除了：它的置顶、顺序、折叠全删。只对真的从机器表里消失的调，不对离线的调。 */
export function forget_machine_ui(ui, machine) {
  ui.machines.delete(machine_key(machine));
  ui.project_order = ui.project_order.filter((key) => key.machine !== machine);
  ui.collapsed_machines.delete(machine_key(machine));
  const prefix = `bucket:${machine_id(machine)}:`;
  retain(ui.collapsed_buckets, (key) => !key.startsWith(prefix));
}

function toggle_listed(list, key, on) {
  return on ? [...list, key] : list.filter((item) => item !== key);
}

function toggle_set(set, key, on) {
  if (on) set.add(key);
  else set.delete(key);
}

function retain(set, keep) {
  for (const item of [...set]) if (!keep(item)) set.delete(item);
}

function same_list(a, b) {
  return a.length === b.length && a.every((item, ix) => item === b[ix]);
}

/**
 * 盖在一段会话上面、折起来就看不见它的那几层：「对话」段，或者（按项目分组时）它的项目和
 * 工作集。按状态、时间分组时项目、工作集不画，不算。
 */
export function folds_over(machine, placement, grouping) {
  switch (placement.kind) {
    case "hidden":
      return [];
    case "conversations":
      return [{ kind: "conversations" }];
    case "project": {
      if (grouping !== "project") return [];
      const folds = [{ kind: "project", machine, root: placement.root }];
      if (placement.workset != null) folds.push({ kind: "workset", machine, id: placement.workset });
      return folds;
    }
  }
  return [];
}

/**
 * 行首头像用哪个：{ kind: "unset" }（没选过，用第一个填进来的）| { kind: "icon" }（不要头像）|
 * { kind: "view", id }（用这个视图；它没了按没选过算，选择留着）。
 * 在填了插槽的视图 id（按登记顺序）里定用哪个；null 是画图标。
 */
export function pick_avatar(choice, filled) {
  switch (choice.kind) {
    case "icon":
      return null;
    case "view":
      return filled.find((id) => id === choice.id) ?? filled[0] ?? null;
    default:
      return filled[0] ?? null;
  }
}

export function same_avatar_choice(a, b) {
  return a.kind === b.kind && (a.kind !== "view" || a.id === b.id);
}

// ---- 从目录和会话状态算的 ----

/** 这段对话里的守护会话 id：对话的那个，再是每一格终端（还没起过、没有 id 的不算）。 */
export function session_ids(conversation) {
  const ids = [];
  if (conversation.acp) ids.push(conversation.acp.sid);
  for (const terminal of conversation.terminals ?? []) {
    if (terminal.id != null) ids.push(terminal.id);
  }
  return ids;
}

/** states 里属于这段对话的那些。 */
export function states_in(conversation, states) {
  const ids = session_ids(conversation);
  return states.filter((state) => ids.includes(state.id));
}

/**
 * 最近一次动静（unix 秒）：各格最近一次换阶段、开始一轮里最晚的。守护里还没有它的是 0。
 * 「对话」那一段按它排。
 */
export function last_activity(conversation, states) {
  let at = 0;
  for (const state of states_in(conversation, states)) {
    at = Math.max(at, state.phase_since ?? 0, state.turn_started_at ?? 0);
  }
  return at;
}

/** 一台机器的目录里还在的东西，清理库里的行用。facts 是那台机器的 conversation_facts。 */
export function known_ids(directory, facts) {
  const roots = new Set(directory.projects);
  for (const fact of facts) {
    if (fact.placement.kind === "project") roots.add(fact.placement.root);
  }
  return {
    conversations: new Set(directory.conversations.map((conversation) => conversation.id)),
    roots,
    worksets: new Set(directory.worksets.map((workset) => workset.id)),
  };
}

/**
 * 列表里这台机器上按守护会话 id 找到的那段会话：目录里有、在侧栏里（不是归档的、自动化
 * 运行）。没有目录、找不到是 null。
 */
export function listed_session(machine, session_id, directory, facts) {
  if (!directory) return null;
  const ix = directory.conversations.findIndex((conversation) =>
    session_ids(conversation).includes(session_id),
  );
  if (ix < 0 || facts[ix]?.placement.kind === "hidden") return null;
  return { machine, id: directory.conversations[ix].id };
}

/**
 * 选中的是什么（会话插件那份的缓存）。线上是 `{kind: "none"}`、
 * `{kind: "conversation", machine, conversation_id}`、`{kind: "project", machine, root}`；
 * 这里换成 `{kind: "none"}`、`{kind: "session", key: {machine, id}}`、`{kind: "project", machine, root}`。
 */
export function selection_from_wire(selected) {
  switch (selected?.kind) {
    case "conversation":
      return { kind: "session", key: { machine: selected.machine ?? null, id: selected.conversation_id } };
    case "project":
      return { kind: "project", machine: selected.machine ?? null, root: selected.root };
    default:
      return { kind: "none" };
  }
}

/** 选中的会话；不是会话是 null。 */
export function selected_session(selection) {
  return selection.kind === "session" ? selection.key : null;
}

/** 给人看的名字：派生事实里的名字（运行时的或用户起的），没有就是通用名。 */
export function title(fact, has_acp, t) {
  return fact.name ?? (has_acp ? t("kind.conversation") : t("kind.terminal"));
}

/** 状态的优先级（越小越紧急）。 */
const RANK = { needs_you: 0, running: 1, idle: 2 };

/** 最要紧的那个：要你 > 运行中 > 空闲；一个都没有是空闲。 */
export function highest_status(statuses) {
  let best = "idle";
  for (const status of statuses) if (RANK[status] < RANK[best]) best = status;
  return best;
}
