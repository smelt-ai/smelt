// 会话列表插件在 NSH A2 架构下的宿主桥：
// 把 `nsh` 的 `host` 通道与 `smelt/pilot` 纯同步模块拼成 `SessionList` 和 `ListStore` 要的 `host`/`db` 接口，
// 零改动复用 `plugins/session-list/` 的纯 JS 业务逻辑。

import { host, nativeModule, signal } from "nsh";

import { new_sidebar_ui, same_conversation, session_ids } from "./model.js";
import { SessionList, ATTENTION_TOPIC } from "./state.js";
import { ListStore } from "./store.js";
import { message } from "./util.js";

interface SmeltNative {
  newSessionId(): string;
  localOs(): string;
  pathClean(style: string, path: string): string;
  pathJoin(style: string, base: string, child: string): string;
  pathFileName(style: string, path: string): string | null;
  pathParent(style: string, path: string): string | null;
  pathIsAbsolute(style: string, path: string): boolean;
  pathStartsWith(style: string, path: string, base: string): boolean;
  projectLabels(style: string, roots: string[]): string[];
  projectRootOf(style: string, roots: string[], path: string): string | null;
  newSessionIcon(actionJson: string): string;
  conversationAcp(settingsJson: string, optionId: string, sid: string, cwd: string): string | null;
}

const { Smelt } = nativeModule<{ Smelt: SmeltNative }>("smelt/pilot");

/** 宿主推来的最新只读状态（含 locale / machines / connections / views / theme）。 */
export const state = signal<any>(null);
/** 列表每次 rebuild 或状态变化递增，驱动 TSX 细粒度刷新。 */
export const tick = signal(0);

let texts: Record<string, Record<string, string>> = {};
let revision = -1;
let storeReady = false;
let booted = false;
let pendingEvents: any[] = [];
let prevMachinesKey = "";
let prevSessionsKey = "";
let prevLocale = "";
let prevViewsKey = "";

const FALLBACK_THEME = {
  rem: 16,
  palette: {
    bg: "#161618",
    side: "#1a1a1d",
    surface: "#222226",
    soft: "#2a2a30",
    text: "#eeeeee",
    muted: "#9a9aa2",
    muted_selected: "#c4c4cc",
    faint: "#6e6e78",
    faint_selected: "#9e9ea8",
    ink: "#eeeeee",
    on_ink: "#161618",
    link: "#3b82f6",
    run: "#3b82f6",
    need: "#f59e0b",
    ok: "#22c55e",
    bad: "#ef4444",
    on_bad: "#ffffff",
  },
  type: {
    ui: { size: 13, line_height: 20, weight: 440 },
    ui_medium: { size: 13, line_height: 20, weight: 530 },
    small: { size: 11, line_height: 12, weight: 440 },
    small_medium: { size: 11, line_height: 12, weight: 530 },
    text_xsmall: { size: 12, line_height: 18, weight: 400 },
    text_xsmall_medium: { size: 12, line_height: 18, weight: 500 },
  },
  radius: { sm: 4, md: 6, row: 6 },
};

export function theme() {
  const raw = state()?.theme;
  if (!raw) return FALLBACK_THEME;
  return {
    rem: raw.rem ?? FALLBACK_THEME.rem,
    palette: { ...FALLBACK_THEME.palette, ...(raw.palette ?? {}) },
    type: { ...FALLBACK_THEME.type, ...(raw.type ?? {}) },
    radius: { ...FALLBACK_THEME.radius, ...(raw.radius ?? {}) },
  };
}

/** 按当前窗口的 rem 换算间距（0.25rem 一档）。 */
export function space(steps: number): number {
  return (theme().rem * steps) / 4;
}

/** 文案查询，支持 `%{name}` 参数替换。 */
export function t(key: string, params?: Record<string, unknown>): string {
  const locale = state()?.locale ?? "en";
  let text = texts[locale]?.[key] ?? texts.en?.[key] ?? key;
  if (params) {
    for (const [name, value] of Object.entries(params)) {
      text = text.replaceAll(`%{${name}}`, String(value ?? ""));
    }
  }
  return text;
}

function machineRecord(machine: string | null) {
  const list = state()?.machines ?? [];
  return list.find((entry: any) => (entry.machine ?? null) === (machine ?? null)) ?? null;
}

