import { expect, test } from "bun:test";

import {
  apply,
  begin_refresh,
  chips,
  decorate_head,
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
const NAMES = {
  codex: "Codex",
  copilot: "Copilot",
  claude: "Claude",
  antigravity: "Antigravity",
  grok: "Grok",
  cursor: "Cursor",
};
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
  scope: null,
  ...extra,
});
const ready = (used) => ({ quota: { windows: { windows: [window("5h", used)] } }, checked_at_ms: 1, error: null });
const status = (checked_at_ms) => ({ quota: null, checked_at_ms, error: null });

test("换范围时丢掉不在范围里的旧数字；单查一家的结果只覆盖那一家", () => {
  const state = empty_state();
  restore(
    state,
    ["claude", "codex"],
    [
      ["claude", status(1)],
      ["codex", status(1)],
    ],
  );
  begin_refresh(state, "codex");
  expect([...state.refreshing]).toStrictEqual(["codex"]);
  expect([...state.manual]).toStrictEqual(["codex"]);
  apply(state, [["codex", status(2)]]);
  restore(state, ["codex"], []);
  expect(state.targets).toStrictEqual(["codex"]);
  expect([...state.statuses]).toStrictEqual([["codex", status(2)]]);
  expect(state.refreshing.size + state.manual.size).toBe(0);
});

test("后台全量刷新查的是整个范围，但不算用户点的", () => {
  const state = empty_state();
  restore(state, ["claude"], []);
  begin_refresh(state, null);
  expect([...state.refreshing]).toStrictEqual(["claude"]);
  expect(state.manual.size).toBe(0);
});

test("排队的单查合并成一轮，最后点的优先", () => {
  const queue = ["codex", "claude"];
  expect(next_manual(queue)).toBe("claude");
  expect(queue.length).toBe(0);
  expect(next_manual(queue)).toBe(null);
});

test("环的长度表示已用量", () => {
  expect(ring_fraction(0)).toBe(0);
  expect(ring_fraction(20)).toBe(0.2);
  expect(ring_fraction(100)).toBe(1);
  expect(ring_fraction(150)).toBe(1);
});

test("5 小时在外、周在内", () => {
  const facts = quota_facts({ windows: { windows: [window("5h", 20), window("7d", 90)] } });
  expect(ring_windows(t, facts)).toStrictEqual([
    { label: "5h", used: 20 },
    { label: "7d", used: 90 },
  ]);
});

test("没有标准窗口时画最紧的那个", () => {
  const facts = quota_facts({ windows: { windows: [window("month", 35), window("Secondary", 75)] } });
  expect(ring_windows(t, facts)).toStrictEqual([{ label: "Secondary", used: 75 }]);
});

test("Copilot 不知道总额时不画环", () => {
  const facts = quota_facts({ copilot: { unit: "PremiumRequests", remaining: 75, limit: null, used: null } });
  expect(ring_windows(t, facts)).toStrictEqual([]);
});

test("窗口型的提示一行写完，字色按最紧的窗口", () => {
  const facts = quota_facts({
    windows: { windows: [window("5h", 76, { reset_after_seconds: 3_600 }), window("7d", 91)] },
  });
  expect(facts.short_text).toBe("91%");
  expect(quota_tone(facts.used_percent)).toBe("bad");
  expect(quota_hint(t, "Claude", facts, 0)).toBe(
    `Claude ${t("window_reset", { window: "5h", used: 76, reset: t("resets_in", { time: t("duration.hours", { hours: 1 }) }) })} · 7d 91%`,
  );
});

test("Copilot 的提示保留数额的说法", () => {
  const facts = quota_facts({ copilot: { unit: "PremiumRequests", remaining: 75, limit: 100, used: 25 } });
  expect(facts.short_text).toBe("25%");
  expect(quota_tone(facts.used_percent)).toBe("muted");
  expect(quota_hint(t, "Copilot", facts, 0)).toBe(
    t("copilot_used", { label: "Copilot", used: "25", limit: "100", unit: "premium requests", percent: 25 }),
  );
});

