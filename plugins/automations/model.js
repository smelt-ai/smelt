// 自动化插件纯领域模型、投影缓存、通知判定、日程换算、模板与运行详情解析。
// 不依赖 NSH 或宿主运行时，可直接由 `bun test` 验证。和运行时同一份的规则（日程、触发器校验）
// 不在这里写：由调用方经 `rules`（SDK `automationScheduleProblem` / `automationTriggerProblem`）传进来。

import { PLUGIN_ID } from "./contract.js";

// ---- 常量（与 `crates/smelt-model/src/automation.rs` 保持字节级一致） ----

export const AUTOMATION_FILE_SCHEMA_VERSION = 2;
export const DEFAULT_WEBHOOK_BASE_URL = "http://127.0.0.1:17827";
export const REDACTED_WEBHOOK_ENDPOINT = "<redacted>";
export const TOOL_OUTPUT_MAX_LINES = 400;
export const ACP_ENTRY_VERSION = 2;

/** 周一到周日位掩码（bit0 = 周一）。 */
export const SCHEDULE_DAY_MON = 1 << 0;
export const SCHEDULE_DAY_TUE = 1 << 1;
export const SCHEDULE_DAY_WED = 1 << 2;
export const SCHEDULE_DAY_THU = 1 << 3;
export const SCHEDULE_DAY_FRI = 1 << 4;
export const SCHEDULE_DAY_SAT = 1 << 5;
export const SCHEDULE_DAY_SUN = 1 << 6;
export const SCHEDULE_WEEKDAYS_MASK =
  SCHEDULE_DAY_MON |
  SCHEDULE_DAY_TUE |
  SCHEDULE_DAY_WED |
  SCHEDULE_DAY_THU |
  SCHEDULE_DAY_FRI;
export const SCHEDULE_ALL_DAYS_MASK =
  SCHEDULE_WEEKDAYS_MASK | SCHEDULE_DAY_SAT | SCHEDULE_DAY_SUN;

export const BUILTIN_AUTOMATION_ACTIONS = ["agent", "shell"];

export const BUILTIN_AUTOMATION_TRIGGER_PRESETS = [
  "hourly",
  "daily",
  "weekly",
  "interval",
  "webhook",
];

export const NOTIFY_CHOICES = ["never", "failures_only", "always"];

export const TEMPLATE_CATEGORIES = ["work", "engineering", "research"];

/** 运行状态分类。 */
const ACTIVE_RUN_STATUSES = new Set([
  "starting",
  "queued",
  "dispatching",
  "running",
  "awaiting_approval",
  "waiting_for_user",
]);

/** 插件 SQLite 库迁移：与原 Rust `crates/plugins/automations/src/store.rs` 保持 100% 同一表结构。 */
export const DELIVERED_STORE_MIGRATIONS = [
  `
    CREATE TABLE delivered (
        machine TEXT NOT NULL,
        run_id TEXT NOT NULL,
        PRIMARY KEY (machine, run_id)
    );
`,
];

// ---- Webhook 令牌与地址 ----

/**
 * 与 Rust `uuid::Uuid::new_v4().simple().to_string()` 一致：32 位小写十六进制无连字符串。
 * 运行时传入来自 `smelt/sdk` 的 `newSessionId`（Rust OS CSPRNG UUID v4）。
 */
export function newWebhookSecret(uuidFn) {
  if (typeof uuidFn !== "function") {
    throw new Error("newWebhookSecret requires a cryptographic UUID generator");
  }
  return String(uuidFn()).replace(/-/g, "").toLowerCase();
}

export function webhookUrl(base, token) {
  const cleanBase = String(base || DEFAULT_WEBHOOK_BASE_URL).replace(/\/+$/, "");
  return `${cleanBase}/hooks/${token}`;
}

export function webhookToken(webhook) {
  if (!webhook || typeof webhook !== "object") return null;
  const secret = typeof webhook.secret === "string" ? webhook.secret.trim() : "";
  if (secret.length > 0) return secret;
  const endpoint = typeof webhook.endpoint === "string" ? webhook.endpoint.trim() : "";
  return endpoint.length > 0 ? endpoint : null;
}

export function webhookCurlExample(url, t) {
  return `curl -sS -X POST ${url} -H 'Content-Type: application/json' -d '{"text":"${t("webhook.example_text")}"}'`;
}

// ---- 机器键与基础工具 ----

export function machineKey(machine) {
  return typeof machine === "string" && machine.length > 0 ? machine : "";
}

export function machineFromKey(key) {
  return key ? key : null;
}

export function runtimeElementKey(machine) {
  return typeof machine === "string" && machine.length > 0 ? machine : "local";
}

export function sameAutomationRef(a, b) {
  if (!a || !b) return false;
  return machineKey(a.machine) === machineKey(b.machine) && a.id === b.id;
}

