import { describe, expect, test } from "bun:test";
import {
  ADD_MACHINE,
  ADD_MACHINE_NAME,
  EDIT_RELAY,
  OWNER,
  RECONNECT,
  REMOVE,
  RENAME,
  UPGRADE,
  VIEW,
} from "../contract.js";
import {
  access_status,
  last_seen_text,
  machine_menu_items,
  machine_status,
  needs_attention,
  short_id,
} from "../status.js";

const EN = {
  online: "Online",
  "machine.connecting": "Connecting…",
  "machine.offline": "Offline: %{error}",
  "machine.incompatible": "Incompatible version: %{detail}",
  "machine.upgrading": "Upgrading: %{progress}",
  "link.direct": "direct · %{ms}ms",
  "link.relay": "relay %{relay} · %{ms}ms",
  "access.off": "Off",
  "access.starting": "Turning on…",
  "access.running": "On · %{relay} · %{id}",
  "access.public_relay": "public relay",
  "access.failed": "Couldn't turn on, retrying: %{error}",
  "seen.never": "Never connected",
  "seen.minutes.one": "Last seen 1 minute ago",
  "seen.minutes.other": "Last seen %{count} minutes ago",
  "seen.hours.one": "Last seen 1 hour ago",
  "seen.hours.other": "Last seen %{count} hours ago",
  "seen.days.one": "Last seen 1 day ago",
  "seen.days.other": "Last seen %{count} days ago",
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

function machine(state, link = null, extra = {}) {
  return {
    machine: "far",
    name: "far",
    relay: null,
    os: null,
    state,
    link,
    upgrade_note: null,
    can_upgrade: false,
    can_reconnect: false,
    ...extra,
  };
}

const online = () => ({
  state: "online",
  version: "2026.1116.0",
  platform: "linux/x86_64",
});

const relay = (rtt_ms, slow) => ({
  route: { kind: "relay", relay: "n0 usw1-1" },
  rtt_ms,
  slow,
});

describe("smelt.machines contract & status", () => {
  test("对外约定 id 共用 smelt.machines 来源", () => {
    expect(ADD_MACHINE).toBe("smelt.machines/add-machine");
    expect(ADD_MACHINE_NAME).toBe("add-machine");
    for (const id of [VIEW, ADD_MACHINE, RECONNECT, UPGRADE, RENAME, EDIT_RELAY, REMOVE]) {
      expect(id.split("/")[0]).toBe(OWNER);
    }
  });

  test("在线写全版本、平台、路由与延迟，慢了标黄", () => {
    expect(machine_status(machine(online()), t)).toEqual({
      text: "Online · 2026.1116.0 · linux/x86_64",
      tone: "muted",
    });
    expect(machine_status(machine(online(), relay(437, true)), t)).toEqual({
      text: "Online · 2026.1116.0 · linux/x86_64 · relay n0 usw1-1 · 437ms",
      tone: "warning",
    });
    const direct = { route: { kind: "direct" }, rtt_ms: 12, slow: false };
    expect(machine_status(machine(online(), direct), t).text).toBe(
      "Online · 2026.1116.0 · linux/x86_64 · direct · 12ms",
    );
    expect(
      machine_status(machine({ state: "offline", error: "connection refused" }), t).text,
    ).toBe("Offline: connection refused");
  });

  test("离线、版本不对、在线却慢、表读不出来要提醒；连接中、升级中、在线不慢不提醒", () => {
    const of = (remotes) => ({ remotes, load_error: null });
    expect(needs_attention(of([]))).toBe(false);
    expect(needs_attention(of([machine(online(), relay(60, false))]))).toBe(false);
    expect(needs_attention(of([machine({ state: "connecting" })]))).toBe(false);
    expect(
      needs_attention(of([machine({ state: "upgrading", progress: "正在升级" })])),
    ).toBe(false);
    expect(needs_attention(of([machine(online(), relay(628, true))]))).toBe(true);
    expect(needs_attention(of([machine({ state: "offline", error: "断了" })]))).toBe(true);
    expect(
      needs_attention(
        of([machine({ state: "incompatible", version: "0.9.0", detail: "" })]),
      ),
    ).toBe(true);
    expect(
      needs_attention({ remotes: [], load_error: "客户端库比这个程序新" }),
    ).toBe(true);
  });

  test("长 id 留头尾各 8 个字符", () => {
    expect(short_id("abc")).toBe("abc");
    expect(short_id("a".repeat(16))).toBe("a".repeat(16));
    expect(short_id("0123456789abcdef0123456789abcdef")).toBe("01234567…89abcdef");
  });

  test("远程访问状态文案与色阶", () => {
    expect(access_status({ state: "off" }, t)).toEqual({ text: "Off", tone: "muted" });
    expect(access_status({ state: "starting" }, t)).toEqual({
      text: "Turning on…",
      tone: "muted",
    });
    expect(
      access_status(
        {
          state: "running",
          endpoint_id: "0123456789abcdef0123456789abcdef",
          relay: null,
        },
        t,
      ),
    ).toEqual({
      text: "On · public relay · 01234567…89abcdef",
      tone: "ok",
    });
    expect(access_status({ state: "failed", error: "dns" }, t)).toEqual({
      text: "Couldn't turn on, retrying: dns",
      tone: "bad",
    });
  });

  test("上次连接时间取最大单位", () => {
    const device = (connected, last_seen_at_ms) => ({
      endpoint_id: "e",
      name: "手机",
      paired_at_ms: 0,
      last_seen_at_ms,
      connected,
    });
    const now = 10 * 86_400_000;
    expect(last_seen_text(device(true, null), now, t)).toBe("Online");
    expect(last_seen_text(device(false, null), now, t)).toBe("Never connected");
    expect(last_seen_text(device(false, now - 5_000), now, t)).toBe("Last seen 1 minute ago");
    expect(last_seen_text(device(false, now - 2 * 3_600_000), now, t)).toBe(
      "Last seen 2 hours ago",
    );
    expect(last_seen_text(device(false, now - 3 * 86_400_000), now, t)).toBe(
      "Last seen 3 days ago",
    );
  });

  test("机器右键菜单按连接能力条件生成", () => {
    const items = machine_menu_items(
      {
        remotes: [
          machine(online(), null, { can_reconnect: false, can_upgrade: true }),
          machine(
            { state: "offline", error: "down" },
            null,
            { machine: "m2", can_reconnect: true, can_upgrade: false },
          ),
        ],
      },
      (key) => ({ en: key }),
    );
    expect(items.filter((i) => i.machine === "far").map((i) => i.command)).toEqual([
      UPGRADE,
      RENAME,
      EDIT_RELAY,
      REMOVE,
    ]);
    expect(items.filter((i) => i.machine === "m2").map((i) => i.command)).toEqual([
      RECONNECT,
      RENAME,
      EDIT_RELAY,
      REMOVE,
    ]);
  });
});
