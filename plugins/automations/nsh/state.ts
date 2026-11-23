// 自动化插件的响应式状态、事件驱动投影、SQLite 通知记录、智能体缓存与编辑器自动保存编排。
import { signal } from "nsh";
import {
  agentCatalog,
  bootTask,
  call,
  clipboardWrite,
  command,
  initHost,
  machineEntry,
  machines,
  newSessionId,
  onEvent,
  onState,
  registerTopics,
  registerViews,
  request,
  run,
  sessionStates,
  stableMemo,
  state,
  store,
  t,
  text,
  views,
} from "smelt/sdk";
import {
  AGENT_SETTINGS_CHANGED_EVENT,
  AGENT_SETTINGS_CMD,
  AGENTS_VIEW,
  CONVERSATION_VIEW,
  NOTIFY_CMD,
  OPEN_VIEW_CMD,
  PICK_PATHS_CMD,
  SET_VIEW_BADGE_CMD,
  TOPIC_AUTOMATIONS_CHANGED,
  VIEW,
  VIEW_NAME,
  WITHDRAW_NOTIFICATION_CMD,
} from "../contract.js";
import {
  ACP_ENTRY_VERSION,
  applyAutomationsSnapshot,
  automationSaveHint,
  automationTemplate,
  buildAutomationWireFromDraft,
  buildDraftFromAutomation,
  buildRunDetail,
  computeDraftMissingFields,
  countFailingAutomations,
  createEmptyMachineAutomations,
  createTriggerEntryDraft,
  decodeAutomationsPayload,
  deliveredKey,
  DeliveredStore,
  findAutomation,
  intervalDayScopeFromDays,
  isAgentReady,
  isMachineReady,
  machineKey,
  makeRunDetailKey,
  newWebhookSecret,
  normalizeAutomationFile,
  notificationId,
  notifyToSinks,
  parseDeliveredKey,
  runsForAutomation,
  sameAutomationRef,
  sameRunDetailKey,
  SCHEDULE_ALL_DAYS_MASK,
  SCHEDULE_WEEKDAYS_MASK,
  snapshotIsCurrentStore,
  templatePlaceholders,
  triggerPresetKind,
  triggerPresetSchedule,
} from "../model.js";

// ---- 类型定义 ----

export interface AutomationRef {
  machine: string | null;
  id: string;
}

export interface ScheduleRuleDraft {
  kind: "daily" | "weekdays" | "weekly" | "interval";
  intervalUnit: "minutes" | "hours";
  intervalDayScope: "everyday" | "weekdays" | "weekly";
  weekdayMask: number;
  time: string;
  interval: string;
  windowStart: string;
  windowEnd: string;
  windowOpen: boolean;
}

export type TriggerEntryDraft =
  | {
      uid: string;
      triggerKind: "calendar" | "interval";
      schedule: ScheduleRuleDraft;
    }
  | {
      uid: string;
      triggerKind: "webhook";
      webhookSecret: string;
    };

export interface EditorState {
  instanceId: string;
  machine: string | null;
  id: string;
  isNew: boolean;
  actionKind: "agent" | "shell";
  agentId: string;
  name: string;
  prompt: string;
  command: string;
  retainedEvents: any[];
  entries: TriggerEntryDraft[];
  notification: "never" | "failures_only" | "always";
  workspaceDir: string | null;
  placeholders: string[];
  draftRevision: number;
  savedRevision: number;
  savingRevision: number | null;
  savingAction: null | "create" | "autosave" | "regen" | "flush";
  regeneratingUid: string | null;
  runningOnce: boolean;
  error: string | null;
  flash: { id: string; kind: "copied" | "regenerated" } | null;
  autoFocusName: boolean;
}

export interface RunDetailKey {
  machine: string | null;
  runId: string;
  status: string;
  finishedAt: number | null;
}

export interface RunDetailState {
  key: RunDetailKey;
  output: string | null;
  instructions: string | null;
  transcript: any[];
  expanded: Set<string>;
}

export interface DeletePromptState {
  target: AutomationRef;
  name: string;
  runCount: number;
}

export type LiveRunStatus =
  | { kind: "live"; reuseKey: string; args: Record<string, unknown> }
  | { kind: "no_conversation_view" }
  | { kind: "unknown_engine" }
  | { kind: "archived" };

// ---- 全局主题与基础信号 ----

export const currentTheme = stableMemo(() => state().theme);

/** 投影变更计数（驱动依赖 Map 的 computed 重算）。 */
export const storeVersion = signal(0);
/** 智能体缓存变更计数。 */
export const agentsVersion = signal(0);

export const route = signal<AutomationRef | null>(null);
export const editor = signal<EditorState | null>(null);
export const pageError = signal<string | null>(null);
export const catalogTab = signal<"tasks" | "runs">("tasks");
export const templateFilter = signal<string | null>(null);
export const selectedRunId = signal<string | null>(null);
export const runDetail = signal<RunDetailState | null>(null);
export const runDetailLoading = signal<RunDetailKey | null>(null);
export const runDetailError = signal<{ key: RunDetailKey; message: string } | null>(null);
export const deletePrompt = signal<DeletePromptState | null>(null);

// ---- 模块内投影缓存、SQLite 通知库、智能体缓存与定时器 ----

const automationsByMachine = new Map<string, any>();
const agentsByMachine = new Map<string, any[]>();
let agentFetchState: "idle" | "loading" | "done" = "idle";
const agentTouched = new Set<string>();

const deliveredStore = new DeliveredStore(store);
const deliveredSet = new Set<string>();
let lastBadgeLabel: string | null | undefined = undefined;

