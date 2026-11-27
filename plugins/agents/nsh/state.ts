// 智能体插件的响应式状态、运行时桥接、配置编辑队列、记忆监听与设置页动作。

import { signal } from "nsh";
import {
  agentCatalog,
  bootTask,
  call,
  emitEvent,
  initHost,
  machineMeta,
  machineMetaEntry,
  newSessionId,
  onCommand,
  onEvent,
  registerCommands,
  registerTopics,
  registerViews,
  request,
  run,
  sessionStates,
  stableMemo,
  t,
  text,
  texts,
  views,
  viewShown,
  currentTheme,
  waitOnline,
  workspaceDirectory,
} from "smelt/sdk";

import {
  AGENT_SETTINGS_CHANGED,
  SETTINGS_VIEW,
  VIEW,
} from "../contract.js";
import {
  agentConversationCount,
  agentConversationStartError,
  agentDisplayName,
  agentRefKey,
  applyAutomationsPayload,
  automationNamesForAgent,
  automationNextRunAt,
  automationRunsForAgent,
  automationsForAgent,
  cliExecutableForAgent,
  cloneJson,
  createAutomationsProjection,
  createEditQueue,
  formatClockHHMM,
  formatEnvLines,
  isAgentConversationReady,
  isCliExecutableAvailable,
  launchEntryIsBuiltin,
  machineFromKey,
  machineKey,
  newAgentDefinition,
  newConversationLabel,
  parseAgentHostSettings,
  parseEnvLines,
  planNewConversation,
  profileKindMeta,
  removeAgent,
  retainAutomationsMachines,
  sameAgentRef,
  sessionPhaseSettled,
  sortMachines,
  stableEqual,
} from "../model.js";

const LOCAL: string | null = null;
const EDIT_TIMEOUT_MS = 10_000;
const CLI_INSTALL_TIMEOUT_MS = 900_000;
const TEXT_DEBOUNCE_MS = 250;

export const CAPABILITIES_VIEW = "smelt.capabilities/main";
export const AUTOMATIONS_VIEW = "smelt.automations/main";
const START_CONVERSATION_CMD = "smelt.sessions/start-conversation";

export const SETTINGS_KEYWORDS = [
  "智能体",
  "运行环境",
  "cli",
  "安装",
  "版本",
  "检测",
  "路径",
  "claude",
  "codex",
  "copilot",
  "grok",
  "cursor",
  "opencode",
  "kiro",
  "pi",
  "hook",
  "hooks",
  "Agent Hooks",
  "通知接入",
  "配置文件",
  "自定义 agent",
  "acp",
  "适配器",
  "环境变量",
  "数据目录",
  "config dir",
  "启动命令",
  "参数",
  "快捷启动项",
  "启动",
  "launch",
  "命令",
  "+",
  "加号",
  "新建菜单",
  "项目菜单",
];

// ---- 类型 ----

export interface AgentRef {
  machine: string | null;
  id: string;
}

export type EditorTab =
  | { kind: "config" }
  | { kind: "automations" }
  | { kind: "memory" };

export type MemoryFilter = "all" | "pending" | "rules" | "facts";
export type MemoryLayer = "agent" | "user";

export type MachineAgentHost =
  | { state: "ready"; settings: any }
  | { state: "unavailable"; reason: string };

export interface MemoryListing {
  entries: any[];
  core_over_budget: boolean;
}

export interface AgentMemoriesState {
  agent: MemoryListing;
  user: MemoryListing;
}

export interface PiSnapshotState {
  config: any | null;
  error: string | null;
  loading: boolean;
}

export interface ConfirmRequest {
  title: string;
  action: string;
  description: string;
  danger: boolean;
  onConfirm: () => void;
}

export interface ProfileDraft {
  label: string;
  command: string;
  envText: string;
  workspace_dir: string;
  envError: string | null;
}

export interface LaunchRowState {
  uid: string;
  label: string;
  command: string;
  provider: string | null;
}

interface LaunchWireEntry {
  label: string;
  command: string;
  provider: string | null;
}

export const EMPTY_MEMORIES: AgentMemoriesState = {
  agent: { entries: [], core_over_budget: false },
  user: { entries: [], core_over_budget: false },
};

// ---- 主题与元数据 ----

export { currentTheme };

export function errText(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error ?? "");
}

export function catalog() {
  return agentCatalog();
}

export function agentEngineIds(): string[] {
  return catalog().agent_engines.map((e) => e.id);
}

export function findEngineMeta(id: string) {
  const cat = catalog();
  return (
    cat.agent_engines.find((e) => e.id === id) ??
    cat.conversation_agents.find((e) => e.id === id) ??
    null
  );
}

export function notify(subtitle: string, body: string) {
  void run("smelt.app/notify", { subtitle, body }).catch(() => {});
}

// ---- 各台机器的智能体配置只读缓存（权威在运行时） ----

const agentHostsMap = new Map<string, MachineAgentHost>();
export const agentHostsSignal = signal<Map<string, MachineAgentHost>>(new Map());

export function hostEntry(machine: string | null): MachineAgentHost | null {
  return agentHostsSignal().get(machineKey(machine)) ?? null;
}

export function hostSettings(machine: string | null): any | null {
  const entry = hostEntry(machine);
  return entry?.state === "ready" ? entry.settings : null;
}

export function hostUnavailable(machine: string | null): string | null {
  const entry = hostEntry(machine);
  return entry?.state === "unavailable" ? entry.reason : null;
}

export function agentSettingsSnapshot(): Array<
  | { machine: string | null; state: "ready"; settings: any }
  | { machine: string | null; state: "unavailable"; reason: string }
> {
  const out: Array<
    | { machine: string | null; state: "ready"; settings: any }
    | { machine: string | null; state: "unavailable"; reason: string }
  > = [];
  const keys = [...agentHostsMap.keys()].sort();
  for (const key of keys) {
    const entry = agentHostsMap.get(key);
    if (!entry) continue;
    const machine = machineFromKey(key);
    if (entry.state === "ready") {
      out.push({ machine, state: "ready", settings: entry.settings });
    } else {
      out.push({ machine, state: "unavailable", reason: entry.reason });
    }
  }
  return out;
}

function setAgentHost(machine: string | null, next: MachineAgentHost | null) {
  const key = machineKey(machine);
  const current = agentHostsMap.get(key) ?? null;
  if (stableEqual(current, next)) {
    return;
  }
  const firstLocalSnapshot = machine == null && current?.state !== "ready";
  if (next == null) {
    agentHostsMap.delete(key);
  } else {
    agentHostsMap.set(key, next);
  }
  agentHostsSignal.set(new Map(agentHostsMap));

  const payload =
    next == null
      ? { machine: machine ?? null, state: "absent" }
      : next.state === "ready"
        ? { machine: machine ?? null, state: "ready", settings: next.settings }
        : { machine: machine ?? null, state: "unavailable", reason: next.reason };
  void emitEvent(AGENT_SETTINGS_CHANGED, payload).catch((err) => {
    console.error("[agents] emit agent-settings-changed failed:", errText(err));
  });

  if (firstLocalSnapshot && next?.state === "ready" && next.settings?.hooks_enabled === true) {
    void call(LOCAL, "agent.hooks.install", {}).catch((err) => {
      console.error("[agents] 自动安装 Agent hooks 失败：", errText(err));
    });
  }

  syncProfileDraftsFromHost();
  onAgentsOrMachinesChanged();
  syncNewConversationTitle();
}

