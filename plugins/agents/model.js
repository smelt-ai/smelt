// 智能体插件的纯领域模型与辅助函数（不依赖 NSH / 宿主运行时，由 bun test 直接验证）。

export const WORKSPACE_SKILL_DIRS = [".agents/skills", ".claude/skills", ".pi/skills"];
export const CORE_MEMORY_MAX_BYTES = 3000;
export const RULE_MEMORY_TYPES = ["user", "feedback"];
export const LAUNCH_ONLY_ENV = [
  "PATH",
  "HOME",
  "USERPROFILE",
  "SHELL",
  "NODE_OPTIONS",
  "NODE_PATH",
  "LD_PRELOAD",
  "LD_LIBRARY_PATH",
  "DYLD_INSERT_LIBRARIES",
  "BASH_ENV",
];

// ---- 机器键与深拷贝 ----

export function machineKey(machine) {
  return typeof machine === "string" && machine.length > 0 ? machine : "";
}

export function machineFromKey(key) {
  return key ? key : null;
}

export function agentRefKey(machine, id) {
  return `${machineKey(machine)}\0${id ?? ""}`;
}

export function sameAgentRef(a, b) {
  if (!a || !b) return false;
  return machineKey(a.machine) === machineKey(b.machine) && a.id === b.id;
}

export function cloneJson(value) {
  return JSON.parse(JSON.stringify(value));
}

export function stableEqual(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

// ---- 智能体配置校验与读取 ----

export function parseAgentHostSettings(payload) {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    return { ok: false, error: "expected object" };
  }
  if (payload.agents !== undefined && !Array.isArray(payload.agents)) {
    return { ok: false, error: "agents must be an array" };
  }
  if (payload.profiles !== undefined && !Array.isArray(payload.profiles)) {
    return { ok: false, error: "profiles must be an array" };
  }
  if (payload.env !== undefined && (!payload.env || typeof payload.env !== "object" || Array.isArray(payload.env))) {
    return { ok: false, error: "env must be an object" };
  }
  if (
    payload.config_memory !== undefined &&
    (!payload.config_memory || typeof payload.config_memory !== "object" || Array.isArray(payload.config_memory))
  ) {
    return { ok: false, error: "config_memory must be an object" };
  }
  const copy = cloneJson(payload);
  // 控制协议 v3 要求 `commands: {}` 始终存在，保留既有键并补默认值。
  if (!copy.commands || typeof copy.commands !== "object" || Array.isArray(copy.commands)) {
    copy.commands = {};
  }
  copy.hooks_enabled = copy.hooks_enabled !== false;
  copy.env = copy.env ?? {};
  copy.config_memory = copy.config_memory ?? {};
  copy.profiles = (copy.profiles ?? []).map((p) => ({
    ...p,
    command: typeof p.command === "string" ? p.command : "",
    env: p.env && typeof p.env === "object" && !Array.isArray(p.env) ? p.env : {},
    workspace_dir: typeof p.workspace_dir === "string" ? p.workspace_dir : "",
  }));
  copy.agents = (copy.agents ?? []).map((a) => ({
    ...a,
    id: typeof a.id === "string" ? a.id : "",
    name: typeof a.name === "string" ? a.name : "",
    description: typeof a.description === "string" ? a.description : "",
    engine_kind: typeof a.engine_kind === "string" ? a.engine_kind : typeof a.agent_id === "string" ? a.agent_id : "",
    prompt: typeof a.prompt === "string" ? a.prompt : "",
    plugins: Array.isArray(a.plugins) ? a.plugins : [],
    mcp_servers: Array.isArray(a.mcp_servers) ? a.mcp_servers : [],
    context_folders: Array.isArray(a.context_folders) ? a.context_folders : [],
    context_links: Array.isArray(a.context_links) ? a.context_links : [],
    model_provider: typeof a.model_provider === "string" ? a.model_provider : "",
    model_id: typeof a.model_id === "string" ? a.model_id : "",
    memory_enabled: a.memory_enabled !== false,
  }));
  copy.default_conversation_agent =
    typeof copy.default_conversation_agent === "string" && copy.default_conversation_agent.length > 0
      ? copy.default_conversation_agent
      : null;
  if (copy.agent_spaces_root == null) {
    delete copy.agent_spaces_root;
  }
  return { ok: true, settings: copy };
}

