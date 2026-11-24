// 会话列表插件（NSH A2 独立 VM 版）宿主桥与生命周期：
// 零重复复用 `plugins/session-list/` 全部纯 JS 业务逻辑模块，经 `smelt/sdk` 对接宿主状态、
// 事件、双向命令、SQLite 存储（含 smelt.sessions 旧库迁移）与视图登记。

import { signal } from "nsh";
import {
  activeContext,
  call,
  clipboardWrite,
  command,
  connections,
  contextMenuItems,
  conversationAcp,
  conversationFacts,
  dataDir,
  initHost,
  locale,
  localOs,
  machines,
  newSessionId,
  onCommand,
  onEvent,
  onState,
  pathClean,
  pathFileName,
  pathIsAbsolute,
  pathJoin,
  pathKind,
  pathParent,
  pathStartsWith,
  pathStyle,
  projectLabels,
  registerCommands,
  registerTopics,
  registerViews,
  run,
  sessionStates,
  state,
  store as sdkStore,
  t,
  text,
  theme as sdkTheme,
  workspaceDirectory,
} from "smelt/sdk";

import { conversation_key, empty_stage, greet, track } from "../buddy_stage.js";
import {
  FOCUS_LIST,
  LIST_NAME,
  NEW_SESSION,
  NEXT_SESSION,
  OPEN_PROJECT,
  OWNER,
  PREV_SESSION,
  REVEAL_IN_LIST,
  SETTINGS_NAME,
} from "../contract.js";
import { new_sidebar_ui, same_conversation, session_ids } from "../model.js";
import { ATTENTION_TOPIC, SessionList } from "../state.js";
import { ListStore } from "../store.js";
import { message } from "../util.js";

export { clipboardWrite, state, t };

/** 列表每次 rebuild 或状态变化递增，驱动 TSX 细粒度刷新。 */
export const tick = signal(0);

/** 3D 小人动作状态机（直接驱动 `<SmeltNativeBuddy>`，无跨 VM 插槽）。 */
const buddyStage = empty_stage();
let buddySeats = new Map<string, string>();
let buddyKnown = new Set<string>();
export const buddyTick = signal(0);

function sameSeats(a: Map<string, string>, b: Map<string, string>): boolean {
  if (a.size !== b.size) return false;
  for (const [k, v] of a) if (b.get(k) !== v) return false;
  return true;
}

function syncBuddyStage() {
  const viewedSession = activeContext().session;
  const next = new Map<string, string>();
  let viewed: string | null = null;
  const fresh: string[] = [];
  const seenMachines = new Set<string>();

  for (const entry of machines()) {
    const machine = entry.machine ?? null;
    const directory = workspaceDirectory(machine);
    if (!directory) continue;
    const seen = buddyKnown.has(machine ?? "");
    for (const fact of conversationFacts(machine)) {
      const key = conversation_key(machine, fact.id);
      if (seen && !buddySeats.has(key) && !buddyStage.actors.has(key)) fresh.push(key);
      next.set(key, fact.status);
    }
    if (viewedSession && (viewedSession.machine ?? null) === machine) {
      const conv = (directory.conversations ?? []).find((c: any) =>
        session_ids(c).includes(viewedSession.id),
      );
      if (conv) viewed = conversation_key(machine, conv.id);
    }
    seenMachines.add(machine ?? "");
  }
  buddyKnown = seenMachines;
  if (sameSeats(next, buddySeats) && viewed === buddyStage.selected) return;
  const now = Date.now();
  track(buddyStage, next, viewed, now);
  for (const key of fresh) greet(buddyStage, key, now);
  buddySeats = next;
  buddyTick.set(buddyTick() + 1);
}

export function buddyActor(machine: string | null, conversation: string): {
  seed: string;
  act: string;
  since: number;
} {
  buddyTick();
  const current = buddyStage.actors.get(conversation_key(machine, conversation));
  return {
    seed: conversation,
    act: current?.act ?? "rest",
    since: current?.since ?? 0,
  };
}

/** 搜索词两种语言都写：用哪种语言搜都找得到。 */
const SETTINGS_KEYWORDS = ["头像", "小人", "行首", "侧栏", "avatar", "buddy", "sidebar", "sessions"];

const nameOf = (id: string) => id.slice(OWNER.length + 1);

const shortcut = (id: string, titleKey: string, key?: string) => ({
  name: nameOf(id),
  title: text(titleKey),
  key,
  context: "Workspace",
  args: [],
});

export function theme() {
  return sdkTheme();
}

/** 按当前窗口的 rem 换算间距（0.25rem 一档）。 */
export function space(steps: number): number {
  return ((theme().rem ?? 16) * steps) / 4;
}