function receiveAgentHost(machine: string | null, message: any) {
  if (!message || typeof message !== "object") return;
  if (message.kind === "lagged") return;
  let next: MachineAgentHost;
  if (message.kind === "event" || message.kind === "snapshot") {
    const parsed = parseAgentHostSettings(message.payload);
    if (parsed.ok) {
      next = { state: "ready", settings: parsed.settings };
    } else {
      console.error(`[agents] ${machine ?? "local"} 推来的智能体配置解不开：`, parsed.error);
      next = {
        state: "unavailable",
        reason: t("error.settings_unreadable", { error: parsed.error }),
      };
    }
  } else if (message.kind === "unavailable") {
    next = { state: "unavailable", reason: String(message.reason ?? "") };
  } else {
    return;
  }
  setAgentHost(machine, next);
}

function forgetRemovedMachines() {
  const attached = new Set(machineMeta().map((m: any) => machineKey(m.machine)));
  for (const key of [...agentHostsMap.keys()]) {
    if (!attached.has(key)) {
      setAgentHost(machineFromKey(key), null);
    }
  }
}

export async function sendAgentHostEdit(
  machine: string | null,
  before: any,
  after: any,
): Promise<any> {
  return await call(machine ?? null, "agent.host.apply_edit", { before, after }, EDIT_TIMEOUT_MS);
}

const enqueueEdit = createEditQueue({
  getCurrent: (machine: string | null) => {
    const entry = agentHostsMap.get(machineKey(machine));
    return entry?.state === "ready" ? entry.settings : null;
  },
  getUnavailable: (machine: string | null) => {
    const entry = agentHostsMap.get(machineKey(machine));
    return entry?.state === "unavailable" ? entry.reason : null;
  },
  notReceivedMessage: () => t("error.settings_not_received"),
  sendEdit: sendAgentHostEdit,
});

export function editAgentHost(
  machine: string | null,
  mutate: (draft: any) => void,
): Promise<any> {
  return enqueueEdit(machine ?? null, mutate);
}

export function reportEditFailure(promise: Promise<unknown>) {
  void promise.catch((err) => {
    const message = errText(err);
    console.error("[agents] 改智能体配置失败：", message);
    notify(t("notify.error"), t("error.save_settings", { error: message }));
  });
}

// ---- 自动化投影只读缓存 ----

const automationsProj = createAutomationsProjection();
export const automationsVersion = signal(0);
export const automationsTabError = signal<string | null>(null);

export function clearAutomationsTabError() {
  automationsTabError.set(null);
}

export function automationsViewRegistered(): boolean {
  return views().some((v: any) => v.id === AUTOMATIONS_VIEW);
}

export function agentAutomationsFor(machine: string | null, agentId: string): any[] {
  automationsVersion();
  return automationsForAgent(automationsProj, machine, agentId);
}

export function agentAutomationNextRunAt(machine: string | null, automationId: string): number | null {
  automationsVersion();
  return automationNextRunAt(automationsProj, machine, automationId);
}

export function agentAutomationRunsFor(machine: string | null, agentId: string): any[] {
  automationsVersion();
  return automationRunsForAgent(automationsProj, machine, agentId);
}

export function openAutomationsView(args: Record<string, unknown>) {
  void run("smelt.app/open-view", { view: AUTOMATIONS_VIEW, args }).catch((err) => {
    automationsTabError.set(errText(err));
  });
}

export function setAgentAutomationEnabled(
  machine: string | null,
  automationId: string,
  enabled: boolean,
) {
  automationsTabError.set(null);
  void request(machine, {
    op: "automation_command",
    command: {
      command: "set_enabled",
      automation_id: automationId,
      enabled,
    },
  }).catch((err) => {
    automationsTabError.set(errText(err));
  });
}

export function automationCountFor(machine: string | null, agentId: string): number {
  automationsVersion();
  return automationsForAgent(automationsProj, machine, agentId).length;
}

export function automationNamesFor(machine: string | null, agentId: string): string[] {
  automationsVersion();
  return automationNamesForAgent(automationsProj, machine, agentId).map(
    (name: string | null) => name ?? t("delete.unnamed_automation"),
  );
}

// ---- 机器与默认对话智能体决议 ----

export function attachedMachines(): any[] {
  return sortMachines(machineMeta());
}

export function agentMachines(): Array<string | null> {
  return attachedMachines()
    .map((m: any) => (typeof m.machine === "string" && m.machine.length > 0 ? m.machine : null))
    .filter((m) => hostSettings(m) != null);
}

export function creationMachines(): Array<string | null> {
  const list = agentMachines();
  return list.length > 0 ? list : [LOCAL];
}

export function machineDisplayName(machine: string | null): string {
  const entry = machineMetaEntry(machine);
  if (entry?.name) return String(entry.name);
  return machine == null ? t("machine.local") : machine;
}

export function machineUnavailableReason(machine: string | null): string | null {
  const reason = machineMetaEntry(machine)?.unavailable;
  return typeof reason === "string" && reason.length > 0 ? reason : null;
}

export function agentsOnMachine(machine: string | null): any[] {
  return hostSettings(machine)?.agents ?? [];
}

export function getAgentDefinition(machine: string | null, id: string): any | null {
  return agentsOnMachine(machine).find((a: any) => a.id === id) ?? null;
}

export function isDefaultAgent(machine: string | null, id: string): boolean {
  return hostSettings(machine)?.default_conversation_agent === id;
}

export function getNewConversationPlan() {
  const map = new Map<string, any>();
  for (const [key, entry] of agentHostsSignal().entries()) {
    if (entry.state === "ready") {
      map.set(key, entry.settings);
    }
  }
  return planNewConversation(agentMachines(), map, conversationDraft(), agentEngineIds());
}

export function setDefaultConversationAgent(ref: AgentRef) {
  const targetKey = machineKey(ref.machine);
  for (const m of agentMachines()) {
    const target = machineKey(m) === targetKey ? ref.id : null;
    reportEditFailure(
      editAgentHost(m, (cfg) => {
        cfg.default_conversation_agent = target;
      }),
    );
  }
}

export function clearDefaultConversationAgent(machine: string | null) {
  reportEditFailure(
    editAgentHost(machine, (cfg) => {
      cfg.default_conversation_agent = null;
    }),
  );
}

// ---- 页面与编辑器状态 ----

export const editingAgent = signal<AgentRef | null>(null);
export const editorTab = signal<EditorTab>({ kind: "config" });
export const pageError = signal<string | null>(null);

export const conversationDraft = signal<AgentRef | null>(null);
export const armConversationOnCreate = signal(false);
export const pickingForConversation = signal(false);

export const editorName = signal("");
export const editorPrompt = signal("");
export const contextLinkInput = signal("");
export const capabilityQuery = signal("");
export const memoryQuery = signal("");
export const memoryFilter = signal<MemoryFilter>("all");
export const expandedMemory = signal<{ layer: MemoryLayer; file_name: string } | null>(null);

export const piSnapshot = signal<PiSnapshotState>({
  config: null,
  error: null,
  loading: false,
});
export const memoriesState = signal<AgentMemoriesState>(EMPTY_MEMORIES);
export const confirmDialog = signal<ConfirmRequest | null>(null);

const watchedMemories = new Set<string>();
let sessionSettled = new Map<string, boolean>();
let piGen = 0;
let memoryGen = 0;

// ---- 防抖保存名字与指令（250ms，并在失焦/切页签/关闭/开对话时立即冲刷） ----

