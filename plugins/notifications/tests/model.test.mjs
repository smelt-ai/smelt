import { describe, expect, test } from "bun:test";
import {
  AttentionMirror,
  buildMenu,
  conversationRow,
  DEFAULT_PREFS,
  notificationId,
  permissionLine,
  prefsAllows,
  statusTitle,
  statusTooltip,
  TERMINAL_MENU_ICON,
  agentIconAsset,
} from "../model.js";
import { ACTIVATE_SESSION, SHOW_MAIN_WINDOW } from "../contract.js";

const EN = {
  "fallback.conversation": "Conversation",
  "fallback.terminal": "Terminal",
  "status.needs_you": "Needs you",
  "status.running": "Running",
  "status.unread": "Unread",
  "menu.show_main_window": "Open smelt Main Window",
  "tooltip.both": "%{attention}, %{running}",
  "tooltip.attention.one": "1 session needs attention",
  "tooltip.attention.other": "%{count} sessions need attention",
  "tooltip.running.one": "1 session running",
  "tooltip.running.other": "%{count} sessions running",
  "permission.granted": "System notifications are allowed",
  "permission.no_banners": "No banners",
  "permission.no_badge": "The Dock badge is turned off in the system",
  "permission.no_banners_or_badge": "No banners or badge",
  "permission.not_requested": "Not requested",
  "permission.denied": "Denied",
};

function t(key, params) {
  let out = EN[key] ?? key;
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      out = out.replaceAll(`%{${k}}`, String(v));
    }
  }
  return out;
}

const PALETTE = {
  need: "#f59e0b",
  run: "#3b82f6",
  faint: "#71717a",
};

const KNOWN_AGENTS = new Set([
  "claude",
  "copilot",
  "codex",
  "grok",
  "antigravity",
  "cursor",
  "opencode",
  "kiro",
  "pi",
  "omp",
  "crush",
]);

function item(kind) {
  return { kind, title: "Waiting", message: "cmd", note: null };
}

function unread(id) {
  return { session_id: id, action: null, unread: item("success") };
}

