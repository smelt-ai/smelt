// Guest 插件运行时：把插件导出的视图接到外壳的挂载通道。
// 支持单插件独立 VM（`provideViews`）与多内置插件共享 VM（`provideMultiViews` + `createScopedSdk`）；
// 每个实例持有一份响应式 args signal，收到 update 时原地更新 props.args，不重建组件子树；
// 组件体内抛错由 ErrorBoundary 兜底，不影响同 VM 其它插件或宿主。
import { ErrorBoundary, host, mountSlot, signal, Text } from "nsh";

type Component = (props: { args?: unknown }) => unknown;
type Views = Record<string, unknown>;
type CommandHandlerFn = (call: { name: string; args: any }) => any;

interface SyncOp {
  op: "mount" | "update" | "unmount";
  instance: string;
  target?: string;
  plugin?: string;
  view?: string;
  args?: unknown;
}

interface Mounted {
  setArgs: (value: unknown) => void;
  dispose: () => void;
}

interface CommandCall {
  id: number;
  plugin?: string;
  name: string;
  args: any;
}

interface EventListenerEntry {
  owner: string | null;
  fn: (event: any) => void;
  types: Set<string>;
}

const BOOT_TIMEOUT_MS = 3_000;
const pendingBootTasks: Promise<unknown>[] = [];
const bootTasksByPlugin = new Map<string, Promise<unknown>[]>();

const textsByPluginSlice = signal<Record<string, Record<string, Record<string, string>>>>({});
let cachedTextsByPlugin: Record<string, Record<string, Record<string, string>>> = {};

let localeReader: () => string = () => "en";
let eventSyncHandler: () => void = () => {};

const eventListeners = new Set<EventListenerEntry>();

let defaultCommandHandler: CommandHandlerFn | null = null;
const commandHandlers = new Map<string, CommandHandlerFn>();
const pendingCommands: CommandCall[] = [];

export function setTextsByPlugin(
  next: Record<string, Record<string, Record<string, string>>>,
): void {
  cachedTextsByPlugin = next;
  textsByPluginSlice.set(next);
}

export function setLocaleReader(fn: () => string): void {
  localeReader = fn;
}

export function setEventSyncHandler(fn: () => void): void {
  eventSyncHandler = fn;
}

export function resolveTranslation(
  dict: Record<string, Record<string, string>> | undefined,
  curLocale: string,
  key: string,
  params?: Record<string, unknown>,
): string {
  let raw =
    dict?.[curLocale]?.[key] ??
    dict?.[curLocale.split("-")[0]]?.[key] ??
    dict?.en?.[key] ??
    key;
  if (params && typeof raw === "string") {
    for (const [k, v] of Object.entries(params)) {
      raw = raw.replaceAll(`%{${k}}`, String(v ?? ""));
    }
  }
  return raw;
}

export function buildTextTable(
  dict: Record<string, Record<string, string>> | undefined,
  key: string,
): Record<string, string> {
  const out: Record<string, string> = {};
  if (dict && typeof dict === "object") {
    for (const [loc, table] of Object.entries(dict)) {
      if (table && typeof table[key] === "string") {
        out[loc] = table[key];
      }
    }
  }
  if (Object.keys(out).length === 0) out.en = key;
  return out;
}

export function registerEventListener(
  owner: string | null,
  listener: (event: any) => void,
  types: readonly string[],
): () => void {
  const entry: EventListenerEntry = {
    owner,
    fn: listener,
    types: new Set(types),
  };
  eventListeners.add(entry);
  eventSyncHandler();
  return () => {
    eventListeners.delete(entry);
    eventSyncHandler();
  };
}

export function hasEventListeners(): boolean {
  return eventListeners.size > 0;
}

export function collectEventTypes(): string[] {
  const types = new Set<string>();
  for (const entry of eventListeners) {
    for (const kind of entry.types) types.add(kind);
  }
  return [...types];
}

export function dispatchIncomingEvent(event: any): void {
  const evType = typeof event?.type === "string" ? event.type : null;
  const pluginFilter =
    evType === "plugin" && typeof event?.name === "string" ? `plugin:${event.name}` : null;
  const targetPlugin =
    typeof event?.__target_plugin === "string" ? event.__target_plugin : null;
  let cleanEvent = event;
  if (targetPlugin !== null && event && typeof event === "object") {
    cleanEvent = { ...event };
    delete cleanEvent.__target_plugin;
  }
  for (const entry of [...eventListeners]) {
    if (targetPlugin !== null && entry.owner !== null && entry.owner !== targetPlugin) {
      continue;
    }
    const matched =
      (evType != null && entry.types.has(evType)) ||
      (pluginFilter != null && entry.types.has(pluginFilter));
    if (!matched) {
      continue;
    }
    try {
      entry.fn(cleanEvent);
    } catch (error) {
      console.error("[smelt/sdk] event listener error:", error);
    }
  }
}