interface PendingAgentText {
  ref: AgentRef;
  name: string;
  prompt: string;
}

const pendingTextByRef = new Map<string, PendingAgentText>();
let textSaveTimer: ReturnType<typeof setTimeout> | null = null;
let activeTextFlush: Promise<void> = Promise.resolve();

function scheduleEditorTextSave() {
  const ref = editingAgent();
  if (!ref) return;
  const key = agentRefKey(ref.machine, ref.id);
  pendingTextByRef.set(key, {
    ref: { machine: ref.machine, id: ref.id },
    name: editorName(),
    prompt: editorPrompt(),
  });
  if (textSaveTimer != null) {
    clearTimeout(textSaveTimer);
  }
  textSaveTimer = setTimeout(() => {
    textSaveTimer = null;
    void flushEditorText();
  }, TEXT_DEBOUNCE_MS);
}

export function setEditorNameInput(value: string) {
  editorName.set(value);
  scheduleEditorTextSave();
}

export function setEditorPromptInput(value: string) {
  editorPrompt.set(value);
  scheduleEditorTextSave();
}

export function discardPendingTextFor(ref: AgentRef) {
  pendingTextByRef.delete(agentRefKey(ref.machine, ref.id));
  if (pendingTextByRef.size === 0 && textSaveTimer != null) {
    clearTimeout(textSaveTimer);
    textSaveTimer = null;
  }
}

export function flushEditorText(): Promise<void> {
  if (textSaveTimer != null) {
    clearTimeout(textSaveTimer);
    textSaveTimer = null;
  }
  if (pendingTextByRef.size === 0) {
    return activeTextFlush;
  }
  const batch = [...pendingTextByRef.values()];
  pendingTextByRef.clear();
  const next = activeTextFlush.then(async () => {
    for (const item of batch) {
      try {
        await editAgentHost(item.ref.machine, (cfg) => {
          const found = cfg.agents.find((a: any) => a.id === item.ref.id);
          if (found) {
            found.name = item.name;
            found.prompt = item.prompt;
          }
        });
      } catch (err) {
        pageError.set(errText(err));
      }
    }
  });
  activeTextFlush = next.catch(() => {});
  return next;
}

// ---- 编辑器操作 ----

function seedSessionSettled(machine: string | null) {
  const next = new Map<string, boolean>();
  for (const s of sessionStates(machine)) {
    if (s && typeof s.id === "string") {
      next.set(s.id, sessionPhaseSettled(effectivePhase(s)));
    }
  }
  sessionSettled = next;
}

function effectivePhase(sessionState: any): string {
  const phase = typeof sessionState?.phase === "string" ? sessionState.phase : "idle";
  if (
    sessionState?.turn_events === true ||
    phase === "connecting" ||
    phase === "idle" ||
    phase === "dead"
  ) {
    return phase;
  }
  return "idle";
}

function editCurrentAgent(agentId: string, mutate: (agent: any) => void) {
  const ref = editingAgent();
  if (!ref || ref.id !== agentId) return;
  pageError.set(null);
  void editAgentHost(ref.machine, (cfg) => {
    const found = cfg.agents.find((a: any) => a.id === agentId);
    if (found) {
      mutate(found);
    }
  }).catch((err) => {
    pageError.set(errText(err));
  });
}

export function editingConversationDraft(): boolean {
  return sameAgentRef(conversationDraft(), editingAgent());
}

export function releaseConversationDraft() {
  armConversationOnCreate.set(false);
  if (conversationDraft() != null) {
    conversationDraft.set(null);
    syncNewConversationTitle();
  }
}

export function openAgent(ref: AgentRef, tab: EditorTab = { kind: "config" }, fallbackDef?: any) {
  pageError.set(null);
  const current = editingAgent();
  if (sameAgentRef(current, ref)) {
    void flushEditorText();
    editorTab.set(tab);
    return;
  }
  void flushEditorText();
  const def = getAgentDefinition(ref.machine, ref.id) ?? fallbackDef ?? null;
  if (!def) {
    closeEditor(false);
    return;
  }
  editingAgent.set({ machine: ref.machine ?? null, id: ref.id });
  editorTab.set(tab);
  editorName.set(def.name ?? "");
  editorPrompt.set(def.prompt ?? "");
  contextLinkInput.set("");
  capabilityQuery.set("");
  memoryQuery.set("");
  memoryFilter.set("all");
  expandedMemory.set(null);
  piSnapshot.set({ config: null, error: null, loading: true });
  memoriesState.set(EMPTY_MEMORIES);
  seedSessionSettled(ref.machine ?? null);
  refreshAgentMemories();
  refreshAgentPiConfig();
}

export function closeEditor(shouldFlush = true) {
  if (shouldFlush) {
    void flushEditorText();
  }
  editingAgent.set(null);
  editorTab.set({ kind: "config" });
  pageError.set(null);
  sessionSettled.clear();
}

export function closeAgentSettings() {
  if (editingConversationDraft()) {
    cancelConversationDraft();
    return;
  }
  closeEditor(true);
}

export function selectEditorTab(tab: EditorTab) {
  void flushEditorText();
  editorTab.set(tab);
}

export async function createAgent(machine: string | null, engineKindId: string) {
  const id = newSessionId();
  const existing = agentsOnMachine(machine).length;
  const meta = findEngineMeta(engineKindId);
  const kindLabel = meta?.short_label ?? engineKindId;
  const name =
    existing === 0
      ? t("agent.default_name", { kind: kindLabel })
      : t("agent.default_name_numbered", { kind: kindLabel, number: existing + 1 });

  const ref: AgentRef = { machine: machine ?? null, id };
  const asDraft = armConversationOnCreate();
  armConversationOnCreate.set(false);
  const created = newAgentDefinition(id, name, engineKindId);

  try {
    const applied = await editAgentHost(machine, (cfg) => {
      cfg.agents.push(created);
    });
    const parsed = parseAgentHostSettings(applied);
    if (parsed.ok) {
      setAgentHost(machine ?? null, { state: "ready", settings: parsed.settings });
    }
    if (asDraft) {
      conversationDraft.set(ref);
      syncNewConversationTitle();
    }
    openAgent(ref, { kind: "config" }, created);
  } catch (err) {
    armConversationOnCreate.set(asDraft);
    pageError.set(errText(err));
  }
}

export function cancelConversationDraft() {
  const draft = conversationDraft();
  if (!draft) return;
  discardPendingTextFor(draft);
  conversationDraft.set(null);
  armConversationOnCreate.set(false);
  syncNewConversationTitle();
  if (sameAgentRef(editingAgent(), draft)) {
    closeEditor(false);
  }
  void editAgentHost(draft.machine, (cfg) => removeAgent(cfg, draft.id)).catch((err) => {
    pageError.set(t("error.discard_failed", { error: errText(err) }));
  });
}

export async function commitConversationDraft() {
  await flushEditorText();
  const draft = conversationDraft();
  if (!draft) return;
  const def = getAgentDefinition(draft.machine, draft.id);
  if (!isAgentConversationReady(def, agentEngineIds())) {
    pageError.set(t("error.not_ready"));
    return;
  }
  conversationDraft.set(null);
  armConversationOnCreate.set(false);
  syncNewConversationTitle();
  setDefaultConversationAgent(draft);
  closeEditor(false);
  startAgentConversation(draft);
}