export function agentEngineId(agent) {
  if (!agent) return "";
  return typeof agent.engine_kind === "string"
    ? agent.engine_kind
    : typeof agent.agent_id === "string"
      ? agent.agent_id
      : "";
}

export function isAgentConversationReady(agent, engineIds) {
  if (!agent || typeof agent.id !== "string" || agent.id.trim().length === 0) return false;
  const eng = agentEngineId(agent);
  return Array.isArray(engineIds) && engineIds.includes(eng);
}

export function newAgentDefinition(id, name, engineKindId) {
  return {
    id,
    name,
    description: "",
    engine_kind: engineKindId,
    prompt: "",
    plugins: [],
    mcp_servers: [],
    context_folders: [],
    context_links: [],
    model_provider: "",
    model_id: "",
    memory_enabled: true,
  };
}

export function removeAgent(settings, id) {
  settings.agents = (settings.agents ?? []).filter((a) => a.id !== id);
  if (settings.default_conversation_agent === id) {
    settings.default_conversation_agent = null;
  }
}

export function agentModelLabel(agent, inheritLabel) {
  const provider = (agent?.model_provider ?? "").trim();
  const modelId = (agent?.model_id ?? "").trim();
  if (!provider || !modelId) return inheritLabel;
  return `${provider} / ${modelId}`;
}

// ---- 每台机器串行配置编辑队列 ----

export function createEditQueue({ getCurrent, getUnavailable, notReceivedMessage, sendEdit }) {
  /** @type {Map<string, Array<{ mutate: (draft: any) => void, resolve: (v: any) => void, reject: (e: Error) => void }>>} */
  const queues = new Map();

  async function drain(key, machine) {
    let last = null;
    while (true) {
      const queue = queues.get(key);
      if (!queue || queue.length === 0) {
        queues.delete(key);
        return;
      }
      const job = queue.shift();
      try {
        const before = last ? cloneJson(last) : getCurrent(machine);
        if (!before) {
          const reason = getUnavailable(machine) ?? notReceivedMessage();
          throw new Error(reason);
        }
        const after = cloneJson(before);
        job.mutate(after);
        if (stableEqual(before, after)) {
          last = before;
          job.resolve(before);
          continue;
        }
        const applied = await sendEdit(machine, before, after);
        last = applied ?? after;
        job.resolve(last);
      } catch (err) {
        job.reject(err instanceof Error ? err : new Error(String(err)));
      }
    }
  }

  return function enqueue(machine, mutate) {
    const key = machineKey(machine);
    return new Promise((resolve, reject) => {
      let queue = queues.get(key);
      const idle = !queue;
      if (!queue) {
        queue = [];
        queues.set(key, queue);
      }
      queue.push({ mutate, resolve, reject });
      if (idle) {
        void drain(key, machine);
      }
    });
  };
}

// ---- 自动化投影只读缓存（数量统计、删除阻断说明与详情页「自动化」页签） ----

const SCHEDULE_ALL_DAYS_MASK = 0x7f;
const SCHEDULE_WEEKDAYS_MASK = 0x1f;

export function createAutomationsProjection() {
  return new Map();
}

