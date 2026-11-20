import { expect, test } from "bun:test";

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
  expect(result.above.tally).toStrictEqual({ need: 1, unread: 1 });
  expect(result.above.nearest.id).toBe("above");
  expect(result.below.tally).toStrictEqual({ need: 1, unread: 2 });
  expect(result.below.nearest.id).toBe("below");
});

test("都看得见就没有提示", () => {
  const result = hint([notice("a", 1, 0)], [], []);
  expect(result).toStrictEqual({
    above: { tally: { need: 0, unread: 0 }, nearest: null },
    below: { tally: { need: 0, unread: 0 }, nearest: null },
  });
});

test("标签只写有的那几样", () => {
  expect(tally_label({ need: 2, unread: 0 }, t)).toBe("2 need you");
  expect(tally_label({ need: 0, unread: 1 }, t)).toBe("1 unread");
  expect(tally_label({ need: 1, unread: 3 }, t)).toBe("1 need you · 3 unread");
});
