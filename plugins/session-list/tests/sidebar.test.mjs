import assert from "node:assert/strict";
import test from "node:test";

import { Unread } from "../attention.js";
import { new_sidebar_ui, set_pinned } from "../model.js";
import {
  Sidebar,
  area,
  bucket_flip,
  bucket_groups,
  build_sidebar,
  machine_flip,
  manual_first,
  merge_project_order,
  move_near,
  move_to_pin_boundary,
  pinned_first,
  project_flip,
  project_matches,
  project_name,
  project_visible,
  reordered,
  row_icon,
  session_flip,
  splice_group,
} from "../sidebar.js";
import { path, t } from "./helpers.mjs";

test("手动排过的在前，其余保持原来的顺序", () => {
  assert.deepEqual(
    manual_first(["a", "b", "c", "d"], ["c", "x", "a"], (item, m) => item === m),
    ["c", "a", "b", "d"],
  );
});

test("置顶提前是稳定的", () => {
  assert.deepEqual(pinned_first(["a", "b", "c", "d"], (s) => s === "c" || s === "a"), ["a", "c", "b", "d"]);
});

test("只有共有行的先后变了才算重排", () => {
  const order = ["a", "b", "c"];
  assert.ok(!reordered(order, order));
  // 新建、关掉、折起只是多了少了行。
  assert.ok(!reordered(order, ["new", "a", "b", "c"]));
  assert.ok(!reordered(order, ["a", "c"]));
  assert.ok(reordered(order, ["b", "a", "c"]));
  assert.ok(reordered(order, ["c", "new", "a"]));
});

test("挪到前面、后面", () => {
  const items = ["a", "b", "c", "d"];
  assert.ok(move_near(items, "d", "b", true));
  assert.deepEqual(items, ["a", "d", "b", "c"]);
  assert.ok(move_near(items, "a", "c", false));
  assert.deepEqual(items, ["d", "b", "c", "a"]);
  assert.ok(!move_near(items, "b", "c", true), "已经在那儿了");
  assert.ok(!move_near(items, "b", "b", true));
  assert.ok(!move_near(items, "z", "b", true));
  assert.deepEqual(items, ["d", "b", "c", "a"]);
});

test("换一组的顺序不动别的组", () => {
  assert.deepEqual(splice_group(["a1", "b1", "a2", "b2", "a3"], ["a3", "a1", "a2"]), [
    "a3",
    "b1",
    "a1",
    "b2",
    "a2",
  ]);
});

test("没显示的机器上的项目留在顺序里", () => {
  // x 在没连上的机器上：列表里没有它，拖完照样留在原来跟着的那一项后面。
  assert.deepEqual(merge_project_order(["x", "a", "b"], ["c", "a", "b"]), ["x", "c", "a", "b"]);
  assert.deepEqual(merge_project_order(["a", "x", "b"], ["b", "a", "c"]), ["b", "a", "x", "c"]);
  // 从没拖过：就是列表里的新顺序。
  assert.deepEqual(merge_project_order([], ["c", "a", "b"]), ["c", "a", "b"]);
});

test("改了置顶落在交界上", () => {
  const order = ["p1", "p2", "a", "b", "c"];
  // 置顶 c：排在 p1、p2 之后。
  assert.deepEqual(
    move_to_pin_boundary(order, "c", (id) => ["p1", "p2", "c"].includes(id)),
    ["p1", "p2", "c", "a", "b"],
  );
  // 取消置顶 p1：排在其余的最前。
  assert.deepEqual(move_to_pin_boundary(order, "p1", (id) => id === "p2"), ["p2", "p1", "a", "b", "c"]);
});

// ---- 按测试里摆好的项目算 ----

const row = (id, status, pinned, workset) => ({
  key: { machine: null, id },
  session_id: null,
  title: id,
  icon: { kind: "terminal" },
  status,
  pinned,
  activity: 0,
  turn_started_at: 0,
  unread: false,
  unavailable: null,
  tooltip: id,
  machine_name: null,
  group: { kind: "project", machine: null, root: "/p", workset: workset ?? null },
  origin: null,
});