export function stableEqual(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

/** 通知 id：`smelt.automations/local/<run>` 或 `smelt.automations/remote:<id>/<run>`。 */
export function notificationId(machine, runId) {
  const host = typeof machine === "string" && machine.length > 0 ? `remote:${machine}` : "local";
  return `${PLUGIN_ID}/${host}/${runId}`;
}

export function deliveredKey(machine, runId) {
  return `${machineKey(machine)}\0${runId}`;
}

export function parseDeliveredKey(key) {
  const sep = key.indexOf("\0");
  if (sep < 0) return { machine: null, runId: key };
  return {
    machine: machineFromKey(key.slice(0, sep)),
    runId: key.slice(sep + 1),
  };
}

// ---- 插件 SQLite 库：已发通知表 `delivered(machine, run_id)` ----

/**
 * 与原 Rust `crates/plugins/automations/src/store.rs` 等价的 SQLite 仓储。
 * `db` 为 `{ migrate, query, execute }`（运行时用 `smelt/sdk` 的 `store`，单测用 `bun:sqlite`）。
 */
export class DeliveredStore {
  constructor(db) {
    this.db = db;
    this.ready = false;
    this.queue = Promise.resolve();
  }

  async open() {
    try {
      await this.db.migrate(DELIVERED_STORE_MIGRATIONS);
      this.ready = true;
    } catch (error) {
      console.error(`[automations] 库打不开，发过的通知重启后撤不掉：${error?.message ?? error}`);
      this.ready = false;
    }
  }

  async delivered() {
    const out = new Set();
    if (!this.ready) return out;
    try {
      const rows = await this.db.query("SELECT machine, run_id FROM delivered");
      if (Array.isArray(rows)) {
        for (const row of rows) {
          if (row && typeof row.run_id === "string") {
            const machine = typeof row.machine === "string" && row.machine.length > 0 ? row.machine : null;
            out.add(deliveredKey(machine, row.run_id));
          }
        }
      }
    } catch (error) {
      console.error(`[automations] 读库失败：${error?.message ?? error}`);
    }
    return out;
  }

  add(machine, runId) {
    if (!this.ready) return Promise.resolve();
    const col = machineKey(machine);
    this.queue = this.queue
      .then(() =>
        this.db.execute(
          "INSERT OR IGNORE INTO delivered (machine, run_id) VALUES (?1, ?2)",
          [col, runId],
        ),
      )
      .catch((error) => {
        console.error(`[automations] 写库失败：${error?.message ?? error}`);
      });
    return this.queue;
  }

  remove(machine, runId) {
    if (!this.ready) return Promise.resolve();
    const col = machineKey(machine);
    this.queue = this.queue
      .then(() =>
        this.db.execute("DELETE FROM delivered WHERE machine = ?1 AND run_id = ?2", [col, runId]),
      )
      .catch((error) => {
        console.error(`[automations] 写库失败：${error?.message ?? error}`);
      });
    return this.queue;
  }
}

// ---- 动作、通知与运行状态 ----

export function actionAgentDefinitionId(action) {
  if (!action || typeof action !== "object") return null;
  if (action.type === "agent") {
    return typeof action.agent_definition_id === "string"
      ? action.agent_definition_id
      : typeof action.agent_id === "string"
        ? action.agent_id
        : "";
  }
  return null;
}

export function actionPrompt(action) {
  if (!action || typeof action !== "object") return null;
  if (action.type === "agent" && typeof action.prompt === "string") {
    return action.prompt;
  }
  return null;
}

export function isShellAction(action) {
  return Boolean(action && typeof action === "object" && action.type === "shell");
}

export function formatShellCommand(action) {
  if (!isShellAction(action)) return "";
  const cmd = typeof action.command === "string" ? action.command : "";
  const args = Array.isArray(action.args) ? action.args.filter((a) => typeof a === "string") : [];
  if (args.length === 0) return cmd;
  return [cmd, ...args].filter((part) => part.length > 0).join(" ");
}

export function isRunStatusActive(status) {
  return ACTIVE_RUN_STATUSES.has(status);
}

export function isRunStatusTerminal(status) {
  return !ACTIVE_RUN_STATUSES.has(status);
}

export function runBlocksOverlap(run) {
  if (!run) return false;
  if (isRunStatusActive(run.status)) return true;
  if (run.runtime_released_at != null) return false;
  return Boolean(
    run.session_id != null ||
      (run.status === "cancelled" && isShellAction(run.context?.action)),
  );
}

export function notifyFromSinks(sinks) {
  const list = Array.isArray(sinks) ? sinks : [];
  if (list.some((sink) => sink?.type === "off")) return "never";
  const locals = list.filter((sink) => sink?.type === "local");
  if (locals.length === 0) {
    return list.length === 0 ? "always" : "never";
  }
  if (locals.every((sink) => Boolean(sink.failures_only))) {
    return "failures_only";
  }
  return "always";
}

export function notifyToSinks(notify) {
  if (notify === "never") return [{ type: "off" }];
  if (notify === "failures_only") return [{ type: "local", failures_only: true }];
  return [{ type: "local" }];
}

export function notifyShouldFire(notify, failed) {
  if (notify === "never") return false;
  if (notify === "failures_only") return Boolean(failed);
  return true;
}

export function notifyLabel(notify, t) {
  if (notify === "never") return t("notification.off");
  if (notify === "failures_only") return t("notification.failures");
  return t("notification.always");
}

export function notifyHint(notify, t) {
  if (notify === "never") return t("notification.off_hint");
  if (notify === "failures_only") return t("notification.failures_hint");
  return t("notification.always_hint");
}

// ---- 运行失败原因与问题码文案 ----

const RUN_FAILURE_KEY_MAP = {
  previous_run_active: "automation_run.previous_run_active",
  nothing_to_send: "automation.nothing_to_send",
  workspace_not_ready: "automation.workspace_not_ready",
  workspace_missing: "automation_run.workspace_missing",
  automation_invalid: "automation_run.automation_invalid",
  agent_unavailable: "automation_run.agent_unavailable",
  session_missing: "automation_run.session_missing",
  session_lost_on_restart: "automation_run.session_lost_on_restart",
  session_conflict: "automation_run.session_conflict",
  restart_during_delivery: "automation_run.restart_during_delivery",
  context_not_saved: "automation_run.context_not_saved",
  engine_changed: "automation_run.engine_changed",
  start_failed: "automation_run.start_failed",
  delivery_failed: "automation_run.delivery_failed",
  exited_before_delivery: "automation_run.exited_before_delivery",
  session_lost: "automation_run.session_lost",
  agent_exited: "automation_run.agent_exited",
  turn_failed: "automation_run.turn_failed",
  turn_interrupted: "turn_failure.interrupted",
  turn_max_tokens: "turn_failure.max_tokens",
  turn_max_turn_requests: "turn_failure.max_turn_requests",
  turn_refused: "turn_failure.refused",
  turn_without_outcome: "automation_run.turn_without_outcome",
  not_shell_action: "automation_run.not_shell_action",
  command_start_failed: "automation_run.command_start_failed",
  command_wait_failed: "automation_run.command_wait_failed",
  command_failed: "automation_run.command_failed",
};

/** 与 Rust `AutomationRunFailure::describe` 完全一致。 */
export function describeAutomationRunFailure(code, detail, t) {
  const cleanDetail = typeof detail === "string" ? detail.trim() : "";
  const key = typeof code === "string" ? RUN_FAILURE_KEY_MAP[code] : null;
  const reason = key ? t(key) : null;
  if (reason && cleanDetail) {
    return t("automation_run.with_detail", { reason, detail: cleanDetail });
  }
  if (reason) return reason;
  if (cleanDetail) return cleanDetail;
  return t("automation_run.other");
}

/** 与 Rust `AutomationRun::error_text` 完全一致。 */
export function runErrorText(run, t) {
  if (!run) return null;
  if (run.error_code != null) {
    return describeAutomationRunFailure(run.error_code, run.error ?? null, t);
  }
  return typeof run.error === "string" && run.error.length > 0 ? run.error : null;
}

const AUTOMATION_PROBLEM_KEYS = {
  no_agent: "automation.no_agent",
  no_shell_command: "automation.no_shell_command",
  no_webhook_token: "automation.no_webhook_token",
  no_trigger: "automation.no_trigger",
  no_id: "automation.no_id",
  no_name: "automation.no_name",
  workspace_not_ready: "automation.workspace_not_ready",
  workspace_missing: "automation.workspace_missing",
  no_instruction: "automation.no_instruction",
  no_days: "automation.no_days",
  time_out_of_range: "automation.time_out_of_range",
  minutes_out_of_range: "automation.minutes_out_of_range",
  hours_out_of_range: "automation.hours_out_of_range",
  end_before_start: "automation.end_before_start",
  nothing_to_send: "automation.nothing_to_send",
  not_found: "automation.not_found",
  run_not_found: "automation.run_not_found",
  already_running: "automation.already_running",
  delete_while_running: "automation.delete_while_running",
  delete_while_cleaning_up: "automation.delete_while_cleaning_up",
  event_topic_missing: "automation.event_topic_missing",
  event_topic_too_long: "automation.event_topic_too_long",
  event_topic_malformed: "automation.event_topic_malformed",
  event_id_missing: "automation.event_id_missing",
  event_id_too_long: "automation.event_id_too_long",
  event_id_invalid: "automation.event_id_invalid",
  event_filter_not_object: "automation.event_filter_not_object",
};

export function describeAutomationProblem(problem, t) {
  if (!problem) return "automation problem";
  const kind = typeof problem === "string" ? problem : problem.kind;
  const key = AUTOMATION_PROBLEM_KEYS[kind];
  if (!key) return "automation problem";
  const params = typeof problem === "object" ? problem : {};
  return t(key, params);
}

// ---- 投影缓存与通知边沿检测 ----

export function normalizeSchedule(raw) {
  if (!raw || typeof raw !== "object") return raw;
  if (raw.type === "weekdays") {
    return {
      type: "weekly",
      days: SCHEDULE_WEEKDAYS_MASK,
      hour: Number(raw.hour ?? 0),
      minute: Number(raw.minute ?? 0),
    };
  }
  return raw;
}

export function normalizeTrigger(raw) {
  if (!raw || typeof raw !== "object") {
    return { schedules: [], webhooks: [], events: [] };
  }
  if (typeof raw.kind === "string") {
    if (raw.kind === "schedule") {
      const list = Array.isArray(raw.schedules) && raw.schedules.length > 0
        ? raw.schedules
        : raw.schedule
          ? [raw.schedule]
          : [];
      return { schedules: list.map(normalizeSchedule), webhooks: [], events: [] };
    }
    if (raw.kind === "webhook") {
      return {
        schedules: [],
        webhooks: [{ endpoint: raw.endpoint ?? "", ...(raw.secret != null ? { secret: raw.secret } : {}) }],
        events: [],
      };
    }
    if (raw.kind === "event") {
      return {
        schedules: [],
        webhooks: [],
        events: [{ topic: raw.topic ?? "", ...(raw.filter != null ? { filter: raw.filter } : {}) }],
      };
    }
  }
  const schedules = Array.isArray(raw.schedules) ? raw.schedules.map(normalizeSchedule) : [];
  const webhooks = [];
  if (raw.webhook && typeof raw.webhook === "object") webhooks.push(raw.webhook);
  if (Array.isArray(raw.webhooks)) webhooks.push(...raw.webhooks);
  const events = [];
  if (raw.event && typeof raw.event === "object") events.push(raw.event);
  if (Array.isArray(raw.events)) events.push(...raw.events);
  return { schedules, webhooks, events };
}

export function normalizeAutomation(raw) {
  if (!raw || typeof raw !== "object") return raw;
  return {
    id: String(raw.id ?? ""),
    name: String(raw.name ?? ""),
    enabled: raw.enabled !== false,
    workspace_dir: typeof raw.workspace_dir === "string" ? raw.workspace_dir : null,
    trigger: normalizeTrigger(raw.trigger),
    action: raw.action ?? { type: "shell", command: "", args: [] },
    sinks: Array.isArray(raw.sinks) ? raw.sinks : [],
  };
}

export function normalizeAutomationFile(raw) {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  return {
    schema_version: Number(raw.schema_version ?? AUTOMATION_FILE_SCHEMA_VERSION),
    store_id: typeof raw.store_id === "string" ? raw.store_id : "",
    revision: Number(raw.revision ?? 0),
    timezone_fingerprint: typeof raw.timezone_fingerprint === "string" ? raw.timezone_fingerprint : "",
    automations: Array.isArray(raw.automations) ? raw.automations.map(normalizeAutomation) : [],
    states: Array.isArray(raw.states) ? raw.states : [],
    runs: Array.isArray(raw.runs) ? raw.runs : [],
    store_error: typeof raw.store_error === "string" ? raw.store_error : null,
    webhook_base_url: typeof raw.webhook_base_url === "string" ? raw.webhook_base_url : null,
  };
}

/** 解码 `automations.changed` 话题的 `CoreProjectionEvent` 载荷。 */
export function decodeAutomationsPayload(payload) {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    return { ok: false, error: "expected projection envelope object" };
  }
  if (typeof payload.revision !== "number" || !("data" in payload)) {
    return { ok: false, error: "missing revision or data in projection envelope" };
  }
  if (payload.data === null) {
    return { ok: true, value: null };
  }
  const file = normalizeAutomationFile(payload.data);
  if (!file) {
    return { ok: false, error: "invalid automation snapshot payload" };
  }
  return { ok: true, value: file };
}

export function createEmptyMachineAutomations() {
  return {
    automations: [],
    store_id: "",
    retiredStoreIds: new Set(),
    revision: 0,
    states: [],
    runs: [],
    store_error: null,
    webhook_base_url: null,
  };
}

export function isMachineReady(machine) {
  return Boolean(machine && typeof machine.store_id === "string" && machine.store_id.length > 0);
}

export function findAutomation(machine, automationId) {
  if (!machine || !Array.isArray(machine.automations)) return null;
  return machine.automations.find((a) => a.id === automationId) ?? null;
}

