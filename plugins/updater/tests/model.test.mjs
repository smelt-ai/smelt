import { describe, expect, test } from "bun:test";
import {
  channelLabel,
  checkButtonLabel,
  formatVersionHeader,
  progressModel,
  statusPresentation,
} from "../model.js";

function fakeT(key, params = {}) {
  let out = key;
  for (const [k, v] of Object.entries(params)) {
    out += `|${k}=${v}`;
  }
  return out;
}

describe("updater model", () => {
  test("formatVersionHeader includes channel only when supported", () => {
    expect(formatVersionHeader("0.2.0", true, "prod", fakeT)).toBe("v0.2.0 · channel.prod");
    expect(formatVersionHeader("0.2.0", true, "dev", fakeT)).toBe("v0.2.0 · channel.dev");
    expect(formatVersionHeader("0.2.0", false, "prod", fakeT)).toBe("v0.2.0");
    expect(channelLabel("dev", fakeT)).toBe("channel.dev");
  });

  test("statusPresentation formats download progress and location error override", () => {
    const dl = statusPresentation(
      {
        channel: "prod",
        status: {
          kind: "downloading",
          version: "0.3.0",
          received: 5 * 1024 * 1024,
          total: 10 * 1024 * 1024,
        },
      },
      fakeT,
    );
    expect(dl.tone).toBe("run");
    expect(dl.detail).toContain("percent=50");
    expect(dl.detail).toContain("received=5.0");
    expect(dl.detail).toContain("total=10.0");

    const loc = statusPresentation(
      {
        channel: "prod",
        status: { kind: "failed", failure: "location" },
        move_app_error: "Permission denied",
      },
      fakeT,
    );
    expect(loc.tone).toBe("bad");
    expect(loc.title).toBe("failure.location");
    expect(loc.detail).toBe("Permission denied");
  });

  test("checkButtonLabel and progressModel map active states", () => {
    expect(checkButtonLabel({ kind: "ready" }, fakeT)).toBe("check.again");
    expect(checkButtonLabel({ kind: "failed", failure: "recovery" }, fakeT)).toBe(
      "check.retry_recovery",
    );
    expect(progressModel({ kind: "downloading", received: 25, total: 100 })).toEqual({
      kind: "determinate",
      percent: 25,
    });
    expect(progressModel({ kind: "installing" })).toEqual({ kind: "indeterminate" });
    expect(progressModel({ kind: "idle" })).toBeNull();
  });
});
