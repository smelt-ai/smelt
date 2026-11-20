import assert from "node:assert/strict";
import { test } from "node:test";

import {
  NEW_CONVERSATION,
  SETTINGS,
  affects_launcher,
  bar_insets,
  launchable,
  nav_state,
  same_state,
  settings_state,
} from "../model.js";

test("只有不用参数就能开的视图才有入口", () => {
  const indexes = new Set(["smelt.sessions/session"]);
  assert.ok(launchable("smelt.archive/main", false, false, indexes));
  assert.ok(!launchable("smelt.agents/settings", true, false, indexes), "设置页");
  assert.ok(!launchable("smelt.terminal/terminal", false, true, indexes), "按实例建");
  assert.ok(!launchable("smelt/outlet", false, false, indexes), "出口");
  assert.ok(!launchable("smelt.launcher/nav", false, false, indexes), "自己");
  assert.ok(!launchable("smelt.launcher/settings", false, false, indexes), "自己");
  assert.ok(!launchable("smelt.sessions/session", false, false, indexes), "出口默认内容");
});

const view = (id, extra = {}) => ({
  id,
  title: id,
  icon: "icons/x.svg",
  draws_top_bar: false,
  instances: false,
  settings: null,
  fills: [],
  ...extra,
});

function fake_host({ views = [], pages = [], commands = {}, badges = {}, laid_out = [], shown = [] } = {}) {
  return {
    pages: () => pages,
    views: () => views,
    command: (id) => commands[id] ?? null,
    view_badge: (id) => badges[id] ?? null,
    view_laid_out: (id) => laid_out.includes(id),
    view_shown: (id) => shown.includes(id),
  };
}

test("入口列表：布局摆了的不列，角标和露着照实给，新对话取命令标题", () => {
  const host = fake_host({
    views: [
      view("smelt.files/main"),
      view("smelt.archive/main"),
      view("smelt.history/main"),
      view("smelt.agents/settings", { settings: { group: "agents", keywords: [] } }),
    ],
    pages: [{ id: "smelt.app/main", title: "", home: true, index: "smelt.history/main" }],
    commands: { [NEW_CONVERSATION]: { title: "新对话 · 写作助手" } },
    badges: { "smelt.archive/main": "1" },
    laid_out: ["smelt.files/main"],
    shown: ["smelt.archive/main"],
  });
  assert.deepEqual(nav_state(host), {
    new_conversation: "新对话 · 写作助手",
    entries: [{ id: "smelt.archive/main", title: "smelt.archive/main", icon: "icons/x.svg", attention: true, shown: true }],
  });
  assert.equal(nav_state(fake_host()).new_conversation, null, "命令没登记不出按钮");
});

test("设置按钮：命令登记了才有，设置页挂着角标才带红点", () => {
  const settings = { group: "general", keywords: [] };
  assert.deepEqual(settings_state(fake_host()), { available: false, attention: false });
  const host = fake_host({
    views: [view("smelt.files/main"), view("smelt.updater/settings", { settings })],
    commands: { [SETTINGS]: { title: "设置" } },
    badges: { "smelt.files/main": "3" },
  });
  assert.deepEqual(settings_state(host), { available: true, attention: false }, "不是设置页的角标不算");
  host.view_badge = (id) => (id === "smelt.updater/settings" ? "!" : null);
  assert.deepEqual(settings_state(host), { available: true, attention: true });
});

test("换了语言要重算：标题是按当前语言取好的字", () => {
  assert.ok(affects_launcher({ type: "locale_changed" }));
  assert.ok(affects_launcher({ type: "view_badges_changed" }));
  assert.ok(!affects_launcher({ type: "sessions_changed" }));
});

test("状态按字面比", () => {
  assert.ok(same_state({ available: true, attention: false }, { available: true, attention: false }));
  assert.ok(!same_state({ available: true, attention: false }, { available: true, attention: true }));
});

test("一行两头的边距：碰到的角和标准边距取大的，不相加", () => {
  const corner = (width) => ({ width, height: 0 });
  const area = (left, right) => ({
    title_bar: false,
    corners: { top_left: corner(0), top_right: corner(0), bottom_left: corner(left), bottom_right: corner(right) },
    bar_margin: 8,
  });
  assert.deepEqual(bar_insets(area(0, 0)), { left: 8, right: 8 });
  assert.deepEqual(bar_insets(area(12, 0)), { left: 12, right: 8 });
});