export function automationsForAgent(machine, agentId) {
  if (!machine || !Array.isArray(machine.automations)) return [];
  return machine.automations.filter((a) => actionAgentDefinitionId(a.action) === agentId);
}

export function stateForAutomation(machine, automationId) {
  if (!machine || !Array.isArray(machine.states)) return null;
  return machine.states.find((s) => s.automation_id === automationId) ?? null;
}

export function activeRunForAutomation(machine, automationId) {
  if (!machine || !Array.isArray(machine.runs)) return null;
  return machine.runs.find((r) => r.automation_id === automationId && runBlocksOverlap(r)) ?? null;
}

export function compareRunsDesc(a, b) {
  if (b.created_at !== a.created_at) return b.created_at - a.created_at;
  if (b.id > a.id) return 1;
  if (b.id < a.id) return -1;
  return 0;
}

export function latestRunForAutomation(machine, automationId) {
  if (!machine || !Array.isArray(machine.runs)) return null;
  let best = null;
  for (const run of machine.runs) {
    if (run.automation_id !== automationId) continue;
    if (!best || compareRunsDesc(run, best) < 0) {
      best = run;
    }
  }
  return best;
}

export function runsForAutomation(machine, automationId) {
  if (!machine || !Array.isArray(machine.runs)) return [];
  return machine.runs.filter((r) => r.automation_id === automationId).sort(compareRunsDesc);
}

export function isRetiredStore(machine, storeId) {
  return Boolean(machine?.retiredStoreIds?.has(storeId));
}

export function snapshotIsCurrentStore(machine, snapshot) {
  if (!machine) return true;
  if (!snapshot) return false;
  return (
    !isRetiredStore(machine, snapshot.store_id) &&
    (machine.store_id === "" || machine.store_id === snapshot.store_id)
  );
}

/**
 * 收下运行时推来的一份库：同 store_id 拒绝旧 revision 及同内容重复推送；
 * 切换 store_id（运行时重建了库）时旧 store_id 退休，它迟到的快照不再收。
 */
export function replaceMachineSnapshot(current, snapshot) {
  if (snapshot.store_id === current.store_id) {
    if (snapshot.revision < current.revision) {
      return false;
    }
    if (
      snapshot.revision === current.revision &&
      stableEqual(snapshot.automations, current.automations) &&
      stableEqual(snapshot.states, current.states) &&
      stableEqual(snapshot.runs, current.runs) &&
      snapshot.store_error === current.store_error &&
      snapshot.webhook_base_url === current.webhook_base_url
    ) {
      return false;
    }
  } else {
    if (current.retiredStoreIds.has(snapshot.store_id)) {
      return false;
    }
    if (current.store_id.length > 0) {
      current.retiredStoreIds.add(current.store_id);
    }
  }
  current.store_id = snapshot.store_id;
  current.revision = snapshot.revision;
  current.automations = snapshot.automations;
  current.states = snapshot.states;
  current.runs = snapshot.runs;
  current.store_error = snapshot.store_error;
  current.webhook_base_url = snapshot.webhook_base_url;
  return true;
}

/** 重连快照若仍属同一个 store_id，保留增量完成边沿检测。 */
export function effectiveProjectionOrigin(requested, previousStoreId, currentStoreId) {
  if (
    requested === "initial_snapshot" &&
    previousStoreId &&
    previousStoreId === currentStoreId
  ) {
    return "incremental";
  }
  return requested;
}

export function notificationExcerpt(value) {
  const MAX_CHARS = 240;
  if (typeof value !== "string") return null;
  const compact = value.trim().split(/\s+/).filter(Boolean).join(" ");
  if (!compact) return null;
  const chars = Array.from(compact);
  if (chars.length <= MAX_CHARS) return compact;
  return `${chars.slice(0, MAX_CHARS).join("")}…`;
}

export function automationRunNotifications(previousRuns, currentRuns, origin, t) {
  if (origin === "initial_snapshot") return [];
  const prevById = new Map();
  for (const run of previousRuns ?? []) {
    prevById.set(run.id, run);
  }
  const notifications = [];
  for (const run of currentRuns ?? []) {
    let kind = null;
    let fallback = "";
    if (run.status === "completed") {
      kind = "success";
      fallback = t("notify.run_completed_body");
    } else if (run.status === "failed") {
      kind = "failure";
      fallback = t("notify.run_failed_body");
    } else if (run.status === "skipped") {
      kind = "failure";
      fallback = t("notify.run_skipped_body");
    } else {
      continue;
    }
    const prev = prevById.get(run.id);
    if (prev && isRunStatusTerminal(prev.status)) {
      continue;
    }
    const rawDetail =
      run.status === "completed" ? run.output : runErrorText(run, t);
    const detail = notificationExcerpt(rawDetail) ?? fallback;
    const name = String(run.context?.automation_name ?? "").trim();
    const subtitle =
      run.status === "completed"
        ? t("notify.run_completed", { name })
        : run.status === "failed"
          ? t("notify.run_failed", { name })
          : t("notify.run_skipped", { name });
    notifications.push({
      automation_id: run.automation_id,
      run_id: run.id,
      kind,
      subtitle,
      body: `${runSourceLabel(run.source, t)} · ${detail}`,
    });
  }
  return notifications;
}

/**
 * 将快照应用到按 machineKey 索引的 Map 中，返回 `{ notify, silenced, runs }` 或 `null`（无变化）。
 */
export function applyAutomationsSnapshot(machinesMap, machine, snapshot, origin, machineName, t) {
  const key = machineKey(machine);
  let current = machinesMap.get(key);
  if (!current) {
    current = createEmptyMachineAutomations();
    machinesMap.set(key, current);
  }
  const effOrigin = effectiveProjectionOrigin(origin, current.store_id, snapshot.store_id);
  const candidates = automationRunNotifications(current.runs, snapshot.runs, effOrigin, t);
  const notifyByAutomation = new Map();
  for (const auto of snapshot.automations) {
    notifyByAutomation.set(auto.id, notifyFromSinks(auto.sinks));
  }
  const runs = new Set(snapshot.runs.map((r) => r.id));
  if (!replaceMachineSnapshot(current, snapshot)) {
    return null;
  }
  const notify = [];
  const silenced = [];
  for (const item of candidates) {
    const failed = item.kind === "failure";
    const setting = notifyByAutomation.get(item.automation_id) ?? "always";
    if (notifyShouldFire(setting, failed)) {
      notify.push({
        ...item,
        subtitle: machineName ? `${machineName} · ${item.subtitle}` : item.subtitle,
      });
    } else {
      silenced.push(item.run_id);
    }
  }
  return { notify, silenced, runs };
}

/** 入口角标数：所有机器上「已启用且最近一次运行失败」的自动化条数。 */
export function countFailingAutomations(machinesMap) {
  let total = 0;
  for (const machine of machinesMap.values()) {
    for (const auto of machine.automations ?? []) {
      if (!auto.enabled) continue;
      const latest = latestRunForAutomation(machine, auto.id);
      if (latest?.status === "failed") {
        total += 1;
      }
    }
  }
  return total;
}

// ---- 智能体就绪判断 ----

export function agentEngineId(agent) {
  if (!agent || typeof agent !== "object") return "";
  if (typeof agent.engine_kind === "string") return agent.engine_kind;
  if (typeof agent.engine_kind_id === "string") return agent.engine_kind_id;
  if (typeof agent.agent_id === "string") return agent.agent_id;
  return "";
}

export function agentDisplayName(name, t) {
  const trimmed = typeof name === "string" ? name.trim() : "";
  return trimmed.length > 0 ? trimmed : t("agent.unnamed");
}

// ---- 触发器预设与日程换算 ----

export function actionKindLabel(kind, t) {
  return kind === "agent" ? t("action.agent") : t("action.shell");
}

export function actionKindFromAction(action) {
  return action?.type === "shell" ? "shell" : "agent";
}

export function triggerPresetLabel(preset, t) {
  if (preset === "hourly") return t("trigger_preset.hourly");
  if (preset === "daily") return t("trigger_preset.daily");
  if (preset === "weekly") return t("trigger_preset.weekly");
  if (preset === "interval") return t("trigger_preset.interval");
  return "Webhook";
}

export function triggerPresetHint(preset, t) {
  if (preset === "hourly") return t("trigger_preset.hourly_hint");
  if (preset === "daily") return t("trigger_preset.daily_hint");
  if (preset === "weekly") return t("trigger_preset.weekly_hint");
  if (preset === "interval") return t("trigger_preset.interval_hint");
  return t("trigger_preset.webhook_hint");
}

export function triggerPresetIsAdvanced(preset) {
  return preset === "webhook";
}

export function triggerPresetKind(preset) {
  if (preset === "hourly" || preset === "interval") return "interval";
  if (preset === "daily" || preset === "weekly") return "calendar";
  return "webhook";
}

export function triggerPresetSchedule(preset) {
  if (preset === "hourly") return { type: "every_hours", hours: 1 };
  if (preset === "daily") return { type: "daily", hour: 9, minute: 0 };
  if (preset === "weekly") return { type: "weekly", days: SCHEDULE_DAY_MON, hour: 9, minute: 0 };
  if (preset === "interval") return { type: "every_minutes", minutes: 15 };
  return null;
}

