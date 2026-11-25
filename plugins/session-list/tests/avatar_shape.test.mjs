import { expect, test } from "bun:test";

import { CANVAS, EYE_R, hash, mouthSvg, shapeOf } from "../avatar_shape.js";

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

test("嘴在两眼中间正下方，四种嘴都不出身子", () => {
  for (const seed of seeds) {
    const { path, eyes, mouth } = shapeOf(seed);
    const [left, right, eyeY] = eyes;
    const [x, y] = mouth;
    expect(x).toBeCloseTo((left + right) / 2, 6);
    expect(y).toBeGreaterThan(eyeY + EYE_R);
    // 最低的是张嘴笑，到 y + 2.2；离轮廓底至少留 1.5。
    const bottom = Math.max(...points(path).map(([, py]) => py));
    expect(y + 2.2).toBeLessThan(bottom - 1.5);
  }
  for (const kind of ["smile", "flat", "o", "grin"]) {
    const svg = mouthSvg([18, 22], kind, "#fff");
    expect(svg).toContain('viewBox="0 0 36 36"');
    expect(svg).not.toContain("NaN");
  }
  // 认不得的一律当微笑。
  expect(mouthSvg([18, 22], "?", "#fff")).toBe(mouthSvg([18, 22], "smile", "#fff"));
});
