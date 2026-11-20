// 列表动画的账：比前后两次侧栏，记下哪一项要滑、哪一行刚落进来、要找出来、刚开始跑。只在事件里
// 记（侧栏重算、新建、定位、松手），render 只读。动画本身由 smelt-ui 的零件按时间播：
// SmeltFlip（generation 变了就滑）、SmeltArrival / SmeltWash / SmeltGlowIcon（serial 头一回见或
// 变了就播一次）。组的开合由 SmeltFold 自己播，不用记。
//
// 零件按 serial 判断播没播过，状态挂在画出来的元素上：一行折起来再展开，元素是新建的，同一个
// serial 会再播一次。所以播完的就不再交给零件（expire），和原来只在播的那一会儿画是一样的。

import { reordered } from "./sidebar.js";

/** 落进来、找出来那一行的底色淡入、停住、退干净一共多久（毫秒），同 SmeltWash。 */
export const WASH_END = 1100;
/** 刚开始跑时图标亮一下多久（毫秒），同 SmeltGlowIcon。 */
export const GLOW = 2000;

export class ListMotion {
  /** sidebar 是眼下的侧栏：打开 app、展开一组时露出来的在跑会话不算刚开始跑。 */
  constructor(sidebar) {
    /** 上一次各行的先后（Sidebar.flip_order），比出重排。 */
    this.order = sidebar.flip_order();
    /** 重排了几次：SmeltFlip 看到它变了就从上一帧的位置滑到新位置。 */
    this.generation = 0;
    /** 刚松手的那一项 { key, top }（top 是松手时牌子上沿的窗口高度），交给紧接着的那次重排。 */
    this.dropped = null;
    /** 这一次重排里从手放下的地方落下去的那一项 { key, top }；别的行从原处滑过去。 */
    this.landing = null;
    /** 刚新建、落进列表的会话 { key, serial, at }（key 是滑动身份）。 */
    this.arrival = null;
    /** 要找出来的会话 { key, serial, at }：只铺底色，不撑开。 */
    this.located = null;
    /** 上一次在跑的会话（滑动身份）。 */
    this.running = sidebar.running();
    /** 滑动身份 → { serial, at }：刚开始跑、图标要亮一下的。 */
    this.glows = new Map();
    this.serial = 0;
  }

  /** 松手了：key 那一项从 top 落到新位置。 */
  drop_at(key, top) {
    this.dropped = { key, top };
  }

  /** 侧栏重算过了（now 是毫秒）：先后变了记一次重排，刚开始跑的会话亮一下。 */
  track(sidebar, now) {
    const order = sidebar.flip_order();
    const dropped = this.dropped;
    this.dropped = null;
    if (reordered(this.order, order)) {
      this.generation += 1;
      this.landing = dropped;
    }
    this.order = order;
    const running = sidebar.running();
    for (const key of running) {
      if (!this.running.has(key)) this.glows.set(key, { serial: this.next(), at: now });
    }
    for (const key of [...this.glows.keys()]) if (!running.has(key)) this.glows.delete(key);
    this.running = running;
    this.expire(now);
  }

  arrive(key, now) {
    this.arrival = { key, serial: this.next(), at: now };
  }

  locate(key, now) {
    this.located = { key, serial: this.next(), at: now };
  }

  /** 播完的不再交给零件。 */
  expire(now) {
    if (this.arrival && now - this.arrival.at >= WASH_END) this.arrival = null;
    if (this.located && now - this.located.at >= WASH_END) this.located = null;
    for (const [key, glow] of [...this.glows]) if (now - glow.at >= GLOW) this.glows.delete(key);
  }

  /** 这一项是不是这次重排里从手放下的地方落下去的：是就给落点高度。 */
  landing_top(key) {
    return this.landing?.key === key ? this.landing.top : undefined;
  }

  /** 这一行落进来的 serial；不是刚落进来的是 null。 */
  arrival_serial(key) {
    return this.arrival?.key === key ? this.arrival.serial : null;
  }

  /** 这一行铺底色的 serial（找出来的优先，再是落进来的）；都不是是 null。 */
  wash_serial(key) {
    if (this.located?.key === key) return this.located.serial;
    return this.arrival_serial(key);
  }

  /** 这一行图标亮一下的 serial；不是刚开始跑的是 null。 */
  glow_serial(key) {
    return this.glows.get(key)?.serial ?? null;
  }

  next() {
    this.serial += 1;
    return this.serial;
  }
}