export function applyAutomationsPayload(projection, machine, payload) {
  const key = machineKey(machine);
  if (!payload || typeof payload !== "object") return false;
  const data = payload.data;
  if (data == null) {
    return projection.delete(key);
  }
  if (typeof data !== "object") return false;
  const storeId = typeof data.store_id === "string" ? data.store_id : "";
  const revision = typeof data.revision === "number" ? data.revision : 0;
  const current = projection.get(key);
  const automations = Array.isArray(data.automations) ? data.automations : [];
  const states = Array.isArray(data.states) ? data.states : [];
  const runs = Array.isArray(data.runs) ? data.runs : [];
  if (current) {
    if (storeId && current.retiredStoreIds.has(storeId)) return false;
    if (storeId === current.storeId && revision <= current.revision) return false;
    const retired = new Set(current.retiredStoreIds);
    if (current.storeId && current.storeId !== storeId) {
      retired.add(current.storeId);
    }
    projection.set(key, {
      storeId,
      revision,
      retiredStoreIds: retired,
      automations,
      states,
      runs,
    });
    return true;
  }
  projection.set(key, {
    storeId,
    revision,
    retiredStoreIds: new Set(),
    automations,
    states,
    runs,
  });
  return true;
}

export function retainAutomationsMachines(projection, liveMachineKeys) {
  let changed = false;
  for (const key of [...projection.keys()]) {
    if (!liveMachineKeys.has(key)) {
      projection.delete(key);
      changed = true;
    }
  }
  return changed;
}

export function automationsForAgent(projection, machine, agentId) {
  const entry = projection.get(machineKey(machine));
  if (!entry) return [];
  return entry.automations.filter(
    (a) =>
      a?.action &&
      (a.action.type === "agent" || a.action.kind === "agent") &&
      a.action.agent_definition_id === agentId,
  );
}

export function automationNextRunAt(projection, machine, automationId) {
  const entry = projection.get(machineKey(machine));
  if (!entry) return null;
  const st = (entry.states ?? []).find((s) => s?.automation_id === automationId);
  return typeof st?.next_run_at === "number" ? st.next_run_at : null;
}

export function automationRunsForAgent(projection, machine, agentId, limit = 5) {
  const entry = projection.get(machineKey(machine));
  if (!entry) return [];
  const mineIds = new Set(automationsForAgent(projection, machine, agentId).map((a) => String(a.id)));
  if (mineIds.size === 0) return [];
  return (entry.runs ?? [])
    .filter((r) => mineIds.has(String(r?.automation_id ?? "")))
    .slice()
    .sort((a, b) => {
      const ta = a?.finished_at ?? a?.created_at ?? 0;
      const tb = b?.finished_at ?? b?.created_at ?? 0;
      if (tb !== ta) return tb - ta;
      return String(b?.id ?? "").localeCompare(String(a?.id ?? ""));
    })
    .slice(0, limit);
}

export function automationNamesForAgent(projection, machine, agentId) {
  return automationsForAgent(projection, machine, agentId).map((a) => {
    const trimmed = typeof a?.name === "string" ? a.name.trim() : "";
    return trimmed.length > 0 ? trimmed : null;
  });
}

function clockText(hour, minute) {
  return `${String(hour ?? 0).padStart(2, "0")}:${String(minute ?? 0).padStart(2, "0")}`;
}

function weekdayLabel(index, t) {
  const keys = [
    "automations.weekday.mon",
    "automations.weekday.tue",
    "automations.weekday.wed",
    "automations.weekday.thu",
    "automations.weekday.fri",
    "automations.weekday.sat",
    "automations.weekday.sun",
  ];
  return t(keys[index] ?? keys[6]);
}

function weeklyDaysSummary(days, t) {
  const mask = (days ?? 0) & SCHEDULE_ALL_DAYS_MASK;
  if (mask === SCHEDULE_ALL_DAYS_MASK) return t("automations.schedule.every_day");
  const labels = [];
  for (let i = 0; i < 7; i++) {
    if ((mask & (1 << i)) !== 0) labels.push(weekdayLabel(i, t));
  }
  if (labels.length === 0) return t("automations.schedule.no_days");
  return t("automations.schedule.weekly_days", { days: labels.join(t("delete.name_separator")) });
}