function runCommandCall(call: CommandCall, handler: CommandHandlerFn): void {
  const pluginOwner = typeof call.plugin === "string" ? call.plugin : null;
  Promise.resolve()
    .then(() => handler({ name: call.name, args: call.args }))
    .then(
      (value) =>
        host.call("command.reply", {
          ...(pluginOwner !== null ? { __plugin: pluginOwner } : {}),
          id: call.id,
          ok: true,
          value: value ?? null,
        }),
      (error: any) =>
        host.call("command.reply", {
          ...(pluginOwner !== null ? { __plugin: pluginOwner } : {}),
          id: call.id,
          ok: false,
          error: error?.message ?? String(error),
        }),
    )
    .catch(() => {});
}

function flushPendingCommands(): void {
  if (pendingCommands.length === 0) return;
  const remaining: CommandCall[] = [];
  for (const call of pendingCommands.splice(0)) {
    const owner = typeof call.plugin === "string" ? call.plugin : null;
    const handler =
      (owner !== null ? commandHandlers.get(owner) : undefined) ?? defaultCommandHandler;
    if (handler) {
      runCommandCall(call, handler);
    } else {
      remaining.push(call);
    }
  }
  pendingCommands.push(...remaining);
}

export function dispatchIncomingCommand(call: any): void {
  if (!call || typeof call.id !== "number" || typeof call.name !== "string") return;
  const owner = typeof call.plugin === "string" ? call.plugin : null;
  const handler =
    (owner !== null ? commandHandlers.get(owner) : undefined) ?? defaultCommandHandler;
  if (!handler) {
    pendingCommands.push(call);
    return;
  }
  runCommandCall(call, handler);
}

export function registerDefaultCommandHandler(handler: CommandHandlerFn): void {
  defaultCommandHandler = handler;
  flushPendingCommands();
}

export function registerScopedCommandHandler(pluginId: string, handler: CommandHandlerFn): void {
  commandHandlers.set(pluginId, handler);
  flushPendingCommands();
}

function runBootTask(
  queue: Promise<unknown>[],
  work: (() => Promise<unknown>) | Promise<unknown>,
): Promise<unknown> {
  const task = Promise.resolve()
    .then(() => (typeof work === "function" ? work() : work))
    .catch((error) => {
      console.error("[smelt/provider] bootTask failed:", error);
    });
  queue.push(task);
  return task;
}

/** 登记插件启动期异步初始化任务（如注册页/布局/读库恢复设置）；全部完成后才上报 smelt.plugin.ready。 */
export function bootTask(work: (() => Promise<unknown>) | Promise<unknown>): Promise<unknown> {
  return runBootTask(pendingBootTasks, work);
}

function bootTaskFor(
  pluginId: string,
  work: (() => Promise<unknown>) | Promise<unknown>,
): Promise<unknown> {
  let queue = bootTasksByPlugin.get(pluginId);
  if (!queue) {
    queue = [];
    bootTasksByPlugin.set(pluginId, queue);
  }
  return runBootTask(queue, work);
}

async function flushBootTaskQueue(queue: Promise<unknown>[]): Promise<void> {
  while (queue.length > 0) {
    await Promise.allSettled(queue.splice(0));
  }
}

function announceAfterBootTasks(queue: Promise<unknown>[] | undefined, announce: () => void): void {
  if (!queue || queue.length === 0) {
    announce();
    return;
  }
  let timer: ReturnType<typeof setTimeout> | undefined;
  void Promise.race([
    flushBootTaskQueue(queue),
    new Promise<void>((resolve) => {
      timer = setTimeout(resolve, BOOT_TIMEOUT_MS);
    }),
  ]).finally(() => {
    if (timer !== undefined) clearTimeout(timer);
    announce();
  });
}

function Failed(props: { id: string }) {
  return <Text class="text-xs text-destructive">{props.id} 出错了</Text>;
}

function attachSlotSyncHandler(
  resolveComponent: (item: SyncOp) => { id: string; Comp: Component | undefined },
): void {
  const mounts = new Map<string, Mounted>();

  function unmount(instance: string) {
    const current = mounts.get(instance);
    if (!current) return;
    mounts.delete(instance);
    current.dispose();
  }

  function mountOne(
    instance: string,
    target: string,
    pluginId: string,
    Comp: Component | undefined,
    initialArgs: unknown,
  ) {
    unmount(instance);
    if (typeof Comp !== "function") return;
    const args = signal<unknown>(initialArgs);
    const dispose = mountSlot(target, () => (
      <ErrorBoundary
        fallback={(error: unknown) => {
          console.error(`[plugin:${pluginId}] render error:`, error);
          return <Failed id={pluginId} />;
        }}
      >
        <Comp args={args()} />
      </ErrorBoundary>
    ));
    mounts.set(instance, {
      setArgs: (value) => args.set(value),
      dispose,
    });
  }

  host.on("smelt.slot.sync", (data: unknown) => {
    if (!Array.isArray(data)) return;
    for (const item of data as SyncOp[]) {
      if (item.op === "mount" && item.target && item.view) {
        const resolved = resolveComponent(item);
        mountOne(item.instance, item.target, resolved.id, resolved.Comp, item.args);
      } else if (item.op === "update") {
        const entry = mounts.get(item.instance);
        if (entry) {
          entry.setArgs(item.args);
        }
      } else if (item.op === "unmount") {
        unmount(item.instance);
      }
    }
  });
}