let triggerUidSeq = 0;
let autosaveTimer: ReturnType<typeof setTimeout> | null = null;
let flashTimer: ReturnType<typeof setTimeout> | null = null;
let inflightSavePromise: Promise<boolean> | null = null;
let booted = false;

function nextTriggerUid(): string {
  triggerUidSeq += 1;
  return `t-${triggerUidSeq}`;
}

function errMsg(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err ?? "");
}

function bumpStore() {
  storeVersion.set(storeVersion() + 1);
}

function bumpAgents() {
  agentsVersion.set(agentsVersion() + 1);
}

function clearAutosaveTimer() {
  if (autosaveTimer !== null) {
    clearTimeout(autosaveTimer);
    autosaveTimer = null;
  }
}

function clearFlashTimer() {
  if (flashTimer !== null) {
    clearTimeout(flashTimer);
    flashTimer = null;
  }
}

// ---- 只读查询辅助（render 里安全可读） ----

export function validConversationEngineIds(): string[] {
  try {
    const list = agentCatalog().conversation_agents;
    if (Array.isArray(list) && list.length > 0) {
      return list.map((item) => item.id).filter((id) => id !== "custom");
    }
  } catch {
    // fallback in tests / early boot
  }
  return ["claude", "codex", "copilot", "antigravity", "pi"];
}

export function allConversationAgentIds(): Set<string> {
  try {
    const list = agentCatalog().conversation_agents;
    if (Array.isArray(list) && list.length > 0) {
      return new Set(list.map((item) => item.id));
    }
  } catch {
    // fallback
  }
  return new Set(["claude", "codex", "copilot", "antigravity", "pi", "custom"]);
}

export function machineAutomationsOn(machine: string | null | undefined): any | null {
  return automationsByMachine.get(machineKey(machine)) ?? null;
}

export function allMachineAutomationsEntries(): Array<[string | null, any]> {
  const out: Array<[string | null, any]> = [];
  for (const [k, v] of automationsByMachine.entries()) {
    out.push([k ? k : null, v]);
  }
  return out;
}

export function agentsOn(machine: string | null | undefined): any[] {
  return agentsByMachine.get(machineKey(machine)) ?? [];
}

export function agentDefinitionOn(machine: string | null | undefined, agentId: string): any | null {
  return agentsOn(machine).find((a) => a.id === agentId) ?? null;
}

export function readyAgentsOn(machine: string | null | undefined): any[] {
  const engines = validConversationEngineIds();
  return agentsOn(machine).filter((a) => isAgentReady(a, engines));
}

export function readyAgentIdOn(machine: string | null | undefined): string {
  return readyAgentsOn(machine)[0]?.id ?? "";
}

export function machineName(machine: string | null | undefined): string {
  const target = machine ?? null;
  const entry = machineEntry(target);
  if (entry?.name) return String(entry.name);
  return target ?? t("machine.local");
}

export function machineUnavailable(machine: string | null | undefined): string | null {
  const target = machine ?? null;
  const list = machines();
  if (list.length === 0) return null;
  const found = list.find((m) => (m.machine ?? null) === target);
  if (!found) return t("error.machine_removed");
  return typeof found.unavailable === "string" && found.unavailable.length > 0
    ? found.unavailable
    : null;
}

export function isViewRegistered(viewId: string): boolean {
  return views().some((v) => v.id === viewId);
}

export function creationMachines(): Array<string | null> {
  const list = machines();
  const ready: Array<string | null> = [];
  for (const item of list) {
    const m = item.machine ?? null;
    const proj = machineAutomationsOn(m);
    if (proj && isMachineReady(proj) && proj.store_error == null) {
      ready.push(m);
    }
  }
  return ready.length > 0 ? ready : [null];
}

export function automationMachineGroups(): Array<{
  machine: string | null;
  automations: any[];
  status: string | null;
}> {
  const list = machines();
  const keys: Array<string | null> = list.length > 0 ? list.map((m) => m.machine ?? null) : [null];
  return keys.map((machine) => {
    const proj = machineAutomationsOn(machine);
    const unavailable = machineUnavailable(machine);
    let status: string | null = unavailable;
    if (!status && proj?.store_error) {
      status = t("error.store_unavailable", { error: proj.store_error });
    } else if (!status && (!proj || !isMachineReady(proj))) {
      status = t("catalog.loading");
    }
    return {
      machine,
      automations: proj?.automations ?? [],
      status,
    };
  });
}

export function findRunOn(machine: string | null, runId: string, automationId?: string): any | null {
  const proj = machineAutomationsOn(machine);
  if (!proj || !Array.isArray(proj.runs)) return null;
  return (
    proj.runs.find(
      (r: any) => r.id === runId && (automationId == null || r.automation_id === automationId),
    ) ?? null
  );
}

/** 判定当前选中的运行是否可以挂载对话现场。 */
export function liveRunStatusFor(machine: string | null, runObj: any): LiveRunStatus {
  const sessionId = typeof runObj?.session_id === "string" && runObj.session_id.length > 0
    ? runObj.session_id
    : null;
  if (!sessionId) return { kind: "archived" };
  const states = sessionStates(machine);
  const live = Array.isArray(states) && states.some((s: any) => s?.id === sessionId);
  if (!live) return { kind: "archived" };
  const engine = runObj?.context?.engine_kind_id;
  if (typeof engine !== "string" || !allConversationAgentIds().has(engine)) {
    return { kind: "unknown_engine" };
  }
  if (!isViewRegistered(CONVERSATION_VIEW)) {
    return { kind: "no_conversation_view" };
  }
  return {
    kind: "live",
    reuseKey: `${machineKey(machine)}:${sessionId}`,
    args: {
      machine: machine ?? null,
      session_id: sessionId,
      spec: {
        agent: engine,
        cwd: String(runObj?.context?.cwd ?? ""),
        binding: {
          type: "automation",
          run_id: String(runObj.id),
        },
      },
      placeholder: t("run.opening"),
    },
  };
}

