import { describe, expect, test } from "bun:test";
import {
  applyPendingConfigSelection,
  buildAcpLaunchPayload,
  buildCompletionCandidates,
  buildConversationLayout,
  buildDisplayRows,
  buildForkCutAfter,
  buildForkCutBefore,
  buildVisibleModels,
  collectEditFilesFromRange,
  colorWithAlpha,
  compactTokenCount,
  composerSweepMode,
  computePromptRailWindow,
  consumeComposerRestore,
  decodeFileUri,
  detectCompletionTrigger,
  diffLineStats,
  effortDotColor,
  effortProgress,
  isThinkingConfig,
  mergeEnvelope,
  mergeSnapshotEntries,
  overlayModelState,
  reconcilePendingConfigValues,
  selectedConfigIndex,
  subsequenceScore,
  usagePercent,
} from "./model.js";

describe("mergeSnapshotEntries", () => {
  test("merges initial, incremental tail, and prepended history pages", () => {
    const s0 = mergeSnapshotEntries(
      { entries: [], loadedOffset: 0, entriesTotal: 0 },
      10,
      12,
      [{ User: "u1" }, { Assistant: { text: "a1", thought: false } }],
      true,
    );
    expect(s0.loadedOffset).toBe(10);
    expect(s0.entries.length).toBe(2);

    const s1 = mergeSnapshotEntries(
      s0,
      11,
      13,
      [
        { Assistant: { text: "a1 updated", thought: false } },
        { Assistant: { text: "a2", thought: false } },
      ],
      false,
    );
    expect(s1.loadedOffset).toBe(10);
    expect(s1.entries.length).toBe(3);
    expect(s1.entries[1].Assistant.text).toBe("a1 updated");

    const s2 = mergeSnapshotEntries(
      s1,
      8,
      13,
      [{ User: "old1" }, { Assistant: { text: "oldA", thought: false } }],
      false,
    );
    expect(s2.loadedOffset).toBe(8);
    expect(s2.entries.length).toBe(5);
    expect(s2.entries[0].User).toBe("old1");
    expect(s2.entries[4].Assistant.text).toBe("a2");
  });

  test("ignores empty mismatched frame instead of wiping loaded entries", () => {
    const s0 = { entries: [{ User: "keep" }], loadedOffset: 5, entriesTotal: 6 };
    const s1 = mergeSnapshotEntries(s0, 0, 2, [], false);
    expect(s1.dirtyFrom).toBeNull();
    expect(s1.entries.length).toBe(1);
  });
});

describe("buildConversationLayout & buildDisplayRows", () => {
  test("groups process tools, folds multi-segment turns, and marks final answer", () => {
    const entries = [
      { User: "fix bug" },
      { Assistant: { text: "thinking...", thought: true } },
      {
        ToolCall: {
          id: "t1",
          title: "src/lib.rs",
          kind: "read",
          status: "completed",
          output: [],
          children: [],
        },
      },
      { Assistant: { text: "mid note", thought: false } },
      {
        ToolCall: {
          id: "t2",
          title: "src/lib.rs",
          kind: "edit",
          status: "completed",
          output: [{ Diff: { path: "/repo/src/lib.rs", old_text: "a\nb", new_text: "a\nc\nd" } }],
          children: [],
        },
      },
      { Assistant: { text: "All fixed.", thought: false } },
    ];
    const layout = buildConversationLayout(entries, false, 0, [
      { user_index: 0, started_at_ms: 1000, finished_at_ms: 5200 },
    ]);
    expect(layout[5].finalAnswer).toBe(true);
    expect(layout[1].turnFold).not.toBeNull();
    expect(layout[1].turnFold.elapsedMs).toBe(4200);
    expect(layout[1].turnFold.tally.reads).toBe(1);
    expect(layout[1].turnFold.tally.edits).toBe(1);

    const collapsed = buildDisplayRows(entries, layout, 0, new Set(), new Set());
    expect(collapsed.map((r) => r.kind)).toEqual(["entry", "turn_fold", "entry"]);

    const edits = collectEditFilesFromRange(entries, 0, entries.length);
    expect(edits.length).toBe(1);
    expect(edits[0].added).toBe(2);
    expect(edits[0].removed).toBe(1);
  });

  test("active turn has no provisional final answer", () => {
    const entries = [
      { User: "hi" },
      { Assistant: { text: "streaming...", thought: false } },
    ];
    const layout = buildConversationLayout(entries, true, 0, []);
    expect(layout[1].finalAnswer).toBe(false);
  });
});

