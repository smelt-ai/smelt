import { expect, test } from "bun:test";

import { Unread } from "../attention.js";

const item = { kind: "success", title: "Done", message: "", note: null };
const session = (id, unread) => ({ session_id: id, action: null, unread: unread ? item : null });
const snapshot = (revision, sessions) => ({ kind: "snapshot", payload: { revision, sessions } });
const event = (revision, s) => ({ kind: "event", payload: { revision, session: s, alert: null } });

test("快照整份换掉", () => {
  const unread = new Unread();
  expect(unread.apply(null, event(1, session("old", true)))).toBeTruthy();
  expect(unread.apply(null, snapshot(5, [session("a", true)]))).toBeTruthy();
  expect(unread.has(null, "a")).toBeTruthy();
  expect(unread.has(null, "old"), "快照整份换掉").toBeFalsy();
  expect(unread.apply(null, snapshot(6, [session("a", true)])), "内容一样不算变").toBeFalsy();
});

test("比快照旧的变化丢掉", () => {
  const unread = new Unread();
  unread.apply(null, snapshot(5, []));
  expect(unread.apply(null, event(5, session("a", true)))).toBeFalsy();
  expect(unread.has(null, "a")).toBeFalsy();
  expect(unread.apply(null, event(6, session("a", true)))).toBeTruthy();
  expect(unread.has(null, "a")).toBeTruthy();
});

test("看过了就不算了，本来就不算的再来一次不算变", () => {
  const unread = new Unread();
  unread.apply(null, snapshot(1, [session("a", true)]));
  expect(unread.apply(null, event(2, session("a", false)))).toBeTruthy();
  expect(unread.has(null, "a")).toBeFalsy();
  expect(unread.apply(null, event(3, session("a", false)))).toBeFalsy();
});

test("机器各管各的，移除了就忘掉", () => {
  const unread = new Unread();
  unread.apply(null, snapshot(1, [session("a", true)]));
  unread.apply("m", snapshot(1, [session("b", true)]));
  expect(unread.has(null, "b")).toBeFalsy();
  unread.forget("m");
  expect(unread.has("m", "b")).toBeFalsy();
  expect(unread.has(null, "a")).toBeTruthy();
});

test("读不懂、积压、不推了都不算变", () => {
  const unread = new Unread();
  const error = console.error;
  console.error = () => {};
  try {
    expect(unread.apply(null, { kind: "snapshot", payload: { nope: 1 } })).toBeFalsy();
  } finally {
    console.error = error;
  }
  expect(unread.apply(null, { kind: "lagged" })).toBeFalsy();
  expect(unread.apply(null, { kind: "unavailable", reason: "旧" })).toBeFalsy();
});