const project = (name, branch, items) => ({
  machine: null,
  root: `/${name}`,
  name,
  machine_name: null,
  link: { light: "good", detail: "", unavailable: null },
  opened: true,
  pinned: false,
  collapsed: false,
  status: "idle",
  branch,
  repositories: 0,
  sessions: 0,
  items,
});

const workset = (task, sessions) => ({
  kind: "workset",
  workset: {
    machine: null,
    id: task,
    task,
    path: "",
    tooltip: `工作树：${task}`,
    pinned: false,
    collapsed: false,
    status: "idle",
    sessions,
    repositories: [],
    merge: "unknown",
    checking: false,
  },
});

const session = (r) => ({ kind: "session", row: r });
const ids = (rows) => rows.map((r) => r.key.id);

/** 两个项目：本机的 a（分支 main）里一段会话加一个工作树，远程机器 box 上的 b 里一段置顶的、一段要人管的。 */
function two_projects() {
  const projects = [
    project("a", "main", [session(row("r1", "idle", false)), workset("修复", [row("r2", "needs_you", false, "修复")])]),
    project("b", null, [session(row("r3", "idle", true)), session(row("r4", "needs_you", false))]),
  ];
  projects[1].machine = "box";
  projects[1].machine_name = "box";
  return projects;
}

/** 按 ui 摆好的侧栏：machines 为空时不按机器分开，一片装所有项目；否则一台一片。 */
function laid_out(projects, machines, ui) {
  const now = new Date();
  const areas =
    machines.length === 0
      ? [area(undefined, projects, ui, "", now)]
      : machines.map((machine) => area(machine, projects, ui, "", now));
  return new Sidebar({ projects, areas, by_machine: machines.length > 0, grouping: ui.grouping });
}

const keys = (order) => order.map((key) => key.id);

test("按状态分组拿走每个项目里的行，带上来源", () => {
  const ui = new_sidebar_ui();
  ui.grouping = "status";
  ui.collapsed_buckets.add(bucket_flip({ machine: undefined, bucket: "Idle" }));
  const groups = bucket_groups(undefined, two_projects(), ui, new Date());
  assert.deepEqual(
    groups.map((g) => g.key.bucket),
    ["NeedsYou", "Idle"],
  );
  assert.deepEqual(ids(groups[0].rows), ["r2", "r4"]);
  // 组里置顶的在前。
  assert.deepEqual(ids(groups[1].rows), ["r3", "r1"]);
  assert.equal(groups[0].status, "needs_you");
  assert.ok(!groups[0].collapsed);
  assert.ok(groups[1].collapsed);

  const origin = groups[0].rows[0].origin;
  assert.equal(origin.project, "a");
  assert.equal(origin.branch, "main");
  assert.equal(origin.workset.task, "修复");
  const other = groups[0].rows[1].origin;
  assert.equal(other.project, "b");
  assert.ok(other.branch === null && other.workset === null);
  // 项目标题不在了，远程机器的会话行上自己标机器名；本机的不标。
  assert.equal(groups[0].rows[0].machine_name, null);
  assert.equal(groups[0].rows[1].machine_name, "box");
  // 分组不改会话属于哪个项目。
  assert.equal(groups[0].rows[0].group.workset, "修复");
});

test("按项目分组保持项目的顺序", () => {
  const ui = new_sidebar_ui();
  assert.deepEqual(bucket_groups(undefined, two_projects(), ui, new Date()), []);
  assert.deepEqual(keys(laid_out(two_projects(), [], ui).visible_order()), ["r1", "r2", "r3", "r4"]);

  // 按状态时上一段 / 下一段按组走，折起来的组跳过。
  const by_status = new_sidebar_ui();
  by_status.grouping = "status";
  by_status.collapsed_buckets.add(bucket_flip({ machine: undefined, bucket: "Idle" }));
  assert.deepEqual(keys(laid_out(two_projects(), [], by_status).visible_order()), ["r2", "r4"]);
});

