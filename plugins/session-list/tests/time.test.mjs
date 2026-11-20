import { expect, test } from "bun:test";

import { relative_time, span } from "../time.js";
import { t } from "./helpers.mjs";

test("相对时间从现在往回数", () => {
  expect(relative_time(0, 100, t)).toBe("Unknown");
  expect(relative_time(100, 100, t)).toBe("now");
  expect(relative_time(100, 130, t)).toBe("now");
  expect(relative_time(100, 100 + 3 * 60, t)).toBe("3m ago");
  expect(relative_time(100, 100 + 5 * 3600, t)).toBe("5h ago");
  expect(relative_time(100, 100 + 2 * 86400, t)).toBe("2d ago");
  expect(relative_time(100, 100 + 40 * 86400, t)).toBe("1mo ago");
  expect(relative_time(100, 100 + 400 * 86400, t)).toBe("1y ago");
  // 远程机器的时钟可能比本机快，不写成负数。
  expect(relative_time(100 + 3600, 100, t)).toBe("now");
});

test("时长只留最大的一级", () => {
  expect(span(0, t)).toBe("now");
  expect(span(59, t)).toBe("now");
  expect(span(60, t)).toBe("1m");
  expect(span(6 * 60 + 59, t)).toBe("6m");
  expect(span(3600 - 1, t)).toBe("59m");
  expect(span(3600, t)).toBe("1h");
  expect(span(3600 + 50 * 60, t)).toBe("1h");
  expect(span(3 * 86400, t)).toBe("3d");
});
