// Smelt NSH Guest SDK（通过 `import { ... } from "smelt/sdk"` 导入）。
// 封装通用的宿主状态、离散事件、命令双向派发、SQLite 存储、纯同步模型函数，以及原子原生能力组件。
// 不含任何具体插件逻辑，也不提供插件嵌插件的跨 VM 插槽。
import {
  cleanup,
  computed,
  effect,
  host,
  Image,
  nativeComponent,
  nativeModule,
  signal,
  Text,
  View,
} from "nsh";
import {
  Button as BaseButton,
  Dialog,
  DiffView as BaseDiffView,
  Editor as BaseEditor,
  TextView,
  Tooltip,
} from "nsh/base";
import { Icon as NativeSvgIcon, Vector as NativeSvgVector } from "smelt/host";
export { bootTask } from "./provider.tsx";

// ---- 原生组件封装（仅保留 NSH 拼不出的 GPUI 能力件：重视图、小人、标题带、毛玻璃坐标） ----

export const SmeltNativeView = nativeComponent("smelt.view", {
  props: ["spec", "ends", "safeArea", "focusSeq"],
  events: ["focusIn"],
}) as any;

export const SmeltNativeBuddy = nativeComponent("smelt.buddy", {
  props: ["seed", "act", "since", "opacity"],
  events: [],
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
  props: ["spec", "contextMenu", "labels", "focusSeq", "retrySeq", "sendText"],
  events: ["stateChange", "reconnecting", "writeError", "focusIn"],
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

export function terminalThemeSnapshot(): any {
  return JSON.parse(Smelt.terminalThemeSnapshot());
}

export function setTerminalFontPx(px: number): Promise<void> {
  return host.call("set_terminal_font_px", { px: Math.round(px) });
}

export function attentionTexts(item: unknown): { title: string; message: string } {
  return JSON.parse(Smelt.attentionTexts(JSON.stringify(item)));
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
  machines: any[];
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
  machines: [],
  connections: { remotes: [], load_error: null },
  active_context: { project: null, session: null },
  pages: [],
  views: [],
  commands: [],
  js_views: {},
  safe_areas: {},
  bar_insets: {},
  theme: {
    rem: 16,
    palette: {
      bg: "#161616",
      side: "#121212",
      surface: "#191919",
      soft: "#242424",
      soft_hover: "#2e2e2e",
      text: "#fafafa",
      muted: "#aeaeae",
      muted_selected: "#d4d4d8",
      faint: "#808080",
      faint_selected: "#a1a1aa",
      ink: "#5c5c5c",
      on_ink: "#ffffff",
      link: "#a2bcff",
      run: "#7698fd",
      need: "#ffca76",
      ok: "#6bd586",
      bad: "#f17471",
      on_bad: "#ffffff",
      hair: "rgba(255, 255, 255, 0.102)",
      hair_strong: "rgba(255, 255, 255, 0.200)",
      row_hover: "rgba(255, 255, 255, 0.059)",
      row_selected: "rgba(255, 255, 255, 0.102)",
      focus: "rgba(118, 152, 253, 1.000)",
      button_primary: ["rgba(92, 92, 92, 1)", "rgba(74, 74, 74, 1)", "rgba(55, 55, 55, 1)"],
      button_primary_fg: "#ffffff",
      button_secondary: ["rgba(255, 255, 255, 0.06)", "rgba(255, 255, 255, 0.11)", "rgba(255, 255, 255, 0.16)"],
      button_secondary_fg: "#fafafa",
      button_submit: ["rgba(219, 219, 219, 1)", "rgba(201, 201, 201, 1)", "rgba(176, 176, 176, 1)"],
      button_submit_fg: "#161616",
    },
    fonts: { mono: "Maple Mono NF" },
    type: {
      ui: { size: 13, line_height: 20, weight: 440 },
      ui_medium: { size: 13, line_height: 20, weight: 530 },
      small: { size: 11, line_height: 12, weight: 440 },
      small_medium: { size: 11, line_height: 12, weight: 530 },
      text_xsmall: { size: 12, line_height: 18, weight: 400 },
      text_xsmall_medium: { size: 12, line_height: 18, weight: 500 },
    },
    radius: { sm: 4, md: 6, lg: 8, row: 6 },
  },
  texts: {},
};

export const state = signal<SmeltState>(DEFAULT_STATE);

// 按顶层切片拆分的细粒度信号：只在对应切片真正变化时触发订阅方，
// 避免窗口失焦、机器心跳或切换会话把所有插件里的 `t()` / `theme()` / `commands()` 连带重算一遍。
const localeSlice = signal<string>(DEFAULT_STATE.locale);
const systemLocaleSlice = signal<string>(DEFAULT_STATE.system_locale);
const localesSlice = signal<Array<{ code: string; name: string }>>(DEFAULT_STATE.locales);
const windowActiveSlice = signal<boolean>(DEFAULT_STATE.window_active);
const notificationPermissionSlice = signal<SmeltState["notification_permission"]>(
  DEFAULT_STATE.notification_permission,
);
const machinesSlice = signal<any[]>(DEFAULT_STATE.machines);
const connectionsSlice = signal<any>(DEFAULT_STATE.connections);
const activeContextSlice = signal<{ project: any; session: any }>(DEFAULT_STATE.active_context);
const pagesSlice = signal<any[]>(DEFAULT_STATE.pages);
const viewsSlice = signal<any[]>(DEFAULT_STATE.views);
const commandsSlice = signal<any[]>(DEFAULT_STATE.commands);
const jsViewsSlice = signal<Record<string, { plugin: string; view: string }>>(DEFAULT_STATE.js_views);
const safeAreasSlice = signal<Record<string, any>>(DEFAULT_STATE.safe_areas);
const themeSlice = signal<any>(DEFAULT_STATE.theme);
const textsSlice = signal<Record<string, Record<string, string>>>({});
const shellSlice = signal<any>(undefined);

const sliceJsonCache: Record<string, string> = {};

function syncObjectSlice<T>(key: string, sig: { set: (v: T) => void }, next: T) {
  const encoded = JSON.stringify(next);
  if (sliceJsonCache[key] === encoded) return;
  sliceJsonCache[key] = encoded;
  sig.set(next);
}

let cachedTexts: Record<string, Record<string, string>> = {};
let initialized = false;
const stateListeners = new Set<(next: SmeltState) => void>();
const eventListeners = new Set<(event: any) => void>();
const acpEventListeners = new Set<(event: any) => void>();
let commandHandler: ((call: { name: string; args: any }) => any) | null = null;
const pendingCommands: Array<{ id: number; name: string; args: any }> = [];

function notifyStateListeners(next: SmeltState) {
  for (const listener of [...stateListeners]) {
    try {
      listener(next);
    } catch (error) {
      console.error("[smelt/sdk] state listener error:", error);
    }
  }
}

function applyState(next: any) {
  if (!next || typeof next !== "object") return;
  const hasTexts = Boolean(next.texts && typeof next.texts === "object");
  if (hasTexts) {
    cachedTexts = next.texts;
  }
  const current = state();
  const rev = typeof next.revision === "number" ? next.revision : 0;
  if (rev > 0 && rev < current.revision) {
    if (hasTexts && current.texts !== next.texts) {
      syncObjectSlice("texts", textsSlice, next.texts);
      const patched: SmeltState = { ...current, texts: next.texts };
      state.set(patched);
      notifyStateListeners(patched);
    }
    return;
  }
  const merged: SmeltState = {
    ...DEFAULT_STATE,
    ...next,
    texts: hasTexts ? next.texts : cachedTexts,
  };
  if (localeSlice() !== merged.locale) localeSlice.set(merged.locale);
  if (systemLocaleSlice() !== merged.system_locale) systemLocaleSlice.set(merged.system_locale);
  if (windowActiveSlice() !== (merged.window_active !== false)) {
    windowActiveSlice.set(merged.window_active !== false);
  }
  syncObjectSlice("locales", localesSlice, merged.locales ?? []);
  syncObjectSlice(
    "notification_permission",
    notificationPermissionSlice,
    merged.notification_permission ?? { kind: "unsupported" },
  );
  syncObjectSlice("machines", machinesSlice, merged.machines ?? []);
  syncObjectSlice(
    "connections",
    connectionsSlice,
    merged.connections ?? { remotes: [], load_error: null },
  );
  syncObjectSlice(
    "active_context",
    activeContextSlice,
    merged.active_context ?? { project: null, session: null },
  );
  syncObjectSlice("pages", pagesSlice, merged.pages ?? []);
  syncObjectSlice("views", viewsSlice, merged.views ?? []);
  syncObjectSlice("commands", commandsSlice, merged.commands ?? []);
  syncObjectSlice("js_views", jsViewsSlice, merged.js_views ?? {});
  syncObjectSlice("safe_areas", safeAreasSlice, merged.safe_areas ?? {});
  syncObjectSlice("theme", themeSlice, merged.theme ?? DEFAULT_STATE.theme);
  if (hasTexts) {
    syncObjectSlice("texts", textsSlice, next.texts);
  }
  if (merged.shell !== undefined) {
    syncObjectSlice("shell", shellSlice, merged.shell);
  }
  state.set(merged);
  notifyStateListeners(merged);
}

export function initHost(): Promise<SmeltState> {
  if (!initialized) {
    initialized = true;
    host.on("smelt.state", (next: any) => applyState(next));
    host.on("smelt.event", (event: any) => {
      for (const listener of [...eventListeners]) {
        try {
          listener(event);
        } catch (error) {
          console.error("[smelt/sdk] event listener error:", error);
        }
      }
    });
    host.on("smelt.acp.event", (event: any) => {
      for (const listener of [...acpEventListeners]) {
        try {
          listener(event);
        } catch (error) {
          console.error("[smelt/sdk] acp event listener error:", error);
        }
      }
    });
    host.on("smelt.command", (call: any) => {
      if (!call || typeof call.id !== "number" || typeof call.name !== "string") return;
      if (!commandHandler) {
        pendingCommands.push(call);
        return;
      }
      dispatchCommand(call);
    });
  }
  return host
    .call("state")
    .then((initial: any) => {
      applyState(initial);
      return state();
    })
    .catch((error: unknown) => {
      console.error("[smelt/sdk] state() failed:", error);
      return state();
    });
}

function dispatchCommand(call: { id: number; name: string; args: any }) {
  const handler = commandHandler;
  if (!handler) return;
  Promise.resolve()
    .then(() => handler({ name: call.name, args: call.args }))
    .then(
      (value) => host.call("command.reply", { id: call.id, ok: true, value: value ?? null }),
      (error: any) =>
        host.call("command.reply", {
          id: call.id,
          ok: false,
          error: error?.message ?? String(error),
        }),
    )
    .catch(() => {});
}

// 立即挂载监听并拉首份状态。
void initHost();

export function onState(listener: (next: SmeltState) => void): () => void {
  stateListeners.add(listener);
  return () => stateListeners.delete(listener);
}

export function onEvent(listener: (event: any) => void): () => void {
  eventListeners.add(listener);
  return () => eventListeners.delete(listener);
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
  commandHandler = handler;
  while (pendingCommands.length > 0) {
    const next = pendingCommands.shift();
    if (next) dispatchCommand(next);
  }
}

// ---- 国际化 ----

export function locale(): string {
  return localeSlice() || "en";
}

export function t(key: string, params?: Record<string, unknown>): string {
  const curLocale = localeSlice() || "en";
  const tables = textsSlice();
  const dict = Object.keys(tables).length > 0 ? tables : cachedTexts;
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

/** 构造给 registerViews / registerCommands 用的多语言表（宿主按当前语言动态取）。 */
export function text(key: string): Record<string, string> {
  const tables = textsSlice();
  const dict = Object.keys(tables).length > 0 ? tables : cachedTexts;
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

// ---- 宿主读取与动作（均绑定细粒度切片信号） ----

/** 细粒度主题快照：只在主题或窗口 `rem` 真正变化时触发订阅方。 */
export function theme(): any {
  return themeSlice();
}

export const currentTheme = theme;

export function jsViews(): Record<string, { plugin: string; view: string }> {
  return jsViewsSlice();
}

export function shellState(): any {
  return shellSlice();
}

export function machines(): any[] {
  return machinesSlice();
}

export function machineEntry(machine: string | null | undefined): any {
  const target = machine ?? null;
  return machines().find((m) => (m.machine ?? null) === target) ?? null;
}

export function pathStyle(machine: string | null | undefined): string {
  return machineEntry(machine)?.path_style ?? (localOs() === "win32" ? "windows" : "posix");
}

export function workspaceDirectory(machine: string | null | undefined): any {
  return machineEntry(machine)?.directory ?? null;
}

export function sessionStates(machine: string | null | undefined): any[] {
  return machineEntry(machine)?.states ?? [];
}

export function conversationFacts(machine: string | null | undefined): any[] {
  return machineEntry(machine)?.facts ?? [];
}

export function connections(): any {
  return connectionsSlice();
}

export function activeContext(): { project: any; session: any } {
  return activeContextSlice();
}

export function views(): any[] {
  return viewsSlice();
}

export function pages(): any[] {
  return pagesSlice();
}

export function commands(): any[] {
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
  return windowActiveSlice();
}

export function notificationPermission(): SmeltState["notification_permission"] {
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

export function pathKind(path: string): Promise<"dir" | "file" | null> {
  return host.call("fs.path_kind", { path });
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
  return localesSlice();
}

export function systemLocale(): string {
  return systemLocaleSlice() || "en";
}

export function setLocale(locale: string | null): Promise<void> {
  return host.call("set_locale", { locale });
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
    const entry = machineEntry(target);
    if (entry?.online === true) return true;
    if (target != null && entry == null && state().revision > 0) return false;
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
      const unsubEvent = onEvent((event) => {
        if (event?.type === "reconnected" && (event.machine ?? null) === target) {
          finish();
        }
      });
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
        unsubState();
        unsubEvent();
        resolve(next);
      }
    };
    const unsubState = onState(check);
    const unsubEvent = onEvent((event) => {
      if (event?.type === "machines_changed" || event?.type === "reconnected") {
        check();
      }
    });
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
    if (props.spin) parts.push("animate-spin");
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

export const POPUP_CLASS = "rounded-lg border border-border bg-popover p-1 shadow-lg";
export const TOOLTIP_CLASS = "rounded-(--radius) border border-border bg-popover px-2 py-1 shadow-md";
export const MENU_ITEM_CLASS =
  "flex-row items-center rounded-(--radius) px-2 hover:bg-accent focus:bg-accent";
export const INPUT_CLASS = "rounded-(--radius) border border-input bg-card px-2 text-foreground";
export const DIALOG_SURFACE_CLASS = "rounded-xl border border-border bg-popover shadow-xl";

/** 统一纯 JS/NSH 按钮（建在 `nsh/base <Button>` 上）：无 `BlockMouse` 悬停断层，完整遵循宿主主题色阶。 */
export function SmeltButton(props: SmeltButtonProps) {
  const look = theme;
  const iconOnly = () => !props.label && !props.children && Boolean(props.icon) && !props.trailingIcon;
  const interactive = () => !props.disabled && !props.loading;
  const hasTooltip = computed(() => Boolean(props.tooltip));
  const shortcut = () => (props.command ? formatShortcut(command(props.command)?.key) : null);

  const colors = computed(() => {
    const p = look().palette;
    const primary = iconOnly() ? (p.button_submit ?? p.button_primary) : p.button_primary;
    const primaryFg = iconOnly() ? (p.button_submit_fg ?? p.button_primary_fg) : p.button_primary_fg;
    const neutral = p.button_secondary ?? [p.soft, p.soft_hover ?? p.soft, p.row_selected];
    const neutralFg = p.button_secondary_fg ?? p.text;
    const softHover = p.soft_hover ?? p.row_selected;

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
        pressBg: p.soft_hover ?? p.row_selected,
        pressFg: p.text,
        surfaceClass: "border border-border shadow-sm",
        disabledOpacity: 0.45,
      };
    }
    if (props.primary) {
      return {
        bg: primary?.[0] ?? p.ink,
        fg: primaryFg ?? p.on_ink,
        hoverBg: primary?.[1] ?? p.ink,
        hoverFg: primaryFg ?? p.on_ink,
        pressBg: primary?.[2] ?? p.ink,
        pressFg: primaryFg ?? p.on_ink,
        surfaceClass: "shadow-xs",
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
        surfaceClass: "border border-border shadow-xs",
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
      bg: props.selected ? softHover : neutral[0],
      fg: neutralFg,
      hoverBg: neutral[1],
      hoverFg: neutralFg,
      pressBg: neutral[2],
      pressFg: neutralFg,
      surfaceClass: "border border-border shadow-xs",
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
          <Tooltip class={TOOLTIP_CLASS}>
            <View class="flex-row items-center gap-1.5">
              <Text class="text-xs text-foreground">{props.tooltip}</Text>
              <If when={shortcut() != null}>
                <Text class="text-xs text-muted-foreground">{shortcut() ?? ""}</Text>
              </If>
            </View>
          </Tooltip>
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
        backgroundColor: props.loading ? (look().palette.soft_hover ?? colors().bg) : colors().bg,
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
  return <SmeltNativeWindowBar safeArea={area()} style={{ width: "100%", flexShrink: 0 }} />;
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
    renderMermaidSvg(src)
      .then((res) => {
        if (mermaidClientCache.size >= 128) {
          const oldest = mermaidClientCache.keys().next().value;
          if (oldest !== undefined) mermaidClientCache.delete(oldest);
        }
        mermaidClientCache.set(cacheKey, res);
        if (!stale) result.set(res);
      })
      .catch((err: any) => {
        if (!stale) result.set({ ok: false, error: err?.message ?? String(err) });
      });
    cleanup(() => {
      stale = true;
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
          <Dialog
            open={previewOpen()}
            animation="fade"
            backdropClass="bg-black/65"
            onOpenChange={({ open }: any) => previewOpen.set(Boolean(open))}
            class="m-auto"
          >
            <If when={previewOpen() && Boolean(result()?.ok)}>
              <View
                class="rounded-xl border border-border bg-background p-4 shadow-xl items-center justify-center"
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
          </Dialog>
        </If>
      </If>
    </View>
  );
}

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
        backgroundColor: p.side ?? p.surface,
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
  return (
    <TextView
      markdown={props.markdown}
      options={props.streaming ? { streaming: true } : undefined}
      selectable={props.selectable ?? true}
      links="report"
      extensions={MERMAID_EXTENSIONS}
      renderCustom={(node: any) => {
        if (node?.name === "mermaid") {
          return <MermaidDiagramBlock source={node.text ?? ""} />;
        }
        return null;
      }}
      styles={tvStyles()}
      style={{ width: "100%", minWidth: 0, ...(props.style ?? {}) }}
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

/** 清理订阅的便捷 hook，绑定到当前组件生命周期。 */
export function useHostEvent(listener: (event: any) => void) {
  effect(() => {
    const unsub = onEvent(listener);
    cleanup(unsub);
  });
}

export {
  SmeltConversation,
  type SmeltConversationProps,
  type SmeltConversationSpec,
  type SmeltConversationStart,
} from "./conversation/index.tsx";
