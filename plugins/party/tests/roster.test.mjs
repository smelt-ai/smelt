// roster.js 的测试。在仓库根目录跑：node --test plugins/party/tests/

import { test } from "node:test";
import assert from "node:assert/strict";
import { LOOSE, launchChoices, roster, stamina } from "../roster.js";

const LOCAL = { id: "local", name: "本机", local: true, online: true };

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

function project(root, fields = {}) {
  return { root, name: root.slice(1), opened: true, pinned: false, active: false, ...fields };
}

test("会话挂到各自项目下，项目以外的归到「其他」", () => {
  const machines = [{ ...LOCAL, projects: [project("/a"), project("/b")] }];
  const team = roster(machines, [
    session({ id: "1", project: "/a" }),
    session({ id: "2", project: null }),
    session({ id: "3", project: "/b", status: "needs_you" }),
  ]);
  assert.equal(team.length, 1);
  assert.deepEqual(
    team[0].projects.map((p) => [p.root, p.members.map((m) => m.id)]),
    [
      ["/a", ["1"]],
      ["/b", ["3"]],
      [LOOSE, ["2"]],
    ],
  );
  assert.equal(team[0].waiting, 1);
});

test("没打开过、也没有会话的项目不显示；会话只算到自己的机器", () => {
  const remote = { id: "r1", name: "构建机", local: false, online: true };
  const machines = [
    { ...LOCAL, projects: [project("/a", { opened: false }), project("/b")] },
    { ...remote, projects: [project("/a")] },
  ];
  const team = roster(machines, [session({ id: "x", project: "/a", machine: remote })]);
  assert.deepEqual(
    team[0].projects.map((p) => p.root),
    ["/b"],
  );
  assert.deepEqual(
    team[1].projects.map((p) => [p.root, p.members.map((m) => m.id)]),
    [["/a", ["x"]]],
  );
});

test("体力是上下文剩余比例，没报用量时不画", () => {
  assert.equal(stamina(null), null);
  assert.equal(stamina({ used: 10, size: 0 }), null);
  assert.equal(stamina({ used: 25, size: 100 }), 0.75);
  assert.equal(stamina({ used: 150, size: 100 }), 0);
});

test("新建选项常用在前、不重复", () => {
  const option = (key, kind) => ({ key, label: key, kind, agent: null });
  const choices = launchChoices({
    common: [option("pi", "conversation")],
    terminal: [option("blank", "terminal")],
    conversation: [option("claude", "conversation"), option("pi", "conversation")],
  });
  assert.deepEqual(
    choices.map((c) => c.key),
    ["pi", "claude", "blank"],
  );
  assert.equal(choices[2].label, "blank · 终端");
});
