import { describe, expect, test } from "bun:test";
import {
  applyPendingConfigSelection,
  buildAcpLaunchPayload,
  buildCompletionCandidates,
  buildConversationLayout,
  buildDisplayRows,
  buildVisibleModels,
  collectEditFilesFromRange,
  colorWithAlpha,
  compactTokenCount,
  stopRingMode,
  showsEmptyWelcome,
  launchBringsContent,
  isTurnActive,
  computePromptRailWindow,
  consumeComposerRestore,
  detectCompletionTrigger,
  isStreamingMetaUnchanged,
  isThinkingConfig,
  mergeEnvelope,
  mergeSnapshotEntries,
  overlayModelState,
  reconcilePendingConfigValues,
  selectedConfigIndex,
  subsequenceScore,
  turnTimingsEqual,
  usagePercent,
} from "../model.js";

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

  test("overlapping snapshot truncates stale tail when total shrinks", () => {
    const s0 = { entries: [{ User: "2" }, { User: "3" }, { User: "4" }], loadedOffset: 2, entriesTotal: 5 };
    const s1 = mergeSnapshotEntries(s0, 0, 3, [{ User: "0" }, { User: "1" }, { User: "2" }], false);
    expect(s1.dirtyFrom).toBe(0);
    expect(s1.loadedOffset).toBe(0);
    expect(s1.entriesTotal).toBe(3);
    expect(s1.entries.map((entry) => entry.User)).toEqual(["0", "1", "2"]);
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

    // 宿主的 diffLineStats 在 bun 里没有；这里只验证按文件归并，行数由替身给。
    const lineStats = () => ({ added: 2, removed: 1 });
    const collapsed = buildDisplayRows(entries, layout, 0, new Set(), new Set(), lineStats);
    expect(collapsed.map((r) => r.kind)).toEqual(["entry", "turn_fold", "entry"]);

    const edits = collectEditFilesFromRange(entries, 0, entries.length, lineStats);
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

  test("mergeEnvelope keeps keys the host left out and takes every key the frame carries", () => {
    const prev = {
      pending_permissions: [{ id: "p1", question: "allow?", tool_call_id: "t1", options: [] }],
      status_line: "Running tests",
      turn_started_at_ms: 12345,
      model: { config_id: "model", current_value: "sonnet", current_name: "Sonnet", options: [] },
      usage: [100, 1000],
      acp_session_id: null,
      provider_retry: { attempt: 1, max_attempts: 3, delay_ms: 500, error: "429" },
    };
    // 宿主去掉了信封键：只剩每帧都带的字段。
    const incoming = {
      envelope_omitted: true,
      entries_offset: 1,
      entries_total: 2,
      snapshot_revision: 5,
      entries: [{ Assistant: { text: "stream", thought: false } }],
      phase: "thinking",
      acp_session_id: "acp-live-1",
      usage: [250, 1000],
      provider_retry: null,
    };
    const merged = mergeEnvelope(prev, incoming);
    expect(merged.envelope_omitted).toBe(false);
    expect(merged.pending_permissions).toEqual(prev.pending_permissions);
    expect(merged.status_line).toBe("Running tests");
    expect(merged.turn_started_at_ms).toBe(12345);
    expect(merged.model).toEqual(prev.model);
    expect(merged.usage).toEqual([250, 1000]);
    expect(merged.acp_session_id).toBe("acp-live-1");
    expect(merged.provider_retry).toBeNull();
    expect(mergeEnvelope(null, incoming)).toBe(incoming);
  });

  test("selectedConfigIndex finds the current tier", () => {
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
    expect(selectedConfigIndex(cfg)).toBe(2);
  });

  test("stopRingMode: spins while running, still without focus or motion, off when idle or waiting on you", () => {
    const mode = (o) =>
      stopRingMode({ turnActive: true, awaitingUser: false, windowActive: true, reduceMotion: false, ...o });
    expect(mode({})).toBe("spin");
    expect(mode({ turnActive: false })).toBe("off");
    expect(mode({ awaitingUser: true })).toBe("off");
    expect(mode({ awaitingUser: true, windowActive: false, reduceMotion: true })).toBe("off");
    expect(mode({ windowActive: false })).toBe("still");
    expect(mode({ reduceMotion: true })).toBe("still");
  });

  test("colorWithAlpha formats #rrggbb, #rgb, rgb(), rgba(), clamps alpha, and preserves unknown inputs", () => {
    expect(colorWithAlpha("#7698fd", 0.35)).toBe("#7698fd59");
    expect(colorWithAlpha("#abc", 1)).toBe("#aabbccff");
    expect(colorWithAlpha("rgb(118, 152, 253)", 0.35)).toBe("rgba(118, 152, 253, 0.35)");
    expect(colorWithAlpha("rgba(118, 152, 253, 0.9)", -0.5)).toBe("rgba(118, 152, 253, 0)");
    expect(colorWithAlpha("#7698fd", 2)).toBe("#7698fdff");
    expect(colorWithAlpha("transparent", 0.35)).toBe("transparent");
  });

  test("turnTimingsEqual compares turn timing lists structurally", () => {
    const t1 = [{ user_index: 0, started_at_ms: 100, ended_at_ms: 250 }];
    const t2 = [{ user_index: 0, started_at_ms: 100, ended_at_ms: 250 }];
    const t3 = [{ user_index: 0, started_at_ms: 100, ended_at_ms: 260 }];
    expect(turnTimingsEqual(t1, t2)).toBe(true);
    expect(turnTimingsEqual(t1, t3)).toBe(false);
    expect(turnTimingsEqual([], undefined)).toBe(true);
  });

  test("isStreamingMetaUnchanged detects pure text delta frames vs metadata changes", () => {
    const base = {
      phase: "responding",
      replaying_history: false,
      end_reason: "",
      end_kind: null,
      end_code: null,
      acp_session_id: "acp-1",
      history_session_id: null,
      history_abandoned: false,
      supports_image: true,
      supports_compaction: true,
      supports_native_queue: true,
      supports_rewind: false,
      compacting: false,
      completed_unread: false,
      turn_outcome: null,
      usage: { input_tokens: 10, output_tokens: 20 },
      usage_cached_read: 0,
      usage_cost: 0,
      provider_retry: null,
      history_error: null,
      history_omitted: false,
      turn_timings: [{ user_index: 0, started_at_ms: 100 }],
      entries: [{ Assistant: { text: "hello", thought: false } }],
    };
    const sameMeta = mergeEnvelope(base, {
      ...base,
      envelope_omitted: true,
      snapshot_revision: 42,
      usage: { input_tokens: 10, output_tokens: 20 },
      entries: [{ Assistant: { text: "hello world", thought: false } }],
    });
    expect(isStreamingMetaUnchanged(base, sameMeta)).toBe(true);

    const changedUsage = mergeEnvelope(base, {
      ...base,
      envelope_omitted: true,
      snapshot_revision: 43,
      usage: { input_tokens: 10, output_tokens: 25 },
      entries: [{ Assistant: { text: "hello world!", thought: false } }],
    });
    expect(isStreamingMetaUnchanged(base, changedUsage)).toBe(false);

    const changedRetry = mergeEnvelope(base, {
      ...base,
      envelope_omitted: true,
      provider_retry: { attempt: 1, delay_ms: 500 },
    });
    expect(isStreamingMetaUnchanged(base, changedRetry)).toBe(false);
  });
});