function createMemoryInput(placeholder: string) {
  let value = "";
  const listeners = new Set<(next: string) => void>();
  return {
    placeholder,
    value: () => value,
    set_value(next: string) {
      value = String(next ?? "");
      for (const fn of listeners) fn(value);
    },
    on(_event: string, fn: (next: string) => void) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    release() {
      listeners.clear();
    },
  };
}

export const hostAdapter = {
  machines() {
    return (state()?.machines ?? []).map((m: any) => ({
      machine: m.machine ?? null,
      name: m.name ?? (m.machine == null ? t("machine.local") : String(m.machine)),
      unavailable: m.unavailable ?? null,
    }));
  },
  connections() {
    return state()?.connections ?? { remotes: [], load_error: null };
  },
  local_os: Smelt.localOs(),
  path_style(machine: string | null) {
    return machineRecord(machine)?.path_style ?? "posix";
  },
  workspace_directory(machine: string | null) {
    return machineRecord(machine)?.directory ?? null;
  },
  session_states(machine: string | null) {
    return machineRecord(machine)?.states ?? [];
  },
  conversation_facts(machine: string | null) {
    return machineRecord(machine)?.facts ?? [];
  },
  active_context() {
    return state()?.active_context ?? { project: null, session: null };
  },
  path: {
    clean: (style: string, path: string) => Smelt.pathClean(style, path),
    file_name: (style: string, path: string) => Smelt.pathFileName(style, path),
    join: (style: string, base: string, child: string) => Smelt.pathJoin(style, base, child),
    is_absolute: (style: string, path: string) => Smelt.pathIsAbsolute(style, path),
    starts_with: (style: string, path: string, base: string) => Smelt.pathStartsWith(style, path, base),
  },
  project_labels: (style: string, roots: string[]) => Smelt.projectLabels(style, roots),
  call(machine: string | null, method: string, params?: unknown, timeout_ms?: number) {
    return host.call("call", {
      machine: machine ?? null,
      method,
      params: params ?? {},
      timeout_ms: timeout_ms ?? null,
    });
  },
  run(command: string, args?: unknown) {
    return host.call("run", { command, args: args ?? null });
  },
  t,
  redraw() {
    tick.set(tick() + 1);
  },
  path_kind(path: string): Promise<"dir" | "file" | null> {
    return host.call("fs.path_kind", { path }) as Promise<"dir" | "file" | null>;
  },
  parent_dir(path: string): string | null {
    const style = hostAdapter.path_style(null);
    return Smelt.pathParent(style, path);
  },
  now: () => new Date(),
  uuid: () => Smelt.newSessionId(),
  input: (placeholder: string) => createMemoryInput(placeholder),
  conversation_acp(settings: unknown, option_id: string, sid: string, cwd: string) {
    const raw = Smelt.conversationAcp(JSON.stringify(settings), option_id, sid, cwd);
    return raw ? JSON.parse(raw) : null;
  },
};

export const store = new ListStore({
  query: (sql: string, params?: unknown[]) => host.call("store.query", { sql, params: params ?? [] }),
  execute: (sql: string, params?: unknown[]) => host.call("store.execute", { sql, params: params ?? [] }),
  write: (statements: { sql: string; params?: unknown[] }[]) => host.call("store.write", { statements }),
  migrate: (steps: string[]) => host.call("store.migrate", { steps }),
});

export const list = new SessionList(hostAdapter, store, new_sidebar_ui(), { kind: "unset" });

export function clipboardWrite(text: string): Promise<void> {
  return host.call("clipboard.write", { text }).then(() => undefined);
}

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
  return host
    .call("context_menu_items", { menu, machine: machine ?? null, context })
    .then((items: unknown) => (Array.isArray(items) ? (items as ContributedMenuItem[]) : []))
    .catch(() => []);
}

export function slotViews(slotId: string): string[] {
  const views = state()?.views ?? [];
  return views
    .filter((view: any) => Array.isArray(view.fills) && view.fills.includes(slotId))
    .map((view: any) => String(view.id));
}