export function editorMissingFields(ed: EditorState) {
  const agentExists =
    ed.agentId.length > 0 && agentDefinitionOn(ed.machine, ed.agentId) != null;
  return computeDraftMissingFields(ed, agentExists, t, newSessionId);
}

// ---- 徽标与通知管理 ----

function refreshBadge() {
  const failing = countFailingAutomations(automationsByMachine);
  const label = failing > 0 ? String(failing) : null;
  if (label === lastBadgeLabel) return;
  lastBadgeLabel = label;
  void run(SET_VIEW_BADGE_CMD, { view: VIEW, label }).catch((err) => {
    console.error("[automations] set-view-badge 失败：", errMsg(err));
  });
}

function deliverNotification(machine: string | null, notification: any) {
  const key = deliveredKey(machine, notification.run_id);
  const payload = {
    id: notificationId(machine, notification.run_id),
    subtitle: notification.subtitle,
    body: notification.body,
    on_click: {
      command: OPEN_VIEW_CMD,
      args: {
        view: VIEW,
        args: {
          machine: machine ?? null,
          automation: notification.automation_id,
          run: notification.run_id,
        },
      },
    },
  };
  void run(NOTIFY_CMD, payload).catch((err) => {
    console.error("[automations] 发通知失败：", errMsg(err));
  });
  if (!deliveredSet.has(key)) {
    deliveredSet.add(key);
    void deliveredStore.add(machine, notification.run_id);
  }
}

function withdrawNotification(machine: string | null, runId: string) {
  const key = deliveredKey(machine, runId);
  if (!deliveredSet.delete(key)) return;
  void deliveredStore.remove(machine, runId);
  void run(WITHDRAW_NOTIFICATION_CMD, { id: notificationId(machine, runId) }).catch((err) => {
    console.error("[automations] 撤通知失败：", errMsg(err));
  });
}

export function notifyOnce(body: string) {
  void run(NOTIFY_CMD, {
    id: null,
    subtitle: t("notify.done"),
    body,
    on_click: null,
  }).catch((err) => {
    console.error("[automations] 发通知失败：", errMsg(err));
  });
}

export function openAgentsView(machine: string | null) {
  void run(OPEN_VIEW_CMD, {
    view: AGENTS_VIEW,
    args: { machine: machine ?? null },
  }).catch((err) => {
    console.error("[automations] 打开智能体页失败：", errMsg(err));
  });
}

export function openAutomationsView(args: Record<string, unknown>) {
  void run(OPEN_VIEW_CMD, { view: VIEW, args }).catch((err) => {
    console.error("[automations] 打开自动化页失败：", errMsg(err));
  });
}

// ---- `smelt.agents` 智能体配置只读缓存 ----

function syncAgentSettingsRegistry() {
  const registered = command(AGENT_SETTINGS_CMD) != null;
  if (registered && agentFetchState === "idle") {
    agentFetchState = "loading";
    agentTouched.clear();
    void run(AGENT_SETTINGS_CMD, null).then(
      (list: any) => {
        if (Array.isArray(list)) {
          for (const entry of list) {
            const key = machineKey(entry?.machine);
            if (agentTouched.has(key)) continue;
            if (entry?.state === "ready" && Array.isArray(entry?.settings?.agents)) {
              agentsByMachine.set(key, entry.settings.agents);
            }
          }
        }
        agentTouched.clear();
        agentFetchState = "done";
        bumpAgents();
      },
      (err: unknown) => {
        console.error("[automations] 取智能体配置失败：", errMsg(err));
        agentFetchState = "idle";
      },
    );
  } else if (!registered && agentFetchState === "done") {
    agentFetchState = "idle";
    agentTouched.clear();
    agentsByMachine.clear();
    bumpAgents();
  }
}

function applyAgentSettingsEvent(payload: any) {
  if (!payload || typeof payload !== "object") return;
  const key = machineKey(payload.machine);
  if (agentFetchState === "loading") {
    agentTouched.add(key);
  }
  if (payload.state === "ready" && Array.isArray(payload.settings?.agents)) {
    agentsByMachine.set(key, payload.settings.agents);
  } else {
    agentsByMachine.delete(key);
  }
  bumpAgents();
}

// ---- 话题推送与机器列表对账 ----

function onAutomationsTopic(machine: string | null, message: any) {
  if (!message || typeof message !== "object") return;
  let origin: "initial_snapshot" | "incremental";
  let payload: unknown;
  if (message.kind === "snapshot") {
    origin = "initial_snapshot";
    payload = message.payload;
  } else if (message.kind === "event") {
    origin = "incremental";
    payload = message.payload;
  } else if (message.kind === "lagged") {
    return;
  } else if (message.kind === "unavailable") {
    console.warn(`[automations] ${machine ?? "local"} 不推自动化：${message.reason ?? ""}`);
    return;
  } else {
    return;
  }

  const decoded = decodeAutomationsPayload(payload);
  if (!decoded.ok) {
    console.error(`[automations] ${machine ?? "local"} 推来的自动化认不出：${decoded.error}`);
    return;
  }
  if (!decoded.value) return;

  const remoteName = machine != null ? machineName(machine) : null;
  const outcome = applyAutomationsSnapshot(
    automationsByMachine,
    machine,
    decoded.value,
    origin,
    remoteName,
    t,
  );
  if (!outcome) return;

  // 撤掉快照里已不存在的运行通知
  const targetKey = machineKey(machine);
  for (const dKey of [...deliveredSet]) {
    const parsed = parseDeliveredKey(dKey);
    if (machineKey(parsed.machine) === targetKey && !outcome.runs.has(parsed.runId)) {
      withdrawNotification(parsed.machine, parsed.runId);
    }
  }
  for (const runId of outcome.silenced) {
    withdrawNotification(machine, runId);
  }
  for (const note of outcome.notify) {
    deliverNotification(machine, note);
  }

  bumpStore();
  refreshBadge();
  reconcile();
}

