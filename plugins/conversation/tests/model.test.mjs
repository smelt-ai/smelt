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
  computePromptRailScrollMetrics,
  computePromptRailWindow,
  extractTurnAnswerPreview,
  promptRailEdgeFactor,
  consumeComposerRestore,
  createBottomStickController,
  detectCompletionTrigger,
  entryWindowOf,
  isStreamingMetaUnchanged,
  isThinkingConfig,
  mergeEnvelope,
  overlayModelState,
  reconcilePendingConfigValues,
  sessionSkills,
  selectedConfigIndex,
  spliceEntries,
  subsequenceScore,
  transcriptWindowOf,
  turnTimingsEqual,
  usagePercent,
  RECONNECT_ATTEMPTS,
  RECONNECT_STABLE_AFTER_MS,
  reconnectAttempt,
} from "../model.js";

describe("entryWindowOf / spliceEntries", () => {
  test("快照只取位置和归属，缺的字段给默认值", () => {
    expect(entryWindowOf({ entries_offset: 3, entries_total: 5, entries: [1, 2], snapshot_revision: 7 })).toEqual({
      offset: 3,
      len: 2,
      total: 5,
      revision: 7,
      history_session_id: null,
      acp_session_id: null,
    });
  });

  test("手里前 keep 条 + 新来的 + 手里从 resume 起的，截到窗口条数", () => {
    const plan = (keep, take_next, resume, len) => ({ keep, take_next, resume, window: { len } });
    expect(spliceEntries(["a", "b", "c"], plan(2, true, 3, 4), ["C", "D"])).toEqual(["a", "b", "C", "D"]);
    expect(spliceEntries(["c", "d"], plan(0, true, 0, 4), ["a", "b"])).toEqual(["a", "b", "c", "d"]);
    expect(spliceEntries(["a", "b", "c", "d"], plan(1, true, 2, 4), ["B"])).toEqual(["a", "B", "c", "d"]);
    expect(spliceEntries(["a", "b", "c"], plan(3, false, 3, 2), ["x"])).toEqual(["a", "b"]);
  });

  test("不改手里的数组", () => {
    const current = ["a", "b"];
    spliceEntries(current, { keep: 1, take_next: true, resume: 2, window: { len: 2 } }, ["B"]);
    expect(current).toEqual(["a", "b"]);
  });

  test("子会话的一页拿读到的字节数当修订号，老运行时没有就记 0", () => {
    expect(transcriptWindowOf({ entries: [1, 2], offset: 3, total: 5, cursor: 900 })).toEqual({
      offset: 3,
      len: 2,
      total: 5,
      revision: 900,
      history_session_id: null,
      acp_session_id: null,
    });
    expect(transcriptWindowOf({ entries: [1], offset: 0, total: 1 }).revision).toBe(0);
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
    expect(win.start).toBe(9);
    expect(win.clippedTop).toBe(true);
    expect(win.clippedBottom).toBe(true);
    expect(win.items[0].edgeFactor).toBeLessThan(1);
    expect(win.items[5].globalIndex).toBe(28);
    expect(win.items[5].edgeFactor).toBe(1);

    // 手动滚轮滑窗覆盖起点：可滑到顶部/底部并自动钳位。
    const top = computePromptRailWindow(prompts, 28, 11, -5);
    expect(top.start).toBe(0);
    expect(top.clippedTop).toBe(false);
    expect(top.clippedBottom).toBe(true);
    expect(top.items[0].globalIndex).toBe(0);

    const bottom = computePromptRailWindow(prompts, 28, 11, 99);
    expect(bottom.start).toBe(19);
    expect(bottom.clippedTop).toBe(true);
    expect(bottom.clippedBottom).toBe(false);
    expect(bottom.items[10].globalIndex).toBe(58);

    // 连续滚动模式：计算滑块区间、上下可滚状态与边缘缩放因子。
    const mid = computePromptRailScrollMetrics({
      total: 30,
      scrollY: 100,
      viewportHeight: 142,
      pitch: 12,
      tickHeight: 10,
      padY: 6,
    });
    expect(mid.canScroll).toBe(true);
    expect(mid.canScrollUp).toBe(true);
    expect(mid.canScrollDown).toBe(true);
    expect(mid.thumbHeight).toBeGreaterThanOrEqual(16);
    expect(mid.thumbTop).toBeGreaterThan(6);

    const edgeTop = promptRailEdgeFactor({
      index: 8,
      scrollY: 100,
      viewportHeight: 142,
      pitch: 12,
      tickHeight: 10,
      padY: 6,
      fadeZone: 24,
      canScrollUp: true,
      canScrollDown: true,
    });
    const middle = promptRailEdgeFactor({
      index: 14,
      scrollY: 100,
      viewportHeight: 142,
      pitch: 12,
      tickHeight: 10,
      padY: 6,
      fadeZone: 24,
      canScrollUp: true,
      canScrollDown: true,
    });
    expect(edgeTop).toBeLessThan(1);
    expect(middle).toBe(1);

    // 提取回合回答开头：跳过思考段与工具，取最终回答并清理 Markdown 标记。
    const convEntries = [
      { User: "第一问" },
      { Assistant: { text: "先想想", thought: true } },
      { ToolCall: { title: "read", kind: "read", status: "completed" } },
      { Assistant: { text: "## 结论\n\n这是**最终回答**内容。", thought: false } },
      { User: "第二问" },
      { Assistant: { text: "下一回合", thought: false } },
    ];
    expect(extractTurnAnswerPreview(convEntries, 0)).toBe("结论 这是最终回答内容。");
    expect(extractTurnAnswerPreview(convEntries, 4)).toBe("下一回合");
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

describe("sessionSkills", () => {
  test("keeps only the skill commands the session reported", () => {
    expect(
      sessionSkills([
        ["compact", "压缩上下文"],
        ["skill:deploy", "把构建推上去"],
      ]),
    ).toEqual([{ command: "skill:deploy", name: "deploy", description: "把构建推上去" }]);
    expect(sessionSkills([["review", "Review"]])).toEqual([]);
    expect(sessionSkills(undefined)).toEqual([]);
  });
});

describe("createBottomStickController", () => {
  function makeHarness(windowMs = 3000) {
    const scrollCalls = [];
    const atLatestEvents = [];
    const frames = new Map();
    const timers = new Map();
    let nextFrame = 1;
    let nextTimer = 1;
    const ctrl = createBottomStickController({
      scrollToEnd: (opts) => scrollCalls.push(opts),
      setAtLatest: (v) => atLatestEvents.push(v),
      requestFrame: (fn) => {
        const id = nextFrame++;
        frames.set(id, fn);
        return id;
      },
      cancelFrame: (id) => frames.delete(id),
      setTimer: (fn, ms) => {
        const id = nextTimer++;
        timers.set(id, { fn, ms });
        return id;
      },
      clearTimer: (id) => timers.delete(id),
      windowMs,
    });
    const flushFrames = () => {
      const pending = [...frames.entries()];
      frames.clear();
      for (const [, fn] of pending) fn();
    };
    const flushTimers = () => {
      const pending = [...timers.entries()];
      timers.clear();
      for (const [, t] of pending) t.fn();
    };
    return { ctrl, scrollCalls, atLatestEvents, frames, timers, flushFrames, flushTimers };
  }

  test("scrolls immediately non-animated on send and keeps re-sticking across multiple appended rows", () => {
    const h = makeHarness();
    h.ctrl.trigger();
    expect(h.scrollCalls).toEqual([{ animated: false }]);
    expect(h.atLatestEvents).toEqual([true]);
    expect(h.ctrl.isActive()).toBe(true);

    // Post-layout follow-up after composer shrinks / initial jump settles.
    h.flushFrames();
    expect(h.scrollCalls).toEqual([{ animated: false }, { animated: false }]);

    // Row 1 (user bubble) and row 2 (tool card) arrive in the same frame -> coalesced to 1 rAF.
    h.ctrl.onContentGrew();
    h.ctrl.onContentGrew();
    expect(h.frames.size).toBe(1);
    h.flushFrames();
    expect(h.scrollCalls).toHaveLength(3);

    // Row 3 (assistant streaming text) arrives -> still sticks (does not drop after row 1).
    h.ctrl.onContentGrew();
    h.flushFrames();
    expect(h.scrollCalls).toHaveLength(4);
  });

  test("suppresses stale nearEnd=false flicker during sticky window and re-sticks", () => {
    const h = makeHarness();
    h.ctrl.trigger();
    h.flushFrames();
    h.scrollCalls.length = 0;
    h.atLatestEvents.length = 0;

    // New row pushed content down before rAF ran -> onViewableItemsChange reports nearEnd=false.
    h.ctrl.onViewableAtLatest(false);
    expect(h.atLatestEvents).toEqual([]);
    expect(h.frames.size).toBe(1);
    h.flushFrames();
    expect(h.scrollCalls).toEqual([{ animated: false }]);
    expect(h.atLatestEvents).toEqual([true]);
  });

  test("immediately yields control when user scrolls manually or switches sessions", () => {
    const h = makeHarness();
    h.ctrl.trigger();
    expect(h.frames.size).toBe(1);

    h.ctrl.cancel();
    expect(h.ctrl.isActive()).toBe(false);
    expect(h.frames.size).toBe(0);
    expect(h.timers.size).toBe(0);

    h.ctrl.onContentGrew();
    expect(h.frames.size).toBe(0);

    h.ctrl.onViewableAtLatest(false);
    expect(h.atLatestEvents[h.atLatestEvents.length - 1]).toBe(false);
  });

  test("deactivates cleanly after window expires", () => {
    const h = makeHarness(2500);
    h.ctrl.trigger();
    h.flushFrames();
    h.flushTimers();
    expect(h.ctrl.isActive()).toBe(false);

    h.ctrl.onViewableAtLatest(false);
    expect(h.atLatestEvents[h.atLatestEvents.length - 1]).toBe(false);
  });
});

describe("reconnectAttempt", () => {
  const transport = { phase: "dead", end_kind: "transport_disconnected" };

  test("只是连接断了就重连，次数照上次累计", () => {
    expect(reconnectAttempt(transport, 0, null, 0)).toBe(0);
    expect(reconnectAttempt(transport, 2, null, 0)).toBe(2);
    expect(reconnectAttempt({ phase: "idle" }, 1, null, 0)).toBe(1);
    expect(reconnectAttempt(null, 0, null, 0)).toBe(0);
  });

  test("会话真结束了不重连", () => {
    expect(reconnectAttempt({ phase: "dead", end_kind: "provider_failed" }, 0, null, 0)).toBeNull();
    expect(reconnectAttempt({ phase: "dead", end_kind: "session_terminated" }, 0, null, 0)).toBeNull();
  });

  test("连续失败用完就停，连上撑过稳定期再断才回满", () => {
    expect(reconnectAttempt(transport, RECONNECT_ATTEMPTS, null, 0)).toBeNull();
    expect(reconnectAttempt(transport, RECONNECT_ATTEMPTS, 0, RECONNECT_STABLE_AFTER_MS - 1)).toBeNull();
    expect(reconnectAttempt(transport, RECONNECT_ATTEMPTS, 0, RECONNECT_STABLE_AFTER_MS)).toBe(0);
  });
});