export interface MemoryInputState {
  placeholder: string;
  value: () => string;
  set_value: (next: string) => void;
  submit: () => void;
  on: (event: string, fn: (value: string) => void) => () => void;
  release: () => void;
}

function createMemoryInput(placeholder: string): MemoryInputState {
  let current = "";
  const changeListeners = new Set<(next: string) => void>();
  const submitListeners = new Set<(next: string) => void>();
  return {
    placeholder,
    value: () => current,
    set_value(next: string) {
      current = String(next ?? "");
      for (const fn of changeListeners) fn(current);
    },
    submit() {
      for (const fn of submitListeners) fn(current);
    },
    on(event: string, fn: (next: string) => void) {
      const target = event === "submit" ? submitListeners : changeListeners;
      target.add(fn);
      return () => target.delete(fn);
    },
    release() {
      changeListeners.clear();
      submitListeners.clear();
    },
  };
}

export const hostAdapter = {
  machines() {
    return machines().map((m: any) => ({
      machine: m.machine ?? null,
      name: m.name ?? (m.machine == null ? t("machine.local") : String(m.machine)),
      unavailable: m.unavailable ?? null,
    }));
  },
  connections,
  local_os: localOs(),
  path_style: pathStyle,
  workspace_directory: workspaceDirectory,
  session_states: sessionStates,
  conversation_facts: conversationFacts,
  active_context: activeContext,
  path: {
    clean: pathClean,
    file_name: pathFileName,
    join: pathJoin,
    is_absolute: pathIsAbsolute,
    starts_with: pathStartsWith,
  },
  project_labels: projectLabels,
  call,
  command,
  run,
  t,
  redraw() {
    tick.set(tick() + 1);
  },
  path_kind: pathKind,
  parent_dir(path: string): string | null {
    return pathParent(pathStyle(null), path);
  },
  now: () => new Date(),
  uuid: () => newSessionId(),
  input: (placeholder: string) => createMemoryInput(placeholder),
  conversation_acp: conversationAcp,
};

export const listStore = new ListStore({
  query: sdkStore.query,
  execute: sdkStore.execute,
  write: sdkStore.write,
  migrate: sdkStore.migrate,
});

export const list = new SessionList(hostAdapter, listStore, new_sidebar_ui(), { kind: "unset" });

export interface ContributedMenuItem {
  label: string;
  icon: string | null;
  image: string | null;
  command?: string;
  args?: unknown;
  submenu?: ContributedMenuItem[];
}

export function fetchContextMenuItems(
  menu: string,
  machine: string | null,
  context: unknown,
): Promise<ContributedMenuItem[]> {
  return contextMenuItems(menu, machine, context)
    .then((items: unknown) => (Array.isArray(items) ? (items as ContributedMenuItem[]) : []))
    .catch(() => []);
}

export const machineMenuVersion = signal(0);

let storeReady = false;
let stateReady = false;
let booted = false;
let pendingEvents: any[] = [];
let prevMachinesKey = "";
let prevSessionsKey = "";
let prevLocale = "";

const focusCallbacks = new Set<() => void>();

export function registerFocusCallback(fn: () => void): () => void {
  focusCallbacks.add(fn);
  return () => focusCallbacks.delete(fn);
}

function triggerFocusList() {
  for (const fn of focusCallbacks) fn();
}

function syncFallbackSelection(next: any) {
  if (list.heard_selection) return;
  const session = next?.active_context?.session;
  if (!session || typeof session.id !== "string") {
    if (list.selection.kind !== "none") {
      list.selection = { kind: "none" };
      hostAdapter.redraw();
    }
    return;
  }
  const machine = session.machine ?? null;
  const directory = hostAdapter.workspace_directory(machine);
  const found = directory?.conversations?.find(
    (conv: any) => conv.id === session.id || session_ids(conv).includes(session.id),
  );
  if (!found) return;
  const key = { machine, id: found.id };
  if (list.selection.kind !== "session" || !same_conversation(list.selection.key, key)) {
    list.selection = { kind: "session", key };
    list.reveal(key);
    hostAdapter.redraw();
  }
}

function digestMachines(next: any): string {
  const m = next?.machines ?? [];
  const remotes = next?.connections?.remotes ?? [];
  return JSON.stringify([
    m.map((item: any) => [item.machine ?? null, item.name ?? "", item.unavailable ?? null]),
    remotes.map((r: any) => [r.machine, r.state, r.link]),
  ]);
}

function digestSessions(next: any): string {
  const m = next?.machines ?? [];
  return JSON.stringify(
    m.map((item: any) => [
      item.machine ?? null,
      item.directory?.epoch ?? null,
      item.directory?.revision ?? null,
      item.directory?.projects?.length ?? 0,
      item.directory?.conversations?.length ?? 0,
      item.directory?.worksets?.length ?? 0,
      item.facts,
      item.states,
    ]),
  );
}

