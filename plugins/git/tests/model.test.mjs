import { describe, expect, test } from "bun:test";
import {
  buildGitTree,
  clearConfirmedGitIndexOps,
  commitButtonBusyKey,
  commitGraphRowSvg,
  diffFeedbackText,
  diffParseFailure,
  diffScrollTarget,
  fullFilePath,
  GIT_AUTOFETCH_INTERVAL_MS,
  GIT_DISCOVERY_MIN_INTERVAL_MS,
  gitAutofetchWait,
  gitDiscoveryWait,
  gitEmptyListKind,
  gitIndexOpInFlight,
  gitStatusResponseAction,
  gitStatusRetryDelay,
  graphWidth,
  hunkPatch,
  layoutGraph,
  mapJoinedLineToFile,
  needsFetch,
  parseUnifiedPatch,
  pullRequestLink,
  pullRequestUrl,
  releaseGitWrite,
  repoNeedsAttention,
  reserveGitWrite,
  resolveGitWriteTarget,
  shouldCloseAggregateDiff,
  splitStagedAndChanged,
} from "../model.js";

function files(paths) {
  return paths.map((p) => [" M", p]);
}

describe("pullRequestUrl & pullRequestLink", () => {
  test("GitHub remotes in every form open the compare page", () => {
    const expected = "https://github.com/smelt-ai/smelt/compare/main...feat/x?expand=1";
    for (const remote of [
      "git@github.com:smelt-ai/smelt.git",
      "git@github.com:smelt-ai/smelt",
      "ssh://git@github.com:22/smelt-ai/smelt.git",
      "https://github.com/smelt-ai/smelt.git",
      "https://token@github.com/smelt-ai/smelt/",
    ]) {
      expect(pullRequestUrl(remote, "main", "feat/x")).toBe(expected);
    }
  });

  test("GitLab keeps nested groups and the https port", () => {
    expect(
      pullRequestUrl("https://gitlab.example.com:8443/group/sub/app.git", "develop", "fix/y"),
    ).toBe(
      "https://gitlab.example.com:8443/group/sub/app/-/merge_requests/new?merge_request%5Bsource_branch%5D=fix/y&merge_request%5Btarget_branch%5D=develop",
    );
    expect(pullRequestUrl("ssh://git@gitlab.example.com:2222/group/app.git", "main", "a")).toStartWith(
      "https://gitlab.example.com/group/app/-/",
    );
  });

  test("branch names are percent-encoded except slashes", () => {
    expect(pullRequestUrl("git@github.com:o/r.git", "main", "feat/搜索 框#1")).toBe(
      "https://github.com/o/r/compare/main...feat/%E6%90%9C%E7%B4%A2%20%E6%A1%86%231?expand=1",
    );
  });

  test("unknown hosts and local paths give no link", () => {
    for (const remote of [
      "git@git.internal.corp:team/app.git",
      "/Users/me/remotes/app.git",
      "file:///srv/app.git",
      "https://github.com/only-owner",
    ]) {
      expect(pullRequestUrl(remote, "main", "x")).toBeNull();
    }
  });

  test("pullRequestLink explains why it cannot open a page", () => {
    const tr = (k, p) => (p ? `${k}:${JSON.stringify(p)}` : k);
    const pushed = {
      branch: "feat/x",
      remote_url: "git@github.com:o/r.git",
      default_branch: "main",
      upstream_branch: "feat/x",
    };
    expect(pullRequestLink({ info: pushed }, tr)).toEqual({
      ok: true,
      url: "https://github.com/o/r/compare/main...feat/x?expand=1",
    });
    expect(pullRequestLink({ info: null }, tr)).toEqual({
      ok: false,
      error: "pull_request.not_repo",
    });
    expect(pullRequestLink({ info: { ...pushed, upstream_branch: null } }, tr).error).toBe(
      "pull_request.not_pushed",
    );
    expect(pullRequestLink({ info: { ...pushed, default_branch: null } }, tr).error).toBe(
      "pull_request.no_default_branch",
    );
    expect(pullRequestLink({ info: { ...pushed, upstream_branch: "main" } }, tr).error).toContain(
      "pull_request.on_default_branch",
    );
    expect(
      pullRequestLink({ info: { ...pushed, remote_url: "git@git.corp:o/r.git" } }, tr).error,
    ).toContain("pull_request.unknown_host");
  });
});

