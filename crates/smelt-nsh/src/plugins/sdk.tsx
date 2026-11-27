// Smelt NSH Guest SDK（通过 `import { ... } from "smelt/sdk"` 导入）。
// 封装通用的宿主状态、离散事件、命令双向派发、SQLite 存储、纯同步模型函数，以及原子原生能力组件。
// 不含任何具体插件逻辑，也不提供插件嵌插件的跨 VM 插槽。
import {
  cleanup,
  computed,
  effect,
  host,
  Image,
  lifecycle,
  nativeComponent,
  nativeModule,
  reduceMotion,
  signal,
  Text,
  View,
} from "nsh";
import {
  Button as BaseButton,
  Checkbox as BaseCheckbox,
  Collapsible,
  ContextMenu,
  Dialog,
  DiffView as BaseDiffView,
  Editor as BaseEditor,
  Menu,
  Popover,
  Radio as BaseRadio,
  Select,
  Switch as BaseSwitch,
  TextView,
  Tooltip,
} from "nsh/base";
import { Icon as NativeSvgIcon, Vector as NativeSvgVector } from "smelt/host";
import {
  buildTextTable,
  collectEventTypes,
  dispatchIncomingCommand,
  dispatchIncomingEvent,
  hasEventListeners,
  registerDefaultCommandHandler,
  registerEventListener,
  resolveTranslation,
  setEventSyncHandler,
  setHostThemeArrived,
  setLocaleReader,
  setTextsByPlugin,
} from "./provider.tsx";
export { bootTask } from "./provider.tsx";

// ---- 原生组件封装（仅保留 NSH 拼不出的 GPUI 能力件：重视图、标题带、毛玻璃坐标） ----

export const SmeltNativeView = nativeComponent("smelt.view", {
  props: ["spec", "ends", "safeArea", "focusSeq"],
  events: ["focusIn"],
}) as any;

export const SmeltNativeWindowBar = nativeComponent("smelt.window-bar", {
  props: ["safeArea", "titleBar"],
  events: [],
}) as any;

export const SmeltNativeWindowDrag = nativeComponent("smelt.window-drag", {
  props: [],
  events: [],
}) as any;

export const SmeltBackdropRegion = nativeComponent("smelt.backdrop-region", {
  props: ["placement"],
  events: [],
}) as any;

export const SmeltNativeDesktop = nativeComponent("smelt.desktop", {
  props: ["machine", "active", "control", "reconnectSeq"],
  events: ["connected", "disconnected", "clipboard", "text", "error"],
}) as any;

export const SmeltNativeTerminal = nativeComponent("smelt.terminal", {
  props: ["spec", "focusSeq", "retrySeq", "sendText", "searchQuery", "searchStep"],
  events: [
    "stateChange",
    "reconnecting",
    "writeError",
    "focusIn",
    "searchOpen",
    "searchStatus",
    "selectionChange",
  ],
}) as any;

// ---- 纯同步宿主模块 ----

const { Smelt } = nativeModule("smelt/host") as any;
export const nativeApi = Smelt;

export function newSessionId(): string {
  return Smelt.newSessionId();
}

export function localOs(): string {
  return Smelt.localOs();
}

export function smeltVersion(): string {
  return Smelt.smeltVersion();
}

export function qrMatrix(text: string): { size: number; bits: string } | null {
  const raw = Smelt.qrMatrix(text);
  return raw ? JSON.parse(raw) : null;
}

export function pathClean(style: string, path: string): string {
  return Smelt.pathClean(style, path);
}

export function pathJoin(style: string, base: string, child: string): string {
  return Smelt.pathJoin(style, base, child);
}

export function pathFileName(style: string, path: string): string | null {
  return Smelt.pathFileName(style, path) ?? null;
}

export function pathParent(style: string, path: string): string | null {
  return Smelt.pathParent(style, path) ?? null;
}

export function pathIsAbsolute(style: string, path: string): boolean {
  return Boolean(Smelt.pathIsAbsolute(style, path));
}

export function pathStartsWith(style: string, path: string, base: string): boolean {
  return Boolean(Smelt.pathStartsWith(style, path, base));
}

export function pathStripPrefix(style: string, path: string, base: string): string | null {
  return Smelt.pathStripPrefix(style, path, base) ?? null;
}

export function pathExtension(style: string, path: string): string | null {
  return Smelt.pathExtension(style, path) ?? null;
}

export function projectLabels(style: string, roots: string[]): string[] {
  return Smelt.projectLabels(style, roots);
}

export function projectRootOf(style: string, roots: string[], path: string): string | null {
  return Smelt.projectRootOf(style, roots, path) ?? null;
}

export function newSessionIcon(action: unknown): string {
  return Smelt.newSessionIcon(JSON.stringify(action));
}

export function conversationAcp(
  settings: unknown,
  optionId: string,
  sid: string,
  cwd: string,
): unknown {
  const raw = Smelt.conversationAcp(JSON.stringify(settings), optionId, sid, cwd);
  return raw ? JSON.parse(raw) : null;
}

export function quotaTarget(id: string): { id: string; service: string; name: string; icon: string } | null {
  const raw = Smelt.quotaTarget(id);
  return raw ? JSON.parse(raw) : null;
}

/** 对话快照条目窗口：全局下标 `[offset, offset + len)`，带所在快照的总数、修订号和对话身份。 */
export interface EntryWindow {
  offset: number;
  len: number;
  total: number;
  revision: number;
  history_session_id: string | null;
  acp_session_id: string | null;
}

/**
 * 合并后的条目 = 手里窗口前 `keep` 条 + 新来的全部（`take_next`）+ 手里从 `resume` 起的，截到 `window.len` 条。
 * `changed_from` 是第一条变了的条目在窗口里的下标，没变是 null。
 */
export interface EntryWindowMerge {
  window: EntryWindow;
  keep: number;
  take_next: boolean;
  resume: number;
  next_is_newer: boolean;
  /** 两边是不同的对话。翻页拿回来的页是这样时属于旧对话（拉页期间重新开始了），应丢掉。 */
  conversation_changed: boolean;
  changed_from: number | null;
}

/** 对话快照条目窗口合并，规则和手机端同一份（`smelt_model::entry_window`）。 */
export function mergeEntryWindow(current: EntryWindow, next: EntryWindow): EntryWindowMerge {
  return JSON.parse(Smelt.mergeEntryWindow(JSON.stringify(current), JSON.stringify(next)));
}

export function quotaFacts(data: unknown): any {
  return JSON.parse(Smelt.quotaFacts(JSON.stringify(data)));
}

export function parseFileLink(
  style: string,
  target: string,
  cwd?: string | null,
): { path: string; line: number | null; fragment: string } | null {
  const raw = Smelt.parseFileLink(style, target, cwd ?? "");
  return raw ? JSON.parse(raw) : null;
}

export function isInterruptMarker(text: string): boolean {
  return Boolean(Smelt.isInterruptMarker(text));
}

export function diffLineStats(oldText: string | null, newText: string): { added: number; removed: number } {
  const [added, removed] = Smelt.diffLineStats(oldText ?? "", newText);
  return { added, removed };
}

export function fileUriToPath(uri: string): string {
  return Smelt.fileUriToPath(uri);
}

export function isManagedAutomationWorkspace(
  style: string,
  path: string,
  automationId: string,
): boolean {
  return Boolean(Smelt.isManagedAutomationWorkspace(style, path, automationId));
}

/** 自动化配置哪里不对：运行时 `AutomationProblem` 的线格式，`kind` 是种类，其余是带的参数。 */
export type AutomationProblem = { kind: string; [param: string]: unknown };

/** 一条日程哪里不对（和运行时保存时同一条规则），没问题是 null。文案由插件按 `kind` 写。 */
export function automationScheduleProblem(schedule: unknown): AutomationProblem | null {
  const problem = Smelt.automationScheduleProblem(JSON.stringify(schedule));
  return problem == null ? null : JSON.parse(problem);
}

/** 一组触发条件（日程、外部触发、事件）哪里不对，没问题是 null。 */
export function automationTriggerProblem(trigger: unknown): AutomationProblem | null {
  const problem = Smelt.automationTriggerProblem(JSON.stringify(trigger));
  return problem == null ? null : JSON.parse(problem);
}

/** 智能体定义能不能用来开对话（引擎认得出且已开放）：新建菜单、手机、自动化都以这一条为准。 */
export function agentConversationReady(agent: unknown): boolean {
  return Boolean(Smelt.agentConversationReady(JSON.stringify(agent)));
}

export type ProviderInputProblem = "id_required" | "id_invalid" | "base_url_invalid";

export function customProviderIdProblem(id: string): ProviderInputProblem | null {
  return Smelt.customProviderIdProblem(id) ?? null;
}

export function apiBaseUrlProblem(baseUrl: string): ProviderInputProblem | null {
  return Smelt.apiBaseUrlProblem(baseUrl) ?? null;
}

export interface PiMcpPreset {
  id: string;
  name: string;
  summary: string;
  needs_sign_in: boolean;
  extension_url: string | null;
}

export function piMcpPresets(remoteDesktop: boolean): PiMcpPreset[] {
  return JSON.parse(Smelt.piMcpPresets(remoteDesktop));
}

export function rememberedAcpConfig(settings: unknown, agentId: string): Array<[string, string]> {
  if (!settings) return [];
  return JSON.parse(Smelt.rememberedAcpConfig(JSON.stringify(settings), agentId));
}

export function rememberAcpConfigValue(
  settings: unknown,
  agentId: string,
  configId: string,
  valueId: string,
): unknown | null {
  const raw = Smelt.rememberAcpConfigValue(JSON.stringify(settings), agentId, configId, valueId);
  return raw ? JSON.parse(raw) : null;
}

export interface AgentSourceMeta {
  id: string;
  label: string;
  short_label: string;
  acp_id: string | null;
  deletable: boolean;
  icon: string;
}

export interface ConversationAgentMeta {
  id: string;
  label: string;
  short_label: string;
  icon: string;
  config_dir_env_var: string | null;
}

export interface AgentCatalog {
  history_sources: AgentSourceMeta[];
  conversation_agents: ConversationAgentMeta[];
  agent_engines: ConversationAgentMeta[];
  terminal_agent_ids: string[];
}

let cachedCatalog: AgentCatalog | null = null;

export function agentCatalog(): AgentCatalog {
  if (!cachedCatalog) {
    cachedCatalog = JSON.parse(Smelt.agentCatalog());
  }
  return cachedCatalog!;
}