test("按机器分开：一台一片", () => {
  const machines = ["box", null];
  const sidebar = laid_out(two_projects(), machines, new_sidebar_ui());
  assert.deepEqual(sidebar.areas[0].projects, [1]);
  assert.deepEqual(sidebar.areas[1].projects, [0]);
  assert.equal(sidebar.areas[0].status, "needs_you");
  assert.equal(sidebar.areas[0].sessions, 2);
  // 片按机器表的顺序，不按项目的顺序。
  assert.deepEqual(keys(sidebar.visible_order()), ["r3", "r4", "r1", "r2"]);
  const flips = sidebar.flip_order();
  assert.equal(flips[0], machine_flip("box"));
  assert.ok(flips.includes(machine_flip(null)));

  const ui = new_sidebar_ui();
  ui.grouping = "status";
  ui.collapsed_machines.add("box");
  // 折起来的组按机器各折各的：折本机的「需要你」不折 box 的。
  const local_need = bucket_flip({ machine: null, bucket: "NeedsYou" });
  ui.collapsed_buckets.add(local_need);
  const split = laid_out(two_projects(), machines, ui);
  assert.ok(split.areas[0].collapsed);
  const on_box = split.areas[0].buckets[0];
  assert.equal(on_box.key.machine, "box");
  assert.ok(!on_box.collapsed);
  assert.equal(on_box.rows[0].machine_name, null);
  assert.deepEqual(keys(split.visible_order()), ["r1"]);
  assert.deepEqual(
    split.collect_notices().map((notice) => notice.key),
    [machine_flip("box"), local_need],
  );
});

test("要你看的指着会话行或折起来的组", () => {
  const projects = two_projects();
  projects[0].items[0].row.unread = true;
  const sidebar = laid_out(projects, [], new_sidebar_ui());
  const notices = (s) => s.collect_notices().map((notice) => [notice.key, notice.tally]);
  const opens = (s) => s.collect_notices().map((notice) => notice.session.id);
  const key = (id) => session_flip({ machine: null, id });
  const need = { need: 1, unread: 0 };
  const unread = { need: 0, unread: 1 };
  assert.deepEqual(notices(sidebar), [
    [key("r1"), unread],
    [key("r2"), need],
    [key("r4"), need],
  ]);

  sidebar.projects[0].collapsed = true;
  sidebar.notices = sidebar.collect_notices();
  const a = project_flip(null, "/a");
  assert.deepEqual(notices(sidebar), [
    [a, { need: 1, unread: 1 }],
    [key("r4"), need],
  ]);
  assert.deepEqual(opens(sidebar), ["r1", "r4"]);
  assert.deepEqual(
    sidebar.notices_besides({ machine: null, id: "r4" }).map((notice) => notice.key),
    [a],
  );
});

test("搜项目不分大小写、不管两头空白", () => {
  assert.ok(project_matches("Smelt-Plugin-Demo", "  plugin "));
  assert.ok(project_matches("onoo", ""));
  assert.ok(project_matches("onoo", "   "));
  assert.ok(!project_matches("onoo", "smelt"));
});

test("搜索只留搜得到的项目", () => {
  const now = new Date();
  const projects = two_projects();
  assert.deepEqual(area(undefined, projects, new_sidebar_ui(), "B", now).projects, [1]);
  assert.deepEqual(area(undefined, projects, new_sidebar_ui(), "zzz", now).projects, []);
  const ui = new_sidebar_ui();
  ui.grouping = "status";
  const found = area(undefined, projects, ui, "b", now);
  assert.deepEqual(
    found.buckets.flatMap((group) => ids(group.rows)),
    ["r4", "r3"],
  );
});

test("空项目只在要求时藏，置顶的、选中的照常显示", () => {
  assert.ok(project_visible(true, true, false, false));
  assert.ok(project_visible(true, false, true, false));
  assert.ok(project_visible(true, false, false, true));
  assert.ok(!project_visible(true, false, false, false));
  assert.ok(project_visible(false, false, false, false));
});