export function chooseDefaultConversationAgent(ref: AgentRef, start: boolean) {
  const def = getAgentDefinition(ref.machine, ref.id);
  if (!isAgentConversationReady(def, agentEngineIds())) {
    if (isDefaultAgent(ref.machine, ref.id)) {
      clearDefaultConversationAgent(ref.machine);
    }
    pageError.set(t("error.cannot_start"));
    return;
  }
  releaseConversationDraft();
  pickingForConversation.set(false);
  setDefaultConversationAgent(ref);
  if (start) {
    startAgentConversation(ref);
  }
}

export async function startConversation(ref: AgentRef): Promise<void> {
  await flushEditorText();
  const def = getAgentDefinition(ref.machine, ref.id);
  if (!def) {
    throw new Error(t("error.agent_gone"));
  }
  const startErr = agentConversationStartError(def, agentEngineIds(), t);
  if (startErr) {
    throw new Error(startErr);
  }
  let space: any;
  try {
    space = await call(ref.machine ?? null, "agent.space.ensure", { id: ref.id });
  } catch (err) {
    throw new Error(t("error.workspace_failed", { error: errText(err) }));
  }
  await run(START_CONVERSATION_CMD, {
    machine: ref.machine ?? null,
    agent_definition_id: ref.id,
    cwd: space?.path ?? "",
  });
}

export function startAgentConversation(ref: AgentRef) {
  pageError.set(null);
  void startConversation(ref).catch((err) => {
    pageError.set(errText(err));
  });
}

export function requestDeleteAgent(ref: AgentRef) {
  const def = getAgentDefinition(ref.machine, ref.id);
  if (!def) return;
  const name = agentDisplayName(def.name, t);
  const conversations = agentConversationCount(workspaceDirectory(ref.machine), ref.id);
  const description =
    conversations === 0
      ? t("delete.message", { name })
      : conversations === 1
        ? t("delete.message_kept.one", { name })
        : t("delete.message_kept.other", { name, count: conversations });
  confirmDialog.set({
    title: t("delete.confirm_title"),
    action: t("common.delete"),
    description,
    danger: true,
    onConfirm: () => deleteAgent(ref),
  });
}

function deleteAgent(ref: AgentRef) {
  pageError.set(null);
  discardPendingTextFor(ref);
  if (sameAgentRef(conversationDraft(), ref)) {
    conversationDraft.set(null);
    armConversationOnCreate.set(false);
    syncNewConversationTitle();
  }
  if (sameAgentRef(editingAgent(), ref)) {
    closeEditor(false);
  }
  void editAgentHost(ref.machine, (cfg) => removeAgent(cfg, ref.id)).catch((err) => {
    pageError.set(errText(err));
  });
}

export function setAgentEngine(agentId: string, engineKindId: string) {
  editCurrentAgent(agentId, (agent) => {
    agent.engine_kind = engineKindId;
  });
}

export function setAgentModel(agentId: string, modelProvider: string, modelId: string) {
  const provider = modelProvider.trim();
  const model = modelId.trim();
  editCurrentAgent(agentId, (agent) => {
    agent.model_provider = provider;
    agent.model_id = model;
  });
}

export function setAgentMemoryEnabled(agentId: string, enabled: boolean) {
  editCurrentAgent(agentId, (agent) => {
    agent.memory_enabled = enabled;
  });
}

export function toggleAgentPlugin(agentId: string, pluginId: string) {
  editCurrentAgent(agentId, (agent) => {
    const idx = agent.plugins.indexOf(pluginId);
    if (idx >= 0) {
      agent.plugins.splice(idx, 1);
    } else {
      agent.plugins.push(pluginId);
    }
  });
}

export function toggleAgentMcpServer(agentId: string, serverName: string) {
  editCurrentAgent(agentId, (agent) => {
    const idx = agent.mcp_servers.indexOf(serverName);
    if (idx >= 0) {
      agent.mcp_servers.splice(idx, 1);
    } else {
      agent.mcp_servers.push(serverName);
    }
  });
}

export async function pickAgentContextFolders(agentId: string) {
  const ref = editingAgent();
  if (!ref || ref.id !== agentId) return;
  try {
    const picked = await run("smelt.app/pick-paths", {
      machine: ref.machine ?? null,
      title: t("context.pick_title"),
      confirm: t("context.pick_confirm"),
      files: false,
      multiple: true,
    });
    if (!Array.isArray(picked) || picked.length === 0) return;
    editCurrentAgent(agentId, (agent) => {
      for (const raw of picked) {
        if (typeof raw !== "string") continue;
        const folder = raw.replace(/\/+$/, "");
        if (!folder || agent.context_folders.includes(folder)) continue;
        agent.context_folders.push(folder);
      }
    });
  } catch (err) {
    pageError.set(errText(err));
  }
}

export function removeAgentContextFolder(agentId: string, folder: string) {
  editCurrentAgent(agentId, (agent) => {
    agent.context_folders = agent.context_folders.filter((item: string) => item !== folder);
  });
}

export function addAgentContextLink(agentId: string) {
  const ref = editingAgent();
  if (!ref || ref.id !== agentId) return;
  const link = contextLinkInput().trim();
  if (!link) return;
  editCurrentAgent(agentId, (agent) => {
    if (!agent.context_links.includes(link)) {
      agent.context_links.push(link);
    }
  });
  contextLinkInput.set("");
}

export function removeAgentContextLink(agentId: string, link: string) {
  editCurrentAgent(agentId, (agent) => {
    agent.context_links = agent.context_links.filter((item: string) => item !== link);
  });
}

export function capabilitiesViewRegistered(): boolean {
  return views().some((v: any) => v.id === CAPABILITIES_VIEW);
}

export function openCapabilities(tab = "skills") {
  if (!capabilitiesViewRegistered()) return;
  const machine = editingAgent()?.machine ?? null;
  void run("smelt.app/open-view", {
    view: CAPABILITIES_VIEW,
    args: { machine, tab },
  }).catch((err) => {
    pageError.set(errText(err));
  });
}

// ---- Pi 配置与记忆读取 ----

export function refreshAgentPiConfig() {
  const ref = editingAgent();
  if (!ref) return;
  const gen = ++piGen;
  const prev = piSnapshot();
  piSnapshot.set({ ...prev, loading: true });
  void call(ref.machine ?? null, "pi.config.get", {})
    .then((config) => {
      if (!sameAgentRef(editingAgent(), ref) || gen !== piGen) return;
      piSnapshot.set({ config, error: null, loading: false });
    })
    .catch((err) => {
      if (!sameAgentRef(editingAgent(), ref) || gen !== piGen) return;
      piSnapshot.set({
        config: piSnapshot().config,
        error: t("pi.load_failed", { error: errText(err) }),
        loading: false,
      });
    });
}

function normalizeMemories(raw: any): AgentMemoriesState {
  return {
    agent: {
      entries: Array.isArray(raw?.agent?.entries) ? raw.agent.entries : [],
      core_over_budget: Boolean(raw?.agent?.core_over_budget),
    },
    user: {
      entries: Array.isArray(raw?.user?.entries) ? raw.user.entries : [],
      core_over_budget: Boolean(raw?.user?.core_over_budget),
    },
  };
}

