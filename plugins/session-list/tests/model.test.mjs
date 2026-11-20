import assert from "node:assert/strict";
import test from "node:test";

import {
  folds_over,
  forget_machine_ui,
  is_collapsed,
  is_pinned,
  known_ids,
  last_activity,
  listed_session,
  new_sidebar_ui,
  pick_avatar,
  prune_ui,
  selected_session,
  selection_from_wire,
  set_collapsed,
  set_pinned,
  title,
} from "../model.js";
import { t } from "./helpers.mjs";

const terminal_session = (id, terminal, cwd) => ({ id, terminals: [{ id: terminal, cwd }], acp: null });
const state = (id, since, turn = 0) => ({ id, phase_since: since, turn_started_at: turn });

test("头像在填了插槽的视图里挑", () => {
  const filled = ["a.x/avatar", "b.y/avatar"];
  assert.equal(pick_avatar({ kind: "unset" }, filled), "a.x/avatar");
  assert.equal(pick_avatar({ kind: "icon" }, filled), null);
  assert.equal(pick_avatar({ kind: "view", id: "b.y/avatar" }, filled), "b.y/avatar");
  // 选的那个卸了按没选过算。
  assert.equal(pick_avatar({ kind: "view", id: "gone/avatar" }, filled), "a.x/avatar");
  // 没人填，怎么选都是图标。
  assert.equal(pick_avatar({ kind: "unset" }, []), null);
  assert.equal(pick_avatar({ kind: "view", id: "b.y/avatar" }, []), null);
});

test("最近动静只看守护", () => {
  const conversation = terminal_session("c", "t1", "/repo");
  conversation.terminals.push({ id: "t2" });
  const states = [state("t1", 100), state("t2", 50), state("other", 900)];
  assert.equal(last_activity(conversation, states), 100);
  assert.equal(last_activity(conversation, [state("t2", 50, 120)]), 120);
  assert.equal(last_activity(conversation, []), 0);
});

test("名字：派生事实里的，没有是通用名", () => {
  assert.equal(title({ name: null }, true, t), "Conversation");
  assert.equal(title({ name: null }, false, t), "Terminal");
  assert.equal(title({ name: "运行时的" }, true, t), "运行时的");
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
  assert.ok(known.roots.has("/repo") && known.roots.has("/tmp/x"));
  assert.ok(known.worksets.has("ws"));
  assert.ok(known.conversations.has("auto"));
});

test("按守护会话 id 找列表里的会话：归档的、找不到的没有", () => {
  const directory = { conversations: [terminal_session("c", "t1", "/repo")] };
  const listed = [{ id: "c", placement: { kind: "project", root: "/repo", workset: null } }];
  assert.deepEqual(listed_session("box", "t1", directory, listed), { machine: "box", id: "c" });
  assert.equal(listed_session("box", "t9", directory, listed), null);
  assert.equal(listed_session("box", "t1", null, listed), null);
  assert.equal(listed_session("box", "t1", directory, [{ id: "c", placement: { kind: "hidden" } }]), null);
});

test("选中照线上的形状读", () => {
  const session = selection_from_wire({ kind: "conversation", machine: "box", conversation_id: "c" });
  assert.deepEqual(selected_session(session), { machine: "box", id: "c" });
  const project = selection_from_wire({ kind: "project", machine: null, root: "/repo" });
  assert.equal(selected_session(project), null);
  assert.deepEqual(project, { kind: "project", machine: null, root: "/repo" });
  assert.deepEqual(selection_from_wire({ kind: "none" }), { kind: "none" });
});

test("内存里的界面状态照库的规则改", () => {
  const ui = new_sidebar_ui();
  const a = { kind: "conversation", machine: null, id: "a" };
  const b = { kind: "conversation", machine: null, id: "b" };
  assert.ok(set_pinned(ui, b, true));
  assert.ok(set_pinned(ui, a, true));
  assert.ok(!set_pinned(ui, a, true));
  assert.deepEqual(ui.machines.get("").pinned_conversations, ["b", "a"]);
  assert.ok(set_pinned(ui, b, false));
  assert.ok(!is_pinned(ui, b));

  const fold = { kind: "workset", machine: null, id: "w" };
  assert.ok(set_collapsed(ui, fold, true));
  assert.ok(is_collapsed(ui, fold));
  assert.ok(set_collapsed(ui, { kind: "conversations" }, true));
  assert.ok(is_collapsed(ui, { kind: "conversations" }));

  prune_ui(ui, null, { conversations: new Set(["a"]), roots: new Set(), worksets: new Set() });
  const local = ui.machines.get("");
  assert.deepEqual(local.pinned_conversations, ["a"]);
  assert.equal(local.collapsed_worksets.size, 0);
});

test("忘掉机器连它的组折叠一起清", () => {
  const ui = new_sidebar_ui();
  set_pinned(ui, { kind: "project", machine: "box", root: "/r" }, true);
  ui.project_order = [{ machine: "box", root: "/r" }, { machine: null, root: "/l" }];
  ui.collapsed_machines.add("box");
  ui.collapsed_buckets.add("bucket:box:Idle");
  ui.collapsed_buckets.add("bucket:@local:Idle");
  ui.collapsed_buckets.add("bucket:Idle");
  forget_machine_ui(ui, "box");
  assert.ok(!ui.machines.has("box"));
  assert.deepEqual(ui.project_order, [{ machine: null, root: "/l" }]);
  assert.equal(ui.collapsed_machines.size, 0);
  assert.deepEqual([...ui.collapsed_buckets], ["bucket:@local:Idle", "bucket:Idle"]);
});

test("盖在会话上面的几层跟着摆法和分组走", () => {
  const in_workset = { kind: "project", root: "/repo", workset: "w" };
  assert.deepEqual(folds_over("box", in_workset, "project"), [
    { kind: "project", machine: "box", root: "/repo" },
    { kind: "workset", machine: "box", id: "w" },
  ]);
  assert.deepEqual(folds_over("box", { kind: "project", root: "/repo", workset: null }, "project"), [
    { kind: "project", machine: "box", root: "/repo" },
  ]);
  // 按状态、时间分组时不画项目、工作集，不去改它们存着的折叠。
  assert.deepEqual(folds_over("box", in_workset, "status"), []);
  for (const grouping of ["project", "last_updated"]) {
    assert.deepEqual(folds_over("box", { kind: "conversations" }, grouping), [{ kind: "conversations" }]);
    assert.deepEqual(folds_over("box", { kind: "hidden" }, grouping), []);
  }
});
