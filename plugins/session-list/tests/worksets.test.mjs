import assert from "node:assert/strict";
import test from "node:test";

import { RECHECK_AFTER, describe, due_checks, merge_summary, workset_key } from "../worksets.js";
import { t } from "./helpers.mjs";

const repo = (name, dirty, merge) => ({ name, dirty, merge });
const checked = (repositories) => ({
  running: false,
  last: { at: Date.now(), result: { repositories, refresh_failures: [] } },
});

test("合并情况：挡着的排在确认不了的前面", () => {
  const summary = (repos) => merge_summary(checked(repos));
  assert.equal(summary([repo("api", false, "merged"), repo("web", false, null)]), "merged");
  assert.equal(summary([repo("api", false, "unconfirmed")]), "unconfirmed");
  assert.equal(summary([repo("api", false, "unconfirmed"), repo("web", true, "merged")]), "pending");
  assert.equal(summary([repo("api", false, "unmerged")]), "pending");
  assert.equal(merge_summary(undefined), "unknown");
  assert.equal(merge_summary({ running: false, last: { at: 0, error: "连不上" } }), "unknown");
});

test("说明每个仓库一行，刷新失败另起一行", () => {
  const lines = describe(
    { repositories: [repo("api", true, null), repo("web", false, "merged")], refresh_failures: ["web"] },
    t,
  );
  assert.equal(lines.length, 3);
  assert.ok(lines[0].includes("api"));
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
  assert.deepEqual(due_checks(present, checks, false, now), [key("new")]);
  assert.deepEqual(due_checks(present, checks, true, now), [key("new"), key("old")]);
});
