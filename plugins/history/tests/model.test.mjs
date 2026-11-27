import { describe, expect, test } from "bun:test";
import {
  continueLabel,
  detailFromWire,
  fallbackSource,
  filterSessions,
  formatCount,
  handoffTargets,
  isAgentSession,
  knownBindings,
  listKey,
  rowKey,
  rowLabels,
  sessionWhen,
  sourceTabs,
  submittedTitle,
  summaryFromWire,
  tabsWithSessions,
  toolSummary,
} from "../model.js";

const t = (key, params = {}) => {
  const dict = {
    "row.when_ran": "%{time} · ran %{minutes} min",
    "menu.continue_agent": "Continue conversation",
    "menu.continue_acp": "Continue via ACP",
  };
  let out = dict[key] ?? key;
  for (const [k, v] of Object.entries(params)) {
    out = out.replaceAll(`%{${k}}`, String(v));
  }
  return out;
};

const HISTORY_SOURCES = [
  { id: "claude", short_label: "Claude", acp_id: "claude", deletable: true },
  { id: "codex", short_label: "Codex", acp_id: "codex", deletable: true },
  { id: "pi", short_label: "Pi", acp_id: "pi", deletable: true },
];

const CONV_AGENTS = [
  { id: "claude", label: "Claude Code" },
  { id: "codex", label: "Codex" },
  { id: "pi", label: "Pi" },
];

describe("smelt.history model", () => {
  test("listKey separates machines, agents, profiles, and cwds", () => {
    const local = listKey(null, "claude", null, "/app");
    expect(local).not.toBe(listKey("box", "claude", null, "/app"));
    expect(local).not.toBe(listKey(null, "claude", "quant", "/app"));
    expect(local).not.toBe(listKey(null, "codex", null, "/app"));
    expect(local).not.toBe(listKey(null, "claude", null, "/other"));
  });

  test("rowLabels shows custom title first and agent title in secondary", () => {
    const labels = rowLabels(
      {
        title: "用户起的名",
        custom_title: "用户起的名",
        agent_title: "原始标题",
        started_at_ms: null,
        last_active_at_ms: null,
      },
      t,
    );
    expect(labels.title).toBe("用户起的名");
    expect(labels.secondary).toBe("原始标题");
  });

  test("rowKey prefers resume_id over path", () => {
    expect(rowKey({ resume_id: "resume-1", path: "/ignored" })).toBe("resume-1");
    expect(rowKey({ resume_id: "", path: "/tmp/session" })).toBe("/tmp/session");
  });

  test("summaryFromWire shows custom title first and formats ran minutes", () => {
    const summary = summaryFromWire({
      path: "/p",
      resume_id: "r",
      title: "原始",
      custom_title: "改过的",
      started_at_ms: 0,
      last_active_at_ms: 120_000,
      message_count: 3,
      total_tokens: 10,
    });
    expect(summary.title).toBe("改过的");
    expect(summary.agent_title).toBe("原始");
    expect(sessionWhen(summary, t)).toContain("ran 2 min");
  });

  test("sourceTabs lists all sources for ordinary projects and only engine for agent space", () => {
    const ordinary = sourceTabs(null, [], HISTORY_SOURCES);
    expect(ordinary.length).toBe(HISTORY_SOURCES.length);
    expect(ordinary.every((tab) => tab.profile_id == null)).toBe(true);

    const space = sourceTabs("pi", [], HISTORY_SOURCES);
    expect(space).toEqual([{ kind: "pi", profile_id: null, label: "Pi" }]);

    const withProfile = sourceTabs(
      null,
      [
        { id: "quant", label: "Claude Quant", kind: "claude", in_history_sources: true },
        { id: "custom", label: "Generic", kind: "custom", in_history_sources: false },
      ],
      HISTORY_SOURCES,
    );
    expect(withProfile.some((tab) => tab.profile_id === "quant")).toBe(true);
    expect(withProfile.some((tab) => tab.profile_id === "custom")).toBe(false);
  });

  test("tabsWithSessions hides empty sources but keeps selected tab", () => {
    const all = sourceTabs(null, [], HISTORY_SOURCES);
    const shown = tabsWithSessions(
      all,
      { kind: "claude", profile_id: null },
      (tab) => tab.kind === "codex",
    );
    expect(shown.map((tab) => tab.kind)).toEqual(["claude", "codex"]);
  });

  test("bound sessions and agent space continue as agent", () => {
    expect(isAgentSession(false, null)).toBe(false);
    expect(isAgentSession(false, "writer")).toBe(true);
    expect(isAgentSession(true, null)).toBe(true);
    expect(continueLabel(false, t)).toBe("Continue via ACP");
    expect(continueLabel(true, t)).toBe("Continue conversation");
  });

  test("submittedTitle distinguishes unchanged, renamed, and cleared", () => {
    expect(submittedTitle("旧名", " 旧名 ")).toBeUndefined();
    expect(submittedTitle("旧名", "新名")).toBe("新名");
    expect(submittedTitle("旧名", "   ")).toBeNull();
  });

  test("handoffTargets skips source itself unless profile differs", () => {
    const targets = handoffTargets("claude", null, CONV_AGENTS, []);
    expect(targets.every((item) => item.agent !== "claude")).toBe(true);
    expect(targets.length).toBe(CONV_AGENTS.length - 1);

    const fromProfile = handoffTargets("claude", "quant", CONV_AGENTS, [
      { id: "quant", label: "Claude Quant", kind: "claude", launchable: true },
      { id: "other", label: "Claude Other", kind: "claude", launchable: true },
      { id: "broken", label: "Broken", kind: "codex", launchable: false },
    ]);
    expect(fromProfile.some((item) => item.agent === "claude" && item.profile_id == null)).toBe(
      true,
    );
    expect(fromProfile.some((item) => item.profile_id === "quant")).toBe(false);
    expect(fromProfile.some((item) => item.profile_id === "other")).toBe(true);
    expect(fromProfile.some((item) => item.profile_id === "broken")).toBe(false);
  });

  test("knownBindings keeps only definitions still present on that machine", () => {
    const bindings = { a: "gone", b: "writer" };
    const settings = { agents: [{ id: "writer" }] };
    expect(knownBindings(bindings, settings)).toEqual({ b: "writer" });
    expect(knownBindings(bindings, null)).toEqual({});
  });

  test("toolSummary preserves order and counts duplicates", () => {
    expect(toolSummary([])).toBeNull();
    expect(toolSummary(["Read", "Edit", "Read"])).toBe("Read ×2 · Edit");
  });

  test("formatCount and filterSessions", () => {
    expect(formatCount(950)).toBe("950");
    expect(formatCount(1500)).toBe("1.5K");
    expect(formatCount(2_500_000)).toBe("2.5M");
    const list = [
      { title: "Fix bug", agent_title: "Original A" },
      { title: "Refactor", agent_title: "Custom B" },
    ];
    expect(filterSessions(list, "original").length).toBe(1);
    expect(filterSessions(list, "  ").length).toBe(2);
  });
});