describe("completion", () => {
  test("detects @ and / at word start and ignores embedded slashes", () => {
    expect(detectCompletionTrigger("@src/ma")).toEqual({ kind: "at", start: 0, needle: "src/ma" });
    expect(detectCompletionTrigger("check /comp")).toEqual({ kind: "slash", start: 6, needle: "comp" });
    expect(detectCompletionTrigger("a@b.com")).toBeNull();
    expect(detectCompletionTrigger("foo/bar")).toBeNull();
  });

  test("ranks files and filters placeholder command hints", () => {
    const trig = detectCompletionTrigger("/co");
    const items = buildCompletionCandidates(trig, [], [["compact", "..."], ["commit", "Create git commit"]]);
    expect(items.find((i) => i.label === "/compact")?.hint).toBe("");
    expect(items.find((i) => i.label === "/commit")?.hint).toBe("Create git commit");

    const fileTrig = detectCompletionTrigger("@");
    const fItems = buildCompletionCandidates(fileTrig, [".github/ci.yml", "README.md", "src/main.rs"], []);
    expect(fItems[0].label).toBe("@README.md");
  });
});

describe("model search, pending config overlay, prompt rail, formatting", () => {
  test("subsequence scoring & cross-provider search", () => {
    expect(subsequenceScore("Claude Sonnet 4", "cs4")).toBeGreaterThan(0);
    expect(subsequenceScore("Claude Sonnet 4", "xyz")).toBeNull();

    const model = {
      config_id: "model",
      current_value: "anthropic/sonnet",
      current_name: "Sonnet",
      options: [
        ["anthropic/sonnet", "Sonnet"],
        ["openai/gpt-5", "GPT-5"],
      ],
      provider_groups: [
        { id: "anthropic", name: "Anthropic", options: [["anthropic/sonnet", "Sonnet"]] },
        { id: "openai", name: "OpenAI", options: [["openai/gpt-5", "GPT-5"]] },
      ],
    };
    const found = buildVisibleModels(model, "anthropic", "gpt");
    expect(found).toEqual([{ value: "openai/gpt-5", label: "GPT-5", providerTag: "OpenAI" }]);

    let pending = applyPendingConfigSelection([], "model", "openai/gpt-5", "anthropic/sonnet");
    expect(overlayModelState(model, pending).current_name).toBe("GPT-5");
    pending = reconcilePendingConfigValues(pending, [], { ...model, current_value: "openai/gpt-5" });
    expect(pending).toEqual([]);
  });

  test("prompt rail windows long conversations around reading prompt", () => {
    const prompts = Array.from({ length: 30 }, (_, i) => ({
      globalIndex: i * 2,
      rowIndex: i * 2,
      text: `Q${i}`,
    }));
    const win = computePromptRailWindow(prompts, 28, 11);
    expect(win.items.length).toBe(11);
    expect(win.clippedTop).toBe(true);
    expect(win.clippedBottom).toBe(true);
    expect(win.items[0].edgeFactor).toBeLessThan(1);
    expect(win.items[5].globalIndex).toBe(28);
    expect(win.items[5].edgeFactor).toBe(1);
  });

  test("token count, diff stats, composer restore", () => {
    expect(usagePercent(50_000, 200_000)).toBe(25);
    expect(compactTokenCount(12_340)).toBe("12.3k");
    expect(compactTokenCount(1_500_000)).toBe("1.5M");
    expect(diffLineStats("a\nb\nc", "a\nx\nc")).toEqual({ added: 1, removed: 1 });
    expect(consumeComposerRestore(2, 2, ["a"], []).restoreTexts).toBeNull();
    expect(consumeComposerRestore(2, 3, ["restored"], []).restoreTexts).toEqual(["restored"]);
  });
});

