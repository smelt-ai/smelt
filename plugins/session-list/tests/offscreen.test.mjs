import assert from "node:assert/strict";
import test from "node:test";

import { hint, tally_label } from "../offscreen.js";
import { t } from "./helpers.mjs";

const notice = (key, need, unread) => ({ key, tally: { need, unread }, session: { machine: null, id: key } });

test("在外面的按边数，各边记最近的那一行；看得见的、没画过的不算", () => {
  const notices = [
    notice("far-above", 1, 0),
    notice("above", 0, 1),
    notice("inside", 1, 0),
    notice("below", 0, 2),
    notice("far-below", 1, 0),
    notice("not-drawn", 1, 0),
  ];
  // 列表按 watch 的顺序报；这里故意打乱，结果只看行在列表里的先后。
  const result = hint(notices, ["above", "far-above"], ["far-below", "below"]);
  assert.deepEqual(result.above.tally, { need: 1, unread: 1 });
  assert.equal(result.above.nearest.id, "above");
  assert.deepEqual(result.below.tally, { need: 1, unread: 2 });
  assert.equal(result.below.nearest.id, "below");
});

test("都看得见就没有提示", () => {
  const result = hint([notice("a", 1, 0)], [], []);
  assert.deepEqual(result, {
    above: { tally: { need: 0, unread: 0 }, nearest: null },
    below: { tally: { need: 0, unread: 0 }, nearest: null },
  });
});

test("标签只写有的那几样", () => {
  assert.equal(tally_label({ need: 2, unread: 0 }, t), "2 need you");
  assert.equal(tally_label({ need: 0, unread: 1 }, t), "1 unread");
  assert.equal(tally_label({ need: 1, unread: 3 }, t), "1 need you · 3 unread");
});