/**
 * 尚未收到 `smelt.sessions/selection-changed` 时（例如试点独立环境），用宿主的
 * `active_context.session` 同步选中态，让高亮与主视图保持一致。
 */
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
  const machines = next?.machines ?? [];
  const remotes = next?.connections?.remotes ?? [];
  return JSON.stringify([
    machines.map((m: any) => [m.machine ?? null, m.name ?? "", m.unavailable ?? null]),
    remotes.map((r: any) => [r.machine, r.state, r.link]),
  ]);
}

function digestSessions(next: any): string {
  const machines = next?.machines ?? [];
  return JSON.stringify(
    machines.map((m: any) => [
      m.machine ?? null,
      m.directory?.epoch ?? null,
      m.directory?.revision ?? null,
      m.directory?.projects?.length ?? 0,
      m.directory?.conversations?.length ?? 0,
      m.directory?.worksets?.length ?? 0,
      m.facts,
      m.states,
    ]),
  );
}

function digestViews(next: any): string {
  return JSON.stringify((next?.views ?? []).map((v: any) => [v.id, v.fills ?? []]));
}

function maybeBoot() {
  if (booted || !storeReady || !state()) return;
  booted = true;
  const current = state();
  prevMachinesKey = digestMachines(current);
  prevSessionsKey = digestSessions(current);
  prevLocale = String(current.locale ?? "");
  prevViewsKey = digestViews(current);
  syncFallbackSelection(current);
  list.start().catch((error: unknown) => {
    console.error(`[session-list] 启动失败：${message(error)}`);
  });
  const queued = pendingEvents;
  pendingEvents = [];
  for (const event of queued) {
    try {
      list.on_event(event);
    } catch (error) {
      console.error(`[session-list] 处理事件 ${event?.type} 失败：`, error);
    }
  }
}

function receiveState(next: any) {
  if (!next || typeof next !== "object") return;
  if (next.texts && typeof next.texts === "object") {
    texts = next.texts;
  }
  const incomingRev = typeof next.revision === "number" ? next.revision : 0;
  if (incomingRev <= revision) {
    if (next.texts && state()) {
      list.rebuild();
    }
    return;
  }
  revision = incomingRev;
  state.set(next);

  if (!booted) {
    maybeBoot();
    return;
  }

  const nextMachines = digestMachines(next);
  const nextSessions = digestSessions(next);
  const nextLocale = String(next.locale ?? "");
  const nextViews = digestViews(next);

  let rebuilt = false;
  if (nextMachines !== prevMachinesKey) {
    prevMachinesKey = nextMachines;
    list.on_event({ type: "machines_changed" });
    rebuilt = true;
  }
  if (nextSessions !== prevSessionsKey) {
    prevSessionsKey = nextSessions;
    list.on_event({ type: "sessions_changed" });
    rebuilt = true;
  }
  if (nextLocale !== prevLocale) {
    prevLocale = nextLocale;
    if (!rebuilt) {
      list.on_event({ type: "locale_changed" });
      rebuilt = true;
    }
  }
  if (nextViews !== prevViewsKey) {
    prevViewsKey = nextViews;
    list.on_event({ type: "views_changed" });
  }
  syncFallbackSelection(next);
  if (!rebuilt) {
    hostAdapter.redraw();
  }
}

function receiveEvent(event: any) {
  if (!event || typeof event !== "object") return;
  if (!booted) {
    pendingEvents.push(event);
    return;
  }
  try {
    list.on_event(event);
  } catch (error) {
    console.error(`[session-list] 处理宿主事件 ${event.type} 失败：`, error);
  }
}

host.on("smelt.state", receiveState);
host.on("smelt.event", receiveEvent);
host.call("state").then(receiveState, (error: unknown) => {
  console.error(`[session-list] 取宿主状态失败：${message(error)}`);
});
host.call("register_topics", { topics: [ATTENTION_TOPIC] }).catch(() => {});

(async () => {
  await store.open(null);
  const [ui, avatar] = await Promise.all([store.read_sidebar(), store.avatar_choice()]);
  storeReady = true;
  list.loaded(ui, avatar);
  maybeBoot();
})().catch((error: unknown) => {
  console.error(`[session-list] 打开 SQLite 存储失败：${message(error)}`);
  storeReady = true;
  maybeBoot();
});
