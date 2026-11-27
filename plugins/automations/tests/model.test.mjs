import { Database } from "bun:sqlite";
import { describe, expect, test } from "bun:test";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import * as contract from "../contract.js";
import {
  BUILTIN_AUTOMATION_ACTIONS,
  BUILTIN_AUTOMATION_TRIGGER_PRESETS,
  BUILTIN_TEMPLATES,
  DELIVERED_STORE_MIGRATIONS,
  NOTIFY_CHOICES,
  REDACTED_WEBHOOK_ENDPOINT,
  SCHEDULE_ALL_DAYS_MASK,
  SCHEDULE_DAY_FRI,
  SCHEDULE_DAY_MON,
  SCHEDULE_DAY_WED,
  SCHEDULE_WEEKDAYS_MASK,
  TEMPLATE_CATEGORIES,
  TOOL_OUTPUT_MAX_LINES,
  DeliveredStore,
  actionKindLabel,
  agentDisplayName,
  applyAutomationsSnapshot,
  automationMissingFields,
  automationPromptHint,
  automationRowStatus,
  automationRowStatusLabel,
  automationRunNotifications,
  automationSaveHint,
  automationTemplate,
  buildAutomationWireFromDraft,
  buildDraftFromAutomation,
  buildRunDetail,
  classifyAcpEntry,
  classifyToolOutputPart,
  clockInputIsEditable,
  compactToolHeadline,
  countFailingAutomations,
  createEmptyMachineAutomations,
  createTriggerEntryDraft,
  decodeAutomationsPayload,
  deleteConfirmDescription,
  deliveredKey,
  describeAutomationRunFailure,
  editorLanguageForPath,
  effectiveProjectionOrigin,
  estimateDiffHeight,
  fencedBlock,
  filteredTemplates,
  finalReplyIndex,
  formatElapsed,
  formatLastRun,
  formatNextRunCell,
  formatRelativeSince,
  formatRelativeUntil,
  intervalTitle,
  missingFieldLabel,
  newSessionLabel,
  newWebhookSecret,
  normalizeSchedule,
  notificationExcerpt,
  notificationId,
  notifyFromSinks,
  notifyHint,
  notifyLabel,
  notifyShouldFire,
  notifyToSinks,
  replaceMachineSnapshot,
  runErrorText,
  runMetadataRows,
  runSummaryInfo,
  scheduleFromValues,
  scheduleSummary,
  taskNoteSummary,
  templateCategoryLabel,
  templatePlaceholders,
  toolProcessIconPath,
  transcriptSummary,
  triggerFromDraft,
  triggerPresetHint,
  triggerPresetIsAdvanced,
  triggerPresetKind,
  triggerPresetLabel,
  triggerPresetSchedule,
  triggerSummary,
  turnFailedText,
  webhookCurlExample,
  webhookUrl,
} from "../model.js";

// ---- 读取真实 locales/ui.yml，构造与运行时一致的 t(key, vars) ----

const LOCALES_PATH = resolve(import.meta.dir, "../locales/ui.yml");
const LOCALES_DATA = Bun.YAML.parse(readFileSync(LOCALES_PATH, "utf8"));

function flattenLocaleMap(node, prefix = "", out = new Map()) {
  if (!node || typeof node !== "object") return out;
  for (const [k, v] of Object.entries(node)) {
    if (k === "_version") continue;
    const path = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === "object" && !Array.isArray(v)) {
      if ("en" in v || "zh-CN" in v) {
        out.set(path, v);
      } else {
        flattenLocaleMap(v, path, out);
      }
    }
  }
  return out;
}

const LOCALE_ENTRIES = flattenLocaleMap(LOCALES_DATA);

function makeT(lang = "en") {
  return (key, vars = {}) => {
    const entry = LOCALE_ENTRIES.get(key);
    if (!entry) throw new Error(`Missing locale key: ${key}`);
    const raw = entry[lang] ?? entry.en;
    if (typeof raw !== "string") throw new Error(`Missing locale text for ${key} (${lang})`);
    return raw.replace(/%\{([^}]+)\}/g, (_, name) =>
      name in vars ? String(vars[name]) : `%{${name}}`,
    );
  };
}

const tEn = makeT("en");
const tZh = makeT("zh-CN");

// 运行时规则（SDK）在 Rust 那边测；这里只测表单换算和文案，规则一律放行。
const NO_PROBLEM = { scheduleProblem: () => null, triggerProblem: () => null };

function makeTestSqliteDb() {
  const db = new Database(":memory:");
  return {
    raw: db,
    async migrate(migrations) {
      db.exec(
        "CREATE TABLE IF NOT EXISTS _smelt_migrations (version INTEGER PRIMARY KEY)",
      );
      const row = db.query("SELECT COALESCE(MAX(version), 0) AS v FROM _smelt_migrations").get();
      const current = Number(row?.v ?? 0);
      for (let i = current; i < migrations.length; i++) {
        db.exec(migrations[i]);
        db.query("INSERT INTO _smelt_migrations (version) VALUES (?1)").run(i + 1);
      }
    },
    async query(sql, params = []) {
      return db.query(sql).all(...params);
    },
    async execute(sql, params = []) {
      db.query(sql).run(...params);
    },
  };
}

function makeRun(overrides = {}) {
  return {
    id: "run-1",
    automation_id: "automation-1",
    context: {
      automation_name: "日报汇总",
      action: { type: "shell", command: "echo done", args: [] },
      cwd: "/tmp",
      trigger_payload: null,
      prompt: null,
      agent_definition_name: null,
      engine_kind_id: null,
      agent_instructions: null,
    },
    source: "scheduled",
    scheduled_for: 10,
    status: "running",
    created_at: 10,
    started_at: 11,
    delivery_attempt_at: null,
    delivery_attempts: 0,
    finished_at: null,
    session_id: null,
    provider_session_id: null,
    output: null,
    error_code: null,
    error: null,
    runtime_released_at: null,
    ...overrides,
  };
}

