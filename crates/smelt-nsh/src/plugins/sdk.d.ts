import type { Signal } from "../nsh/index";
import type { AccessibilityRole, Bounds, InputRef, JSX, StyleProp, TextStyle, ViewStyle } from "../nsh/types";
import type { Collapsible, ContextMenu, Dialog, Menu, Popover, Select, Tooltip } from "../nsh/base";

export type LocalizedText = string | Record<string, string>;

export type HostPathStyle = "posix" | "windows";

export interface ViewSettingsEntry {
  group?: "general" | "agents" | "system" | "plugins";
  keywords?: string[];
}

export interface ViewSpec {
  name: string;
  export?: string;
  title: LocalizedText;
  icon?: SmeltIconPath | (string & {});
  draws_top_bar?: boolean;
  instances?: boolean;
  settings?: ViewSettingsEntry;
}

export interface CommandSpec {
  name: string;
  title: LocalizedText;
  key?: string | null;
  context?: string;
  requires_args?: boolean;
  args?: string[] | null;
  cli?: boolean;
  when_view_shown?: string | null;
  when_active_session?: boolean;
}

export interface LayoutMaterials {
  left?: "sidebar" | "window" | null;
  right?: "sidebar" | "window" | null;
  bottom?: "sidebar" | "window" | null;
  center?: "sidebar" | "window" | null;
}

export interface LayoutSpec {
  page: string;
  name: string;
  title: LocalizedText;
  state: string | Record<string, unknown>;
  materials?: LayoutMaterials;
}

export interface PageSpec {
  name: string;
  title: LocalizedText;
  home?: boolean;
  index?: string | null;
}

export interface MenuItemSpec {
  slot: "app" | "help";
  title?: LocalizedText;
  command: string;
}

export interface ContextMenuItemSpec {
  label: LocalizedText;
  icon?: SmeltIconPath | (string & {}) | null;
  image?: string | null;
  machine?: string | null;
  command?: string;
  args?: unknown;
  submenu?: ContextMenuItemSpec[];
}

export interface StatusMenuIcon {
  png: string | number[];
  color: string;
}

export type StatusMenuItemSpec =
  | {
      type: "action";
      label: string;
      icon?: StatusMenuIcon | null;
      command: string;
      args?: unknown;
    }
  | { type: "separator" };

export interface StatusMenuSpec {
  icon?: string | number[] | null;
  title?: string;
  tooltip?: string;
  items?: StatusMenuItemSpec[];
}

export type HostEvent =
  | { type: "runtime_topic"; machine: string | null; topic: string; message: unknown }
  | { type: "plugin"; name: string; payload: unknown }
  | { type: "view_opened"; view: string; args: unknown }
  | { type: "window_activated" }
  | { type: "context_menu_items_changed" }
  | { type: "reconnected"; machine: string | null }
  | { type: "open_paths"; machine: string | null; paths: string[] }
  | { type: "project_changed"; machine: string | null; root: string }
  | { type: "sessions_changed" }
  | {
      type: "attention_alert";
      machine: string | null;
      session: SessionAttention;
      alert: AttentionItem;
      title: string;
      message: string;
    }
  | { type: "machines_changed" }
  | { type: "active_context_changed" }
  | { type: "views_changed" }
  | { type: "view_badges_changed" }
  | { type: "shown_views_changed" }
  | { type: "laid_out_views_changed" }
  | { type: "pages_changed" }
  | { type: "layouts_changed" }
  | { type: "commands_changed" }
  | { type: "locale_changed" }
  | { type: "safe_area_changed" }
  | { type: "themes_changed" }
  | { type: "overlay_changed" }
  | { type: "notification_permission_changed" };

/** Register an async startup task; `smelt.plugin.ready` waits for all tasks (up to 3s). */
export declare function bootTask(work: (() => Promise<unknown>) | Promise<unknown>): Promise<unknown>;

export declare const SmeltNativeView: (props: {
  spec: unknown;
  ends?: unknown;
  safeArea?: unknown;
  focusSeq?: number;
  style?: StyleProp<ViewStyle>;
  onFocusIn?: (event: unknown) => void;
}) => JSX.Element;

export declare const SmeltNativeWindowBar: (props: {
  safeArea?: unknown;
  titleBar?: unknown;
  style?: StyleProp<ViewStyle>;
}) => JSX.Element;