export function refreshAgentMemories() {
  const ref = editingAgent();
  if (!ref) return;
  const key = agentRefKey(ref.machine, ref.id);
  const gen = ++memoryGen;
  const needWatch = !watchedMemories.has(key);
  if (needWatch) {
    watchedMemories.add(key);
  }
  void (async () => {
    if (needWatch) {
      try {
        await call(ref.machine ?? null, "agent.memory.watch", { id: ref.id });
      } catch (err) {
        console.error("[agents] 请运行时监听记忆失败：", errText(err));
        watchedMemories.delete(key);
      }
    }
    try {
      const raw = await call(ref.machine ?? null, "agent.memory.list", { id: ref.id });
      if (!sameAgentRef(editingAgent(), ref) || gen !== memoryGen) return;
      const next = normalizeMemories(raw);
      if (!stableEqual(memoriesState(), next)) {
        memoriesState.set(next);
      }
    } catch (err) {
      if (!sameAgentRef(editingAgent(), ref) || gen !== memoryGen) return;
      pageError.set(t("memory.load_failed", { error: errText(err) }));
    }
  })();
}

export function toggleMemoryExpanded(layer: MemoryLayer, fileName: string) {
  const cur = expandedMemory();
  if (cur && cur.layer === layer && cur.file_name === fileName) {
    expandedMemory.set(null);
  } else {
    expandedMemory.set({ layer, file_name: fileName });
  }
}

export function openAgentMemoryFile(filePath: string) {
  const ref = editingAgent();
  if (!ref || ref.machine != null) return;
  void run("smelt.app/open-in-text-editor", { path: filePath }).catch((err) => {
    pageError.set(errText(err));
  });
}

export function openAgentMemoryFolder(agentId: string, layer: MemoryLayer) {
  const ref = editingAgent();
  if (!ref || ref.id !== agentId || ref.machine != null) return;
  void call(LOCAL, "agent.memory.ensure_dir", { agent_id: agentId, layer })
    .then((dir) => run("smelt.app/open-in-file-manager", { path: dir?.path ?? "" }))
    .catch((err) => {
      pageError.set(errText(err));
    });
}

export function approveAgentMemory(agentId: string, layer: MemoryLayer, fileName: string) {
  const ref = editingAgent();
  if (!ref || ref.id !== agentId) return;
  void call(ref.machine ?? null, "agent.memory.approve", {
    agent_id: agentId,
    layer,
    file_name: fileName,
  })
    .then(() => {
      pageError.set(null);
      refreshAgentMemories();
    })
    .catch((err) => {
      pageError.set(t("error.detail", { what: t("memory.accept_failed"), error: errText(err) }));
      refreshAgentMemories();
    });
}

export function requestDeleteMemory(target: {
  agent_id: string;
  layer: MemoryLayer;
  file_name: string;
  name: string;
}) {
  const description =
    target.layer === "agent"
      ? t("memory.delete_message_agent", { name: target.name })
      : t("memory.delete_message_user", { name: target.name });
  confirmDialog.set({
    title: t("memory.delete_title"),
    action: t("common.delete"),
    description,
    danger: true,
    onConfirm: () => {
      const ref = editingAgent();
      if (!ref || ref.id !== target.agent_id) return;
      void call(ref.machine ?? null, "agent.memory.delete", {
        agent_id: target.agent_id,
        layer: target.layer,
        file_name: target.file_name,
      })
        .then(() => {
          pageError.set(null);
          refreshAgentMemories();
        })
        .catch((err) => {
          pageError.set(
            t("error.detail", { what: t("memory.delete_failed"), error: errText(err) }),
          );
          refreshAgentMemories();
        });
    },
  });
}

// ---- 设置「Agent」页状态：CLI、Hooks、自定义 Agent、新建菜单启动项 ----

export const cliDiagnostics = signal<any | null>(null);
export const cliRefreshing = signal(false);
export const cliCheckedAt = signal<string | null>(null);
export const cliInstalling = signal<string | null>(null);
export const cliInstallResult = signal<{ message: string; failed: boolean } | null>(null);

export const hooksStatus = signal<{
  providers: Array<{ label: string; installed: boolean }>;
  notify_path: string;
} | null>(null);
export const hooksBusy = signal(false);

export const profileDrafts = signal<Record<string, ProfileDraft>>({});
const profileSaveTimers = new Map<string, ReturnType<typeof setTimeout>>();
const profileDirtyIds = new Set<string>();

export const launchRows = signal<LaunchRowState[] | null>(null);
export const launchError = signal<string | null>(null);
let launchServerWire: LaunchWireEntry[] | null = null;
let launchUidCounter = 0;
let launchLoadToken = 0;
let launchSending = false;
let launchSaveTimer: ReturnType<typeof setTimeout> | null = null;
const launchPending: Array<{ base: LaunchWireEntry[]; entries: LaunchWireEntry[] }> = [];

let settingsInitialized = false;
let settingsWasShown = false;

export function ensureSettingsInitialized() {
  if (settingsInitialized) return;
  settingsInitialized = true;
  settingsWasShown = true;
  onSettingsShown();
}

export function onSettingsShown() {
  refreshHooks();
  loadLaunch();
  if (cliDiagnostics() == null) {
    refreshCli();
  }
}

export function refreshCli() {
  if (cliRefreshing() || cliInstalling() != null) return;
  cliRefreshing.set(true);
  void call(LOCAL, "agent.cli.inspect", {})
    .then((diag) => {
      cliDiagnostics.set(diag);
    })
    .catch((err) => {
      console.error("[agents] 探测 agent CLI 失败：", errText(err));
    })
    .finally(() => {
      cliRefreshing.set(false);
      cliCheckedAt.set(formatClockHHMM(new Date()));
    });
}

export function installCli(agentId: string, agentLabel: string) {
  if (cliRefreshing() || cliInstalling() != null) return;
  cliInstalling.set(agentId);
  cliInstallResult.set(null);
  void (async () => {
    let result: { ok: true; installer: string } | { ok: false; error: string };
    let diagnostics: any | null = null;
    try {
      const installed = await call(
        LOCAL,
        "agent.cli.install",
        { agent: agentId },
        CLI_INSTALL_TIMEOUT_MS,
      );
      result = { ok: true, installer: String(installed?.installer ?? "") };
      diagnostics = installed?.diagnostics ?? null;
    } catch (err) {
      result = { ok: false, error: errText(err) };
      diagnostics = await call(LOCAL, "agent.cli.inspect", {}).catch(() => null);
    }

    const exec = diagnostics ? cliExecutableForAgent(diagnostics, agentId) : null;
    const available = exec == null || isCliExecutableAvailable(exec);
    if (result.ok && available) {
      cliInstallResult.set({
        message: t("cli.installed", { agent: agentLabel, installer: result.installer }),
        failed: false,
      });
    } else if (result.ok) {
      cliInstallResult.set({
        message: t("cli.installed_not_found", { agent: agentLabel, installer: result.installer }),
        failed: true,
      });
    } else {
      cliInstallResult.set({
        message: t("cli.install_failed", { error: result.error }),
        failed: true,
      });
    }
    if (diagnostics != null) {
      cliDiagnostics.set(diagnostics);
    }
    cliInstalling.set(null);
    cliCheckedAt.set(formatClockHHMM(new Date()));
  })();
}

export function refreshHooks() {
  void call(LOCAL, "agent.hooks.status", {})
    .then((status) => {
      if (status && typeof status === "object") {
        hooksStatus.set({
          providers: Array.isArray(status.providers) ? status.providers : [],
          notify_path: typeof status.notify_path === "string" ? status.notify_path : "",
        });
      }
    })
    .catch((err) => {
      console.error("[agents] 读 agent hooks 状态失败：", errText(err));
    });
}

