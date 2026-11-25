// 会话头像：外形按会话 id 算（../avatar_shape.js），深色身子、按会话变色的描边、白色圆点眼睛。纯 JS。
//
// 演哪一出、从什么时候起照用会话列表的 avatar_stage.js（act/since），这里只管怎么演。
// 身子是一张静态 SVG，只在第一次显示时算；动的是叠在上面的眼睛和整个身子，全用关键帧交给 NSH
// 原生跑，限 30 帧。干活、喊你按拍子走：每拍按（种子, 第几拍）挑一个动作，演完再挑下一拍；
// 每个头像有按种子定的性格（节奏快慢、幅度、第一拍晚多久起）。站着不动不要帧；App 不在前台
// 或系统要求减弱动态效果时也不动。

import { cleanup, createMotion, Easing, effect, lifecycle, reduceMotion, View } from "nsh";
import { SmeltVector } from "smelt/sdk";

import { bodySvg, CANVAS, EYE_R, hash, shapeOf } from "../avatar_shape.js";
import { ONE_SHOT_MS } from "../avatar_stage.js";
/** 动作的帧率上限：眼睛小、动得慢，30 帧够。 */
const FPS = 30;

/** 描边颜色，按种子挑一个。 */
const TINTS = ["#4FC3F7", "#81C784", "#4DB6AC", "#FFB74D", "#BA68C8", "#F06292", "#9FA8DA", "#E57373"];
const BODY = "#141B24";
const EYE = "#F4F8FF";
const EYE_CALL = "#FFC24A";

type Frames = Record<string, number[]>;
/** 一个动作：身子和眼睛各自的关键帧，时长按一拍的倍数。 */
interface Move {
  body?: Frames;
  eyes?: Frames;
  beats: number;
}

/** 干活时挑的动作，后面的数是权重。 */
const WORK: [Move, number][] = [
  // 左右扫一遍，像在读一行。
  [{ eyes: { translateX: [0, -1.8, -1.8, 1.8, 1.8, 0] }, beats: 2 }, 3],
  // 低头一格一格往右看。
  [{ eyes: { translateY: [0, 1, 1, 1, 1, 0], translateX: [0, -1.5, -0.4, 0.8, 1.8, 0] }, beats: 2 }, 3],
  // 往右上想事情。
  [{ eyes: { translateY: [0, -1.4, -1.4, 0], translateX: [0, 1.2, 1.2, 0] }, beats: 1.5 }, 2],
  // 眨眼。
  [{ eyes: { scaleY: [1, 0.1, 1] }, beats: 0.4 }, 3],
  // 点头。
  [{ body: { translateY: [0, 1, 0, 1, 0] }, beats: 1 }, 2],
  // 敲键盘似的轻颤。
  [{ body: { translateY: [0, 0.6, 0, 0.6, 0, 0.6, 0] }, eyes: { translateY: [0, 0.8, 0.8, 0.8, 0.8, 0.8, 0] }, beats: 1 }, 2],
  // 压扁回弹一下。
  [{ body: { scaleX: [1, 1.05, 0.98, 1], scaleY: [1, 0.94, 1.02, 1] }, beats: 0.8 }, 2],
  // 发会儿呆，不动。
  [{ beats: 1 }, 2],
];

/** 喊你时挑的动作。 */
const CALL: [Move, number][] = [
  [{ body: { translateY: [0, -3, 0, 0], scaleY: [1, 1.05, 0.93, 1] }, eyes: { scale: [1, 1.2, 1, 1] }, beats: 0.6 }, 4],
  [{ body: { rotate: [0, -8, 8, -8, 0] }, beats: 0.8 }, 2],
  [{ eyes: { scale: [1, 1.25, 1.25, 1] }, beats: 0.6 }, 2],
];

/** 欢呼、打招呼各几种演法，按种子挑一种。 */
const CHEER: Move[] = [
  { body: { translateY: [0, -4, 0, -2.5, 0] }, eyes: { scaleY: [1, 0.3, 0.3, 0.3, 1] }, beats: 1 },
  { body: { rotate: [0, 360] }, eyes: { scaleY: [1, 0.3, 0.3, 1] }, beats: 1 },
  { body: { translateY: [0, -3, 0], scale: [1, 1.1, 1] }, eyes: { scale: [1, 1.3, 1] }, beats: 1 },
];
const GREET: Move[] = [
  { body: { rotate: [0, -12, 8, 0] }, eyes: { scaleY: [1, 1, 0.1, 1, 1] }, beats: 1 },
  { body: { translateY: [0, 1.5, 0] }, eyes: { translateX: [0, 1.5, -1.5, 0], scaleY: [1, 0.1, 1, 1] }, beats: 1 },
];

