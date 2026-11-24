// Smelt 对话纯逻辑模块（不 import NSH，直接用 `bun test` 覆盖）。
// 严格对齐原 `crates/plugins/conversation/src/view.rs`、`model_popover.rs`、`prompt_rail.rs`、`edit_diff.rs`
// 与 `smelt-ui/src/acp_completion.rs` 的全部核心归约与排序规则。

export const MAX_COMPLETION_ITEMS = 12;
export const PROMPT_RAIL_MAX_TICKS = 11;
export const PROMPT_RAIL_EDGE_FADE = 2;

const INTERRUPT_PREFIXES = [
  "[Request interrupted by user",
  "[User interrupted",
  "Request interrupted by user",
];

export function isInterruptMarker(text) {
  if (typeof text !== "string") return false;
  const trimmed = text.trim();
  return INTERRUPT_PREFIXES.some((p) => trimmed.startsWith(p));
}

export function isNewConversationCommand(text) {
  return typeof text === "string" && text.trim().toLowerCase() === "/clear";
}

export function isPhaseActive(phase) {
  return (
    phase === "connecting" ||
    phase === "thinking" ||
    phase === "executing_tool" ||
    phase === "awaiting_approval" ||
    phase === "waiting_for_user"
  );
}

export function isTaskCompletionToolTitle(title) {
  if (typeof title !== "string") return false;
  const low = title.trim().toLowerCase();
  return low === "task_complete" || low === "attempt_completion" || low === "complete_task";
}

export function stripCodeFence(text) {
  if (typeof text !== "string") return "";
  const trimmed = text.trim();
  if (!trimmed.startsWith("```")) return trimmed;
  const firstNl = trimmed.indexOf("\n");
  if (firstNl < 0) return "";
  let body = trimmed.slice(firstNl + 1);
  if (body.endsWith("```")) {
    body = body.slice(0, -3);
  }
  return body.trim();
}

export function entryKind(entry) {
  if (!entry || typeof entry !== "object") return "unknown";
  if (typeof entry === "string") return entry;
  const keys = Object.keys(entry);
  return keys.length > 0 ? keys[0] : "unknown";
}

export function isUserEntry(entry) {
  if (!entry || typeof entry !== "object") return false;
  return "User" in entry || "UserWithImages" in entry;
}

export function isDividerEntry(entry) {
  if (!entry) return false;
  if (typeof entry === "string") return entry === "TurnDivider" || entry === "Compacted";
  return (
    "Divider" in entry ||
    "NewSession" in entry ||
    "TurnDivider" in entry ||
    "Compacted" in entry ||
    "Compaction" in entry
  );
}

export function isCompletionEntry(entry) {
  if (!entry || typeof entry !== "object" || !("ToolCall" in entry)) return false;
  return isTaskCompletionToolTitle(entry.ToolCall?.title);
}

export function isEventToolEntry(entry) {
  if (!entry || typeof entry !== "object" || !("ToolCall" in entry)) return false;
  const tc = entry.ToolCall;
  return Boolean(tc?.peer || tc?.asked);
}

export function drawsNothing(entry) {
  if (!entry) return true;
  if (typeof entry === "object" && "Assistant" in entry) {
    return !(entry.Assistant?.text ?? "").trim();
  }
  return false;
}

export function isActiveAgentTool(tc) {
  if (!tc) return false;
  if (tc.kind !== "collaborate" && !(Array.isArray(tc.subagents) && tc.subagents.length > 0)) {
    return false;
  }
  if (tc.status === "in_progress" || tc.status === "pending") return true;
  if (Array.isArray(tc.subagents)) {
    return tc.subagents.some((s) => s.status === "running" || s.status === "pending");
  }
  return false;
}

export function foldsIntoTurnSummary(entry) {
  if (!entry || typeof entry !== "object") return false;
  if ("ToolCall" in entry) return !isCompletionEntry(entry);
  if ("Assistant" in entry) return Boolean(entry.Assistant?.thought);
  return false;
}

export function entryUserText(entry) {
  if (!entry || typeof entry !== "object") return "";
  if ("User" in entry) return String(entry.User ?? "");
  if ("UserWithImages" in entry) return String(entry.UserWithImages?.text ?? "");
  return "";
}

export function latestUserMessageIndex(entries) {
  for (let i = entries.length - 1; i >= 0; i--) {
    if (isUserEntry(entries[i]) && !isInterruptMarker(entryUserText(entries[i]))) {
      return i;
    }
  }
  return null;
}

/**
 * 合并服务端快照条目窗口（严格对齐 `view.rs:merge_snapshot_entries`）。
 * 返回 `{ entries, loadedOffset, entriesTotal, dirtyFrom }`，其中 `dirtyFrom == null` 表示无变更。
 */