export function triggerPresetIconPath(preset) {
  if (preset === "webhook") return "icons/webhook.svg";
  if (preset === "hourly" || preset === "interval") return "icons/clock.svg";
  return "icons/calendar.svg";
}

export function scheduleIsInterval(schedule) {
  if (!schedule) return false;
  return (
    schedule.type === "every_minutes" ||
    schedule.type === "every_hours" ||
    schedule.type === "during"
  );
}

export function intervalDayScopeFromDays(days) {
  const mask = (days ?? 0) & SCHEDULE_ALL_DAYS_MASK;
  if (mask === 0 || mask === SCHEDULE_ALL_DAYS_MASK) return "everyday";
  if (mask === SCHEDULE_WEEKDAYS_MASK) return "weekdays";
  return "weekly";
}

export function clockInputIsEditable(value) {
  if (typeof value !== "string" || value.length > 5) return false;
  if (!/^[0-9:]*$/.test(value)) return false;
  const parts = value.split(":");
  if (parts.length > 2) return false;
  const hour = parts[0] ?? "";
  const minute = parts.length === 2 ? parts[1] : null;
  if (hour.length > 2 || (hour.length === 0 && minute !== null)) return false;
  if (hour.length > 0) {
    const h = Number(hour);
    if (!Number.isInteger(h) || h > 23) return false;
  }
  if (minute !== null) {
    if (minute.length > 2) return false;
    if (minute.length > 0) {
      const m = Number(minute);
      if (!Number.isInteger(m) || m > 59) return false;
    }
  }
  return true;
}

export function parseClock(value) {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  const idx = trimmed.indexOf(":");
  if (idx <= 0 || idx !== trimmed.lastIndexOf(":")) return null;
  const hourStr = trimmed.slice(0, idx);
  const minuteStr = trimmed.slice(idx + 1);
  if (
    hourStr.length === 0 ||
    minuteStr.length === 0 ||
    hourStr.length > 2 ||
    minuteStr.length > 2 ||
    !/^\d+$/.test(hourStr) ||
    !/^\d+$/.test(minuteStr)
  ) {
    return null;
  }
  const hour = Number(hourStr);
  const minute = Number(minuteStr);
  if (hour > 23 || minute > 59) return null;
  return [hour, minute];
}

function parseStrictPositiveInt(value) {
  if (typeof value !== "string") return null;
  const trimmed = value.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const num = Number(trimmed);
  if (!Number.isSafeInteger(num)) return null;
  return num;
}

/**
 * 编辑器里一条日程规则的取值 → 线格式日程。表单层的检查（时刻、间隔、星期）在这里，日程本身的
 * 规则交给 `rules.scheduleProblem`（SDK `automationScheduleProblem`，和运行时同一条）。
 */
export function scheduleFromValues(
  kind,
  time,
  interval,
  intervalUnit,
  weekdayMask,
  windowStart,
  windowEnd,
  t,
  rules,
) {
  let schedule;
  if (kind === "daily") {
    const parsed = parseClock(time);
    if (!parsed) return { ok: false, error: t("error.invalid_time") };
    schedule = { type: "daily", hour: parsed[0], minute: parsed[1] };
  } else if (kind === "weekdays") {
    const parsed = parseClock(time);
    if (!parsed) return { ok: false, error: t("error.invalid_time") };
    schedule = {
      type: "weekly",
      days: SCHEDULE_WEEKDAYS_MASK,
      hour: parsed[0],
      minute: parsed[1],
    };
  } else if (kind === "weekly") {
    const parsed = parseClock(time);
    if (!parsed) return { ok: false, error: t("error.invalid_time") };
    const days = (weekdayMask ?? 0) & SCHEDULE_ALL_DAYS_MASK;
    if (days === 0) return { ok: false, error: t("error.no_days") };
    if (days === SCHEDULE_ALL_DAYS_MASK) {
      schedule = { type: "daily", hour: parsed[0], minute: parsed[1] };
    } else {
      schedule = { type: "weekly", days, hour: parsed[0], minute: parsed[1] };
    }
  } else if (kind === "interval") {
    const start = String(windowStart ?? "").trim();
    const end = String(windowEnd ?? "").trim();
    let window = null;
    if (start.length > 0 && end.length > 0) {
      const s = parseClock(start);
      const e = parseClock(end);
      if (!s || !e) return { ok: false, error: t("error.invalid_time") };
      window = [s, e];
    } else if (start.length > 0 || end.length > 0) {
      return { ok: false, error: t("error.window_incomplete") };
    }
    const days = (weekdayMask ?? 0) & SCHEDULE_ALL_DAYS_MASK;
    const constrained = window !== null || (days !== 0 && days !== SCHEDULE_ALL_DAYS_MASK);
    if (intervalUnit === "minutes") {
      const minutes = parseStrictPositiveInt(interval);
      if (minutes === null || minutes < 1 || minutes > 24 * 60) {
        return { ok: false, error: t("error.interval_minutes") };
      }
      if (!constrained) {
        schedule = { type: "every_minutes", minutes };
      } else {
        const [[start_hour, start_minute], [end_hour, end_minute]] = window ?? [
          [0, 0],
          [23, 59],
        ];
        schedule = {
          type: "during",
          minutes,
          days: days === 0 ? SCHEDULE_ALL_DAYS_MASK : days,
          start_hour,
          start_minute,
          end_hour,
          end_minute,
        };
      }
    } else {
      const hours = parseStrictPositiveInt(interval);
      if (hours === null || hours < 1 || hours > 24 * 7) {
        return { ok: false, error: t("error.interval_hours") };
      }
      if (!constrained) {
        schedule = { type: "every_hours", hours };
      } else {
        const [[start_hour, start_minute], [end_hour, end_minute]] = window ?? [
          [0, 0],
          [23, 59],
        ];
        schedule = {
          type: "during",
          minutes: Math.min(hours * 60, 0xffffffff),
          days: days === 0 ? SCHEDULE_ALL_DAYS_MASK : days,
          start_hour,
          start_minute,
          end_hour,
          end_minute,
        };
      }
    }
  } else {
    return { ok: false, error: t("automation.no_trigger") };
  }

  const problem = rules.scheduleProblem(schedule);
  if (problem) return { ok: false, error: describeAutomationProblem(problem, t) };
  return { ok: true, schedule };
}

export function scheduleFromRule(rule, t, rules) {
  let weekdayMask = rule.weekdayMask;
  if (rule.kind === "interval") {
    if (rule.intervalDayScope === "everyday") {
      weekdayMask = SCHEDULE_ALL_DAYS_MASK;
    } else if (rule.intervalDayScope === "weekdays") {
      weekdayMask = SCHEDULE_WEEKDAYS_MASK;
    } else {
      const days = (rule.weekdayMask ?? 0) & SCHEDULE_ALL_DAYS_MASK;
      if (days === 0) return { ok: false, error: t("error.no_days") };
      weekdayMask = days;
    }
  }
  return scheduleFromValues(
    rule.kind,
    rule.time,
    rule.interval,
    rule.intervalUnit,
    weekdayMask,
    rule.windowStart,
    rule.windowEnd,
    t,
    rules,
  );
}

export function createScheduleRuleDraft(schedule) {
  let hour = 9;
  let minute = 0;
  let interval = 15;
  let kind = "daily";
  let intervalUnit = "minutes";
  let weekdayMask = SCHEDULE_ALL_DAYS_MASK;
  let intervalDayScope = "everyday";
  let windowStart = "";
  let windowEnd = "";

  const normalized = normalizeSchedule(schedule);
  if (!normalized) {
    interval = 1;
  } else if (normalized.type === "weekly") {
    hour = normalized.hour;
    minute = normalized.minute;
    kind = normalized.days === SCHEDULE_WEEKDAYS_MASK ? "weekdays" : "weekly";
    weekdayMask = normalized.days;
    intervalDayScope = intervalDayScopeFromDays(normalized.days);
  } else if (normalized.type === "daily") {
    hour = normalized.hour;
    minute = normalized.minute;
    kind = "daily";
  } else if (normalized.type === "every_minutes") {
    interval = Math.max(1, normalized.minutes);
    kind = "interval";
  } else if (normalized.type === "every_hours") {
    interval = Math.max(1, normalized.hours);
    kind = "interval";
    intervalUnit = "hours";
  } else if (normalized.type === "during") {
    hour = normalized.start_hour;
    minute = normalized.start_minute;
    if (normalized.minutes >= 60 && normalized.minutes % 60 === 0) {
      interval = normalized.minutes / 60;
      intervalUnit = "hours";
    } else {
      interval = Math.max(1, normalized.minutes);
    }
    kind = "interval";
    weekdayMask = normalized.days === 0 ? SCHEDULE_ALL_DAYS_MASK : normalized.days;
    intervalDayScope = intervalDayScopeFromDays(normalized.days);
    if (
      !(
        normalized.start_hour === 0 &&
        normalized.start_minute === 0 &&
        normalized.end_hour === 23 &&
        normalized.end_minute === 59
      )
    ) {
      windowStart = clockText(normalized.start_hour, normalized.start_minute);
      windowEnd = clockText(normalized.end_hour, normalized.end_minute);
    }
  }

  return {
    kind,
    intervalUnit,
    intervalDayScope,
    weekdayMask,
    time: clockText(hour, minute),
    interval: String(interval),
    windowStart,
    windowEnd,
    windowOpen: windowStart.length > 0,
  };
}