describe("git tree and staging helpers", () => {
  test("compacts single-child directory chains and stops at forks", () => {
    const rows = buildGitTree(files(["crates/smelt/src/main.rs"]));
    expect(rows).toEqual([
      { depth: 0, name: "crates/smelt/src", path: "crates/smelt/src", status: null },
      { depth: 1, name: "main.rs", path: "crates/smelt/src/main.rs", status: " M" },
    ]);

    const forked = buildGitTree(files(["a/b/x.rs", "a/c/y.rs"]));
    expect(forked.map((r) => r.name)).toEqual(["a", "b", "x.rs", "c", "y.rs"]);
    expect(forked[2].depth).toBe(2);
  });

  test("lists dirs before files, hides collapsed children, and ignores trailing slash", () => {
    expect(buildGitTree(files(["zz.txt", "aa/b.rs"])).map((r) => r.name)).toEqual([
      "aa",
      "b.rs",
      "zz.txt",
    ]);
    expect(
      buildGitTree(files(["a/b/x.rs", "a/c/y.rs", "top.rs"]), new Set(["a"])).map((r) => r.name),
    ).toEqual(["a", "top.rs"]);
    const trailing = buildGitTree(files(["new-dir/"]));
    expect(trailing).toHaveLength(1);
    expect(trailing[0].name).toBe("new-dir");
    expect(trailing[0].status).toBeNull();
  });

  test("partially staged file shows in both staged and changed groups", () => {
    const { staged, changed } = splitStagedAndChanged([
      ["MM", "both.rs"],
      ["M ", "staged.rs"],
      [" M", "changed.rs"],
      ["??", "new.rs"],
    ]);
    expect(staged.map(([, p]) => p)).toEqual(["both.rs", "staged.rs"]);
    expect(changed.map(([, p]) => p)).toEqual(["both.rs", "changed.rs", "new.rs"]);
  });

  test("fullFilePath returns null for aggregate diffs", () => {
    expect(fullFilePath("/repo", "", true)).toBeNull();
    expect(fullFilePath("/repo", "src/main.rs", false)).toBe("/repo/src/main.rs");
  });
});