export function mergeSnapshotEntries(state, incomingOffset, incomingTotalRaw, incoming, initial) {
  const incomingList = Array.isArray(incoming) ? incoming : [];
  const incomingEnd = incomingOffset + incomingList.length;
  const incomingTotal = Math.max(incomingTotalRaw ?? 0, incomingEnd);

  if (initial) {
    return {
      entries: incomingList.slice(),
      loadedOffset: incomingOffset,
      entriesTotal: incomingTotal,
      dirtyFrom: 0,
    };
  }

  const prevEntries = state.entries;
  const loadedOffset = state.loadedOffset;
  const loadedEnd = loadedOffset + prevEntries.length;

  if (incomingTotal < loadedOffset) {
    if (incomingList.length === 0 && incomingTotal > 0 && prevEntries.length > 0) {
      return { ...state, dirtyFrom: null };
    }
    return {
      entries: incomingList.slice(),
      loadedOffset: incomingOffset,
      entriesTotal: incomingTotal,
      dirtyFrom: 0,
    };
  }

  if (incomingOffset === loadedEnd && incomingList.length === 0) {
    if (state.entriesTotal === incomingTotal) {
      return { ...state, dirtyFrom: null };
    }
    return { ...state, entriesTotal: incomingTotal, dirtyFrom: null };
  }

  if (incomingOffset >= loadedOffset && incomingOffset <= loadedEnd) {
    const localOffset = incomingOffset - loadedOffset;
    const next = prevEntries.slice(0, localOffset).concat(incomingList);
    return {
      entries: next,
      loadedOffset,
      entriesTotal: incomingTotal,
      dirtyFrom: localOffset,
    };
  }

  if (incomingOffset < loadedOffset && incomingEnd >= loadedOffset) {
    const overlap = incomingEnd - loadedOffset;
    const suffixStart = Math.min(overlap, prevEntries.length);
    const next = incomingList.concat(prevEntries.slice(suffixStart));
    return {
      entries: next,
      loadedOffset: incomingOffset,
      entriesTotal: incomingTotal,
      dirtyFrom: 0,
    };
  }

  if (incomingOffset > loadedEnd) {
    if (incomingList.length === 0) {
      return { ...state, entriesTotal: incomingTotal, dirtyFrom: null };
    }
    return {
      entries: incomingList.slice(),
      loadedOffset: incomingOffset,
      entriesTotal: incomingTotal,
      dirtyFrom: 0,
    };
  }

  return { ...state, entriesTotal: incomingTotal, dirtyFrom: null };
}

export function tallyTools(entries, indices) {
  const tally = {
    total: 0,
    reads: 0,
    searches: 0,
    edits: 0,
    commands: 0,
    web: 0,
    subagents: 0,
    other: 0,
    failed: 0,
    running: 0,
  };
  for (const ix of indices) {
    const entry = entries[ix];
    if (!entry || typeof entry !== "object" || !("ToolCall" in entry)) continue;
    const tc = entry.ToolCall;
    if (!tc || isTaskCompletionToolTitle(tc.title)) continue;
    tally.total += 1;
    if (tc.status === "failed") tally.failed += 1;
    if (tc.status === "in_progress" || tc.status === "pending") tally.running += 1;
    switch (tc.kind) {
      case "read":
        tally.reads += 1;
        break;
      case "search":
        tally.searches += 1;
        break;
      case "edit":
      case "delete":
      case "move":
        tally.edits += 1;
        break;
      case "execute":
        tally.commands += 1;
        break;
      case "fetch":
        tally.web += 1;
        break;
      case "collaborate":
        tally.subagents += 1;
        break;
      default:
        tally.other += 1;
        break;
    }
  }
  return tally;
}

/**
 * 将 `entries` 归约为每条的渲染布局元数据（对齐 `view.rs:build_conversation_layout_with_timings`）。
 */