test("智能体对话标引擎，悬停写智能体名", () => {
  const pi = { id: "pi", label: "Pi", icon: "smelt-icons/agent-pi.svg" };
  const with_definition = (definition) => ({ acp: { agent: "pi", sid: "s", agent_definition_id: definition } });
  const agents = new Map([
    ["dev", "开发助理"],
    ["blank", " "],
  ]);
  const icon = (definition) => row_icon(with_definition(definition), { agent: pi }, agents);
  const badge = (label) => ({ kind: "provider", path: pi.icon, label });
  assert.deepEqual(icon("dev"), badge("开发助理 · Pi"));
  assert.deepEqual(icon("blank"), badge("blank · Pi"));
  assert.deepEqual(icon("gone"), badge("gone · Pi"));
  assert.deepEqual(icon(null), badge("Pi"));
  assert.deepEqual(row_icon({ acp: { sid: "s" } }, { agent: null }, agents), { kind: "conversation" });
  assert.deepEqual(row_icon({ acp: null, terminals: [] }, { agent: null }, agents), { kind: "terminal" });
});

test("项目名取最后一段", () => {
  assert.equal(project_name("posix", "/a/smelt", path, t), "smelt");
  assert.equal(project_name("posix", "", path, t), "No Folder");
});

// ---- 从宿主的数据整份算 ----

function host_input(overrides = {}) {
  const directory = {
    projects: ["/repo"],
    repositories: [{ project_root: "/repo" }, { project_root: "/other" }],
    worksets: [
      { id: "w2", project_root: "/repo", task: "b", branch: "wb", position: 1, repositories: [{ name: "api", worktree: "/repo/worktrees/b/api" }] },
      { id: "w1", project_root: "/repo", task: "a", branch: "wa", position: 1, repositories: [] },
    ],
    conversations: [
      { id: "c1", terminals: [{ id: "t1", cwd: "/repo" }], acp: null, last_updated_at: 50 },
      { id: "c2", terminals: [{ id: "t2", cwd: "/repo/worktrees/a" }], acp: null, workset_id: "w1" },
      { id: "c3", terminals: [], acp: { sid: "s3", agent: "pi", agent_definition_id: "dev" } },
      { id: "c4", terminals: [], acp: { sid: "s4", agent: "pi" } },
      { id: "c5", terminals: [{ id: "t5", cwd: "/loose" }], acp: null },
      { id: "c6", terminals: [], acp: { sid: "s6", agent: "pi" } },
    ],
  };
  const fact = (id, placement, status = "idle", name = null) => ({
    id,
    name,
    status,
    agent: null,
    automation: false,
    cwd: null,
    placement,
  });
  const facts = [
    fact("c1", { kind: "project", root: "/repo", workset: null }, "running", "一号"),
    fact("c2", { kind: "project", root: "/repo", workset: "w1" }, "needs_you"),
    fact("c3", { kind: "conversations" }),
    fact("c4", { kind: "conversations" }),
    fact("c5", { kind: "project", root: "/loose", workset: null }),
    fact("c6", { kind: "hidden" }),
  ];
  const states = [
    { id: "t1", phase_since: 10, turn_started_at: 40, cwd: "/repo/", branch: "main" },
    { id: "s3", phase_since: 5, turn_started_at: 0 },
    { id: "s4", phase_since: 30, turn_started_at: 0 },
  ];
  return {
    machines: [{ machine: null, name: "This Mac", unavailable: null }, { machine: "box", name: "box", unavailable: null }],
    connections: { remotes: [{ machine: "box", os: "linux", state: { state: "connecting" }, link: null }] },
    local_os: "darwin",
    data: (machine) => (machine === null ? { style: "posix", directory, states, facts } : null),
    agent_names: new Map(),
    unread: new Unread(),
    workset_checks: new Map(),
    ui: new_sidebar_ui(),
    project_filter: "",
    now: new Date(),
    t,
    path,
    ...overrides,
  };
}

