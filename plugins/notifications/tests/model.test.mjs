import { describe, expect, test } from "bun:test";
import {
  attentionIndex,
  buildMenu,
  deliveredKey,
  DEFAULT_PREFS,
  dockBadge,
  machineRows,
  notificationId,
  permissionLine,
  prefsAllows,
  sessionTitle,
  statusTitle,
  statusTooltip,
  TERMINAL_MENU_ICON,
} from "../model.js";
import { ACTIVATE_SESSION, SHOW_MAIN_WINDOW } from "../contract.js";

const EN = {
  "fallback.conversation": "Conversation",
  "fallback.terminal": "Terminal",
  "status.needs_you": "Needs you",
  "status.running": "Running",
  "status.unread": "Unread",
  "menu.show_main_window": "Open smelt main window",
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

const PALETTE = { need: "#ff9900", muted: "#666666", faint: "#888888" };
const statusOf = (state) => state.status;
const deps = { t, statusOf };
const item = { kind: "success", title: "", message: "" };

function row(ids, statuses, title, status = statuses[0], machine = null) {
  return { machine, ids, statuses, status, title, icon: TERMINAL_MENU_ICON };
}

describe("偏好与通知 id", () => {
  test("关了的种类不发；终端通知、智能体消息、认不出的新种类没有开关", () => {
    const prefs = { ...DEFAULT_PREFS, success: false };
    expect(prefsAllows(prefs, "success")).toBe(false);
    expect(prefsAllows(prefs, "approval")).toBe(true);
    expect(prefsAllows(prefs, "notice")).toBe(true);
    expect(prefsAllows(prefs, "from_the_future")).toBe(true);
  });

  test("通知 id 分开机器", () => {
    expect(notificationId(null, "s1")).toBe("smelt.notifications/local/s1");
    expect(notificationId("rt", "s1")).toBe("smelt.notifications/remote:rt/s1");
    expect(deliveredKey(null, "s1")).not.toBe(deliveredKey("rt", "s1"));
  });
});

describe("提醒合起来", () => {
  test("没收到快照的机器不算确定；同一个会话 id 在两台机器上各是各的", () => {
    const index = attentionIndex([
      { machine: null, attention: [{ session_id: "a", unread: item }] },
      { machine: "rt", attention: [{ session_id: "b", action: { ...item, kind: "approval" } }] },
      { machine: "late", attention: null },
    ]);
    expect([...index.primed].sort()).toStrictEqual(["", "rt"]);
    expect(index.unread.has(deliveredKey(null, "a"))).toBe(true);
    expect(index.unread.has(deliveredKey("rt", "a"))).toBe(false);
    expect(index.bySession).toStrictEqual(new Map([["a", false], ["b", true]]));
    expect(dockBadge(index.bySession)).toBe("2");
    expect(dockBadge(new Map())).toBe(null);
  });
});

describe("菜单栏的行", () => {
  const entry = (conversations, extra = {}) => ({
    machine: null,
    directory: { conversations },
    states: [],
    facts: [],
    ...extra,
  });

  test("一格都没起过的终端、归档了的不算；名字、状态、图标用宿主算好的", () => {
    const rows = machineRows(
      entry(
        [
          { id: "c0", terminals: [{ id: null }] },
          { id: "c1", terminals: [{ id: null }, { id: "b" }] },
          { id: "c2", acp: { sid: "s", agent: "claude" }, archived_at: 5 },
          { id: "c3", acp: { sid: "s3", agent: "claude" } },
        ],
        {
          states: [{ id: "b", status: "running" }],
          facts: [
            { id: "c1", name: "整段的名字", status: "running", agent: { icon: "smelt-icons/agent-codex.svg" } },
            { id: "c3", name: null, status: "needs_you", agent: null },
          ],
        },
      ),
      deps,
    );
    expect(rows).toStrictEqual([
      {
        machine: null,
        ids: ["b"],
        statuses: ["running"],
        status: "running",
        title: "整段的名字",
        icon: "smelt-icons/agent-codex.svg",
      },
      {
        machine: null,
        ids: ["s3"],
        statuses: ["idle"],
        status: "needs_you",
        title: "Conversation",
        icon: TERMINAL_MENU_ICON,
      },
    ]);
    expect(sessionTitle(rows, "b")).toBe("整段的名字");
    expect(sessionTitle(rows, "nope")).toBe(null);
  });

  test("远程的名字接上机器名；还没名字的终端写通用名", () => {
    const rows = machineRows(
      entry([{ id: "c1", terminals: [{ id: "a" }] }], { machine: "rt", name: "远程机" }),
      deps,
    );
    expect(rows[0].title).toBe("Terminal · 远程机");
    expect(rows[0].machine).toBe("rt");
  });
});

describe("菜单栏的菜单", () => {
  test("标题是要你看的数和运行数，哪项是 0 就省掉", () => {
    expect(statusTitle(0, 0)).toBe("");
    expect(statusTitle(5, 0)).toBe("5@");
    expect(statusTitle(0, 660)).toBe("660");
    expect(statusTitle(5, 660)).toBe("5@·660");
    expect(statusTooltip(0, 0, t)).toBe("smelt");
    expect(statusTooltip(5, 660, t)).toBe("5 sessions need attention, 660 sessions running");
    expect(statusTooltip(1, 0, t)).toBe("1 session needs attention");
  });

  test("列出在跑、挂着提醒的，要你处理的排前面；关掉的对话的提醒也算在数里", () => {
    const rows = [
      row(["idle"], ["idle"], "空闲"),
      row(["run"], ["running"], "在跑"),
      row(["unread"], ["idle"], "没看"),
      row(["ask"], ["running"], "等批准"),
    ];
    const bySession = new Map([
      ["unread", false],
      ["ask", true],
      ["closed", false],
    ]);
    const menu = buildMenu(rows, bySession, PALETTE, t);
    const actions = menu.items.filter((item) => item.type === "action");
    expect(actions.map((item) => item.label)).toStrictEqual([
      "等批准 — Needs you",
      "在跑 — Running",
      "没看 — Unread",
      "Open smelt main window",
    ]);
    expect(actions[0].icon.color).toBe(PALETTE.need);
    expect(menu.title).toBe("3@·2");
  });

  test("分屏的终端跳到挂着提醒的那格，其次是和整段状态一样的那格", () => {
    const split = () => [row(["left", "right"], ["idle", "running"], "分屏", "running")];
    expect(buildMenu(split(), new Map(), PALETTE, t).items[0].args.session_id).toBe("right");
    expect(buildMenu(split(), new Map([["left", false]]), PALETTE, t).items[0].args.session_id).toBe(
      "left",
    );
  });

  test("最后一项是打开主窗口；会话项带机器和会话 id", () => {
    const empty = buildMenu([], new Map(), PALETTE, t);
    expect(empty.title).toBe("");
    expect(empty.items).toStrictEqual([
      { type: "action", label: "Open smelt main window", icon: null, command: SHOW_MAIN_WINDOW, args: null },
    ]);

    const listed = buildMenu([row(["run"], ["running"], "在跑", "running", "box")], new Map(), PALETTE, t);
    expect(listed.items.length).toBe(3);
    expect(listed.items[0].command).toBe(ACTIVATE_SESSION);
    expect(listed.items[0].args).toStrictEqual({ machine: "box", session_id: "run" });
    expect(listed.items[0].icon).toStrictEqual({ png: TERMINAL_MENU_ICON, color: PALETTE.muted });
    expect(listed.items[1]).toStrictEqual({ type: "separator" });
  });
});

describe("系统通知那一行", () => {
  const granted = (banners, badge) => ({ kind: "granted", banners, badge, sound: false });

  test("全开着也给去系统设置的路", () => {
    expect(permissionLine(granted(true, true), t)).toStrictEqual({
      text: "System notifications are allowed",
      severity: "ok",
      fix: "open_settings",
    });
  });

  test("关了横幅或角标的写明是哪项", () => {
    expect(permissionLine(granted(false, true), t).text).toBe("No banners");
    expect(permissionLine(granted(true, false), t).text).toBe("The Dock badge is turned off in the system");
    expect(permissionLine(granted(false, false), t)).toStrictEqual({
      text: "No banners or badge",
      severity: "warning",
      fix: "open_settings",
    });
  });

  test("没问过的先问；拒了的去系统设置；系统报错只写原因；还在查、不支持的没有这一行", () => {
    expect(permissionLine({ kind: "not_requested" }, t).fix).toBe("request");
    expect(permissionLine({ kind: "denied" }, t).severity).toBe("error");
    expect(permissionLine({ kind: "unavailable", reason: "坏了" }, t)).toStrictEqual({
      text: "坏了",
      severity: "error",
      fix: null,
    });
    expect(permissionLine({ kind: "checking" }, t)).toBe(null);
    expect(permissionLine({ kind: "unsupported" }, t)).toBe(null);
  });
});
