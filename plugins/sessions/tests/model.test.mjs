import { describe, expect, test } from "bun:test";
import * as contract from "../contract.js";
import {
  childSafeArea,
  contextRoot,
  conversationTitle,
  extractMachineSettings,
  findResumedConversation,
  forkedAcp,
  idToCopy,
  leadPane,
  listedRoots,
  locateSession,
  paneSpecs,
  paneTitle,
  parseSessionArgs,
  selectionToWire,
  shouldOfferAddMachine,
  shownConversation,
  terminalPaneKey,
  terminalWriteErrorKey,
  worksetOfCwd,
} from "../model.js";
import {
  cyclePaneId,
  decodeTree,
  defaultTree,
  encodeTree,
  leaf,
  leafOrder,
  paneTouches,
  reconcileTree,
  removePane,
  restoreTree,
  split,
  splitPaneAt,
  updateSplitSizes,
} from "../split_tree.js";

const t = (key, params = {}) => {
  const table = {
    "kind.conversation": "Conversation",
    "kind.terminal": "Terminal",
    "error.agent_settings_loading": "Still reading that machine's agent settings.",
    "error.agent_settings_unavailable": `Unavailable: ${params.reason ?? ""}`,
    "error.machine_removed": "This machine has been removed",
  };
  return table[key] ?? key;
};

const posix = {
  clean: (p) => {
    const trimmed = p.replace(/\/+$/, "");
    return trimmed || "/";
  },
  join: (a, b) => (a.endsWith("/") ? `${a}${b}` : `${a}/${b}`),
  isAbsolute: (p) => p.startsWith("/"),
  startsWith: (p, base) => p === base || p.startsWith(`${base}/`),
};

describe("sessions contract", () => {
  test("every id is under smelt.sessions/ and unique", () => {
    const ids = [
      contract.SESSION_VIEW,
      contract.SELECTION_CHANGED,
      contract.CONVERSATION_CREATED,
      contract.ACTIVATE,
      contract.START_CONVERSATION,
      contract.FORK,
      contract.RESUME,
      contract.RESUME_IN_TERMINAL,
      contract.HANDOFF,
      contract.SEND_TEXT,
      contract.SELECTION,
      contract.CREATE_CONVERSATION,
      contract.SELECT_PROJECT,
      contract.PREV_PANE,
      contract.NEXT_PANE,
      contract.SPLIT_RIGHT,
      contract.SPLIT_DOWN,
      contract.CLOSE_PANE,
      contract.COPY_SESSION_ID,
    ];
    const seen = new Set();
    for (const id of ids) {
      expect(id.startsWith("smelt.sessions/")).toBe(true);
      expect(seen.has(id)).toBe(false);
      seen.add(id);
    }
  });
});

describe("sessions split_tree (v2 binary split)", () => {
  test("saved layout round-trips cleanly and drops legacy v1 or malformed json", () => {
    const tree = split(
      "horizontal",
      leaf("acp"),
      split("vertical", leaf("t1"), leaf("t2"), [300, 200]),
      [600, 400],
    );
    const encoded = encodeTree(tree);
    expect(restoreTree(encoded, ["acp", "t1", "t2"])).toEqual(tree);

    // 旧版 v1（DockArea 叠标签）或不合法 JSON 一律回退默认横排。
    const fallback = split("horizontal", leaf("acp"), leaf("t1"), null);
    expect(restoreTree(null, ["acp", "t1"])).toEqual(fallback);
    expect(restoreTree("not json", ["acp", "t1"])).toEqual(fallback);
    expect(restoreTree(JSON.stringify({ v: 1, root: { pane: "acp" } }), ["acp", "t1"])).toEqual(
      fallback,
    );
    expect(decodeTree(JSON.stringify({ v: 99, root: leaf("acp") }))).toBeNull();
  });

  test("gone panes collapse parent splits and new panes append on the right", () => {
    const tree = split(
      "horizontal",
      leaf("acp"),
      split("vertical", leaf("t1"), leaf("t2"), [300, 200]),
      [600, 400],
    );
    const encoded = encodeTree(tree);
    // 去掉 t1：右侧 vertical 坍缩成 t2，外层保留 sizes。
    expect(restoreTree(encoded, ["acp", "t2"])).toEqual(
      split("horizontal", leaf("acp"), leaf("t2"), [600, 400]),
    );
    // 去掉 t1、t2：只剩 acp 叶子。
    expect(restoreTree(encoded, ["acp"])).toEqual(leaf("acp"));
    // 新增 t3：追加到最右侧。
    expect(restoreTree(encoded, ["acp", "t1", "t2", "t3"])).toEqual(
      split("horizontal", tree, leaf("t3"), null),
    );
  });

  test("defaultTree deduplicates and handles empty/single/multi", () => {
    expect(defaultTree([])).toBeNull();
    expect(defaultTree(["a"])).toEqual(leaf("a"));
    expect(defaultTree(["a", "a"])).toEqual(leaf("a"));
    expect(defaultTree(["a", "b", "c"])).toEqual(
      split("horizontal", split("horizontal", leaf("a"), leaf("b")), leaf("c")),
    );
  });

  test("splitPaneAt, removePane, updateSplitSizes, leafOrder, cyclePaneId", () => {
    let tree = leaf("acp");
    tree = splitPaneAt(tree, "acp", "horizontal", "t1");
    tree = splitPaneAt(tree, "t1", "vertical", "t2");
    expect(leafOrder(tree)).toEqual(["acp", "t1", "t2"]);

    tree = updateSplitSizes(tree, [1], [250, 150]);
    expect(tree.second.sizes).toEqual([250, 150]);

    expect(cyclePaneId(leafOrder(tree), "acp", true)).toBe("t1");
    expect(cyclePaneId(leafOrder(tree), "t2", true)).toBe("acp");
    expect(cyclePaneId(leafOrder(tree), "acp", false)).toBe("t2");

    tree = removePane(tree, "t1");
    expect(tree).toEqual(split("horizontal", leaf("acp"), leaf("t2"), null));
  });

  test("reconcileTree places expected split panes beside target on requested axis", () => {
    const initial = leaf("acp");
    const expected = new Map([["t1", { beside: "acp", axis: "vertical" }]]);
    const next = reconcileTree(initial, ["acp", "t1"], expected);
    expect(next).toEqual(split("vertical", leaf("acp"), leaf("t1"), null));
  });

  test("paneTouches propagates outer edges for safeArea distribution", () => {
    const tree = split(
      "horizontal",
      leaf("acp"),
      split("vertical", leaf("t1"), leaf("t2")),
    );
    const touches = paneTouches(tree);
    expect(touches.get("acp")).toEqual({ top: true, right: false, bottom: true, left: true });
    expect(touches.get("t1")).toEqual({ top: true, right: true, bottom: false, left: false });
    expect(touches.get("t2")).toEqual({ top: false, right: true, bottom: true, left: false });

    const parent = { title_bar: true, top_left: 78, top_right: 20, bottom_left: 0, bottom_right: 0 };
    expect(childSafeArea(parent, touches.get("acp"))).toEqual({
      title_bar: true,
      top_left: 78,
      top_right: 0,
      bottom_left: 0,
      bottom_right: 0,
    });
    expect(childSafeArea(parent, touches.get("t2"))).toEqual({
      title_bar: false,
      top_left: 0,
      top_right: 0,
      bottom_left: 0,
      bottom_right: 0,
    });
  });
});

