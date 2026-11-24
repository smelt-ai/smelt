import { expect, test } from "bun:test";

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
  same_value,
  session_flip,
  splice_group,
  ValuePool,
} from "../sidebar.js";
import { path, t } from "./helpers.mjs";

test("手动排过的在前，其余保持原来的顺序", () => {
  expect(manual_first(["a", "b", "c", "d"], ["c", "x", "a"], (item, m) => item === m)).toStrictEqual([
    "c",
    "a",
    "b",
    "d",
  ]);
});

test("置顶提前是稳定的", () => {
  expect(pinned_first(["a", "b", "c", "d"], (s) => s === "c" || s === "a")).toStrictEqual(["a", "c", "b", "d"]);
});

test("只有共有行的先后变了才算重排", () => {
  const order = ["a", "b", "c"];
  expect(reordered(order, order)).toBeFalsy();
  // 新建、关掉、折起只是多了少了行。
  expect(reordered(order, ["new", "a", "b", "c"])).toBeFalsy();
  expect(reordered(order, ["a", "c"])).toBeFalsy();
  expect(reordered(order, ["b", "a", "c"])).toBeTruthy();
  expect(reordered(order, ["c", "new", "a"])).toBeTruthy();
});

test("挪到前面、后面", () => {
  const items = ["a", "b", "c", "d"];
  expect(move_near(items, "d", "b", true)).toBeTruthy();
  expect(items).toStrictEqual(["a", "d", "b", "c"]);
  expect(move_near(items, "a", "c", false)).toBeTruthy();
  expect(items).toStrictEqual(["d", "b", "c", "a"]);
  expect(move_near(items, "b", "c", true), "已经在那儿了").toBeFalsy();
  expect(move_near(items, "b", "b", true)).toBeFalsy();
  expect(move_near(items, "z", "b", true)).toBeFalsy();
  expect(items).toStrictEqual(["d", "b", "c", "a"]);
});

test("换一组的顺序不动别的组", () => {
  expect(splice_group(["a1", "b1", "a2", "b2", "a3"], ["a3", "a1", "a2"])).toStrictEqual([
    "a3",
    "b1",
    "a1",
    "b2",
    "a2",
  ]);
});

test("没显示的机器上的项目留在顺序里", () => {
  // x 在没连上的机器上：列表里没有它，拖完照样留在原来跟着的那一项后面。
  expect(merge_project_order(["x", "a", "b"], ["c", "a", "b"])).toStrictEqual(["x", "c", "a", "b"]);
  expect(merge_project_order(["a", "x", "b"], ["b", "a", "c"])).toStrictEqual(["b", "a", "x", "c"]);
  // 从没拖过：就是列表里的新顺序。
  expect(merge_project_order([], ["c", "a", "b"])).toStrictEqual(["c", "a", "b"]);
});

test("改了置顶落在交界上", () => {
  const order = ["p1", "p2", "a", "b", "c"];
  // 置顶 c：排在 p1、p2 之后。
  expect(move_to_pin_boundary(order, "c", (id) => ["p1", "p2", "c"].includes(id))).toStrictEqual([
    "p1",
    "p2",
    "c",
    "a",
    "b",
  ]);
  // 取消置顶 p1：排在其余的最前。
  expect(move_to_pin_boundary(order, "p1", (id) => id === "p2")).toStrictEqual(["p2", "p1", "a", "b", "c"]);
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
  expect(groups.map((g) => g.key.bucket)).toStrictEqual(["NeedsYou", "Idle"]);
  expect(ids(groups[0].rows)).toStrictEqual(["r2", "r4"]);
  // 组里置顶的在前。
  expect(ids(groups[1].rows)).toStrictEqual(["r3", "r1"]);
  expect(groups[0].status).toBe("needs_you");
  expect(groups[0].collapsed).toBeFalsy();
  expect(groups[1].collapsed).toBeTruthy();

  const origin = groups[0].rows[0].origin;
  expect(origin.project).toBe("a");
  expect(origin.branch).toBe("main");
  expect(origin.workset.task).toBe("修复");
  const other = groups[0].rows[1].origin;
  expect(other.project).toBe("b");
  expect(other.branch === null && other.workset === null).toBeTruthy();
  // 项目标题不在了，远程机器的会话行上自己标机器名；本机的不标。
  expect(groups[0].rows[0].machine_name).toBe(null);
  expect(groups[0].rows[1].machine_name).toBe("box");
  // 分组不改会话属于哪个项目。
  expect(groups[0].rows[0].group.workset).toBe("修复");
});

