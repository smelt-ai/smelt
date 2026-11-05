// board.js 的分列测试。在仓库根目录跑：node --test plugins/kanban/tests/

import { test } from "node:test";
import assert from "node:assert/strict";
import { columns } from "../board.js";

const LOCAL = { id: "local", name: "本机", local: true, online: true };
const BOX = { id: "r1", name: "构建机", local: false, online: false };

function session(fields) {
  return {
    id: "s",
    title: "会话",
    kind: "conversation",
    agent: "claude",
    status: "idle",
    unread: false,
    cwd: null,
    project: null,
    machine: LOCAL,
    model: null,
    context: null,
    updated_at: 0,
    turn_started_at: 0,
    viewing: false,
    ...fields,
  };
}

test("按宿主的三态分列，不另造状态", () => {
  const board = columns(
    [
      session({ id: "a", status: "running" }),
      session({ id: "b", status: "needs_you" }),
      session({ id: "c", status: "idle" }),
    ],
    [],
  );
  assert.deepEqual(
    board.map((column) => [column.status, column.cards.map((card) => card.id)]),
    [
      ["needs_you", ["b"]],
      ["running", ["a"]],
      ["idle", ["c"]],
    ],
  );
});

test("未读在前，其余按最近活动从新到旧", () => {
  const [, , idle] = columns(
    [
      session({ id: "old", updated_at: 1 }),
      session({ id: "new", updated_at: 3 }),
      session({ id: "unread", updated_at: 2, unread: true }),
    ],
    [],
  );
  assert.deepEqual(
    idle.cards.map((card) => card.id),
    ["unread", "new", "old"],
  );
});

test("项目用侧栏显示名，远程写上机器名并标出离线", () => {
  const machines = [
    { ...LOCAL, projects: [{ root: "/w/smelt", name: "smelt", opened: true, pinned: false, active: true }] },
    { ...BOX, projects: [{ root: "/w/smelt", name: "smelt · 构建", opened: true, pinned: false, active: false }] },
  ];
  const [, , idle] = columns(
    [
      session({ id: "here", project: "/w/smelt", updated_at: 2 }),
      session({ id: "there", project: "/w/smelt", machine: BOX, updated_at: 1 }),
      session({ id: "loose", project: "/tmp/scratch/", updated_at: 0 }),
    ],
    machines,
  );
  const [here, there, loose] = idle.cards;
  assert.equal(here.place, "smelt");
  assert.equal(here.offline, false);
  assert.equal(there.place, "smelt · 构建 · 构建机");
  assert.equal(there.offline, true);
  assert.equal(loose.place, "scratch");
});

test("模型和上下文用量只写宿主给了的", () => {
  const [, running] = columns(
    [session({ status: "running", model: "Opus", context: { used: 50, size: 200 } })],
    [],
  );
  assert.equal(running.cards[0].detail, "Opus · 上下文 25%");
  const [, , idle] = columns([session({})], []);
  assert.equal(idle.cards[0].detail, "");
});

test("给了机器就只要那台机器上的会话", () => {
  const sessions = [session({ id: "here" }), session({ id: "there", machine: BOX })];
  const ids = (machine) =>
    columns(sessions, [], machine)
      .flatMap((column) => column.cards)
      .map((card) => card.id);
  assert.deepEqual(ids(null), ["here", "there"]);
  assert.deepEqual(ids("r1"), ["there"]);
  assert.deepEqual(ids("local"), ["here"]);
  assert.deepEqual(ids("gone"), []);
});
