import assert from "node:assert/strict";
import test from "node:test";

import { initial_location, NewSession, pinned_by_digit, slots } from "../new_session.js";
import { AGENT_SETTINGS, CREATE_CONVERSATION } from "../peers.js";
import { fake_host, list, local_directory } from "./fakes.mjs";
import { t } from "./helpers.mjs";

const settle = () => new Promise(setImmediate);
const row = (section, key) => ({ section, action: { key, label: key, target: "blankTerminal" } });
const action = (key, target, more = {}) => ({ key, label: key, target, ...more });

/** 一份开着的菜单：owner 记下关了几次。 */
function open_menu(sessions, location) {
  const owner = { palette: false, closed: 0, redraw: () => {}, close: () => (owner.closed += 1) };
  const menu = new NewSession(sessions, location, owner);
  return { menu, owner };
}

test("常用在前、不加标题，和下面隔一条线；终端、对话各带组标题", () => {
  assert.deepEqual(slots([row(0, "a"), row(0, "b"), row(1, "c"), row(2, "d")]), [
    { kind: "row", index: 0 },
    { kind: "row", index: 1 },
    { kind: "separator" },
    { kind: "header", section: 1 },
    { kind: "row", index: 2 },
    { kind: "header", section: 2 },
    { kind: "row", index: 3 },
  ]);
  assert.deepEqual(slots([row(1, "a"), row(2, "b")]), [
    { kind: "header", section: 1 },
    { kind: "row", index: 0 },
    { kind: "header", section: 2 },
    { kind: "row", index: 1 },
  ]);
  assert.deepEqual(slots([row(0, "a")]), [{ kind: "row", index: 0 }]);
});

test("数字键只挑常用项", () => {
  const rows = [row(0, "a"), row(0, "b"), row(1, "c")];
  assert.equal(pinned_by_digit(rows, "1"), 0);
  assert.equal(pinned_by_digit(rows, "2"), 1);
  assert.equal(pinned_by_digit(rows, "3"), null);
  assert.equal(pinned_by_digit(rows, "0"), null);
  assert.equal(pinned_by_digit(rows, "a"), null);
});

test("预选位置：给的路径落在工作树里选工作树，是项目根选项目，没给按活动项目再按第一个", () => {
  const host = fake_host();
  assert.deepEqual(initial_location(host, null, "/repo/worktrees/fix/sub"), { machine: null, root: "/repo", workset: "ws" });
  assert.deepEqual(initial_location(host, null, "/repo/"), { machine: null, root: "/repo/", workset: null });
  assert.deepEqual(initial_location(host, null, "/new"), { machine: null, root: "/new", workset: null });
  assert.deepEqual(initial_location(host, undefined, null), { machine: null, root: "/repo", workset: null });
  host.active = { machine: null, root: "/active" };
  assert.deepEqual(initial_location(host, undefined, null), { machine: null, root: "/active", workset: null });
});

test("在工作树里建空白终端：先关菜单再建，会话落在任务目录", async () => {
  const host = fake_host();
  host.replies["new_session.catalog"] = { sections: { terminal: [action("blank", "blankTerminal")] } };
  const sessions = list(host);
  const { menu, owner } = open_menu(sessions, { machine: null, root: "/repo", workset: "ws" });
  assert.equal(menu.location, "repo / fix");
  await settle();
  assert.equal(menu.selected, 0);

  assert.equal(menu.launch(0), true);
  assert.equal(owner.closed, 1);
  assert.equal(sessions.launchers.has(menu), false);
  const [[command, args]] = host.runs;
  assert.equal(command, CREATE_CONVERSATION);
  assert.equal(args.conversation.workset_id, "ws");
  assert.deepEqual(
    args.conversation.terminals.map((terminal) => terminal.cwd),
    ["/repo/worktrees/fix"],
  );
});

test("建不成的留着菜单，把原因写在里面", async () => {
  const host = fake_host();
  host.replies["new_session.catalog"] = {
    sections: { common: [action("pi", "conversation", { agentOptionId: "pi" })], terminal: [action("blank", "blankTerminal")] },
  };
  const sessions = list(host);
  const { menu, owner } = open_menu(sessions, { machine: null, root: null, workset: null });
  await settle();
  assert.equal(menu.launch(1), false);
  assert.equal(menu.error, "Choose a project first");

  menu.set_location(null, "/repo", null);
  assert.equal(menu.error, null);
  assert.equal(menu.launch(0), false);
  assert.match(menu.error, /agent settings/i);

  sessions.on_agent_settings({ machine: null, state: "ready", settings: { agents: [], options: ["pi"] } });
  assert.equal(menu.launch(0), true);
  assert.equal(owner.closed, 1);
  assert.deepEqual(host.runs[0][1].conversation.acp, { option: "pi", sid: "acp-id-3", cwd: "/repo" });
});

test("换了机器，上一台晚回来的目录丢掉", async () => {
  const host = fake_host({ "": local_directory(), far: local_directory() });
  const pending = [];
  host.replies["new_session.catalog"] = () => new Promise((resolve) => pending.push(resolve));
  const { menu } = open_menu(list(host), { machine: null, root: "/repo", workset: null });
  menu.set_location("far", "/repo", null);
  assert.equal(pending.length, 2);

  pending[0]({ sections: { terminal: [action("old", "blankTerminal")] } });
  await settle();
  assert.equal(menu.catalog.state, "loading");
  pending[1]({ sections: { terminal: [action("new", "blankTerminal")] } });
  await settle();
  assert.deepEqual(menu.rows.map((row) => row.action.key), ["new"]);
});

test("从这里建的工作树推到了就选上", async () => {
  const host = fake_host();
  host.replies["new_session.catalog"] = { sections: {} };
  const sessions = list(host);
  const { menu } = open_menu(sessions, { machine: null, root: "/repo", workset: null });
  menu.await_workset("/repo", "search");
  assert.equal(menu.workset, null);

  host.data[""].directory.worksets.push({ id: "ws2", project_root: "/repo", task: "search", repositories: [] });
  menu.on_event({ type: "sessions_changed" });
  assert.equal(menu.workset, "ws2");
  assert.equal(menu.location, "repo / search");
});

test("启动时没读到智能体配置：菜单写出原因，下次打开再读", async () => {
  const host = fake_host();
  host.replies["new_session.catalog"] = { sections: { common: [action("pi", "conversation", { agentOptionId: "pi" })] } };
  const run = host.run;
  let settings_reply = () => Promise.reject(new Error("智能体插件没起来"));
  host.run = (command, args) => (command === AGENT_SETTINGS ? settings_reply() : run(command, args));
  const sessions = list(host);
  await sessions.read_agent_settings().catch(() => {});

  const first = open_menu(sessions, { machine: null, root: "/repo", workset: null });
  await settle();
  assert.equal(first.menu.launch(0), false);
  assert.equal(
    first.menu.error,
    t("error.agent_settings_unavailable", { reason: t("error.read_agent_settings", { error: "智能体插件没起来" }) }),
  );
  first.menu.dispose();

  settings_reply = async () => [{ machine: null, state: "ready", settings: { agents: [], options: ["pi"] } }];
  const second = open_menu(sessions, { machine: null, root: "/repo", workset: null });
  await settle();
  assert.equal(sessions.agent_settings_failure, null);
  assert.equal(second.menu.launch(0), true);
});
