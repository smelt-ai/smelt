import { expect, test } from "bun:test";

import { ONE_SHOT_MS, conversation_key, empty_stage, greet, track } from "../avatar_stage.js";

const key = (id) => conversation_key(null, id);
const rows = (entries) => new Map(entries.map(([id, status]) => [key(id), status]));
const act = (stage, id) => stage.actors.get(key(id))?.act;

test("第一次看到的只按状态摆，不起反应", () => {
  const stage = empty_stage();
  track(
    stage,
    rows([
      ["a", "idle"],
      ["b", "running"],
      ["c", "needs_you"],
    ]),
    null,
    0,
  );
  expect([act(stage, "a"), act(stage, "b"), act(stage, "c")]).toStrictEqual(["rest", "work", "call"]);
});

test("状态变了换一出：跑完才欢呼", () => {
  const stage = empty_stage();
  track(stage, rows([["a", "idle"]]), null, 0);
  for (const [status, expected] of [
    ["running", "work"],
    ["needs_you", "call"],
    ["running", "work"],
    ["idle", "cheer"],
    ["needs_you", "call"],
    ["idle", "rest"],
  ]) {
    track(stage, rows([["a", status]]), null, 0);
    expect(act(stage, "a")).toBe(expected);
  }
});

test("状态没变接着演，不从头来", () => {
  const stage = empty_stage();
  track(stage, rows([["a", "running"]]), null, 0);
  track(stage, rows([["a", "running"]]), null, 5_000);
  expect(stage.actors.get(key("a")).since).toBe(0);
});

test("选中闲着的打一次招呼；在忙的不打；招呼没打完再选回来不从头打", () => {
  const stage = empty_stage();
  const both = rows([
    ["a", "idle"],
    ["b", "running"],
  ]);
  track(stage, both, null, 0);
  track(stage, both, key("a"), 0);
  expect(act(stage, "a")).toBe("greet");
  track(stage, both, key("b"), 0);
  expect(act(stage, "b")).toBe("work");
  track(stage, both, key("a"), 300);
  expect(stage.actors.get(key("a")).since).toBe(0);
});

test("欢呼完了才算闲着", () => {
  const stage = empty_stage();
  track(stage, rows([["a", "running"]]), null, 0);
  track(stage, rows([["a", "idle"]]), null, 0);
  greet(stage, key("a"), ONE_SHOT_MS.cheer - 1);
  expect(act(stage, "a")).toBe("cheer");
  greet(stage, key("a"), ONE_SHOT_MS.cheer);
  expect(act(stage, "a")).toBe("greet");
});

test("没了的会话去掉", () => {
  const stage = empty_stage();
  track(
    stage,
    rows([
      ["a", "idle"],
      ["b", "idle"],
    ]),
    null,
    0,
  );
  track(stage, rows([["a", "idle"]]), null, 0);
  expect(stage.actors.has(key("b"))).toBe(false);
});

test("不同机器上同 id 的会话是两段", () => {
  expect(conversation_key(null, "a")).not.toBe(conversation_key("runtime-1", "a"));
});
