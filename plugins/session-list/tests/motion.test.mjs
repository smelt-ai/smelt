import { expect, test } from "bun:test";

import { GLOW, ListMotion, WASH_END } from "../motion.js";

/** 只有 flip_order、running 的假侧栏。 */
const sidebar = (order, running = []) => ({ flip_order: () => order, running: () => new Set(running) });

test("先后变了才算重排，松手的落点只给紧接着的那次", () => {
  const motion = new ListMotion(sidebar(["a", "b", "c"]));
  motion.drop_at("c", 120);
  // 只多了一行：不算重排，落点也作废。
  motion.track(sidebar(["a", "b", "c", "d"]), 0);
  expect(motion.generation).toBe(0);
  expect(motion.landing_top("c")).toBe(undefined);
  motion.drop_at("c", 80);
  motion.track(sidebar(["c", "a", "b", "d"]), 0);
  expect(motion.generation).toBe(1);
  expect(motion.landing_top("c")).toBe(80);
  expect(motion.landing_top("a")).toBe(undefined);
});

test("刚开始跑的亮一下，起来时已经在跑的不亮，停了、播完了就不再交给图标", () => {
  const motion = new ListMotion(sidebar(["a", "b"], ["a"]));
  motion.track(sidebar(["a", "b"], ["a", "b"]), 1000);
  expect(motion.glow_serial("a")).toBe(null);
  const serial = motion.glow_serial("b");
  expect(serial).not.toBe(null);
  motion.track(sidebar(["a", "b"], ["a", "b"]), 1000 + GLOW - 1);
  expect(motion.glow_serial("b")).toBe(serial);
  motion.track(sidebar(["a", "b"], ["a", "b"]), 1000 + GLOW);
  expect(motion.glow_serial("b")).toBe(null);
  // 停了再跑，又亮一次，serial 换新的。
  motion.track(sidebar(["a", "b"], ["a"]), 5000);
  motion.track(sidebar(["a", "b"], ["a", "b"]), 6000);
  expect(motion.glow_serial("b") > serial).toBeTruthy();
});

test("落进来的撑开带底色，找出来的只铺底色，播完就清掉", () => {
  const motion = new ListMotion(sidebar(["a", "b"]));
  motion.arrive("a", 0);
  motion.locate("b", 0);
  expect(motion.arrival_serial("a")).not.toBe(null);
  expect(motion.wash_serial("a")).toBe(motion.arrival_serial("a"));
  expect(motion.arrival_serial("b")).toBe(null);
  expect(motion.wash_serial("b")).not.toBe(null);
  motion.expire(WASH_END);
  expect(motion.arrival_serial("a")).toBe(null);
  expect(motion.wash_serial("b")).toBe(null);
});