describe("isTurnActive", () => {
  test("starting the agent is not a running turn", () => {
    expect(isTurnActive("connecting")).toBe(false);
    expect(isTurnActive("idle")).toBe(false);
    expect(isTurnActive("dead")).toBe(false);
    for (const p of ["thinking", "executing_tool", "awaiting_approval", "waiting_for_user"]) {
      expect(isTurnActive(p)).toBe(true);
    }
  });
});

describe("launchBringsContent", () => {
  test("resume, fork and a first prompt bring content; a prefill does not", () => {
    expect(launchBringsContent({ dirAcp: null, start: null })).toBe(false);
    expect(launchBringsContent({ dirAcp: {}, start: { prefill: "hi" } })).toBe(false);
    expect(launchBringsContent({ dirAcp: { pending_prompt: "  " }, start: null })).toBe(false);
    expect(launchBringsContent({ dirAcp: { history_session_id: "h" }, start: null })).toBe(true);
    expect(launchBringsContent({ dirAcp: {}, start: { fork: { provider_session_id: "p" } } })).toBe(true);
    expect(launchBringsContent({ dirAcp: { pending_prompt: "go" }, start: null })).toBe(true);
  });
});

describe("showsEmptyWelcome", () => {
  const base = {
    embedded: false,
    phase: "connecting",
    replayingHistory: false,
    bringsContent: false,
    connectFailed: false,
    loadingOlder: false,
    entryCount: 0,
    loadedOffset: 0,
  };
  const shows = (o) => showsEmptyWelcome({ ...base, ...o });

  test("an empty launch is centered from the first frame through connecting to idle", () => {
    expect(shows({})).toBe(true);
    expect(shows({ phase: "idle" })).toBe(true);
  });

  test("a launch that brings content is not centered until it has connected and replayed", () => {
    expect(shows({ bringsContent: true })).toBe(false);
    expect(shows({ bringsContent: true, phase: "idle", replayingHistory: true })).toBe(false);
    expect(shows({ bringsContent: true, phase: "idle" })).toBe(true);
  });

  test("a running turn, an end, an error or any content leaves the center", () => {
    expect(shows({ phase: "thinking" })).toBe(false);
    expect(shows({ phase: "dead" })).toBe(false);
    expect(shows({ connectFailed: true })).toBe(false);
    expect(shows({ embedded: true })).toBe(false);
    expect(shows({ loadingOlder: true })).toBe(false);
    expect(shows({ phase: "idle", entryCount: 1 })).toBe(false);
    expect(shows({ phase: "idle", loadedOffset: 3 })).toBe(false);
  });
});