describe("notifications model", () => {
  test("notificationId keeps machines apart", () => {
    expect(notificationId(null, "s1")).toBe("smelt.notifications/local/s1");
    expect(notificationId("rt", "s1")).toBe("smelt.notifications/remote:rt/s1");
  });

  test("prefsAllows honors switches while notice and unknown always allow", () => {
    const allOff = {
      approval: false,
      input: false,
      success: false,
      failure: false,
      bell: false,
    };
    expect(prefsAllows(DEFAULT_PREFS, "bell")).toBe(true);
    expect(prefsAllows(allOff, "bell")).toBe(false);
    expect(prefsAllows(allOff, "notice")).toBe(true);
    expect(prefsAllows(allOff, "unknown")).toBe(true);
  });

  test("AttentionMirror snapshot primes, drops stale events, handles restarts and machine removal", () => {
    const mirror = new AttentionMirror();
    expect(mirror.primed(null)).toBe(false);
    mirror.apply(null, { kind: "event", payload: { revision: 1, session: unread("old"), alert: null } });
    expect(mirror.primed(null)).toBe(false);

    expect(
      mirror.apply(null, { kind: "snapshot", payload: { revision: 5, sessions: [unread("a")] } }),
    ).toEqual({ kind: "replaced" });
    expect(mirror.primed(null)).toBe(true);
    expect(mirror.hasUnread(null, "a")).toBe(true);
    expect(mirror.hasUnread(null, "old")).toBe(false);

    // Stale event dropped
    expect(
      mirror.apply(null, {
        kind: "event",
        payload: { revision: 5, session: unread("b"), alert: item("success") },
      }),
    ).toEqual({ kind: "nothing" });
    expect(mirror.hasUnread(null, "b")).toBe(false);

    // Newer event applied
    const applied = mirror.apply(null, {
      kind: "event",
      payload: { revision: 6, session: unread("b"), alert: item("success") },
    });
    expect(applied.kind).toBe("changed");
    expect(applied.alert).toBeTruthy();

    // Empty session removed
    mirror.apply(null, {
      kind: "event",
      payload: { revision: 7, session: { session_id: "b", action: null, unread: null } },
    });
    expect(mirror.hasUnread(null, "b")).toBe(false);

    // Restarted daemon resets revision via snapshot
    expect(mirror.apply(null, { kind: "lagged" })).toEqual({ kind: "nothing" });
    mirror.apply(null, { kind: "snapshot", payload: { revision: 0, sessions: [] } });
    expect(mirror.hasUnread(null, "a")).toBe(false);

    // Machines isolated
    mirror.apply(null, { kind: "snapshot", payload: { revision: 1, sessions: [unread("a")] } });
    mirror.apply("rt", {
      kind: "snapshot",
      payload: {
        revision: 1,
        sessions: [{ session_id: "b", action: item("approval"), unread: null }],
      },
    });
    expect(mirror.hasUnread("rt", "a")).toBe(false);
    expect(Object.fromEntries(mirror.bySession())).toEqual({ a: false, b: true });

    expect(mirror.retainMachines((m) => m === null)).toBe(true);
    expect(mirror.primed("rt")).toBe(false);
    expect(mirror.retainMachines((m) => m === null)).toBe(false);

    // Unreadable payload changes nothing
    expect(mirror.apply("bad", { kind: "event", payload: 42 })).toEqual({ kind: "nothing" });
    expect(mirror.primed("bad")).toBe(false);
  });

  test("statusTitle and statusTooltip match compact format", () => {
    expect(statusTitle(0, 0)).toBe("");
    expect(statusTitle(5, 0)).toBe("5@");
    expect(statusTitle(0, 660)).toBe("660");
    expect(statusTitle(5, 660)).toBe("5@·660");
    expect(statusTooltip(0, 0, t)).toBe("smelt");
    expect(statusTooltip(5, 660, t)).toBe("5 sessions need attention, 660 sessions running");
    expect(statusTooltip(1, 0, t)).toBe("1 session needs attention");
  });

  test("conversationRow handles unstarted terminals, remote titles, and agent resolution", () => {
    const deps = {
      t,
      statusOf: (st) =>
        st.phase === "thinking"
          ? "running"
          : st.phase === "awaiting_approval"
            ? "needs_you"
            : "idle",
      agentFromLaunch: (cmd) => (cmd.startsWith("claude") ? "claude" : null),
    };
    expect(
      conversationRow(null, { terminals: [{ id: null }] }, new Map(), null, deps),
    ).toBeNull();

    const row = conversationRow(
      "box",
      { terminals: [{ id: "a", launch_cmd: "claude" }, { id: "b" }] },
      new Map([
        ["a", { id: "a", phase: "idle" }],
        ["b", { id: "b", phase: "thinking", conversation_name: "整段的名字" }],
      ]),
      "远程机",
      deps,
    );
    expect(row.status).toBe("running");
    expect(row.title).toBe("整段的名字 · 远程机");
    expect(row.agent).toBe("claude");

    const acpRow = conversationRow(
      null,
      { acp: { sid: "s", agent: "claude" } },
      new Map([["s", { id: "s", phase: "awaiting_approval", conversation_name: "运行时名" }]]),
      null,
      deps,
    );
    expect(acpRow.title).toBe("运行时名");
    expect(acpRow.status).toBe("needs_you");
  });

  test("buildMenu orders needs_you -> running -> unread, handles split panes and trailing open-window", () => {
    const rows = [
      { machine: null, ids: ["idle"], statuses: ["idle"], title: "空闲", agent: null },
      { machine: null, ids: ["run"], statuses: ["running"], title: "在跑", agent: "claude" },
      { machine: null, ids: ["unread"], statuses: ["idle"], title: "没看", agent: null },
      { machine: null, ids: ["ask"], statuses: ["running"], title: "等批准", agent: "omp" },
    ];
    const attention = new Map([
      ["unread", false],
      ["ask", true],
      ["closed", false],
    ]);
    const built = buildMenu(rows, attention, PALETTE, KNOWN_AGENTS, t);
    expect(built.entries.map((e) => e.label)).toEqual([
      "等批准 — Needs you",
      "在跑 — Running",
      "没看 — Unread",
    ]);
    expect(built.entries[0].color).toBe(PALETTE.need);
    expect(built.title).toBe("3@·2");
    expect(built.statusMenu.items[0].icon.png).toBe("smelt-icons/agent-omp.svg");
    expect(built.statusMenu.items[2].icon.png).toBe(TERMINAL_MENU_ICON);
    expect(built.statusMenu.items[3]).toEqual({ type: "separator" });
    expect(built.statusMenu.items[4].command).toBe(SHOW_MAIN_WINDOW);

    // Split terminal picks raised pane first, then busy pane
    const splitBusy = buildMenu(
      [{ machine: "box", ids: ["left", "right"], statuses: ["idle", "running"], title: "分屏", agent: null }],
      new Map(),
      PALETTE,
      KNOWN_AGENTS,
      t,
    );
    expect(splitBusy.entries[0].session_id).toBe("right");
    expect(splitBusy.statusMenu.items[0].command).toBe(ACTIVATE_SESSION);
    expect(splitBusy.statusMenu.items[0].args).toEqual({ machine: "box", session_id: "right" });

    const splitRaised = buildMenu(
      [{ machine: null, ids: ["left", "right"], statuses: ["idle", "running"], title: "分屏", agent: null }],
      new Map([["left", false]]),
      PALETTE,
      KNOWN_AGENTS,
      t,
    );
    expect(splitRaised.entries[0].session_id).toBe("left");

    // Empty menu only has open-main-window item
    const empty = buildMenu([], new Map(), PALETTE, KNOWN_AGENTS, t);
    expect(empty.title).toBe("");
    expect(empty.statusMenu.items).toHaveLength(1);
    expect(empty.statusMenu.items[0].command).toBe(SHOW_MAIN_WINDOW);

    // Icon asset lookup
    expect(agentIconAsset("omp", KNOWN_AGENTS)).toBe("smelt-icons/agent-omp.svg");
    expect(agentIconAsset("custom-unknown", KNOWN_AGENTS)).toBe(TERMINAL_MENU_ICON);
  });

  test("permissionLine maps all permission states accurately", () => {
    expect(permissionLine({ kind: "checking" }, t)).toBeNull();
    expect(permissionLine({ kind: "unsupported" }, t)).toBeNull();
    expect(permissionLine({ kind: "granted", banners: true, sound: false, badge: true }, t)).toEqual({
      text: "System notifications are allowed",
      severity: "ok",
      fix: "open_settings",
    });
    expect(permissionLine({ kind: "granted", banners: true, sound: false, badge: false }, t)).toEqual({
      text: "The Dock badge is turned off in the system",
      severity: "warning",
      fix: "open_settings",
    });
    expect(permissionLine({ kind: "not_requested" }, t)).toEqual({
      text: "Not requested",
      severity: "info",
      fix: "request",
    });
    expect(permissionLine({ kind: "denied" }, t)).toEqual({
      text: "Denied",
      severity: "error",
      fix: "open_settings",
    });
    expect(permissionLine({ kind: "unavailable", reason: "坏了" }, t)).toEqual({
      text: "坏了",
      severity: "error",
      fix: null,
    });
  });
});