function onMachinesChanged() {
  const list = machines();
  if (list.length === 0 && state().revision === 0) return;
  const knownKeys = new Set(list.map((m) => machineKey(m.machine)));

  let removed = false;
  for (const key of [...automationsByMachine.keys()]) {
    if (!knownKeys.has(key)) {
      automationsByMachine.delete(key);
      removed = true;
    }
  }
  for (const dKey of [...deliveredSet]) {
    const parsed = parseDeliveredKey(dKey);
    if (!knownKeys.has(machineKey(parsed.machine))) {
      withdrawNotification(parsed.machine, parsed.runId);
    }
  }
  if (removed) {
    bumpStore();
  }
  refreshBadge();
  reconcile();
}

/**
 * 投影、机器、视图或会话状态变化后的统一对账（Bug Fix #6：删除自动化后不再保留孤儿运行页）。
 */
export function reconcile() {
  const r = route();
  if (!r) return;

  const knownMachines = machines();
  if (
    state().revision > 0 &&
    knownMachines.length > 0 &&
    !knownMachines.some((m) => (m.machine ?? null) === r.machine)
  ) {
    closeEditor();
    return;
  }

  const proj = machineAutomationsOn(r.machine);
  const storeReady = isMachineReady(proj);
  const automation = findAutomation(proj, r.id);
  const currentEditor = editor();
  const isNewDraft = Boolean(
    currentEditor && currentEditor.isNew && sameAutomationRef(currentEditor, r),
  );

  // Bug Fix #6: 删除后立即退回目录，不留孤儿状态。
  if (storeReady && !automation && !isNewDraft) {
    closeEditor();
    return;
  }

  const selRunId = selectedRunId();
  const selectedRun = selRunId ? findRunOn(r.machine, selRunId, r.id) : null;
  if (storeReady && selRunId && !selectedRun) {
    selectedRunId.set(null);
    runDetail.set(null);
    runDetailLoading.set(null);
    runDetailError.set(null);
  }

  if (automation && (!currentEditor || !sameAutomationRef(currentEditor, r))) {
    openEditorForAutomation(r.machine, automation, false);
  }

  if (selectedRun) {
    const liveStatus = liveRunStatusFor(r.machine, selectedRun);
    if (liveStatus.kind !== "live") {
      syncRunDetail(r.machine, selectedRun, false);
    }
  }
}

// ---- 运行详情异步加载（Bug Fix #8：错误显式呈现 + 支持重试） ----

export function syncRunDetail(machine: string | null, runObj: any, force = false) {
  const key = makeRunDetailKey(machine, runObj);
  if (!force) {
    if (sameRunDetailKey(runDetail()?.key, key)) return;
    if (sameRunDetailKey(runDetailLoading(), key)) return;
    if (sameRunDetailKey(runDetailError()?.key, key)) return;
  }
  runDetailError.set(null);
  runDetailLoading.set(key);

  const needArchive =
    runObj?.output == null || runObj?.context?.agent_instructions == null;

  void call(machine, "automation.run_detail", {
    run_id: runObj.id,
    include_archived_run: needArchive,
    entry_version: ACP_ENTRY_VERSION,
  }).then(
    (res: any) => {
      if (!sameRunDetailKey(runDetailLoading(), key)) return;
      runDetailLoading.set(null);
      const prev = runDetail();
      const keepExpanded =
        prev &&
        machineKey(prev.key.machine) === machineKey(key.machine) &&
        prev.key.runId === key.runId
          ? prev.expanded
          : new Set<string>();
      runDetail.set(
        buildRunDetail(
          key,
          runObj,
          res?.archived_run ?? null,
          Array.isArray(res?.transcript) ? res.transcript : [],
          keepExpanded,
        ),
      );
      runDetailError.set(null);
    },
    (err: unknown) => {
      if (!sameRunDetailKey(runDetailLoading(), key)) return;
      runDetailLoading.set(null);
      runDetailError.set({ key, message: errMsg(err) });
    },
  );
}

export function retryRunDetail() {
  const r = route();
  const runId = selectedRunId();
  if (!r || !runId) return;
  const runObj = findRunOn(r.machine, runId, r.id);
  if (!runObj) return;
  syncRunDetail(r.machine, runObj, true);
}

export function toggleRunDetailBlock(blockKey: string) {
  const cur = runDetail();
  if (!cur) return;
  const nextExpanded = new Set(cur.expanded);
  if (!nextExpanded.delete(blockKey)) {
    nextExpanded.add(blockKey);
  }
  runDetail.set({ ...cur, expanded: nextExpanded });
}

// ---- 守护写入命令 ----

export async function submitAutomationCommand(
  machine: string | null,
  commandPayload: Record<string, unknown>,
) {
  return request(machine, {
    op: "automation_command",
    command: commandPayload,
  });
}

