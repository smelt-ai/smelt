import assert from "node:assert/strict";
import test from "node:test";

import { Unread } from "../attention.js";

const item = { kind: "success", title: "Done", message: "", note: null };
const session = (id, unread) => ({ session_id: id, action: null, unread: unread ? item : null });
const snapshot = (revision, sessions) => ({ kind: "snapshot", payload: { revision, sessions } });
const event = (revision, s) => ({ kind: "event", payload: { revision, session: s, alert: null } });

test("快照整份换掉", () => {
  const unread = new Unread();
  assert.ok(unread.apply(null, event(1, session("old", true))));
  assert.ok(unread.apply(null, snapshot(5, [session("a", true)])));
  assert.ok(unread.has(null, "a"));
  assert.ok(!unread.has(null, "old"), "快照整份换掉");
  assert.ok(!unread.apply(null, snapshot(6, [session("a", true)])), "内容一样不算变");
});

test("比快照旧的变化丢掉", () => {
  const unread = new Unread();
  unread.apply(null, snapshot(5, []));
  assert.ok(!unread.apply(null, event(5, session("a", true))));
  assert.ok(!unread.has(null, "a"));
  assert.ok(unread.apply(null, event(6, session("a", true))));
  assert.ok(unread.has(null, "a"));
});

test("看过了就不算了，本来就不算的再来一次不算变", () => {
  const unread = new Unread();
  unread.apply(null, snapshot(1, [session("a", true)]));
  assert.ok(unread.apply(null, event(2, session("a", false))));
  assert.ok(!unread.has(null, "a"));
  assert.ok(!unread.apply(null, event(3, session("a", false))));
});

test("机器各管各的，移除了就忘掉", () => {
  const unread = new Unread();
  unread.apply(null, snapshot(1, [session("a", true)]));
  unread.apply("m", snapshot(1, [session("b", true)]));
  assert.ok(!unread.has(null, "b"));
  unread.forget("m");
  assert.ok(!unread.has("m", "b"));
  assert.ok(unread.has(null, "a"));
});

test("读不懂、积压、不推了都不算变", () => {
  const unread = new Unread();
  const error = console.error;
  console.error = () => {};
  try {
    assert.ok(!unread.apply(null, { kind: "snapshot", payload: { nope: 1 } }));
  } finally {
    console.error = error;
  }
  assert.ok(!unread.apply(null, { kind: "lagged" }));
  assert.ok(!unread.apply(null, { kind: "unavailable", reason: "旧" }));
});