export function setHooks(enabled: boolean) {
  if (hooksBusy()) return;
  reportEditFailure(
    editAgentHost(LOCAL, (settings) => {
      settings.hooks_enabled = enabled;
    }),
  );
  hooksBusy.set(true);
  const doneMsg = enabled ? t("hooks.installed") : t("hooks.removed");
  const failWhat = enabled ? t("hooks.install_failed") : t("hooks.remove_failed");
  const method = enabled ? "agent.hooks.install" : "agent.hooks.uninstall";
  void call(LOCAL, method, {})
    .then((status) => {
      if (status && typeof status === "object") {
        hooksStatus.set({
          providers: Array.isArray(status.providers) ? status.providers : [],
          notify_path: typeof status.notify_path === "string" ? status.notify_path : "",
        });
      }
      notify(t("notify.done"), doneMsg);
    })
    .catch((err) => {
      const msg = errText(err);
      console.error("[agents] agent hooks：", msg);
      notify(t("notify.error"), t("error.detail", { what: failWhat, error: msg }));
      refreshHooks();
    })
    .finally(() => {
      hooksBusy.set(false);
    });
}

// ---- 自定义 Agent（Profiles） ----

function syncProfileDraftsFromHost() {
  const local = agentHostsMap.get(machineKey(LOCAL));
  const profiles: any[] = local?.state === "ready" ? (local.settings.profiles ?? []) : [];
  const current = profileDrafts();
  const next: Record<string, ProfileDraft> = {};
  const liveIds = new Set<string>();
  let changed = false;

  for (const p of profiles) {
    liveIds.add(p.id);
    const existing = current[p.id];
    if (!existing) {
      next[p.id] = {
        label: p.label ?? "",
        command: p.command ?? "",
        envText: formatEnvLines(p.env),
        workspace_dir: p.workspace_dir ?? "",
        envError: null,
      };
      changed = true;
    } else if (!profileDirtyIds.has(p.id)) {
      const serverEnv = formatEnvLines(p.env);
      const updated: ProfileDraft = {
        label: p.label ?? "",
        command: p.command ?? "",
        envText: existing.envError ? existing.envText : serverEnv,
        workspace_dir: p.workspace_dir ?? "",
        envError: existing.envError,
      };
      if (!stableEqual(existing, updated)) {
        changed = true;
      }
      next[p.id] = updated;
    } else {
      next[p.id] = existing;
    }
  }

  for (const id of Object.keys(current)) {
    if (!liveIds.has(id)) {
      changed = true;
      profileDirtyIds.delete(id);
      const timer = profileSaveTimers.get(id);
      if (timer != null) {
        clearTimeout(timer);
        profileSaveTimers.delete(id);
      }
    }
  }

  if (changed) {
    profileDrafts.set(next);
  }
}

export function getProfileDraft(profile: any): ProfileDraft {
  const draft = profileDrafts()[profile.id];
  if (draft) return draft;
  return {
    label: profile.label ?? "",
    command: profile.command ?? "",
    envText: formatEnvLines(profile.env),
    workspace_dir: profile.workspace_dir ?? "",
    envError: null,
  };
}

function scheduleProfileSave(id: string) {
  profileDirtyIds.add(id);
  const existing = profileSaveTimers.get(id);
  if (existing != null) {
    clearTimeout(existing);
  }
  profileSaveTimers.set(
    id,
    setTimeout(() => {
      profileSaveTimers.delete(id);
      flushProfileDraft(id);
    }, TEXT_DEBOUNCE_MS),
  );
}

export function flushProfileDraft(id: string) {
  const timer = profileSaveTimers.get(id);
  if (timer != null) {
    clearTimeout(timer);
    profileSaveTimers.delete(id);
  }
  if (!profileDirtyIds.has(id)) return;
  profileDirtyIds.delete(id);
  const draft = profileDrafts()[id];
  if (!draft) return;

  const parsedEnv = parseEnvLines(draft.envText, t);
  const nextEnvError = parsedEnv.ok ? null : parsedEnv.error;
  if (draft.envError !== nextEnvError) {
    profileDrafts.set({
      ...profileDrafts(),
      [id]: { ...draft, envError: nextEnvError },
    });
  }

  reportEditFailure(
    editAgentHost(LOCAL, (settings) => {
      const target = settings.profiles.find((p: any) => p.id === id);
      if (!target) return;
      target.label = draft.label;
      target.command = draft.command;
      target.workspace_dir = draft.workspace_dir;
      if (parsedEnv.ok) {
        target.env = parsedEnv.env;
      }
    }),
  );
}

export function updateProfileDraft(
  id: string,
  patch: Partial<Pick<ProfileDraft, "label" | "command" | "envText" | "workspace_dir">>,
) {
  const current = profileDrafts();
  const base =
    current[id] ??
    (() => {
      const p = hostSettings(LOCAL)?.profiles?.find((x: any) => x.id === id);
      return p
        ? {
            label: p.label ?? "",
            command: p.command ?? "",
            envText: formatEnvLines(p.env),
            workspace_dir: p.workspace_dir ?? "",
            envError: null,
          }
        : { label: "", command: "", envText: "", workspace_dir: "", envError: null };
    })();
  const updated: ProfileDraft = { ...base, ...patch };
  if (patch.envText !== undefined) {
    const parsed = parseEnvLines(patch.envText, t);
    updated.envError = parsed.ok ? null : parsed.error;
  }
  profileDrafts.set({ ...current, [id]: updated });
  scheduleProfileSave(id);
}

export function addProfile() {
  reportEditFailure(
    editAgentHost(LOCAL, (settings) => {
      settings.profiles.push({
        id: newSessionId(),
        kind_id: "custom",
        label: t("profile.new"),
        command: "",
        env: {},
        workspace_dir: "",
      });
    }),
  );
}

export function removeProfile(id: string) {
  const timer = profileSaveTimers.get(id);
  if (timer != null) {
    clearTimeout(timer);
    profileSaveTimers.delete(id);
  }
  profileDirtyIds.delete(id);
  reportEditFailure(
    editAgentHost(LOCAL, (settings) => {
      settings.profiles = settings.profiles.filter((p: any) => p.id !== id);
    }),
  );
}

export function setProfileKind(id: string, kindId: string) {
  flushProfileDraft(id);
  const meta = profileKindMeta(kindId, catalog().conversation_agents);
  const clearsDir = !meta?.config_dir_env_var;
  if (clearsDir) {
    const cur = profileDrafts();
    if (cur[id]) {
      profileDrafts.set({
        ...cur,
        [id]: { ...cur[id], workspace_dir: "" },
      });
    }
  }
  reportEditFailure(
    editAgentHost(LOCAL, (settings) => {
      const target = settings.profiles.find((p: any) => p.id === id);
      if (!target) return;
      target.kind_id = kindId;
      if (clearsDir) {
        target.workspace_dir = "";
      }
    }),
  );
}

// ---- 新建菜单启动项（Launch Entries） ----

function toLaunchWire(rows: LaunchRowState[]): LaunchWireEntry[] {
  return rows.map((r) => ({
    label: r.label,
    command: r.command,
    provider: r.provider ?? null,
  }));
}

function fromLaunchWire(entries: any[]): LaunchRowState[] {
  return (entries ?? []).map((e: any) => ({
    uid: `launch-${++launchUidCounter}`,
    label: typeof e?.label === "string" ? e.label : "",
    command: typeof e?.command === "string" ? e.command : "",
    provider: typeof e?.provider === "string" && e.provider.length > 0 ? e.provider : null,
  }));
}