describe("status state machine, write locks, and multi-repo target resolution", () => {
  test("status response action & retry delays", () => {
    expect(gitStatusResponseAction(8, 7, true)).toBe("accept_stale_and_refresh");
    expect(gitStatusResponseAction(7, 7, true)).toBe("accept_current");
    expect(gitStatusResponseAction(7, 7, false)).toBe("preserve_and_retry");
    expect(gitStatusRetryDelay(1)).toBe(150);
    expect(gitStatusRetryDelay(3)).toBe(600);
    expect(gitStatusRetryDelay(4)).toBeNull();
  });

  test("empty list & aggregate diff closing distinguish clean vs failed vs loading", () => {
    const clean = { ok: true, files: [] };
    const failed = { ok: false, files: [] };
    expect(gitEmptyListKind(clean, true, 0)).toBe("clean");
    expect(shouldCloseAggregateDiff(clean, true)).toBe(true);
    expect(gitEmptyListKind(failed, true, 1)).toBe("retrying");
    expect(shouldCloseAggregateDiff(failed, true)).toBe(false);
    expect(gitEmptyListKind(null, true, 0)).toBe("loading");
    expect(shouldCloseAggregateDiff(null, true)).toBe(false);
  });

  test("pending index ops and repository write lock", () => {
    expect(gitIndexOpInFlight({ completed: false })).toBe(true);
    expect(gitIndexOpInFlight({ completed: true })).toBe(false);
    expect(gitIndexOpInFlight(null)).toBe(false);

    const pending = new Map([
      ["/repo\0running.rs", { completed: false }],
      ["/repo\0done.rs", { completed: true }],
      ["/other\0done.rs", { completed: true }],
    ]);
    clearConfirmedGitIndexOps(pending, "/repo");
    expect(pending.has("/repo\0running.rs")).toBe(true);
    expect(pending.has("/repo\0done.rs")).toBe(false);
    expect(pending.has("/other\0done.rs")).toBe(true);

    const writes = new Map();
    expect(reserveGitWrite(writes, "/a", "commit")).toBe(true);
    expect(reserveGitWrite(writes, "/a", "fetch")).toBe(false);
    expect(reserveGitWrite(writes, "/b", "fetch")).toBe(true);
    expect(releaseGitWrite(writes, "/a", "fetch")).toBe(false);
    expect(releaseGitWrite(writes, "/a", "commit")).toBe(true);
    expect(writes.has("/a")).toBe(false);
    expect(writes.get("/b")).toBe("fetch");
  });

  test("multi-repo write target resolution and attention check", () => {
    const set = {
      repos: [
        { root: "/p", rel_path: "", kind: "root" },
        { root: "/p/sub", rel_path: "sub", kind: "submodule" },
      ],
    };
    expect(resolveGitWriteTarget("/p", "/p/sub", set, null)).toBe("/p/sub");
    expect(resolveGitWriteTarget("/p", null, set, "/p/sub")).toBe("/p/sub");
    expect(resolveGitWriteTarget("/p", "/p", set, "/p/sub")).toBe("/p");
    expect(resolveGitWriteTarget("/p", "/p/gone", { repos: [set.repos[0]] }, null)).toBe("/p");
    expect(resolveGitWriteTarget("/p", "/p/sub", null, null)).toBe("/p/sub");

    const nested = {
      repos: [
        { root: "/task/api", rel_path: "api", kind: "nested" },
        { root: "/task/web", rel_path: "web", kind: "nested" },
      ],
    };
    expect(resolveGitWriteTarget("/task", null, nested, null)).toBe("/task/api");
    expect(resolveGitWriteTarget("/task", "/task/web", nested, null)).toBe("/task/web");

    expect(repoNeedsAttention(false, 2, 0, 0)).toBe(true);
    expect(repoNeedsAttention(false, 0, 3, 0)).toBe(true);
    expect(repoNeedsAttention(false, 0, 0, 1)).toBe(true);
    expect(repoNeedsAttention(false, 0, 0, 0)).toBe(false);
    expect(commitButtonBusyKey("commit_and_push")).toBe("busy.commit_and_push");
    expect(commitButtonBusyKey("fetch")).toBe("commit.commit_and_push");
  });

  test("cache freshness, discovery throttle, and autofetch wait", () => {
    const now = 1_000_000;
    expect(needsFetch({ stale: false, fetchedAt: now - 3_600_000 })).toBe(false);
    expect(needsFetch({ stale: true, fetchedAt: now })).toBe(true);
    expect(needsFetch(null)).toBe(true);

    expect(gitDiscoveryWait(null, now)).toBe(0);
    expect(gitDiscoveryWait({ stale: false, fetchedAt: now - 3_600_000 }, now)).toBeNull();
    expect(gitDiscoveryWait({ stale: true, fetchedAt: now - 5_000 }, now)).toBe(
      GIT_DISCOVERY_MIN_INTERVAL_MS - 5_000,
    );
    expect(gitDiscoveryWait({ stale: true, fetchedAt: now - 60_000 }, now)).toBe(0);

    expect(gitAutofetchWait(null, now)).toBe(0);
    expect(gitAutofetchWait(now, now)).toBe(GIT_AUTOFETCH_INTERVAL_MS);
    expect(gitAutofetchWait(now - 59_000, now)).toBe(1_000);
    expect(gitAutofetchWait(now - 61_000, now)).toBe(0);
  });
});