export function runAutomationOnceFromCatalog(target: AutomationRef) {
  pageError.set(null);
  void submitAutomationCommand(target.machine, {
    command: "run_once",
    automation_id: target.id,
  }).catch((err) => {
    pageError.set(errMsg(err));
  });
}

export function cancelAutomationRun(machine: string | null, runId: string) {
  pageError.set(null);
  void submitAutomationCommand(machine, {
    command: "cancel_run",
    run_id: runId,
  }).catch((err) => {
    pageError.set(errMsg(err));
  });
}

export function setAutomationEnabled(target: AutomationRef, enabled: boolean) {
  pageError.set(null);
  void submitAutomationCommand(target.machine, {
    command: "set_enabled",
    automation_id: target.id,
    enabled,
  }).catch((err) => {
    pageError.set(errMsg(err));
  });
}

export function startDeleteAutomation(target: AutomationRef) {
  const proj = machineAutomationsOn(target.machine);
  const auto = findAutomation(proj, target.id);
  if (!auto) return;
  const name = auto.name?.trim() ? auto.name : t("automation.unnamed");
  const runCount = runsForAutomation(proj, target.id).length;
  deletePrompt.set({ target, name, runCount });
}

export function confirmDeleteAutomation() {
  const req = deletePrompt();
  if (!req) return;
  deletePrompt.set(null);
  const { target } = req;
  if (editor() && sameAutomationRef(editor(), target)) {
    clearAutosaveTimer();
    clearFlashTimer();
    editor.set(null);
  }
  if (route() && sameAutomationRef(route(), target)) {
    route.set(null);
    selectedRunId.set(null);
    runDetail.set(null);
    runDetailLoading.set(null);
    runDetailError.set(null);
  }
  pageError.set(null);
  void submitAutomationCommand(target.machine, {
    command: "delete",
    automation_id: target.id,
  }).catch((err) => {
    pageError.set(errMsg(err));
  });
}

// ---- 导航与编辑器生命周期 ----

export function openEditorForAutomation(
  machine: string | null,
  automation: any,
  isNew: boolean,
  placeholders: string[] = [],
) {
  clearAutosaveTimer();
  clearFlashTimer();
  const target: AutomationRef = { machine: machine ?? null, id: automation.id };
  route.set(target);

  const sel = selectedRunId();
  if (sel && !findRunOn(target.machine, sel, target.id)) {
    selectedRunId.set(null);
    runDetail.set(null);
    runDetailLoading.set(null);
    runDetailError.set(null);
  }

  const draft = buildDraftFromAutomation(
    automation,
    target.machine,
    isNew,
    nextTriggerUid,
    newSessionId,
  );

  editor.set({
    instanceId: newSessionId(),
    ...draft,
    placeholders,
    draftRevision: 0,
    savedRevision: 0,
    savingRevision: null,
    savingAction: null,
    regeneratingUid: null,
    runningOnce: false,
    error: null,
    flash: null,
    autoFocusName: isNew && draft.name.trim().length === 0,
  });
  pageError.set(null);
}

export function openAutomationEditor(target: AutomationRef) {
  const cur = editor();
  if (cur && sameAutomationRef(cur, target)) {
    route.set(target);
    return;
  }
  const proj = machineAutomationsOn(target.machine);
  const auto = findAutomation(proj, target.id);
  if (!auto) {
    editor.set(null);
    return;
  }
  openEditorForAutomation(target.machine, auto, false);
}

export function closeEditor() {
  clearAutosaveTimer();
  clearFlashTimer();
  editor.set(null);
  selectedRunId.set(null);
  runDetail.set(null);
  runDetailLoading.set(null);
  runDetailError.set(null);
  route.set(null);
  pageError.set(null);
}

export function selectAutomationRun(machine: string | null, runId: string) {
  const runObj = findRunOn(machine, runId);
  if (!runObj) return;
  selectedRunId.set(runObj.id);
  pageError.set(null);
  const liveStatus = liveRunStatusFor(machine, runObj);
  if (liveStatus.kind !== "live") {
    syncRunDetail(machine, runObj, false);
  }
}

export function showAutomationTask() {
  selectedRunId.set(null);
  pageError.set(null);
}

export function navigateToAutomationRun(target: AutomationRef, runId: string) {
  route.set(target);
  const proj = machineAutomationsOn(target.machine);
  const auto = findAutomation(proj, target.id);
  if (auto) {
    const cur = editor();
    if (!cur || !sameAutomationRef(cur, target)) {
      openEditorForAutomation(target.machine, auto, false);
    }
  }
  selectAutomationRun(target.machine, runId);
}

export function openCatalogRun(machine: string | null, runId: string) {
  const runObj = findRunOn(machine, runId);
  if (!runObj) return;
  navigateToAutomationRun({ machine: machine ?? null, id: runObj.automation_id }, runId);
}

export function addAutomation(machine: string | null) {
  addAutomationForAgent(machine, readyAgentIdOn(machine));
}

export function addAutomationForAgent(machine: string | null, agentId: string) {
  catalogTab.set("tasks");
  const action =
    agentId.trim().length === 0
      ? { type: "shell", command: "", args: [] }
      : { type: "agent", agent_definition_id: agentId };
  const automation = {
    id: newSessionId(),
    name: "",
    enabled: true,
    workspace_dir: null,
    trigger: { schedules: [], webhooks: [], events: [] },
    action,
    sinks: notifyToSinks("always"),
  };
  openEditorForAutomation(machine, automation, true);
}

