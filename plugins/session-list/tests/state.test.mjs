import { expect, test } from "bun:test";

import * as peers from "../peers.js";
import { bucket_flip } from "../sidebar.js";
import { conversations_under, id_to_copy, SessionList, step } from "../state.js";
import { fact, fake_host, fake_store, list, local_directory, project, terminals } from "./fakes.mjs";
import { path, t } from "./helpers.mjs";

test("关项目连它工作树里的会话一起关", async () => {
  const { directory, facts } = local_directory();
  const at = (root) => (placed) => path.clean("posix", placed) === root;
  expect(conversations_under(directory, facts, at("/repo"))).toStrictEqual(["w", "p"]);
  expect(conversations_under(directory, facts, at("/tmp/x"))).toStrictEqual(["l"]);
  expect(conversations_under(directory, facts, at("/nowhere"))).toStrictEqual([]);

  const host = fake_host();
  await list(host).close_project(null, " /repo/ ");
  expect(host.calls).toStrictEqual([
    [null, "workspace.conversation.delete", { conversation_id: "w" }],
    [null, "workspace.conversation.delete", { conversation_id: "p" }],
    [null, "workspace.project.close", { root: "/repo" }],
  ]);
});

test("关不掉一段会话就停下，项目留着", async () => {
  const host = fake_host();
  host.fail = (method, params) => params.conversation_id === "w";
  await expect(list(host).close_project(null, "/repo")).rejects.toThrow(/运行时拒绝了/);
  expect(host.calls.length).toBe(1);
});

test("复制的会话 id 优先给智能体自己的", () => {
  const directory = {
    conversations: [{ id: "c", acp: { sid: "acp-1", history_session_id: "h1" } }, terminals("t", "/repo")],
  };
  const reported = { id: "t-t", conversation_id: " tui-9 " };
  expect(id_to_copy(directory, [], "acp-1")).toBe("h1");
  expect(id_to_copy(directory, [reported], "t-t")).toBe("tui-9");
  expect(id_to_copy(directory, [], "t-t")).toBe("t-t");
  expect(id_to_copy(null, [], "acp-1")).toBe("acp-1");
});

test("上一段下一段到头绕回", () => {
  const key = (id) => ({ machine: null, id });
  const order = [key("a"), key("b"), key("c")];
  expect(step(order, key("c"), true)).toStrictEqual(key("a"));
  expect(step(order, key("a"), false)).toStrictEqual(key("c"));
  expect(step(order, key("a"), true)).toStrictEqual(key("b"));
  expect(step(order, null, true)).toStrictEqual(key("a"));
  expect(step(order, key("gone"), false)).toStrictEqual(key("c"));
  expect(step([], null, true)).toBe(null);
});

test("目录版本变了才清库，机器移除了忘掉它", () => {
  const host = fake_host({ "": local_directory(), box: local_directory() });
  const store = fake_store();
  const sessions = list(host, store);
  sessions.known_machines = new Set(["", "box"]);
  sessions.on_event({ type: "sessions_changed" });
  sessions.on_event({ type: "sessions_changed" });
  expect(store.writes.filter(([name]) => name === "prune").length, "两台各清一次").toBe(2);
  host.data[""].directory = { ...host.data[""].directory, revision: 2 };
  sessions.on_event({ type: "sessions_changed" });
  expect(store.writes.filter(([name]) => name === "prune").length).toBe(3);

  delete host.data.box;
  sessions.on_event({ type: "machines_changed" });
  expect(store.writes.at(-1)).toStrictEqual(["forget_machine", "box"]);
  expect(sessions.seen.has("box")).toBeFalsy();
});

test("打开已经开着的项目只选中，没开的先开再带版本选中", async () => {
  const host = fake_host();
  const sessions = list(host);
  await sessions.open_project(null, "/repo/");
  expect(host.calls).toStrictEqual([]);
  expect(host.runs.at(-1)).toStrictEqual([peers.SELECT_PROJECT, { machine: null, root: "/repo" }]);
  await sessions.open_project(null, "/new");
  expect(host.calls).toStrictEqual([[null, "workspace.project.open", { root: "/new" }]]);
  expect(host.runs.at(-1)).toStrictEqual([
    peers.SELECT_PROJECT,
    { machine: null, root: "/new", after: { epoch: "e", revision: 9 } },
  ]);
  await expect(sessions.open_project(null, "  ")).rejects.toMatchObject({ message: t("error.no_project_dir") });
});

test("拖进来的只把本机上的目录当项目打开", async () => {
  const host = fake_host();
  const sessions = list(host);
  await sessions.open_paths(null, ["/dir/a", "/file.txt", "/dir/a"]);
  await new Promise((resolve) => setImmediate(resolve));
  expect(host.calls).toStrictEqual([[null, "workspace.project.open", { root: "/dir/a" }]]);
  const error = console.error;
  console.error = () => {};
  try {
    await sessions.open_paths("box", ["/dir/b"]);
  } finally {
    console.error = error;
  }
  expect(host.calls.length).toBe(1);
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
  expect(sessions.heard_selection).toBeTruthy();
  expect(sessions.selection).toStrictEqual({ kind: "session", key: { machine: null, id: "w" } });
  expect(store.writes.map(([name, fold, collapsed]) => [name, fold.kind, collapsed])).toStrictEqual([
    ["set_collapsed", "project", false],
    ["set_collapsed", "workset", false],
  ]);
});

