import assert from "node:assert/strict";
import test from "node:test";

import { relative_time, span } from "../time.js";
import { t } from "./helpers.mjs";

test("相对时间从现在往回数", () => {
  assert.equal(relative_time(0, 100, t), "Unknown");
  assert.equal(relative_time(100, 100, t), "now");
  assert.equal(relative_time(100, 130, t), "now");
  assert.equal(relative_time(100, 100 + 3 * 60, t), "3m ago");
  assert.equal(relative_time(100, 100 + 5 * 3600, t), "5h ago");
  assert.equal(relative_time(100, 100 + 2 * 86400, t), "2d ago");
  assert.equal(relative_time(100, 100 + 40 * 86400, t), "1mo ago");
  assert.equal(relative_time(100, 100 + 400 * 86400, t), "1y ago");
  // 远程机器的时钟可能比本机快，不写成负数。
  assert.equal(relative_time(100 + 3600, 100, t), "now");
});

test("时长只留最大的一级", () => {
  assert.equal(span(0, t), "now");
  assert.equal(span(59, t), "now");
  assert.equal(span(60, t), "1m");
  assert.equal(span(6 * 60 + 59, t), "6m");
  assert.equal(span(3600 - 1, t), "59m");
  assert.equal(span(3600, t), "1h");
  assert.equal(span(3600 + 50 * 60, t), "1h");
  assert.equal(span(3 * 86400, t), "3d");
});
