import { expect, test } from "bun:test";

import {
  NEW_CONVERSATION,
  SETTINGS,
  affects_launcher,
  launchable,
  nav_state,
  same_state,
  settings_state,
} from "../model.js";

test("只有不用参数就能开的视图才有入口", () => {
  const indexes = new Set(["smelt.sessions/session"]);
  expect(launchable("smelt.archive/main", false, false, indexes)).toBeTruthy();
  expect(launchable("smelt.agents/settings", true, false, indexes), "设置页").toBeFalsy();
  expect(launchable("smelt.desktop/desktop", false, true, indexes), "按实例建").toBeFalsy();
  expect(launchable("smelt/outlet", false, false, indexes), "出口").toBeFalsy();
  expect(launchable("smelt.launcher/nav", false, false, indexes), "自己").toBeFalsy();
  expect(launchable("smelt.launcher/settings", false, false, indexes), "自己").toBeFalsy();
  expect(launchable("smelt.sessions/session", false, false, indexes), "出口默认内容").toBeFalsy();
});

const view = (id, extra = {}) => ({
  id,
  title: id,
  icon: "icons/x.svg",
  draws_top_bar: false,
  instances: false,
  settings: null,
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
  expect(nav_state(host)).toStrictEqual({
    new_conversation: "新对话 · 写作助手",
    entries: [
      { id: "smelt.archive/main", title: "smelt.archive/main", icon: "icons/x.svg", attention: true, shown: true },
    ],
  });
  expect(nav_state(fake_host()).new_conversation, "命令没登记不出按钮").toBe(null);
});

test("设置按钮：命令登记了才有，设置页挂着角标才带红点", () => {
  const settings = { group: "general", keywords: [] };
  expect(settings_state(fake_host())).toStrictEqual({ available: false, attention: false });
  const host = fake_host({
    views: [view("smelt.files/main"), view("smelt.updater/settings", { settings })],
    commands: { [SETTINGS]: { title: "设置" } },
    badges: { "smelt.files/main": "3" },
  });
  expect(settings_state(host), "不是设置页的角标不算").toStrictEqual({ available: true, attention: false });
  host.view_badge = (id) => (id === "smelt.updater/settings" ? "!" : null);
  expect(settings_state(host)).toStrictEqual({ available: true, attention: true });
});

test("换了语言要重算：标题是按当前语言取好的字", () => {
  expect(affects_launcher({ type: "locale_changed" })).toBeTruthy();
  expect(affects_launcher({ type: "view_badges_changed" })).toBeTruthy();
  expect(affects_launcher({ type: "sessions_changed" })).toBeFalsy();
});

test("状态按字面比", () => {
  expect(same_state({ available: true, attention: false }, { available: true, attention: false })).toBeTruthy();
  expect(same_state({ available: true, attention: false }, { available: true, attention: true })).toBeFalsy();
});