export function formatAutomationTriggerSummary(trigger, t) {
  const parts = [];
  const schedules = Array.isArray(trigger?.schedules) ? trigger.schedules : [];
  for (const raw of schedules) {
    if (!raw || typeof raw !== "object") continue;
    const s =
      raw.type === "weekdays"
        ? { type: "weekly", days: SCHEDULE_WEEKDAYS_MASK, hour: raw.hour, minute: raw.minute }
        : raw;
    if (s.type === "every_minutes") {
      parts.push(t("automations.schedule.every_minutes", { count: s.minutes }));
    } else if (s.type === "every_hours") {
      parts.push(t("automations.schedule.every_hours", { count: s.hours }));
    } else if (s.type === "daily") {
      parts.push(t("automations.schedule.daily", { time: clockText(s.hour, s.minute) }));
    } else if (s.type === "weekly") {
      parts.push(
        t("automations.schedule.weekly", {
          days: weeklyDaysSummary(s.days, t),
          time: clockText(s.hour, s.minute),
        }),
      );
    } else if (s.type === "during") {
      const days = weeklyDaysSummary(s.days === 0 ? SCHEDULE_ALL_DAYS_MASK : s.days, t);
      parts.push(
        t("automations.schedule.during", {
          days,
          start: clockText(s.start_hour, s.start_minute),
          end: clockText(s.end_hour, s.end_minute),
          count: s.minutes,
        }),
      );
    }
  }
  const webhooks = Array.isArray(trigger?.webhooks) ? trigger.webhooks : [];
  for (const wh of webhooks) {
    const hasToken =
      (typeof wh?.secret === "string" && wh.secret.trim().length > 0) ||
      (typeof wh?.endpoint === "string" && wh.endpoint.trim().length > 0);
    parts.push(hasToken ? t("automations.trigger.webhook") : t("automations.trigger.webhook_unset"));
  }
  const events = Array.isArray(trigger?.events) ? trigger.events : [];
  for (const ev of events) {
    if (ev?.topic) parts.push(t("automations.trigger.event", { topic: ev.topic }));
  }
  return parts.length === 0 ? t("automations.trigger.none") : parts.join(t("automations.trigger.separator"));
}

export function formatAutomationRelativeUntil(ts, now, t) {
  const delta = ts - now;
  if (delta <= 0) return t("automations.time.due_now");
  const MINUTE = 60;
  const HOUR = 60 * MINUTE;
  const DAY = 24 * HOUR;
  if (delta < HOUR) {
    return t("automations.time.in_minutes", { count: Math.max(1, Math.trunc(delta / MINUTE)) });
  }
  if (delta < DAY) {
    return t("automations.time.in_hours", { count: Math.trunc(delta / HOUR) });
  }
  return t("automations.time.in_days", { count: Math.trunc(delta / DAY) });
}

export function formatAutomationUnixLocal(tsSeconds) {
  if (typeof tsSeconds !== "number" || !Number.isFinite(tsSeconds)) return String(tsSeconds ?? "");
  const d = new Date(tsSeconds * 1000);
  if (Number.isNaN(d.getTime())) return String(tsSeconds);
  const mm = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  const hh = String(d.getHours()).padStart(2, "0");
  const mi = String(d.getMinutes()).padStart(2, "0");
  return `${mm}-${dd} ${hh}:${mi}`;
}

export function automationRunStatusLabel(status, t) {
  switch (status) {
    case "starting":
    case "queued":
    case "dispatching":
    case "running":
    case "awaiting_approval":
    case "waiting_for_user":
    case "completed":
    case "failed":
    case "cancelled":
    case "skipped":
      return t(`automations.run_status.${status}`);
    default:
      return String(status ?? "");
  }
}

export function automationRunErrorText(run, t) {
  if (!run) return null;
  const detail = typeof run.error === "string" ? run.error.trim() : "";
  if (detail) return excerptText(detail, 96);
  if (run.error_code) return t("automations.run_failed");
  return null;
}

// ---- 新对话决议（与 smelt-model/default_conversation.rs 一致） ----