export function buildConversationLayout(
  entries,
  currentTurnActive,
  entriesOffset = 0,
  turnTimings = [],
) {
  const layout = entries.map(() => ({
    finalAnswer: false,
    showSettledThoughtElapsed: false,
    turnFold: null,
    processGroup: null,
  }));

  let start = 0;
  while (start < entries.length) {
    if (isUserEntry(entries[start]) || isDividerEntry(entries[start])) {
      start += 1;
      continue;
    }
    let end = entries.length;
    for (let i = start; i < entries.length; i++) {
      if (isUserEntry(entries[i]) || isDividerEntry(entries[i])) {
        end = i;
        break;
      }
    }
    const closed = end < entries.length || !currentTurnActive;

    let finalIx = null;
    if (closed) {
      for (let i = end - 1; i >= start; i--) {
        const e = entries[i];
        const isAnswer =
          e &&
          typeof e === "object" &&
          "Assistant" in e &&
          !e.Assistant?.thought &&
          Boolean((e.Assistant?.text ?? "").trim());
        if (isAnswer || isCompletionEntry(e)) {
          finalIx = i;
          break;
        }
      }
    }
    if (finalIx !== null) {
      layout[finalIx].finalAnswer = true;
    }

    let turnElapsedMs = null;
    if (closed && Array.isArray(turnTimings) && turnTimings.length > 0) {
      let userIndex = null;
      for (let i = start - 1; i >= 0; i--) {
        if (isUserEntry(entries[i])) {
          userIndex = i;
          break;
        }
      }
      if (userIndex !== null) {
        const globalIx = entriesOffset + userIndex;
        const timing = turnTimings.find((item) => item?.user_index === globalIx);
        const endedAt = timing?.ended_at_ms ?? timing?.finished_at_ms;
        if (timing && timing.started_at_ms != null && endedAt != null) {
          turnElapsedMs = Math.max(0, endedAt - timing.started_at_ms);
        }
      }
    }

    const processGroups = [];
    let currentIndices = [];
    for (let ix = start; ix < end; ix++) {
      const e = entries[ix];
      if (drawsNothing(e)) continue;
      const breaksGroup =
        (e && typeof e === "object" && "Assistant" in e && !e.Assistant?.thought) ||
        isCompletionEntry(e) ||
        isEventToolEntry(e) ||
        (e && typeof e === "object" && ("TaskNote" in e || "TurnFailed" in e));
      if (breaksGroup) {
        if (currentIndices.length > 0) {
          processGroups.push(currentIndices);
          currentIndices = [];
        }
      } else {
        currentIndices.push(ix);
      }
    }
    if (currentIndices.length > 0) {
      processGroups.push(currentIndices);
    }
    const toolGroups = processGroups.filter((grp) =>
      grp.some((ix) => entries[ix] && typeof entries[ix] === "object" && "ToolCall" in entries[ix]),
    );

    if (toolGroups.length === 0) {
      for (let ix = start; ix < end; ix++) {
        const e = entries[ix];
        if (e && typeof e === "object" && "Assistant" in e && e.Assistant?.thought) {
          layout[ix].showSettledThoughtElapsed = true;
        }
      }
    }

    let lastVisible = null;
    for (let ix = end - 1; ix >= start; ix--) {
      if (!drawsNothing(entries[ix])) {
        lastVisible = ix;
        break;
      }
    }
    const lastGroup = toolGroups.length > 0 ? toolGroups[toolGroups.length - 1] : null;
    const trailingStandaloneThought =
      !closed &&
      lastVisible !== null &&
      entries[lastVisible] &&
      typeof entries[lastVisible] === "object" &&
      "Assistant" in entries[lastVisible] &&
      Boolean(entries[lastVisible].Assistant?.thought) &&
      (!lastGroup || !lastGroup.includes(lastVisible));

    let soleSegment = null;
    let turnFold = null;
    if (finalIx !== null) {
      let agentRunning = false;
      let hasFoldable = false;
      for (let ix = start; ix < finalIx; ix++) {
        const e = entries[ix];
        if (e && typeof e === "object" && "ToolCall" in e && isActiveAgentTool(e.ToolCall)) {
          agentRunning = true;
        }
        if (foldsIntoTurnSummary(e)) hasFoldable = true;
      }
      if (!agentRunning && hasFoldable) {
        const beforeAnswer = toolGroups.filter((g) => g[0] < finalIx);
        let isSole = false;
        if (beforeAnswer.length === 1) {
          const g = beforeAnswer[0];
          const gFirst = g[0];
          const gLast = g[g.length - 1];
          isSole = true;
          for (let ix = start; ix < finalIx; ix++) {
            if (!drawsNothing(entries[ix]) && (ix < gFirst || ix > gLast)) {
              isSole = false;
              break;
            }
          }
          if (isSole) soleSegment = gFirst;
        }
        if (!isSole) {
          let first = start;
          for (let ix = start; ix < finalIx; ix++) {
            if (!drawsNothing(entries[ix])) {
              first = ix;
              break;
            }
          }
          const spanIndices = [];
          for (let ix = start; ix < finalIx; ix++) spanIndices.push(ix);
          turnFold = {
            first,
            end: finalIx,
            elapsedMs: turnElapsedMs,
            tally: tallyTools(entries, spanIndices),
          };
        }
      }
    }

    if (turnFold) {
      for (let ix = turnFold.first; ix < turnFold.end; ix++) {
        layout[ix].turnFold = turnFold;
      }
    }

    const elapsedOwner = turnFold
      ? null
      : soleSegment !== null
        ? soleSegment
        : trailingStandaloneThought
          ? null
          : lastGroup
            ? lastGroup[0]
            : null;

    for (const grp of toolGroups) {
      const first = grp[0];
      const groupEnd = grp[grp.length - 1] + 1;
      const showElapsed = elapsedOwner === first;
      const info = {
        first,
        end: groupEnd,
        active: !closed,
        showElapsed,
        elapsedMs: showElapsed ? turnElapsedMs : null,
        tally: tallyTools(entries, grp),
      };
      for (const ix of grp) {
        layout[ix].processGroup = info;
      }
    }

    start = end;
  }

  return layout;
}

/**
 * 将 `entries + layout + expandedSets` 投影成虚拟列表可直接渲染的扁平行列表。
 * 折叠的 `turnFold` 或 `processGroup` 压成单行，展开时露出内部行，大幅减少列表项数。
 */
export function buildDisplayRows(
  entries,
  layout,
  entriesOffset,
  expandedTurnFolds,
  expandedProcessGroups,
) {
  const rows = [];
  let i = 0;
  while (i < entries.length) {
    const entry = entries[i];
    const pres = layout[i];
    if (drawsNothing(entry)) {
      i += 1;
      continue;
    }

    const fold = pres?.turnFold;
    if (fold && i === fold.first) {
      const foldKey = `tf:${entriesOffset + fold.first}`;
      const expanded = expandedTurnFolds.has(foldKey);
      rows.push({
        key: foldKey,
        kind: "turn_fold",
        entryIndex: i,
        globalIndex: entriesOffset + i,
        fold,
        expanded,
      });
      if (!expanded) {
        i = fold.end;
        continue;
      }
    }

    const group = pres?.processGroup;
    if (group && i === group.first) {
      const groupKey = `pg:${entriesOffset + group.first}`;
      const expanded = expandedProcessGroups.has(groupKey);
      const editFiles = collectEditFilesFromRange(entries, group.first, group.end);
      rows.push({
        key: groupKey,
        kind: "process_group",
        entryIndex: i,
        globalIndex: entriesOffset + i,
        group,
        editFiles,
        expanded,
      });
      if (!expanded) {
        i = group.end;
        continue;
      }
    }

    rows.push({
      key: `e:${entriesOffset + i}`,
      kind: "entry",
      entryIndex: i,
      globalIndex: entriesOffset + i,
      entry,
      presentation: pres,
      inProcess: Boolean(group),
    });
    i += 1;
  }
  return rows;
}