/**
 * 构建触发器编辑条目（Bug Fix #4）：
 * webhook 条目只存 `{ uid, triggerKind: "webhook", webhookSecret }`，不创建无用的排程输入状态。
 */
export function createTriggerEntryDraft(uid, triggerKind, schedule, webhookSecret, uuidFn) {
  if (triggerKind === "webhook") {
    const clean = typeof webhookSecret === "string" ? webhookSecret.trim() : "";
    return {
      uid,
      triggerKind: "webhook",
      webhookSecret: clean.length > 0 ? clean : newWebhookSecret(uuidFn),
    };
  }
  const baseSchedule =
    schedule ?? (triggerKind === "interval" ? { type: "every_minutes", minutes: 15 } : null);
  const rule = createScheduleRuleDraft(baseSchedule);
  if (triggerKind === "interval") {
    rule.kind = "interval";
  } else if (triggerKind === "calendar" && rule.kind === "interval") {
    rule.kind = "daily";
  }
  return {
    uid,
    triggerKind,
    schedule: rule,
  };
}

/** 编辑器的触发条目 → 线格式触发器；整组的规则交给 `rules.triggerProblem`（SDK `automationTriggerProblem`）。 */
export function triggerFromDraft(entries, retainedEvents, t, uuidFn, rules) {
  const trigger = {
    schedules: [],
    webhooks: [],
    events: Array.isArray(retainedEvents) ? retainedEvents.slice() : [],
  };
  for (const entry of entries ?? []) {
    if (entry.triggerKind === "webhook") {
      const raw = String(entry.webhookSecret ?? "").trim();
      if (raw === REDACTED_WEBHOOK_ENDPOINT) {
        trigger.webhooks.push({ endpoint: REDACTED_WEBHOOK_ENDPOINT });
      } else {
        const secret = raw.length > 0 ? raw : newWebhookSecret(uuidFn);
        trigger.webhooks.push({ endpoint: secret, secret });
      }
    } else {
      const res = scheduleFromRule(entry.schedule, t, rules);
      if (!res.ok) return res;
      trigger.schedules.push(res.schedule);
    }
  }
  const problem = rules.triggerProblem(trigger);
  if (problem) return { ok: false, error: describeAutomationProblem(problem, t) };
  const wire = {};
  if (trigger.schedules.length > 0) wire.schedules = trigger.schedules;
  if (trigger.webhooks.length > 0) wire.webhooks = trigger.webhooks;
  if (trigger.events.length > 0) wire.events = trigger.events;
  return { ok: true, trigger: wire };
}

// ---- 表单草稿与缺失项检查 ----

export function actionFromDraft(draft) {
  if (draft.actionKind === "agent") {
    const prompt = String(draft.prompt ?? "").trim();
    const out = {
      type: "agent",
      agent_definition_id: String(draft.agentId ?? ""),
    };
    if (prompt.length > 0) out.prompt = prompt;
    return out;
  }
  return {
    type: "shell",
    command: String(draft.command ?? "").trim(),
    args: [],
  };
}

export function countUnfilledPlaceholders(placeholders, prompt) {
  if (!Array.isArray(placeholders) || placeholders.length === 0) return 0;
  if (typeof prompt !== "string" || prompt.length === 0) return 0;
  return placeholders.filter((slot) => prompt.includes(slot)).length;
}

/**
 * 与 Rust `automation_missing_fields` 完全一致。
 * 返回 MissingField 对象列表：`{ kind: "name"|"trigger"|"agent"|"prompt"|"command" } | { kind: "placeholders", count }`
 */
export function automationMissingFields(
  name,
  triggerReady,
  hasSchedule,
  action,
  agentExists,
  unfilledPlaceholders,
) {
  const missing = [];
  if (String(name ?? "").trim().length === 0) {
    missing.push({ kind: "name" });
  }
  if (!triggerReady) {
    missing.push({ kind: "trigger" });
  }
  if (action?.type === "agent") {
    if (!agentExists) {
      missing.push({ kind: "agent" });
    }
    const promptEmpty = !action.prompt || String(action.prompt).trim().length === 0;
    if (hasSchedule && promptEmpty) {
      missing.push({ kind: "prompt" });
    } else if (unfilledPlaceholders > 0) {
      missing.push({ kind: "placeholders", count: unfilledPlaceholders });
    }
  } else {
    if (String(action?.command ?? "").trim().length === 0) {
      missing.push({ kind: "command" });
    }
  }
  return missing;
}

export function missingFieldLabel(field, t) {
  if (field.kind === "name") return t("missing.name");
  if (field.kind === "trigger") return t("missing.trigger");
  if (field.kind === "agent") return t("missing.agent");
  if (field.kind === "prompt") return t("missing.prompt");
  if (field.kind === "placeholders") return t("missing.placeholders", { count: field.count });
  return t("missing.command");
}

export function automationSaveHint(missing, isNew, t) {
  if (!Array.isArray(missing) || missing.length === 0) return null;
  const fields = missing.map((f) => missingFieldLabel(f, t)).join(t("list.separator"));
  return isNew ? t("missing.hint_new", { fields }) : t("missing.hint", { fields });
}

export function computeDraftMissingFields(draft, agentExists, t, uuidFn, rules) {
  const hasSchedule = (draft.entries ?? []).some((e) => e.triggerKind !== "webhook");
  const triggerRes = triggerFromDraft(draft.entries, draft.retainedEvents, t, uuidFn, rules);
  const action = actionFromDraft(draft);
  const unfilled =
    action.type === "agent" && action.prompt
      ? countUnfilledPlaceholders(draft.placeholders, action.prompt)
      : 0;
  return automationMissingFields(
    draft.name,
    triggerRes.ok,
    hasSchedule,
    action,
    agentExists,
    unfilled,
  );
}

/** 从 Automation 对象构建编辑器草稿（不带死字段 `name_editing`）。 */
export function buildDraftFromAutomation(automation, machine, isNew, nextUid, uuidFn) {
  const actionKind = actionKindFromAction(automation.action);
  const agentId = actionAgentDefinitionId(automation.action) ?? "";
  const command = isShellAction(automation.action) ? formatShellCommand(automation.action) : "";
  const prompt = actionPrompt(automation.action) ?? "";
  const trigger = normalizeTrigger(automation.trigger);
  const entries = [];
  for (const schedule of trigger.schedules) {
    entries.push(
      createTriggerEntryDraft(
        nextUid(),
        scheduleIsInterval(schedule) ? "interval" : "calendar",
        schedule,
        null,
        uuidFn,
      ),
    );
  }
  for (const webhook of trigger.webhooks) {
    entries.push(
      createTriggerEntryDraft(
        nextUid(),
        "webhook",
        null,
        webhookToken(webhook),
        uuidFn,
      ),
    );
  }
  return {
    machine: machine ?? null,
    id: automation.id,
    isNew: Boolean(isNew),
    actionKind,
    agentId,
    name: automation.name ?? "",
    prompt,
    command,
    retainedEvents: trigger.events.slice(),
    entries,
    notification: notifyFromSinks(automation.sinks),
    workspaceDir: typeof automation.workspace_dir === "string" ? automation.workspace_dir : null,
    placeholders: [],
  };
}

/** 从编辑器草稿组装发往 smeltd 的 `Automation` 线格式对象。 */
export function buildAutomationWireFromDraft(draft, enabled, t, uuidFn, rules) {
  const triggerRes = triggerFromDraft(draft.entries, draft.retainedEvents, t, uuidFn, rules);
  if (!triggerRes.ok) return triggerRes;
  const automation = {
    id: draft.id,
    name: String(draft.name ?? "").trim(),
    enabled: Boolean(enabled),
    trigger: triggerRes.trigger,
    action: actionFromDraft(draft),
    sinks: notifyToSinks(draft.notification),
  };
  if (typeof draft.workspaceDir === "string" && draft.workspaceDir.length > 0) {
    automation.workspace_dir = draft.workspaceDir;
  }
  return { ok: true, automation };
}

// ---- 内置模板与占位空 ----

export const BUILTIN_TEMPLATES = [
  {
    id: "morning-brief",
    nameKey: "template.morning_brief.name",
    descriptionKey: "template.morning_brief.description",
    promptKey: "template.morning_brief.prompt",
    category: "work",
    icon: "icons/sun.svg",
    schedule: { type: "daily", hour: 7, minute: 30 },
  },
  {
    id: "weekly-review",
    nameKey: "template.weekly_review.name",
    descriptionKey: "template.weekly_review.description",
    promptKey: "template.weekly_review.prompt",
    category: "work",
    icon: "icons/calendar.svg",
    schedule: { type: "weekly", days: SCHEDULE_DAY_FRI, hour: 16, minute: 0 },
  },
  {
    id: "daily-planner",
    nameKey: "template.daily_planner.name",
    descriptionKey: "template.daily_planner.description",
    promptKey: "template.daily_planner.prompt",
    category: "work",
    icon: "icons/layout-dashboard.svg",
    schedule: { type: "daily", hour: 8, minute: 30 },
  },
  {
    id: "task-extractor",
    nameKey: "template.task_extractor.name",
    descriptionKey: "template.task_extractor.description",
    promptKey: "template.task_extractor.prompt",
    category: "work",
    icon: "icons/inbox.svg",
    schedule: { type: "daily", hour: 17, minute: 0 },
  },
  {
    id: "pending-changes",
    nameKey: "template.pending_changes.name",
    descriptionKey: "template.pending_changes.description",
    promptKey: "template.pending_changes.prompt",
    category: "engineering",
    icon: "icons/file.svg",
    schedule: { type: "daily", hour: 9, minute: 0 },
  },
  {
    id: "dependency-check",
    nameKey: "template.dependency_check.name",
    descriptionKey: "template.dependency_check.description",
    promptKey: "template.dependency_check.prompt",
    category: "engineering",
    icon: "icons/warning.svg",
    schedule: { type: "weekly", days: SCHEDULE_DAY_MON, hour: 9, minute: 0 },
  },
  {
    id: "competitor-watch",
    nameKey: "template.competitor_watch.name",
    descriptionKey: "template.competitor_watch.description",
    promptKey: "template.competitor_watch.prompt",
    category: "research",
    icon: "icons/globe.svg",
    schedule: { type: "weekly", days: SCHEDULE_DAY_MON, hour: 9, minute: 0 },
  },
  {
    id: "tech-digest",
    nameKey: "template.tech_digest.name",
    descriptionKey: "template.tech_digest.description",
    promptKey: "template.tech_digest.prompt",
    category: "research",
    icon: "icons/cpu.svg",
    schedule: { type: "daily", hour: 8, minute: 0 },
  },
  {
    id: "paper-watch",
    nameKey: "template.paper_watch.name",
    descriptionKey: "template.paper_watch.description",
    promptKey: "template.paper_watch.prompt",
    category: "research",
    icon: "icons/book-open.svg",
    schedule: { type: "weekly", days: SCHEDULE_DAY_WED, hour: 9, minute: 0 },
  },
];