export function agentForSpace(settings: unknown, root: string): string | null {
  return Smelt.agentForSpace(JSON.stringify(settings), root) ?? null;
}

export function launchableProfiles(settings: unknown): Array<{
  id: string;
  label: string;
  kind: string | null;
  launchable: boolean;
  in_history_sources: boolean;
}> {
  return JSON.parse(Smelt.launchableProfiles(JSON.stringify(settings)));
}

export function handoffPrompt(payload: {
  source_agent: string;
  source_profile_label?: string | null;
  source_title: string;
  cwd?: string | null;
  target_agent: string;
  target_profile_label?: string | null;
  detail: {
    model?: string | null;
    turns: Array<{
      is_user: boolean;
      timestamp_ms?: number | null;
      text: string;
      tools?: string[];
      tool_paths?: string[];
    }>;
  };
}): string {
  return Smelt.handoffPrompt(JSON.stringify(payload));
}

export type ResolveResult<T> =
  | ({ ok: true } & T)
  | { ok: false; code: string; params: Record<string, unknown> };

export function resolveStartAcp(
  settings: unknown,
  withSpec: unknown,
  cwd: string,
  sid: string,
): ResolveResult<{ acp: any }> {
  return JSON.parse(Smelt.resolveStartAcp(JSON.stringify(settings), JSON.stringify(withSpec), cwd, sid));
}

export function resolveResumeAcp(
  settings: unknown,
  args: unknown,
  sid: string,
): ResolveResult<{ acp: any }> {
  return JSON.parse(Smelt.resolveResumeAcp(JSON.stringify(settings), JSON.stringify(args), sid));
}

export function resolveHandoffAcp(
  settings: unknown,
  args: unknown,
  sid: string,
): ResolveResult<{ acp: any }> {
  return JSON.parse(Smelt.resolveHandoffAcp(JSON.stringify(settings), JSON.stringify(args), sid));
}

export function resolveCliResume(
  style: string,
  entries: unknown[],
  settings: unknown | null,
  agentId: string,
  profileId: string | null,
  resumeId: string,
): ResolveResult<{ label: string; command: string }> {
  return JSON.parse(
    Smelt.resolveCliResume(
      style,
      JSON.stringify(entries),
      settings ? JSON.stringify(settings) : "",
      agentId,
      profileId ?? "",
      resumeId,
    ),
  );
}

export function daemonStateStatus(stateObj: unknown): "needs_you" | "running" | "idle" | null {
  return Smelt.daemonStateStatus(JSON.stringify(stateObj)) ?? null;
}

export function agentIdFromLaunch(launch: string): string | null {
  return Smelt.agentIdFromLaunch(launch) ?? null;
}

// ---- VM 身份与目录 ----

export function pluginId(): string {
  const id = (host.params as any)?.__smelt_plugin_id;
  return typeof id === "string" ? id : "";
}

export function dataDir(): string {
  const dir = (host.params as any)?.__smelt_data_dir;
  return typeof dir === "string" ? dir : "";
}

// ---- 响应式宿主状态 ----

export type StateSlice =
  | "locale"
  | "system_locale"
  | "locales"
  | "window_active"
  | "notification_permission"
  | "machines_meta"
  | "sessions_data"
  | "connections"
  | "active_context"
  | "pages"
  | "views"
  | "commands"
  | "js_views"
  | "safe_areas"
  | "bar_insets"
  | "theme"
  | "shell";

export type EventFilter = string;

export interface SmeltState {
  revision: number;
  locale: string;
  system_locale: string;
  locales: Array<{ code: string; name: string }>;
  window_active: boolean;
  notification_permission:
    | { kind: "checking" }
    | { kind: "not_requested" }
    | { kind: "granted"; banners: boolean; sound: boolean; badge: boolean }
    | { kind: "denied" }
    | { kind: "unavailable"; reason: string }
    | { kind: "unsupported" };
  machines_meta: any[];
  sessions_data: any[];
  connections: any;
  active_context: { project: any; session: any };
  pages: any[];
  views: any[];
  commands: any[];
  js_views: Record<string, { plugin: string; view: string }>;
  safe_areas: Record<string, any>;
  bar_insets: Record<string, { left: number; right: number }>;
  theme: any;
  texts?: Record<string, Record<string, string>>;
  shell?: any;
}

const DEFAULT_STATE: SmeltState = {
  revision: 0,
  locale: "en",
  system_locale: "en",
  locales: [],
  window_active: true,
  notification_permission: { kind: "unsupported" },
  machines_meta: [],
  sessions_data: [],
  connections: { remotes: [], load_error: null },
  active_context: { project: null, session: null },
  pages: [],
  views: [],
  commands: [],
  js_views: {},
  safe_areas: {},
  bar_insets: {},
  // 主题只有宿主那一份（`host.rs` 的 `theme`），首份状态里一定带；到之前插件不上报 ready、视图不挂。
  theme: null,
  texts: {},
};

const stateSignal = signal<SmeltState>(DEFAULT_STATE);

// 按顶层切片拆分的细粒度信号：仅当对应切片在宿主侧真正变化并推送过来时更新，
// 零 JSON.stringify 全量重比对；插件与主壳各自只订实际读取的切片。
const hostReadySlice = signal<boolean>(false);
const localeSlice = signal<string>(DEFAULT_STATE.locale);
const systemLocaleSlice = signal<string>(DEFAULT_STATE.system_locale);
const localesSlice = signal<Array<{ code: string; name: string }>>(DEFAULT_STATE.locales);
const windowActiveSlice = signal<boolean>(DEFAULT_STATE.window_active);
const notificationPermissionSlice = signal<SmeltState["notification_permission"]>(
  DEFAULT_STATE.notification_permission,
);
const machinesMetaSlice = signal<any[]>(DEFAULT_STATE.machines_meta);
const sessionsDataSlice = signal<any[]>(DEFAULT_STATE.sessions_data);
const connectionsSlice = signal<any>(DEFAULT_STATE.connections);
const activeContextSlice = signal<{ project: any; session: any }>(DEFAULT_STATE.active_context);
const pagesSlice = signal<any[]>(DEFAULT_STATE.pages);
const viewsSlice = signal<any[]>(DEFAULT_STATE.views);
const commandsSlice = signal<any[]>(DEFAULT_STATE.commands);
const jsViewsSlice = signal<Record<string, { plugin: string; view: string }>>(DEFAULT_STATE.js_views);
const safeAreasSlice = signal<Record<string, any>>(DEFAULT_STATE.safe_areas);
const themeSlice = signal<any>(DEFAULT_STATE.theme);
let themeArrived: () => void = () => {};
setHostThemeArrived(
  new Promise<void>((resolve) => {
    themeArrived = resolve;
  }),
);
const textsSlice = signal<Record<string, Record<string, string>>>({});
const shellSlice = signal<any>(undefined);

const sliceRevision: Record<string, number> = {};

let cachedTexts: Record<string, Record<string, string>> = {};
let handlersAttached = false;
let initStarted = false;
let initScheduled = false;
let initPromise: Promise<SmeltState> | null = null;
let fallbackInitTimer: ReturnType<typeof setTimeout> | null = null;

interface SliceListenerEntry {
  slices: Set<string>;
  fn: (changed: readonly StateSlice[]) => void;
}
const sliceListeners = new Set<SliceListenerEntry>();

const acpEventListeners = new Set<(event: any) => void>();

const subscribedSlices = new Set<string>();
const pendingAddSlices = new Set<string>();
let pendingEventSync = false;
let subscribeScheduled = false;
let inFlightSubscribe: Promise<void> | null = null;

function flushSubscribe(): Promise<void> {
  subscribeScheduled = false;
  if (!initStarted || initScheduled) {
    return inFlightSubscribe ?? Promise.resolve();
  }
  const payload: Record<string, unknown> = {};
  if (pendingAddSlices.size > 0) {
    payload.add_slices = [...pendingAddSlices];
    pendingAddSlices.clear();
  }
  if (pendingEventSync) {
    payload.events_all = false;
    payload.event_types = collectEventTypes();
    pendingEventSync = false;
  }
  if (Object.keys(payload).length === 0) {
    return inFlightSubscribe ?? Promise.resolve();
  }
  const base = inFlightSubscribe ?? Promise.resolve();
  const tracked = base
    .then(() =>
      host
        .call("subscribe", payload)
        .then((res: any) => {
          if (res && typeof res === "object" && res.patch && typeof res.patch === "object") {
            if (Object.keys(res.patch).length > 0) {
              const rev = typeof res.revision === "number" ? res.revision : undefined;
              applyState(res.patch, rev);
            }
          }
        })
        .catch(() => {}),
    )
    .finally(() => {
      if (inFlightSubscribe === tracked) {
        inFlightSubscribe = null;
      }
    });
  inFlightSubscribe = tracked;
  return tracked;
}

function scheduleSubscribe() {
  if (!initStarted || initScheduled || subscribeScheduled) return;
  subscribeScheduled = true;
  queueMicrotask(() => {
    void flushSubscribe();
  });
}

function wantSlice(key: StateSlice) {
  if (subscribedSlices.has(key)) return;
  subscribedSlices.add(key);
  pendingAddSlices.add(key);
  scheduleSubscribe();
}

function syncEventSubscription() {
  pendingEventSync = true;
  scheduleSubscribe();
}

function notifySliceListeners(changedKeys: readonly string[]) {
  if (changedKeys.length === 0 || sliceListeners.size === 0) return;
  for (const entry of [...sliceListeners]) {
    const matched = changedKeys.filter((k) => entry.slices.has(k)) as StateSlice[];
    if (matched.length === 0) continue;
    try {
      entry.fn(matched);
    } catch (error) {
      console.error("[smelt/sdk] slice listener error:", error);
    }
  }
}

function canAcceptSlice(key: string, rev: number): boolean {
  if (rev > 0 && sliceRevision[key] !== undefined && rev < sliceRevision[key]) {
    return false;
  }
  if (rev > 0) sliceRevision[key] = rev;
  return true;
}

