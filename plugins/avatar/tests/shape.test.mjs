import { expect, test } from "bun:test";

import { BADGE, bodySvg, CANVAS, EYE_R, hash, shapeOf, tintOf } from "../shape.js";

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
    expect(right - left).toBeGreaterThan(EYE_R * 2);
    // 眼睛那一行，轮廓左右两边离眼睛外沿都还有空。
    const row = points(path).filter(([, py]) => Math.abs(py - y) < 3);
    expect(Math.min(...row.map(([x]) => x))).toBeLessThan(left - EYE_R - 1);
    expect(Math.max(...row.map(([x]) => x))).toBeGreaterThan(right + EYE_R + 1);
    const ys = points(path).map(([, py]) => py);
    expect(y - EYE_R).toBeGreaterThan(Math.min(...ys) + 1);
    expect(y + EYE_R).toBeLessThan(Math.max(...ys) - 1);
  }
});

test("一排看下来大小差不多：外接框长边都在 23 到 26.5 之间", () => {
  for (const seed of seeds) {
    const pts = points(shapeOf(seed).path);
    const xs = pts.map(([x]) => x);
    const ys = pts.map(([, y]) => y);
    const long = Math.max(Math.max(...xs) - Math.min(...xs), Math.max(...ys) - Math.min(...ys));
    expect(long).toBeGreaterThan(23);
    expect(long).toBeLessThan(26.5);
  }
});

test("描边渐变：同一个种子同一组颜色，两端都是合法颜色、色相错开", () => {
  expect(tintOf(hash("conv-a"))).toEqual(tintOf(hash("conv-a")));
  expect(tintOf(hash("conv-a"))).not.toEqual(tintOf(hash("conv-b")));
  for (const seed of seeds) {
    const { from, to, angle } = tintOf(seed);
    expect(from).toMatch(/^#[0-9a-f]{6}$/);
    expect(to).toMatch(/^#[0-9a-f]{6}$/);
    expect(from).not.toBe(to);
    expect(angle).toBeGreaterThanOrEqual(0);
    expect(angle).toBeLessThanOrEqual(360);
  }
});

test("描边颜色不扎堆：300 个会话的起点色几乎各不相同", () => {
  const starts = new Set(seeds.map((seed) => tintOf(seed).from));
  expect(starts.size).toBeGreaterThan(290);
});

test("身子 SVG 带渐变描边，坐标没有 NaN", () => {
  const seed = hash("conv-a");
  const svg = bodySvg(shapeOf(seed).path, tintOf(seed), "#141B24");
  expect(svg).toContain('stroke="url(#t)"');
  expect(svg).toContain(tintOf(seed).from);
  expect(svg).not.toContain("NaN");
});

test("角标在右下、整个图标在画布里、碰不到眼睛", () => {
  for (const seed of seeds) {
    const { badge, eyes } = shapeOf(seed);
    const [x, y] = badge;
    const [, right, eyeY] = eyes;
    expect(x + BADGE / 2).toBeLessThanOrEqual(CANVAS);
    expect(y + BADGE / 2).toBeLessThanOrEqual(CANVAS);
    expect(x).toBeGreaterThan(CANVAS / 2);
    expect(y).toBeGreaterThan(eyeY);
    expect(Math.hypot(x - right, y - eyeY)).toBeGreaterThanOrEqual(BADGE / 2 + EYE_R + 1 - 1e-9);
  }
});

test("给了角标位置，身子 SVG 在那里挖一个圆；不给就不挖", () => {
  const seed = hash("conv-a");
  const { path, badge } = shapeOf(seed);
  const cut = bodySvg(path, tintOf(seed), "#141B24", badge);
  // NSH 的矢量管线不画 mask，只能用 clip-path。
  expect(cut).toContain('clip-path="url(#c)"');
  expect(cut).toContain('clip-rule="evenodd"');
  expect(cut).toContain(`M${(badge[0] - 6.5).toFixed(2)} ${badge[1].toFixed(2)}a6.5 6.5`);
  expect(cut).not.toContain("<mask");
  expect(bodySvg(path, tintOf(seed), "#141B24")).not.toContain("clip");
});