export declare const SmeltNativeWindowDrag: (props: {
  style?: StyleProp<ViewStyle>;
}) => JSX.Element;

export declare const SmeltBackdropRegion: (props: {
  placement?: unknown;
  style?: StyleProp<ViewStyle>;
}) => JSX.Element;

export declare const SmeltNativeDesktop: (props: {
  machine?: string | null;
  active?: boolean;
  control?: boolean;
  reconnectSeq?: number;
  style?: StyleProp<ViewStyle>;
  onConnected?: (event: unknown) => void;
  onDisconnected?: (event: unknown) => void;
  onClipboard?: (event: unknown) => void;
  onText?: (event: unknown) => void;
  onError?: (event: unknown) => void;
}) => JSX.Element;

export declare const SmeltNativeTerminal: (props: {
  spec: unknown;
  focusSeq?: number;
  retrySeq?: number;
  sendText?: unknown;
  searchQuery?: { open: boolean; query: string } | null;
  searchStep?: { seq: number; query: string; backward: boolean } | null;
  style?: StyleProp<ViewStyle>;
  onStateChange?: (event: unknown) => void;
  onReconnecting?: (event: unknown) => void;
  onWriteError?: (event: unknown) => void;
  onFocusIn?: (event: unknown) => void;
  onSearchOpen?: (event: unknown) => void;
  onSearchStatus?: (event: { current?: number; total?: number }) => void;
  onSelectionChange?: (event: { text?: string | null }) => void;
}) => JSX.Element;

export declare const nativeApi: typeof import("./host").Smelt;

export declare function newSessionId(): string;
export declare function localOs(): string;
export declare function smeltVersion(): string;
export declare function qrMatrix(text: string): { size: number; bits: string } | null;

export declare function pathClean(style: string, path: string): string;
export declare function pathJoin(style: string, base: string, child: string): string;
export declare function pathFileName(style: string, path: string): string | null;
export declare function pathParent(style: string, path: string): string | null;
export declare function pathIsAbsolute(style: string, path: string): boolean;
export declare function pathStartsWith(style: string, path: string, base: string): boolean;
export declare function pathStripPrefix(style: string, path: string, base: string): string | null;
export declare function pathExtension(style: string, path: string): string | null;
export declare function projectLabels(style: string, roots: string[]): string[];
export declare function projectRootOf(style: string, roots: string[], path: string): string | null;

export declare function newSessionIcon(action: unknown): string;
export declare function conversationAcp(
  settings: unknown,
  optionId: string,
  sid: string,
  cwd: string,
): unknown;
export declare function quotaTarget(id: string): {
  id: string;
  service: string;
  name: string;
  icon: string;
} | null;
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
export declare function mergeEntryWindow(current: EntryWindow, next: EntryWindow): EntryWindowMerge;
export declare function quotaFacts(data: unknown): any;
export declare function parseFileLink(
  style: string,
  target: string,
  cwd?: string | null,
): { path: string; line: number | null; fragment: string } | null;
export declare function isInterruptMarker(text: string): boolean;
/** 一处改动的增删行数，和 `<SmeltDiffView>` 同一份按行比对；新建文件 `oldText` 传 `null`。 */
export declare function diffLineStats(
  oldText: string | null,
  newText: string,
): { added: number; removed: number };
/** 拖进窗口的 `file://` 按本机规则解成路径；不是文件 URL 的原样返回。 */
export declare function fileUriToPath(uri: string): string;
/** 工作目录是不是运行时按 id 给这条自动化分配的托管目录；`style` 是自动化所在机器的路径风格。 */
export declare function isManagedAutomationWorkspace(
  style: string,
  path: string,
  automationId: string,
): boolean;
/** 自动化配置哪里不对：运行时 `AutomationProblem` 的线格式，`kind` 是种类，其余是带的参数。 */
export type AutomationProblem = { kind: string; [param: string]: unknown };
/** 一条日程哪里不对（和运行时保存时同一条规则），没问题是 null。文案由插件按 `kind` 写。 */
export declare function automationScheduleProblem(schedule: unknown): AutomationProblem | null;
/** 一组触发条件（日程、外部触发、事件）哪里不对，没问题是 null。 */
export declare function automationTriggerProblem(trigger: unknown): AutomationProblem | null;
/** 智能体定义能不能用来开对话（引擎认得出且已开放）：新建菜单、手机、自动化都以这一条为准。 */
export declare function agentConversationReady(agent: unknown): boolean;
export type ProviderInputProblem = "id_required" | "id_invalid" | "base_url_invalid";
/** 自定义 Provider ID 哪里不对，对的给 `null`。和运行时保存时拦的是同一份规则。 */
export declare function customProviderIdProblem(id: string): ProviderInputProblem | null;
/** Provider 的 API 地址（完整 HTTP(S) URL）哪里不对，对的给 `null`。同上。 */
export declare function apiBaseUrlProblem(baseUrl: string): ProviderInputProblem | null;
export interface PiMcpPreset {
  id: string;
  name: string;
  summary: string;
  needs_sign_in: boolean;
  extension_url: string | null;
}
/** 内置 MCP 一键添加预设，按这台机器装没装远程桌面挑那一套。 */
export declare function piMcpPresets(remoteDesktop: boolean): PiMcpPreset[];
/** 某个 agent 记住的最近选择（去重后）。 */
export declare function rememberedAcpConfig(settings: unknown, agentId: string): Array<[string, string]>;
/** 记一次用户显式选择，返回改后的设置；不该记或没变化时为 `null`。 */
export declare function rememberAcpConfigValue(
  settings: unknown,
  agentId: string,
  configId: string,
  valueId: string,
): unknown | null;

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