export function resolveNewConversation(agents, storedDefaultId, draftAgentId, engineIds) {
  const draft = typeof draftAgentId === "string" && draftAgentId.trim() ? draftAgentId.trim() : null;
  const ready = (agents ?? [])
    .filter((a) => isAgentConversationReady(a, engineIds))
    .filter((a) => !draft || a.id !== draft)
    .map((a) => ({ id: a.id, name: a.name ?? "" }));

  const stored =
    typeof storedDefaultId === "string" && storedDefaultId.trim() ? storedDefaultId.trim() : null;
  const found = stored ? ready.find((a) => a.id === stored) ?? null : null;
  const intent = found
    ? { kind: "start", agent_id: found.id }
    : ready.length === 0
      ? { kind: "create_agent" }
      : { kind: "pick_agent" };
  return {
    intent,
    clear_stored_default: stored !== null && found === null,
    default_name: found ? found.name : null,
    ready,
  };
}

export function planNewConversation(orderedMachines, hostSettingsByKey, draftRef, engineIds) {
  let start = null;
  let defaultName = null;
  const ready = [];
  const staleDefaults = [];

  for (const machine of orderedMachines) {
    const key = machineKey(machine);
    const settings = hostSettingsByKey.get(key);
    if (!settings) continue;
    const draftId =
      draftRef && machineKey(draftRef.machine) === key ? draftRef.id : null;
    const res = resolveNewConversation(
      settings.agents,
      settings.default_conversation_agent,
      draftId,
      engineIds,
    );
    if (res.clear_stored_default) {
      staleDefaults.push(machine);
    }
    if (!start && res.intent.kind === "start") {
      start = { machine, id: res.intent.agent_id };
      defaultName = res.default_name;
    }
    for (const choice of res.ready) {
      ready.push({ agent: { machine, id: choice.id }, name: choice.name });
    }
  }

  const intent = start
    ? { kind: "start", agent: start }
    : ready.length === 0
      ? { kind: "create_agent" }
      : { kind: "pick_agent" };

  return {
    intent,
    default_name: defaultName,
    ready,
    stale_defaults: staleDefaults,
  };
}

// ---- 记忆筛选、排序与文案 ----

export function isRuleMemoryKind(kind) {
  return typeof kind === "string" && RULE_MEMORY_TYPES.includes(kind);
}

export function sessionPhaseSettled(phase) {
  return phase !== "connecting" && phase !== "thinking" && phase !== "executing_tool";
}

function systemTimePair(value) {
  if (value == null) return null;
  if (typeof value === "number" && Number.isFinite(value)) {
    return [value, 0];
  }
  if (typeof value === "object" && typeof value.secs_since_epoch === "number") {
    return [value.secs_since_epoch, typeof value.nanos_since_epoch === "number" ? value.nanos_since_epoch : 0];
  }
  return null;
}

export function compareSystemTimeDesc(a, b) {
  const pa = systemTimePair(a);
  const pb = systemTimePair(b);
  if (pa === null && pb === null) return 0;
  if (pa === null) return 1;
  if (pb === null) return -1;
  if (pb[0] !== pa[0]) return pb[0] - pa[0];
  return pb[1] - pa[1];
}

export function matchesMemoryFilter(entry, filter) {
  const rule = isRuleMemoryKind(entry?.kind);
  switch (filter) {
    case "pending":
      return entry?.pending_reason != null;
    case "rules":
      return rule;
    case "facts":
      return !rule;
    case "all":
    default:
      return true;
  }
}

export function visibleMemories(entries, query, filter) {
  const q = (query ?? "").trim().toLowerCase();
  const list = (entries ?? [])
    .filter((entry) => matchesMemoryFilter(entry, filter))
    .filter((entry) => {
      if (!q) return true;
      return [entry.name, entry.description, entry.file_name, entry.body].some(
        (field) => typeof field === "string" && field.toLowerCase().includes(q),
      );
    });

  return [...list].sort((a, b) => {
    const ap = a.pending_reason != null ? 1 : 0;
    const bp = b.pending_reason != null ? 1 : 0;
    if (bp !== ap) return bp - ap;
    const tCmp = compareSystemTimeDesc(a.last_used, b.last_used);
    if (tCmp !== 0) return tCmp;
    const an = (a.name ?? "").toLowerCase();
    const bn = (b.name ?? "").toLowerCase();
    if (an < bn) return -1;
    if (an > bn) return 1;
    return 0;
  });
}