export function automationTemplate(id) {
  return BUILTIN_TEMPLATES.find((tpl) => tpl.id === id) ?? null;
}

export function filteredTemplates(category) {
  if (!category) return BUILTIN_TEMPLATES;
  return BUILTIN_TEMPLATES.filter((tpl) => tpl.category === category);
}

export function templateCategoryLabel(category, t) {
  if (category === "work") return t("template.work");
  if (category === "engineering") return t("template.engineering");
  return t("template.research");
}

/** 模板指令里留给用户补的空：英文 `[…]`，中文 `「填写…」`，按出现顺序去重。 */
export function templatePlaceholders(prompt) {
  const found = [];
  if (typeof prompt !== "string") return found;
  for (const [open, close] of [
    ["[", "]"],
    ["「填写", "」"],
  ]) {
    let offset = 0;
    while (offset < prompt.length) {
      const start = prompt.indexOf(open, offset);
      if (start < 0) break;
      const closeIdx = prompt.indexOf(close, start + open.length);
      if (closeIdx < 0) break;
      const end = closeIdx + close.length;
      const slot = prompt.slice(start, end);
      if (!found.includes(slot)) {
        found.push(slot);
      }
      offset = end;
    }
  }
  return found;
}

// ---- 时间、文案与状态格式化 ----

export function clockText(hour, minute) {
  return `${String(hour).padStart(2, "0")}:${String(minute).padStart(2, "0")}`;
}

export function localTimezoneLabel(t, date = new Date()) {
  const offsetMinutes = -date.getTimezoneOffset();
  const sign = offsetMinutes >= 0 ? "+" : "-";
  const abs = Math.abs(offsetMinutes);
  const hh = String(Math.floor(abs / 60)).padStart(2, "0");
  const mm = String(abs % 60).padStart(2, "0");
  return t("timezone.local", { offset: `UTC${sign}${hh}:${mm}` });
}

export function formatUnixLocal(tsSeconds) {
  if (typeof tsSeconds !== "number" || !Number.isFinite(tsSeconds)) return String(tsSeconds ?? "");
  const d = new Date(tsSeconds * 1000);
  if (Number.isNaN(d.getTime())) return String(tsSeconds);
  const mm = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  const hh = String(d.getHours()).padStart(2, "0");
  const mi = String(d.getMinutes()).padStart(2, "0");
  return `${mm}-${dd} ${hh}:${mi}`;
}

export function formatRelativeUntil(ts, now, t) {
  const delta = ts - now;
  if (delta <= 0) return t("time.due_now");
  const MINUTE = 60;
  const HOUR = 60 * MINUTE;
  const DAY = 24 * HOUR;
  if (delta < HOUR) {
    return t("time.in_minutes", { count: Math.max(1, Math.trunc(delta / MINUTE)) });
  }
  if (delta < DAY) {
    return t("time.in_hours", { count: Math.trunc(delta / HOUR) });
  }
  return t("time.in_days", { count: Math.trunc(delta / DAY) });
}

export function formatNextRunCell(nextRunAt, enabled, now, t) {
  if (!enabled || nextRunAt == null) return "—";
  return formatRelativeUntil(nextRunAt, now, t);
}

export function formatRelativeSince(ts, now, t) {
  const delta = now - ts;
  const MINUTE = 60;
  const HOUR = 60 * MINUTE;
  const DAY = 24 * HOUR;
  if (delta < MINUTE) return t("time.just_now");
  if (delta < HOUR) return t("time.minutes_ago", { count: Math.trunc(delta / MINUTE) });
  if (delta < DAY) return t("time.hours_ago", { count: Math.trunc(delta / HOUR) });
  if (delta < 30 * DAY) return t("time.days_ago", { count: Math.trunc(delta / DAY) });
  if (delta < 365 * DAY) return t("time.months_ago", { count: Math.trunc(delta / (30 * DAY)) });
  return t("time.years_ago", { count: Math.trunc(delta / (365 * DAY)) });
}

export function formatLastRun(status, at, now, t) {
  return t("catalog.last_run", {
    status: runStatusLabel(status, t),
    when: formatRelativeSince(at, now, t),
  });
}

export function formatElapsed(seconds, t) {
  const s = Math.max(0, Math.trunc(seconds));
  if (s < 60) return t("elapsed.seconds", { seconds: s });
  if (s < 3600) {
    return t("elapsed.minutes", { minutes: Math.trunc(s / 60), seconds: s % 60 });
  }
  return t("elapsed.hours", {
    hours: Math.trunc(s / 3600),
    minutes: Math.trunc((s % 3600) / 60),
  });
}

export function runSourceLabel(source, t) {
  if (source === "manual") return t("run_source.manual");
  if (source === "scheduled") return t("run_source.scheduled");
  if (source === "webhook") return t("run_source.webhook");
  return t("run_source.event");
}

export function runStatusLabel(status, t) {
  switch (status) {
    case "starting":
      return t("run_status.starting");
    case "queued":
      return t("run_status.queued");
    case "dispatching":
      return t("run_status.dispatching");
    case "running":
      return t("run_status.running");
    case "awaiting_approval":
      return t("run_status.awaiting_approval");
    case "waiting_for_user":
      return t("run_status.waiting_for_user");
    case "completed":
      return t("run_status.completed");
    case "failed":
      return t("run_status.failed");
    case "cancelled":
      return t("run_status.cancelled");
    case "skipped":
      return t("run_status.skipped");
    default:
      return String(status ?? "");
  }
}

export function weekdayLabel(index, t) {
  switch (index) {
    case 0:
      return t("weekday.mon");
    case 1:
      return t("weekday.tue");
    case 2:
      return t("weekday.wed");
    case 3:
      return t("weekday.thu");
    case 4:
      return t("weekday.fri");
    case 5:
      return t("weekday.sat");
    default:
      return t("weekday.sun");
  }
}

export function weeklyDaysSummary(days, t) {
  const mask = (days ?? 0) & SCHEDULE_ALL_DAYS_MASK;
  if (mask === SCHEDULE_ALL_DAYS_MASK) return t("schedule.every_day");
  const labels = [];
  for (let i = 0; i < 7; i++) {
    if ((mask & (1 << i)) !== 0) {
      labels.push(weekdayLabel(i, t));
    }
  }
  if (labels.length === 0) return t("schedule.no_days");
  return t("schedule.weekly_days", { days: labels.join(t("list.separator")) });
}

export function scheduleSummary(schedule, t) {
  const s = normalizeSchedule(schedule);
  if (!s) return "";
  switch (s.type) {
    case "every_minutes":
      return t("schedule.every_minutes", { count: s.minutes });
    case "every_hours":
      return t("schedule.every_hours", { count: s.hours });
    case "daily":
      return t("schedule.daily", { time: clockText(s.hour, s.minute) });
    case "weekly":
      return t("schedule.weekly", {
        days: weeklyDaysSummary(s.days, t),
        time: clockText(s.hour, s.minute),
      });
    case "during": {
      const days = weeklyDaysSummary(s.days === 0 ? SCHEDULE_ALL_DAYS_MASK : s.days, t);
      return t("schedule.during", {
        days,
        start: clockText(s.start_hour, s.start_minute),
        end: clockText(s.end_hour, s.end_minute),
        count: s.minutes,
      });
    }
    default:
      return "";
  }
}

export function triggerSummary(trigger, t) {
  const normalized = normalizeTrigger(trigger);
  const parts = normalized.schedules.map((s) => scheduleSummary(s, t));
  for (const webhook of normalized.webhooks) {
    parts.push(
      webhookToken(webhook) === null ? t("trigger.webhook_unset") : t("trigger.webhook"),
    );
  }
  for (const ev of normalized.events) {
    parts.push(t("trigger.event", { topic: ev.topic }));
  }
  return parts.length === 0 ? t("trigger.none") : parts.join(t("trigger.separator"));
}