function makeAutomation(id, sinks = [{ type: "local" }], extra = {}) {
  return {
    id,
    name: id,
    enabled: true,
    workspace_dir: null,
    trigger: { schedules: [], webhooks: [], events: [] },
    action: { type: "shell", command: "true", args: [] },
    sinks,
    ...extra,
  };
}

function makeFile(revision, automations = [], runs = [], extra = {}) {
  return {
    schema_version: 2,
    store_id: "store",
    revision,
    timezone_fingerprint: "",
    automations,
    states: [],
    runs,
    store_error: null,
    webhook_base_url: null,
    ...extra,
  };
}

// ---- 1. 跨插件契约与翻译完整性 ----

describe("contract & locales", () => {
  test("wire contract constants match Rust smelt.automations plugin", () => {
    expect(contract.PLUGIN_ID).toBe("smelt.automations");
    expect(contract.VIEW).toBe("smelt.automations/main");
    expect(contract.TOPIC_AUTOMATIONS_CHANGED).toBe("automations.changed");
    expect(contract.AGENTS_VIEW).toBe("smelt.agents/main");
    expect(contract.AGENT_SETTINGS_CMD).toBe("smelt.agents/agent-settings");
    expect(contract.AGENT_SETTINGS_CHANGED_EVENT).toBe("smelt.agents/agent-settings-changed");
    expect(contract.OPEN_VIEW_CMD).toBe("smelt.app/open-view");
    expect(contract.NOTIFY_CMD).toBe("smelt.app/notify");
    expect(contract.WITHDRAW_NOTIFICATION_CMD).toBe("smelt.app/withdraw-notification");
    expect(contract.SET_VIEW_BADGE_CMD).toBe("smelt.app/set-view-badge");
    expect(contract.PICK_PATHS_CMD).toBe("smelt.app/pick-paths");
  });

  test("locales/ui.yml has complete en and zh-CN parity", () => {
    expect(LOCALES_DATA._version).toBe(2);
    expect(LOCALE_ENTRIES.size).toBeGreaterThan(150);
    for (const [key, entry] of LOCALE_ENTRIES.entries()) {
      expect(typeof entry.en, `${key}.en`).toBe("string");
      expect(typeof entry["zh-CN"], `${key}.zh-CN`).toBe("string");
      expect(entry.en.length, `${key}.en non-empty`).toBeGreaterThan(0);
      expect(entry["zh-CN"].length, `${key}.zh-CN non-empty`).toBeGreaterThan(0);
    }
  });
});

// ---- 2. Webhook Secret ----

describe("webhook secret", () => {
  test("newWebhookSecret formats UUID v4 as 32-char lowercase hex", () => {
    const fixed = newWebhookSecret(() => "550E8400-E29B-41D4-A716-446655440000");
    expect(fixed).toBe("550e8400e29b41d4a716446655440000");
    const real = newWebhookSecret(randomUUID);
    expect(real).toMatch(/^[0-9a-f]{32}$/);
    expect(() => newWebhookSecret(null)).toThrow();
  });

  test("webhookUrl strips trailing slash and webhookCurlExample is single-line copyable", () => {
    expect(webhookUrl("http://127.0.0.1:17827/", "tok")).toBe("http://127.0.0.1:17827/hooks/tok");
    const curl = webhookCurlExample("http://127.0.0.1:17827/hooks/tok", tEn);
    expect(curl.startsWith("curl -sS -X POST http://127.0.0.1:17827/hooks/tok ")).toBe(true);
    expect(curl.includes('{"text":"Hello"}')).toBe(true);
    expect(curl.includes("\n")).toBe(false);
  });
});

// ---- 3. SQLite DeliveredStore（与 crates/plugins/automations/src/store.rs 一致） ----

describe("DeliveredStore (SQLite)", () => {
  test("preserves exact delivered(machine, run_id) schema and keeps local and remote apart", async () => {
    const db = makeTestSqliteDb();
    const store = new DeliveredStore(db);
    await store.open();

    // 验证表结构与原 Rust SQLite 迁移完全一致
    const cols = db.raw.query("PRAGMA table_info(delivered)").all();
    expect(cols.map((c) => [c.name, c.type, c.notnull, c.pk])).toEqual([
      ["machine", "TEXT", 1, 1],
      ["run_id", "TEXT", 1, 2],
    ]);
    expect(DELIVERED_STORE_MIGRATIONS.length).toBe(1);

    await store.add(null, "r1");
    await store.add(null, "r1"); // 幂等
    await store.add("rt", "r1");
    expect(await store.delivered()).toEqual(
      new Set([deliveredKey(null, "r1"), deliveredKey("rt", "r1")]),
    );

    await store.remove(null, "r1");
    expect(await store.delivered()).toEqual(new Set([deliveredKey("rt", "r1")]));
  });
});

// ---- 4. 投影缓存与单调性 ----