export declare function agentCatalog(): AgentCatalog;
export declare function agentForSpace(settings: unknown, root: string): string | null;
export declare function launchableProfiles(settings: unknown): Array<{
  id: string;
  label: string;
  kind: string | null;
  launchable: boolean;
  in_history_sources: boolean;
}>;

export type ResolveResult<T> =
  | ({ ok: true } & T)
  | { ok: false; code: string; params: Record<string, unknown> };

export declare function resolveStartAcp(
  settings: unknown,
  withSpec: unknown,
  cwd: string,
  sid: string,
): ResolveResult<{ acp: any }>;
export declare function resolveResumeAcp(
  settings: unknown,
  args: unknown,
  sid: string,
): ResolveResult<{ acp: any }>;
export declare function resolveHandoffAcp(
  settings: unknown,
  args: unknown,
  sid: string,
): ResolveResult<{ acp: any }>;
export declare function resolveCliResume(
  style: string,
  entries: unknown[],
  settings: unknown | null,
  agentId: string,
  profileId: string | null,
  resumeId: string,
): ResolveResult<{ label: string; command: string }>;

export declare function daemonStateStatus(stateObj: unknown): "needs_you" | "running" | "idle" | null;
export declare function agentIdFromLaunch(launch: string): string | null;

/** Plugin id from `smelt-plugin.json`. */
export declare function pluginId(): string;
/** Persistent writable data directory (`~/.smelt/plugin-data/<id>/`). */
export declare function dataDir(): string;

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

export type EventFilter = HostEvent["type"] | `plugin:${string}`;

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

export declare function initHost(initialSlices?: readonly StateSlice[]): Promise<SmeltState>;
export declare function onSlices(
  slices: readonly StateSlice[],
  listener: (changed: readonly StateSlice[]) => void,
): () => void;
export declare function onEvent(
  listener: (event: HostEvent) => void,
  types: readonly EventFilter[],
): () => void;
/**
 * 对话流事件 `{ sub_id, kind }`：`snapshot`（带 `snapshot`）、`initial_prompt_unsupported`、
 * `closed`（连接已断，这个 `sub_id` 不会再有事件；要不要重新订阅由调用方定）。
 */
export declare function onAcpEvent(listener: (event: any) => void): () => void;

export declare function acpSubscribe(params: {
  subId: string;
  machine: string | null;
  reuseKey?: string;
  launch: Record<string, unknown>;
}): Promise<void>;
export declare function acpUnsubscribe(subId: string, reuseKey?: string): Promise<void>;
export declare function acpAction(subId: string, action: unknown, imageRefs?: string[]): Promise<void>;
export declare function acpHistory(
  machine: string | null,
  sessionId: string,
  before: number,
  limit?: number,
): Promise<any>;
export declare function acpRestart(machine: string | null, sessionId: string): Promise<void>;
export declare function acpPasteImage(params: {
  dataBase64?: string;
  mime?: string;
  path?: string;
}): Promise<any>;
export declare function acpClipboardImages(): Promise<{ images: any[]; paths: string[] }>;
export declare function acpImageUri(refId: string): Promise<string | null>;