describe("unified patch parsing, line mapping, and feedback formatting", () => {
  test("parses file headers, hunks, renames, and builds valid hunk patch", () => {
    const patch =
      "diff --git a/old name.rs b/new name.rs\n" +
      "similarity index 90%\n" +
      "rename from old name.rs\n" +
      "rename to new name.rs\n" +
      "--- a/old name.rs\n" +
      "+++ b/new name.rs\n" +
      "@@ -10,2 +10,2 @@ fn foo()\n" +
      "-old\n" +
      "+new\n" +
      " ctx\n";
    const parsed = parseUnifiedPatch(patch);
    expect(parsed.files).toHaveLength(1);
    expect(parsed.files[0].path).toBe("new name.rs");
    expect(parsed.files[0].oldPath).toBe("old name.rs");
    expect(diffScrollTarget(parsed.files, "old name.rs")).toBe("new name.rs");
    expect(diffScrollTarget(parsed.files, "new name.rs")).toBe("new name.rs");
    expect(diffScrollTarget(parsed.files, "missing.rs")).toBeNull();

    const single = hunkPatch(parsed.header, parsed.hunks[0]);
    expect(single).toContain("diff --git a/old name.rs b/new name.rs");
    expect(single).toContain("@@ -10,2 +10,2 @@ fn foo()");
    expect(single).toContain("+new\n");

    expect(mapJoinedLineToFile(parsed, "new", 0)).toEqual({ path: "new name.rs", line: 10 });
    expect(mapJoinedLineToFile(parsed, "new", 1)).toEqual({ path: "new name.rs", line: 11 });
  });

  test("parseUnifiedPatch normalizes CRLF, handles EOF without newline, and supports includeRaw=false", () => {
    const crlf =
      "diff --git a/a.txt b/a.txt\r\n" +
      "--- a/a.txt\r\n" +
      "+++ b/a.txt\r\n" +
      "@@ -1,2 +1,2 @@\r\n" +
      "-old\r\n" +
      "+new\r\n" +
      " end";
    const full = parseUnifiedPatch(crlf);
    expect(full.files).toHaveLength(1);
    expect(full.hunks[0].raw).toBe("@@ -1,2 +1,2 @@\n-old\n+new\n end\n");
    expect(full.files[0].oldLineCount).toBe(2);
    expect(full.files[0].newLineCount).toBe(2);

    const light = parseUnifiedPatch(crlf, { includeRaw: false });
    expect(light.files[0].path).toBe("a.txt");
    expect(light.files[0].oldLineCount).toBe(2);
    expect(light.files[0].newLineCount).toBe(2);
    expect(light.hunks[0].raw).toBe("");
    expect(mapJoinedLineToFile(light, "new", 1)).toEqual({ path: "a.txt", line: 2 });
  });

  test("diffParseFailure & diffFeedbackText", () => {
    expect(diffParseFailure("  \n", "(No differences)")).toBe("(No differences)");
    expect(diffParseFailure("fatal: not a git repository", "(No differences)")).toBe(
      "fatal: not a git repository",
    );
    expect(diffParseFailure("diff --git a/f b/f\n@@ -1 +1 @@\n-a\n+b\n", "(No differences)")).toBeNull();

    expect(diffFeedbackText("Feedback on src/a.rs:", 8, 3, "hello", "look here")).toBe(
      "Feedback on src/a.rs:\nL3-L8\nhello\n\nlook here\n",
    );
    expect(diffFeedbackText("Feedback on src/a.rs:", 3, 3, "hello\n", "")).toBe(
      "Feedback on src/a.rs:\nL3\nhello\n",
    );
  });
});

describe("commit graph layout & SVG generation", () => {
  const c = (hash, parents) => ({ hash, parents });

  test("linear history stays in lane 0", () => {
    const rows = layoutGraph([c("a", ["b"]), c("b", ["c"]), c("c", [])]);
    expect(rows.every((r) => r.node === 0 && r.lanes <= 1)).toBe(true);
    expect(rows[1].edges).toContainEqual({ kind: "in", from: 0 });
    expect(rows[1].edges).toContainEqual({ kind: "out", to: 0 });
    expect(rows[2].edges.some((e) => e.kind === "out")).toBe(false);
  });

  test("merge commit forks a second lane and reuses freed lane after merge", () => {
    const rows = layoutGraph([
      c("m", ["a", "b"]),
      c("a", ["base"]),
      c("b", ["base"]),
      c("base", ["old"]),
      c("old", []),
    ]);
    expect(rows[0].node).toBe(0);
    const outs = rows[0].edges.filter((e) => e.kind === "out").map((e) => e.to);
    expect(outs).toHaveLength(2);
    expect(outs).toContain(0);
    expect(rows[1].node).not.toBe(rows[2].node);
    expect(rows[3].edges.filter((e) => e.kind === "in")).toHaveLength(2);
    expect(rows[4].node).toBe(0);
    expect(rows.every((r) => r.lanes <= 2)).toBe(true);

    const w = graphWidth(rows);
    const svg = commitGraphRowSvg(rows[0], w, 24, false);
    expect(svg).toContain("<svg");
    expect(svg).toContain("<circle");
    expect(svg).toContain("Q");
  });
});