// ---- @ / 补全（对齐 smelt-ui/src/acp_completion.rs） ----

export function detectCompletionTrigger(beforeCursor) {
  if (typeof beforeCursor !== "string" || beforeCursor.length === 0) return null;
  for (let i = beforeCursor.length - 1; i >= 0; i--) {
    const ch = beforeCursor[i];
    if (/\s/.test(ch)) break;
    if (ch !== "@" && ch !== "/") continue;
    if (i === 0 || /\s/.test(beforeCursor[i - 1])) {
      return {
        kind: ch === "@" ? "at" : "slash",
        start: i,
        needle: beforeCursor.slice(i + 1).toLowerCase(),
      };
    }
  }
  return null;
}

function meaningfulHint(value) {
  const trimmed = String(value ?? "").trim();
  if (!trimmed || /^[.…]+$/.test(trimmed)) return "";
  return trimmed;
}

function matchRank(value, needle) {
  const low = value.toLowerCase();
  const pos = needle ? low.indexOf(needle) : 0;
  let quality = 2;
  if (!needle || low.startsWith(needle)) {
    quality = 0;
  } else if (low.split(/[\/\-_.]/).some((part) => part.startsWith(needle))) {
    quality = 1;
  }
  return [quality, pos < 0 ? 1e9 : pos, low];
}

function compareTuples(a, b) {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    if (a[i] < b[i]) return -1;
    if (a[i] > b[i]) return 1;
  }
  return 0;
}