export function addAutomationFromTemplate(machine: string | null, templateId: string) {
  const tpl = automationTemplate(templateId);
  if (!tpl) return;
  catalogTab.set("tasks");
  const prompt = t(tpl.promptKey);
  const automation = {
    id: newSessionId(),
    name: t(tpl.nameKey),
    enabled: true,
    workspace_dir: null,
    trigger: { schedules: [tpl.schedule], webhooks: [], events: [] },
    action: {
      type: "agent",
      agent_definition_id: readyAgentIdOn(machine),
      prompt,
    },
    sinks: notifyToSinks("always"),
  };
  openEditorForAutomation(machine, automation, true, templatePlaceholders(prompt));
}

// ---- 编辑器字段更新与防抖自动保存 ----

function patchEditor(instanceId: string, updater: (cur: EditorState) => EditorState) {
  const cur = editor();
  if (!cur || cur.instanceId !== instanceId) return;
  editor.set(updater(cur));
}

function mutateEditorDraft(updater: (cur: EditorState) => EditorState) {
  const cur = editor();
  if (!cur) return;
  const nextRevision = cur.draftRevision + 1;
  const updated: EditorState = {
    ...updater(cur),
    draftRevision: nextRevision,
    error: null,
  };
  editor.set(updated);

  if (updated.isNew) return;
  clearAutosaveTimer();
  const instanceId = updated.instanceId;
  // 防抖输入保存（非轮询）：停止输入 400ms 后保存一次，关闭编辑器或点「运行一次」前立即取消/冲刷。
  autosaveTimer = setTimeout(() => {
    autosaveTimer = null;
    const latest = editor();
    if (!latest || latest.instanceId !== instanceId || latest.draftRevision !== nextRevision) {
      return;
    }
    void persistEditor({ action: "autosave" });
  }, 400);
}

export function setEditorName(name: string) {
  mutateEditorDraft((cur) => ({ ...cur, name }));
}

export function setEditorPrompt(prompt: string) {
  mutateEditorDraft((cur) => ({ ...cur, prompt }));
}

export function setEditorCommand(commandText: string) {
  mutateEditorDraft((cur) => ({ ...cur, command: commandText }));
}

export function setEditorActionKind(kind: "agent" | "shell") {
  const cur = editor();
  if (!cur) return;
  const fallbackAgent = kind === "agent" && !cur.agentId ? readyAgentIdOn(cur.machine) : cur.agentId;
  mutateEditorDraft((ed) => ({
    ...ed,
    actionKind: kind,
    agentId: fallbackAgent,
  }));
}

export function setEditorAgent(agentId: string) {
  mutateEditorDraft((cur) => ({ ...cur, agentId }));
}

export function setEditorNotification(notification: "never" | "failures_only" | "always") {
  mutateEditorDraft((cur) => ({ ...cur, notification }));
}

export function setEditorWorkspace(workspaceDir: string | null) {
  mutateEditorDraft((cur) => ({ ...cur, workspaceDir }));
}

export function pickEditorWorkspace() {
  const cur = editor();
  if (!cur) return;
  const instanceId = cur.instanceId;
  void run(PICK_PATHS_CMD, {
    machine: cur.machine,
    title: t("workspace.pick_title"),
    confirm: t("workspace.pick_confirm"),
    files: false,
    multiple: false,
  }).then(
    (paths: any) => {
      const dir = Array.isArray(paths) && typeof paths[0] === "string" ? paths[0] : null;
      if (!dir) return;
      if (editor()?.instanceId !== instanceId) return;
      setEditorWorkspace(dir);
    },
    (err: unknown) => {
      patchEditor(instanceId, (ed) => ({ ...ed, error: errMsg(err) }));
    },
  );
}

export function addEditorTriggerPreset(preset: string) {
  const schedule = triggerPresetSchedule(preset);
  const kind = triggerPresetKind(preset);
  const entry = createTriggerEntryDraft(
    nextTriggerUid(),
    kind,
    schedule,
    null,
    newSessionId,
  ) as TriggerEntryDraft;
  mutateEditorDraft((cur) => ({
    ...cur,
    entries: [...cur.entries, entry],
  }));
}

export function removeEditorTriggerEntry(uid: string) {
  mutateEditorDraft((cur) => ({
    ...cur,
    entries: cur.entries.filter((e) => e.uid !== uid),
  }));
}

function updateScheduleEntry(
  uid: string,
  updater: (rule: ScheduleRuleDraft) => ScheduleRuleDraft,
) {
  mutateEditorDraft((cur) => ({
    ...cur,
    entries: cur.entries.map((entry) => {
      if (entry.uid !== uid || entry.triggerKind === "webhook") return entry;
      return { ...entry, schedule: updater(entry.schedule) };
    }),
  }));
}

export function setTriggerScheduleKind(
  uid: string,
  kind: "daily" | "weekdays" | "weekly" | "interval",
) {
  updateScheduleEntry(uid, (rule) => {
    let weekdayMask = rule.weekdayMask;
    let intervalDayScope = rule.intervalDayScope;
    if (kind === "weekdays") {
      weekdayMask = SCHEDULE_WEEKDAYS_MASK;
      intervalDayScope = "weekdays";
    } else if (kind === "weekly") {
      if (weekdayMask === 0) weekdayMask = SCHEDULE_WEEKDAYS_MASK;
      intervalDayScope = "weekly";
    } else if (kind === "daily") {
      weekdayMask = SCHEDULE_ALL_DAYS_MASK;
      intervalDayScope = "everyday";
    } else {
      intervalDayScope = intervalDayScopeFromDays(weekdayMask);
    }
    return { ...rule, kind, weekdayMask, intervalDayScope };
  });
}