export function loadLaunch() {
  if (launchSending) return;
  const token = ++launchLoadToken;
  void (async () => {
    if (!(await waitOnline(LOCAL, 0))) return;
    if (token !== launchLoadToken) return;
    try {
      const fetched = await call(LOCAL, "launch.get", {});
      if (token !== launchLoadToken) return;
      const entries: LaunchWireEntry[] = (fetched?.entries ?? []).map((e: any) => ({
        label: typeof e?.label === "string" ? e.label : "",
        command: typeof e?.command === "string" ? e.command : "",
        provider: typeof e?.provider === "string" && e.provider.length > 0 ? e.provider : null,
      }));
      launchError.set(null);
      const curRows = launchRows();
      const curWire = curRows ? toLaunchWire(curRows) : null;
      launchServerWire = cloneJson(entries);
      if (!curWire || !stableEqual(curWire, entries)) {
        launchRows.set(fromLaunchWire(entries));
      }
    } catch (err) {
      if (token !== launchLoadToken) return;
      launchError.set(errText(err));
    }
  })();
}

function enqueueLaunchReplace(nextRows: LaunchRowState[], immediate: boolean) {
  launchRows.set(nextRows);
  if (launchSaveTimer != null) {
    clearTimeout(launchSaveTimer);
    launchSaveTimer = null;
  }
  if (immediate) {
    flushLaunchEdits();
  } else {
    launchSaveTimer = setTimeout(() => {
      launchSaveTimer = null;
      flushLaunchEdits();
    }, TEXT_DEBOUNCE_MS);
  }
}

export function flushLaunchEdits() {
  if (launchSaveTimer != null) {
    clearTimeout(launchSaveTimer);
    launchSaveTimer = null;
  }
  const rows = launchRows();
  if (!rows || launchServerWire == null) return;
  const nextWire = toLaunchWire(rows);
  const baseWire =
    launchPending.length > 0
      ? launchPending[launchPending.length - 1].entries
      : launchServerWire;
  if (stableEqual(baseWire, nextWire)) return;
  launchPending.push({ base: cloneJson(baseWire), entries: cloneJson(nextWire) });
  drainLaunch();
}

function drainLaunch() {
  if (launchSending) return;
  const job = launchPending.shift();
  if (!job) return;
  launchSending = true;
  launchLoadToken += 1;
  void call(LOCAL, "launch.replace", { base: job.base, entries: job.entries })
    .then((settings) => {
      launchSending = false;
      const committed: LaunchWireEntry[] = (settings?.entries ?? job.entries).map((e: any) => ({
        label: typeof e?.label === "string" ? e.label : "",
        command: typeof e?.command === "string" ? e.command : "",
        provider: typeof e?.provider === "string" && e.provider.length > 0 ? e.provider : null,
      }));
      launchServerWire = cloneJson(committed);
      if (launchPending.length === 0) {
        const cur = launchRows();
        if (!cur || !stableEqual(toLaunchWire(cur), committed)) {
          launchRows.set(fromLaunchWire(committed));
        }
      }
      drainLaunch();
    })
    .catch((err) => {
      launchSending = false;
      launchPending.length = 0;
      launchServerWire = null;
      notify(t("notify.error"), t("launch.save_failed", { error: errText(err) }));
      launchRows.set(null);
      loadLaunch();
    });
}

export function flushAllSettingsDrafts() {
  for (const id of [...profileDirtyIds]) {
    flushProfileDraft(id);
  }
  flushLaunchEdits();
}

export function updateLaunchRow(uid: string, patch: { label?: string; command?: string }) {
  const cur = launchRows();
  if (!cur) {
    notify(t("notify.error"), t("launch.not_loaded"));
    return;
  }
  const next = cur.map((row) => (row.uid === uid ? { ...row, ...patch } : row));
  enqueueLaunchReplace(next, false);
}

export function addLaunchEntry() {
  const cur = launchRows();
  if (!cur) {
    notify(t("notify.error"), t("launch.not_loaded"));
    return;
  }
  const next: LaunchRowState[] = [
    ...cur,
    {
      uid: `launch-${++launchUidCounter}`,
      label: t("launch.new_entry"),
      command: "",
      provider: null,
    },
  ];
  enqueueLaunchReplace(next, true);
}

export function removeLaunchEntry(uid: string) {
  const cur = launchRows();
  if (!cur) {
    notify(t("notify.error"), t("launch.not_loaded"));
    return;
  }
  const target = cur.find((r) => r.uid === uid);
  if (!target || launchEntryIsBuiltin(target, catalog().terminal_agent_ids)) return;
  const next = cur.filter((r) => r.uid !== uid);
  enqueueLaunchReplace(next, true);
}

// ---- 命令注册（随默认智能体名称动态更新） ----

let commandsRegistered = false;
let titledDefault: string | null | undefined = undefined;

function buildNewConversationTitle(defaultName: string | null): Record<string, string> {
  if (defaultName == null) {
    return text("new_conversation.title");
  }
  const tables = texts();
  const out: Record<string, string> = {};
  if (tables && typeof tables === "object") {
    const trimmed = defaultName.trim();
    for (const [locale, dict] of Object.entries(tables)) {
      if (!dict) continue;
      const template = dict["new_conversation.named"];
      if (typeof template !== "string") continue;
      const display = trimmed.length > 0 ? trimmed : (dict["agent.unnamed"] ?? "Untitled");
      out[locale] = template.replaceAll("%{name}", display);
    }
  }
  if (Object.keys(out).length === 0) {
    out.en = newConversationLabel(defaultName, t);
  }
  return out;
}

async function publishCommands(defaultName: string | null, force = false): Promise<void> {
  if (!force && titledDefault === defaultName) {
    return;
  }
  titledDefault = defaultName;
  await registerCommands(
    [
      {
        name: "new-conversation",
        title: buildNewConversationTitle(defaultName),
        context: "Workspace",
        requires_args: false,
        args: [],
      },
      {
        name: "agent-settings",
        title: text("command.agent_settings"),
        requires_args: true,
        args: [],
      },
      {
        name: "edit-agent-settings",
        title: text("command.edit_agent_settings"),
        requires_args: true,
        args: ["machine", "before", "after"],
      },
    ],
    text("title"),
  );
  commandsRegistered = true;
}

function syncNewConversationTitle() {
  if (!commandsRegistered) return;
  const plan = getNewConversationPlan();
  void publishCommands(plan.default_name, false).catch((err) => {
    console.error("[agents] registerCommands 失败：", errText(err));
  });
}

// ---- 宿主事件同步 ----

function onAgentsOrMachinesChanged() {
  const attachedKeys = new Set(machineMeta().map((m: any) => machineKey(m.machine)));
  for (const key of [...watchedMemories]) {
    const sep = key.indexOf("\0");
    const mKey = sep >= 0 ? key.slice(0, sep) : "";
    if (!attachedKeys.has(mKey)) {
      watchedMemories.delete(key);
    }
  }

  const isGone = (ref: AgentRef): boolean => {
    const mKey = machineKey(ref.machine);
    if (attachedKeys.size > 0 && !attachedKeys.has(mKey)) return true;
    const settings = hostSettings(ref.machine);
    if (settings != null && !settings.agents.some((a: any) => a.id === ref.id)) {
      return true;
    }
    return false;
  };

  const draft = conversationDraft();
  if (draft && isGone(draft)) {
    conversationDraft.set(null);
  }
  const editing = editingAgent();
  if (editing && isGone(editing)) {
    discardPendingTextFor(editing);
    closeEditor(false);
  }
}

