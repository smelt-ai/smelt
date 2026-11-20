import assert from "node:assert/strict";
import test from "node:test";

import { GLOW, ListMotion, WASH_END } from "../motion.js";

/** 只有 flip_order、running 的假侧栏。 */
const sidebar = (order, running = []) => ({ flip_order: () => order, running: () => new Set(running) });

test("先后变了才算重排，松手的落点只给紧接着的那次", () => {
  const motion = new ListMotion(sidebar(["a", "b", "c"]));
  motion.drop_at("c", 120);
  // 只多了一行：不算重排，落点也作废。
  motion.track(sidebar(["a", "b", "c", "d"]), 0);
  assert.equal(motion.generation, 0);
  assert.equal(motion.landing_top("c"), undefined);
  motion.drop_at("c", 80);
  motion.track(sidebar(["c", "a", "b", "d"]), 0);
  assert.equal(motion.generation, 1);
  assert.equal(motion.landing_top("c"), 80);
  assert.equal(motion.landing_top("a"), undefined);
});

test("刚开始跑的亮一下，起来时已经在跑的不亮，停了、播完了就不再交给图标", () => {
  const motion = new ListMotion(sidebar(["a", "b"], ["a"]));
  motion.track(sidebar(["a", "b"], ["a", "b"]), 1000);
  assert.equal(motion.glow_serial("a"), null);
  const serial = motion.glow_serial("b");
  assert.notEqual(serial, null);
  motion.track(sidebar(["a", "b"], ["a", "b"]), 1000 + GLOW - 1);
  assert.equal(motion.glow_serial("b"), serial);
  motion.track(sidebar(["a", "b"], ["a", "b"]), 1000 + GLOW);
  assert.equal(motion.glow_serial("b"), null);
  // 停了再跑，又亮一次，serial 换新的。
  motion.track(sidebar(["a", "b"], ["a"]), 5000);
  motion.track(sidebar(["a", "b"], ["a", "b"]), 6000);
  assert.ok(motion.glow_serial("b") > serial);
});

test("落进来的撑开带底色，找出来的只铺底色，播完就清掉", () => {
  const motion = new ListMotion(sidebar(["a", "b"]));
  motion.arrive("a", 0);
  motion.locate("b", 0);
  assert.notEqual(motion.arrival_serial("a"), null);
  assert.equal(motion.wash_serial("a"), motion.arrival_serial("a"));
  assert.equal(motion.arrival_serial("b"), null);
  assert.notEqual(motion.wash_serial("b"), null);
  motion.expire(WASH_END);
  assert.equal(motion.arrival_serial("a"), null);
  assert.equal(motion.wash_serial("b"), null);
});
