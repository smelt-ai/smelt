import assert from "node:assert/strict";
import test from "node:test";

import { new_sidebar_ui } from "../model.js";
import * as peers from "../peers.js";
import { conversations_under, id_to_copy, SessionList, step } from "../state.js";
import { path, t } from "./helpers.mjs";

/** 假宿主：一台本机，目录、派生事实、会话状态都按 machines 给；记下调用和重画。 */
function fake_host(machines = { "": local_directory() }) {
  const calls = [];
  const runs = [];
  const host = {
    calls,
    runs,
    redraws: 0,
    clipboard: null,
    data: machines,
    machines: () => Object.keys(host.data).map((key) => ({ machine: key === "" ? null : key, name: key || "本机", unavailable: null })),
    connections: () => ({ remotes: [] }),
    local_os: "darwin",
    path_style: () => "posix",
    workspace_directory: (machine) => host.data[machine ?? ""]?.directory ?? null,
    session_states: (machine) => host.data[machine ?? ""]?.states ?? [],
    conversation_facts: (machine) => host.data[machine ?? ""]?.facts ?? [],
    path,
    async call(machine, method, params) {
      calls.push([machine, method, params]);
      if (host.fail?.(method, params)) throw new Error("运行时拒绝了");
      return method === "workspace.project.open" ? { epoch: "e", revision: 9 } : null;
    },
    async run(command, args) {
      runs.push([command, args]);
      if (command === "smelt.app/open-view") return true;
      return host.answers?.[command] ?? null;
    },
    t,
    redraw: () => (host.redraws += 1),
    write_clipboard: (text) => (host.clipboard = text),
    is_dir: async (dir) => dir.startsWith("/dir"),
    now: () => new Date(2026, 10, 20, 12),
  };
  return host;
}

function fake_store() {
  const writes = [];
  const record = (name) => (...args) => writes.push([name, ...args]);
  return {
    writes,
    set_pinned: record("set_pinned"),
    set_collapsed: record("set_collapsed"),
    set_project_order: record("set_project_order"),
    set_conversation_order: record("set_conversation_order"),
    save_avatar_choice: record("save_avatar_choice"),
    prune: record("prune"),
    forget_machine: record("forget_machine"),
  };
}

const project = (root, workset = null) => ({ kind: "project", root, workset });
const fact = (id, placement) => ({ id, name: id, status: "idle", agent: null, automation: false, cwd: null, placement });

function terminals(id, cwd) {
  return { id, terminals: [{ id: `${id}-t`, cwd }], created_at: 1 };
}

function local_directory() {
  const directory = {
    epoch: "e",
    revision: 1,
    projects: ["/repo"],
    repositories: [],
    worksets: [{ id: "ws", project_root: "/repo", task: "fix", repositories: [] }],
    conversations: [
      { ...terminals("w", "/elsewhere"), workset_id: "ws" },
      terminals("p", "/repo/sub"),
      terminals("l", "/tmp/x"),
      { ...terminals("old", "/repo"), archived_at: 1 },
    ],
  };
  const facts = [
    fact("w", project("/repo", "ws")),
    fact("p", project("/repo")),
    fact("l", project("/tmp/x")),
    fact("old", { kind: "hidden" }),
  ];
  return { directory, facts, states: [] };
}

function list(host = fake_host(), store = fake_store()) {
  return new SessionList(host, store, new_sidebar_ui(), { kind: "unset" });
}

test("关项目连它工作树里的会话一起关", async () => {
  const { directory, facts } = local_directory();
  const at = (root) => (placed) => path.clean("posix", placed) === root;
  assert.deepEqual(conversations_under(directory, facts, at("/repo")), ["w", "p"]);
  assert.deepEqual(conversations_under(directory, facts, at("/tmp/x")), ["l"]);
  assert.deepEqual(conversations_under(directory, facts, at("/nowhere")), []);

  const host = fake_host();
  await list(host).close_project(null, " /repo/ ");
  assert.deepEqual(host.calls, [
    [null, "workspace.conversation.delete", { conversation_id: "w" }],
    [null, "workspace.conversation.delete", { conversation_id: "p" }],
    [null, "workspace.project.close", { root: "/repo" }],
  ]);
});

test("关不掉一段会话就停下，项目留着", async () => {
  const host = fake_host();
  host.fail = (method, params) => params.conversation_id === "w";
  await assert.rejects(list(host).close_project(null, "/repo"), /运行时拒绝了/);
  assert.equal(host.calls.length, 1);
});

test("复制的会话 id 优先给智能体自己的", () => {
  const directory = {
    conversations: [
      { id: "c", acp: { sid: "acp-1", history_session_id: "h1" } },
      terminals("t", "/repo"),
    ],
  };
  const reported = { id: "t-t", conversation_id: " tui-9 " };
  assert.equal(id_to_copy(directory, [], "acp-1"), "h1");
  assert.equal(id_to_copy(directory, [reported], "t-t"), "tui-9");
  assert.equal(id_to_copy(directory, [], "t-t"), "t-t");
  assert.equal(id_to_copy(null, [], "acp-1"), "acp-1");
});

test("上一段下一段到头绕回", () => {
  const key = (id) => ({ machine: null, id });
  const order = [key("a"), key("b"), key("c")];
  assert.deepEqual(step(order, key("c"), true), key("a"));
  assert.deepEqual(step(order, key("a"), false), key("c"));
  assert.deepEqual(step(order, key("a"), true), key("b"));
  assert.deepEqual(step(order, null, true), key("a"));
  assert.deepEqual(step(order, key("gone"), false), key("c"));
  assert.equal(step([], null, true), null);
});

