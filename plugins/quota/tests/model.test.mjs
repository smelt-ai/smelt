import assert from "node:assert/strict";
import { test } from "node:test";

import {
  apply,
  begin_refresh,
  chips,
  empty_state,
  enabled_targets,
  next_manual,
  popover_entries,
  quota_hint,
  quota_tone,
  restore,
  ring_fraction,
  ring_windows,
  setting_row,
  until_text,
} from "../model.js";

// 宿主那两份规则的假版本，够测试用（真的见 smelt-model 的 quota_service、provider_quota）。
const SERVICES = { "pi:github-copilot": "copilot", "pi:anthropic": "claude" };
const NAMES = { codex: "Codex", copilot: "Copilot", claude: "Claude", antigravity: "Antigravity", grok: "Grok", cursor: "Cursor" };
const quota_target = (id) => {
  const service = SERVICES[id] ?? (id.includes(":") ? null : id);
  if (service == null) return null;
  return {
    id,
    service,
    name: id.startsWith("pi:") ? `${NAMES[service]} · Pi` : NAMES[service],
    icon: `smelt-icons/agent-${service}.svg`,
  };
};
const quota_facts = (data) => {
  if (data.windows) {
    const used = Math.max(0, ...data.windows.windows.map((window) => window.used_percent));
    return {
      short_text: `${Math.round(used)}%`,
      used_percent: used,
      windows: data.windows.windows.map((window) => ({ ...window, display_label: window.label })),
      copilot: null,
    };
  }
  const copilot = data.copilot;
  const used = copilot.limit == null ? null : (copilot.used ?? Math.max(copilot.limit - copilot.remaining, 0));
  return {
    short_text: used == null ? String(copilot.remaining) : `${Math.round((used / copilot.limit) * 100)}%`,
    used_percent: used == null ? 0 : (used / copilot.limit) * 100,
    windows: [],
    copilot: {
      unit: "premium requests",
      remaining: String(copilot.remaining),
      limit: copilot.limit == null ? null : String(copilot.limit),
      used: used == null ? null : String(used),
    },
  };
};
const t = (key, params) => (params ? `${key}:${JSON.stringify(params)}` : key);
const host = { quota_target, quota_facts, t };

const window = (label, used_percent, extra = {}) => ({
  label,
  used_percent,
  resets_at_ms: null,
  reset_after_seconds: null,
  ...extra,
});
const ready = (used) => ({ quota: { windows: { windows: [window("5h", used)] } }, checked_at_ms: 1, error: null });
const status = (checked_at_ms) => ({ quota: null, checked_at_ms, error: null });

test("换范围时丢掉不在范围里的旧数字；单查一家的结果只覆盖那一家", () => {
  const state = empty_state();
  restore(state, ["claude", "codex"], [["claude", status(1)], ["codex", status(1)]]);
  begin_refresh(state, "codex");
  assert.deepEqual([...state.refreshing], ["codex"]);
  assert.deepEqual([...state.manual], ["codex"]);
  apply(state, [["codex", status(2)]]);
  restore(state, ["codex"], []);
  assert.deepEqual(state.targets, ["codex"]);
  assert.deepEqual([...state.statuses], [["codex", status(2)]]);
  assert.equal(state.refreshing.size + state.manual.size, 0);
});

test("后台全量刷新查的是整个范围，但不算用户点的", () => {
  const state = empty_state();
  restore(state, ["claude"], []);
  begin_refresh(state, null);
  assert.deepEqual([...state.refreshing], ["claude"]);
  assert.equal(state.manual.size, 0);
});

test("排队的单查合并成一轮，最后点的优先", () => {
  const queue = ["codex", "claude"];
  assert.equal(next_manual(queue), "claude");
  assert.equal(queue.length, 0);
  assert.equal(next_manual(queue), null);
});

test("环的长度表示已用量", () => {
  assert.equal(ring_fraction(0), 0);
  assert.equal(ring_fraction(20), 0.2);
  assert.equal(ring_fraction(100), 1);
  assert.equal(ring_fraction(150), 1);
});

test("5 小时在外、周在内", () => {
  const facts = quota_facts({ windows: { windows: [window("5h", 20), window("7d", 90)] } });
  assert.deepEqual(ring_windows(t, facts), [
    { label: "5h", used: 20 },
    { label: "7d", used: 90 },
  ]);
});

test("没有标准窗口时画最紧的那个", () => {
  const facts = quota_facts({ windows: { windows: [window("month", 35), window("Secondary", 75)] } });
  assert.deepEqual(ring_windows(t, facts), [{ label: "Secondary", used: 75 }]);
});

test("Copilot 不知道总额时不画环", () => {
  const facts = quota_facts({ copilot: { unit: "PremiumRequests", remaining: 75, limit: null, used: null } });
  assert.deepEqual(ring_windows(t, facts), []);
});