function onSessionsChanged() {
  const editing = editingAgent();
  if (!editing) {
    sessionSettled.clear();
    return;
  }
  const next = new Map<string, boolean>();
  let justSettled = false;
  for (const s of sessionStates(editing.machine)) {
    if (!s || typeof s.id !== "string") continue;
    const settled = sessionPhaseSettled(effectivePhase(s));
    next.set(s.id, settled);
    if (settled && sessionSettled.get(s.id) === false) {
      justSettled = true;
    }
  }
  sessionSettled = next;
  if (justSettled) {
    refreshAgentMemories();
  }
}

function handleMainViewOpened(args: any) {
  pageError.set(null);
  const newConv = Boolean(args?.new_conversation);
  pickingForConversation.set(newConv);
  if (newConv) {
    const noAgents = agentMachines().every((m) => agentsOnMachine(m).length === 0);
    armConversationOnCreate.set(noAgents);
    if (noAgents) {
      closeEditor(true);
    }
  }
  const agentId = typeof args?.agent === "string" && args.agent.length > 0 ? args.agent : null;
  if (agentId) {
    const machine =
      typeof args?.machine === "string" && args.machine.length > 0 ? args.machine : null;
    const tab: EditorTab =
      args?.tab === "memory"
        ? { kind: "memory" }
        : args?.tab === "automations" && automationsViewRegistered()
          ? { kind: "automations" }
          : { kind: "config" };
    const ref: AgentRef = { machine, id: agentId };
    if (getAgentDefinition(machine, agentId)) {
      releaseConversationDraft();
      openAgent(ref, tab);
    } else {
      pageError.set(t("error.agent_gone"));
    }
  } else if (conversationDraft()) {
    openAgent(conversationDraft()!, { kind: "config" });
  }
}

function onRuntimeTopic(machine: string | null, topic: string, message: any) {
  if (topic === "agent_host.changed") {
    receiveAgentHost(machine, message);
    return;
  }
  if (topic === "automations.changed") {
    if (message?.kind === "event" || message?.kind === "snapshot") {
      if (applyAutomationsPayload(automationsProj, machine, message.payload)) {
        automationsVersion.set(automationsVersion() + 1);
      }
    }
    return;
  }
  if (topic === "pi_config.changed") {
    const editing = editingAgent();
    if (editing && machineKey(editing.machine) === machineKey(machine)) {
      refreshAgentPiConfig();
    }
    return;
  }
  if (topic === "agent_memory.changed") {
    const editing = editingAgent();
    if (!editing || machineKey(editing.machine) !== machineKey(machine)) return;
    if (message?.kind === "event") {
      const changedAgentId = message.payload?.agent_id;
      if (changedAgentId == null || changedAgentId === editing.id) {
        refreshAgentMemories();
      }
    } else if (message?.kind === "snapshot" || message?.kind === "lagged") {
      refreshAgentMemories();
    }
  }
}

// ---- 启动注册与事件/命令绑定 ----

let pluginInitialized = false;

export function initPlugin() {
  if (pluginInitialized) return;
  pluginInitialized = true;

  onCommand(async ({ name, args }) => {
    switch (name) {
      case "new-conversation": {
        const plan = getNewConversationPlan();
        for (const staleMachine of plan.stale_defaults) {
          clearDefaultConversationAgent(staleMachine);
        }
        if (plan.intent.kind === "start") {
          releaseConversationDraft();
          await startConversation(plan.intent.agent);
          return null;
        }
        const openArgs =
          plan.intent.kind === "pick_agent"
            ? { new_conversation: true }
            : {
                new_conversation:
                  conversationDraft() == null &&
                  agentMachines().every((m) => agentsOnMachine(m).length === 0),
              };
        await run("smelt.app/open-view", { view: VIEW, args: openArgs });
        return null;
      }
      case "agent-settings": {
        return agentSettingsSnapshot();
      }
      case "edit-agent-settings": {
        const machine =
          typeof args?.machine === "string" && args.machine.length > 0 ? args.machine : null;
        return await sendAgentHostEdit(machine, args?.before, args?.after);
      }
      default:
        throw new Error(`Unknown command: ${name}`);
    }
  });

  onEvent(
    (event: any) => {
      if (!event) return;
      switch (event.type) {
        case "runtime_topic": {
          const machine =
            typeof event.machine === "string" && event.machine.length > 0 ? event.machine : null;
          onRuntimeTopic(machine, String(event.topic ?? ""), event.message);
          break;
        }
        case "reconnected": {
          const machine =
            typeof event.machine === "string" && event.machine.length > 0 ? event.machine : null;
          const prefix = `${machineKey(machine)}\0`;
          for (const key of [...watchedMemories]) {
            if (key.startsWith(prefix)) {
              watchedMemories.delete(key);
            }
          }
          const editing = editingAgent();
          if (editing && machineKey(editing.machine) === machineKey(machine)) {
            refreshAgentMemories();
            refreshAgentPiConfig();
          }
          break;
        }
        case "sessions_changed": {
          onSessionsChanged();
          break;
        }
        case "machines_changed": {
          forgetRemovedMachines();
          const attachedKeys = new Set(machineMeta().map((m: any) => machineKey(m.machine)));
          if (retainAutomationsMachines(automationsProj, attachedKeys)) {
            automationsVersion.set(automationsVersion() + 1);
          }
          onAgentsOrMachinesChanged();
          syncNewConversationTitle();
          break;
        }
        case "shown_views_changed": {
          if (!viewShown(VIEW)) {
            armConversationOnCreate.set(false);
            pickingForConversation.set(false);
          }
          const settingsShown = viewShown(SETTINGS_VIEW);
          if (settingsShown && !settingsWasShown) {
            onSettingsShown();
          }
          settingsWasShown = settingsShown;
          break;
        }
        case "views_changed": {
          const tab = editorTab();
          if (tab.kind === "automations" && !automationsViewRegistered()) {
            editorTab.set({ kind: "config" });
          }
          break;
        }
        case "view_opened": {
          if (event.view === VIEW) {
            handleMainViewOpened(event.args);
          } else if (event.view === SETTINGS_VIEW) {
            settingsWasShown = true;
            onSettingsShown();
          }
          break;
        }
      }
    },
    [
      "runtime_topic",
      "reconnected",
      "sessions_changed",
      "machines_changed",
      "shown_views_changed",
      "views_changed",
      "view_opened",
    ],
  );

  bootTask(async () => {
    await initHost(["machines_meta", "views", "commands"]);
    await registerTopics([
      "automations.changed",
      "agent_host.changed",
      "pi_config.changed",
      "agent_memory.changed",
    ]);
    await registerViews([
      {
        name: "main",
        export: "main",
        title: text("title"),
        icon: "icons/bot.svg",
        draws_top_bar: false,
        instances: false,
      },
      {
        name: "settings",
        export: "settings",
        title: text("settings.title"),
        icon: "icons/bot.svg",
        draws_top_bar: false,
        instances: false,
        settings: {
          group: "agents",
          keywords: SETTINGS_KEYWORDS,
        },
      },
    ]);
    const initialDefault = getNewConversationPlan().default_name;
    await publishCommands(initialDefault, true);
    syncNewConversationTitle();
  });
}