function fileRank(path, needle) {
  const hidden = path.split("/").some((p) => p.startsWith(".")) ? 1 : 0;
  const depth = (path.match(/\//g) ?? []).length;
  const trimmed = path.endsWith("/") ? path.slice(0, -1) : path;
  const slash = trimmed.lastIndexOf("/");
  const basename = slash >= 0 ? trimmed.slice(slash + 1) : trimmed;
  const [quality, pos] = matchRank(basename, needle);
  const fileKind = path.endsWith("/") ? 0 : 1;
  return [quality, hidden, pos, fileKind, depth, path.toLowerCase()];
}

export function buildCompletionCandidates(trigger, files, commands, builtinDescriptions = {}) {
  if (!trigger) return [];
  const needle = trigger.needle ?? "";
  if (trigger.kind === "slash") {
    const builtins = [
      ["clear", builtinDescriptions.clear ?? "Start a new conversation"],
      ["compact", builtinDescriptions.compact ?? "Compact conversation context"],
      ["handoff", builtinDescriptions.handoff ?? "Hand off to another agent"],
      ["shake", builtinDescriptions.shake ?? "Shake buddy"],
    ];
    const list = [];
    const seen = new Set();
    for (const item of commands ?? []) {
      const name = Array.isArray(item) ? item[0] : item?.name;
      const desc = Array.isArray(item) ? item[1] : item?.description;
      if (!name || name.toLowerCase() === "clear") continue;
      seen.add(name.toLowerCase());
      list.push([name, desc ?? ""]);
    }
    for (const [name, desc] of builtins) {
      if (!seen.has(name.toLowerCase())) {
        seen.add(name.toLowerCase());
        list.push([name, desc]);
      }
    }
    return list
      .filter(([name]) => !needle || name.toLowerCase().includes(needle))
      .sort((a, b) => compareTuples(matchRank(a[0], needle), matchRank(b[0], needle)))
      .slice(0, MAX_COMPLETION_ITEMS)
      .map(([name, desc]) => ({
        label: `/${name}`,
        insert: `/${name} `,
        hint: meaningfulHint(desc),
      }));
  }

  return (files ?? [])
    .filter((p) => !needle || p.toLowerCase().includes(needle))
    .slice()
    .sort((a, b) => compareTuples(fileRank(a, needle), fileRank(b, needle)))
    .slice(0, MAX_COMPLETION_ITEMS)
    .map((p) => ({
      label: `@${p}`,
      insert: p.endsWith("/") ? `@${p}` : `@${p} `,
      hint: "",
    }));
}

// ---- 模型搜索与配置叠加（对齐 model_popover.rs & view.rs） ----

export function subsequenceScore(haystack, query) {
  const q = String(query ?? "").trim().toLowerCase();
  if (!q) return 0;
  const chars = Array.from(String(haystack ?? ""));
  const folded = chars.map((c) => c.toLowerCase());
  const qChars = Array.from(q);
  let best = null;
  for (let start = 0; start < folded.length; start++) {
    if (folded[start] !== qChars[0]) continue;
    let total = 0;
    let prev = -1;
    let matched = 0;
    for (let i = start; i < folded.length && matched < qChars.length; i++) {
      if (folded[i] !== qChars[matched]) continue;
      let points = 10;
      if (prev >= 0 && prev + 1 === i) points += 12;
      const prevCh = i > 0 ? chars[i - 1] : null;
      const isWordStart =
        prevCh === null ||
        /[\s\-_/.:()]/.test(prevCh) ||
        (prevCh === prevCh.toLowerCase() &&
          prevCh !== prevCh.toUpperCase() &&
          chars[i] === chars[i].toUpperCase() &&
          chars[i] !== chars[i].toLowerCase());
      if (isWordStart) points += 8;
      const skipped = prev < 0 ? i : i - prev - 1;
      total += points - skipped;
      prev = i;
      matched += 1;
    }
    if (matched === qChars.length && (best === null || total > best)) {
      best = total;
    }
  }
  return best;
}

export function modelLabelWithProvider(value, name) {
  const slash = String(value ?? "").indexOf("/");
  if (slash > 0 && slash < String(value).length - 1) {
    const provider = String(value).slice(0, slash);
    return `${provider} · ${name}`;
  }
  return String(name ?? "");
}

export function composerModelLabel(model, fallbackShort) {
  if (!model) return fallbackShort;
  if (model.current_name && model.current_name.trim()) return model.current_name;
  if (model.current_value && model.current_value.trim()) return model.current_value;
  return fallbackShort;
}

export function selectedProviderGroup(model) {
  const groups = model?.provider_groups ?? [];
  if (groups.length === 0) return null;
  return (
    groups.find((g) => (g.options ?? []).some(([v]) => v === model.current_value)) ??
    groups.find((g) => (g.options ?? []).some(([, n]) => n === model.current_name)) ??
    groups[0]
  );
}

export function buildVisibleModels(model, activeProviderId, query) {
  if (!model) return [];
  const trimmed = String(query ?? "").trim();
  const groups = model.provider_groups ?? [];
  if (groups.length > 1) {
    if (!trimmed) {
      const group =
        (activeProviderId ? groups.find((g) => g.id === activeProviderId) : null) ??
        selectedProviderGroup(model) ??
        groups[0];
      if (!group) return [];
      return (group.options ?? []).map(([value, label]) => ({
        value,
        label,
        providerTag: null,
      }));
    }
    const scored = [];
    let order = 0;
    for (const group of groups) {
      for (const [value, label] of group.options ?? []) {
        const s1 = subsequenceScore(label, trimmed);
        const s2Raw = subsequenceScore(`${group.name} ${label}`, trimmed);
        const s2 = s2Raw !== null ? Math.trunc(s2Raw / 2) : null;
        const points = s1 !== null && s2 !== null ? Math.max(s1, s2) : (s1 ?? s2);
        if (points !== null) {
          scored.push({ points, order, value, label, providerTag: group.name });
        }
        order += 1;
      }
    }
    scored.sort((a, b) => b.points - a.points || a.order - b.order);
    return scored.map(({ value, label, providerTag }) => ({ value, label, providerTag }));
  }

  const options = groups.length === 1 ? (groups[0].options ?? []) : (model.options ?? []);
  const grouped = groups.length > 0;
  const scored = [];
  options.forEach(([value, name], ix) => {
    const label = grouped ? name : modelLabelWithProvider(value, name);
    const s1 = subsequenceScore(label, trimmed);
    const s2Raw = subsequenceScore(value, trimmed);
    const s2 = s2Raw !== null ? Math.trunc(s2Raw / 2) : null;
    const points = s1 !== null && s2 !== null ? Math.max(s1, s2) : (s1 ?? s2);
    if (points !== null) {
      scored.push({ points, ix, value, label, providerTag: null });
    }
  });
  scored.sort((a, b) => b.points - a.points || a.ix - b.ix);
  return scored.map(({ value, label, providerTag }) => ({ value, label, providerTag }));
}

export function isThinkingConfig(config) {
  if (!config || config.boolean != null || (config.options?.length ?? 0) < 2) return false;
  const id = String(config.config_id ?? "").toLowerCase();
  return (
    Boolean(config.is_thought_level) ||
    config.smelt === "thinking_level" ||
    id === "thinking_level" ||
    id === "reasoning_effort" ||
    id === "effort"
  );
}

export function buildInitialPrompt(pendingPrompt) {
  if (typeof pendingPrompt !== "string" || pendingPrompt.trim().length === 0) {
    return null;
  }
  return { text: pendingPrompt, images: [] };
}

export function buildAcpLaunchPayload({
  sessionId,
  cwd,
  launchSpec,
  agentId,
  dirAcp = null,
  directSpec = null,
  start = null,
  initialConfig = [],
}) {
  const cfgMap = new Map();
  for (const [k, v] of initialConfig ?? []) cfgMap.set(k, v);
  for (const [k, v] of dirAcp?.config_values ?? []) cfgMap.set(k, v);

  return {
    id: sessionId,
    cwd: cwd ?? null,
    launch: launchSpec,
    engine_kind: agentId,
    ephemeral_env: {},
    resume_id: dirAcp?.history_session_id ?? null,
    fork_id: start?.fork?.provider_session_id ?? null,
    fork_cut: start?.fork?.cut ?? null,
    retained_entries_end: 0,
    conversation_binding: directSpec?.binding ?? { type: "direct" },
    pending_agent_preset: dirAcp?.pending_agent_preset ?? null,
    initial_config: [...cfgMap.entries()],
    initial_prompt: buildInitialPrompt(dirAcp?.pending_prompt),
  };
}

/**
 * 从回答分叉时的切点（对齐 `smelt_model::acp_chat::fork_cut_after`）。
 * 返回 `{ text, occurrence }` 或 `null`（点在最后一回合时整份历史照抄、不设切点）。
 */
export function buildForkCutAfter(entries, throughIndex) {
  let nextIndex = -1;
  let text = "";
  for (let i = throughIndex + 1; i < entries.length; i++) {
    if (!isUserEntry(entries[i])) continue;
    const candidate = entryUserText(entries[i]);
    if (isInterruptMarker(candidate)) continue;
    nextIndex = i;
    text = candidate;
    break;
  }
  if (nextIndex < 0) return null;
  let occurrence = 0;
  for (let i = 0; i < nextIndex; i++) {
    if (isUserEntry(entries[i]) && entryUserText(entries[i]) === text) {
      occurrence += 1;
    }
  }
  return { text, occurrence };
}

/**
 * 从较早用户消息分叉编辑时的切点（对齐 `smelt_model::acp_chat::fork_cut_before`）。
 */
export function buildForkCutBefore(entries, userIndex) {
  if (userIndex < 0 || userIndex >= entries.length || !isUserEntry(entries[userIndex])) {
    return null;
  }
  const text = entryUserText(entries[userIndex]);
  if (isInterruptMarker(text)) return null;
  let occurrence = 0;
  for (let i = 0; i < userIndex; i++) {
    if (isUserEntry(entries[i]) && entryUserText(entries[i]) === text) {
      occurrence += 1;
    }
  }
  return { text, occurrence };
}

export function applyPendingConfigSelection(pending, configId, valueId, confirmedValue) {
  const next = pending.filter(([id]) => id !== configId);
  if (confirmedValue !== valueId) {
    next.push([configId, valueId]);
  }
  return next;
}

export function confirmedConfigValue(configId, configs, model) {
  if (model && (configId === model.config_id || configId === "model") && model.current_value) {
    return model.current_value;
  }
  const cfg = (configs ?? []).find((c) => c.config_id === configId);
  if (!cfg) return null;
  const opt = (cfg.options ?? []).find(([, name]) => name === cfg.current_name);
  return opt ? opt[0] : null;
}

export function reconcilePendingConfigValues(pending, configs, model) {
  return pending.filter(([id, val]) => confirmedConfigValue(id, configs, model) !== val);
}

export function overlayModelState(model, pending) {
  if (!model) return null;
  const hit = pending.find(([id]) => id === model.config_id || id === "model");
  if (!hit) return model;
  const opt = (model.options ?? []).find(([v]) => v === hit[1]);
  if (!opt) return model;
  return { ...model, current_value: opt[0], current_name: opt[1] };
}

export function overlaySessionConfigs(configs, pending) {
  return (configs ?? []).map((cfg) => {
    const hit = pending.find(([id]) => id === cfg.config_id);
    if (!hit) return cfg;
    const opt = (cfg.options ?? []).find(([v]) => v === hit[1]);
    if (!opt) return cfg;
    return {
      ...cfg,
      current_name: opt[1],
      ...(cfg.boolean != null ? { boolean: hit[1] === "true" } : {}),
    };
  });
}

// ---- PromptRail 滑动窗口（对齐 prompt_rail.rs） ----

export function computePromptRailWindow(prompts, readingGlobalIndex, maxVisible = PROMPT_RAIL_MAX_TICKS) {
  const total = prompts.length;
  if (total === 0 || maxVisible < 2) {
    return { items: [], clippedTop: false, clippedBottom: false };
  }
  if (total <= maxVisible) {
    return {
      items: prompts.map((p, slot) => ({ ...p, slot, edgeFactor: 1 })),
      clippedTop: false,
      clippedBottom: false,
    };
  }
  const readingPos = Math.max(
    0,
    prompts.findIndex((p) => p.globalIndex === readingGlobalIndex),
  );
  const half = Math.floor(maxVisible / 2);
  let start = Math.max(0, readingPos - half);
  if (start + maxVisible > total) {
    start = total - maxVisible;
  }
  const end = start + maxVisible;
  const clippedTop = start > 0;
  const clippedBottom = end < total;
  const items = [];
  for (let i = start; i < end; i++) {
    const slot = i - start;
    let edgeFactor = 1;
    if (clippedTop && slot < PROMPT_RAIL_EDGE_FADE) {
      edgeFactor = (slot + 1) / (PROMPT_RAIL_EDGE_FADE + 1);
    } else if (clippedBottom && maxVisible - 1 - slot < PROMPT_RAIL_EDGE_FADE) {
      edgeFactor = (maxVisible - slot) / (PROMPT_RAIL_EDGE_FADE + 1);
    }
    items.push({ ...prompts[i], slot, edgeFactor });
  }
  return { items, clippedTop, clippedBottom };
}

// ---- Diff 提取与统计（对齐 edit_diff.rs） ----

export function diffLineStats(oldText, newText) {
  const oldLines = oldText == null ? [] : String(oldText).split("\n");
  const newLines = String(newText ?? "").split("\n");
  if (oldText == null) {
    return { added: newLines.length, removed: 0 };
  }
  // 轻量公共前后缀裁剪 + 多重集计数，用于卡片标签汇总（完整差异交 Rust <SmeltDiffView> 算）。
  let prefix = 0;
  while (prefix < oldLines.length && prefix < newLines.length && oldLines[prefix] === newLines[prefix]) {
    prefix += 1;
  }
  let suffix = 0;
  while (
    suffix < oldLines.length - prefix &&
    suffix < newLines.length - prefix &&
    oldLines[oldLines.length - 1 - suffix] === newLines[newLines.length - 1 - suffix]
  ) {
    suffix += 1;
  }
  const oldMid = oldLines.slice(prefix, oldLines.length - suffix);
  const newMid = newLines.slice(prefix, newLines.length - suffix);
  const counts = new Map();
  for (const l of oldMid) counts.set(l, (counts.get(l) ?? 0) + 1);
  let common = 0;
  for (const l of newMid) {
    const n = counts.get(l) ?? 0;
    if (n > 0) {
      counts.set(l, n - 1);
      common += 1;
    }
  }
  return {
    added: newMid.length - common,
    removed: oldMid.length - common,
  };
}

export function compactPathLeaf(path) {
  const parts = String(path ?? "")
    .split(/[\/\\]/)
    .filter(Boolean);
  return parts.length > 0 ? parts[parts.length - 1] : String(path ?? "");
}

export function relativePathToCwd(path, cwd) {
  if (!cwd) return path;
  const normCwd = cwd.endsWith("/") || cwd.endsWith("\\") ? cwd : `${cwd}/`;
  if (path.startsWith(normCwd)) return path.slice(normCwd.length);
  return path;
}

export function collectEditFilesFromTool(tc) {
  const byPath = new Map();
  const order = [];
  for (const part of tc?.output ?? []) {
    if (!part || typeof part !== "object" || !("Diff" in part)) continue;
    const d = part.Diff;
    if (!d?.path) continue;
    const stats = diffLineStats(d.old_text ?? null, d.new_text ?? "");
    let file = byPath.get(d.path);
    if (!file) {
      file = {
        path: d.path,
        label: compactPathLeaf(d.path),
        added: 0,
        removed: 0,
        edits: [],
      };
      byPath.set(d.path, file);
      order.push(file);
    }
    file.added += stats.added;
    file.removed += stats.removed;
    file.edits.push({ old: d.old_text ?? null, new: d.new_text ?? "" });
  }
  return order;
}

export function collectEditFilesFromRange(entries, start, end) {
  const byPath = new Map();
  const order = [];
  for (let i = start; i < end; i++) {
    const e = entries[i];
    if (!e || typeof e !== "object" || !("ToolCall" in e)) continue;
    for (const f of collectEditFilesFromTool(e.ToolCall)) {
      let existing = byPath.get(f.path);
      if (!existing) {
        existing = { ...f, edits: [...f.edits] };
        byPath.set(f.path, existing);
        order.push(existing);
      } else {
        existing.added += f.added;
        existing.removed += f.removed;
        existing.edits.push(...f.edits);
      }
    }
  }
  return order;
}

// ---- 统计与格式化 ----

export function usagePercent(used, size) {
  if (!size) return 0;
  return Math.min(100, Math.round((used / size) * 100));
}

export function compactTokenCount(n) {
  const fmt = (val, suffix) => {
    const rounded = Math.round(val * 10) / 10;
    return Number.isInteger(rounded) ? `${rounded}${suffix}` : `${rounded.toFixed(1)}${suffix}`;
  };
  if (n >= 1_000_000) return fmt(n / 1_000_000, "M");
  if (n >= 1_000) return fmt(n / 1_000, "k");
  return String(n ?? 0);
}

export function formatDuration(ms) {
  if (ms == null || ms < 0) return "";
  const totalSec = Math.floor(ms / 1000);
  if (totalSec < 60) return `${Math.max(1, totalSec)}s`;
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  return s > 0 ? `${m}m ${s}s` : `${m}m`;
}

export function runningSubagents(subagents) {
  return (subagents ?? []).filter((s) => s?.status === "running" || s?.status === "pending");
}

export function runningBackgroundTasks(tasks) {
  return (tasks ?? []).filter((t) => t?.status === "running");
}

export function decodeFileUri(uri) {
  const raw = String(uri ?? "");
  if (!raw.startsWith("file://")) return raw;
  const path = raw.slice(7);
  try {
    return decodeURIComponent(path);
  } catch {
    return path;
  }
}

/**
 * 合并省略了信封的快照帧。宿主（`smelt-acp`）发来省略帧时已经去掉了信封键，
 * 本帧带的键都是这一帧的权威值，没带的沿用上一份；不在这里列哪些是信封字段。
 */
export function mergeEnvelope(prev, incoming) {
  if (!prev || !incoming?.envelope_omitted) return incoming;
  return { ...prev, ...incoming, envelope_omitted: false };
}

function shallowEqualRecord(a, b) {
  if (a === b) return true;
  if (!a || !b || typeof a !== "object" || typeof b !== "object") return false;
  const keysA = Object.keys(a);
  const keysB = Object.keys(b);
  if (keysA.length !== keysB.length) return false;
  for (const k of keysA) {
    if (a[k] !== b[k]) return false;
  }
  return true;
}

export function turnTimingsEqual(a, b) {
  if (a === b) return true;
  const listA = Array.isArray(a) ? a : [];
  const listB = Array.isArray(b) ? b : [];
  if (listA.length !== listB.length) return false;
  for (let i = 0; i < listA.length; i++) {
    if (!shallowEqualRecord(listA[i], listB[i])) return false;
  }
  return true;
}

export function isStreamingMetaUnchanged(prev, merged) {
  if (!prev || !merged) return false;
  return (
    prev.phase === merged.phase &&
    prev.replaying_history === merged.replaying_history &&
    prev.end_reason === merged.end_reason &&
    prev.end_kind === merged.end_kind &&
    prev.end_code === merged.end_code &&
    prev.acp_session_id === merged.acp_session_id &&
    prev.history_session_id === merged.history_session_id &&
    prev.history_abandoned === merged.history_abandoned &&
    prev.supports_image === merged.supports_image &&
    prev.supports_compaction === merged.supports_compaction &&
    prev.supports_native_queue === merged.supports_native_queue &&
    prev.supports_rewind === merged.supports_rewind &&
    prev.compacting === merged.compacting &&
    prev.completed_unread === merged.completed_unread &&
    prev.turn_outcome === merged.turn_outcome &&
    prev.usage_cached_read === merged.usage_cached_read &&
    prev.usage_cost === merged.usage_cost &&
    prev.history_error === merged.history_error &&
    prev.history_omitted === merged.history_omitted &&
    shallowEqualRecord(prev.usage ?? null, merged.usage ?? null) &&
    shallowEqualRecord(prev.provider_retry ?? null, merged.provider_retry ?? null)
  );
}

export function consumeComposerRestore(lastRev, incomingRev, incomingTexts, incomingImages) {
  if (!incomingRev || incomingRev <= lastRev) {
    return { lastRevision: lastRev, restoreTexts: null, restoreImages: null };
  }
  return {
    lastRevision: incomingRev,
    restoreTexts: Array.isArray(incomingTexts) && incomingTexts.length > 0 ? incomingTexts : null,
    restoreImages: Array.isArray(incomingImages) && incomingImages.length > 0 ? incomingImages : null,
  };
}

// ---- 推理强度档位视觉映射（对齐原 effort_slider.rs） ----

export function selectedConfigIndex(cfg) {
  const opts = cfg?.options ?? [];
  if (opts.length === 0) return 0;
  const ix = opts.findIndex(([, name]) => name === cfg?.current_name);
  return ix >= 0 ? ix : 0;
}

export function effortProgress(selected, count) {
  if (!count || count <= 1) return 0;
  return Math.max(0, Math.min(1, selected / (count - 1)));
}

export function effortDotColor(progress) {
  const p = Math.max(0, Math.min(1, Number(progress) || 0));
  if (p < 0.18) return "#60a5fa";
  if (p < 0.44) return "#fb923c";
  if (p < 0.62) return "#f43f5e";
  if (p < 0.85) return "#c084fc";
  return "#38bdf8";
}

export function effortGlowTint(progress) {
  const p = Math.max(0, Math.min(1, Number(progress) || 0));
  if (p < 0.18) return "rgba(96, 165, 250, 0.16)";
  if (p < 0.44) return "rgba(251, 146, 60, 0.18)";
  if (p < 0.62) return "rgba(244, 63, 94, 0.18)";
  if (p < 0.85) return "rgba(192, 132, 252, 0.18)";
  return "rgba(56, 189, 248, 0.20)";
}

// ---- 输入框运行态顶边扫光模式与颜色透明度（composer.tsx 与 model.test.mjs 共用） ----

export function composerSweepMode({ turnActive, awaitingUser, windowActive, reduceMotion }) {
  if (!turnActive && !awaitingUser) return "off";
  if (awaitingUser) return "hold";
  if (reduceMotion || !windowActive) return "still";
  return "sweep";
}

export function colorWithAlpha(color, alpha) {
  if (typeof color !== "string") return color;
  const numeric = Number(alpha);
  const clamped = Math.max(0, Math.min(1, Number.isFinite(numeric) ? numeric : 1));
  const trimmed = color.trim();
  if (/^#[0-9a-fA-F]{6}$/.test(trimmed)) {
    const aa = Math.round(clamped * 255)
      .toString(16)
      .padStart(2, "0");
    return `${trimmed}${aa}`;
  }
  if (/^#[0-9a-fA-F]{3}$/.test(trimmed)) {
    const [, r, g, b] = trimmed;
    const aa = Math.round(clamped * 255)
      .toString(16)
      .padStart(2, "0");
    return `#${r}${r}${g}${g}${b}${b}${aa}`;
  }
  const rgbMatch = trimmed.match(
    /^rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)(?:\s*,\s*[\d.]+\s*)?\)$/i,
  );
  if (rgbMatch) {
    const a = Math.round(clamped * 1000) / 1000;
    return `rgba(${rgbMatch[1]}, ${rgbMatch[2]}, ${rgbMatch[3]}, ${a})`;
  }
  return color;
}
