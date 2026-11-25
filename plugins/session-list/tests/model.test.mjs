import { expect, test } from "bun:test";

import {
  folds_over,
  forget_machine_ui,
  is_collapsed,
  is_pinned,
  known_ids,
  last_activity,
  listed_session,
  new_sidebar_ui,
  prune_ui,
  selected_session,
  selection_from_wire,
  set_collapsed,
  set_pinned,
  show_avatar,
  title,
} from "../model.js";
import { t } from "./helpers.mjs";

const terminal_session = (id, terminal, cwd) => ({ id, terminals: [{ id: terminal, cwd }], acp: null });
const state = (id, since, turn = 0) => ({ id, phase_since: since, turn_started_at: turn });

test("行首默认显示头像，选不要头像时画图标", () => {
  expect(show_avatar({ kind: "unset" })).toBe(true);
  expect(show_avatar({ kind: "view", id: "other.avatars/avatar" })).toBe(true);
  expect(show_avatar({ kind: "icon" })).toBe(false);
});

test("最近动静只看守护", () => {
  const conversation = terminal_session("c", "t1", "/repo");
  conversation.terminals.push({ id: "t2" });
  const states = [state("t1", 100), state("t2", 50), state("other", 900)];
  expect(last_activity(conversation, states)).toBe(100);
  expect(last_activity(conversation, [state("t2", 50, 120)])).toBe(120);
  expect(last_activity(conversation, [])).toBe(0);
});

test("一轮里换阶段不算动静：只认这一轮开始的时刻", () => {
  const conversation = terminal_session("c", "t1", "/repo");
  // 120 开始这一轮，300 从思考换到调工具。
  expect(last_activity(conversation, [state("t1", 300, 120)])).toBe(120);
});

test("名字：派生事实里的，没有是通用名", () => {
  expect(title({ name: null }, true, t)).toBe("Conversation");
  expect(title({ name: null }, false, t)).toBe("Terminal");
  expect(title({ name: "运行时的" }, true, t)).toBe("运行时的");
});

test("还在的东西：打开的项目加上会话撑起来的组", () => {
  const directory = {
    projects: ["/repo"],
    worksets: [{ id: "ws", project_root: "/repo" }],
    conversations: [{ id: "a" }, { id: "b" }, { id: "auto" }],
  };
  const facts = [
    { id: "a", placement: { kind: "project", root: "/repo", workset: "ws" } },
    { id: "b", placement: { kind: "project", root: "/tmp/x", workset: null } },
    { id: "auto", placement: { kind: "hidden" } },
  ];
  const known = known_ids(directory, facts);
  expect(known.roots.has("/repo") && known.roots.has("/tmp/x")).toBeTruthy();
  expect(known.worksets.has("ws")).toBeTruthy();
  expect(known.conversations.has("auto")).toBeTruthy();
});

test("按守护会话 id 找列表里的会话：归档的、找不到的没有", () => {
  const directory = { conversations: [terminal_session("c", "t1", "/repo")] };
  const listed = [{ id: "c", placement: { kind: "project", root: "/repo", workset: null } }];
  expect(listed_session("box", "t1", directory, listed)).toStrictEqual({ machine: "box", id: "c" });
  expect(listed_session("box", "t9", directory, listed)).toBe(null);
  expect(listed_session("box", "t1", null, listed)).toBe(null);
  expect(listed_session("box", "t1", directory, [{ id: "c", placement: { kind: "hidden" } }])).toBe(null);
});

test("选中照线上的形状读", () => {
  const session = selection_from_wire({ kind: "conversation", machine: "box", conversation_id: "c" });
  expect(selected_session(session)).toStrictEqual({ machine: "box", id: "c" });
  const project = selection_from_wire({ kind: "project", machine: null, root: "/repo" });
  expect(selected_session(project)).toBe(null);
  expect(project).toStrictEqual({ kind: "project", machine: null, root: "/repo" });
  expect(selection_from_wire({ kind: "none" })).toStrictEqual({ kind: "none" });
});

test("内存里的界面状态照库的规则改", () => {
  const ui = new_sidebar_ui();
  const a = { kind: "conversation", machine: null, id: "a" };
  const b = { kind: "conversation", machine: null, id: "b" };
  expect(set_pinned(ui, b, true)).toBeTruthy();
  expect(set_pinned(ui, a, true)).toBeTruthy();
  expect(set_pinned(ui, a, true)).toBeFalsy();
  expect(ui.machines.get("").pinned_conversations).toStrictEqual(["b", "a"]);
  expect(set_pinned(ui, b, false)).toBeTruthy();
  expect(is_pinned(ui, b)).toBeFalsy();

  const fold = { kind: "workset", machine: null, id: "w" };
  expect(set_collapsed(ui, fold, true)).toBeTruthy();
  expect(is_collapsed(ui, fold)).toBeTruthy();
  expect(set_collapsed(ui, { kind: "conversations" }, true)).toBeTruthy();
  expect(is_collapsed(ui, { kind: "conversations" })).toBeTruthy();

  prune_ui(ui, null, { conversations: new Set(["a"]), roots: new Set(), worksets: new Set() });
  const local = ui.machines.get("");
  expect(local.pinned_conversations).toStrictEqual(["a"]);
  expect(local.collapsed_worksets.size).toBe(0);
});

test("忘掉机器连它的组折叠一起清", () => {
  const ui = new_sidebar_ui();
  set_pinned(ui, { kind: "project", machine: "box", root: "/r" }, true);
  ui.project_order = [
    { machine: "box", root: "/r" },
    { machine: null, root: "/l" },
  ];
  ui.collapsed_machines.add("box");
  ui.collapsed_buckets.add("bucket:box:Idle");
  ui.collapsed_buckets.add("bucket:@local:Idle");
  ui.collapsed_buckets.add("bucket:Idle");
  forget_machine_ui(ui, "box");
  expect(ui.machines.has("box")).toBeFalsy();
  expect(ui.project_order).toStrictEqual([{ machine: null, root: "/l" }]);
  expect(ui.collapsed_machines.size).toBe(0);
  expect([...ui.collapsed_buckets]).toStrictEqual(["bucket:@local:Idle", "bucket:Idle"]);
});

test("盖在会话上面的几层跟着摆法和分组走", () => {
  const in_workset = { kind: "project", root: "/repo", workset: "w" };
  expect(folds_over("box", in_workset, "project")).toStrictEqual([
    { kind: "project", machine: "box", root: "/repo" },
    { kind: "workset", machine: "box", id: "w" },
  ]);
  expect(folds_over("box", { kind: "project", root: "/repo", workset: null }, "project")).toStrictEqual([
    { kind: "project", machine: "box", root: "/repo" },
  ]);
  // 按状态、时间分组时不画项目、工作集，不去改它们存着的折叠。
  expect(folds_over("box", in_workset, "status")).toStrictEqual([]);
  for (const grouping of ["project", "last_updated"]) {
    expect(folds_over("box", { kind: "conversations" }, grouping)).toStrictEqual([{ kind: "conversations" }]);
    expect(folds_over("box", { kind: "hidden" }, grouping)).toStrictEqual([]);
  }
});
