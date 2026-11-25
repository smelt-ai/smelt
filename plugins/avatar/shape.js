// 头像的外形和描边颜色：按种子算出来，同一个种子永远同一个头像。
//
// 超椭圆打底（方圆程度、宽高比），上下收窄，叠几道左右对称的低频起伏，再统一大小、塞进 36 见方的
// 画布，眼睛差不多在身子正中。坐标都在 36×36 里，用的时候按头像尺寸缩放。

/** 画布边长。 */
export const CANVAS = 36;
/** 眼睛半径。 */
export const EYE_R = 2.6;

/** 字符串 → 32 位种子（FNV-1a）。 */
export function hash(text) {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h;
}

/** 种子起一串 0..1 的随机数（mulberry32）。 */
export function rng(seed) {
  let x = seed >>> 0;
  return () => {
    x = (x + 0x6d2b79f5) >>> 0;
    let t = Math.imul(x ^ (x >>> 15), 1 | x);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const lerp = (a, b, t) => a + (b - a) * t;
/** 轮廓取多少个点。 */
const POINTS = 48;
/** 统一到的面积（36 见方里的平方单位）。 */
const AREA = 400;
/** 外接框长边统一到多长。 */
const BOX = 25;
/** 定大小时外接框占几成：只按面积统一的话，扁宽的看着比圆的大。 */
const BOX_WEIGHT = 0.6;
/** 各阶起伏（2、3、4 阶）的最大幅度：奇数阶让上下不对称，给得最少。 */
const WAVES = [0.06, 0.02, 0.035];

/**
 * 种子 → `{ path, eyes: [左眼 x, 右眼 x, y] }`。新加随机量只能接在最后取，前面的不变，
 * 不然升级后人人换脸。
 */
export function shapeOf(seed) {
  const r = rng(seed);
  // 取值范围收在好看的一段：太圆像蛋、太方像屏幕，收窄和起伏大了会出盾牌、钟形。
  const n = lerp(2.6, 4.2, r()); // 2 是椭圆，越大越方
  const a = lerp(10.5, 13, r());
  const b = lerp(9.5, 12.5, r());
  const taper = lerp(-0.16, 0.16, r()); // 正：下宽上窄；负：上宽下窄
  const waves = WAVES.map((w, i) => [i + 2, (r() - 0.5) * 2 * w]);

  const pts = [];
  for (let i = 0; i < POINTS; i++) {
    const th = (i / POINTS) * Math.PI * 2;
    const c = Math.cos(th);
    const s = Math.sin(th);
    let x = a * Math.sign(c) * Math.abs(c) ** (2 / n);
    const y = b * Math.sign(s) * Math.abs(s) ** (2 / n);
    x *= 1 + taper * (y / b);
    // 以竖轴为对称轴的起伏：cos(k(θ-π/2)) 左右镜像不变，外形总是正对着人。
    let f = 1;
    for (const [k, w] of waves) f += w * Math.cos(k * (th - Math.PI / 2));
    pts.push([x * f, y * f]);
  }

  let area = 0;
  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  for (let i = 0; i < POINTS; i++) {
    const [x1, y1] = pts[i];
    const [x2, y2] = pts[(i + 1) % POINTS];
    area += x1 * y2 - x2 * y1;
    minX = Math.min(minX, x1);
    maxX = Math.max(maxX, x1);
    minY = Math.min(minY, y1);
    maxY = Math.max(maxY, y1);
  }
  // 大小按面积和外接框长边混着统一，再保证四边留够描边的位置。
  const width = maxX - minX;
  const height = maxY - minY;
  const byArea = Math.sqrt(AREA / Math.abs(area / 2));
  const byBox = BOX / Math.max(width, height);
  const k = Math.min(byArea * (1 - BOX_WEIGHT) + byBox * BOX_WEIGHT, 27 / width, 26 / height);
  const cy = 19.5 - ((minY + maxY) / 2) * k;
  const P = pts.map(([x, y]) => [CANVAS / 2 + x * k, cy + y * k]);

  // 平滑闭合：Catmull-Rom 转三次贝塞尔。
  const f2 = (v) => v.toFixed(2);
  let path = `M${f2(P[0][0])} ${f2(P[0][1])}`;
  for (let i = 0; i < POINTS; i++) {
    const p0 = P[(i - 1 + POINTS) % POINTS];
    const p1 = P[i];
    const p2 = P[(i + 1) % POINTS];
    const p3 = P[(i + 2) % POINTS];
    path +=
      ` C${f2(p1[0] + (p2[0] - p0[0]) / 6)} ${f2(p1[1] + (p2[1] - p0[1]) / 6)}` +
      ` ${f2(p2[0] - (p3[0] - p1[0]) / 6)} ${f2(p2[1] - (p3[1] - p1[1]) / 6)}` +
      ` ${f2(p2[0])} ${f2(p2[1])}`;
  }
  path += " Z";

  const eyeY = cy + height * k * 0.02;
  const half = Math.min(4.6, width * k * 0.17);
  return { path, eyes: [CANVAS / 2 - half, CANVAS / 2 + half, eyeY] };
}

/** OKLCH → 线性 sRGB 三分量（可能越界）。 */
function oklchToLinear(l, c, hue) {
  const a = c * Math.cos(hue);
  const b = c * Math.sin(hue);
  const l_ = (l + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m_ = (l - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s_ = (l - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
    -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
    -0.0041960863 * l_ - 0.7034186147 * m_ + 1.707614701 * s_,
  ];
}

/** OKLCH（色相用度）→ `#rrggbb`；sRGB 给不出这么艳就降彩度到刚好能显示。 */
function oklchHex(l, c, hueDeg) {
  const hue = (hueDeg * Math.PI) / 180;
  let rgb = oklchToLinear(l, c, hue);
  while (rgb.some((v) => v < 0 || v > 1)) {
    c = Math.max(0, c - 0.005);
    rgb = oklchToLinear(l, c, hue);
  }
  const channel = (v) => {
    const s = v <= 0.0031308 ? 12.92 * v : 1.055 * v ** (1 / 2.4) - 0.055;
    return Math.round(Math.min(1, Math.max(0, s)) * 255)
      .toString(16)
      .padStart(2, "0");
  };
  return `#${rgb.map(channel).join("")}`;
}

/**
 * 种子 → 描边渐变 `{ from, to, angle }`：两端颜色和渐变方向（度，0 是从左到右）。
 * 色相整圈随便取，另一端错开 40°～120°（再远中段会发灰）；两端亮度、彩度各取各的，故意不齐，
 * 有的艳有的淡。用单独一串随机数，外形不受影响；新加随机量同样只能接在最后取。
 */
export function tintOf(seed) {
  const r = rng(seed ^ 0x5bd1e995);
  const hue = r() * 360;
  const spread = lerp(40, 120, r()) * (r() < 0.5 ? -1 : 1);
  const end = () => [lerp(0.64, 0.88, r()), lerp(0.07, 0.19, r())];
  const [l1, c1] = end();
  const [l2, c2] = end();
  return {
    from: oklchHex(l1, c1, hue),
    to: oklchHex(l2, c2, hue + spread),
    angle: Math.round(r() * 360),
  };
}

/** 外形 → 头像身子的 SVG：先描一圈渐变彩边（`tintOf` 的结果），再填深色身子。 */
export function bodySvg(path, tint, fill) {
  // 渐变沿 angle 方向穿过外接框中心，两端落在框边上。
  const rad = (tint.angle * Math.PI) / 180;
  const at = (v) => (0.5 + v).toFixed(3);
  const dx = Math.cos(rad) / 2;
  const dy = Math.sin(rad) / 2;
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${CANVAS} ${CANVAS}">` +
    `<defs><linearGradient id="t" x1="${at(-dx)}" y1="${at(-dy)}" x2="${at(dx)}" y2="${at(dy)}">` +
    `<stop offset="0" stop-color="${tint.from}"/><stop offset="1" stop-color="${tint.to}"/>` +
    `</linearGradient></defs>` +
    `<path d="${path}" fill="none" stroke="url(#t)" stroke-width="4.2" stroke-linejoin="round"/>` +
    `<path d="${path}" fill="${fill}" stroke="${fill}" stroke-width="1.2" stroke-linejoin="round"/>` +
    `</svg>`
  );
}
