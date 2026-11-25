import { expect, test } from "bun:test";

import { RECHECK_AFTER, describe, due_checks, merge_summary, workset_key } from "../worksets.js";
import { t } from "./helpers.mjs";

const repo = (name, dirty, merge) => ({ name, dirty, merge });
const checked = (repositories) => ({
  running: false,
  last: { at: Date.now(), result: { repositories, refresh_failures: [] } },
});

test("合并情况：挡着的排在确认不了的前面", () => {
  const summary = (repos) => merge_summary(checked(repos));
  expect(summary([repo("api", false, "merged"), repo("web", false, null)])).toBe("merged");
  expect(summary([repo("api", false, "unconfirmed")])).toBe("unconfirmed");
  expect(summary([repo("api", false, "unconfirmed"), repo("web", true, "merged")])).toBe("pending");
  expect(summary([repo("api", false, "unmerged")])).toBe("pending");
  expect(merge_summary(undefined)).toBe("unknown");
  expect(merge_summary({ running: false, last: { at: 0, error: "连不上" } })).toBe("unknown");
});

test("说明每个仓库一行，刷新失败另起一行", () => {
  const lines = describe(
    { repositories: [repo("api", true, null), repo("web", false, "merged")], refresh_failures: ["web"] },
    t,
  );
  expect(lines.length).toBe(3);
  expect(lines[0].includes("api")).toBeTruthy();

  // Rust `WorkspaceWorksetInspectResult.refresh_failures` 带 `skip_serializing_if = "Vec::is_empty"`，
  // 干净检查结果不会序列化该字段，必须正常返回而不抛 TypeError。
  const clean = describe({ repositories: [repo("api", false, "merged")] }, t);
  expect(clean.length).toBe(1);
});

test("只问没问过的，要旧的时才连过期的也问", () => {
  const now = 1_000_000_000;
  const key = (id) => ({ machine: null, id });
  const checks = new Map([
    [workset_key(null, "old"), { running: false, last: { at: now - RECHECK_AFTER, error: "x" } }],
    [workset_key(null, "fresh"), { running: false, last: { at: now, error: "x" } }],
    [workset_key(null, "running"), { running: true, last: null }],
  ]);
  const present = [key("new"), key("old"), key("fresh"), key("running")];
  expect(due_checks(present, checks, false, now)).toStrictEqual([key("new")]);
  expect(due_checks(present, checks, true, now)).toStrictEqual([key("new"), key("old")]);
});