test("几组模型各有窗口：环取每种时长里最紧的那组，提示一组一行", () => {
  const facts = quota_facts({
    windows: {
      windows: [
        window("5h", 28, { scope: "Gemini" }),
        window("7d", 12, { scope: "Gemini" }),
        window("5h", 0, { scope: "Claude and GPT" }),
        window("7d", 50, { scope: "Claude and GPT" }),
      ],
    },
  });
  expect(ring_windows(t, facts)).toStrictEqual([
    { label: "5h", used: 28 },
    { label: "7d", used: 50 },
  ]);
  expect(quota_hint(t, "Antigravity", facts, 0)).toBe("Antigravity\nGemini 5h 28% · 7d 12%\nClaude and GPT 5h 0% · 7d 50%");
});

test("整个账户的窗口和名字一行，单独计的模型另起一行", () => {
  const facts = quota_facts({
    windows: { windows: [window("5h", 9), window("7d", 34), window("7d", 61, { scope: "Sonnet" })] },
  });
  expect(quota_hint(t, "Claude", facts, 0)).toBe("Claude 5h 9% · 7d 34%\nSonnet 7d 61%");
  expect(ring_windows(t, facts)).toStrictEqual([
    { label: "5h", used: 9 },
    { label: "7d", used: 61 },
  ]);
});

test("外环内环、点击刷新这类说明接在名字那行", () => {
  expect(decorate_head("Claude 5h 9%\nSonnet 7d 61%", (head) => `${head} · x`)).toBe("Claude 5h 9% · x\nSonnet 7d 61%");
  expect(decorate_head("Codex 5h 9%", (head) => `${head} · x`)).toBe("Codex 5h 9% · x");
});

test("剩余时间从大单位写起", () => {
  expect(until_text(t, 0)).toBe(null);
  expect(until_text(t, -5)).toBe(null);
  expect(until_text(t, 90)).toBe(t("duration.minutes", { minutes: 1 }));
  expect(until_text(t, 3_600)).toBe(t("duration.hours", { hours: 1 }));
  expect(until_text(t, 3_660)).toBe(t("duration.hours_minutes", { hours: 1, minutes: 1 }));
  expect(until_text(t, 86_400 + 7_200)).toBe(t("duration.days_hours", { days: 1, hours: 2 }));
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
  expect(shown.map((chip) => chip.id)).toStrictEqual(["codex"]);

  // Grok 没登录、Cursor 和 Pi 的 Copilot 还没查过，都不占行；Claude 没登录但用户刚点了，查的时候占一行。
  const entries = popover_entries(state, host, 0);
  expect(entries.length).toBe(3);
  expect(entries[0].kind).toBe("ready");
  expect(entries[0].chip.id).toBe("codex");
  expect([entries[1].id, entries[1].refreshing, entries[1].error]).toStrictEqual(["antigravity", true, "HTTP 401"]);
  expect([entries[2].id, entries[2].refreshing, entries[2].error]).toStrictEqual(["claude", true, null]);
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
  expect(shown.map((chip) => chip.id)).toStrictEqual(["codex", "pi:github-copilot", "pi:anthropic"]);
  expect(shown[1].hint.startsWith("Copilot · Pi ")).toBeTruthy();
  expect(popover_entries(state, host, 0).map((entry) => entry.chip.id)).toStrictEqual([
    "codex",
    "copilot",
    "pi:github-copilot",
    "pi:anthropic",
  ]);
});

test("设置行：钥匙串被拒只在开着时算，失败原因只在没有额度时给", () => {
  const state = empty_state();
  restore(state, ["codex"], [["codex", { quota: null, checked_at_ms: 1, error: "boom" }]]);
  expect(setting_row(state, { target: "codex", enabled: true, keychain_denied: false })).toStrictEqual({
    id: "codex",
    enabled: true,
    denied: false,
    refreshing: false,
    error: "boom",
  });
  expect(setting_row(state, { target: "claude", enabled: false, keychain_denied: true }).denied).toBe(false);
  expect(
    enabled_targets([
      { target: "codex", enabled: true, keychain_denied: false },
      { target: "claude", enabled: false, keychain_denied: false },
    ]),
  ).toStrictEqual(["codex"]);
});