/** 0..1 的随机数，同一个（种子, 序号）总是同一个。 */
function dice(seed: number, n: number): number {
  let x = (seed ^ Math.imul(n + 1, 0x9e3779b1)) >>> 0;
  x = Math.imul(x ^ (x >>> 15), 0x2c1b3c6d) >>> 0;
  x = Math.imul(x ^ (x >>> 12), 0x297a2d39) >>> 0;
  return ((x ^ (x >>> 15)) >>> 0) / 4294967296;
}

function pick(list: [Move, number][], roll: number): Move {
  const total = list.reduce((sum, [, weight]) => sum + weight, 0);
  let at = roll * total;
  for (const [move, weight] of list) {
    if ((at -= weight) < 0) return move;
  }
  return list[list.length - 1][0];
}

/** 位移按头像尺寸和性格里的幅度缩放；缩放、旋转不变。 */
function scaled(frames: Frames | undefined, factor: number): Frames | undefined {
  if (!frames) return undefined;
  const out: Frames = {};
  for (const [channel, values] of Object.entries(frames)) {
    out[channel] = channel.startsWith("translate") ? values.map((v) => v * factor) : values;
  }
  return out;
}

export interface AvatarProps {
  seed: string;
  act: string;
  since: number;
  opacity?: number;
  size: number;
}

export function Avatar(props: AvatarProps) {
  const body = createMotion();
  const eyes = createMotion();
  const seed = hash(props.seed);
  const shape = shapeOf(seed);
  const markup = bodySvg(shape.path, TINTS[seed % TINTS.length], BODY);
  // 性格：一拍多长、动作幅度、第一拍晚多久起。
  const beat = 700 + dice(seed, 0) * 600;
  const reach = 0.75 + dice(seed, 1) * 0.5;
  const offset = dice(seed, 2) * 1200;
  // 换一出就加一，旧的那一串拍子看到对不上就停。
  let run = 0;
  cleanup(() => {
    run = -1;
  });

  const play = (move: Move, delay: number) => {
    const timing = { duration: move.beats * beat, delay, easing: Easing.inOut(Easing.ease), frameRate: FPS };
    const factor = (reach * props.size) / CANVAS;
    const b = scaled(move.body, factor);
    const e = scaled(move.eyes, factor);
    const waits: Promise<{ finished: boolean }>[] = [];
    if (b) waits.push(body.play({ keyframes: b, ...timing }));
    if (e) waits.push(eyes.play({ keyframes: e, ...timing }));
    // 发呆的那拍没有动画，也不要帧：只等一拍再挑下一个。
    if (waits.length === 0) {
      return new Promise<{ finished: boolean }>((done) =>
        setTimeout(() => done({ finished: true }), delay + timing.duration),
      );
    }
    return Promise.all(waits).then((results) => ({ finished: results.every((r) => r.finished) }));
  };

  const loop = (mine: number, list: [Move, number][], n: number, delay: number) => {
    if (mine !== run) return;
    play(pick(list, dice(seed, 100 + n)), delay).then(({ finished }) => {
      if (finished) loop(mine, list, n + 1, 0);
    });
  };

  effect(() => {
    const act = props.act;
    const since = props.since;
    const still = lifecycle() !== "active" || reduceMotion();
    const mine = ++run;
    body.reset();
    eyes.reset();
    if (still) return;
    const length = (ONE_SHOT_MS as Record<string, number>)[act];
    if (length != null && Date.now() - since >= length) return;
    switch (act) {
      case "work":
        loop(mine, WORK, Math.floor(Date.now() / beat), offset);
        break;
      case "call":
        loop(mine, CALL, Math.floor(Date.now() / beat), offset / 3);
        break;
      case "cheer":
        play(CHEER[Math.floor(dice(seed, 3) * CHEER.length)], 0);
        break;
      case "greet":
        play(GREET[Math.floor(dice(seed, 4) * GREET.length)], 0);
        break;
    }
  });

  const unit = () => props.size / CANVAS;
  const [left, right, eyeY] = shape.eyes;
  const eyeColor = () => (props.act === "call" ? EYE_CALL : EYE);
  const eye = () => (
    <View
      style={{ width: EYE_R * 2 * unit(), height: EYE_R * 2 * unit(), borderRadius: EYE_R * unit(), backgroundColor: eyeColor() }}
    />
  );

  return (
    <View style={{ width: props.size, height: props.size, opacity: props.opacity ?? 1, pointerEvents: "none" }}>
      <View style={{ position: "absolute", left: 0, top: 0, width: props.size, height: props.size, ...body.style }}>
        <SmeltVector markup={markup} width={props.size} height={props.size} />
        <View
          style={{
            position: "absolute",
            left: (left - EYE_R) * unit(),
            top: (eyeY - EYE_R) * unit(),
            width: (right - left + EYE_R * 2) * unit(),
            height: EYE_R * 2 * unit(),
            flexDirection: "row",
            justifyContent: "space-between",
            ...eyes.style,
          }}
        >
          {eye()}
          {eye()}
        </View>
      </View>
    </View>
  );
}