function applyState(next: any, explicitRev?: number) {
  if (!next || typeof next !== "object") return;
  const current = stateSignal();
  const rev =
    typeof explicitRev === "number"
      ? explicitRev
      : typeof next.revision === "number"
        ? next.revision
        : 0;
  if (rev > 0 && !hostReadySlice()) {
    hostReadySlice.set(true);
  }
  const changedKeys: string[] = [];
  const merged: SmeltState = { ...current };

  let hasTextPatch = false;
  if (next.texts && typeof next.texts === "object" && canAcceptSlice("texts", rev)) {
    cachedTexts = next.texts;
    textsSlice.set(next.texts);
    merged.texts = next.texts;
    hasTextPatch = true;
  } else {
    merged.texts = cachedTexts;
  }
  if (
    next.texts_by_plugin &&
    typeof next.texts_by_plugin === "object" &&
    canAcceptSlice("texts_by_plugin", rev)
  ) {
    setTextsByPlugin(next.texts_by_plugin);
    hasTextPatch = true;
  }

  if ("locale" in next && canAcceptSlice("locale", rev)) {
    const val = typeof next.locale === "string" ? next.locale : DEFAULT_STATE.locale;
    if (localeSlice() !== val) localeSlice.set(val);
    merged.locale = val;
    changedKeys.push("locale");
  }
  if ("system_locale" in next && canAcceptSlice("system_locale", rev)) {
    const val =
      typeof next.system_locale === "string" ? next.system_locale : DEFAULT_STATE.system_locale;
    if (systemLocaleSlice() !== val) systemLocaleSlice.set(val);
    merged.system_locale = val;
    changedKeys.push("system_locale");
  }
  if ("window_active" in next && canAcceptSlice("window_active", rev)) {
    const val = next.window_active !== false;
    if (windowActiveSlice() !== val) windowActiveSlice.set(val);
    merged.window_active = val;
    changedKeys.push("window_active");
  }
  if ("locales" in next && canAcceptSlice("locales", rev)) {
    const val = next.locales ?? [];
    localesSlice.set(val);
    merged.locales = val;
    changedKeys.push("locales");
  }
  if ("notification_permission" in next && canAcceptSlice("notification_permission", rev)) {
    const val = next.notification_permission ?? { kind: "unsupported" };
    notificationPermissionSlice.set(val);
    merged.notification_permission = val;
    changedKeys.push("notification_permission");
  }
  if ("machines_meta" in next && canAcceptSlice("machines_meta", rev)) {
    const val = next.machines_meta ?? [];
    machinesMetaSlice.set(val);
    merged.machines_meta = val;
    changedKeys.push("machines_meta");
  }
  if ("sessions_data" in next && canAcceptSlice("sessions_data", rev)) {
    const val = next.sessions_data ?? [];
    sessionsDataSlice.set(val);
    merged.sessions_data = val;
    changedKeys.push("sessions_data");
  } else if ("sessions_data_delta" in next && canAcceptSlice("sessions_data", rev)) {
    const delta = next.sessions_data_delta;
    const keyOf = (m: any) => (m == null ? "" : String(m));
    const prevList = Array.isArray(sessionsDataSlice()) ? sessionsDataSlice() : [];
    const byMachine = new Map<string, any>();
    for (const item of prevList) {
      byMachine.set(keyOf(item?.machine), item);
    }
    for (const item of delta?.upserted ?? []) {
      byMachine.set(keyOf(item?.machine), item);
    }
    const order = Array.isArray(delta?.order) ? delta.order : [];
    const val: any[] = [];
    for (const m of order) {
      const found = byMachine.get(keyOf(m));
      if (found !== undefined) val.push(found);
    }
    sessionsDataSlice.set(val);
    merged.sessions_data = val;
    changedKeys.push("sessions_data");
  }
  if ("connections" in next && canAcceptSlice("connections", rev)) {
    const val = next.connections ?? { remotes: [], load_error: null };
    connectionsSlice.set(val);
    merged.connections = val;
    changedKeys.push("connections");
  }
  if ("active_context" in next && canAcceptSlice("active_context", rev)) {
    const val = next.active_context ?? { project: null, session: null };
    activeContextSlice.set(val);
    merged.active_context = val;
    changedKeys.push("active_context");
  }
  if ("pages" in next && canAcceptSlice("pages", rev)) {
    const val = next.pages ?? [];
    pagesSlice.set(val);
    merged.pages = val;
    changedKeys.push("pages");
  }
  if ("views" in next && canAcceptSlice("views", rev)) {
    const val = next.views ?? [];
    viewsSlice.set(val);
    merged.views = val;
    changedKeys.push("views");
  }
  if ("commands" in next && canAcceptSlice("commands", rev)) {
    const val = next.commands ?? [];
    commandsSlice.set(val);
    merged.commands = val;
    changedKeys.push("commands");
  }
  if ("js_views" in next && canAcceptSlice("js_views", rev)) {
    const val = next.js_views ?? {};
    jsViewsSlice.set(val);
    merged.js_views = val;
    changedKeys.push("js_views");
  }
  if ("safe_areas" in next && canAcceptSlice("safe_areas", rev)) {
    const val = next.safe_areas ?? {};
    safeAreasSlice.set(val);
    merged.safe_areas = val;
    changedKeys.push("safe_areas");
  }
  if ("theme" in next && canAcceptSlice("theme", rev)) {
    const val = next.theme;
    themeSlice.set(val);
    if (val) themeArrived();
    merged.theme = val;
    changedKeys.push("theme");
  }
  if (next.shell !== undefined && canAcceptSlice("shell", rev)) {
    shellSlice.set(next.shell);
    merged.shell = next.shell;
    changedKeys.push("shell");
  }
  if (changedKeys.length === 0 && !hasTextPatch) return;
  merged.revision = Math.max(current.revision, rev);
  stateSignal.set(merged);
  notifySliceListeners(changedKeys);
}

function attachHostHandlers() {
  if (handlersAttached) return;
  handlersAttached = true;
  host.on("smelt.state", (next: any) => applyState(next));
  host.on("smelt.event", (event: any) => dispatchIncomingEvent(event));
  host.on("smelt.acp.event", (event: any) => {
    for (const listener of [...acpEventListeners]) {
      try {
        listener(event);
      } catch (error) {
        console.error("[smelt/sdk] acp event listener error:", error);
      }
    }
  });
  host.on("smelt.command", (call: any) => dispatchIncomingCommand(call));
}

export function initHost(initialSlices?: readonly StateSlice[]): Promise<SmeltState> {
  attachHostHandlers();
  if (fallbackInitTimer !== null) {
    clearTimeout(fallbackInitTimer);
    fallbackInitTimer = null;
  }
  const newlyAdded: string[] = [];
  if (Array.isArray(initialSlices)) {
    for (const slice of initialSlices) {
      if (!subscribedSlices.has(slice)) {
        subscribedSlices.add(slice);
        if (initStarted && !initScheduled) {
          pendingAddSlices.add(slice);
          newlyAdded.push(slice);
        }
      }
    }
  }
  if (!initStarted) {
    initStarted = true;
    initScheduled = true;
    // 首份状态始终带上 locale 和 theme：`t()` 要语言，视图要主题才画得出来（ready 等主题到）；
    // 其余切片等同一微任务周期的并发 `initHost` 合并后一次拉取。
    subscribedSlices.add("locale");
    subscribedSlices.add("theme");
    initPromise = Promise.resolve().then(() => {
      initScheduled = false;
      pendingAddSlices.clear();
      const requestSlices = [...subscribedSlices];
      if (hasEventListeners()) pendingEventSync = true;
      const eventSync = pendingEventSync ? flushSubscribe() : Promise.resolve();
      return Promise.all([
        host
          .call("state", { slices: requestSlices })
          .then((initial: any) => {
            applyState(initial);
            return stateSignal();
          })
          .catch((error: unknown) => {
            console.error("[smelt/sdk] state() failed:", error);
            return stateSignal();
          }),
        eventSync,
      ]).then(([st]) => st);
    });
    return initPromise;
  }
  if (initScheduled) {
    return initPromise ?? Promise.resolve(stateSignal());
  }
  if (newlyAdded.length > 0 || pendingEventSync || inFlightSubscribe !== null) {
    return Promise.resolve(initPromise)
      .then(() => flushSubscribe())
      .then(() => stateSignal());
  }
  return initPromise ?? Promise.resolve(stateSignal());
}

// 立即挂载本地事件监听；仅当插件自己未显式调 `initHost(...)` 时在下一微任务/宏任务兜底拉默认状态。
setEventSyncHandler(() => syncEventSubscription());
attachHostHandlers();
fallbackInitTimer = setTimeout(() => {
  fallbackInitTimer = null;
  if (!initStarted) {
    void initHost();
  }
}, 0);

export function onSlices(
  slices: readonly StateSlice[],
  listener: (changed: readonly StateSlice[]) => void,
): () => void {
  const wanted = new Set<string>(slices);
  for (const key of wanted) wantSlice(key as StateSlice);
  const entry: SliceListenerEntry = { slices: wanted, fn: listener };
  sliceListeners.add(entry);
  return () => sliceListeners.delete(entry);
}

export function onEvent(
  listener: (event: any) => void,
  types: readonly EventFilter[],
): () => void {
  return registerEventListener(null, listener, types);
}

export function onAcpEvent(listener: (event: any) => void): () => void {
  acpEventListeners.add(listener);
  return () => acpEventListeners.delete(listener);
}

// ---- ACP 活体订阅与 Mermaid SVG 能力 ----

export function acpSubscribe(params: {
  subId: string;
  machine: string | null;
  reuseKey?: string;
  launch: Record<string, unknown>;
}): Promise<void> {
  return host.call("acp.subscribe", {
    sub_id: params.subId,
    machine: params.machine ?? null,
    reuse_key: params.reuseKey ?? null,
    launch: params.launch,
  });
}

export function acpUnsubscribe(subId: string, reuseKey?: string): Promise<void> {
  return host.call("acp.unsubscribe", {
    sub_id: subId,
    reuse_key: reuseKey ?? null,
  });
}

export function acpAction(subId: string, action: unknown, imageRefs?: string[]): Promise<void> {
  return host.call("acp.action", {
    sub_id: subId,
    action,
    image_refs: imageRefs ?? [],
  });
}

export function acpHistory(
  machine: string | null,
  sessionId: string,
  before: number,
  limit = 100,
): Promise<any> {
  return host.call("acp.history", {
    machine: machine ?? null,
    session_id: sessionId,
    before,
    limit,
  });
}

export function acpRestart(machine: string | null, sessionId: string): Promise<void> {
  return host.call("acp.restart", {
    machine: machine ?? null,
    session_id: sessionId,
  });
}

export function acpPasteImage(params: {
  dataBase64?: string;
  mime?: string;
  path?: string;
}): Promise<any> {
  return host.call("acp.paste_image", {
    data_base64: params.dataBase64 ?? null,
    mime: params.mime ?? null,
    path: params.path ?? null,
  });
}