test("智能体名字没变不重算", () => {
  const host = fake_host();
  const sessions = list(host);
  const ready = { machine: null, state: "ready", settings: { agents: [{ id: "a", name: " Claude " }] } };
  sessions.on_agent_settings(ready);
  expect([...sessions.agent_names.get("")]).toStrictEqual([["a", "Claude"]]);
  const redraws = host.redraws;
  sessions.on_agent_settings(ready);
  expect(host.redraws).toBe(redraws);
  sessions.on_agent_settings({ machine: null, state: "absent" });
  expect(sessions.agent_names.has("")).toBeFalsy();
  expect(host.redraws).toBe(redraws + 1);
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
  expect([...sessions.failures.values()]).toStrictEqual([{ what: "关闭会话", error: "不行" }]);
  sessions.report_row_failure(target, "关闭会话", Promise.resolve());
  expect(sessions.failures.size).toBe(0);
});

test("工作树：没问过的问一次，回到前台时过期的再问", async () => {
  const host = fake_host();
  const sessions = list(host);
  sessions.refresh_worksets(false);
  sessions.refresh_worksets(false);
  expect(host.calls.filter(([, method]) => method === "workspace.workset.inspect").length, "正在问的不再问").toBe(1);
  await new Promise((resolve) => setImmediate(resolve));
  const check = [...sessions.workset_checks.values()][0];
  expect(!check.running && check.last).toBeTruthy();
  sessions.refresh_worksets(true);
  expect(host.calls.length, "刚问过").toBe(1);
  check.last.at -= 6 * 60 * 1000;
  sessions.refresh_worksets(true);
  expect(host.calls.length).toBe(2);
});

test("在列表里找出会话：不在列表里是失败", async () => {
  const host = fake_host();
  const sessions = list(host);
  await sessions.reveal_in_list({ machine: null, session_id: "p-t" });
  expect(sessions.located).toStrictEqual({ machine: null, id: "p" });
  await expect(sessions.reveal_in_list({ machine: null, session_id: "old-t" })).rejects.toThrow();
});

test("露出会话时展开它所在的状态、时间组", () => {
  const sessions = list(fake_host());
  sessions.set_grouping("status");
  const key = { machine: null, id: "p" };
  const bucket = bucket_flip(sessions.sidebar.bucket_of(key));
  sessions.set_bucket_collapsed(bucket, true);
  expect(sessions.ui.collapsed_buckets.has(bucket)).toBeTruthy();
  sessions.reveal(key);
  expect(sessions.ui.collapsed_buckets.has(bucket)).toBeFalsy();
});

test("行内改名：没改不发，会话清空回到运行时的名字，工作树清空不存", async () => {
  const host = fake_host();
  const sessions = list(host);
  const conversation = { kind: "conversation", key: { machine: null, id: "p" } };
  const workset = { kind: "workset", machine: null, id: "ws" };
  sessions.commit_rename(conversation, "旧名", " 旧名 ");
  sessions.commit_rename(workset, "fix", "  ");
  expect(host.calls).toStrictEqual([]);
  sessions.commit_rename(conversation, "旧名", " 新名 ");
  sessions.commit_rename(conversation, "旧名", "");
  sessions.commit_rename(workset, "fix", "fix it");
  expect(host.calls).toStrictEqual([
    [null, "workspace.rename", { conversation_id: "p", terminal_id: null, title: "新名" }],
    [null, "workspace.rename", { conversation_id: "p", terminal_id: null, title: null }],
    [null, "workspace.workset.rename_preserving_branch", { workset_id: "ws", task: "fix it" }],
  ]);
});

test("启动时对端命令尚未登记不弹错，commands_changed 到来后再补拉", async () => {
  const registered = new Set();
  const host = fake_host();
  host.command = (id) => (registered.has(id) ? { id } : null);
  host.answers = {
    "smelt.sessions/selection": { kind: "none" },
    "smelt.agents/agent-settings": [],
  };
  const sessions = list(host);
  await sessions.start();
  expect(host.runs).toStrictEqual([]);
  expect(sessions.agent_settings_failure).toBeNull();

  registered.add("smelt.sessions/selection");
  registered.add("smelt.agents/agent-settings");
  sessions.on_event({ type: "commands_changed" });
  await Promise.resolve();
  expect(host.runs.map(([cmd]) => cmd)).toStrictEqual([
    "smelt.sessions/selection",
    "smelt.agents/agent-settings",
  ]);
});

test("group.changed 里的 group_owned_session_ids 会从侧栏隐藏，linked 会话照常保留", () => {
  const host = fake_host();
  const sessions = list(host);
  sessions.rebuild();
  expect(sessions.sidebar.has_session({ machine: null, id: "p" })).toBe(true);
  expect(sessions.sidebar.has_session({ machine: null, id: "w" })).toBe(true);

  sessions.on_event({
    type: "runtime_topic",
    machine: null,
    topic: "group.changed",
    message: { kind: "snapshot", payload: { group_owned_session_ids: ["p-t"] } },
  });
  expect(sessions.sidebar.has_session({ machine: null, id: "p" })).toBe(false);
  expect(sessions.sidebar.has_session({ machine: null, id: "w" })).toBe(true);

  sessions.on_event({
    type: "runtime_topic",
    machine: null,
    topic: "group.changed",
    message: { kind: "event", payload: { group_owned_session_ids: [] } },
  });
  expect(sessions.sidebar.has_session({ machine: null, id: "p" })).toBe(true);
});