test("按项目分组保持项目的顺序", () => {
  const ui = new_sidebar_ui();
  expect(bucket_groups(undefined, two_projects(), ui, new Date())).toStrictEqual([]);
  expect(keys(laid_out(two_projects(), [], ui).visible_order())).toStrictEqual(["r1", "r2", "r3", "r4"]);

  // 按状态时上一段 / 下一段按组走，折起来的组跳过。
  const by_status = new_sidebar_ui();
  by_status.grouping = "status";
  by_status.collapsed_buckets.add(bucket_flip({ machine: undefined, bucket: "Idle" }));
  expect(keys(laid_out(two_projects(), [], by_status).visible_order())).toStrictEqual(["r2", "r4"]);
});

test("按机器分开：一台一片", () => {
  const machines = ["box", null];
  const sidebar = laid_out(two_projects(), machines, new_sidebar_ui());
  expect(sidebar.areas[0].projects).toStrictEqual([1]);
  expect(sidebar.areas[1].projects).toStrictEqual([0]);
  expect(sidebar.areas[0].status).toBe("needs_you");
  expect(sidebar.areas[0].sessions).toBe(2);
  // 片按机器表的顺序，不按项目的顺序。
  expect(keys(sidebar.visible_order())).toStrictEqual(["r3", "r4", "r1", "r2"]);
  const flips = sidebar.flip_order();
  expect(flips[0]).toBe(machine_flip("box"));
  expect(flips.includes(machine_flip(null))).toBeTruthy();

  const ui = new_sidebar_ui();
  ui.grouping = "status";
  ui.collapsed_machines.add("box");
  // 折起来的组按机器各折各的：折本机的「需要你」不折 box 的。
  const local_need = bucket_flip({ machine: null, bucket: "NeedsYou" });
  ui.collapsed_buckets.add(local_need);
  const split = laid_out(two_projects(), machines, ui);
  expect(split.areas[0].collapsed).toBeTruthy();
  const on_box = split.areas[0].buckets[0];
  expect(on_box.key.machine).toBe("box");
  expect(on_box.collapsed).toBeFalsy();
  expect(on_box.rows[0].machine_name).toBe(null);
  expect(keys(split.visible_order())).toStrictEqual(["r1"]);
  expect(split.collect_notices().map((notice) => notice.key)).toStrictEqual([machine_flip("box"), local_need]);
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
  expect(notices(sidebar)).toStrictEqual([
    [key("r1"), unread],
    [key("r2"), need],
    [key("r4"), need],
  ]);

  sidebar.projects[0].collapsed = true;
  sidebar.notices = sidebar.collect_notices();
  const a = project_flip(null, "/a");
  expect(notices(sidebar)).toStrictEqual([
    [a, { need: 1, unread: 1 }],
    [key("r4"), need],
  ]);
  expect(opens(sidebar)).toStrictEqual(["r1", "r4"]);
  expect(sidebar.notices_besides({ machine: null, id: "r4" }).map((notice) => notice.key)).toStrictEqual([a]);
});

test("搜项目不分大小写、不管两头空白", () => {
  expect(project_matches("Smelt-Plugin-Demo", "  plugin ")).toBeTruthy();
  expect(project_matches("onoo", "")).toBeTruthy();
  expect(project_matches("onoo", "   ")).toBeTruthy();
  expect(project_matches("onoo", "smelt")).toBeFalsy();
});

test("搜索只留搜得到的项目", () => {
  const now = new Date();
  const projects = two_projects();
  expect(area(undefined, projects, new_sidebar_ui(), "B", now).projects).toStrictEqual([1]);
  expect(area(undefined, projects, new_sidebar_ui(), "zzz", now).projects).toStrictEqual([]);
  const ui = new_sidebar_ui();
  ui.grouping = "status";
  const found = area(undefined, projects, ui, "b", now);
  expect(found.buckets.flatMap((group) => ids(group.rows))).toStrictEqual(["r4", "r3"]);
});