export function acpClipboardImages(): Promise<{ images: any[]; paths: string[] }> {
  return host.call("acp.clipboard_images", {});
}

export function acpImageUri(refId: string): Promise<string | null> {
  return host.call("acp.image_uri", { ref_id: refId });
}

export function renderMermaidSvg(source: string): Promise<
  | { ok: true; data_uri: string; width: number; height: number }
  | { ok: false; error: string }
> {
  return host.call("mermaid.render", { source });
}

export function onCommand(handler: (call: { name: string; args: any }) => any): void {
  registerDefaultCommandHandler(handler);
}

// ---- 国际化 ----

export function locale(): string {
  wantSlice("locale");
  return localeSlice() || "en";
}

setLocaleReader(() => locale());

export function t(key: string, params?: Record<string, unknown>): string {
  wantSlice("locale");
  const curLocale = localeSlice() || "en";
  const tables = textsSlice();
  const dict = Object.keys(tables).length > 0 ? tables : cachedTexts;
  return resolveTranslation(dict, curLocale, key, params);
}

/** 构造给 registerViews / registerCommands 用的多语言表（宿主按当前语言动态取）。 */
export function text(key: string): Record<string, string> {
  const tables = textsSlice();
  const dict = Object.keys(tables).length > 0 ? tables : cachedTexts;
  return buildTextTable(dict, key);
}

// ---- 宿主读取与动作（均绑定细粒度切片信号） ----

/** 细粒度主题快照：只在主题或窗口 `rem` 真正变化时触发订阅方。 */
export function theme(): any {
  wantSlice("theme");
  return themeSlice();
}

export const currentTheme = theme;

export function jsViews(): Record<string, { plugin: string; view: string }> {
  wantSlice("js_views");
  return jsViewsSlice();
}

export function hostReady(): boolean {
  return hostReadySlice();
}

export function texts(): Record<string, Record<string, string>> {
  const tables = textsSlice();
  return Object.keys(tables).length > 0 ? tables : cachedTexts;
}

export function shellState(): any {
  wantSlice("shell");
  return shellSlice();
}

export function machineMeta(): any[] {
  wantSlice("machines_meta");
  return machinesMetaSlice();
}

export function machineMetaEntry(machine: string | null | undefined): any {
  const target = machine ?? null;
  return machineMeta().find((m) => (m.machine ?? null) === target) ?? null;
}

function sessionEntry(machine: string | null | undefined): any {
  wantSlice("sessions_data");
  const target = machine ?? null;
  return sessionsDataSlice().find((m) => (m?.machine ?? null) === target) ?? null;
}

export function machines(): any[] {
  const metas = machineMeta();
  wantSlice("sessions_data");
  const sessions = sessionsDataSlice();
  if (sessions.length === 0) {
    return metas.map((m) => ({ ...m, directory: null, states: [], facts: [], attention: null }));
  }
  const byMachine = new Map<string | null, any>();
  for (const s of sessions) {
    byMachine.set(s?.machine ?? null, s);
  }
  return metas.map((m) => {
    const s = byMachine.get(m?.machine ?? null);
    return {
      ...m,
      directory: s?.directory ?? null,
      states: s?.states ?? [],
      facts: s?.facts ?? [],
      attention: s?.attention ?? null,
    };
  });
}

export function machineEntry(machine: string | null | undefined): any {
  const meta = machineMetaEntry(machine);
  wantSlice("sessions_data");
  const s = sessionEntry(machine);
  if (!meta && !s) return null;
  return {
    ...(meta ?? { machine: machine ?? null }),
    directory: s?.directory ?? null,
    states: s?.states ?? [],
    facts: s?.facts ?? [],
    attention: s?.attention ?? null,
  };
}

export function pathStyle(machine: string | null | undefined): string {
  return machineMetaEntry(machine)?.path_style ?? (localOs() === "win32" ? "windows" : "posix");
}

export function workspaceDirectory(machine: string | null | undefined): any {
  return sessionEntry(machine)?.directory ?? null;
}

export function sessionStates(machine: string | null | undefined): any[] {
  return sessionEntry(machine)?.states ?? [];
}

export function conversationFacts(machine: string | null | undefined): any[] {
  return sessionEntry(machine)?.facts ?? [];
}

export interface AttentionItem {
  /** approval、input、success、failure、bell、notice，或比这个客户端新的种类。 */
  kind: string;
  title: string;
  message: string;
  note?: unknown;
}

export interface SessionAttention {
  session_id: string;
  action?: AttentionItem;
  unread?: AttentionItem;
}

/**
 * 那台机器上有提醒的会话（运行时推来的镜像）：[{ session_id, action?, unread? }]。action 是等你
 * 处理的事（看过也留着），unread 是还没看过的那条。还没收到快照是 null，这时说不准谁看过了。
 * 变了发 sessions_changed。刚来、该响一下的那条另发 attention_alert 事件。
 */
export function sessionAttention(machine: string | null | undefined): SessionAttention[] | null {
  return sessionEntry(machine)?.attention ?? null;
}

export function connections(): any {
  wantSlice("connections");
  return connectionsSlice();
}

export function activeContext(): { project: any; session: any } {
  wantSlice("active_context");
  return activeContextSlice();
}

export function views(): any[] {
  wantSlice("views");
  return viewsSlice();
}

export function pages(): any[] {
  wantSlice("pages");
  return pagesSlice();
}

export function commands(): any[] {
  wantSlice("commands");
  return commandsSlice();
}

export function command(id: string): any {
  return commands().find((c) => c.id === id) ?? null;
}

export function viewBadge(id: string): string | null {
  return views().find((v) => v.id === id)?.badge ?? null;
}

export function viewLaidOut(id: string): boolean {
  return Boolean(views().find((v) => v.id === id)?.laid_out);
}

export function viewShown(id: string): boolean {
  return Boolean(views().find((v) => v.id === id)?.shown);
}

export function safeArea(viewId: string): {
  title_bar: boolean;
  top_left: number;
  top_right: number;
  bottom_left: number;
  bottom_right: number;
  bar_insets: { left: number; right: number };
} {
  wantSlice("safe_areas");
  return (
    safeAreasSlice()?.[viewId] ?? {
      title_bar: false,
      top_left: 0,
      top_right: 0,
      bottom_left: 0,
      bottom_right: 0,
      bar_insets: { left: 8, right: 8 },
    }
  );
}

export function run(command: string, args: unknown = null): Promise<any> {
  return host.call("run", { command, args });
}

export function call(
  machine: string | null,
  method: string,
  params: unknown = {},
  timeoutMs?: number,
): Promise<any> {
  const payload: Record<string, unknown> = { machine, method, params };
  if (timeoutMs != null) payload.timeout_ms = timeoutMs;
  return host.call("call", payload);
}

/**
 * 同 `call`，回复里 `blobs` 这些位置（JSON 指针，如 `/patch`、`/diff/text`）上的长文本以
 * `Blob` 交来，不进插件的 JS 堆：用 `URL.createObjectURL` 交给原生组件（`SmeltDiffView` 的
 * `patchUrl`），换内容时 `URL.revokeObjectURL`。那里没有文本时照原样。
 */
export function callWithBlobs(
  machine: string | null,
  method: string,
  params: unknown,
  blobs: string[],
  timeoutMs?: number,
): Promise<any> {
  const payload: Record<string, unknown> = { machine, method, params, blobs };
  if (timeoutMs != null) payload.timeout_ms = timeoutMs;
  return host.call("call", payload);
}

export type CallDetailResult<T = any> =
  | { ok: true; value: T }
  | { ok: false; code: string | null; message: string };

export function callDetail<T = any>(
  machine: string | null,
  method: string,
  params: unknown = {},
  timeoutMs?: number,
): Promise<CallDetailResult<T>> {
  const payload: Record<string, unknown> = { machine, method, params, detailed: true };
  if (timeoutMs != null) payload.timeout_ms = timeoutMs;
  return host.call("call", payload);
}

export function request(
  machine: string | null,
  payload: Record<string, unknown>,
  timeoutMs?: number,
): Promise<any> {
  const body: Record<string, unknown> = { machine, payload };
  if (timeoutMs != null) body.timeout_ms = timeoutMs;
  return host.call("request", body);
}

export function windowActive(): boolean {
  wantSlice("window_active");
  return windowActiveSlice();
}

export function notificationPermission(): SmeltState["notification_permission"] {
  wantSlice("notification_permission");
  return notificationPermissionSlice();
}

export function setStatusMenu(menu: unknown | null): Promise<void> {
  return host.call("set_status_menu", { menu });
}

export function setActiveContext(context: {
  project: { machine: string | null; root: string } | null;
  session: { machine: string | null; id: string; at_latest: boolean } | null;
}): Promise<void> {
  return host.call("set_active_context", context);
}

export function registerFileOpener(name: string): Promise<void> {
  return host.call("register_file_opener", { name });
}

export function openFile(location: {
  machine: string | null;
  path: string;
  line?: number | null;
}): Promise<void> {
  return host.call("open_file", {
    machine: location.machine ?? null,
    path: location.path,
    line: location.line ?? null,
  });
}

export function openUrl(url: string): Promise<void> {
  return host.call("open_url", { url });
}

export function emitEvent(name: string, payload: unknown = null): Promise<void> {
  return host.call("emit_event", { name, payload });
}

export function contextMenuItems(menu: string, machine: string | null, context: unknown): Promise<any[]> {
  return host.call("context_menu_items", { menu, machine, context });
}

export function clipboardWrite(value: string): Promise<void> {
  return host.call("clipboard.write", { text: value });
}

export interface ClipboardImageInput {
  ref_id?: string | null;
  data_b64?: string | null;
  mime?: string | null;
  data_uri?: string | null;
}

export function clipboardWriteImage(image: ClipboardImageInput): Promise<void> {
  return host.call("clipboard.write_image", image);
}

export function clipboardRead(): Promise<string | null> {
  return host.call("clipboard.read", {});
}

export function revealPath(path: string): Promise<void> {
  return host.call("fs.reveal_path", { path });
}

export interface ProcessMemoryBreakdown {
  total: number;
  graphics: number;
  heap: number;
  other: number;
}

export interface ProcessUsageSample {
  cpu_percent: number;
  memory_bytes: number;
  process_count: number;
  memory_breakdown: ProcessMemoryBreakdown | null;
}

