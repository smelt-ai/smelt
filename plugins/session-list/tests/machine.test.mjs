import assert from "node:assert/strict";
import test from "node:test";

import { icon_for_os, link, remote_link } from "../machine.js";
import { t } from "./helpers.mjs";

const link_of = (route, rtt_ms, slow) => ({ route, rtt_ms, slow });
const online = { state: "online", version: "1", platform: null };

test("在线的说走哪条路、多慢", () => {
  const direct = remote_link(online, link_of({ kind: "direct" }, 12, false), t);
  assert.equal(direct.light, "good");
  assert.equal(direct.detail, "Direct · 12ms");
  assert.equal(direct.unavailable, null);
  const slow = remote_link(online, link_of({ kind: "relay", relay: "hk" }, 480, true), t);
  assert.equal(slow.light, "degraded");
  assert.equal(slow.detail, "Relay hk · 480ms");
  assert.equal(remote_link(online, link_of({ kind: "other" }, 30, false), t).detail, "Online · 30ms");
  assert.equal(remote_link(online, null, t).detail, "Online");
});

test("用不了的机器灭灯、写短字", () => {
  const offline = remote_link({ state: "offline", error: "timed out" }, null, t);
  assert.equal(offline.light, "down");
  assert.equal(offline.detail, "Offline: timed out");
  assert.equal(offline.unavailable, "Offline");
  const incompatible = remote_link(
    { state: "incompatible", version: "0.9", detail: "upgrade the remote runtime" },
    link_of({ kind: "direct" }, 8, false),
    t,
  );
  assert.equal(incompatible.light, "down");
  assert.equal(incompatible.detail, "Incompatible version · 0.9 · Direct · 8ms\nupgrade the remote runtime");
  assert.equal(incompatible.unavailable, "Incompatible Version");
  const upgrading = remote_link({ state: "upgrading", progress: "downloading" }, null, t);
  assert.equal(upgrading.light, "degraded");
  assert.equal(upgrading.unavailable, "Upgrading");
  assert.equal(remote_link({ state: "connecting" }, null, t).unavailable, "Offline");
});

test("本机只看机器表", () => {
  assert.equal(link({ machine: null, unavailable: null }, undefined, t).light, "good");
  const down = link({ machine: null, unavailable: "守护没起来" }, undefined, t);
  assert.equal(down.light, "down");
  assert.equal(down.unavailable, "Offline");
});

test("系统图标", () => {
  assert.equal(icon_for_os("Darwin"), "icons/apple.svg");
  assert.equal(icon_for_os("macos"), "icons/apple.svg");
  assert.equal(icon_for_os("linux"), "icons/linux.svg");
  assert.equal(icon_for_os("windows"), "icons/windows.svg");
  assert.equal(icon_for_os("win32"), "icons/windows.svg");
  assert.equal(icon_for_os(null), "icons/laptop.svg");
});