describe("ACP launch & fork wire contract", () => {
  test("builds ConversationClientLaunch payload with tagged Direct/Automation binding and AcpInitialPrompt", () => {
    const direct = buildAcpLaunchPayload({
      sessionId: "acp-1",
      cwd: "/repo",
      launchSpec: { command: "pi", env: {} },
      agentId: "pi",
      dirAcp: {
        history_session_id: "hist-1",
        config_values: [["model", "session-model"]],
        pending_prompt: "接手这里继续",
        pending_agent_preset: "reviewer",
      },
      initialConfig: [
        ["model", "agent-default"],
        ["thinking_level", "high"],
      ],
    });
    expect(direct.conversation_binding).toEqual({ type: "direct" });
    expect(direct.initial_prompt).toEqual({ text: "接手这里继续", images: [] });
    expect(direct.initial_config).toEqual([
      ["model", "session-model"],
      ["thinking_level", "high"],
    ]);

    const auto = buildAcpLaunchPayload({
      sessionId: "acp-2",
      cwd: "/repo",
      launchSpec: { command: "pi", env: {} },
      agentId: "pi",
      directSpec: { agent: "pi", cwd: "/repo", binding: { type: "automation", run_id: "run-9" } },
      dirAcp: { pending_prompt: "   " },
    });
    expect(auto.conversation_binding).toEqual({ type: "automation", run_id: "run-9" });
    expect(auto.initial_prompt).toBeNull();
  });

  test("computes AcpForkCut matching smelt-model fork_cut_after and fork_cut_before", () => {
    const entries = [
      { User: "go" },
      { Assistant: { text: "a1", thought: false } },
      { User: "[Request interrupted by user]" },
      { User: "go" },
      { Assistant: { text: "a2", thought: false } },
      { UserWithImages: { text: "go", images: [] } },
      { Assistant: { text: "a3", thought: false } },
    ];
    expect(buildForkCutAfter(entries, 1)).toEqual({ text: "go", occurrence: 1 });
    expect(buildForkCutAfter(entries, 4)).toEqual({ text: "go", occurrence: 2 });
    expect(buildForkCutAfter(entries, 6)).toBeNull();

    expect(buildForkCutBefore(entries, 0)).toEqual({ text: "go", occurrence: 0 });
    expect(buildForkCutBefore(entries, 2)).toBeNull();
    expect(buildForkCutBefore(entries, 3)).toEqual({ text: "go", occurrence: 1 });
    expect(buildForkCutBefore(entries, 5)).toEqual({ text: "go", occurrence: 2 });
  });

  test("recognizes thinking config via is_thought_level and smelt marker", () => {
    expect(
      isThinkingConfig({
        config_id: "custom_id",
        is_thought_level: true,
        options: [
          ["low", "Low"],
          ["high", "High"],
        ],
      }),
    ).toBe(true);
    expect(
      isThinkingConfig({
        config_id: "custom_id",
        smelt: "thinking_level",
        options: [
          ["low", "Low"],
          ["high", "High"],
        ],
      }),
    ).toBe(true);
  });

  test("mergeEnvelope preserves envelope-gated fields while updating always-present streaming fields", () => {
    const prev = {
      pending_permissions: [{ id: "p1", question: "allow?", tool_call_id: "t1", options: [] }],
      pending_elicitation: { id: "e1", message: "pick", fields: [], chosen: {}, text_values: {} },
      status_line: "Running tests",
      turn_started_at_ms: 12345,
      model: { config_id: "model", current_value: "sonnet", current_name: "Sonnet", options: [] },
      usage: [100, 1000],
      acp_session_id: null,
      provider_retry: null,
    };
    const incoming = {
      envelope_omitted: true,
      entries_offset: 1,
      entries_total: 2,
      snapshot_revision: 5,
      replaying_history: false,
      entries: [{ Assistant: { text: "stream", thought: false } }],
      phase: "thinking",
      end_reason: "",
      end_kind: "unknown",
      end_code: null,
      pending_permissions: [],
      pending_elicitation: null,
      status_line: null,
      turn_started_at_ms: null,
      acp_session_id: "acp-live-1",
      history_session_id: "hist-1",
      history_abandoned: false,
      supports_image: true,
      supports_compaction: true,
      supports_native_queue: true,
      supports_rewind: true,
      usage: [250, 1000],
      usage_cached_read: 40,
      usage_cost: 0.01,
      compacting: false,
      provider_retry: { attempt: 1, max_attempts: 3, delay_ms: 500, error: "429" },
      completed_unread: false,
      turn_outcome: null,
      should_persist: false,
      history_error: null,
      history_omitted: false,
    };
    const merged = mergeEnvelope(prev, incoming);
    expect(merged.envelope_omitted).toBe(false);
    expect(merged.pending_permissions).toEqual(prev.pending_permissions);
    expect(merged.pending_elicitation).toEqual(prev.pending_elicitation);
    expect(merged.status_line).toBe("Running tests");
    expect(merged.turn_started_at_ms).toBe(12345);
    expect(merged.model).toEqual(prev.model);
    expect(merged.usage).toEqual([250, 1000]);
    expect(merged.acp_session_id).toBe("acp-live-1");
    expect(merged.provider_retry.attempt).toBe(1);
    expect(merged.supports_rewind).toBe(true);
  });

  test("decodeFileUri strips file:// and percent-decodes paths with spaces and CJK", () => {
    expect(decodeFileUri("file:///tmp/my%20shot%20%E6%88%AA%E5%9B%BE.png")).toBe(
      "/tmp/my shot 截图.png",
    );
    expect(decodeFileUri("/plain/path.png")).toBe("/plain/path.png");
  });

  test("effortProgress, effortDotColor, and selectedConfigIndex map reasoning tiers consistently", () => {
    const cfg = {
      config_id: "reasoning_effort",
      current_name: "High",
      options: [
        ["low", "Low"],
        ["medium", "Medium"],
        ["high", "High"],
        ["xhigh", "Extra"],
      ],
    };
    const ix = selectedConfigIndex(cfg);
    expect(ix).toBe(2);
    expect(effortProgress(ix, cfg.options.length)).toBeCloseTo(2 / 3, 5);
    expect(effortDotColor(0)).toBe("#60a5fa");
    expect(effortDotColor(1 / 3)).toBe("#fb923c");
    expect(effortDotColor(2 / 3)).toBe("#c084fc");
    expect(effortDotColor(1)).toBe("#38bdf8");
  });

  test("composerSweepMode resolves off, sweep, hold, and still transitions", () => {
    expect(
      composerSweepMode({
        turnActive: false,
        awaitingUser: false,
        windowActive: true,
        reduceMotion: false,
      }),
    ).toBe("off");
    expect(
      composerSweepMode({
        turnActive: true,
        awaitingUser: false,
        windowActive: true,
        reduceMotion: false,
      }),
    ).toBe("sweep");
    expect(
      composerSweepMode({
        turnActive: true,
        awaitingUser: true,
        windowActive: false,
        reduceMotion: true,
      }),
    ).toBe("hold");
    expect(
      composerSweepMode({
        turnActive: false,
        awaitingUser: true,
        windowActive: true,
        reduceMotion: false,
      }),
    ).toBe("hold");
    expect(
      composerSweepMode({
        turnActive: true,
        awaitingUser: false,
        windowActive: false,
        reduceMotion: false,
      }),
    ).toBe("still");
    expect(
      composerSweepMode({
        turnActive: true,
        awaitingUser: false,
        windowActive: true,
        reduceMotion: true,
      }),
    ).toBe("still");
  });

  test("colorWithAlpha formats #rrggbb, #rgb, rgb(), rgba(), clamps alpha, and preserves unknown inputs", () => {
    expect(colorWithAlpha("#7698fd", 0.35)).toBe("#7698fd59");
    expect(colorWithAlpha("#abc", 1)).toBe("#aabbccff");
    expect(colorWithAlpha("rgb(118, 152, 253)", 0.35)).toBe("rgba(118, 152, 253, 0.35)");
    expect(colorWithAlpha("rgba(118, 152, 253, 0.9)", -0.5)).toBe("rgba(118, 152, 253, 0)");
    expect(colorWithAlpha("#7698fd", 2)).toBe("#7698fdff");
    expect(colorWithAlpha("transparent", 0.35)).toBe("transparent");
  });
});