export function processUsage(): Promise<ProcessUsageSample | null> {
  return host.call("sys.process_usage", {});
}

export interface DiscoverAppQuery {
  id: string;
  mac_apps?: string[];
  commands?: string[];
  with_icon?: boolean;
}

export interface DiscoveredApp {
  id: string;
  kind: "mac_app" | "command";
  target: string;
  icon?: string | null;
}

export function discoverApps(queries: DiscoverAppQuery[]): Promise<DiscoveredApp[]> {
  return host.call("os.discover_apps", { apps: queries });
}

export function launchPath(opts: {
  kind: "mac_app" | "command";
  target: string;
  path: string;
  require_dir?: boolean;
}): Promise<void> {
  return host.call("os.launch_path", opts);
}

export function locales(): Array<{ code: string; name: string }> {
  wantSlice("locales");
  return localesSlice();
}

export function systemLocale(): string {
  wantSlice("system_locale");
  return systemLocaleSlice() || "en";
}

export function registerViews(specs: any[]): Promise<void> {
  return host.call("register_views", { views: specs });
}

export function registerCommands(specs: any[], groupTitle?: string | Record<string, string>): Promise<void> {
  return host.call("register_commands", { commands: specs, group_title: groupTitle ?? null });
}

export function registerTopics(topics: string[]): Promise<void> {
  return host.call("register_topics", { topics });
}

export function registerLayouts(specs: any[]): Promise<void> {
  return host.call("register_layouts", { layouts: specs });
}

export function registerPages(specs: any[]): Promise<void> {
  return host.call("register_pages", { pages: specs });
}

export function registerContextMenuItems(menu: string, items: any[]): Promise<void> {
  return host.call("register_context_menu_items", { menu, items });
}

export function registerMenuItems(items: any[]): Promise<void> {
  return host.call("register_menu_items", { items });
}

export const store = {
  query: (sql: string, params: unknown[] = []): Promise<any[]> =>
    host.call("store.query", { sql, params }),
  execute: (sql: string, params: unknown[] = []): Promise<number> =>
    host.call("store.execute", { sql, params }),
  write: (statements: Array<{ sql: string; params?: unknown[] }>): Promise<number[]> =>
    host.call("store.write", { statements }),
  migrate: (steps: string[]): Promise<void> => host.call("store.migrate", { steps }),
};

/**
 * 事件驱动等待某台机器在线（不轮询、不受 `host.call` 30s 超时限制）：
 * 在线事实完全以宿主 `smelt_plugin_api::Host::machine_online` 随 `MachinesChanged` 推送的
 * `machines[].online` 为准；连续失败时按与 `daemon_reconnect_backoff` 一致的 250ms..8s 退避，
 * 重连事件会提前唤醒。
 */
export async function waitOnline(machine: string | null, failures = 0): Promise<boolean> {
  const target = machine ?? null;
  const status = (): boolean | null => {
    const entry = machineMetaEntry(target);
    if (entry?.online === true) return true;
    if (target != null && entry == null && hostReady()) return false;
    return null;
  };
  if (failures > 0) {
    const backoffMs = Math.min(8_000, 250 * Math.pow(2, Math.min(failures - 1, 5)));
    await new Promise<void>((resolve) => {
      let done = false;
      const finish = () => {
        if (done) return;
        done = true;
        unsubEvent();
        clearTimeout(timer);
        resolve();
      };
      const unsubEvent = onEvent(
        (event) => {
          if (event?.type === "reconnected" && (event.machine ?? null) === target) {
            finish();
          }
        },
        ["reconnected"],
      );
      // 轮询例外：运行时在线但上一次调用失败，没有「可以再试了」的事件可等；250ms 起翻倍，上限 8s，重连事件会提前叫醒。
      const timer = setTimeout(finish, backoffMs);
    });
  }
  const immediate = status();
  if (immediate !== null) return immediate;
  return new Promise<boolean>((resolve) => {
    const check = () => {
      const next = status();
      if (next !== null) {
        unsubSlices();
        unsubEvent();
        resolve(next);
      }
    };
    const unsubSlices = onSlices(["machines_meta"], check);
    const unsubEvent = onEvent(check, ["machines_changed", "reconnected"]);
    check();
  });
}

// ---- 结构去重 computed（对象内容未变时复用旧引用，阻止无意义重挂） ----

export function stableMemo<T>(fn: () => T): () => T {
  let prevJson: string | undefined;
  let prevVal: T;
  return computed(() => {
    const next = fn();
    const nextJson = JSON.stringify(next);
    if (prevJson !== undefined && nextJson === prevJson) {
      return prevVal;
    }
    prevJson = nextJson;
    prevVal = next;
    return next;
  });
}

// ---- 宿主原生视图挂载（如设置页内嵌的宿主视图） ----

export function ViewInstance(props: {
  view: string;
  args?: unknown;
  seq?: number;
  reuseKey?: string;
  safeArea?: any;
  focusSeq?: number;
  onFocusIn?: () => void;
  class?: string;
  style?: any;
}) {
  const nativeSpec = stableMemo(() => ({
    view: props.view,
    args: props.args ?? null,
    ...(props.seq != null ? { seq: props.seq } : {}),
    ...(props.reuseKey ? { reuseKey: props.reuseKey } : {}),
  }));
  return (
    <View
      class={props.class}
      style={{
        display: "flex",
        flexDirection: "column",
        minWidth: 0,
        minHeight: 0,
        ...(props.style ?? {}),
      }}
    >
      <SmeltNativeView
        spec={nativeSpec()}
        safeArea={props.safeArea}
        focusSeq={props.focusSeq}
        onFocusIn={props.onFocusIn}
        style={{ flex: 1, minWidth: 0, minHeight: 0 }}
      />
    </View>
  );
}

function formatShortcut(raw: string | null | undefined): string | null {
  if (!raw) return null;
  const mac = localOs() === "darwin";
  const parts = raw.split("-").filter(Boolean);
  if (parts.length === 0) return null;
  const formatted = parts.map((part) => {
    const low = part.toLowerCase();
    if (low === "cmd" || low === "super" || low === "secondary") return mac ? "⌘" : "Ctrl";
    if (low === "ctrl" || low === "control") return mac ? "⌃" : "Ctrl";
    if (low === "alt" || low === "option") return mac ? "⌥" : "Alt";
    if (low === "shift") return mac ? "⇧" : "Shift";
    if (low === "enter" || low === "return") return mac ? "↵" : "Enter";
    if (low === "escape" || low === "esc") return mac ? "⎋" : "Esc";
    if (low === "backspace") return mac ? "⌫" : "Backspace";
    if (low === "tab") return mac ? "⇥" : "Tab";
    if (low === "up") return "↑";
    if (low === "down") return "↓";
    if (low === "left") return "←";
    if (low === "right") return "→";
    return part.length === 1 ? part.toUpperCase() : part;
  });
  return mac ? formatted.join("") : formatted.join("+");
}

/** 一等 NSH 矢量图标：原生支持 `color`（可继承父元素色）、`opacity`、`rotate`、`spin`、`class` 与 CSS 过渡。 */
export function SmeltIcon(props: {
  path?: string;
  size?: number;
  color?: string;
  opacity?: number;
  rotate?: number;
  spin?: boolean;
  class?: string;
  style?: any;
  testID?: string;
}) {
  const iconClass = () => {
    const parts: string[] = [];
    // 窗口失焦或开启系统「减弱动态效果」时暂停旋转动画，避免后台空转 60/120Hz 重绘。
    if (props.spin && lifecycle() === "active" && !reduceMotion()) parts.push("animate-spin");
    if (props.class) parts.push(props.class);
    return parts.length > 0 ? parts.join(" ") : undefined;
  };
  return (
    <NativeSvgIcon
      path={props.spin ? "icons/loader.svg" : (props.path ?? "")}
      class={iconClass()}
      testID={props.testID}
      style={{
        width: props.size ?? 15,
        height: props.size ?? 15,
        flexShrink: 0,
        pointerEvents: "none",
        ...(props.color != null ? { color: props.color } : {}),
        ...(props.opacity != null && props.opacity !== 1 ? { opacity: props.opacity } : {}),
        ...(props.rotate != null && props.rotate !== 0
          ? { transform: [{ rotate: `${props.rotate}deg` }] }
          : {}),
        ...(props.style ?? {}),
      }}
    />
  );
}

// 展开收起（docs/ui-design-language.md「动效」disclosure 档）：300ms 临界阻尼弹簧，同 Rust 的
// `kit::MOTION.disclosure`（`Spring::new(300ms)`：刚度 (2π/0.3)²、阻尼 2·2π/0.3）。
const DISCLOSURE_SPRING = { stiffness: 438.6, damping: 41.9 };

/**
 * 展开收起：`nsh/base` 的 `Collapsible` 带上 Smelt 的展开动效，内容区高度按 `disclosure` 弹簧弹开弹收，
 * 下面的东西跟着挪。收起时内容区零高、仍在布局里，标题和内容之间的距离写成内容的上内边距，别用 `gap`。
 */
export function SmeltDisclosure(props: any) {
  return <Collapsible {...props} reveal revealSpring={DISCLOSURE_SPRING} />;
}

/** 展开收起的指示箭头：收起时朝右，展开时转到朝下，和内容同一时长转过去。配 `SmeltDisclosure` 用。 */
export function SmeltDisclosureChevron(props: {
  open: boolean;
  size?: number;
  color?: string;
  opacity?: number;
  testID?: string;
}) {
  return (
    <SmeltIcon
      path="icons/chevron-right.svg"
      size={props.size ?? 12}
      color={props.color}
      opacity={props.opacity}
      testID={props.testID}
      class="transition-transform duration-[300ms] ease-[cubic-bezier(0.32,0.72,0,1)]"
      rotate={props.open ? 90 : 0}
    />
  );
}

// ---- 开关、复选框、单选：选中一律铺强调色 `accent`，和 smelt-ui 的 Switch / Checkbox / Radio 同一套 ----

// 控件档动效（docs/ui-design-language.md「动效」control 档）：200ms。
const CONTROL_TRANSITION = "duration-[200ms] ease-[cubic-bezier(0.32,0.72,0,1)]";
const FOCUS_RING =
  "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background";
// 复选框、单选整行可点，焦点环只圈指示块：行上挂 `group`，指示块跟着它的键盘焦点。
const INDICATOR_FOCUS_RING =
  "group-focus-visible:ring-2 group-focus-visible:ring-ring group-focus-visible:ring-offset-1 group-focus-visible:ring-offset-background";