test("空项目只在要求时藏，置顶的、选中的照常显示", () => {
  expect(project_visible(true, true, false, false)).toBeTruthy();
  expect(project_visible(true, false, true, false)).toBeTruthy();
  expect(project_visible(true, false, false, true)).toBeTruthy();
  expect(project_visible(true, false, false, false)).toBeFalsy();
  expect(project_visible(false, false, false, false)).toBeTruthy();
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
  expect(icon("dev")).toStrictEqual(badge("开发助理 · Pi"));
  expect(icon("blank")).toStrictEqual(badge("blank · Pi"));
  expect(icon("gone")).toStrictEqual(badge("gone · Pi"));
  expect(icon(null)).toStrictEqual(badge("Pi"));
  expect(row_icon({ acp: { sid: "s" } }, { agent: null }, agents)).toStrictEqual({ kind: "conversation" });
  expect(row_icon({ acp: null, terminals: [] }, { agent: null }, agents)).toStrictEqual({ kind: "terminal" });
});

test("项目名取最后一段", () => {
  expect(project_name("posix", "/a/smelt", path, t)).toBe("smelt");
  expect(project_name("posix", "", path, t)).toBe("No Folder");
});

// ---- 从宿主的数据整份算 ----

function host_input(overrides = {}) {
  const directory = {
    projects: ["/repo"],
    repositories: [{ project_root: "/repo" }, { project_root: "/other" }],
    worksets: [
      {
        id: "w2",
        project_root: "/repo",
        task: "b",
        branch: "wb",
        position: 1,
        repositories: [{ name: "api", worktree: "/repo/worktrees/b/api" }],
      },
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
    machines: [
      { machine: null, name: "This Mac", unavailable: null },
      { machine: "box", name: "box", unavailable: null },
    ],
    connections: { remotes: [{ machine: "box", os: "linux", state: { state: "connecting" }, link: null }] },
    local_os: "darwin",
    data: (machine) =>
      machine === null ? { style: "posix", directory, states, facts, attention: overrides.attention ?? null } : null,
    agent_names: new Map(),
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
  expect(sidebar.projects.map((p) => p.root)).toStrictEqual(["/repo", "/loose"]);
  const repo = sidebar.projects[0];
  expect(repo.name).toBe("repo");
  expect(repo.branch).toBe("main");
  expect(repo.repositories).toBe(1);
  expect(repo.sessions).toBe(2);
  expect(repo.status).toBe("needs_you");
  expect(repo.opened && !sidebar.projects[1].opened).toBeTruthy();
  // 工作树按位置、再按 id；没置顶的排在普通会话后面。
  expect(repo.items.map((item) => (item.kind === "session" ? item.row.key.id : item.workset.id))).toStrictEqual([
    "c1",
    "w1",
    "w2",
  ]);
  expect(repo.items[1].workset.path).toBe("/repo/worktrees/a");
  expect(repo.items[2].workset.tooltip.includes("api")).toBe(true);
  const c1 = repo.items[0].row;
  expect(c1.title).toBe("一号");
  expect(c1.activity).toBe(40);
  expect(c1.turn_started_at).toBe(40);
  expect(c1.session_id).toBe("t1");
  // 「对话」一段按最近动静排，自动化的不在。
  expect(ids(sidebar.conversations.rows)).toStrictEqual(["c4", "c3"]);
  expect(sidebar.conversations.rows[0].title).toBe("Conversation");
  expect(keys(sidebar.order)).toStrictEqual(["c4", "c3", "c1", "c2", "c5"]);
  expect(sidebar.conversation_ids(null)).toStrictEqual(["c1", "c2", "c5"]);
  // 没收到目录的机器也在机器表里；只有一台远程时按机器分开才起作用。
  expect(sidebar.machines.map((m) => [m.machine, m.received, m.os_icon, m.link.light])).toStrictEqual([
    [null, true, "icons/apple.svg", "good"],
    ["box", false, "icons/linux.svg", "degraded"],
  ]);
  expect(sidebar.by_machine).toBeFalsy();
  expect(sidebar.running()).toStrictEqual(new Set([session_flip({ machine: null, id: "c1" })]));
  expect(sidebar.has_session({ machine: null, id: "c5" })).toBeTruthy();
  expect(sidebar.has_session({ machine: null, id: "c6" })).toBeFalsy();
  expect(ids(sidebar.group_rows({ kind: "project", machine: null, root: "/repo", workset: "w1" }))).toStrictEqual([
    "c2",
  ]);
  expect(sidebar.notices.map((notice) => notice.key)).toStrictEqual([session_flip({ machine: null, id: "c2" })]);
});

test("整份算：手动顺序、置顶、折叠、搜索、按机器分开", () => {
  const ui = new_sidebar_ui();
  ui.project_order = [{ machine: null, root: "/loose" }];
  set_pinned(ui, { kind: "conversation", machine: null, id: "c3" }, true);
  set_pinned(ui, { kind: "workset", machine: null, id: "w2" }, true);
  ui.conversations_collapsed = true;
  ui.by_machine = true;
  const sidebar = build_sidebar(host_input({ ui }));
  expect(sidebar.projects.map((p) => p.root)).toStrictEqual(["/loose", "/repo"]);
  expect(
    sidebar.projects[1].items.map((item) => (item.kind === "session" ? item.row.key.id : item.workset.id)),
  ).toStrictEqual(["w2", "c1", "w1"]);
  expect(ids(sidebar.conversations.rows)).toStrictEqual(["c3", "c4"]);
  expect(sidebar.by_machine).toBeTruthy();
  expect(sidebar.areas.map((a) => [a.machine, a.projects])).toStrictEqual([
    [null, [0, 1]],
    ["box", []],
  ]);
  expect(keys(sidebar.order)).toStrictEqual(["c5", "c1", "c2"]);

  const searched = build_sidebar(host_input({ project_filter: " LOO " }));
  expect(searched.filtering).toBeTruthy();
  expect(searched.areas[0].projects.map((ix) => searched.projects[ix].root)).toStrictEqual(["/loose"]);
});

test("重算侧栏：没变的行、机器沿用上一份的对象，变了的是新的", () => {
  const pool = new ValuePool();
  const first = build_sidebar(host_input({ pool }));
  const attention = [{ session_id: "t5", unread: { kind: "success", title: "", message: "" } }];
  const second = build_sidebar(host_input({ pool, attention }));
  const row = (sidebar, root) => sidebar.projects.find((p) => p.root === root).items[0].row;
  expect(row(second, "/repo")).toBe(row(first, "/repo"));
  expect(second.conversations.rows[0]).toBe(first.conversations.rows[0]);
  expect(second.machines[0]).toBe(first.machines[0]);
  expect(row(second, "/loose")).not.toBe(row(first, "/loose"));
  expect(row(second, "/loose").unread).toBe(true);

  // 按状态分组时行带着来源，是另一样值；同样沿用。
  const ui = new_sidebar_ui();
  ui.grouping = "status";
  const a = build_sidebar(host_input({ pool, ui }));
  const b = build_sidebar(host_input({ pool, ui }));
  const bucket_rows = (sidebar) => sidebar.areas[0].buckets.flatMap((group) => group.rows);
  expect(bucket_rows(a).length).toBeGreaterThan(0);
  bucket_rows(b).forEach((row, ix) => expect(row).toBe(bucket_rows(a)[ix]));
});

test("比值：对象、数组逐项比，键不同就不一样", () => {
  expect(same_value({ a: [1, { b: null }] }, { a: [1, { b: null }] })).toBe(true);
  expect(same_value({ a: 1 }, { a: 1, b: undefined })).toBe(false);
  expect(same_value({ a: 1, b: undefined }, { a: 1, c: undefined })).toBe(false);
  expect(same_value([1], { 0: 1 })).toBe(false);
  expect(same_value(NaN, NaN)).toBe(true);
});