export function intervalTitle(unit, value, t) {
  const trimmed = String(value ?? "").trim();
  if (!trimmed) return t("trigger_preset.interval");
  if (unit === "hours" && trimmed === "1") return t("trigger_preset.hourly");
  if (unit === "hours") return t("schedule.every_hours", { count: trimmed });
  return t("schedule.every_minutes", { count: trimmed });
}

export function triggerEntryTitle(entry, t) {
  if (entry.triggerKind === "webhook") return t("trigger_preset.webhook_hint");
  const rule = entry.schedule;
  if (entry.triggerKind === "calendar") {
    if (rule.kind === "daily") return t("schedule_kind.daily");
    if (rule.kind === "weekdays") return t("schedule_kind.weekdays");
    if (rule.kind === "weekly") return t("schedule_kind.weekly");
  }
  return intervalTitle(rule.intervalUnit, rule.interval, t);
}

export function triggerEntryHint(entry, t) {
  if (entry.triggerKind === "webhook") return t("trigger_preset.webhook_hint");
  if (entry.triggerKind === "calendar") {
    const kind = entry.schedule.kind;
    if (kind === "daily") return t("trigger_preset.daily_hint");
    if (kind === "weekdays") return t("trigger_preset.weekdays_hint");
    if (kind === "weekly") return t("trigger_preset.weekly_hint");
    return t("trigger_preset.fixed_interval_hint");
  }
  return t("trigger_preset.interval_hint");
}

export function automationPromptHint(isWebhook, t) {
  return isWebhook ? t("prompt.webhook_hint") : t("prompt.hint");
}

export function automationAccessNote(isAgent, t) {
  return isAgent ? t("access.agent_note") : t("access.shell_note");
}

export function excerptText(value, limit) {
  const compact = String(value ?? "").trim().split(/\s+/).filter(Boolean).join(" ");
  const chars = Array.from(compact);
  if (chars.length <= limit) return compact;
  return `${chars.slice(0, limit).join("")}…`;
}

export function compactInstructions(value) {
  return excerptText(value, 96);
}

export function automationRowStatus(enabled, storeError, agentMissing, running) {
  if (storeError) return "unavailable";
  if (agentMissing) return "agent_missing";
  if (running) return "running";
  return enabled ? "active" : "paused";
}

export function automationRowStatusLabel(status, t) {
  switch (status) {
    case "unavailable":
      return t("row_status.unavailable");
    case "agent_missing":
      return t("row_status.agent_missing");
    case "running":
      return t("run_status.running");
    case "active":
      return t("row_status.active");
    default:
      return t("row_status.paused");
  }
}

export function deleteConfirmDescription(name, runCount, t) {
  if (runCount <= 0) return t("delete.message", { name });
  if (runCount === 1) return t("delete.message_runs.one", { name });
  return t("delete.message_runs.other", { name, count: runCount });
}

// ---- 工具调用标题与图标 ----

function toolKindLabel(kind) {
  switch (kind) {
    case "read":
      return "Read";
    case "edit":
      return "Edit";
    case "delete":
      return "Delete";
    case "move":
      return "Move";
    case "search":
      return "Search";
    case "execute":
      return "Bash";
    case "fetch":
      return "Fetch";
    case "think":
      return "Think";
    case "switch_mode":
      return "Mode";
    case "collaborate":
      return "Agent";
    case "review":
      return "Review";
    case "image":
      return "Image";
    case "compact":
      return "Compact";
    case "wait":
      return "Wait";
    default:
      return "Tool";
  }
}

function compactFetchTarget(title) {
  let out = title;
  if (out.startsWith("https://")) out = out.slice(8);
  else if (out.startsWith("http://")) out = out.slice(7);
  if (out.startsWith("www.")) out = out.slice(4);
  return out;
}

function compactPathLeaf(title) {
  const parts = title.split(/[/\\]/).filter((p) => p.length > 0);
  return parts.length > 0 ? parts[parts.length - 1] : title;
}

function compactCommandLeaf(title) {
  const first = title.split(/\r?\n/)[0]?.trim() ?? "";
  return first.length > 0 ? first : title;
}

export function compactToolHeadline(kind, title, t) {
  const trimmed = String(title ?? "").trim();
  if (!trimmed) return toolKindLabel(kind);
  switch (kind) {
    case "fetch":
      return t("tool.fetched", { target: compactFetchTarget(trimmed) });
    case "search":
      return t("tool.searched", { target: trimmed });
    case "read":
      return t("tool.read", { target: compactPathLeaf(trimmed) });
    case "edit":
      return t("tool.edited", { target: compactPathLeaf(trimmed) });
    case "delete":
      return t("tool.deleted", { target: compactPathLeaf(trimmed) });
    case "move":
      return t("tool.moved", { target: compactPathLeaf(trimmed) });
    case "execute":
      return t("tool.ran", { target: compactCommandLeaf(trimmed) });
    case "image":
      return t("tool.processed", { target: compactPathLeaf(trimmed) });
    default:
      return trimmed;
  }
}

export function toolProcessIconPath(kind) {
  switch (kind) {
    case "fetch":
      return "icons/globe.svg";
    case "search":
      return "icons/search.svg";
    case "execute":
      return "icons/terminal.svg";
    case "read":
    case "move":
    case "image":
      return "icons/file.svg";
    case "edit":
      return "icons/square-pen.svg";
    case "delete":
      return "icons/trash.svg";
    case "collaborate":
      return "icons/bot.svg";
    case "review":
    case "compact":
    case "think":
    case "switch_mode":
      return "icons/asterisk.svg";
    case "wait":
      return "icons/clock.svg";
    default:
      return "icons/wrench.svg";
  }
}

// ---- 运行详情、Transcript 与 Diff 辅助 ----

export function makeRunDetailKey(machine, run) {
  return {
    machine: machine ?? null,
    runId: run.id,
    status: run.status,
    finishedAt: run.finished_at ?? null,
  };
}

export function sameRunDetailKey(a, b) {
  if (!a || !b) return false;
  return (
    machineKey(a.machine) === machineKey(b.machine) &&
    a.runId === b.runId &&
    a.status === b.status &&
    (a.finishedAt ?? null) === (b.finishedAt ?? null)
  );
}

export function buildRunDetail(key, run, archived, transcript, expanded) {
  const rawOutput = run?.output ?? archived?.output ?? null;
  const rawInstructions =
    run?.context?.agent_instructions ?? archived?.context?.agent_instructions ?? null;
  const output =
    typeof rawOutput === "string" && rawOutput.trim().length > 0 ? rawOutput : null;
  const instructions =
    typeof rawInstructions === "string" && rawInstructions.trim().length > 0
      ? rawInstructions
      : null;
  return {
    key,
    output,
    instructions,
    transcript: Array.isArray(transcript) ? transcript : [],
    expanded: expanded instanceof Set ? new Set(expanded) : new Set(),
  };
}

/**
 * 解析外部标记的 `AcpEntry` 为统一判定形状。
 */
export function classifyAcpEntry(entry) {
  if (!entry || typeof entry !== "object") return { type: "unknown" };
  if (typeof entry.User === "string") {
    return { type: "user", text: entry.User, images: 0 };
  }
  if (entry.UserWithImages && typeof entry.UserWithImages === "object") {
    return {
      type: "user",
      text: String(entry.UserWithImages.text ?? ""),
      images: Array.isArray(entry.UserWithImages.images) ? entry.UserWithImages.images.length : 0,
    };
  }
  if (entry.Assistant && typeof entry.Assistant === "object") {
    return {
      type: "assistant",
      text: String(entry.Assistant.text ?? ""),
      thought: Boolean(entry.Assistant.thought),
    };
  }
  if (entry.ToolCall && typeof entry.ToolCall === "object") {
    const tc = entry.ToolCall;
    return {
      type: "tool_call",
      id: String(tc.id ?? ""),
      title: String(tc.title ?? ""),
      kind: String(tc.kind ?? "other"),
      status: String(tc.status ?? "pending"),
      output: Array.isArray(tc.output) ? tc.output : [],
      children: Array.isArray(tc.children) ? tc.children : [],
    };
  }
  if (typeof entry.Divider === "string") {
    return { type: "divider", label: entry.Divider };
  }
  if (entry.NewSession && typeof entry.NewSession === "object") {
    return { type: "new_session", atMs: Number(entry.NewSession.at_ms ?? 0) };
  }
  if (entry.TurnFailed && typeof entry.TurnFailed === "object") {
    return {
      type: "turn_failed",
      detail: String(entry.TurnFailed.detail ?? ""),
      hint: typeof entry.TurnFailed.hint === "string" ? entry.TurnFailed.hint : null,
    };
  }
  if (entry.TaskNote && typeof entry.TaskNote === "object") {
    return { type: "task_note", note: entry.TaskNote };
  }
  return { type: "unknown" };
}

/** 识别外部标记的 `ToolOutputPart`。 */
export function classifyToolOutputPart(part) {
  if (!part || typeof part !== "object") return { type: "unknown" };
  if (typeof part.Text === "string") {
    return { type: "text", text: part.Text };
  }
  if (part.Diff && typeof part.Diff === "object") {
    return {
      type: "diff",
      path: String(part.Diff.path ?? ""),
      oldText: typeof part.Diff.old_text === "string" ? part.Diff.old_text : "",
      newText: String(part.Diff.new_text ?? ""),
    };
  }
  if (part.Terminal && typeof part.Terminal === "object") {
    return {
      type: "terminal",
      id: String(part.Terminal.id ?? ""),
      output: String(part.Terminal.output ?? ""),
      truncated: Boolean(part.Terminal.truncated),
      exitCode: typeof part.Terminal.exit_code === "number" ? part.Terminal.exit_code : null,
      signal: typeof part.Terminal.signal === "string" ? part.Terminal.signal : null,
    };
  }
  if (part.Image && typeof part.Image === "object") {
    return {
      type: "image",
      mime: String(part.Image.mime ?? "image/*"),
    };
  }
  return { type: "unknown" };
}