/** 开关：一项设置立刻生效的开或关。36×16 的槽、21×14 的胶囊钮，按住时钮伸长。 */
export function SmeltSwitch(props: {
  checked: boolean;
  disabled?: boolean;
  accessibilityLabel?: string;
  testID?: string;
  onChange: (checked: boolean) => void;
}) {
  const p = () => theme().palette;
  return (
    <BaseSwitch
      testID={props.testID}
      checked={props.checked}
      disabled={props.disabled}
      accessibilityLabel={props.accessibilityLabel}
      onChange={({ checked }: any) => props.onChange(Boolean(checked))}
      class={`shrink-0 rounded-full disabled:opacity-50 ${FOCUS_RING}`}
      trackClass={`relative rounded-full shadow-switch-off checked:shadow-switch-on transition-colors ${CONTROL_TRANSITION}`}
      trackStyle={{ width: 36, height: 16, backgroundColor: props.checked ? p().accent : p().track }}
      thumbClass={`absolute rounded-full shadow-elements origin-left checked:origin-right checked:translate-x-[13px] active:scale-x-125 transition-transform ${CONTROL_TRANSITION}`}
      thumbStyle={{
        top: 1,
        left: 1,
        width: 21,
        height: 14,
        backgroundColor: props.checked ? p().knob_on : p().knob,
      }}
    />
  );
}

/** 复选框：16 见方、4 圆角，勾上铺强调色、压白勾。`children` 是标签，点标签和点框一样。 */
export function SmeltCheckbox(props: {
  checked: boolean;
  disabled?: boolean;
  accessibilityLabel?: string;
  testID?: string;
  onChange: (checked: boolean) => void;
  class?: string;
  style?: any;
  children?: any;
}) {
  const p = () => theme().palette;
  return (
    <BaseCheckbox
      testID={props.testID}
      checked={props.checked}
      disabled={props.disabled}
      accessibilityLabel={props.accessibilityLabel}
      onChange={({ checked }: any) => props.onChange(Boolean(checked))}
      class={`group flex-row items-center gap-2 disabled:opacity-50 ${props.class ?? ""}`}
      style={props.style}
      indicatorClass={`items-center justify-center shrink-0 border ${INDICATOR_FOCUS_RING}`}
      indicatorStyle={{
        width: 16,
        height: 16,
        borderRadius: theme().radius.sm,
        borderColor: props.checked ? p().accent : p().hair_strong,
        backgroundColor: props.checked ? p().accent : "transparent",
      }}
      indicator={
        props.checked ? <SmeltIcon path="icons/check.svg" size={10} color={p().on_accent} /> : undefined
      }
    >
      {props.children}
    </BaseCheckbox>
  );
}

/** 单选：16 的圆，选中铺强调色、压 6 的白点。放在 `nsh/base` 的 `RadioGroup` 里；`children` 是标签。 */
export function SmeltRadio(props: {
  checked: boolean;
  disabled?: boolean;
  accessibilityLabel?: string;
  testID?: string;
  onChange: () => void;
  class?: string;
  style?: any;
  children?: any;
}) {
  const p = () => theme().palette;
  return (
    <BaseRadio
      testID={props.testID}
      checked={props.checked}
      disabled={props.disabled}
      accessibilityLabel={props.accessibilityLabel}
      onChange={() => props.onChange()}
      class={`group flex-row items-center gap-2 disabled:opacity-50 ${props.class ?? ""}`}
      style={props.style}
    >
      <View
        class={`items-center justify-center shrink-0 rounded-full border ${INDICATOR_FOCUS_RING}`}
        style={{
          width: 16,
          height: 16,
          borderColor: props.checked ? p().accent : p().hair_strong,
          backgroundColor: props.checked ? p().accent : p().bg,
        }}
      >
        {props.checked ? (
          <View class="rounded-full" style={{ width: 6, height: 6, backgroundColor: p().on_accent }} />
        ) : null}
      </View>
      {props.children}
    </BaseRadio>
  );
}

export interface SmeltButtonProps {
  label?: string;
  icon?: string;
  trailingIcon?: string;
  tooltip?: string;
  accessibilityLabel?: string;
  command?: string;
  quiet?: boolean;
  primary?: boolean;
  danger?: boolean;
  dismiss?: boolean;
  floating?: boolean;
  circle?: boolean;
  small?: boolean;
  fill?: boolean;
  disabled?: boolean;
  loading?: boolean;
  selected?: boolean;
  toggled?: boolean;
  class?: string;
  style?: any;
  testID?: string;
  onPress?: (e?: any) => void;
  children?: any;
}

// ---- 共享 NSH 原生 Tailwind 浮层样式（全插件统一质感，零 JS 闭包、引擎侧直接解析） ----

// 投影一律用主题令牌（宿主按 elevation 给）：菜单、下拉、提示 `shadow-floating`，弹出卡片 `shadow-md`，
// 对话框 `shadow-overlay`，浮在内容上的按钮 `shadow-button-floating`。不用 Tailwind 自带的那几档。

/** 弹出卡片的样子：`surface` 底、1px 细边、`md` 投影。传给 `SmeltPopover` 的 `popupClass` 换掉的就是它。 */
export const POPUP_CLASS = "rounded-lg border border-border bg-popover p-1 shadow-md";
/** 菜单、下拉、选择框的样子：`soft` 底，收边的细圈在 `floating` 投影里。 */
export const MENU_CLASS = "rounded-lg bg-soft p-1 shadow-floating";
/** 提示的样子：同菜单。 */
export const TOOLTIP_CLASS = "rounded-(--radius) bg-soft px-2 py-1 shadow-floating";

// 浮层进出（docs/ui-design-language.md「动效」overlay 档）：入场 240ms、ease_snap 曲线；出场 120ms
// 淡出，比入场短，关掉的东西不该拖着。进出是浮层组件自己的行为，调用处只管样子。
// NSH 不认 `transition-[…]` 任意值，用 `transition`（色、透明度、投影、变换）：进出时只有透明度和位移在变。
const OVERLAY_TRANSITION =
  "transition duration-[240ms] ease-[cubic-bezier(0.32,0.72,0,1)]";
const OVERLAY_EXIT = "opacity-0 duration-[120ms] ease-in";
// 从触发点那一侧挪 4px 到位：按浮层实际落下的那一侧（NSH 的 `data-side`，第一帧前就定了），翻边也对。
const FROM_TRIGGER =
  "opacity-0 data-[side=bottom]:-translate-y-1 data-[side=top]:translate-y-1 " +
  "data-[side=right]:-translate-x-1 data-[side=left]:translate-x-1";
// 右键菜单贴着指针、提示跟着指针，没有可靠的「从哪来」，只淡入。
const FADE_IN = "opacity-0";

const overlayClass = (look: string | undefined, fallback: string) =>
  `${look ?? fallback} ${OVERLAY_TRANSITION}`;

/** 弹出卡片：`nsh/base` 的 `Popover` 带上 Smelt 的样子和进出动效。 */
export function SmeltPopover(props: any) {
  return (
    <Popover
      {...props}
      popupClass={overlayClass(props.popupClass, POPUP_CLASS)}
      popupStartingClass={FROM_TRIGGER}
      popupEndingClass={OVERLAY_EXIT}
    />
  );
}

/** 下拉菜单：`nsh/base` 的 `Menu` 带上 Smelt 的样子和进出动效；子菜单（`Submenu`）跟着它。 */
export function SmeltMenu(props: any) {
  return (
    <Menu
      {...props}
      popupClass={overlayClass(props.popupClass, MENU_CLASS)}
      popupStartingClass={FROM_TRIGGER}
      popupEndingClass={OVERLAY_EXIT}
    />
  );
}

/** 右键菜单：只淡入淡出。 */
export function SmeltContextMenu(props: any) {
  return (
    <ContextMenu
      {...props}
      popupClass={overlayClass(props.popupClass, MENU_CLASS)}
      popupStartingClass={FADE_IN}
      popupEndingClass={OVERLAY_EXIT}
    />
  );
}

/** 选择框：下拉列表同菜单。 */
export function SmeltSelect(props: any) {
  return (
    <Select
      {...props}
      popupClass={overlayClass(props.popupClass, MENU_CLASS)}
      popupStartingClass={FROM_TRIGGER}
      popupEndingClass={OVERLAY_EXIT}
    />
  );
}

/** 提示：只淡入淡出。 */
export function SmeltTooltip(props: any) {
  return (
    <Tooltip
      {...props}
      class={overlayClass(props.class, TOOLTIP_CLASS)}
      startingClass={FADE_IN}
      endingClass={OVERLAY_EXIT}
    />
  );
}

export const MENU_ITEM_CLASS =
  "flex-row items-center rounded-(--radius) px-2 hover:bg-accent focus:bg-accent";
export const INPUT_CLASS = "rounded-(--radius) border border-input bg-card px-2 text-foreground";
/** 对话框的面：`surface` 底，收边的细圈在 `overlay` 投影里。 */
export const DIALOG_SURFACE_CLASS = "rounded-xl bg-popover shadow-overlay";

/**
 * 对话框：`nsh/base` 的 `Dialog` 带上 Smelt 的进出和主题的遮罩色 `scrim`。整层（遮罩连卡片）淡入淡出，
 * 卡片同时从下面升 4px（`overlay` 档）。
 */
export function SmeltDialog(props: any) {
  return (
    <Dialog
      {...props}
      animation="fade"
      backdropClass="bg-scrim"
      class={`${props.class ?? ""} transition-transform duration-[240ms] ease-[cubic-bezier(0.32,0.72,0,1)]`}
      startingClass="translate-y-1"
    />
  );
}