test("整份算：项目、工作树、对话一段、机器", () => {
  const sidebar = build_sidebar(host_input());
  assert.deepEqual(
    sidebar.projects.map((p) => p.root),
    ["/repo", "/loose"],
  );
  const repo = sidebar.projects[0];
  assert.equal(repo.name, "repo");
  assert.equal(repo.branch, "main");
  assert.equal(repo.repositories, 1);
  assert.equal(repo.sessions, 2);
  assert.equal(repo.status, "needs_you");
  assert.ok(repo.opened && !sidebar.projects[1].opened);
  // 工作树按位置、再按 id；没置顶的排在普通会话后面。
  assert.deepEqual(
    repo.items.map((item) => (item.kind === "session" ? item.row.key.id : item.workset.id)),
    ["c1", "w1", "w2"],
  );
  assert.equal(repo.items[1].workset.path, "/repo/worktrees/a");
  assert.equal(repo.items[2].workset.tooltip.includes("api"), true);
  const c1 = repo.items[0].row;
  assert.equal(c1.title, "一号");
  assert.equal(c1.activity, 40);
  assert.equal(c1.turn_started_at, 40);
  assert.equal(c1.session_id, "t1");
  // 「对话」一段按最近动静排，自动化的不在。
  assert.deepEqual(ids(sidebar.conversations.rows), ["c4", "c3"]);
  assert.equal(sidebar.conversations.rows[0].title, "Conversation");
  assert.deepEqual(keys(sidebar.order), ["c4", "c3", "c1", "c2", "c5"]);
  assert.deepEqual(sidebar.conversation_ids(null), ["c1", "c2", "c5"]);
  // 没收到目录的机器也在机器表里；只有一台远程时按机器分开才起作用。
  assert.deepEqual(
    sidebar.machines.map((m) => [m.machine, m.received, m.os_icon, m.link.light]),
    [
      [null, true, "icons/apple.svg", "good"],
      ["box", false, "icons/linux.svg", "degraded"],
    ],
  );
  assert.ok(!sidebar.by_machine);
  assert.deepEqual(sidebar.running(), new Set([session_flip({ machine: null, id: "c1" })]));
  assert.ok(sidebar.has_session({ machine: null, id: "c5" }));
  assert.ok(!sidebar.has_session({ machine: null, id: "c6" }));
  assert.deepEqual(ids(sidebar.group_rows({ kind: "project", machine: null, root: "/repo", workset: "w1" })), ["c2"]);
  assert.deepEqual(
    sidebar.notices.map((notice) => notice.key),
    [session_flip({ machine: null, id: "c2" })],
  );
});

test("整份算：手动顺序、置顶、折叠、搜索、按机器分开", () => {
  const ui = new_sidebar_ui();
  ui.project_order = [{ machine: null, root: "/loose" }];
  set_pinned(ui, { kind: "conversation", machine: null, id: "c3" }, true);
  set_pinned(ui, { kind: "workset", machine: null, id: "w2" }, true);
  ui.conversations_collapsed = true;
  ui.by_machine = true;
  const sidebar = build_sidebar(host_input({ ui }));
  assert.deepEqual(
    sidebar.projects.map((p) => p.root),
    ["/loose", "/repo"],
  );
  assert.deepEqual(
    sidebar.projects[1].items.map((item) => (item.kind === "session" ? item.row.key.id : item.workset.id)),
    ["w2", "c1", "w1"],
  );
  assert.deepEqual(ids(sidebar.conversations.rows), ["c3", "c4"]);
  assert.ok(sidebar.by_machine);
  assert.deepEqual(
    sidebar.areas.map((a) => [a.machine, a.projects]),
    [
      [null, [0, 1]],
      ["box", []],
    ],
  );
  assert.deepEqual(keys(sidebar.order), ["c5", "c1", "c2"]);

  const searched = build_sidebar(host_input({ project_filter: " LOO " }));
  assert.ok(searched.filtering);
  assert.deepEqual(searched.areas[0].projects.map((ix) => searched.projects[ix].root), ["/loose"]);
});