test("目录版本变了才清库，机器移除了忘掉它", () => {
  const host = fake_host({ "": local_directory(), box: local_directory() });
  const store = fake_store();
  const sessions = list(host, store);
  sessions.known_machines = new Set(["", "box"]);
  sessions.on_event({ type: "sessions_changed" });
  sessions.on_event({ type: "sessions_changed" });
  assert.equal(store.writes.filter(([name]) => name === "prune").length, 2, "两台各清一次");
  host.data[""].directory = { ...host.data[""].directory, revision: 2 };
  sessions.on_event({ type: "sessions_changed" });
  assert.equal(store.writes.filter(([name]) => name === "prune").length, 3);

  delete host.data.box;
  sessions.on_event({ type: "machines_changed" });
  assert.deepEqual(store.writes.at(-1), ["forget_machine", "box"]);
  assert.ok(!sessions.seen.has("box"));
});

test("打开已经开着的项目只选中，没开的先开再带版本选中", async () => {
  const host = fake_host();
  const sessions = list(host);
  await sessions.open_project(null, "/repo/");
  assert.deepEqual(host.calls, []);
  assert.deepEqual(host.runs.at(-1), [peers.SELECT_PROJECT, { machine: null, root: "/repo" }]);
  await sessions.open_project(null, "/new");
  assert.deepEqual(host.calls, [[null, "workspace.project.open", { root: "/new" }]]);
  assert.deepEqual(host.runs.at(-1), [
    peers.SELECT_PROJECT,
    { machine: null, root: "/new", after: { epoch: "e", revision: 9 } },
  ]);
  await assert.rejects(sessions.open_project(null, "  "), { message: t("error.no_project_dir") });
});

test("拖进来的只把本机上的目录当项目打开", async () => {
  const host = fake_host();
  const sessions = list(host);
  await sessions.open_paths(null, ["/dir/a", "/file.txt", "/dir/a"]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(host.calls, [[null, "workspace.project.open", { root: "/dir/a" }]]);
  const error = console.error;
  console.error = () => {};
  try {
    await sessions.open_paths("box", ["/dir/b"]);
  } finally {
    console.error = error;
  }
  assert.equal(host.calls.length, 1);
});

test("选中的会话露出来：展开它的项目和工作树", () => {
  const host = fake_host();
  const store = fake_store();
  const sessions = list(host, store);
  sessions.set_collapsed({ kind: "project", machine: null, root: "/repo" }, true);
  sessions.set_collapsed({ kind: "workset", machine: null, id: "ws" }, true);
  store.writes.length = 0;
  sessions.on_event({
    type: "plugin",
    name: peers.SELECTION_CHANGED,
    payload: { kind: "conversation", machine: null, conversation_id: "w" },
  });
  assert.ok(sessions.heard_selection);
  assert.deepEqual(sessions.selection, { kind: "session", key: { machine: null, id: "w" } });
  assert.deepEqual(
    store.writes.map(([name, fold, collapsed]) => [name, fold.kind, collapsed]),
    [
      ["set_collapsed", "project", false],
      ["set_collapsed", "workset", false],
    ],
  );
});

test("智能体名字没变不重算", () => {
  const host = fake_host();
  const sessions = list(host);
  const ready = { machine: null, state: "ready", settings: { agents: [{ id: "a", name: " Claude " }] } };
  sessions.on_agent_settings(ready);
  assert.deepEqual([...sessions.agent_names.get("")], [["a", "Claude"]]);
  const redraws = host.redraws;
  sessions.on_agent_settings(ready);
  assert.equal(host.redraws, redraws);
  sessions.on_agent_settings({ machine: null, state: "absent" });
  assert.ok(!sessions.agent_names.has(""));
  assert.equal(host.redraws, redraws + 1);
});

test("行上的失败：再发起先摘掉，失败了挂回去", async () => {
  const host = fake_host();
  const sessions = list(host);
  const target = { kind: "conversation", key: { machine: null, id: "p" } };
  const error = console.error;
  console.error = () => {};
  try {
    sessions.report_row_failure(target, "关闭会话", Promise.reject(new Error("不行")));
    await new Promise((resolve) => setImmediate(resolve));
  } finally {
    console.error = error;
  }
  assert.deepEqual([...sessions.failures.values()], [{ what: "关闭会话", error: "不行" }]);
  sessions.report_row_failure(target, "关闭会话", Promise.resolve());
  assert.equal(sessions.failures.size, 0);
});

test("工作树：没问过的问一次，回到前台时过期的再问", async () => {
  const host = fake_host();
  const sessions = list(host);
  sessions.refresh_worksets(false);
  sessions.refresh_worksets(false);
  assert.equal(host.calls.filter(([, method]) => method === "workspace.workset.inspect").length, 1, "正在问的不再问");
  await new Promise((resolve) => setImmediate(resolve));
  const check = [...sessions.workset_checks.values()][0];
  assert.ok(!check.running && check.last);
  sessions.refresh_worksets(true);
  assert.equal(host.calls.length, 1, "刚问过");
  check.last.at -= 6 * 60 * 1000;
  sessions.refresh_worksets(true);
  assert.equal(host.calls.length, 2);
});

test("在列表里找出会话：不在列表里是失败", async () => {
  const host = fake_host();
  const sessions = list(host);
  await sessions.reveal_in_list({ machine: null, session_id: "p-t" });
  assert.deepEqual(sessions.located, { machine: null, id: "p" });
  await assert.rejects(sessions.reveal_in_list({ machine: null, session_id: "old-t" }));
});
