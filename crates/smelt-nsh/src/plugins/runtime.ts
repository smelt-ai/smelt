// 在同一个引擎里装、换、卸插件：每个插件一段脚本，在自己的作用域里执行。
//
// 作用域管三样：
// - 响应式的东西（effect、宿主模块的订阅）挂在它自己的 root 下，卸载时随 root 一起撤；
// - 定时器和动画帧 NSH 不归 root，用同名参数盖住全局的，按插件记着；
// - `nsh` 里的 `host` 换成这个插件自己的：宿主由此知道是谁调的。
//
// 脚本只来自宿主（`src/plugins.rs` 打的包）。执行它要 `eval`，所以装载外壳时不开 harden。
import * as core from "nsh";
import { natives } from "./natives.ts";
import { cellOf } from "./slots.tsx";

const { host, mount } = core;

type Stop = () => void;

const running = new Map<string, Stop>();

function report(id: string, state: "running" | "ready" | "failed", message?: string) {
  host.post("smelt.plugins.status", { plugin: id, state, message: message ?? null });
}

function scope(id: string) {
  let closed = false;
  const stops = new Set<Stop>();
  const timeouts = new Map<number, Stop>();
  const intervals = new Map<number, Stop>();
  const frames = new Map<number, Stop>();

  function clear(table: Map<number, Stop>, handle: number) {
    table.get(handle)?.();
  }

  function timed(
    table: Map<number, Stop>,
    start: (run: (...args: any[]) => void) => number,
    stop: (handle: number) => void,
    callback: (...args: any[]) => void,
    once: boolean,
  ) {
    if (closed) return 0;
    const handle = start((...args) => {
      if (once) table.delete(handle);
      callback(...args);
    });
    table.set(handle, () => {
      table.delete(handle);
      stop(handle);
    });
    return handle;
  }

  const globals = [
    (f: () => void, ms?: number) =>
      timed(timeouts, (run) => setTimeout(run, ms), (h) => clearTimeout(h), f, true),
    (f: () => void, ms?: number) =>
      timed(intervals, (run) => setInterval(run, ms), (h) => clearInterval(h), f, false),
    (h: number) => clear(timeouts, h),
    (h: number) => clear(intervals, h),
    (f: (time: number) => void) =>
      timed(frames, (run) => requestAnimationFrame(run), (h) => cancelAnimationFrame(h), f, true),
    (h: number) => clear(frames, h),
  ];

  const ownHost = {
    ...host,
    call: (name: string, data?: unknown) =>
      host.call("smelt.plugins.call", { plugin: id, name, data: data ?? null }),
    post: (name: string, data?: unknown) =>
      host.post("smelt.plugins.call", { plugin: id, name, data: data ?? null }),
    on: (name: string, listener: (data: any) => void) => {
      if (closed) return () => {};
      const off = host.on(name, listener);
      const stop = () => {
        stops.delete(stop);
        off();
      };
      stops.add(stop);
      return stop;
    },
  };
  const ownCore = { ...core, host: ownHost };

  function need(name: string) {
    if (name === "nsh") return ownCore;
    const module = natives[name];
    if (!module) throw new Error(`宿主没有模块 ${name}`);
    return module;
  }

  function close() {
    closed = true;
    for (const table of [timeouts, intervals, frames]) {
      for (const stop of [...table.values()]) stop();
    }
    for (const stop of [...stops]) stop();
  }

  return { globals, need, close };
}

function remove(id: string) {
  const stop = running.get(id);
  running.delete(id);
  cellOf(id).set({});
  stop?.();
}

function install(id: string, code: string) {
  const cell = cellOf(id);
  remove(id);
  // 先记成出错：死循环、内存耗尽是接不住的，那时下面的 catch 不会跑。
  cell.set({ failed: true });
  report(id, "running");
  const own = scope(id);
  running.set(id, own.close);
  try {
    // 成员调用就是间接 eval：在全局作用域里执行。
    const run = globalThis.eval(code);
    let views: Record<string, unknown> = {};
    const dispose = mount(() => {
      run(own.need, (exported: Record<string, unknown>) => (views = exported), ...own.globals);
      return null;
    });
    running.set(id, () => {
      dispose();
      own.close();
    });
    cell.set({ views });
    report(id, "ready");
  } catch (error) {
    own.close();
    report(id, "failed", String(error));
  }
}

host.on("smelt.plugins.install", (data) => {
  const { plugin, code } = data as { plugin: string; code: string };
  install(plugin, code);
});
host.on("smelt.plugins.remove", (data) => {
  remove((data as { plugin: string }).plugin);
});