export function pendingReasonText(reason, t) {
  if (!reason) return "";
  if (reason.startsWith("external_content:")) {
    return t("memory.pending.external_content", { sources: reason.slice("external_content:".length) });
  }
  const keyMap = {
    unattended: "memory.pending.unattended",
    unknown_origin: "memory.pending.unknown_origin",
    edits_pending: "memory.pending.edits_pending",
    rule_lacks_user_words: "memory.pending.rule_lacks_user_words",
    no_provenance: "memory.pending.no_provenance",
    modified_outside: "memory.pending.modified_outside",
    legacy_backfill: "memory.pending.legacy_backfill",
  };
  return keyMap[reason] ? t(keyMap[reason]) : reason;
}

export function memorySummaryText(agentCount, userCount, t) {
  if (agentCount + userCount === 0) return t("memory.summary_empty");
  return t("memory.summary", { agent: agentCount, user: userCount });
}

export function memoryPendingCount(memories) {
  const a = (memories?.agent?.entries ?? []).filter((e) => e.pending_reason != null).length;
  const u = (memories?.user?.entries ?? []).filter((e) => e.pending_reason != null).length;
  return a + u;
}

// ---- Pi 能力与模型 ----

export function splitPluginId(id) {
  if (typeof id !== "string") return null;
  if (id.startsWith("skill:")) return { kind: "skill", name: id.slice("skill:".length) };
  if (id.startsWith("extension:")) return { kind: "extension", name: id.slice("extension:".length) };
  return null;
}

export function extractPiMcpServers(piConfig) {
  if (!piConfig) return [];
  const raw = piConfig.mcp_servers;
  if (Array.isArray(raw)) return raw;
  if (raw && typeof raw === "object" && Array.isArray(raw.Ok)) return raw.Ok;
  return [];
}

export function buildCapabilitiesList(agent, piConfig, piLoaded, t) {
  const plugins = Array.isArray(piConfig?.plugins) ? piConfig.plugins : [];
  const mcpServers = extractPiMcpServers(piConfig);
  const selectedPlugins = new Set(agent?.plugins ?? []);
  const selectedMcps = new Set(agent?.mcp_servers ?? []);
  const rows = [];

  for (const plugin of plugins) {
    rows.push({
      key: `plugin:${plugin.id}`,
      pick: "plugin",
      id: plugin.id,
      name: plugin.name,
      kindLabel: plugin.kind === "extension" ? t("plugins.kind.extension") : t("plugins.kind.skill"),
      description: plugin.description ?? "",
      origin: plugin.origin ?? "",
      broken: plugin.broken ?? null,
      checked: selectedPlugins.has(plugin.id),
    });
  }

  for (const server of mcpServers) {
    let detail = "";
    if (server.transport?.type === "http") {
      detail = server.transport.url ?? "";
    } else if (server.transport?.type === "stdio") {
      detail = `${server.transport.command ?? ""} ${(server.transport.args ?? []).join(" ")}`.trim();
    }
    rows.push({
      key: `mcp:${server.name}`,
      pick: "mcp",
      id: server.name,
      name: server.name,
      kindLabel: "MCP",
      description: server.description ?? "",
      origin: detail,
      broken: server.enabled === false ? t("mcp.disabled") : null,
      checked: selectedMcps.has(server.name),
    });
  }

  if (piLoaded) {
    const knownPluginIds = new Set(plugins.map((p) => p.id));
    for (const id of agent?.plugins ?? []) {
      if (knownPluginIds.has(id)) continue;
      const parts = splitPluginId(id);
      rows.push({
        key: `plugin:${id}`,
        pick: "plugin",
        id,
        name: parts ? parts.name : id,
        kindLabel: parts
          ? parts.kind === "extension"
            ? t("plugins.kind.extension")
            : t("plugins.kind.skill")
          : t("pick.unknown"),
        description: "",
        origin: "",
        broken: t("plugins.missing"),
        checked: true,
      });
    }
    const knownMcpNames = new Set(mcpServers.map((s) => s.name));
    for (const name of agent?.mcp_servers ?? []) {
      if (knownMcpNames.has(name)) continue;
      rows.push({
        key: `mcp:${name}`,
        pick: "mcp",
        id: name,
        name,
        kindLabel: "MCP",
        description: "",
        origin: "",
        broken: t("mcp.missing"),
        checked: true,
      });
    }
  }

  return rows;
}