describe("projection cache & decoding", () => {
  test("decodeAutomationsPayload validates envelope and handles null data", () => {
    expect(decodeAutomationsPayload({ revision: 1, data: null })).toEqual({ ok: true, value: null });
    expect(decodeAutomationsPayload("bad").ok).toBe(false);
    expect(decodeAutomationsPayload({ data: {} }).ok).toBe(false);
    const decoded = decodeAutomationsPayload({
      revision: 3,
      data: makeFile(3, [makeAutomation("a")]),
    });
    expect(decoded.ok).toBe(true);
    expect(decoded.value.revision).toBe(3);
    expect(decoded.value.automations.length).toBe(1);
  });

  test("replaceMachineSnapshot enforces revision monotonicity and retires old store_id", () => {
    const state = createEmptyMachineAutomations();
    expect(replaceMachineSnapshot(state, makeFile(2, [makeAutomation("a")], [], { store_id: "s1" }))).toBe(true);
    // 拒绝旧版本
    expect(replaceMachineSnapshot(state, makeFile(1, [], [], { store_id: "s1" }))).toBe(false);
    // 同版本同内容忽略
    expect(replaceMachineSnapshot(state, makeFile(2, [makeAutomation("a")], [], { store_id: "s1" }))).toBe(false);
    // 同版本但 webhook_base_url 补齐时接受
    expect(
      replaceMachineSnapshot(
        state,
        makeFile(2, [makeAutomation("a")], [], { store_id: "s1", webhook_base_url: "http://127.0.0.1:9999" }),
      ),
    ).toBe(true);
    expect(state.webhook_base_url).toBe("http://127.0.0.1:9999");

    // 切换新 store_id：旧 store_id 退休
    expect(replaceMachineSnapshot(state, makeFile(1, [], [], { store_id: "s2" }))).toBe(true);
    expect(state.store_id).toBe("s2");
    expect(replaceMachineSnapshot(state, makeFile(99, [], [], { store_id: "s1" }))).toBe(false);
  });
});

// ---- 5. 通知边沿检测与投影应用（notifications.rs + projection.rs） ----

describe("notifications & projection application", () => {
  test("shell completion emits one notification and ignores runtime release", () => {
    const running = makeRun({ status: "running" });
    const completed = { ...running, status: "completed", finished_at: 12, output: "42 files updated" };

    const notes = automationRunNotifications([running], [completed], "incremental", tEn);
    expect(notes.length).toBe(1);
    expect(notes[0].kind).toBe("success");
    expect(notes[0].run_id).toBe("run-1");
    expect(notes[0].subtitle).toContain("日报汇总");
    expect(notes[0].body).toContain("42 files updated");

    const released = { ...completed, runtime_released_at: 13 };
    expect(
      automationRunNotifications([makeRun({ status: "completed" })], [released], "incremental", tEn),
    ).toEqual([]);
  });

  test("agent completion notifies on the run rather than hidden session", () => {
    const running = makeRun({
      session_id: "acp-automation-hidden",
      context: {
        ...makeRun().context,
        action: { type: "agent", agent_definition_id: "agent-1", prompt: "生成日报" },
      },
    });
    const completed = { ...running, status: "completed", finished_at: 12 };
    const notes = automationRunNotifications([running], [completed], "incremental", tEn);
    expect(notes.length).toBe(1);
    expect(notes[0].automation_id).toBe("automation-1");
    expect(notes[0].run_id).toBe("run-1");
  });

  test("initial snapshot suppresses replay while same-store reconnect preserves edges", () => {
    const completed = makeRun({ status: "completed", finished_at: 12 });
    expect(automationRunNotifications([], [completed], "initial_snapshot", tEn)).toEqual([]);

    expect(effectiveProjectionOrigin("initial_snapshot", "store-1", "store-1")).toBe("incremental");
    expect(effectiveProjectionOrigin("initial_snapshot", "", "store-1")).toBe("initial_snapshot");
    expect(effectiveProjectionOrigin("initial_snapshot", "retired", "new")).toBe("initial_snapshot");

    const running = makeRun({ status: "running" });
    const notes = automationRunNotifications(
      [running],
      [completed],
      effectiveProjectionOrigin("initial_snapshot", "store-1", "store-1"),
      tEn,
    );
    expect(notes.length).toBe(1);
  });

  test("immediate terminal failures and skipped runs notify as failure; cancelled stays quiet", () => {
    const failed = makeRun({ status: "failed", finished_at: 12, error: "command exited with status 2" });
    const failNotes = automationRunNotifications([], [failed], "incremental", tEn);
    expect(failNotes.length).toBe(1);
    expect(failNotes[0].kind).toBe("failure");
    expect(failNotes[0].body).toContain("status 2");

    const skipped = makeRun({ status: "skipped", finished_at: 12, error_code: "previous_run_active" });
    const skipNotes = automationRunNotifications([], [skipped], "incremental", tEn);
    expect(skipNotes.length).toBe(1);
    expect(skipNotes[0].kind).toBe("failure");
    expect(skipNotes[0].body).toContain("previous run");

    expect(
      automationRunNotifications(
        [makeRun({ status: "running" })],
        [makeRun({ status: "cancelled" })],
        "incremental",
        tEn,
      ),
    ).toEqual([]);
  });

  test("applyAutomationsSnapshot prefixes remote machine, filters sinks, and counts failing badge", () => {
    const map = new Map();
    const auto = makeAutomation("a", [{ type: "local" }]);
    const first = applyAutomationsSnapshot(
      map,
      "rt",
      makeFile(1, [auto], [makeRun({ id: "r", automation_id: "a", status: "running" })]),
      "initial_snapshot",
      "工作站",
      tEn,
    );
    expect(first.notify).toEqual([]);

    const second = applyAutomationsSnapshot(
      map,
      "rt",
      makeFile(2, [auto], [makeRun({ id: "r", automation_id: "a", status: "completed" })]),
      "incremental",
      "工作站",
      tEn,
    );
    expect(second.notify.length).toBe(1);
    expect(second.notify[0].subtitle.startsWith("工作站 · ")).toBe(true);
    expect(second.runs).toEqual(new Set(["r"]));

    // 同版本无变化返回 null
    expect(
      applyAutomationsSnapshot(
        map,
        "rt",
        makeFile(2, [auto], [makeRun({ id: "r", automation_id: "a", status: "completed" })]),
        "incremental",
        "工作站",
        tEn,
      ),
    ).toBeNull();

    // Off 静音但仍计入失败角标
    const offMap = new Map();
    const offAuto = makeAutomation("a", [{ type: "off" }]);
    expect(notifyFromSinks(offAuto.sinks)).toBe("never");
    applyAutomationsSnapshot(
      offMap,
      null,
      makeFile(1, [offAuto], [makeRun({ id: "r", automation_id: "a", status: "running" })]),
      "initial_snapshot",
      null,
      tEn,
    );
    const offDone = applyAutomationsSnapshot(
      offMap,
      null,
      makeFile(2, [offAuto], [makeRun({ id: "r", automation_id: "a", status: "failed" })]),
      "incremental",
      null,
      tEn,
    );
    expect(offDone.notify).toEqual([]);
    expect(offDone.silenced).toEqual(["r"]);
    expect(countFailingAutomations(offMap)).toBe(1);

    // FailuresOnly 仅弹失败
    const foMap = new Map();
    const foAuto = makeAutomation("a", notifyToSinks("failures_only"));
    applyAutomationsSnapshot(
      foMap,
      null,
      makeFile(1, [foAuto], [
        makeRun({ id: "ok", automation_id: "a", status: "running" }),
        makeRun({ id: "bad", automation_id: "a", status: "running" }),
      ]),
      "initial_snapshot",
      null,
      tEn,
    );
    const foDone = applyAutomationsSnapshot(
      foMap,
      null,
      makeFile(2, [foAuto], [
        makeRun({ id: "ok", automation_id: "a", status: "completed" }),
        makeRun({ id: "bad", automation_id: "a", status: "failed" }),
      ]),
      "incremental",
      null,
      tEn,
    );
    expect(foDone.notify.map((n) => n.run_id)).toEqual(["bad"]);
    expect(foDone.silenced).toEqual(["ok"]);
  });

  test("notificationId keeps local and remote machines apart & notificationExcerpt truncates at 240 chars", () => {
    expect(notificationId(null, "r")).toBe("smelt.automations/local/r");
    expect(notificationId("rt", "r")).toBe("smelt.automations/remote:rt/r");
    const long = "a".repeat(300);
    const excerpt = notificationExcerpt(long);
    expect(excerpt.length).toBe(241);
    expect(excerpt.endsWith("…")).toBe(true);
  });
});

