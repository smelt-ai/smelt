import { expect, test } from "bun:test";

import { CANVAS, EYE_RX, EYE_RY, hash, mix, shapeOf } from "../avatar_shape.js";

/** 路径里所有坐标点（每对数是一个点）。 */
function points(path) {
  const nums = path.match(/-?\d+(\.\d+)?/g).map(Number);
  const out = [];
  for (let i = 0; i + 1 < nums.length; i += 2) out.push([nums[i], nums[i + 1]]);
  return out;
}

const seeds = Array.from({ length: 300 }, (_, n) => hash(`conversation-${n}`));

test("同一个会话 id 永远同一个外形", () => {
  expect(shapeOf(hash("conv-a"))).toEqual(shapeOf(hash("conv-a")));
  expect(shapeOf(hash("conv-a")).path).not.toBe(shapeOf(hash("conv-b")).path);
});

test("外形都在画布里，四周留够描边", () => {
  for (const seed of seeds) {
    const { path } = shapeOf(seed);
    expect(path).not.toContain("NaN");
    for (const [x, y] of points(path)) {
      // 控制点可以略出轮廓，描边 4.2 一半在外：留 2 的边就不会被裁。
      expect(x).toBeGreaterThan(2);
      expect(x).toBeLessThan(CANVAS - 2);
      expect(y).toBeGreaterThan(2);
      expect(y).toBeLessThan(CANVAS - 2);
    }
  }
});

test("两只眼睛左右对称、互不重叠、都落在身子里", () => {
  for (const seed of seeds) {
    const { path, eyes } = shapeOf(seed);
    const [left, right, y] = eyes;
    expect(left + right).toBeCloseTo(CANVAS, 6);
    expect(right - left).toBeGreaterThan(EYE_RX * 2);
    // 眼睛那一行，轮廓左右两边离眼睛外沿都还有空。
    const row = points(path).filter(([, py]) => Math.abs(py - y) < 3);
    expect(Math.min(...row.map(([x]) => x))).toBeLessThan(left - EYE_RX - 1);
    expect(Math.max(...row.map(([x]) => x))).toBeGreaterThan(right + EYE_RX + 1);
    const ys = points(path).map(([, py]) => py);
    expect(y - EYE_RY).toBeGreaterThan(Math.min(...ys) + 1);
    expect(y + EYE_RY).toBeLessThan(Math.max(...ys) - 1);
  }
});

test("混色：两头是原色和目标色，中间按比例", () => {
  expect(mix("#4FC3F7", 255, 0)).toBe("#4fc3f7");
  expect(mix("#4FC3F7", 255, 1)).toBe("#ffffff");
  expect(mix("#4FC3F7", 0, 1)).toBe("#000000");
  expect(mix("#808080", 255, 0.5)).toBe("#c0c0c0");
});