function maybeBoot() {
  if (booted || !storeReady || !stateReady) return;
  booted = true;
  const current = state();
  prevMachinesKey = digestMachines(current);
  prevSessionsKey = digestSessions(current);
  prevLocale = String(current.locale ?? "");
  syncBuddyStage();
  syncFallbackSelection(current);
  list.start().catch((error: unknown) => {
    console.error(`[session-list] 启动失败：${message(error)}`);
  });
  const queued = pendingEvents;
  pendingEvents = [];
  for (const event of queued) {
    handleEvent(event);
  }
}

function handleEvent(event: any) {
  if (!event || typeof event !== "object") return;
  try {
    if (event.type === "view_opened") {
      if (event.args?.focus) triggerFocusList();
      return;
    }
    if (event.type === "machines_changed") {
      prevMachinesKey = digestMachines(state());
      machineMenuVersion.set(machineMenuVersion() + 1);
    } else if (event.type === "context_menu_items_changed") {
      machineMenuVersion.set(machineMenuVersion() + 1);
    } else if (event.type === "sessions_changed") {
      prevSessionsKey = digestSessions(state());
    } else if (event.type === "locale_changed") {
      prevLocale = String(locale() ?? "");
    }
    list.on_event(event);
  } catch (error) {
    console.error(`[session-list] 处理宿主事件 ${event.type} 失败：`, error);
  }
}

onState((next) => {
  stateReady = true;
  syncBuddyStage();
  if (!booted) {
    maybeBoot();
    return;
  }
  // 宿主在发出 sessions_changed / machines_changed / locale_changed 事件前总会先推 smelt.state；
  // 这些变化统一由 onEvent 处理，避免双倍 rebuild 与在无关 state 推送时无谓递增 tick。
  syncFallbackSelection(next);
});

onEvent((event) => {
  if (!booted) {
    pendingEvents.push(event);
    return;
  }
  handleEvent(event);
});

onCommand((call) => {
  switch (call.name) {
    case nameOf(REVEAL_IN_LIST):
      return list.reveal_in_list(call.args);
    case nameOf(NEW_SESSION):
      list.open_palette();
      return null;
    case nameOf(OPEN_PROJECT):
      return list.pick_projects();
    case nameOf(FOCUS_LIST):
      return list.focus_list();
    case nameOf(PREV_SESSION):
      return list.step_session(false);
    case nameOf(NEXT_SESSION):
      return list.step_session(true);
  }
  throw new Error(`不认识的命令：${call.name}`);
});

initHost().then(() => {
  stateReady = true;
  const commands = [
    {
      name: nameOf(REVEAL_IN_LIST),
      title: text("command.reveal_in_list"),
      requires_args: true,
      args: ["machine", "session_id"],
    },
    shortcut(NEW_SESSION, "menu.new_session"),
    shortcut(OPEN_PROJECT, "command.open_project"),
    shortcut(FOCUS_LIST, "command.focus_list", "cmd-shift-e"),
    shortcut(PREV_SESSION, "command.prev_session", "cmd-up"),
    shortcut(NEXT_SESSION, "command.next_session", "cmd-down"),
  ];
  registerViews([
    {
      name: LIST_NAME,
      export: LIST_NAME,
      title: text("view.list"),
      icon: "icons/messages-square.svg",
      draws_top_bar: true,
    },
    {
      name: SETTINGS_NAME,
      export: SETTINGS_NAME,
      title: text("settings.title"),
      icon: "icons/messages-square.svg",
      settings: { group: "general", keywords: SETTINGS_KEYWORDS },
    },
  ]).catch((error) => console.error("[session-list] registerViews 失败：", error));
  registerCommands(commands).catch((error) =>
    console.error("[session-list] registerCommands 失败：", error),
  );
  registerTopics([ATTENTION_TOPIC]).catch((error) =>
    console.error("[session-list] registerTopics 失败：", error),
  );
  maybeBoot();
});

(async () => {
  const style = pathStyle(null);
  const own = dataDir();
  let legacy: string | null = null;
  if (own) {
    const parent = pathParent(style, pathClean(style, own));
    if (parent) {
      legacy = pathJoin(style, pathJoin(style, parent, "smelt.sessions"), "store.sqlite3");
    }
  }
  await listStore.open(legacy);
  const [ui, avatar] = await Promise.all([listStore.read_sidebar(), listStore.avatar_choice()]);
  storeReady = true;
  list.loaded(ui, avatar);
  maybeBoot();
})().catch((error: unknown) => {
  console.error(`[session-list] 打开 SQLite 存储失败：${message(error)}`);
  storeReady = true;
  maybeBoot();
});