/** 将单个插件导出的视图注册给引擎内宿主挂载路由器，并按需 mountSlot 到外壳分配的容器节点。 */
export function provideViews(pluginId: string, views: Views): void {
  attachSlotSyncHandler((item) => ({
    id: pluginId,
    Comp: item.view ? (views[item.view] as Component | undefined) : undefined,
  }));

  const exported = Object.keys(views).filter((key) => typeof views[key] === "function");
  announceAfterBootTasks(pendingBootTasks, () =>
    host.post("smelt.plugin.ready", { views: exported }),
  );
}

/** 将多个共驻同一内置 VM 的插件导出的视图注册给宿主挂载路由器，并各自独立完成 bootTask 与 ready 上报。 */
export function provideMultiViews(pluginsMap: Record<string, Views>): void {
  attachSlotSyncHandler((item) => {
    const id = item.plugin ?? "";
    const Comp = item.view ? (pluginsMap[id]?.[item.view] as Component | undefined) : undefined;
    return { id, Comp };
  });

  for (const [id, views] of Object.entries(pluginsMap)) {
    const exported = Object.keys(views).filter((key) => typeof views[key] === "function");
    announceAfterBootTasks(bootTasksByPlugin.get(id), () =>
      host.post("smelt.plugin.ready", { plugin: id, views: exported }),
    );
  }
}

/** 为共享内置 VM 内的单个逻辑插件构造按 `scopedId` 隔离的 SDK API。 */
export function createScopedSdk(scopedId: string) {
  const scopedCall = (method: string, data: Record<string, unknown> = {}) =>
    host.call(method, { __plugin: scopedId, ...data });

  const getTexts = (): Record<string, Record<string, string>> => {
    const all = textsByPluginSlice();
    return all[scopedId] ?? cachedTextsByPlugin[scopedId] ?? {};
  };

  return {
    pluginId: (): string => scopedId,
    dataDir: (): string => {
      const dirs = (host.params as any)?.__smelt_data_dirs;
      return typeof dirs?.[scopedId] === "string" ? dirs[scopedId] : "";
    },
    bootTask: (work: (() => Promise<unknown>) | Promise<unknown>) => bootTaskFor(scopedId, work),
    texts: (): Record<string, Record<string, string>> => getTexts(),
    t: (key: string, params?: Record<string, unknown>): string =>
      resolveTranslation(getTexts(), localeReader(), key, params),
    text: (key: string): Record<string, string> => buildTextTable(getTexts(), key),
    onEvent: (listener: (event: any) => void, types: readonly string[]) =>
      registerEventListener(scopedId, listener, types),
    onCommand: (handler: CommandHandlerFn) => registerScopedCommandHandler(scopedId, handler),
    store: {
      query: (sql: string, params: unknown[] = []): Promise<any[]> =>
        scopedCall("store.query", { sql, params }),
      execute: (sql: string, params: unknown[] = []): Promise<number> =>
        scopedCall("store.execute", { sql, params }),
      write: (statements: Array<{ sql: string; params?: unknown[] }>): Promise<number[]> =>
        scopedCall("store.write", { statements }),
      migrate: (steps: string[]): Promise<void> => scopedCall("store.migrate", { steps }),
    },
    registerViews: (specs: any[]): Promise<void> => scopedCall("register_views", { views: specs }),
    registerCommands: (
      specs: any[],
      groupTitle?: string | Record<string, string>,
    ): Promise<void> =>
      scopedCall("register_commands", { commands: specs, group_title: groupTitle ?? null }),
    registerTopics: (topics: string[]): Promise<void> =>
      scopedCall("register_topics", { topics }),
    registerLayouts: (specs: any[]): Promise<void> =>
      scopedCall("register_layouts", { layouts: specs }),
    registerPages: (specs: any[]): Promise<void> => scopedCall("register_pages", { pages: specs }),
    registerContextMenuItems: (menu: string, items: any[]): Promise<void> =>
      scopedCall("register_context_menu_items", { menu, items }),
    registerMenuItems: (items: any[]): Promise<void> =>
      scopedCall("register_menu_items", { items }),
    registerFileOpener: (name: string): Promise<void> =>
      scopedCall("register_file_opener", { name }),
    setStatusMenu: (menu: unknown | null): Promise<void> =>
      scopedCall("set_status_menu", { menu }),
    emitEvent: (name: string, payload: unknown = null): Promise<void> =>
      scopedCall("emit_event", { name, payload }),
  };
}
