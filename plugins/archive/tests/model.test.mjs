import { expect, test } from "bun:test";

import { archived_when, busy_key, machine_group, title_of } from "../model.js";

// posix 的路径规则，够测试用（宿主那份见 smelt-model 的 runtime_path）。
const clean = (_style, p) => p.replace(/\/+$/, "") || "/";
const parent = (_style, p) => {
  const c = clean(null, p);
  if (c === "/" || c === "") return null;
  const i = c.lastIndexOf("/");
  return i <= 0 ? "/" : c.slice(0, i);
};
const starts_with = (p, prefix) => p === prefix || p.startsWith(`${prefix}/`);
const path = {
  clean,
  parent,
  project_root_of: (_style, roots, cwd) =>
    cwd === "" ? null : (roots.filter((root) => starts_with(cwd, root)).sort((a, b) => b.length - a.length)[0] ?? null),
  labels: (_style, roots) => roots.map((root) => root.slice(root.lastIndexOf("/") + 1)),
};
const t = (key, params) => (params ? `${key}:${JSON.stringify(params)}` : key);
const NOW = 1_700_000_000;

const acp = (id, cwd, archived_at) => ({
  id,
  custom_title: null,
  last_updated_at: 0,
  archived_at,
  workset_id: null,
  closed_workset_root: null,
  terminals: [],
  acp: { cwd, sid: `sid-${id}`, session_title: null },
});

const directory = (fields) => ({ projects: [], worksets: [], conversations: [], ...fields });
const group = (dir) => machine_group(path, t, { machine: null, name: "本机", unavailable: null }, "posix", dir, NOW);

test("没有归档就没有这一组", () => {
  expect(group(directory({ projects: ["/p/a"], conversations: [acp("1", "/p/a", null)] }))).toBe(null);
});

test("按项目分组，最近归档的在前", () => {
  const result = group(
    directory({
      projects: ["/p/a", "/p/b"],
      conversations: [acp("1", "/p/a/sub", 10), acp("2", "/p/b", 30), acp("3", "/p/a", 20), acp("4", "/p/a", null)],
    }),
  );
  expect(result.projects.map((project) => [project.root, project.rows.map((row) => row.id)])).toStrictEqual([
    ["/p/b", ["2"]],
    ["/p/a", ["3", "1"]],
  ]);
  expect(result.projects[1].rows[0].session_id).toBe("sid-3");
});

test("项目已关的按工作目录自成一组", () => {
  const result = group(directory({ conversations: [acp("1", "/gone/x", 1)] }));
  expect(result.projects[0].root).toBe("/gone/x");
  expect(result.projects[0].label).toBe("x");
});

test("原工作树关了：按路径推回项目，给同一项目下还在的工作树", () => {
  const conversation = { ...acp("1", "/p/a/worktrees/old", 1), closed_workset_root: "/p/a/worktrees/old" };
  const result = group(
    directory({
      projects: ["/p/a"],
      worksets: [
        { id: "w1", project_root: "/p/a", task: "new", branch: "feat/new" },
        { id: "w2", project_root: "/p/b", task: "other", branch: "other" },
      ],
      conversations: [conversation],
    }),
  );
  expect(result.projects[0].root).toBe("/p/a");
  expect(result.projects[0].rows[0].closed_workset).toStrictEqual({
    old_root: "/p/a/worktrees/old",
    candidates: [{ id: "w1", label: "new · feat/new" }],
  });
});

test("终端组用第一格", () => {
  const result = group(
    directory({
      conversations: [
        {
          ...acp("t", null, 1),
          acp: null,
          terminals: [{ id: "pane-1", cwd: "/p", custom_title: null, launch_label: "zsh", launch_cmd: null }],
        },
      ],
    }),
  );
  const row = result.projects[0].rows[0];
  expect(row.is_conversation).toBe(false);
  expect(row.title).toBe("zsh");
  expect(row.session_id).toBe("pane-1");
});

test("名字：取第一个有的，空白的按种类叫", () => {
  expect(title_of(t, { ...acp("1", "/p", 1), custom_title: "改过" })).toBe("改过");
  expect(title_of(t, { ...acp("1", "/p", 1), custom_title: "  " })).toBe("row.untitled");
  expect(title_of(t, { ...acp("1", "/p", 1), acp: null })).toBe("row.terminal");
});

test("归档时间：今年的写月日时分，往年的写年月日", () => {
  const at = new Date(2023, 10, 14, 10, 32).getTime() / 1000;
  expect(archived_when(t, at, at + 60)).toBe(`row.archived_at:${JSON.stringify({ time: "11-14 10:32" })}`);
  expect(archived_when(t, at, new Date(2024, 0, 2).getTime() / 1000)).toBe(
    `row.archived_at:${JSON.stringify({ time: "2023-11-14" })}`,
  );
  expect(archived_when(t, null, NOW)).toBe("");
});

test("忙着的身份分机器", () => {
  expect(busy_key(null, "1")).not.toBe(busy_key("m", "1"));
});
