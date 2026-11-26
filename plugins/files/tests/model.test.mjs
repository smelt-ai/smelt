import { describe, expect, test } from "bun:test";
import {
  ancestorsUnderRoot,
  breadcrumbSegments,
  buildSearchTree,
  collectFileTreeRows,
  editorLanguageForPath,
  fileTreeRowId,
  fileTreeRowPath,
  flattenSearchTree,
  gitStatusMark,
  isHtmlPath,
  isMarkdownPath,
  pathToFileUrl,
  treeEntries,
  treeRoots,
} from "../model.js";

const posixOps = {
  isWindows: false,
  join: (base, child) => `${base.replace(/\/+$/, "")}/${child}`,
  fileName: (p) => {
    const clean = p.replace(/\/+$/, "");
    if (!clean) return null;
    const idx = clean.lastIndexOf("/");
    return idx >= 0 ? clean.slice(idx + 1) : clean;
  },
  startsWith: (p, base) => {
    const b = base.replace(/\/+$/, "");
    return p === b || p.startsWith(`${b}/`);
  },
  stripPrefix: (p, base) => {
    const b = base.replace(/\/+$/, "");
    if (p === b) return "";
    if (p.startsWith(`${b}/`)) return p.slice(b.length + 1);
    return null;
  },
};

const windowsOps = {
  isWindows: true,
  join: (base, child) => `${base.replace(/[/\\]+$/, "")}\\${child}`,
  fileName: (p) => {
    const clean = p.replace(/[/\\]+$/, "");
    const idx = Math.max(clean.lastIndexOf("\\"), clean.lastIndexOf("/"));
    return idx >= 0 ? clean.slice(idx + 1) : clean;
  },
  startsWith: (p, base) => {
    const norm = (s) => s.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
    const np = norm(p);
    const nb = norm(base);
    return np === nb || np.startsWith(`${nb}/`);
  },
  stripPrefix: (p, base) => {
    const np = p.replace(/\\/g, "/");
    const nb = base.replace(/\\/g, "/").replace(/\/+$/, "");
    if (np.toLowerCase() === nb.toLowerCase()) return "";
    if (np.toLowerCase().startsWith(`${nb.toLowerCase()}/`)) return np.slice(nb.length + 1);
    return null;
  },
};

function cached(entries) {
  return {
    entries: entries.map(([name, is_dir]) => ({ name, is_dir })),
    stale: false,
  };
}

describe("breadcrumbSegments", () => {
  test("splits path under workspace root", () => {
    expect(
      breadcrumbSegments(
        posixOps,
        "/workspace/project/crates/smelt-core/tests/fake_llm.py",
        ["/workspace/project"],
      ),
    ).toEqual([
      { label: "project", path: "/workspace/project" },
      { label: "crates", path: "/workspace/project/crates" },
      { label: "smelt-core", path: "/workspace/project/crates/smelt-core" },
      { label: "tests", path: "/workspace/project/crates/smelt-core/tests" },
      { label: "fake_llm.py", path: "/workspace/project/crates/smelt-core/tests/fake_llm.py" },
    ]);
  });

  test("prefers longest matching root", () => {
    expect(
      breadcrumbSegments(
        posixOps,
        "/repo/crates/smelt-core/src/lib.rs",
        ["/repo", "/repo/crates/smelt-core"],
      ),
    ).toEqual([
      { label: "smelt-core", path: "/repo/crates/smelt-core" },
      { label: "src", path: "/repo/crates/smelt-core/src" },
      { label: "lib.rs", path: "/repo/crates/smelt-core/src/lib.rs" },
    ]);
  });

  test("falls back to filename outside roots", () => {
    expect(breadcrumbSegments(posixOps, "/tmp/scratch.rs", ["/repo"])).toEqual([
      { label: "scratch.rs", path: "/tmp/scratch.rs" },
    ]);
  });

  test("splits Windows paths on both separators", () => {
    const segs = breadcrumbSegments(windowsOps, "C:\\repo\\src\\lib.rs", ["C:\\repo"]);
    expect(segs.map((s) => s.label)).toEqual(["repo", "src", "lib.rs"]);
  });
});