export function filterCapabilities(rows, query) {
  const q = (query ?? "").trim().toLowerCase();
  if (!q) return rows;
  return rows.filter((row) =>
    [row.name, row.kindLabel, row.description, row.origin, row.broken ?? ""].some(
      (field) => typeof field === "string" && field.toLowerCase().includes(q),
    ),
  );
}

// ---- 环境变量与自定义 Agent Profile 校验 ----

export function isEnvName(name) {
  return typeof name === "string" && /^[A-Za-z_][A-Za-z0-9_]*$/.test(name);
}

export function parseEnvLines(text, t) {
  const env = {};
  const lines = String(text ?? "").split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].trim();
    if (!line || line.startsWith("#")) continue;
    const number = i + 1;
    const eq = line.indexOf("=");
    if (eq < 0) {
      return { ok: false, error: t("env.missing_equals", { line: number, text: line }) };
    }
    const name = line.slice(0, eq).trim();
    const value = line.slice(eq + 1).trim();
    if (!isEnvName(name)) {
      return { ok: false, error: t("env.invalid_name", { line: number, name }) };
    }
    if (LAUNCH_ONLY_ENV.includes(name)) {
      return { ok: false, error: t("env.launch_only", { line: number, name }) };
    }
    if (Object.prototype.hasOwnProperty.call(env, name)) {
      return { ok: false, error: t("env.duplicate", { line: number, name }) };
    }
    env[name] = value;
  }
  return { ok: true, env };
}

export function formatEnvLines(env) {
  if (!env || typeof env !== "object") return "";
  return Object.keys(env)
    .sort()
    .map((k) => `${k}=${env[k] ?? ""}`)
    .join("\n");
}

export function profileKindMeta(kindId, conversationAgents) {
  if (kindId === "custom") {
    return { id: "custom", isCustom: true, label: "Custom", short_label: "Custom", config_dir_env_var: null };
  }
  const found = (conversationAgents ?? []).find((a) => a.id === kindId);
  return found ? { ...found, isCustom: false } : null;
}

export function validateProfile(profile, conversationAgents, t) {
  const kind = profileKindMeta(profile?.kind_id, conversationAgents);
  if (!kind) {
    return t("custom_agent.unknown_kind", { name: profile?.label ?? "", kind: profile?.kind_id ?? "" });
  }
  if (!profile?.label?.trim()) {
    return t("custom_agent.name_required");
  }
  if (kind.isCustom && !profile?.command?.trim()) {
    return t("custom_agent.command_required", { name: profile.label });
  }
  const dir = (profile?.workspace_dir ?? "").trim();
  if (dir && !kind.config_dir_env_var) {
    return t("custom_agent.data_dir_unsupported", { agent: kind.label });
  }
  const env = profile?.env ?? {};
  if (kind.config_dir_env_var && Object.prototype.hasOwnProperty.call(env, kind.config_dir_env_var)) {
    return t("custom_agent.data_dir_in_env", { var: kind.config_dir_env_var });
  }
  for (const name of Object.keys(env)) {
    if (LAUNCH_ONLY_ENV.includes(name) || !isEnvName(name)) {
      return t("custom_agent.env_not_allowed", { name });
    }
  }
  return null;
}