export function finalReplyIndex(output, transcript) {
  if (typeof output !== "string") return null;
  const trimmedOutput = output.trim();
  if (!trimmedOutput || !Array.isArray(transcript)) return null;
  for (let i = transcript.length - 1; i >= 0; i--) {
    const item = classifyAcpEntry(transcript[i]);
    if (item.type === "assistant" && !item.thought) {
      return item.text.trim() === trimmedOutput ? i : null;
    }
  }
  return null;
}

export function transcriptSummary(entries, t) {
  let tools = 0;
  let failed = 0;
  const walk = (list) => {
    for (const raw of list ?? []) {
      const item = classifyAcpEntry(raw);
      if (item.type === "tool_call") {
        tools += 1;
        if (item.status === "failed") failed += 1;
        walk(item.children);
      }
    }
  };
  walk(entries);
  const totalEntries = Array.isArray(entries) ? entries.length : 0;
  if (tools === 0) {
    return totalEntries === 1 ? t("run.messages.one") : t("run.messages.other", { count: totalEntries });
  }
  if (failed === 0) {
    return tools === 1 ? t("run.tool_calls.one") : t("run.tool_calls.other", { count: tools });
  }
  return t("run.tool_calls_failed", { count: tools, failed });
}

export function turnHintText(hint, t) {
  if (hint === "no_api_key") return t("turn_hint.no_api_key");
  if (hint === "api_key_rejected") return t("turn_hint.api_key_rejected");
  if (hint === "connection_refused") return t("turn_hint.connection_refused");
  if (hint === "rate_limited") return t("turn_hint.rate_limited");
  return null;
}

export function turnFailedText(detail, hint, t) {
  const clean = String(detail ?? "").trim();
  const head = clean.length === 0
    ? t("entry.turn_failed")
    : t("entry.turn_failed_with_detail", { detail: clean });
  const hintStr = turnHintText(hint, t);
  return hintStr ? `${head}\n\n${hintStr}` : head;
}

export function newSessionLabel(atMs, t) {
  const at = typeof atMs === "number" && Number.isFinite(atMs)
    ? formatUnixLocal(Math.floor(atMs / 1000))
    : "";
  return t("entry.new_session", { at });
}

export function taskNoteSummary(note, t) {
  if (!note || typeof note !== "object") return "";
  if (note.peer && typeof note.peer === "object") {
    const from = note.peer.sourceTitle ? note.peer.sourceTitle : note.peer.sourceSessionId ?? "";
    const kind = note.peer.kind ?? "message";
    if (kind === "task") return t("peer.received.task", { from });
    if (kind === "completion") return t("peer.received.completion", { from });
    if (kind === "bounce") return t("peer.received.bounce", { from });
    return t("peer.received.message", { from });
  }
  let status = String(note.status ?? "");
  if (status === "completed") status = t("task_note.status.completed");
  else if (status === "failed") status = t("task_note.status.failed");
  else if (status === "timed_out") status = t("task_note.status.timed_out");
  else if (status === "stopped") status = t("task_note.status.stopped");
  else if (status === "running") status = t("task_note.status.running");

  const kind = note.kind ?? "backgroundTask";
  const runs = Array.isArray(note.runs) ? note.runs : [];
  let who;
  if (kind === "subagent") {
    if (runs.length === 1) who = t("task_note.subagent", { id: runs[0].id });
    else if (runs.length === 0) who = t("task_note.subagent", { id: note.id ?? "" });
    else who = t("task_note.subagents", { count: runs.length });
  } else if (kind === "peerMessage") {
    who = t("task_note.message", { id: note.id ?? "" });
  } else {
    who = t("task_note.background_task", { id: note.id ?? "" });
  }
  if (note.title) {
    who = t("task_note.titled", { who, title: note.title });
  }
  if (typeof note.exitCode === "number") {
    return t("task_note.ended_with_code", { who, status, code: note.exitCode });
  }
  return t("task_note.ended", { who, status });
}

/** 围栏长度比正文里最长反引号串多一个，超长输出按行裁切并标明剩余行数。 */
export function fencedBlock(lang, body, t) {
  const raw = String(body ?? "");
  const lines = raw.length === 0 ? [] : raw.replace(/\r?\n$/, "").split(/\r?\n/);
  let clipped;
  if (lines.length > TOOL_OUTPUT_MAX_LINES) {
    clipped = `${lines.slice(0, TOOL_OUTPUT_MAX_LINES).join("\n")}\n${t("run.lines_hidden", {
      count: lines.length - TOOL_OUTPUT_MAX_LINES,
    })}`;
  } else {
    clipped = raw;
  }
  let longest = 0;
  let runLen = 0;
  for (let i = 0; i < clipped.length; i++) {
    if (clipped.charCodeAt(i) === 96) {
      runLen += 1;
      if (runLen > longest) longest = runLen;
    } else {
      runLen = 0;
    }
  }
  const fence = "`".repeat(Math.max(longest, 2) + 1);
  return `${fence}${lang}\n${clipped}\n${fence}`;
}

export function editorLanguageForPath(path) {
  if (typeof path !== "string" || path.length === 0) return "text";
  const leaf = compactPathLeaf(path);
  const dot = leaf.lastIndexOf(".");
  if (dot > 0 && dot < leaf.length - 1) {
    return leaf.slice(dot + 1).toLowerCase();
  }
  return leaf.toLowerCase() || "text";
}

export function estimateDiffHeight(oldText, newText) {
  const oldLines = oldText ? oldText.split("\n").length : 0;
  const newLines = newText ? newText.split("\n").length : 0;
  const approxLines = Math.max(oldLines, newLines, 4);
  return Math.max(120, Math.min(420, approxLines * 20 + 28));
}

/**
 * 运行摘要行信息（Bug Fix #5）：
 * Shell 运行使用终端图标和 "Shell" 标签，不再错标为 "Unknown agent" + Bot 图标。
 */
export function runSummaryInfo(run, t) {
  const shell = isShellAction(run?.context?.action);
  const icon = shell ? "icons/terminal.svg" : "icons/bot.svg";
  const actor = shell
    ? t("action.shell")
    : run?.context?.agent_definition_name != null
      ? agentDisplayName(run.context.agent_definition_name, t)
      : t("meta.unknown_agent");
  const parts = [formatUnixLocal(run?.started_at ?? run?.created_at ?? 0)];
  if (run?.started_at != null && run?.finished_at != null) {
    parts.push(
      t("run.elapsed", {
        elapsed: formatElapsed(run.finished_at - run.started_at, t),
      }),
    );
  }
  return { icon, actor, detail: parts.join(" · ") };
}

/**
 * 运行信息元数据行（Bug Fix #5）：
 * Shell 运行显示「执行方式：Shell」与「命令」，不再显示「智能体：未知智能体 · 未知引擎」。
 */
export function runMetadataRows(run, t) {
  const rows = [{ label: t("meta.source"), value: runSourceLabel(run.source, t) }];
  if (isShellAction(run?.context?.action)) {
    rows.push({ label: t("meta.action"), value: t("action.shell") });
    const cmd = formatShellCommand(run.context.action).trim();
    if (cmd) {
      rows.push({ label: t("meta.command"), value: cmd });
    }
  } else {
    const agentName =
      run?.context?.agent_definition_name != null
        ? agentDisplayName(run.context.agent_definition_name, t)
        : t("meta.unknown_agent");
    const engineName = run?.context?.engine_kind_id || t("meta.unknown_engine");
    rows.push({ label: t("meta.agent"), value: `${agentName} · ${engineName}` });
  }
  rows.push({ label: t("meta.workspace"), value: String(run?.context?.cwd ?? "") });
  rows.push({ label: t("meta.created"), value: formatUnixLocal(run.created_at) });
  if (run.scheduled_for != null) {
    rows.push({ label: t("meta.scheduled"), value: formatUnixLocal(run.scheduled_for) });
  }
  if (run.delivery_attempt_at != null) {
    rows.push({
      label: t("meta.delivered"),
      value: t("meta.delivered_value", {
        time: formatUnixLocal(run.delivery_attempt_at),
        count: run.delivery_attempts ?? 0,
      }),
    });
  }
  if (run.started_at != null) {
    rows.push({ label: t("meta.started"), value: formatUnixLocal(run.started_at) });
  }
  if (run.finished_at != null) {
    rows.push({ label: t("meta.finished"), value: formatUnixLocal(run.finished_at) });
  }
  rows.push({ label: "Run ID", value: run.id });
  rows.push({ label: "Automation ID", value: run.automation_id });
  const defId = actionAgentDefinitionId(run?.context?.action);
  if (defId) {
    rows.push({ label: t("meta.agent_definition_id"), value: defId });
  }
  if (run.session_id) {
    rows.push({ label: t("meta.acp_session"), value: run.session_id });
  }
  if (run.provider_session_id) {
    rows.push({ label: t("meta.provider_session"), value: run.provider_session_id });
  }
  return rows;
}