export function setTriggerTime(uid: string, time: string) {
  updateScheduleEntry(uid, (rule) => ({ ...rule, time }));
}

export function setTriggerInterval(uid: string, interval: string) {
  updateScheduleEntry(uid, (rule) => ({ ...rule, interval }));
}

export function setTriggerIntervalUnit(uid: string, intervalUnit: "minutes" | "hours") {
  updateScheduleEntry(uid, (rule) => ({ ...rule, intervalUnit }));
}

export function setTriggerIntervalDayScope(
  uid: string,
  scope: "everyday" | "weekdays" | "weekly",
) {
  updateScheduleEntry(uid, (rule) => {
    let weekdayMask = rule.weekdayMask;
    if (scope === "everyday") weekdayMask = SCHEDULE_ALL_DAYS_MASK;
    else if (scope === "weekdays") weekdayMask = SCHEDULE_WEEKDAYS_MASK;
    else if (weekdayMask === 0) weekdayMask = SCHEDULE_WEEKDAYS_MASK;
    return { ...rule, intervalDayScope: scope, weekdayMask };
  });
}

export function setTriggerWindowOpen(uid: string, open: boolean) {
  updateScheduleEntry(uid, (rule) => ({
    ...rule,
    windowOpen: open,
    windowStart: open ? rule.windowStart : "",
    windowEnd: open ? rule.windowEnd : "",
  }));
}

export function setTriggerWindowStart(uid: string, windowStart: string) {
  updateScheduleEntry(uid, (rule) => ({ ...rule, windowStart }));
}

export function setTriggerWindowEnd(uid: string, windowEnd: string) {
  updateScheduleEntry(uid, (rule) => ({ ...rule, windowEnd }));
}

export function toggleTriggerWeekday(uid: string, dayBit: number) {
  updateScheduleEntry(uid, (rule) => ({
    ...rule,
    weekdayMask: rule.weekdayMask ^ dayBit,
  }));
}

export function flashEditorButton(id: string, kind: "copied" | "regenerated") {
  const cur = editor();
  if (!cur) return;
  const instanceId = cur.instanceId;
  clearFlashTimer();
  patchEditor(instanceId, (ed) => ({ ...ed, flash: { id, kind } }));
  flashTimer = setTimeout(() => {
    flashTimer = null;
    patchEditor(instanceId, (ed) =>
      ed.flash?.id === id ? { ...ed, flash: null } : ed,
    );
  }, 2000);
}

export function copyWithFlash(buttonId: string, content: string) {
  void clipboardWrite(content).then(
    () => flashEditorButton(buttonId, "copied"),
    (err) => {
      pageError.set(errMsg(err));
    },
  );
}

export function regenerateWebhookSecret(uid: string) {
  const cur = editor();
  if (!cur) return;
  const nextSecret = newWebhookSecret(newSessionId);
  const flashId = `automation-regen-webhook-${uid}`;
  if (cur.isNew) {
    mutateEditorDraft((ed) => ({
      ...ed,
      entries: ed.entries.map((entry) =>
        entry.uid === uid && entry.triggerKind === "webhook"
          ? { ...entry, webhookSecret: nextSecret }
          : entry,
      ),
    }));
    return;
  }
  clearAutosaveTimer();
  patchEditor(cur.instanceId, (ed) => ({
    ...ed,
    draftRevision: ed.draftRevision + 1,
    error: null,
    entries: ed.entries.map((entry) =>
      entry.uid === uid && entry.triggerKind === "webhook"
        ? { ...entry, webhookSecret: nextSecret }
        : entry,
    ),
  }));
  void persistEditor({
    action: "regen",
    regeneratingUid: uid,
    flash: { id: flashId, kind: "regenerated" },
  });
}

export function createAutomationFromEditor() {
  const cur = editor();
  if (!cur || !cur.isNew) return;
  void persistEditor({ action: "create" });
}

interface PersistOptions {
  action: "create" | "autosave" | "regen" | "flush";
  regeneratingUid?: string;
  flash?: { id: string; kind: "copied" | "regenerated" };
}

/**
 * 持久化编辑器草稿到 smeltd。返回 `true` 表示最新草稿已落盘，供 `runOnceFromEditor` 等待冲刷。
 */