export declare function renderMermaidSvg(source: string): Promise<
  | { ok: true; data_uri: string; width: number; height: number }
  | { ok: false; error: string }
>;

export declare function onCommand(handler: (call: { name: string; args: any }) => any): void;

export declare function locale(): string;
export declare function t(key: string, params?: Record<string, unknown>): string;
/** Build an i18n map from `locales/*.yml` for `registerViews` / `registerCommands`. */
export declare function text(key: string): Record<string, string>;

export declare function hostReady(): boolean;
export declare function texts(): Record<string, Record<string, string>>;
export declare function theme(): any;
export declare const currentTheme: typeof theme;
export declare function jsViews(): Record<string, { plugin: string; view: string }>;
export declare function shellState(): any;
export declare function machineMeta(): any[];
export declare function machineMetaEntry(machine: string | null | undefined): any;
export declare function machines(): any[];
export declare function machineEntry(machine: string | null | undefined): any;
export declare function pathStyle(machine: string | null | undefined): string;
export declare function workspaceDirectory(machine: string | null | undefined): any;
export declare function sessionStates(machine: string | null | undefined): any[];
export declare function conversationFacts(machine: string | null | undefined): any[];
export interface AttentionItem {
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
export declare function sessionAttention(
  machine: string | null | undefined,
): SessionAttention[] | null;
export declare function connections(): any;
export declare function activeContext(): { project: any; session: any };
export declare function views(): any[];
export declare function pages(): any[];
export declare function commands(): any[];
export declare function command(id: string): any;
export declare function viewBadge(id: string): string | null;
export declare function viewLaidOut(id: string): boolean;
export declare function viewShown(id: string): boolean;
export declare function safeArea(viewId: string): {
  title_bar: boolean;
  top_left: number;
  top_right: number;
  bottom_left: number;
  bottom_right: number;
  bar_insets: { left: number; right: number };
};

export declare function run(command: string, args?: unknown): Promise<any>;
export declare function call<T = any>(
  machine: string | null,
  method: ControlMethodName | (string & {}),
  params?: unknown,
  timeoutMs?: number,
): Promise<T>;

/**
 * 同 `call`，回复里 `blobs` 这些位置（JSON 指针，如 `/patch`、`/diff/text`）上的正文以
 * `Blob` 交来，不进插件的 JS 堆：用 `URL.createObjectURL` 交给原生组件（`SmeltDiffView` 的
 * `patchUrl`、`SmeltEditor` 的 `valueUrl`、`<Image>` 的 `src`），换内容时 `URL.revokeObjectURL`。
 * base64 编码的字节（图片）写成 `{ at, base64: true, mime_at }`，`mime_at` 指回复里类型所在的
 * 位置。那里没有正文时照原样。反方向不用这个：参数里放 `Blob`，宿主收到的就是它的文本。
 */
export declare function callWithBlobs<T = any>(
  machine: string | null,
  method: ControlMethodName | (string & {}),
  params: unknown,
  blobs: BlobAt[],
  timeoutMs?: number,
): Promise<T>;

/**
 * `callWithBlobs` 的一处正文：文本的 JSON 指针，或 `{ at, … }`。`base64: true` 是 base64 字节，
 * 类型在 `mime_at`；`each` 是 `at` 那个数组每一项里的位置（一页条目各自的正文），这时
 * `mime_at` 也相对那一项。
 */
export type BlobAt =
  | string
  | { at: string; each?: string; base64?: boolean; mime_at?: string };

/**
 * `blob()` 的 `blob:` 地址，交给原生组件读（编辑器、Markdown、图片）：换了 Blob 才换地址，旧地址
 * 和组件卸载时 revoke。在组件里调，随组件清理。
 */
export declare function blobUrl(blob: () => Blob | null | undefined): () => string | undefined;

export type CallDetailResult<T = any> =
  | { ok: true; value: T }
  | { ok: false; code: string | null; message: string };

/** 同 `call`，失败也作值交来（带运行时的错误码）；`blobs` 同 `callWithBlobs`，位置相对 `value`。 */
export declare function callDetail<T = any>(
  machine: string | null,
  method: ControlMethodName | (string & {}),
  params?: unknown,
  timeoutMs?: number,
  blobs?: BlobAt[],
): Promise<CallDetailResult<T>>;

export declare function request(
  machine: string | null,
  payload: Record<string, unknown>,
  timeoutMs?: number,
): Promise<any>;

export declare function windowActive(): boolean;
export declare function notificationPermission(): SmeltState["notification_permission"];
export declare function setStatusMenu(menu: StatusMenuSpec | null): Promise<void>;
export declare function setActiveContext(context: {
  project: { machine: string | null; root: string } | null;
  session: { machine: string | null; id: string; at_latest: boolean } | null;
}): Promise<void>;
export declare function registerFileOpener(name: string): Promise<void>;
export declare function openFile(location: {
  machine: string | null;
  path: string;
  line?: number | null;
}): Promise<void>;
export declare function openUrl(url: string): Promise<void>;
export declare function emitEvent(name: string, payload?: unknown): Promise<void>;
export declare function contextMenuItems(
  menu: string,
  machine: string | null,
  context: unknown,
): Promise<any[]>;
export declare function clipboardWrite(value: string): Promise<void>;
export interface ClipboardImageInput {
  ref_id?: string | null;
  data_b64?: string | null;
  mime?: string | null;
  data_uri?: string | null;
}
export declare function clipboardWriteImage(image: ClipboardImageInput): Promise<void>;
export declare function clipboardRead(): Promise<string | null>;
export declare function revealPath(path: string): Promise<void>;

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

export declare function processUsage(): Promise<ProcessUsageSample | null>;

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

export declare function discoverApps(queries: DiscoverAppQuery[]): Promise<DiscoveredApp[]>;
export declare function launchPath(opts: {
  kind: "mac_app" | "command";
  target: string;
  path: string;
  require_dir?: boolean;
}): Promise<void>;

export declare function locales(): Array<{ code: string; name: string }>;
export declare function systemLocale(): string;

export declare function registerViews(specs: ViewSpec[]): Promise<void>;
export declare function registerCommands(specs: CommandSpec[], groupTitle?: LocalizedText): Promise<void>;
export declare function registerTopics(topics: string[]): Promise<void>;
export declare function registerLayouts(specs: LayoutSpec[]): Promise<void>;
export declare function registerPages(specs: PageSpec[]): Promise<void>;
export declare function registerContextMenuItems(
  menu: string,
  items: ContextMenuItemSpec[],
): Promise<void>;
export declare function registerMenuItems(items: MenuItemSpec[]): Promise<void>;

/** Per-plugin SQLite store backed by `~/.smelt/plugin-data/<id>/store.sqlite3`. */
export declare const store: {
  query: (sql: string, params?: unknown[]) => Promise<any[]>;
  execute: (sql: string, params?: unknown[]) => Promise<number>;
  write: (statements: Array<{ sql: string; params?: unknown[] }>) => Promise<number[]>;
  migrate: (steps: string[]) => Promise<void>;
};

export declare function waitOnline(machine: string | null, failures?: number): Promise<boolean>;
export declare function stableMemo<T>(fn: () => T): () => T;

export declare function ViewInstance(props: {
  view: string;
  args?: unknown;
  seq?: number;
  reuseKey?: string;
  safeArea?: any;
  focusSeq?: number;
  onFocusIn?: () => void;
  class?: string;
  style?: StyleProp<ViewStyle>;
}): JSX.Element;

export declare function SmeltIcon(props: {
  path?: SmeltIconPath | (string & {});
  size?: number;
  color?: string;
  opacity?: number;
  rotate?: number;
  spin?: boolean;
  class?: string;
  style?: StyleProp<ViewStyle>;
  testID?: string;
}): JSX.Element;

/** 展开收起：`nsh/base` 的 `Collapsible` 带上 Smelt 的展开动效（`disclosure` 弹簧）。标题和内容的间距写成内容的上内边距。 */
export declare function SmeltDisclosure(
  props: Omit<Parameters<typeof Collapsible>[0], "reveal" | "revealSpring">,
): JSX.Element;
export declare function SmeltDisclosureChevron(props: {
  open: boolean;
  size?: number;
  color?: string;
  opacity?: number;
  testID?: string;
}): JSX.Element;

/** 开关：一项设置立刻生效的开或关。打开铺强调色 `accent`。 */
export declare function SmeltSwitch(props: {
  checked: boolean;
  disabled?: boolean;
  accessibilityLabel?: string;
  testID?: string;
  onChange: (checked: boolean) => void;
}): JSX.Element;
/** 复选框：勾上铺强调色、压白勾。`children` 是标签，点标签和点框一样。 */
export declare function SmeltCheckbox(props: {
  checked: boolean;
  disabled?: boolean;
  accessibilityLabel?: string;
  testID?: string;
  onChange: (checked: boolean) => void;
  class?: string;
  style?: StyleProp<ViewStyle>;
  children?: JSX.Element;
}): JSX.Element;
/** 单选：选中铺强调色、压白点。放在 `nsh/base` 的 `RadioGroup` 里；`children` 是标签。 */
export declare function SmeltRadio(props: {
  checked: boolean;
  disabled?: boolean;
  accessibilityLabel?: string;
  testID?: string;
  onChange: () => void;
  class?: string;
  style?: StyleProp<ViewStyle>;
  children?: JSX.Element;
}): JSX.Element;

/** 按钮的颜色角色。`secondary` 次要（默认），`quiet` 安静（无底，悬停才铺），`danger` 次要底红字，
 * `danger-quiet` 安静红字，`floating` 浮在内容上（`surface` 底加投影），`dismiss` 卡片角上的关闭。 */
export type SmeltButtonVariant =
  | "secondary"
  | "primary"
  | "quiet"
  | "danger"
  | "danger-quiet"
  | "floating"
  | "dismiss";
/** 控件尺寸：按钮（纯图标按钮见方）和输入框共用一张表，同一档并排一样高。图标跟同档的字配，有字没字一样大。
 * `xs` 20 高、small 字、图标 14：行里的就地操作。
 * `sm` 24 高、text_xsmall 字、图标 14：面板顶栏、工具条。
 * `md` 28 高、ui 字、图标 16：标题栏、表单、对话框。 */
export type SmeltControlSize = "xs" | "sm" | "md";
/** 外形只改圆角：`rounded` 走主题圆角，`pill` 两端全圆，`circle` 用在纯图标按钮上。 */
export type SmeltButtonShape = "rounded" | "pill" | "circle";

export interface SmeltButtonProps {
  label?: string;
  icon?: SmeltIconPath | (string & {});
  trailingIcon?: SmeltIconPath | (string & {});
  tooltip?: string;
  accessibilityLabel?: string;
  /** 读屏角色，默认 `button`；页签传 `tab`、列表项传 `option`，选中态随 `selected` 播报。 */
  role?: AccessibilityRole;
  /** 读屏补充：按下会做什么（「插入到输入框」）。 */
  accessibilityHint?: string;
  command?: string;
  /** 默认 `secondary`。 */
  variant?: SmeltButtonVariant;
  /** 默认 `md`。 */
  size?: SmeltControlSize;
  /** 默认 `rounded`。 */
  shape?: SmeltButtonShape;
  /** 撑满父级宽度（文字过长时截断）。 */
  fill?: boolean;
  disabled?: boolean;
  loading?: boolean;
  selected?: boolean;
  /** 只放布局：宽度、`flex`、外边距、对齐。颜色、圆角、内边距、字由 `variant` / `size` / `shape` 定，不接 `style`。 */
  class?: string;
  testID?: string;
  onPress?: (e?: any) => void;
  children?: JSX.Element;
}

/** 弹出卡片的样子（`SmeltPopover` 默认的 `popupClass`）。 */
export declare const POPUP_CLASS: string;
/** 菜单、下拉、选择框的样子（`SmeltMenu`、`SmeltContextMenu`、`SmeltSelect` 默认的 `popupClass`）。 */
export declare const MENU_CLASS: string;
export declare const TOOLTIP_CLASS: string;
/** Smelt 的浮层：`nsh/base` 的同名组件带上 Smelt 的样子（`popupClass` / `class` 换掉默认样子）和进出动效。 */
export declare function SmeltPopover(
  props: Omit<Parameters<typeof Popover>[0], "popupStartingClass" | "popupEndingClass">,
): JSX.Element;
export declare function SmeltMenu(
  props: Omit<Parameters<typeof Menu>[0], "popupStartingClass" | "popupEndingClass">,
): JSX.Element;
export declare function SmeltContextMenu(
  props: Omit<Parameters<typeof ContextMenu>[0], "popupStartingClass" | "popupEndingClass">,
): JSX.Element;
export declare function SmeltSelect(
  props: Omit<Parameters<typeof Select>[0], "popupStartingClass" | "popupEndingClass">,
): JSX.Element;
export declare function SmeltTooltip(
  props: Omit<Parameters<typeof Tooltip>[0], "startingClass" | "endingClass">,
): JSX.Element;
export declare const MENU_ITEM_CLASS: string;
export declare const INPUT_CLASS: string;
export declare const DIALOG_SURFACE_CLASS: string;
/** 对话框：`nsh/base` 的 `Dialog` 带上 Smelt 的进出（整层淡入淡出、卡片升 4px）和主题的遮罩色 `scrim`。 */
export declare function SmeltDialog(
  props: Omit<
    Parameters<typeof Dialog>[0],
    "animation" | "backdropClass" | "startingClass" | "startingStyle"
  >,
): JSX.Element;

export declare function SmeltButton(props: SmeltButtonProps): JSX.Element;

/** 列表行：悬停 `row_hover`、选中 `row_selected`（只靠底色，不画竖条），箭头光标，键盘可聚焦。
 * 行高、内边距、缩进、间距放 `class` / `style`；颜色和圆角归组件，`style` 里写了也会被盖掉。 */
export declare function SmeltRow(props: {
  selected?: boolean;
  disabled?: boolean;
  /** 默认圆角 `radius.row`；贴边铺满的行传 `false`。 */
  rounded?: boolean;
  accessibilityLabel?: string;
  /** 读屏角色，默认 `button`；页签传 `tab`、列表项传 `option`，选中态随 `selected` 播报。 */
  role?: AccessibilityRole;
  /** 读屏补充：按下会做什么（「插入到输入框」）。 */
  accessibilityHint?: string;
  tooltip?: JSX.Element;
  testID?: string;
  class?: string;
  style?: StyleProp<ViewStyle>;
  onPress?: (e?: any) => void;
  onPressDown?: () => void;
  onMouseEnter?: (e: any) => void;
  onMouseLeave?: (e: any) => void;
  ref?: any;
  children?: JSX.Element;
}): JSX.Element;

/** 文本框：`card` 底、`input` 色细边、主题圆角。单行 `md` 28 高 `ui` 字、`sm` 26 高 `small` 字；
 * 多行的最小高度放 `class`。 */
export declare function SmeltInput(props: {
  value?: string;
  defaultValue?: string;
  placeholder?: string;
  masked?: boolean;
  multiline?: boolean;
  disabled?: boolean;
  readonly?: boolean;
  autoFocus?: boolean;
  selectTextOnFocus?: boolean;
  maxLength?: number;
  /** 校验没过：边框换 `bad`。 */
  invalid?: boolean;
  /** 不画底和边：嵌在已有外框里（搜索条、菜单里的过滤框）。 */
  bare?: boolean;
  /** 高度和字档同按钮（见 `SmeltControlSize`），默认 `md`。行里就地改名用 `md`：字和行标题同为 ui，进出编辑不跳。 */
  size?: SmeltControlSize;
  /** 等宽字（命令、路径、代码）。 */
  mono?: boolean;
  accessibilityLabel?: string;
  testID?: string;
  ref?: InputRef | ((ref: InputRef) => void);
  /** 只放布局：宽度、`flex`、最小高度、外边距。 */
  class?: string;
  onChange?: (e: { value: string }) => void;
  onSubmit?: (e: { text: string }) => void;
  onKeyPress?: (e: { key: string }) => void;
  onFocus?: () => void;
  onBlur?: () => void;
  onLayout?: (e: { bounds: Bounds }) => void;
}): JSX.Element;

/** 分段控件：一组互斥的选项，`multiple` 时可多选。整组是 Tab 的一站，左右键移焦点，空格 / 回车按下。 */
export declare function SmeltSegmented<T extends string>(props: {
  /** `tooltip` 给这一段的悬停说明。 */
  options: ReadonlyArray<{ value: T; label: string; disabled?: boolean; tooltip?: string }>;
  /** 单选：当前值；`multiple` 时传选中的值数组。 */
  value: T | ReadonlyArray<T>;
  multiple?: boolean;
  /** 单选：按下那段的值；`multiple` 时是被切换的那段，增删由调用方定。 */
  onChange: (value: T) => void;
  disabled?: boolean;
  accessibilityLabel?: string;
  testID?: string;
  /** 只放布局。 */
  class?: string;
}): JSX.Element;

/** 菜单项：28 高，左边可选 14 的图标或图片，右边可选勾；`danger` 用 `bad`。放在 `SmeltMenu` / `SmeltContextMenu` 里。 */
export declare function SmeltMenuItem(props: {
  label: string;
  icon?: SmeltIconPath | (string & {}) | null;
  image?: string | null;
  iconColor?: string;
  /** 给了就留勾的位置（`false` 只留空位），对齐一组可勾选的项。 */
  checked?: boolean;
  danger?: boolean;
  disabled?: boolean;
  /** 默认 `true`。 */
  closeOnSelect?: boolean;
  testID?: string;
  onSelect: () => void;
}): JSX.Element;
/** 菜单分隔线。 */
export declare function SmeltMenuSeparator(): JSX.Element;
/** 菜单分组名：`small_medium`、`faint`，不可点。 */
export declare function SmeltMenuLabel(props: { label: string }): JSX.Element;

/** 确认框：标题、说明、可选补充（`children`，在说明下面），右下角取消 + 确认（`danger` 时是危险按钮）。
 * 只能用按钮或 Esc 关；`busy` 时确认转圈，取消和 Esc 不可用。开关归调用方：`onConfirm` 里自己把 `open` 置回。 */
export declare function SmeltConfirmDialog(props: {
  open: boolean;
  title: string;
  description?: string;
  confirmLabel: string;
  cancelLabel: string;
  danger?: boolean;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
  children?: JSX.Element;
}): JSX.Element;
export declare function SmeltWindowBar(props: { view: string }): JSX.Element;

export declare function SmeltMarkdown(props: {
  /** 二选一：正文，或 blob 里正文的 `blob:` 地址（大文件不进插件的 JS 堆）。 */
  markdown?: string;
  markdownUrl?: string;
  cwd?: string | null;
  machine?: string | null;
  selectable?: boolean;
  streaming?: boolean;
  style?: StyleProp<ViewStyle>;
  styles?: Record<string, any>;
  onLinkError?: (error: unknown) => void;
}): JSX.Element;

export declare function SmeltEditor(props: {
  editorRef?: (api: any) => void;
  value?: string;
  defaultValue?: string;
  /** blob 里正文的 `blob:` 地址，代替 `value`：大文件不进插件的 JS 堆，改后的正文从 `onBlobChange` 拿。 */
  valueUrl?: string;
  /** 已保存正文的 `blob:` 地址：`onModifiedChange` 报告当前正文和它是否不同。 */
  savedValueUrl?: string;
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
  style?: StyleProp<TextStyle>;
  styles?: Record<string, any>;
  classes?: Record<string, string>;
  onChange?: (event: { value: string }) => void;
  /** 每次改动后的正文，以 Blob 交来（代替 `onChange` 的字符串，不进 JS 堆）。 */
  onBlobChange?: (event: { blob: Blob }) => void;
  onModifiedChange?: (event: { modified: boolean }) => void;
  onSelectionChange?: (event: any) => void;
  onFocus?: () => void;
  onBlur?: () => void;
  onContextMenu?: (event: {
    position: { line: number; character: number };
    x: number;
    y: number;
  }) => void;
}): JSX.Element;

export declare function SmeltVector(props: {
  markup: string;
  width?: number;
  height?: number;
  class?: string;
  style?: StyleProp<ViewStyle>;
}): JSX.Element;

export declare function SmeltDiffView(props: {
  diffRef?: (api: any) => void;
  patch?: string;
  /**
   * `blob:` 地址的补丁（`callWithBlobs` 拿到的 Blob），组件自己读原生字节，正文不进插件的堆。
   * ref 的 `hunkPatch(gap)` 给出一块单独的补丁（可直接 `git apply`），`fileLine(side, line)`
   * 给出一行所在的文件和行号。
   */
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
  style?: StyleProp<ViewStyle>;
  styles?: Record<string, any>;
  classes?: Record<string, string>;
  onLineSelectionChange?: (event: {
    range: { side: "old" | "new"; start: number; end: number } | null;
  }) => void;
  onFileToggle?: (event: { file: any }) => void;
  onStatsChange?: (event: { stats: any }) => void;
}): JSX.Element;
