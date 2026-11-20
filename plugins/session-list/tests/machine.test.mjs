import { expect, test } from "bun:test";

import { icon_for_os, link, remote_link } from "../machine.js";
import { t } from "./helpers.mjs";

const link_of = (route, rtt_ms, slow) => ({ route, rtt_ms, slow });
const online = { state: "online", version: "1", platform: null };

test("在线的说走哪条路、多慢", () => {
  const direct = remote_link(online, link_of({ kind: "direct" }, 12, false), t);
  expect(direct.light).toBe("good");
  expect(direct.detail).toBe("Direct · 12ms");
  expect(direct.unavailable).toBe(null);
  const slow = remote_link(online, link_of({ kind: "relay", relay: "hk" }, 480, true), t);
  expect(slow.light).toBe("degraded");
  expect(slow.detail).toBe("Relay hk · 480ms");
  expect(remote_link(online, link_of({ kind: "other" }, 30, false), t).detail).toBe("Online · 30ms");
  expect(remote_link(online, null, t).detail).toBe("Online");
});

test("用不了的机器灭灯、写短字", () => {
  const offline = remote_link({ state: "offline", error: "timed out" }, null, t);
  expect(offline.light).toBe("down");
  expect(offline.detail).toBe("Offline: timed out");
  expect(offline.unavailable).toBe("Offline");
  const incompatible = remote_link(
    { state: "incompatible", version: "0.9", detail: "upgrade the remote runtime" },
    link_of({ kind: "direct" }, 8, false),
    t,
  );
  expect(incompatible.light).toBe("down");
  expect(incompatible.detail).toBe("Incompatible version · 0.9 · Direct · 8ms\nupgrade the remote runtime");
  expect(incompatible.unavailable).toBe("Incompatible Version");
  const upgrading = remote_link({ state: "upgrading", progress: "downloading" }, null, t);
  expect(upgrading.light).toBe("degraded");
  expect(upgrading.unavailable).toBe("Upgrading");
  expect(remote_link({ state: "connecting" }, null, t).unavailable).toBe("Offline");
});

test("本机只看机器表", () => {
  expect(link({ machine: null, unavailable: null }, undefined, t).light).toBe("good");
  const down = link({ machine: null, unavailable: "守护没起来" }, undefined, t);
  expect(down.light).toBe("down");
  expect(down.unavailable).toBe("Offline");
});

test("系统图标", () => {
  expect(icon_for_os("Darwin")).toBe("icons/apple.svg");
  expect(icon_for_os("macos")).toBe("icons/apple.svg");
  expect(icon_for_os("linux")).toBe("icons/linux.svg");
  expect(icon_for_os("windows")).toBe("icons/windows.svg");
  expect(icon_for_os("win32")).toBe("icons/windows.svg");
  expect(icon_for_os(null)).toBe("icons/laptop.svg");
});