// ---- 6. 排程解析、校验、草稿往返与文案（crates/plugins/automations/src/tests.rs） ----

describe("schedules, validation, copy & formatting", () => {
  test("clockInputIsEditable accepts partial valid values and rejects impossible times", () => {
    for (const v of ["", "0", "09", "09:", "09:3", "09:35", "9:05"]) {
      expect(clockInputIsEditable(v), v).toBe(true);
    }
    for (const v of [":", "24", "09:60", "009:00", "09::30", "noon"]) {
      expect(clockInputIsEditable(v), v).toBe(false);
    }
  });

  test("product copy matches exact English terms", () => {
    expect(automationPromptHint(false, tEn)).toBe(
      "Sent to the agent on every run. Set standing instructions in the agent's settings.",
    );
    expect(automationPromptHint(true, tEn)).toBe(
      "The request body is appended to the instructions; to place it elsewhere, use {{payload}} or a field variable (such as {{text}}). Leave empty to send the request body as is. Set standing instructions in the agent's settings.",
    );
    expect(triggerPresetLabel("daily", tEn)).toBe("Daily");
    expect(triggerPresetLabel("webhook", tEn)).toBe("Webhook");
    expect(notifyLabel("always", tEn)).toBe("Every run");
    expect(notifyLabel("never", tEn)).toBe("No notification");
  });

  test("scheduleSummary & triggerSummary read naturally without leaking tokens", () => {
    expect(
      scheduleSummary({ type: "weekly", days: SCHEDULE_WEEKDAYS_MASK, hour: 9, minute: 35 }, tEn),
    ).toBe("Every Mon, Tue, Wed, Thu, Fri · 09:35");
    expect(scheduleSummary({ type: "daily", hour: 9, minute: 0 }, tEn)).toBe("Daily 09:00");
    expect(scheduleSummary({ type: "every_minutes", minutes: 15 }, tEn)).toBe("Every 15 min");
    expect(scheduleSummary({ type: "every_hours", hours: 1 }, tEn)).toBe("Every 1 h");
    expect(
      scheduleSummary(
        {
          type: "during",
          minutes: 15,
          days: SCHEDULE_WEEKDAYS_MASK,
          start_hour: 9,
          start_minute: 30,
          end_hour: 15,
          end_minute: 0,
        },
        tEn,
      ),
    ).toBe("Every Mon, Tue, Wed, Thu, Fri 09:30–15:00 · every 15 min");
    expect(
      scheduleSummary(
        { type: "weekly", days: SCHEDULE_DAY_MON | SCHEDULE_DAY_WED | SCHEDULE_DAY_FRI, hour: 9, minute: 35 },
        tEn,
      ),
    ).toBe("Every Mon, Wed, Fri · 09:35");

    // 兼容旧版 weekdays 类型归一
    expect(normalizeSchedule({ type: "weekdays", hour: 8, minute: 15 })).toEqual({
      type: "weekly",
      days: SCHEDULE_WEEKDAYS_MASK,
      hour: 8,
      minute: 15,
    });

    expect(
      triggerSummary(
        { webhooks: [{ endpoint: "tok_should_not_leak", secret: "tok_should_not_leak" }] },
        tEn,
      ),
    ).toBe("External trigger");
    expect(
      triggerSummary(
        {
          schedules: [{ type: "daily", hour: 9, minute: 0 }],
          webhooks: [{ endpoint: "tok", secret: "tok" }],
        },
        tEn,
      ),
    ).toBe("Daily 09:00; External trigger");
    expect(
      triggerSummary(
        {
          webhooks: [
            { endpoint: "a", secret: "a" },
            { endpoint: "b", secret: "b" },
          ],
        },
        tEn,
      ),
    ).toBe("External trigger; External trigger");
    expect(triggerSummary({}, tEn)).toBe("No trigger set up");
  });

  test("intervalTitle names the actual interval", () => {
    expect(intervalTitle("hours", "1", tEn)).toBe("Hourly");
    expect(intervalTitle("hours", " 6 ", tEn)).toBe("Every 6 h");
    expect(intervalTitle("minutes", "15", tEn)).toBe("Every 15 min");
    expect(intervalTitle("hours", "", tEn)).toBe("Interval");
    expect(intervalTitle("minutes", "  ", tEn)).toBe("Interval");
  });

  test("automationMissingFields & automationSaveHint list every missing field", () => {
    const agent = (prompt) => ({ type: "agent", agent_definition_id: "agent-1", ...(prompt != null ? { prompt } : {}) });
    expect(automationMissingFields("", false, false, agent(null), true, 0)).toEqual([
      { kind: "name" },
      { kind: "trigger" },
    ]);
    expect(automationMissingFields("晨报", true, true, agent("  "), true, 0)).toEqual([
      { kind: "prompt" },
    ]);
    expect(automationMissingFields("Hook", true, false, agent(null), true, 0)).toEqual([]);
    expect(automationMissingFields("晨报", true, true, agent("汇总"), false, 0)).toEqual([
      { kind: "agent" },
    ]);
    expect(
      automationMissingFields("巡检", true, true, { type: "shell", command: "", args: [] }, false, 0),
    ).toEqual([{ kind: "command" }]);
    expect(automationMissingFields("晨报", true, true, agent("看「填写仓库路径」"), true, 2)).toEqual([
      { kind: "placeholders", count: 2 },
    ]);
    expect(missingFieldLabel({ kind: "placeholders", count: 2 }, tEn)).toBe("Template blanks (2)");

    expect(automationSaveHint([], true, tEn)).toBeNull();
    expect(automationSaveHint([{ kind: "name" }, { kind: "trigger" }], true, tEn)).toBe(
      "Still needed: Name, Trigger",
    );
    expect(automationSaveHint([{ kind: "prompt" }], false, tEn)).toBe(
      "Still needed: Instructions. Saves automatically once complete",
    );
  });

  test("scheduleFromValues rejects invalid clocks/intervals and maps weekday windows to during", () => {
    const parse = (kind, time, interval, unit, days) =>
      scheduleFromValues(kind, time, interval, unit, days, "", "", tEn, NO_PROBLEM);

    expect(parse("daily", "23:59", "", "minutes", 0)).toEqual({
      ok: true,
      schedule: { type: "daily", hour: 23, minute: 59 },
    });
    expect(parse("daily", "24:00", "", "minutes", 0).ok).toBe(false);
    expect(parse("interval", "", "15", "minutes", 0)).toEqual({
      ok: true,
      schedule: { type: "every_minutes", minutes: 15 },
    });
    expect(parse("interval", "", "2", "hours", 0)).toEqual({
      ok: true,
      schedule: { type: "every_hours", hours: 2 },
    });
    expect(parse("interval", "", "0", "minutes", 0).ok).toBe(false);

    const weekdays935 = {
      ok: true,
      schedule: { type: "weekly", days: SCHEDULE_WEEKDAYS_MASK, hour: 9, minute: 35 },
    };
    expect(parse("weekdays", "09:35", "", "minutes", 0)).toEqual(weekdays935);
    expect(parse("weekly", "09:35", "", "minutes", SCHEDULE_WEEKDAYS_MASK)).toEqual(weekdays935);
    expect(parse("weekly", "09:35", "", "minutes", SCHEDULE_DAY_MON | SCHEDULE_DAY_FRI)).toEqual({
      ok: true,
      schedule: { type: "weekly", days: SCHEDULE_DAY_MON | SCHEDULE_DAY_FRI, hour: 9, minute: 35 },
    });
    expect(parse("weekly", "09:35", "", "minutes", 0).ok).toBe(false);

    const win = (s, e) =>
      scheduleFromValues("interval", "", "15", "minutes", SCHEDULE_WEEKDAYS_MASK, s, e, tEn, NO_PROBLEM);
    expect(win("09:30", "11:30")).toEqual({
      ok: true,
      schedule: {
        type: "during",
        minutes: 15,
        days: SCHEDULE_WEEKDAYS_MASK,
        start_hour: 9,
        start_minute: 30,
        end_hour: 11,
        end_minute: 30,
      },
    });
    expect(win("09:30", "").ok).toBe(false);
  });

  test("日程、触发器的规则问题交给注入的 rules，按种类出文案", () => {
    const seen = [];
    const rules = {
      scheduleProblem: (schedule) => {
        seen.push(schedule);
        return { kind: "end_before_start" };
      },
      triggerProblem: () => ({ kind: "no_trigger" }),
    };
    const rev = scheduleFromValues("interval", "", "15", "minutes", 0, "11:30", "09:30", tEn, rules);
    expect(rev).toEqual({ ok: false, error: "The end time must be later than the start time" });
    expect(seen[0]).toMatchObject({ type: "during", start_hour: 11, end_hour: 9 });

    const triggerRules = { ...NO_PROBLEM, triggerProblem: (trigger) => (trigger.webhooks.length ? null : { kind: "no_trigger" }) };
    expect(triggerFromDraft([], [], tEn, randomUUID, triggerRules).ok).toBe(false);
  });

  test("REDACTED_WEBHOOK_ENDPOINT and draft round-trip", () => {
    // 脱敏令牌往返保持 `<redacted>` 不被覆盖
    const hookRes = triggerFromDraft(
      [{ uid: "t-1", triggerKind: "webhook", webhookSecret: REDACTED_WEBHOOK_ENDPOINT }],
      [],
      tEn,
      randomUUID,
      NO_PROBLEM,
    );
    expect(hookRes).toEqual({
      ok: true,
      trigger: { webhooks: [{ endpoint: REDACTED_WEBHOOK_ENDPOINT }] },
    });

    // 完整草稿 -> 线格式往返
    let uid = 0;
    const auto = {
      id: "auto-1",
      name: "Nightly",
      enabled: true,
      workspace_dir: "/repo",
      trigger: {
        schedules: [{ type: "daily", hour: 9, minute: 0 }],
        webhooks: [{ endpoint: "abc123", secret: "abc123" }],
        events: [{ topic: "build.failed" }],
      },
      action: { type: "agent", agent_definition_id: "ag-1", prompt: "Check logs" },
      sinks: notifyToSinks("failures_only"),
    };
    const draft = buildDraftFromAutomation(auto, null, false, () => `t-${++uid}`, randomUUID);
    const wire = buildAutomationWireFromDraft(draft, true, tEn, randomUUID, NO_PROBLEM);
    expect(wire.ok).toBe(true);
    expect(wire.automation).toEqual(auto);
  });

  test("agentDisplayName, row status, relative time cells & catalogs", () => {
    expect(agentDisplayName("", tEn)).toBe("Untitled");
    expect(agentDisplayName("   ", tEn)).toBe("Untitled");
    expect(agentDisplayName(" 量化交易 ", tEn)).toBe("量化交易");

    expect(automationRowStatus(true, true, true, true)).toBe("unavailable");
    expect(automationRowStatus(true, false, true, true)).toBe("agent_missing");
    expect(automationRowStatus(true, false, false, true)).toBe("running");
    expect(automationRowStatus(true, false, false, false)).toBe("active");
    expect(automationRowStatus(false, false, false, false)).toBe("paused");
    expect(automationRowStatusLabel("active", tEn)).toBe("Enabled");
    expect(automationRowStatusLabel("paused", tEn)).toBe("Paused");

    expect(formatRelativeUntil(100, 100, tEn)).toBe("Running soon");
    expect(formatRelativeUntil(100, 160, tEn)).toBe("Running soon");
    expect(formatRelativeUntil(100 + 3 * 60, 100, tEn)).toBe("in 3 min");
    expect(formatRelativeUntil(100 + 15 * 3600, 100, tEn)).toBe("in 15 h");
    expect(formatRelativeUntil(100 + 2 * 86400, 100, tEn)).toBe("in 2 d");
    expect(formatNextRunCell(100 + 3600, true, 100, tEn)).toBe("in 1 h");
    expect(formatNextRunCell(100 + 3600, false, 100, tEn)).toBe("—");
    expect(formatNextRunCell(null, true, 100, tEn)).toBe("—");

    expect(formatRelativeSince(100, 100, tEn)).toBe("Just now");
    expect(formatRelativeSince(100, 130, tEn)).toBe("Just now");
    expect(formatRelativeSince(100, 100 + 3 * 60, tEn)).toBe("3 min ago");
    expect(formatRelativeSince(100, 100 + 5 * 3600, tEn)).toBe("5 h ago");
    expect(formatRelativeSince(100, 100 + 2 * 86400, tEn)).toBe("2 d ago");
    expect(formatRelativeSince(100, 100 + 40 * 86400, tEn)).toBe("1 mo ago");
    expect(formatRelativeSince(100, 100 + 400 * 86400, tEn)).toBe("1 yr ago");
    expect(formatRelativeSince(100 + 3600, 100, tEn)).toBe("Just now");
    expect(formatLastRun("failed", 100, 100 + 3600, tEn)).toBe("Last: Failed · 1 h ago");

    expect(BUILTIN_AUTOMATION_ACTIONS).toEqual(["agent", "shell"]);
    expect(actionKindLabel("agent", tEn)).toBe("Agent");
    expect(actionKindLabel("shell", tEn)).toBe("Shell");

    expect(BUILTIN_AUTOMATION_TRIGGER_PRESETS).toEqual([
      "hourly",
      "daily",
      "weekly",
      "interval",
      "webhook",
    ]);
    expect(triggerPresetHint("webhook", tEn)).toBe("When this webhook receives a request");
    expect(triggerPresetIsAdvanced("webhook")).toBe(true);
    expect(triggerPresetIsAdvanced("daily")).toBe(false);
    expect(triggerPresetSchedule("hourly")).toEqual({ type: "every_hours", hours: 1 });
    expect(triggerPresetKind("webhook")).toBe("webhook");

    expect(NOTIFY_CHOICES).toEqual(["never", "failures_only", "always"]);
    for (const n of NOTIFY_CHOICES) {
      expect(notifyFromSinks(notifyToSinks(n))).toBe(n);
      expect(notifyLabel(n, tEn).length).toBeGreaterThan(0);
      expect(notifyHint(n, tEn).length).toBeGreaterThan(0);
    }
    expect(notifyLabel("failures_only", tEn)).toBe("Failures only");
    expect(notifyShouldFire("never", true)).toBe(false);
    expect(notifyShouldFire("failures_only", false)).toBe(false);
    expect(notifyShouldFire("failures_only", true)).toBe(true);
    expect(notifyShouldFire("always", false)).toBe(true);

    expect(deleteConfirmDescription("Morning", 0, tEn)).toContain("Morning");
    expect(deleteConfirmDescription("Morning", 1, tEn)).toContain("1 run record");
    expect(deleteConfirmDescription("Morning", 3, tEn)).toContain("3 run records");
  });
});