/** 统一纯 JS/NSH 按钮（建在 `nsh/base <Button>` 上）：无 `BlockMouse` 悬停断层，完整遵循宿主主题色阶。 */
export function SmeltButton(props: SmeltButtonProps) {
  const look = theme;
  const iconOnly = () => !props.label && !props.children && Boolean(props.icon) && !props.trailingIcon;
  const interactive = () => !props.disabled && !props.loading;
  const hasTooltip = computed(() => Boolean(props.tooltip));
  const shortcut = () => (props.command ? formatShortcut(command(props.command)?.key) : null);

  const colors = computed(() => {
    const p = look().palette;
    const primary = iconOnly() ? p.button_submit : p.button_primary;
    const primaryFg = iconOnly() ? p.button_submit_foreground : p.button_primary_foreground;
    const neutral = p.button_secondary;
    const neutralFg = p.button_secondary_foreground;

    if (props.dismiss) {
      return {
        bg: "transparent",
        fg: p.faint,
        hoverBg: p.row_hover,
        hoverFg: p.text,
        pressBg: p.row_selected,
        pressFg: p.text,
        surfaceClass: "",
        disabledOpacity: 0.5,
      };
    }
    if (props.floating) {
      return {
        bg: p.surface,
        fg: p.text,
        hoverBg: p.soft,
        hoverFg: p.text,
        pressBg: p.soft_hover,
        pressFg: p.text,
        surfaceClass: "shadow-button-floating",
        disabledOpacity: 0.45,
      };
    }
    if (props.primary) {
      return {
        bg: primary[0],
        fg: primaryFg,
        hoverBg: primary[1],
        hoverFg: primaryFg,
        pressBg: primary[2],
        pressFg: primaryFg,
        surfaceClass: "shadow-button-primary",
        disabledOpacity: 0.35,
      };
    }
    if (props.danger && props.quiet) {
      return {
        bg: "transparent",
        fg: p.bad,
        hoverBg: p.row_hover,
        hoverFg: p.bad,
        pressBg: p.row_selected,
        pressFg: p.bad,
        surfaceClass: "",
        disabledOpacity: 0.5,
      };
    }
    if (props.danger) {
      return {
        bg: neutral[0],
        fg: p.bad,
        hoverBg: neutral[1],
        hoverFg: p.bad,
        pressBg: neutral[2],
        pressFg: p.bad,
        surfaceClass: "shadow-button",
        disabledOpacity: 0.5,
      };
    }
    if (props.quiet) {
      return {
        bg: props.selected ? p.row_selected : "transparent",
        fg: props.selected ? p.text : p.muted,
        hoverBg: p.row_hover,
        hoverFg: p.text,
        pressBg: p.row_selected,
        pressFg: p.text,
        surfaceClass: "",
        disabledOpacity: 0.5,
      };
    }
    return {
      bg: props.selected ? p.soft_hover : neutral[0],
      fg: neutralFg,
      hoverBg: neutral[1],
      hoverFg: neutralFg,
      pressBg: neutral[2],
      pressFg: neutralFg,
      surfaceClass: "shadow-button",
      disabledOpacity: 0.5,
    };
  });

  const metrics = computed(() => {
    const r = look().radius;
    const ty = look().type;
    if (props.dismiss) {
      return {
        width: 20,
        height: 20,
        px: 0,
        radius: r.md ?? 6,
        iconSize: 13,
        gap: 0,
        textStyle: ty.ui,
      };
    }
    if (props.circle && iconOnly()) {
      return props.small
        ? { width: 24, height: 24, px: 0, radius: 999, iconSize: 13, gap: 0, textStyle: ty.small }
        : { width: 28, height: 28, px: 0, radius: 999, iconSize: 15, gap: 0, textStyle: ty.ui };
    }
    if (iconOnly()) {
      return props.small
        ? { width: 22, height: 22, px: 0, radius: r.md ?? 6, iconSize: 14, gap: 0, textStyle: ty.small }
        : { width: 28, height: 28, px: 0, radius: r.md ?? 6, iconSize: 15, gap: 0, textStyle: ty.ui };
    }
    if (props.small) {
      return {
        width: undefined,
        height: 26,
        px: 9,
        radius: props.floating ? 999 : (r.md ?? 6),
        iconSize: 14,
        gap: 5,
        textStyle: props.quiet ? (ty.text_xsmall ?? ty.small) : (ty.text_xsmall_medium ?? ty.small_medium),
      };
    }
    return {
      width: undefined,
      height: 28,
      px: 12,
      radius: props.floating ? 999 : (r.lg ?? r.md ?? 8),
      iconSize: 15,
      gap: 6,
      textStyle: props.quiet ? ty.ui : ty.ui_medium,
    };
  });

  const a11yLabel = () => props.accessibilityLabel || props.label || props.tooltip || undefined;
  const buttonClass = () => {
    const parts: string[] = [];
    if (interactive() && colors().surfaceClass) parts.push(colors().surfaceClass);
    if (props.class) parts.push(props.class);
    return parts.length > 0 ? parts.join(" ") : undefined;
  };

  return (
    <BaseButton
      class={buttonClass()}
      testID={props.testID}
      disabled={!interactive()}
      selected={Boolean(props.selected || props.toggled)}
      accessibilityLabel={a11yLabel()}
      onPress={(e: any) => {
        if (!interactive()) return;
        props.onPress?.(e);
      }}
      tooltip={
        hasTooltip() ? (
          <SmeltTooltip>
            <View class="flex-row items-center gap-1.5">
              <Text class="text-xs text-foreground">{props.tooltip}</Text>
              <If when={shortcut() != null}>
                <Text class="text-xs text-muted-foreground">{shortcut() ?? ""}</Text>
              </If>
            </View>
          </SmeltTooltip>
        ) : undefined
      }
      style={{
        position: "relative",
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        justifyContent: "center",
        flexShrink: props.fill ? 1 : 0,
        ...(props.fill ? { width: "100%", minWidth: 0 } : {}),
        ...(metrics().width != null ? { width: metrics().width } : {}),
        minHeight: metrics().height,
        height: iconOnly() || props.dismiss ? metrics().height : undefined,
        paddingLeft: metrics().px,
        paddingRight: metrics().px,
        borderRadius: metrics().radius,
        backgroundColor: props.loading ? look().palette.soft_hover : colors().bg,
        color: props.loading ? look().palette.text : colors().fg,
        cursor: "default",
        ...(props.style ?? {}),
      }}
      hoverStyle={{
        backgroundColor: colors().hoverBg,
        color: colors().hoverFg,
      }}
      activeStyle={{
        backgroundColor: colors().pressBg,
        color: colors().pressFg,
      }}
      disabledStyle={{
        opacity: props.loading ? 1 : colors().disabledOpacity,
      }}
    >
      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "center",
          gap: metrics().gap,
          minWidth: 0,
          overflow: "hidden",
          opacity: props.loading ? 0 : 1,
          pointerEvents: "none",
        }}
      >
        <If when={Boolean(props.icon)}>
          <SmeltIcon path={props.icon} size={metrics().iconSize} />
        </If>
        <If when={Boolean(props.label)}>
          <Text
            style={{
              fontSize: metrics().textStyle.size,
              lineHeight: metrics().textStyle.line_height,
              fontWeight: metrics().textStyle.weight,
              numberOfLines: 1,
            }}
          >
            {props.label ?? ""}
          </Text>
        </If>
        {props.children}
        <If when={Boolean(props.trailingIcon)}>
          <SmeltIcon path={props.trailingIcon} size={12} opacity={0.72} />
        </If>
      </View>
      <If when={Boolean(props.loading)}>
        <View
          style={{
            position: "absolute",
            top: 0,
            left: 0,
            right: 0,
            bottom: 0,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            pointerEvents: "none",
          }}
        >
          <SmeltIcon spin={true} size={12} color={look().palette.text} />
        </View>
      </If>
    </BaseButton>
  );
}

export function SmeltWindowBar(props: { view: string }) {
  // 停靠区收起期间视图不可见，`safe_areas` 会暂时清空；保留上次可见时的标题带高度，
  // 避免重新展开侧栏时先 0px 压进红绿灯再跳下 34px。
  let lastVisibleArea: ReturnType<typeof safeArea> | null = null;
  const area = stableMemo(() => {
    const next = safeArea(props.view);
    if (next.title_bar || viewShown(props.view)) {
      lastVisibleArea = next;
      return next;
    }
    return lastVisibleArea ?? next;
  });
  return (
    <If when={area().title_bar}>
      <SmeltNativeWindowBar safeArea={area()} style={{ width: "100%", flexShrink: 0 }} />
    </If>
  );
}

const MERMAID_EXTENSIONS = [{ name: "mermaid", language: "mermaid" }];
const mermaidClientCache = new Map<
  string,
  { ok: true; data_uri: string; width: number; height: number } | { ok: false; error: string }
>();

function MermaidDiagramBlock(props: { source: string }) {
  const look = theme;
  const result = signal<
    | { ok: true; data_uri: string; width: number; height: number }
    | { ok: false; error: string }
    | null
  >(null);
  const previewOpen = signal(false);

  effect(() => {
    const src = String(props.source ?? "").trim();
    const themeSig = `${look().palette.bg}:${look().palette.text}`;
    if (!src) {
      result.set(null);
      return;
    }
    const cacheKey = `${themeSig}\0${src}`;
    const cached = mermaidClientCache.get(cacheKey);
    if (cached) {
      result.set(cached);
      return;
    }
    let stale = false;
    // 流式输出 Mermaid 代码块时每来几个字符就重跑：加 180ms 防抖避免反复调 Rust 渲染残缺图；
    // 且仅缓存成功结果，防止半截源码挤掉 128 项缓存里的有效图。
    const timer = setTimeout(() => {
      if (stale) return;
      renderMermaidSvg(src)
        .then((res) => {
          if (res.ok) {
            if (mermaidClientCache.size >= 128) {
              const oldest = mermaidClientCache.keys().next().value;
              if (oldest !== undefined) mermaidClientCache.delete(oldest);
            }
            mermaidClientCache.set(cacheKey, res);
          }
          if (!stale) result.set(res);
        })
        .catch((err: any) => {
          if (!stale) result.set({ ok: false, error: err?.message ?? String(err) });
        });
    }, 180);
    cleanup(() => {
      stale = true;
      clearTimeout(timer);
    });
  });

  const displaySize = computed(() => {
    const r = result();
    if (!r || !r.ok) return { width: 420, height: 140 };
    const maxW = 680;
    const scale = r.width > maxW ? maxW / r.width : 1;
    return {
      width: Math.max(120, Math.round(r.width * scale)),
      height: Math.max(64, Math.min(480, Math.round(r.height * scale))),
    };
  });

  return (
    <View class="my-1.5 p-2.5 rounded-lg border border-border bg-card">
      <If
        when={result() != null}
        else={<Text class="text-xs text-faint">…</Text>}
      >
        <If
          when={Boolean(result()?.ok)}
          else={<Text class="font-mono text-xs text-muted-foreground">{props.source}</Text>}
        >
          <View onPress={() => previewOpen.set(true)} class="items-center">
            <Image
              src={(result() as any)?.data_uri ?? ""}
              style={{
                width: displaySize().width,
                height: displaySize().height,
                maxWidth: "100%",
                objectFit: "contain",
              }}
            />
          </View>
          <SmeltDialog
            open={previewOpen()}
            onOpenChange={({ open }: any) => previewOpen.set(Boolean(open))}
            class="m-auto"
          >
            <If when={previewOpen() && Boolean(result()?.ok)}>
              <View
                class={`${DIALOG_SURFACE_CLASS} p-4 items-center justify-center`}
                style={{
                  width: 920,
                  maxWidth: "92%",
                  height: 640,
                  maxHeight: "84%",
                }}
              >
                <Image
                  src={(result() as any)?.data_uri ?? ""}
                  style={{ width: "100%", height: "100%", objectFit: "contain" }}
                />
              </View>
            </If>
          </SmeltDialog>
        </If>
      </If>
    </View>
  );
}