export async function persistEditor(opts: PersistOptions): Promise<boolean> {
  if (inflightSavePromise) {
    await inflightSavePromise;
  }
  const ed = editor();
  if (!ed) return false;
  if (ed.savingRevision !== null) return false;
  if (editorMissingFields(ed).length > 0) return false;

  const target: AutomationRef = { machine: ed.machine, id: ed.id };
  const proj = machineAutomationsOn(target.machine);
  const stored = findAutomation(proj, target.id);
  const enabled = stored ? stored.enabled !== false : true;

  const built = buildAutomationWireFromDraft(ed, enabled, t, newSessionId);
  if (!built.ok) return false;

  const createdName = ed.isNew ? built.automation.name : null;
  const instanceId = ed.instanceId;
  const draftRevision = ed.draftRevision;

  patchEditor(instanceId, (cur) => ({
    ...cur,
    savingRevision: draftRevision,
    savingAction: opts.action,
    regeneratingUid: opts.regeneratingUid ?? null,
    error: null,
  }));
  pageError.set(null);

  const task = (async (): Promise<boolean> => {
    try {
      const response = await submitAutomationCommand(target.machine, {
        command: "upsert",
        automation: built.automation,
      });
      const snapshot = normalizeAutomationFile(response?.automations);
      const currentStore = snapshot
        ? snapshotIsCurrentStore(machineAutomationsOn(target.machine), snapshot)
        : true;

      let shouldRetry = false;
      let ok = false;
      patchEditor(instanceId, (cur) => {
        if (!currentStore) {
          return {
            ...cur,
            savingRevision: null,
            savingAction: null,
            regeneratingUid: null,
            error: t("error.service_switched"),
          };
        }
        ok = true;
        shouldRetry = cur.draftRevision !== draftRevision;
        return {
          ...cur,
          isNew: false,
          savedRevision: draftRevision,
          savingRevision: null,
          savingAction: null,
          regeneratingUid: null,
        };
      });

      if (ok && createdName != null) {
        notifyOnce(t("notify.created", { name: createdName }));
      }
      if (ok && opts.flash) {
        flashEditorButton(opts.flash.id, opts.flash.kind);
      }
      if (ok && shouldRetry) {
        return persistEditor({ action: opts.action === "flush" ? "flush" : "autosave" });
      }
      return ok;
    } catch (err) {
      patchEditor(instanceId, (cur) => ({
        ...cur,
        savingRevision: null,
        savingAction: null,
        regeneratingUid: null,
        error: errMsg(err),
      }));
      return false;
    } finally {
      if (inflightSavePromise === task) {
        inflightSavePromise = null;
      }
    }
  })();

  inflightSavePromise = task;
  return task;
}

/**
 * Bug Fix #7：点击「运行一次」前先取消防抖定时器并冲刷所有未保存草稿，
 * 确保 smeltd 拿到的是用户最新编辑的指令/命令/工作目录后再触发执行。
 */
export async function runOnceFromEditor() {
  const ed = editor();
  if (!ed || ed.isNew || ed.runningOnce) return;
  const target: AutomationRef = { machine: ed.machine, id: ed.id };
  const instanceId = ed.instanceId;

  clearAutosaveTimer();
  patchEditor(instanceId, (cur) => ({ ...cur, runningOnce: true, error: null }));
  pageError.set(null);

  try {
    if (inflightSavePromise) {
      await inflightSavePromise;
    }
    const latest = editor();
    if (!latest || latest.instanceId !== instanceId) return;

    if (latest.draftRevision !== latest.savedRevision) {
      const missing = editorMissingFields(latest);
      if (missing.length > 0) {
        patchEditor(instanceId, (cur) => ({
          ...cur,
          runningOnce: false,
          error: automationSaveHint(missing, false, t),
        }));
        return;
      }
      const saved = await persistEditor({ action: "flush" });
      if (!saved) {
        patchEditor(instanceId, (cur) => ({ ...cur, runningOnce: false }));
        return;
      }
    }

    const ready = editor();
    if (!ready || ready.instanceId !== instanceId) return;
    await submitAutomationCommand(target.machine, {
      command: "run_once",
      automation_id: target.id,
    });
  } catch (err) {
    pageError.set(errMsg(err));
  } finally {
    patchEditor(instanceId, (cur) => ({ ...cur, runningOnce: false }));
  }
}

// ---- 打开参数处理与宿主事件绑定 ----

function handleOpenArgs(args: any) {
  if (!args || typeof args !== "object") return;
  let machine: string | null;
  if (args.machine === undefined || args.machine === null) {
    machine = null;
  } else if (typeof args.machine === "string") {
    machine = args.machine;
  } else {
    return;
  }

  if (typeof args.create_for_agent === "string") {
    addAutomationForAgent(machine, args.create_for_agent);
    return;
  }
  if (typeof args.automation === "string") {
    const target: AutomationRef = { machine, id: args.automation };
    if (typeof args.run === "string") {
      navigateToAutomationRun(target, args.run);
    } else {
      selectedRunId.set(null);
      runDetail.set(null);
      runDetailLoading.set(null);
      runDetailError.set(null);
      route.set(target);
      openAutomationEditor(target);
    }
    reconcile();
  }
}

export function bootAutomations() {
  if (booted) return;
  booted = true;

  onState(() => {
    syncAgentSettingsRegistry();
    reconcile();
  });

  onEvent((event: any) => {
    if (!event || typeof event !== "object") return;
    if (event.type === "runtime_topic" && event.topic === TOPIC_AUTOMATIONS_CHANGED) {
      onAutomationsTopic(event.machine ?? null, event.message);
      return;
    }
    if (event.type === "plugin" && event.name === AGENT_SETTINGS_CHANGED_EVENT) {
      applyAgentSettingsEvent(event.payload);
      return;
    }
    if (event.type === "commands_changed") {
      syncAgentSettingsRegistry();
      return;
    }
    if (event.type === "machines_changed") {
      onMachinesChanged();
      return;
    }
    if (event.type === "views_changed" || event.type === "sessions_changed") {
      reconcile();
      return;
    }
    if (event.type === "view_opened" && event.view === VIEW) {
      handleOpenArgs(event.args);
    }
  });

  bootTask(async () => {
    await initHost();
    await deliveredStore.open();
    const loaded = await deliveredStore.delivered();
    for (const item of loaded) deliveredSet.add(item);

    await registerTopics([TOPIC_AUTOMATIONS_CHANGED]);
    await registerViews([
      {
        name: VIEW_NAME,
        export: "main",
        title: text("title"),
        icon: "icons/workflow.svg",
        draws_top_bar: false,
        instances: false,
      },
    ]);
    syncAgentSettingsRegistry();
    refreshBadge();
  });
}

// 供子组件构造默认空机器投影
export { createEmptyMachineAutomations };