// ---- 7. 模板与占位符（crates/plugins/automations/src/templates.rs） ----

describe("templates", () => {
  test("9 unique builtin templates have valid copy and placeholders in both en and zh-CN", () => {
    expect(BUILTIN_TEMPLATES.length).toBe(9);
    const ids = new Set();
    for (const tpl of BUILTIN_TEMPLATES) {
      expect(ids.has(tpl.id)).toBe(false);
      ids.add(tpl.id);
      for (const [lang, tFn] of [
        ["en", tEn],
        ["zh-CN", tZh],
      ]) {
        expect(tFn(tpl.nameKey).length, `${tpl.id} ${lang} name`).toBeGreaterThan(0);
        expect(tFn(tpl.descriptionKey).length, `${tpl.id} ${lang} desc`).toBeGreaterThan(0);
        const prompt = tFn(tpl.promptKey);
        expect(templatePlaceholders(prompt).length, `${tpl.id} ${lang} placeholders`).toBeGreaterThan(0);
      }
    }
    expect(automationTemplate("morning-brief")).not.toBeNull();
    expect(automationTemplate("missing")).toBeNull();
  });

  test("templatePlaceholders deduplicates and ignores unclosed slots", () => {
    expect(
      templatePlaceholders("看仓库「填写仓库路径」，再看「填写仓库路径」和「别的」"),
    ).toEqual(["「填写仓库路径」"]);
    expect(
      templatePlaceholders("Read [file path] then [folder path] and [unclosed"),
    ).toEqual(["[file path]", "[folder path]"]);
    expect(templatePlaceholders("没有空")).toEqual([]);
  });

  test("every category has templates and schedule summaries match", () => {
    for (const cat of TEMPLATE_CATEGORIES) {
      const list = filteredTemplates(cat);
      expect(list.length, templateCategoryLabel(cat, tEn)).toBeGreaterThan(0);
      expect(list.length).toBe(BUILTIN_TEMPLATES.filter((x) => x.category === cat).length);
    }
    expect(filteredTemplates(null).length).toBe(BUILTIN_TEMPLATES.length);
    expect(scheduleSummary(automationTemplate("morning-brief").schedule, tEn)).toBe("Daily 07:30");
    expect(scheduleSummary(automationTemplate("weekly-review").schedule, tEn)).toBe("Every Fri · 16:00");
    expect(scheduleSummary(automationTemplate("dependency-check").schedule, tEn)).toBe("Every Mon · 09:00");
  });
});

