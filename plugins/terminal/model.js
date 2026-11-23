// 终端设置插件纯逻辑（不 import NSH，供 bun test 单测）。

import { DEFAULT_FONT_PX, MAX_FONT_PX, MIN_FONT_PX } from "./contract.js";

export function clampFontPx(value) {
  const num = Number(value);
  if (!Number.isFinite(num)) return DEFAULT_FONT_PX;
  return Math.min(MAX_FONT_PX, Math.max(MIN_FONT_PX, Math.round(num)));
}

/** 单在途、后者覆盖前者的终端主题发布队列。 */
export function createThemePublisher(send) {
  let sending = false;
  let pending = null;

  async function drain() {
    while (pending !== null) {
      const next = pending;
      pending = null;
      try {
        await send(next);
      } catch {
        // 运行时未就绪或正在重启，重连后会再发。
      }
    }
    sending = false;
  }

  return {
    enqueue(snapshot) {
      pending = snapshot;
      if (!sending) {
        sending = true;
        void drain();
      }
    },
  };
}
