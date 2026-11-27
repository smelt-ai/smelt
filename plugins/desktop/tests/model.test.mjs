import { describe, expect, test } from "bun:test";
import {
  buildMachineMenuItems,
  describeClosed,
  planMachineProbes,
  RETRY_DELAYS_MS,
  retryDelayMs,
  statusPresentation,
} from "../model.js";

function fakeT(key, params = {}) {
  let out = key;
  for (const [k, v] of Object.entries(params)) {
    out += `|${k}=${v}`;
  }
  return out;
}

describe("desktop model", () => {
  test("retryDelayMs backs off exponentially and stops after 6 attempts", () => {
    expect(RETRY_DELAYS_MS).toEqual([1000, 2000, 4000, 8000, 16000, 30000]);
    expect(retryDelayMs(0)).toBe(1000);
    expect(retryDelayMs(5)).toBe(30000);
    expect(retryDelayMs(6)).toBeNull();
    expect(retryDelayMs(-1)).toBeNull();
  });

  test("statusPresentation formats connected, error, reconnecting, and closed states", () => {
    expect(statusPresentation({ kind: "connecting" }, true, null, fakeT)).toEqual({
      label: "status.connecting",
      tone: "muted",
    });
    expect(statusPresentation({ kind: "connected" }, true, null, fakeT)).toEqual({
      label: "status.controlling",
      tone: "strong",
    });
    expect(statusPresentation({ kind: "reconnecting" }, true, null, fakeT)).toEqual({
      label: "status.reconnecting",
      tone: "muted",
    });
    expect(statusPresentation({ kind: "connected" }, false, null, fakeT)).toEqual({
      label: "status.viewing",
      tone: "muted",
    });
    expect(statusPresentation({ kind: "connected" }, true, "timeout", fakeT)).toEqual({
      label: "status.type_failed|error=timeout",
      tone: "bad",
    });
    expect(
      statusPresentation({ kind: "closed", closed: { kind: "by_user" } }, true, null, fakeT),
    ).toEqual({
      label: "closed.by_user",
      tone: "muted",
    });
    expect(
      describeClosed({ kind: "proxy", error: "port busy" }, fakeT),
    ).toBe("closed.proxy|error=port busy");
  });

  test("planMachineProbes tracks only online remote machines and builds sorted menu items", () => {
    const probes = new Map();
    const first = planMachineProbes(
      [
        { machine: null, online: true },
        { machine: "b-box", online: true },
        { machine: "a-box", online: true },
        { machine: "off-box", online: false },
      ],
      probes,
    );
    expect(first).toEqual({ toAsk: ["a-box", "b-box"], removed: false });

    probes.set("a-box", { kind: "known", desktop: true });
    probes.set("b-box", { kind: "known", desktop: false });
    expect(buildMachineMenuItems(probes, { en: "Remote desktop" }, "smelt.desktop/open")).toEqual([
      {
        label: { en: "Remote desktop" },
        icon: "icons/monitor.svg",
        machine: "a-box",
        command: "smelt.desktop/open",
        args: null,
      },
    ]);

    const second = planMachineProbes([{ machine: null, online: true }], probes);
    expect(second).toEqual({ toAsk: [], removed: true });
    expect(probes.size).toBe(0);
  });
});