describe("fallbackSource", () => {
  const tabs = [
    { kind: "claude", profile_id: null, label: "Claude" },
    { kind: "antigravity", profile_id: null, label: "Antigravity" },
  ];
  const claude = { kind: "claude", profile_id: null };

  const onlyAntigravity = (tab) => tab.kind === "antigravity";

  test("当前来源读完才换，换到第一个有会话的", () => {
    expect(fallbackSource(tabs, claude, false, false, onlyAntigravity)).toBeNull();
    expect(fallbackSource(tabs, claude, true, false, () => true)).toBeNull();
    expect(fallbackSource(tabs, claude, true, false, onlyAntigravity)).toEqual({
      kind: "antigravity",
      profile_id: null,
    });
    expect(fallbackSource(tabs, claude, true, false, () => false)).toBeNull();
  });

  test("用户自己点的空标签不换走", () => {
    expect(fallbackSource(tabs, claude, true, true, onlyAntigravity)).toBeNull();
  });

  test("选中的来源已经不在标签里也换，用户点过也一样", () => {
    const gone = { kind: "claude", profile_id: "gone" };
    expect(fallbackSource(tabs, gone, true, false, onlyAntigravity)).toEqual({
      kind: "antigravity",
      profile_id: null,
    });
    expect(fallbackSource(tabs, gone, true, true, onlyAntigravity)).toEqual({
      kind: "antigravity",
      profile_id: null,
    });
  });
});

describe("detailFromWire", () => {
  test("正文是 Blob 原样留着，空正文算没有；total 是整份的轮数", () => {
    const body = new Blob(["答完了"]);
    const page = detailFromWire({
      total: 120,
      turns: [
        { is_user: false, text: body, tools: ["Bash"] },
        { is_user: true, interrupted: true, text: new Blob(["[Request interrupted by user]"]) },
        { is_user: false, text: new Blob([]), tools: ["Read"] },
      ],
    });
    expect(page.total).toBe(120);
    expect(page.turns[0].text).toBe(body);
    expect(page.turns[0].tools).toEqual(["Bash"]);
    expect(page.turns[1].interrupted).toBe(true);
    expect(page.turns[2].text).toBeNull();
  });

  test("没给 total 时就是这一页的轮数", () => {
    expect(detailFromWire({ turns: [{ is_user: true, text: new Blob(["x"]) }] }).total).toBe(1);
  });
});