describe("sessions model", () => {
  test("locateSession waits only until the written revision", () => {
    const directory = {
      epoch: "e",
      revision: 3,
      conversations: [{ id: "c", terminals: [{ id: "t1", cwd: "/repo" }] }],
    };
    const facts = [{ id: "c", placement: { kind: "project", root: "/repo", workset: null } }];

    expect(locateSession("box", { session_id: "t1" }, null, directory, facts)).toEqual({
      kind: "found",
      machine: "box",
      id: "c",
    });
    expect(
      locateSession("box", { conversation_id: "new" }, { epoch: "e", revision: 4 }, directory, facts),
    ).toEqual({ kind: "wait" });
    expect(
      locateSession("box", { conversation_id: "new" }, { epoch: "e", revision: 3 }, directory, facts),
    ).toEqual({ kind: "missing" });
    expect(
      locateSession("box", { conversation_id: "new" }, { epoch: "f", revision: 9 }, directory, facts),
    ).toEqual({ kind: "missing" });
    expect(locateSession("box", { conversation_id: "c" }, { epoch: "e", revision: 1 }, null, [])).toEqual({
      kind: "wait",
    });
    expect(locateSession("box", { conversation_id: "c" }, null, null, [])).toEqual({
      kind: "missing",
    });

    // 归档（hidden）等待新版本
    const hiddenFacts = [{ id: "c", placement: { kind: "hidden" } }];
    expect(
      locateSession("box", { session_id: "t1" }, { epoch: "e", revision: 4 }, directory, hiddenFacts),
    ).toEqual({ kind: "wait" });
    expect(shownConversation(directory, hiddenFacts, "c")).toBeNull();
  });

  test("contextRoot follows placement: workset, project, conversations, hidden", () => {
    const directory = {
      projects: ["/repo"],
      worksets: [{ id: "ws", project_root: "/repo", task: "fix" }],
      conversations: [],
    };
    expect(
      contextRoot(
        directory,
        { placement: { kind: "project", root: "/repo", workset: "ws" }, cwd: "/elsewhere" },
        posix.join,
      ),
    ).toBe("/repo/worktrees/fix");
    expect(
      contextRoot(
        directory,
        { placement: { kind: "project", root: "/repo", workset: null }, cwd: "/repo/sub" },
        posix.join,
      ),
    ).toBe("/repo");
    expect(
      contextRoot(
        directory,
        { placement: { kind: "conversations" }, cwd: "/home/u/.smelt/agents/writer" },
        posix.join,
      ),
    ).toBe("/home/u/.smelt/agents/writer");
    expect(contextRoot(directory, { placement: { kind: "hidden" }, cwd: "/repo" }, posix.join)).toBeNull();
  });

  test("leadPane, paneSpecs, paneTitle, conversationTitle, idToCopy", () => {
    const conv = {
      id: "c",
      custom_title: "My Conv",
      acp: { sid: "acp-1", history_session_id: "h1" },
      terminals: [
        { id: "t1", cwd: "/repo", custom_title: "Build", launch_cmd: null },
        { id: null, cwd: "/repo" },
      ],
    };
    expect(leadPane(conv)).toEqual({ session_id: "acp-1", kind: "conversation" });
    const specs = paneSpecs(null, conv, { fork: null, prefill: "hi" });
    expect(specs.map((s) => [s.session_id, s.kind])).toEqual([
      ["acp-1", "conversation"],
      ["t1", "terminal"],
    ]);
    expect(specs[0].args.start.prefill).toBe("hi");

    expect(paneTitle(conv, "terminal", "t1", [], t)).toBe("Build");
    expect(paneTitle(conv, "terminal", "t2", [], t)).toBe("Terminal");
    expect(paneTitle(conv, "conversation", "acp-1", [], t)).toBe("Conversation");
    expect(conversationTitle(conv, { name: "Runtime Name" }, t)).toBe("Runtime Name");
    expect(conversationTitle(conv, null, t)).toBe("My Conv");

    const dir = { conversations: [conv] };
    expect(idToCopy(dir, [], "acp-1")).toBe("h1");
    expect(idToCopy(dir, [{ id: "t1", conversation_id: " tui-9 " }], "t1")).toBe("tui-9");
    expect(idToCopy(dir, [], "t1")).toBe("t1");
  });

  test("forkedAcp, findResumedConversation, worksetOfCwd, extractMachineSettings", () => {
    const source = {
      cwd: "/repo",
      sid: "acp-a",
      agent: "pi",
      profile_id: "p",
      history_session_id: "h",
      launch: { command: "pi" },
      config_values: [["model", "a/b"]],
      pending_prompt: "draft",
      agent_definition_id: "writer",
      automation_id: "auto",
    };
    const forked = forkedAcp(source, "acp-b");
    expect(forked.sid).toBe("acp-b");
    expect(forked.history_session_id).toBeNull();
    expect(forked.pending_prompt).toBeNull();
    expect(forked.automation_id).toBeNull();
    expect(forked.agent_definition_id).toBe("writer");

    const dir = {
      conversations: [
        { id: "old", acp: { agent: "claude", history_session_id: "h1", cwd: "/repo/" } },
        { id: "open", acp: { agent: "claude", history_session_id: "h1", cwd: "/repo/" } },
      ],
      worksets: [{ id: "w1", project_root: "/repo", task: "fix" }],
    };
    const facts = [
      { id: "old", placement: { kind: "hidden" } },
      { id: "open", placement: { kind: "project", root: "/repo" } },
    ];
    expect(findResumedConversation(dir, facts, "claude", "/repo", "h1", posix.clean)).toBe("open");
    expect(worksetOfCwd(dir.worksets, "/repo/worktrees/fix/app", posix)).toBe("w1");
    expect(worksetOfCwd(dir.worksets, "/repo/worktrees/fixed", posix)).toBeNull();

    const settingsList = [
      { machine: null, state: "ready", settings: { a: 1 } },
      { machine: "old", state: "unavailable", reason: "too old" },
    ];
    expect(extractMachineSettings(settingsList, null, t)).toEqual({ ok: true, settings: { a: 1 } });
    expect(extractMachineSettings(settingsList, "box", t).ok).toBe(false);
    expect(extractMachineSettings(settingsList, "old", t).error).toContain("too old");
  });

  test("shouldOfferAddMachine, listedRoots, selectionToWire, parseSessionArgs", () => {
    expect(shouldOfferAddMachine(true, 0)).toBe(true);
    expect(shouldOfferAddMachine(true, 1)).toBe(false);
    expect(shouldOfferAddMachine(false, 0)).toBe(false);

    const roots = listedRoots(
      { projects: ["/repo"] },
      [{ id: "l", placement: { kind: "project", root: "/tmp/x" } }],
    );
    expect(roots.has("/repo") && roots.has("/tmp/x")).toBe(true);

    expect(selectionToWire({ kind: "none" })).toEqual({ kind: "none" });
    expect(
      selectionToWire({ kind: "conversation", machine: "box", conversation_id: "c" }),
    ).toEqual({ kind: "conversation", machine: "box", conversation_id: "c" });

    expect(parseSessionArgs({ machine: null, session_id: "t1" })).toEqual({
      machine: null,
      at: { session_id: "t1" },
      after: null,
    });
    expect(parseSessionArgs(null)).toBeNull();

    expect(terminalPaneKey(null, "t1")).not.toBe(terminalPaneKey("box", "t1"));
    expect(terminalPaneKey(null, "t1")).toBe(terminalPaneKey(undefined, "t1"));
    expect(terminalWriteErrorKey("overflow")).toBe("terminal.input_overflow");
    expect(terminalWriteErrorKey("session_ended")).toBe("terminal.input_session_ended");
    expect(terminalWriteErrorKey("reopened")).toBe("terminal.input_reopened");
    expect(terminalWriteErrorKey("other")).toBeNull();
  });
});
