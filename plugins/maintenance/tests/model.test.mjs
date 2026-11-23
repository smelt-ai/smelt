import { describe, expect, test } from "bun:test";
import {
  cleanupSummary,
  cliActionLabel,
  cliInstallErrorReason,
  cliStatusDescription,
  formatBytes,
  totalResidueBytes,
} from "../model.js";

function fakeT(key, params = {}) {
  let out = key;
  for (const [k, v] of Object.entries(params)) {
    out += `|${k}=${v}`;
  }
  return out;
}

describe("maintenance model", () => {
  test("formatBytes picks the nearest unit", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(2048)).toBe("2 KB");
    expect(formatBytes(5 * 1024 * 1024 + 512 * 1024)).toBe("5.5 MB");
    expect(formatBytes(3 * 1024 * 1024 * 1024)).toBe("3.0 GB");
  });

  test("cleanupSummary covers running, failed, empty, and non-empty scans", () => {
    expect(cleanupSummary({ kind: "running" }, fakeT)).toBe("cleanup.scanning");
    expect(cleanupSummary({ kind: "failed", error: "EACCES" }, fakeT)).toBe(
      "cleanup.scan_failed|error=EACCES",
    );
    expect(cleanupSummary({ kind: "done", items: [] }, fakeT)).toBe("cleanup.empty");
    const items = [
      { path: "/a", label: "models/", bytes: 2048 },
      { path: "/b", label: "old.db", bytes: 1024 },
    ];
    expect(totalResidueBytes(items)).toBe(3072);
    expect(cleanupSummary({ kind: "done", items }, fakeT)).toBe(
      "cleanup.summary|count=2|size=3 KB",
    );
  });

  test("cli status description, action label, and error mapping", () => {
    expect(cliStatusDescription("installed", "/usr/local/bin/smelt", fakeT)).toBe(
      "cli.status.installed|link=/usr/local/bin/smelt",
    );
    expect(cliStatusDescription("missing", "/usr/local/bin/smelt", fakeT)).toBe(
      "cli.status.missing",
    );
    expect(cliStatusDescription("stale", "/usr/local/bin/smelt", fakeT)).toBe(
      "cli.status.stale|link=/usr/local/bin/smelt",
    );
    expect(cliStatusDescription("taken", "/usr/local/bin/smelt", fakeT)).toBe(
      "cli.status.taken|link=/usr/local/bin/smelt",
    );
    expect(cliStatusDescription("unknown", null, fakeT)).toBe("cli.status.unknown");

    expect(cliActionLabel("missing", true, fakeT)).toBe("cli.install.running");
    expect(cliActionLabel("installed", false, fakeT)).toBe("cli.install.again");
    expect(cliActionLabel("missing", false, fakeT)).toBe("cli.install.action");

    expect(cliInstallErrorReason({ code: "cancelled" }, fakeT)).toBe("cli.install.cancelled");
    expect(cliInstallErrorReason({ code: "taken", link: "/usr/local/bin/smelt" }, fakeT)).toBe(
      "cli.status.taken|link=/usr/local/bin/smelt",
    );
    expect(cliInstallErrorReason({ code: "osascript_failed", error: "boom" }, fakeT)).toBe(
      "cli.install.osascript_failed|error=boom",
    );
  });
});
