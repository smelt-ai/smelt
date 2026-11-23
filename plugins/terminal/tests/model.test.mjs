import { describe, expect, test } from "bun:test";
import { clampFontPx, createThemePublisher } from "../model.js";
import { DEFAULT_FONT_PX, MAX_FONT_PX, MIN_FONT_PX } from "../contract.js";

describe("terminal model", () => {
  test("clampFontPx clamps into [9, 22] and falls back to default", () => {
    expect(clampFontPx(undefined)).toBe(DEFAULT_FONT_PX);
    expect(clampFontPx(NaN)).toBe(DEFAULT_FONT_PX);
    expect(clampFontPx(4)).toBe(MIN_FONT_PX);
    expect(clampFontPx(16)).toBe(16);
    expect(clampFontPx(99)).toBe(MAX_FONT_PX);
  });

  test("createThemePublisher coalesces bursts keeping only latest snapshot", async () => {
    const sent = [];
    let releaseFirst;
    const firstGate = new Promise((resolve) => {
      releaseFirst = resolve;
    });
    const publisher = createThemePublisher(async (snap) => {
      sent.push(snap);
      if (sent.length === 1) {
        await firstGate;
      }
    });

    publisher.enqueue({ id: 1 });
    publisher.enqueue({ id: 2 });
    publisher.enqueue({ id: 3 });
    expect(sent).toEqual([{ id: 1 }]);

    releaseFirst();
    await new Promise((r) => setTimeout(r, 10));
    expect(sent).toEqual([{ id: 1 }, { id: 3 }]);
  });
});