const STREAMING_MARKDOWN_OPTIONS = { streaming: true };
const DEFAULT_MARKDOWN_STYLE = { width: "100%", minWidth: 0 };
const renderMermaidCustom = (node: any) =>
  node?.name === "mermaid" ? <MermaidDiagramBlock source={node.text ?? ""} /> : null;

export function SmeltMarkdown(props: {
  markdown: string;
  cwd?: string | null;
  machine?: string | null;
  selectable?: boolean;
  streaming?: boolean;
  style?: any;
  styles?: Record<string, any>;
  onLinkError?: (error: unknown) => void;
}) {
  const look = theme;
  const tvStyles = computed(() => {
    const p = look().palette;
    const rem = look().rem ?? 16;
    const bodySize = (14 / 16) * rem;
    const bodyLineHeight = (23 / 16) * rem;
    const mono = look().fonts?.mono;
    const base: Record<string, any> = {
      root: {
        color: p.text,
        fontSize: bodySize,
        lineHeight: bodyLineHeight,
      },
      link: { color: p.link },
      code: {
        ...(mono ? { fontFamily: mono } : {}),
        backgroundColor: p.soft,
        borderRadius: 6,
      },
      codeBlock: {
        backgroundColor: p.side,
        borderRadius: 10,
        borderColor: p.hair,
      },
      codeBlockCode: {
        ...(mono ? { fontFamily: mono } : {}),
      },
      blockquote: {
        borderColor: p.hair_strong,
        color: p.muted,
      },
    };
    if (props.styles) {
      for (const [k, v] of Object.entries(props.styles)) {
        base[k] = { ...(base[k] ?? {}), ...(v ?? {}) };
      }
    }
    return base;
  });
  const rootStyle = computed(() =>
    props.style ? { ...DEFAULT_MARKDOWN_STYLE, ...props.style } : DEFAULT_MARKDOWN_STYLE,
  );
  return (
    <TextView
      markdown={props.markdown}
      options={props.streaming ? STREAMING_MARKDOWN_OPTIONS : undefined}
      streamingFade={Boolean(props.streaming)}
      selectable={props.selectable ?? true}
      links="report"
      extensions={MERMAID_EXTENSIONS}
      renderCustom={renderMermaidCustom}
      styles={tvStyles()}
      style={rootStyle()}
      onLinkPress={(event: { url?: string }) => {
        const url = event?.url ?? "";
        if (!url || url.startsWith("#")) return;
        const machine = props.machine ?? null;
        const parsed = parseFileLink(pathStyle(machine), url, props.cwd ?? null);
        if (parsed) {
          openFile({ machine, path: parsed.path, line: parsed.line }).catch((error) => {
            if (props.onLinkError) props.onLinkError(error);
            else console.error("[smelt/sdk] openFile failed:", error);
          });
        } else {
          openUrl(url).catch((error) => {
            if (props.onLinkError) props.onLinkError(error);
            else console.error("[smelt/sdk] openUrl failed:", error);
          });
        }
      }}
    />
  );
}

export function SmeltEditor(props: {
  editorRef?: (api: any) => void;
  value?: string;
  defaultValue?: string;
  language?: string;
  placeholder?: string;
  readOnly?: boolean;
  autoFocus?: boolean;
  lineNumbers?: boolean;
  softWrap?: boolean;
  folding?: boolean;
  activeLine?: boolean;
  indentGuides?: boolean;
  search?: any;
  fontSize?: number;
  lineHeight?: number;
  accessibilityLabel?: string;
  style?: any;
  styles?: Record<string, any>;
  classes?: Record<string, string>;
  onChange?: (event: { value: string }) => void;
  onSelectionChange?: (event: any) => void;
  onFocus?: () => void;
  onBlur?: () => void;
  onContextMenu?: (event: { position: { line: number; character: number }; x: number; y: number }) => void;
}) {
  const look = currentTheme;
  const editorStyles = computed(() => {
    const p = look().palette;
    const base: Record<string, any> = {
      gutter: { backgroundColor: "transparent" },
      lineNumber: { color: p.faint },
      activeLineNumber: { color: p.text },
      foldMarker: { color: p.faint },
      activeLine: { backgroundColor: p.row_hover },
      selection: { backgroundColor: p.row_selected },
      caret: { backgroundColor: p.ink },
      indentGuide: { backgroundColor: p.hair },
      placeholder: { color: p.faint },
    };
    if (props.styles) {
      for (const [k, v] of Object.entries(props.styles)) {
        base[k] = { ...(base[k] ?? {}), ...(v ?? {}) };
      }
    }
    return base;
  });
  return (
    <BaseEditor
      ref={props.editorRef}
      value={props.value}
      defaultValue={props.defaultValue}
      language={props.language}
      placeholder={props.placeholder}
      readOnly={props.readOnly}
      autoFocus={props.autoFocus}
      lineNumbers={props.lineNumbers ?? true}
      softWrap={props.softWrap ?? false}
      folding={props.folding ?? true}
      activeLine={props.activeLine ?? true}
      indentGuides={props.indentGuides ?? true}
      search={props.search}
      fontFamily={look().fonts?.mono}
      fontSize={props.fontSize ?? 13}
      lineHeight={props.lineHeight ?? 20}
      accessibilityLabel={props.accessibilityLabel}
      classes={props.classes}
      styles={editorStyles()}
      style={{
        flex: 1,
        width: "100%",
        height: "100%",
        minWidth: 0,
        minHeight: 0,
        color: look().palette.text,
        ...(props.style ?? {}),
      }}
      onChange={props.onChange}
      onSelectionChange={props.onSelectionChange}
      onFocus={props.onFocus}
      onBlur={props.onBlur}
      onContextMenu={props.onContextMenu}
    />
  );
}

/** 一等 NSH 内联 SVG 矢量图形（用于分支图、小图表等，不经 GPUI Canvas）。 */
export function SmeltVector(props: {
  markup: string;
  width?: number;
  height?: number;
  class?: string;
  style?: any;
}) {
  return (
    <NativeSvgVector
      markup={props.markup}
      class={props.class}
      style={{
        ...(props.width != null ? { width: props.width } : {}),
        ...(props.height != null ? { height: props.height } : {}),
        flexShrink: 0,
        pointerEvents: "none",
        ...(props.style ?? {}),
      }}
    />
  );
}

/** 一等 Rust `nsh/base <DiffView>` 的 Smelt 主题薄封装。 */
export function SmeltDiffView(props: {
  diffRef?: (api: any) => void;
  patch?: string;
  /** `blob:` 地址的补丁（`callWithBlobs` 拿到的 Blob），组件自己读原生字节。 */
  patchUrl?: string;
  oldValue?: string;
  newValue?: string;
  language?: string;
  layout?: "split" | "unified";
  inline?: "wordAlt" | "word" | "char" | "none";
  indicators?: "bars" | "classic" | "none";
  lineNumbers?: boolean;
  background?: boolean;
  softWrap?: boolean;
  fontSize?: number;
  lineHeight?: number;
  annotations?: Array<{ id: string; side: "old" | "new"; line: number }>;
  renderAnnotation?: (annotation: any) => any;
  renderAddAnnotation?: (range: any) => any;
  renderFileHeader?: (file: any) => any;
  renderHunkActions?: (hunk: any) => any;
  renderSeparator?: (sep: any) => any;
  style?: any;
  styles?: Record<string, any>;
  classes?: Record<string, string>;
  onLineSelectionChange?: (event: { range: { side: "old" | "new"; start: number; end: number } | null }) => void;
  onFileToggle?: (event: { file: any }) => void;
  onStatsChange?: (event: { stats: any }) => void;
}) {
  const look = currentTheme;
  const diffStyles = computed(() => {
    const p = look().palette;
    const base: Record<string, any> = {
      gutter: { backgroundColor: "transparent" },
      lineNumber: { color: p.faint },
      separator: { backgroundColor: p.surface, color: p.muted },
      fileHeader: {
        backgroundColor: p.surface,
        color: p.text,
        borderBottomColor: p.hair,
      },
      lineSelected: { backgroundColor: p.row_selected },
    };
    if (props.styles) {
      for (const [k, v] of Object.entries(props.styles)) {
        base[k] = { ...(base[k] ?? {}), ...(v ?? {}) };
      }
    }
    return base;
  });
  return (
    <BaseDiffView
      ref={props.diffRef}
      patch={props.patch}
      patchUrl={props.patchUrl}
      oldValue={props.oldValue}
      newValue={props.newValue}
      language={props.language}
      layout={props.layout ?? "unified"}
      inline={props.inline ?? "wordAlt"}
      indicators={props.indicators ?? "bars"}
      lineNumbers={props.lineNumbers ?? true}
      background={props.background ?? true}
      softWrap={props.softWrap ?? false}
      fontFamily={look().fonts?.mono}
      fontSize={props.fontSize ?? 13}
      lineHeight={props.lineHeight ?? 20}
      annotations={props.annotations}
      renderAnnotation={props.renderAnnotation}
      renderAddAnnotation={props.renderAddAnnotation}
      renderFileHeader={props.renderFileHeader}
      renderHunkActions={props.renderHunkActions}
      renderSeparator={props.renderSeparator}
      classes={props.classes}
      styles={diffStyles()}
      style={{
        flex: 1,
        width: "100%",
        height: "100%",
        minWidth: 0,
        minHeight: 0,
        color: look().palette.text,
        ...(props.style ?? {}),
      }}
      onLineSelectionChange={props.onLineSelectionChange}
      onFileToggle={props.onFileToggle}
      onStatsChange={props.onStatsChange}
    />
  );
}