describe("collectFileTreeRows", () => {
  test("single root keeps walk order without header and attaches git marks", () => {
    const cache = new Map([
      ["/repo", cached([["a.txt", false], ["src", true]])],
      ["/repo/src", cached([["lib.rs", false]])],
    ]);
    const expanded = new Set(["/repo/src"]);
    const git = new Map([
      [
        "/repo",
        {
          files: [
            [" M", "src/lib.rs"],
            ["??", "a.txt"],
          ],
        },
      ],
    ]);

    expect(
      collectFileTreeRows(
        posixOps,
        { roots: ["/repo"], pinned: [], collapsed: new Set() },
        expanded,
        cache,
        (root) => git.get(root),
      ),
    ).toEqual([
      {
        kind: "entry",
        depth: 0,
        name: "a.txt",
        isDir: false,
        path: "/repo/a.txt",
        expanded: false,
        gitMark: "U",
      },
      {
        kind: "entry",
        depth: 0,
        name: "src",
        isDir: true,
        path: "/repo/src",
        expanded: true,
        gitMark: null,
      },
      {
        kind: "entry",
        depth: 1,
        name: "lib.rs",
        isDir: false,
        path: "/repo/src/lib.rs",
        expanded: false,
        gitMark: "M",
      },
    ]);
  });

  test("row identity follows path when a directory expands", () => {
    const cache = new Map([
      ["/repo", cached([["src", true], ["b.txt", false]])],
      ["/repo/src", cached([["a.rs", false]])],
    ]);
    const collapsed = collectFileTreeRows(
      posixOps,
      { roots: ["/repo"], pinned: [], collapsed: new Set() },
      new Set(),
      cache,
      null,
    );
    const expanded = collectFileTreeRows(
      posixOps,
      { roots: ["/repo"], pinned: [], collapsed: new Set() },
      new Set(["/repo/src"]),
      cache,
      null,
    );

    const findRow = (list, p) => list.find((r) => fileTreeRowPath(r) === p);
    expect(fileTreeRowId(findRow(collapsed, "/repo/b.txt"))).toBe("file:/repo/b.txt");
    expect(fileTreeRowId(findRow(expanded, "/repo/b.txt"))).toBe("file:/repo/b.txt");
    expect(collapsed.findIndex((r) => fileTreeRowPath(r) === "/repo/b.txt")).not.toBe(
      expanded.findIndex((r) => fileTreeRowPath(r) === "/repo/b.txt"),
    );
  });

  test("multi root header collapses children and marks pinned", () => {
    const cache = new Map([
      ["/a", cached([["a.txt", false]])],
      ["/b", cached([["b.txt", false]])],
    ]);
    expect(
      collectFileTreeRows(
        posixOps,
        { roots: ["/a", "/b"], pinned: ["/b"], collapsed: new Set(["/b"]) },
        new Set(),
        cache,
        null,
      ),
    ).toEqual([
      { kind: "root_header", path: "/a", open: true, pinned: false },
      {
        kind: "entry",
        depth: 1,
        name: "a.txt",
        isDir: false,
        path: "/a/a.txt",
        expanded: false,
        gitMark: null,
      },
      { kind: "root_header", path: "/b", open: false, pinned: true },
    ]);
  });

  test("shows loading for uncached roots", () => {
    expect(
      collectFileTreeRows(
        posixOps,
        { roots: ["/a", "/b"], pinned: [], collapsed: new Set() },
        new Set(),
        new Map(),
        null,
      ),
    ).toEqual([
      { kind: "root_header", path: "/a", open: true, pinned: false },
      { kind: "loading", depth: 1, root: "/a" },
      { kind: "root_header", path: "/b", open: true, pinned: false },
      { kind: "loading", depth: 1, root: "/b" },
    ]);
  });
});

describe("search tree", () => {
  test("keeps matching files under their ancestor directories and flattens in order", () => {
    const hits = [
      { path: "/repo/crates/smelt-core/src/lib.rs", rel: "crates/smelt-core/src/lib.rs", line: null },
      {
        path: "/repo/mobile/lib/pages/qr_scan.dart",
        rel: "mobile/lib/pages/qr_scan.dart",
        line: [8, "  library foo;"],
      },
    ];
    const tree = buildSearchTree(hits);
    expect(tree.dirs.get("crates").dirs.get("smelt-core").dirs.get("src").files).toEqual([0]);
    expect(tree.dirs.get("mobile").dirs.get("lib").dirs.get("pages").files).toEqual([1]);

    const rows = flattenSearchTree(tree, hits);
    const dartHit = rows.find((r) => r.kind === "file" && r.name === "qr_scan.dart");
    expect(dartHit.line).toBe(8);
    expect(dartHit.snippet).toBe("library foo;");
  });
});

describe("file helpers", () => {
  test("recognizes markdown, html, language, and file URLs", () => {
    expect(isMarkdownPath("/a/README.md")).toBe(true);
    expect(isHtmlPath("/a/index.html")).toBe(true);
    expect(editorLanguageForPath("/a/Makefile")).toBe("makefile");
    expect(editorLanguageForPath("/a/main.rs")).toBe("rs");

    expect(pathToFileUrl("/tmp/hello world/index.html")).toBe("file:///tmp/hello%20world/index.html");
    expect(pathToFileUrl("C:\\my docs\\a.html")).toBe("file:///C:/my%20docs/a.html");
    expect(pathToFileUrl("relative/a.html")).toBeNull();

    expect(gitStatusMark(" M")).toBe("M");
    expect(gitStatusMark("??")).toBe("U");
    expect(gitStatusMark("A ")).toBe("A");
    expect(gitStatusMark(" D")).toBe("D");
  });

  test("treeRoots hides local pinned roots on remote projects", () => {
    expect(treeRoots({ machine: null, root: "/a" }, ["/a", "/b"])).toEqual(["/a", "/b"]);
    expect(treeRoots({ machine: "box", root: "/remote" }, ["/b"])).toEqual(["/remote"]);
  });

  test("ancestorsUnderRoot lists intermediate directories from shallow to deep", () => {
    expect(ancestorsUnderRoot(posixOps, "/repo/a/b/c.rs", "/repo")).toEqual([
      "/repo/a",
      "/repo/a/b",
    ]);
    expect(ancestorsUnderRoot(posixOps, "/other/c.rs", "/repo")).toEqual([]);
  });
});

describe("treeEntries", () => {
  test("滤掉噪音项，目录在前、按名字不区分大小写排，指向目录的链接当目录", () => {
    const raw = [
      { name: "beta.rs", kind: "file" },
      { name: ".git", kind: "dir" },
      { name: "zeta", kind: "dir" },
      { name: "Alpha.md", kind: "file" },
      { name: "src", kind: "dir" },
      { name: "link", kind: "symlink", target_kind: "dir" },
      { name: "dangling", kind: "symlink", target_kind: null },
      { name: ".DS_Store", kind: "file" },
    ];
    expect(treeEntries(raw)).toEqual([
      { name: "link", is_dir: true },
      { name: "src", is_dir: true },
      { name: "zeta", is_dir: true },
      { name: "Alpha.md", is_dir: false },
      { name: "beta.rs", is_dir: false },
      { name: "dangling", is_dir: false },
    ]);
  });

  test("不是数组时是空的", () => {
    expect(treeEntries(undefined)).toEqual([]);
  });
});
