import type { Signal } from "../nsh/index";
import type { JSX, StyleProp, TextStyle, ViewStyle } from "../nsh/types";

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
  palette?: boolean;
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

export declare const SmeltNativeBuddy: (props: {
  seed?: unknown;
  act?: unknown;
  since?: unknown;
  opacity?: number;
  style?: StyleProp<ViewStyle>;
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
  contextMenu?: unknown;
  labels?: unknown;
  focusSeq?: number;
  retrySeq?: number;
  sendText?: unknown;
  style?: StyleProp<ViewStyle>;
  onStateChange?: (event: unknown) => void;
  onReconnecting?: (event: unknown) => void;
  onWriteError?: (event: unknown) => void;
  onFocusIn?: (event: unknown) => void;
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
export declare function handoffPrompt(payload: {
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
}): string;

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

export type CallDetailResult<T = any> =
  | { ok: true; value: T }
  | { ok: false; code: string | null; message: string };

export declare function callDetail<T = any>(
  machine: string | null,
  method: ControlMethodName | (string & {}),
  params?: unknown,
  timeoutMs?: number,
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
export declare function pathKind(path: string): Promise<"dir" | "file" | null>;
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

export interface SmeltButtonProps {
  label?: string;
  icon?: SmeltIconPath | (string & {});
  trailingIcon?: SmeltIconPath | (string & {});
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
  style?: StyleProp<ViewStyle>;
  testID?: string;
  onPress?: (e?: any) => void;
  children?: JSX.Element;
}

export type OverlaySide = "top" | "bottom" | "left" | "right";
export declare const OVERLAY_MOTION_CLASS: string;
export declare function popupMotion(side?: OverlaySide): {
  popupStartingClass: string;
  popupEndingClass: string;
};
export declare const CONTEXT_MENU_MOTION: { popupStartingClass: string; popupEndingClass: string };
export declare const TOOLTIP_MOTION: { startingClass: string; endingClass: string };
export declare const POPUP_CLASS: string;
export declare const TOOLTIP_CLASS: string;
export declare const MENU_ITEM_CLASS: string;
export declare const INPUT_CLASS: string;
export declare const DIALOG_SURFACE_CLASS: string;

export declare function SmeltButton(props: SmeltButtonProps): JSX.Element;
export declare function SmeltWindowBar(props: { view: string }): JSX.Element;

export declare function SmeltMarkdown(props: {
  markdown: string;
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