test("窗口型的提示一行写完，字色按最紧的窗口", () => {
  const facts = quota_facts({
    windows: { windows: [window("5h", 76, { reset_after_seconds: 3_600 }), window("7d", 91)] },
  });
  assert.equal(facts.short_text, "91%");
  assert.equal(quota_tone(facts.used_percent), "bad");
  assert.equal(
    quota_hint(t, "Claude", facts, 0),
    `Claude ${t("window_reset", { window: "5h", used: 76, reset: t("resets_in", { time: t("duration.hours", { hours: 1 }) }) })} · 7d 91%`,
  );
});

test("Copilot 的提示保留数额的说法", () => {
  const facts = quota_facts({ copilot: { unit: "PremiumRequests", remaining: 75, limit: 100, used: 25 } });
  assert.equal(facts.short_text, "25%");
  assert.equal(quota_tone(facts.used_percent), "muted");
  assert.equal(
    quota_hint(t, "Copilot", facts, 0),
    t("copilot_used", { label: "Copilot", used: "25", limit: "100", unit: "premium requests", percent: 25 }),
  );
});

test("剩余时间从大单位写起", () => {
  assert.equal(until_text(t, 0), null);
  assert.equal(until_text(t, -5), null);
  assert.equal(until_text(t, 90), t("duration.minutes", { minutes: 1 }));
  assert.equal(until_text(t, 3_600), t("duration.hours", { hours: 1 }));
  assert.equal(until_text(t, 3_660), t("duration.hours_minutes", { hours: 1, minutes: 1 }));
  assert.equal(until_text(t, 86_400 + 7_200), t("duration.days_hours", { days: 1, hours: 2 }));
});

test("弹层列失败的和用户刚点的，不列没登录的", () => {
  const state = empty_state();
  restore(
    state,
    ["codex", "antigravity", "grok", "cursor", "claude", "pi:github-copilot"],
    [
      ["codex", ready(40)],
      ["antigravity", { quota: null, checked_at_ms: 2, error: "HTTP 401" }],
      ["grok", status(3)],
      ["claude", status(3)],
    ],
  );
  state.refreshing = new Set(["antigravity", "claude"]);
  state.manual = new Set(["claude"]);

  const shown = chips(state, host, 0);
  assert.deepEqual(
    shown.map((chip) => chip.id),
    ["codex"],
  );

  // Grok 没登录、Cursor 和 Pi 的 Copilot 还没查过，都不占行；Claude 没登录但用户刚点了，查的时候占一行。
  const entries = popover_entries(state, host, 0);
  assert.equal(entries.length, 3);
  assert.equal(entries[0].kind, "ready");
  assert.equal(entries[0].chip.id, "codex");
  assert.deepEqual(
    [entries[1].id, entries[1].refreshing, entries[1].error],
    ["antigravity", true, "HTTP 401"],
  );
  assert.deepEqual([entries[2].id, entries[2].refreshing, entries[2].error], ["claude", true, null]);
});

test("同一家服务几个来源：环一家一个画用得多的，弹层每个来源一行，Pi 的跟在那家自己的后面", () => {
  const state = empty_state();
  restore(
    state,
    ["codex", "copilot", "pi:anthropic", "pi:github-copilot"],
    [
      ["codex", ready(10)],
      ["copilot", ready(20)],
      ["pi:anthropic", ready(30)],
      ["pi:github-copilot", ready(95)],
    ],
  );
  const shown = chips(state, host, 0);
  assert.deepEqual(
    shown.map((chip) => chip.id),
    ["codex", "pi:github-copilot", "pi:anthropic"],
  );
  assert.ok(shown[1].hint.startsWith("Copilot · Pi "));
  assert.deepEqual(
    popover_entries(state, host, 0).map((entry) => entry.chip.id),
    ["codex", "copilot", "pi:github-copilot", "pi:anthropic"],
  );
});

test("设置行：钥匙串被拒只在开着时算，失败原因只在没有额度时给", () => {
  const state = empty_state();
  restore(state, ["codex"], [["codex", { quota: null, checked_at_ms: 1, error: "boom" }]]);
  assert.deepEqual(setting_row(state, { target: "codex", enabled: true, keychain_denied: false }), {
    id: "codex",
    enabled: true,
    denied: false,
    refreshing: false,
    error: "boom",
  });
  assert.equal(setting_row(state, { target: "claude", enabled: false, keychain_denied: true }).denied, false);
  assert.deepEqual(
    enabled_targets([
      { target: "codex", enabled: true, keychain_denied: false },
      { target: "claude", enabled: false, keychain_denied: false },
    ]),
    ["codex"],
  );
});