export function launchEntryIsBuiltin(entry, terminalAgentIds) {
  return Boolean(entry?.provider && Array.isArray(terminalAgentIds) && terminalAgentIds.includes(entry.provider));
}

// ---- 展示文案与统计 ----

export function isCjkChar(ch) {
  const cp = ch.codePointAt(0) ?? 0;
  return cp >= 0x4e00 && cp <= 0x9fff;
}

export function agentCatalogMonogram(name, t) {
  const trimmed = (name ?? "").trim();
  const chars = [...trimmed];
  if (chars.length === 0) return t("monogram.empty");
  const first = chars[0];
  if (isCjkChar(first)) {
    return chars.length > 1 && isCjkChar(chars[1]) ? `${first}${chars[1]}` : first;
  }
  let word = first;
  for (let i = 1; i < chars.length; i++) {
    const ch = chars[i];
    if ([...word].length >= 2 || /\s/.test(ch) || isCjkChar(ch)) break;
    if (/^[A-Za-z0-9]$/.test(ch)) word += ch;
    else break;
  }
  return word;
}

export function excerptText(value, limit) {
  const compact = String(value ?? "").trim().replace(/\s+/g, " ");
  const chars = [...compact];
  if (chars.length <= limit) return compact;
  return `${chars.slice(0, limit).join("")}…`;
}

export function agentDisplayName(name, t) {
  const trimmed = (name ?? "").trim();
  return trimmed.length > 0 ? trimmed : t("agent.unnamed");
}

export function newConversationLabel(defaultName, t) {
  if (defaultName != null) {
    return t("new_conversation.named", { name: agentDisplayName(defaultName, t) });
  }
  return t("new_conversation.title");
}

export function agentCardPrompt(prompt) {
  const compact = excerptText(prompt, 64);
  return compact.length > 0 ? compact : null;
}

export function agentInstructionsFooter(prompt, t) {
  const count = [...String(prompt ?? "")].length;
  if (count === 0) return t("instructions.footer");
  return t("instructions.footer_count", { count });
}

export function agentConversationStartError(agent, engineIds, t) {
  const eng = agentEngineId(agent);
  if (!eng || !Array.isArray(engineIds) || !engineIds.includes(eng)) {
    return t("error.no_engine");
  }
  return null;
}

export function agentUsageSummary(conversations, automations, t) {
  const convText =
    conversations === 1
      ? t("usage.conversations.one")
      : t("usage.conversations.other", { count: conversations });
  const autoText =
    automations === 1
      ? t("usage.automations.one")
      : t("usage.automations.other", { count: automations });
  if (conversations === 0 && automations === 0) return t("usage.none");
  if (automations === 0) return convText;
  if (conversations === 0) return autoText;
  return `${convText} · ${autoText}`;
}

export function agentConversationCount(directory, agentId) {
  const convs = directory?.conversations ?? [];
  let count = 0;
  for (const conv of convs) {
    if (conv?.archived_at != null) continue;
    const acp = conv?.acp;
    if (acp && acp.automation_id == null && acp.agent_definition_id === agentId) {
      count += 1;
    }
  }
  return count;
}

export function cliExecutableForAgent(diagnostics, agentId) {
  if (!diagnostics || typeof diagnostics !== "object") return null;
  return diagnostics.acp?.[agentId] ?? diagnostics.terminal?.[agentId] ?? null;
}

export function isCliExecutableAvailable(executable) {
  return Boolean(executable && typeof executable.path === "string" && executable.path.length > 0);
}

export function formatClockHHMM(date) {
  const hh = String(date.getHours()).padStart(2, "0");
  const mm = String(date.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
}

export function sortMachines(machines) {
  return [...(machines ?? [])].sort((a, b) => {
    const al = a.machine == null ? 0 : 1;
    const bl = b.machine == null ? 0 : 1;
    if (al !== bl) return al - bl;
    return String(a.name ?? "").localeCompare(String(b.name ?? ""));
  });
}