// ---- 8. 工具标题、运行详情、失败码与 Bug 修复回归 ----

describe("tools, run detail, failure descriptions & bug fix regressions", () => {
  test("compactToolHeadline & toolProcessIconPath match Rust tools.rs", () => {
    expect(compactToolHeadline("read", "/repo/src/lib.rs", tEn)).toBe("Read lib.rs");
    expect(compactToolHeadline("fetch", "https://www.example.com/a", tEn)).toBe("Opened example.com/a");
    expect(compactToolHeadline("execute", "cargo test\n--all", tEn)).toBe("Ran cargo test");
    expect(compactToolHeadline("execute", "  ", tEn)).toBe("Bash");
    expect(toolProcessIconPath("execute")).toBe("icons/terminal.svg");
    expect(toolProcessIconPath("edit")).toBe("icons/square-pen.svg");
    expect(toolProcessIconPath("think")).toBe("icons/asterisk.svg");
    expect(toolProcessIconPath("other")).toBe("icons/wrench.svg");
  });

  test("fencedBlock outgrows backticks, shields markdown lookalikes, and clips long output", () => {
    const nested = fencedBlock("", "a ```` b", tEn);
    expect(nested.startsWith("`````\n")).toBe(true);
    expect(nested.endsWith("\n`````")).toBe(true);

    expect(fencedBlock("", "---\nname: persona\n---", tEn)).toBe(
      "```\n---\nname: persona\n---\n```",
    );

    const body = Array.from({ length: TOOL_OUTPUT_MAX_LINES + 5 }, (_, i) => String(i)).join("\n");
    const clipped = fencedBlock("", body, tEn);
    expect(clipped).toContain("5 more lines not shown");
    expect(clipped).not.toContain(`\n${TOOL_OUTPUT_MAX_LINES + 1}\n`);
  });

  test("finalReplyIndex, formatElapsed, transcriptSummary, turnFailedText, taskNoteSummary & diff helpers", () => {
    const entries = [
      { Assistant: { text: "中间说明", thought: false } },
      { Assistant: { text: "结论\n", thought: false } },
    ];
    expect(finalReplyIndex("结论", entries)).toBe(1);
    expect(finalReplyIndex("别的", entries)).toBeNull();
    expect(finalReplyIndex(null, entries)).toBeNull();

    expect(formatElapsed(42, tEn)).toBe("42s");
    expect(formatElapsed(125, tEn)).toBe("2m 5s");
    expect(formatElapsed(3720, tEn)).toBe("1h 2m");

    expect(transcriptSummary([{ User: "hi" }], tEn)).toBe("1 message");
    expect(
      transcriptSummary(
        [
          {
            ToolCall: {
              id: "1",
              title: "ls",
              kind: "execute",
              status: "completed",
              output: [],
              children: [
                {
                  ToolCall: {
                    id: "2",
                    title: "cat",
                    kind: "read",
                    status: "failed",
                    output: [],
                    children: [],
                  },
                },
              ],
            },
          },
        ],
        tEn,
      ),
    ).toBe("2 tool calls · 1 failed");

    expect(turnFailedText("quota exceeded", "rate_limited", tEn)).toContain("Turn failed: quota exceeded");
    expect(turnFailedText("quota exceeded", "rate_limited", tEn)).toContain("rate limiting");
    expect(newSessionLabel(1700000000000, tEn)).toContain("New session");
    expect(
      taskNoteSummary(
        { id: "t1", kind: "backgroundTask", status: "completed", exitCode: 0, title: "build" },
        tEn,
      ),
    ).toBe("Background task t1 (build) ended: done, exit code 0");
    expect(
      taskNoteSummary(
        { peer: { sourceSessionId: "s1", sourceTitle: "Planner", kind: "task" } },
        tEn,
      ),
    ).toBe("Planner sent a task");

    expect(classifyAcpEntry({ User: "hi" })).toEqual({ type: "user", text: "hi", images: 0 });
    expect(
      classifyToolOutputPart({ Diff: { path: "src/app.ts", old_text: "a\n", new_text: "b\n" } }),
    ).toEqual({ type: "diff", path: "src/app.ts", oldText: "a\n", newText: "b\n" });
    expect(editorLanguageForPath("/repo/src/lib.rs")).toBe("rs");
    expect(estimateDiffHeight("a\nb", "a\nc")).toBeGreaterThanOrEqual(120);

    const builtDetail = buildRunDetail(
      { machine: null, runId: "r1", status: "completed", finishedAt: 12 },
      makeRun({ output: null }),
      { output: "archived reply", context: { agent_instructions: "Be concise." } },
      entries,
      new Set(["process"]),
    );
    expect(builtDetail.output).toBe("archived reply");
    expect(builtDetail.instructions).toBe("Be concise.");
    expect(builtDetail.expanded.has("process")).toBe(true);
  });

  test("describeAutomationRunFailure & runErrorText cover known and unknown failure codes", () => {
    expect(describeAutomationRunFailure("previous_run_active", null, tEn)).toContain("previous run");
    expect(describeAutomationRunFailure("command_failed", "exit 127", tEn)).toBe(
      "The command failed: exit 127",
    );
    expect(describeAutomationRunFailure("unknown_future_code", "custom detail", tEn)).toBe(
      "custom detail",
    );
    expect(describeAutomationRunFailure("unknown_future_code", null, tEn)).toBe(
      "The run failed",
    );
    expect(
      runErrorText(makeRun({ error_code: "agent_unavailable", error: "deleted" }), tEn),
    ).toBe("The agent can't be used: deleted");
    expect(runErrorText(makeRun({ error_code: null, error: "raw err" }), tEn)).toBe("raw err");
  });

  test("Bug Fix #4: webhook trigger draft does not allocate unused schedule rule", () => {
    const hook = createTriggerEntryDraft("t-1", "webhook", null, null, randomUUID);
    expect(hook.triggerKind).toBe("webhook");
    expect("schedule" in hook).toBe(false);
    expect(hook.webhookSecret).toMatch(/^[0-9a-f]{32}$/);

    const cal = createTriggerEntryDraft("t-2", "calendar", { type: "daily", hour: 8, minute: 0 }, null, randomUUID);
    expect(cal.triggerKind).toBe("calendar");
    expect(cal.schedule.time).toBe("08:00");
  });

  test("Bug Fix #5: Shell run summary & metadata use terminal icon and Shell label (not Unknown agent)", () => {
    const shellRun = makeRun({
      status: "completed",
      started_at: 100,
      finished_at: 142,
      context: {
        automation_name: "Disk check",
        action: { type: "shell", command: "df", args: ["-h"] },
        cwd: "/tmp",
        trigger_payload: null,
        prompt: null,
        agent_definition_name: null,
        engine_kind_id: null,
        agent_instructions: null,
      },
    });
    const summary = runSummaryInfo(shellRun, tEn);
    expect(summary.icon).toBe("icons/terminal.svg");
    expect(summary.actor).toBe("Shell");
    expect(summary.detail).toContain("42s");

    const rows = runMetadataRows(shellRun, tEn);
    expect(rows.some((r) => r.label === "Action" && r.value === "Shell")).toBe(true);
    expect(rows.some((r) => r.label === "Command" && r.value === "df -h")).toBe(true);
    expect(rows.some((r) => r.value.includes("Unknown agent"))).toBe(false);

    // Agent 运行依然走 Bot 图标与智能体名
    const agentRun = makeRun({
      context: {
        automation_name: "Daily brief",
        action: { type: "agent", agent_definition_id: "ag-1", prompt: "Summarize" },
        cwd: "/repo",
        agent_definition_name: "Researcher",
        engine_kind_id: "claude",
      },
    });
    const agentSummary = runSummaryInfo(agentRun, tEn);
    expect(agentSummary.icon).toBe("icons/bot.svg");
    expect(agentSummary.actor).toBe("Researcher");
    const agentRows = runMetadataRows(agentRun, tEn);
    expect(agentRows.some((r) => r.label === "Agent" && r.value === "Researcher · claude")).toBe(true);
  });
});
