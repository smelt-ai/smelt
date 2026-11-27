// 通用 `<SmeltConversation>` 能力组件（通过 `import { SmeltConversation } from "smelt/sdk"` 供所有插件直接嵌入）。
// 零跨 VM 插槽跳转，零 Rust UI 视图；直接对接宿主纯非 UI ACP 订阅中心 `acp.*` 与 `mermaid.render`。
import { cleanup, computed, effect, Image, LayoutAnimation, signal, Text, View } from "nsh";
import {
  acpAction,
  acpHistory,
  acpImageUri,
  acpRestart,
  acpSubscribe,
  acpUnsubscribe,
  agentCatalog,
  call,
  clipboardWriteImage,
  DIALOG_SURFACE_CLASS,
  type EntryWindow,
  isInterruptMarker,
  mergeEntryWindow,
  newSessionId,
  onAcpEvent,
  pathFileName,
  pathStartsWith,
  pathStyle,
  SmeltButton,
  SmeltDialog,
  SmeltIcon,
  SmeltNativeWindowDrag,
  theme,
  waitOnline,
  workspaceDirectory,
} from "smelt/sdk";
import { MemoryChangesCard } from "./cards.tsx";
import { type AttachedImage, Composer, type ComposerApi } from "./composer.tsx";
import { EditDiffDialog, type EditDiffDialogState } from "./edit_diff_dialog.tsx";
import { ct } from "./i18n.ts";
import {
  applyPendingConfigSelection,
  buildAcpLaunchPayload,
  buildConversationLayout,
  buildDisplayRows,
  confirmedConfigValue,
  consumeComposerRestore,
  createBottomStickController,
  EMPTY_ENTRY_WINDOW,
  entryUserText,
  entryWindowOf,
  extractTurnAnswerPreview,
  isStreamingMetaUnchanged,
  isTransportEnd,
  isTurnActive,
  isUserEntry,
  launchBringsContent,
  mergeEnvelope,
  overlayModelState,
  overlaySessionConfigs,
  reconcilePendingConfigValues,
  reconnectAttempt,
  sessionSkills,
  showsEmptyWelcome,
  spliceEntries,
  turnTimingsEqual,
} from "./model.js";
import { diffPartStats, MessageList } from "./message_list.tsx";
import { PromptRail, type PromptRailItem } from "./prompt_rail.tsx";
import { SubagentDrawer } from "./subagents.tsx";

export interface SmeltConversationStart {
  fork?: {
    provider_session_id: string;
    cut?: any;
  } | null;
  prefill?: string | null;
}

export interface SmeltConversationSpec {
  agent: string;
  cwd: string;
  binding?: any;
}

/** `<SmeltConversation>` 的命令式接口（`conversationRef` 给出）。 */
export interface SmeltConversationApi {
  /** 往输入框里追加文本（如 `smelt.sessions/send-text`）：接在草稿后面，光标放到末尾并聚焦输入框。 */
  insertText(text: string): void;
}

export interface SmeltConversationProps {
  machine: string | null;
  sessionId: string;
  /** 工作区目录中的 `WorkspaceAcp` 条目（会话页用）；未传时可走 `spec`。 */
  directoryAcp?: any | null;
  /** 嵌入式场景直传规格（自动化运行现场 / 第三方插件用）。 */
  spec?: SmeltConversationSpec | null;
  /** 首次新起会话的参数（fork / prefill）。 */
  start?: SmeltConversationStart | null;
  /** 按 agent 记住的初始配置项（会话自身配置优先覆盖）。 */
  initialConfig?: Array<[string, string]>;
  /** 尚未连上时的占位提示。 */
  placeholder?: string | null;
  /** 嵌入模式（如自动化运行页）：隐藏浮动标题胶囊。 */
  embedded?: boolean;
  /** 是否可见（在 `display: none` 隐藏会话或非活动页时传 false，暂停所有旋转动画）。 */
  active?: boolean;
  /** 浮动标题胶囊显示的会话名（非嵌入模式使用）。 */
  title?: string | null;
  /** 顶栏安全区信息（非嵌入模式避让窗口按钮/拖曳区）。 */
  safeArea?: {
    title_bar?: boolean;
    top_left?: number;
    top_right?: number;
  } | null;
  /** 分屏重构同帧零断连复接键。 */
  reuseKey?: string;
  /** 递增触发输入框聚焦。 */
  focusSeq?: number;
  /** 挂上时给出命令式接口，卸下时给 `null`。 */
  conversationRef?: (api: SmeltConversationApi | null) => void;
  /** 递增触发模型列表刷新（如 Pi 配置变更时）。 */
  refreshModelsSeq?: number;
  readOnly?: boolean;
  style?: any;
  onAtLatestChange?: (atLatest: boolean) => void;
  onFocusIn?: () => void;
  onFork?: (req: { provider_session_id: string; cut: any; prompt: string }) => void;
  onNewSession?: () => void;
  onOpenConversation?: (sid: string) => void;
  onManageCapabilities?: (byAgent: boolean) => void;
  onOpenAgentMemory?: (agentId: string) => void;
  onConfigSelected?: (agentId: string, configId: string, valueId: string) => void;
  onRevealInList?: () => void;
}

function defaultLaunchCommandFor(agentId: string): string {
  switch (agentId) {
    case "claude":
      return "claude";
    case "codex":
      return "codex";
    case "copilot":
      return "copilot";
    case "antigravity":
      return "antigravity";
    case "pi":
      return "pi";
    default:
      return agentId || "claude";
  }
}

function agentMeta(agentId: string): { label: string; icon: string | null } {
  try {
    const found = agentCatalog().conversation_agents.find((a) => a.id === agentId);
    if (found) {
      return {
        label: found.short_label || found.label,
        icon: found.icon || null,
      };
    }
  } catch {
    // fallback
  }
  return { label: agentId || ct("title"), icon: null };
}

function agentShortLabel(agentId: string): string {
  return agentMeta(agentId).label;
}

/** 空对话居中与消息流互换的过渡：位置先快后慢，不回弹；离开的先淡出，新出现的稍后淡入。 */
const WELCOME_LAYOUT_TRANSITION = {
  duration: 260,
  update: { type: "easeInEaseOut" },
  delete: { type: "easeIn", property: "opacity", duration: 140 },
  create: { type: "easeOut", property: "opacity", delay: 80, duration: 220 },
};

export function SmeltConversation(props: SmeltConversationProps) {
  const look = theme;
  const visible = computed(() => props.active !== false);

  const subId = signal<string | null>(null);
  const connectError = signal<string | null>(null);
  const snapshot = signal<any | null>(null);
  const turnTimings = signal<any[]>([]);
  let latestSnapshot: any | null = null;
  /** 手里连续的一段条目和它的窗口（`window.offset` 是第一条的全局下标）。 */
  const entriesState = signal<{ entries: any[]; window: EntryWindow }>({
    entries: [],
    window: EMPTY_ENTRY_WINDOW,
  });
  const streamingGlobalIndex = signal<number | null>(null);
  const streamingText = signal("");

  const expandedTurnFolds = signal<Set<string>>(new Set());
  const expandedProcessGroups = signal<Set<string>>(new Set());
  const pendingConfigs = signal<Array<[string, string]>>([]);
  const permissionSubmitting = signal(false);
  const dismissedNoticeId = signal<number | null>(null);
  const loadingOlder = signal(false);
  const atLatest = signal(true);
  const readingGlobalIndex = signal<number | null>(null);
  const pinnedPrompt = signal<string | null>(null);

  // 都是会话自己报的：技能取命令列表里的 `skill:` 条目，MCP 是真正起了连接的服务器。
  const loadedSkills = computed(() => sessionSkills(snapshot()?.available_commands));
  const loadedMcpServers = computed<string[]>(() => snapshot()?.mcp_servers ?? []);
  const diffDialog = signal<EditDiffDialogState | null>(null);
  const inspectedSubagent = signal<{ id: string; title: string } | null>(null);
  const imagePreview = signal<{ uri: string; loading: boolean; img: any } | null>(null);
  const imagePreviewCopied = signal(false);
  let imagePreviewCopyTimer: ReturnType<typeof setTimeout> | null = null;
  const reconnectSeq = signal(0);
  /** 对话流断了，正在等机器上线后自动重新订阅。 */
  const reconnecting = signal(false);
  /** 当前这条对话流已经关了：要再连上得重新订阅。 */
  const streamClosed = signal(false);

  let listApi: any = null;
  /** 输入框跟对话同生同灭，挂上就有。 */
  let composerApi: ComposerApi | null = null;
  let lastRestoreRev = 0;
  cleanup(() => props.conversationRef?.(null));

  let lastRefreshModelsSeq = props.refreshModelsSeq ?? 0;
  let lastNotifiedAtLatest = true;
  let backfilledAt: number | null = null;
  let lastRejectedNoticeId: number | null = null;
  let lastInitialPromptText: string | null = null;
  /** 自动重连连续失败的次数，和这条流第一次收到活帧的时间（判断撑没撑过稳定期）。 */
  let reconnectFailures = 0;
  let usableSince: number | null = null;

  const agentId = computed(() => {
    const dir = props.directoryAcp;
    if (dir?.agent) return String(dir.agent);
    if (props.spec?.agent) return String(props.spec.agent);
    return "claude";
  });

  const cwd = computed(() => {
    const dir = props.directoryAcp;
    if (typeof dir?.cwd === "string" && dir.cwd.length > 0) return dir.cwd;
    if (typeof props.spec?.cwd === "string" && props.spec.cwd.length > 0) return props.spec.cwd;
    return null;
  });

  const phase = computed(() => String(snapshot()?.phase ?? "connecting"));
  const connecting = computed(() => phase() === "connecting");
  const turnActive = computed(() => isTurnActive(phase()));
  const settled = computed(() => !connecting() && !turnActive());
  const dead = computed(() => phase() === "dead");
  const capabilitiesByAgent = computed(() => Boolean(props.directoryAcp?.agent_definition_id));

  const effectiveModel = computed(() =>
    overlayModelState(snapshot()?.model ?? null, pendingConfigs()),
  );
  const effectiveConfigs = computed(() =>
    overlaySessionConfigs(snapshot()?.config_options ?? [], pendingConfigs()),
  );

  const layout = computed(() => {
    const es = entriesState();
    return buildConversationLayout(
      es.entries,
      turnActive(),
      es.window.offset,
      turnTimings(),
    );
  });

  const displayRows = computed(() => {
    const es = entriesState();
    return buildDisplayRows(
      es.entries,
      layout(),
      es.window.offset,
      expandedTurnFolds(),
      expandedProcessGroups(),
      diffPartStats,
    );
  });

  const promptItems = computed<PromptRailItem[]>(() => {
    const es = entriesState();
    const rows = displayRows();
    const out: PromptRailItem[] = [];
    rows.forEach((row: any, rowIndex: number) => {
      if (row.kind !== "entry" || !isUserEntry(row.entry)) return;
      const text = entryUserText(row.entry).trim();
      if (!text || isInterruptMarker(text)) return;
      out.push({
        globalIndex: row.globalIndex,
        rowIndex,
        text,
        answer: extractTurnAnswerPreview(es.entries, row.entryIndex),
      });
    });
    return out;
  });

  const latestUserGlobalIndex = computed(() => {
    const es = entriesState();
    let local: number | null = null;
    for (let i = es.entries.length - 1; i >= 0; i--) {
      const entry = es.entries[i];
      if (isUserEntry(entry) && !isInterruptMarker(entryUserText(entry))) {
        local = i;
        break;
      }
    }
    return local != null ? es.window.offset + local : null;
  });

  const canFork = computed(() =>
    Boolean(props.onFork && snapshot()?.acp_session_id && !props.embedded),
  );

  const applyIncomingSnapshot = (rawSnap: any, initial: boolean) => {
    const prevSnap = latestSnapshot;
    const merged = mergeEnvelope(prevSnap, rawSnap);
    latestSnapshot = merged;
    if (permissionSubmitting()) permissionSubmitting.set(false);

    // 热路径快判：流式阶段绝大多数帧只更新最后一条活跃 Assistant 正文；
    // 先在 JS 侧确认单条尾部补丁，直接跳过 `mergeEntryWindow` 同步 FFI 与 `spliceEntries` 整表拷贝。
    const prevEs = entriesState();
    const prevLen = prevEs.entries.length;
    const prevWin = prevEs.window;
    const incomingEntries = merged.entries ?? [];
    const incWin = entryWindowOf(merged);
    const active = isTurnActive(String(merged.phase ?? ""));
    const prevLast = prevLen > 0 ? prevEs.entries[prevLen - 1] : null;
    const incLast = incomingEntries.length > 0 ? incomingEntries[incomingEntries.length - 1] : null;
    const tailGlobalIndex = prevWin.offset + prevLen - 1;
    const fastTailOnly =
      !initial &&
      active &&
      prevLen > 0 &&
      incWin.history_session_id === prevWin.history_session_id &&
      incWin.acp_session_id === prevWin.acp_session_id &&
      incWin.revision >= prevWin.revision &&
      incomingEntries.length === 1 &&
      incWin.offset === tailGlobalIndex &&
      incWin.total === prevWin.offset + prevLen &&
      Boolean(prevLast?.Assistant) &&
      Boolean(incLast?.Assistant) &&
      Boolean(prevLast.Assistant.thought) === Boolean(incLast.Assistant.thought);

    let nextEs: { entries: any[]; window: any; dirtyFrom: number | null };
    if (fastTailOnly) {
      prevEs.entries[prevLen - 1] = incLast;
      prevEs.window = {
        ...prevWin,
        total: incWin.total,
        revision: incWin.revision,
      };
      nextEs = { entries: prevEs.entries, window: prevEs.window, dirtyFrom: prevLen - 1 };
      streamingGlobalIndex.set(tailGlobalIndex);
      streamingText.set(String(incLast.Assistant.text ?? ""));
      if (!(rawSnap?.envelope_omitted && isStreamingMetaUnchanged(prevSnap, merged))) {
        snapshot.set(merged);
      }
    } else {
      // 合并条目窗口（规则和手机端同一份，见 SDK `mergeEntryWindow`）
      const plan = mergeEntryWindow(prevEs.window, incWin);
      nextEs = {
        entries: spliceEntries(prevEs.entries, plan, incomingEntries),
        window: plan.window,
        dirtyFrom: plan.changed_from,
      };

      // 兜底热路径：整窗重推但仅最后一条活跃 Assistant 正文增长时同样只写 `streamingText`。
      const nextLast = nextEs.entries[nextEs.entries.length - 1];
      const sameShapeTailOnly =
        !initial &&
        active &&
        nextEs.window.offset === prevEs.window.offset &&
        nextEs.entries.length === prevEs.entries.length &&
        nextEs.entries.length > 0 &&
        nextEs.dirtyFrom === nextEs.entries.length - 1 &&
        Boolean(prevLast?.Assistant) &&
        Boolean(nextLast?.Assistant) &&
        Boolean(prevLast.Assistant.thought) === Boolean(nextLast.Assistant.thought);

      if (sameShapeTailOnly) {
        const globalIx = nextEs.window.offset + nextEs.entries.length - 1;
        prevEs.entries[prevEs.entries.length - 1] = nextLast;
        prevEs.window = nextEs.window;
        streamingGlobalIndex.set(globalIx);
        streamingText.set(String(nextLast.Assistant.text ?? ""));
        if (!(rawSnap?.envelope_omitted && isStreamingMetaUnchanged(prevSnap, merged))) {
          snapshot.set(merged);
        }
      } else {
        snapshot.set(merged);
        const nextTimings = merged.turn_timings ?? [];
        if (!turnTimingsEqual(turnTimings(), nextTimings)) {
          turnTimings.set(nextTimings);
        }
        if (nextEs.dirtyFrom !== null || initial) {
          entriesState.set({ entries: nextEs.entries, window: nextEs.window });
          if (active && nextLast?.Assistant) {
            streamingGlobalIndex.set(nextEs.window.offset + nextEs.entries.length - 1);
            streamingText.set(String(nextLast.Assistant.text ?? ""));
          } else {
            streamingGlobalIndex.set(null);
          }
        } else {
          // 条目没变：总数、修订号照样要记，下一帧和翻页按它合并；不触发重画。
          prevEs.window = nextEs.window;
        }
      }
    }

    // 窗口停在末尾却是空的（`acp_open` 默认 `omit_history: true` 的首帧、对面回退到窗口之前），
    // 自动向守护拉一页历史，避免空白。每个起点只补一次，拉不到也不反复拉。
    if (
      !merged.replaying_history &&
      nextEs.window.offset > 0 &&
      nextEs.entries.length === 0 &&
      backfilledAt !== nextEs.window.offset
    ) {
      backfilledAt = nextEs.window.offset;
      void handleLoadOlder();
    }

    // 协调待生效配置覆盖层（同一条 config_rejected notice 只触发一次，避免 envelope_omitted 帧反复清空）
    if (
      !rawSnap?.envelope_omitted &&
      merged.notice?.kind === "config_rejected" &&
      merged.notice?.id !== lastRejectedNoticeId
    ) {
      lastRejectedNoticeId = merged.notice?.id ?? null;
      pendingConfigs.set([]);
    } else if (pendingConfigs().length > 0) {
      const reconciled = reconcilePendingConfigValues(
        pendingConfigs(),
        merged.config_options ?? [],
        merged.model ?? null,
      );
      if (reconciled.length !== pendingConfigs().length) {
        pendingConfigs.set(reconciled);
      }
    }

    // 处理 clear_queue 还回输入框的草稿
    const restored = consumeComposerRestore(
      lastRestoreRev,
      merged.composer_restore_revision ?? 0,
      merged.composer_restore_texts,
      merged.composer_restore_images,
    );
    if (restored.lastRevision !== lastRestoreRev) {
      lastRestoreRev = restored.lastRevision;
      const texts = restored.restoreTexts ?? [];
      const imgs: AttachedImage[] = (restored.restoreImages ?? [])
        .map((img: any) =>
          img?.ref_id && img?.preview_uri
            ? {
                ref_id: img.ref_id,
                mime: img.mime || "image/png",
                preview_uri: img.preview_uri,
                width: img.width ?? 0,
                height: img.height ?? 0,
              }
            : null,
        )
        .filter(Boolean) as AttachedImage[];
      if (texts.length > 0 || imgs.length > 0) {
        composerApi?.restore(texts.join("\n"), imgs);
      }
      const sid = subId();
      if (sid) {
        void acpAction(sid, { AcknowledgeComposerRestore: { revision: lastRestoreRev } }).catch(
          () => {},
        );
      }
    }
  };

  // 同步最新启动上下文，不让 `directoryAcp` 对象引用刷新（如标题/状态推送）触发下方订阅重连。
  let latestDirAcp: any = null;
  let latestSpec: SmeltConversationSpec | null = null;
  let latestStart: SmeltConversationStart | null = null;
  let latestInitialConfig: Array<[string, string]> = [];
  let latestReuseKey: string | undefined = undefined;
  let activeSessionKey: string | null = null;

  effect(() => {
    latestDirAcp = props.directoryAcp ?? null;
    latestSpec = props.spec ?? null;
    latestStart = props.start ?? null;
    latestInitialConfig = props.initialConfig ?? [];
    latestReuseKey = props.reuseKey;
  });

  const launchTargetKey = computed(() => {
    const sid = props.sessionId;
    const hasSource = props.directoryAcp != null || props.spec != null;
    if (!sid || !hasSource) return null;
    return `${props.machine ?? ""}\0${sid}\0${reconnectSeq()}`;
  });

  // 建立与维护 ACP 订阅生命周期
  effect(() => {
    const targetKey = launchTargetKey();
    if (!targetKey) return;
    const machine = props.machine ?? null;
    const sessionId = props.sessionId;
    const dirAcp = latestDirAcp;
    const directSpec = latestSpec;
    const start = latestStart;
    const initialConfig = latestInitialConfig;
    const reuseKey = latestReuseKey;
    if (!dirAcp && !directSpec) return;

    const sessionKey = `${machine ?? ""}\0${sessionId}`;
    if (activeSessionKey !== null && activeSessionKey !== sessionKey) {
      latestSnapshot = null;
      snapshot.set(null);
      turnTimings.set([]);
      entriesState.set({ entries: [], window: EMPTY_ENTRY_WINDOW });
      streamingGlobalIndex.set(null);
      streamingText.set("");
      pendingConfigs.set([]);
      expandedTurnFolds.set(new Set());
      expandedProcessGroups.set(new Set());
      dismissedNoticeId.set(null);
      inspectedSubagent.set(null);
      diffDialog.set(null);
      imagePreview.set(null);
      imagePreviewCopied.set(false);
      pinnedPrompt.set(null);
      readingGlobalIndex.set(null);
      bottomStick.cancel();
      atLatest.set(true);
      loadingOlder.set(false);
      lastRestoreRev = 0;
      backfilledAt = null;
      lastRejectedNoticeId = null;
      lastInitialPromptText = null;
      reconnecting.set(false);
      reconnectFailures = 0;
    }
    activeSessionKey = sessionKey;

    let cancelled = false;
    let firstFrame = true;
    const currentSubId = `sub-${newSessionId()}`;
    subId.set(currentSubId);
    connectError.set(null);
    streamClosed.set(false);
    usableSince = null;

    const unsubEvent = onAcpEvent((ev: any) => {
      if (cancelled || !ev || ev.sub_id !== currentSubId) return;
      if (ev.kind === "snapshot" && ev.snapshot) {
        applyIncomingSnapshot(ev.snapshot, firstFrame);
        firstFrame = false;
        if (latestSnapshot?.phase !== "dead") {
          usableSince ??= Date.now();
          reconnecting.set(false);
        } else if (!isTransportEnd(latestSnapshot)) {
          reconnecting.set(false);
        }
      } else if (ev.kind === "closed") {
        streamClosed.set(true);
        const attempt = reconnectAttempt(
          latestSnapshot,
          reconnectFailures,
          usableSince,
          Date.now(),
        );
        if (attempt == null) {
          reconnecting.set(false);
          return;
        }
        reconnectFailures = attempt + 1;
        reconnecting.set(true);
        void waitOnline(machine, attempt).then((online) => {
          if (cancelled) return;
          if (online) reconnectSeq.set(reconnectSeq() + 1);
          else reconnecting.set(false);
        });
      } else if (ev.kind === "initial_prompt_unsupported" && lastInitialPromptText) {
        const text = lastInitialPromptText;
        lastInitialPromptText = null;
        composerApi?.restore(text, []);
      }
    });

    const resolvedAgent = dirAcp?.agent
      ? String(dirAcp.agent)
      : directSpec?.agent
        ? String(directSpec.agent)
        : "claude";
    const resolvedCwd =
      typeof dirAcp?.cwd === "string" && dirAcp.cwd.length > 0
        ? dirAcp.cwd
        : typeof directSpec?.cwd === "string" && directSpec.cwd.length > 0
          ? directSpec.cwd
          : null;

    const startLaunch = async () => {
      try {
        let launchSpec = dirAcp?.launch
          ? { command: String(dirAcp.launch.command ?? ""), env: dirAcp.launch.env ?? {} }
          : { command: defaultLaunchCommandFor(resolvedAgent), env: {} };

        if (dirAcp) {
          try {
            const prepared = await call(machine, "session.launch.prepare", {
              launch: launchSpec,
              prep: {
                agent_definition_id: dirAcp.agent_definition_id ?? null,
                restore: !start,
                restart: null,
              },
            });
            if (cancelled) return;
            if (prepared?.launch) launchSpec = prepared.launch;
          } catch {
            // 旧运行时或离线时直接沿用原始 launchSpec
          }
        }

        if (cancelled) return;

        if (start?.prefill) {
          composerApi?.restore(start.prefill, []);
        }

        const launchPayload = buildAcpLaunchPayload({
          sessionId,
          cwd: resolvedCwd,
          launchSpec,
          agentId: resolvedAgent,
          dirAcp,
          directSpec,
          start,
          initialConfig,
        });
        lastInitialPromptText = launchPayload.initial_prompt?.text ?? null;

        await acpSubscribe({
          subId: currentSubId,
          machine,
          reuseKey,
          launch: launchPayload,
        });
      } catch (err: any) {
        if (!cancelled) {
          connectError.set(err?.message ?? String(err));
        }
      }
    };

    void startLaunch();

    cleanup(() => {
      cancelled = true;
      unsubEvent();
      if (subId() === currentSubId) subId.set(null);
      void acpUnsubscribe(currentSubId, reuseKey).catch(() => {});
    });
  });

  // 响应外部模型刷新信号（如 Pi 配置变化）
  effect(() => {
    const seq = props.refreshModelsSeq ?? 0;
    if (seq > 0 && seq !== lastRefreshModelsSeq) {
      lastRefreshModelsSeq = seq;
      const sid = subId();
      if (sid) void acpAction(sid, "RefreshModels").catch(() => {});
    }
  });

  const sendAction = (action: unknown, imageRefs?: string[]) => {
    const sid = subId();
    if (!sid) return;
    void acpAction(sid, action, imageRefs).catch((err) => {
      console.error("[SmeltConversation] acpAction failed:", err);
    });
  };

  const handleLoadOlder = async () => {
    const es = entriesState();
    if (loadingOlder() || es.window.offset <= 0) return;
    loadingOlder.set(true);
    try {
      const older = await acpHistory(props.machine ?? null, props.sessionId, es.window.offset, 100);
      if (older && Array.isArray(older.entries)) {
        const current = entriesState();
        const plan = mergeEntryWindow(current.window, entryWindowOf(older));
        // 拉页期间对话重新开始了（`conversation_changed`）：这页属于旧对话，丢掉。
        if (plan.changed_from !== null && !plan.conversation_changed) {
          entriesState.set({
            entries: spliceEntries(current.entries, plan, older.entries),
            window: plan.window,
          });
        } else if (!plan.conversation_changed) {
          current.window = plan.window;
        }
      }
    } catch (err) {
      console.error("[SmeltConversation] loadOlder failed:", err);
    } finally {
      loadingOlder.set(false);
    }
  };

  const clearImagePreviewCopyTimer = () => {
    if (imagePreviewCopyTimer != null) {
      clearTimeout(imagePreviewCopyTimer);
      imagePreviewCopyTimer = null;
    }
    imagePreviewCopied.set(false);
  };

  cleanup(clearImagePreviewCopyTimer);

  const closeImagePreview = () => {
    clearImagePreviewCopyTimer();
    imagePreview.set(null);
  };

  const handleCopyPreviewImage = () => {
    const preview = imagePreview();
    if (!preview) return;
    const img = preview.img;
    const payload = {
      ref_id: typeof img?.ref_id === "string" && img.ref_id.length > 0 ? img.ref_id : null,
      data_b64: typeof img?.data_b64 === "string" && img.data_b64.length > 0 ? img.data_b64 : null,
      mime: typeof img?.mime === "string" && img.mime.length > 0 ? img.mime : null,
      data_uri: typeof preview.uri === "string" && preview.uri.length > 0 ? preview.uri : null,
    };
    if (!payload.ref_id && !payload.data_b64 && !payload.data_uri) return;
    void clipboardWriteImage(payload)
      .then(() => {
        imagePreviewCopied.set(true);
        if (imagePreviewCopyTimer != null) clearTimeout(imagePreviewCopyTimer);
        imagePreviewCopyTimer = setTimeout(() => {
          imagePreviewCopied.set(false);
          imagePreviewCopyTimer = null;
        }, 1500);
      })
      .catch(() => {});
  };

  const handlePreviewImage = (img: any) => {
    const quick =
      typeof img?.preview_uri === "string" && img.preview_uri.length > 0
        ? img.preview_uri
        : typeof img?.data_b64 === "string" && img.data_b64.length > 0
          ? `data:${img.mime || "image/png"};base64,${img.data_b64}`
          : "";
    if (!quick && !img?.ref_id) return;
    clearImagePreviewCopyTimer();
    imagePreview.set({ uri: quick, loading: Boolean(img?.ref_id), img });
    if (img?.ref_id) {
      acpImageUri(img.ref_id)
        .then((fullUri) => {
          const cur = imagePreview();
          if (cur == null || cur.img !== img) return;
          if (fullUri) {
            imagePreview.set({ uri: fullUri, loading: false, img });
          } else {
            imagePreview.set({ uri: quick, loading: false, img });
          }
        })
        .catch(() => {
          const cur = imagePreview();
          if (cur != null && cur.img === img) imagePreview.set({ uri: quick, loading: false, img });
        });
    }
  };

  const handleSelectConfig = (configId: string, valueId: string, boolVal?: boolean) => {
    const confirmed = confirmedConfigValue(
      configId,
      snapshot()?.config_options ?? [],
      snapshot()?.model ?? null,
    );
    pendingConfigs.set(applyPendingConfigSelection(pendingConfigs(), configId, valueId, confirmed));
    sendAction({
      SetConfigOption: {
        config_id: configId,
        value_id: valueId,
        boolean: boolVal ?? null,
      },
    });
    props.onConfigSelected?.(agentId(), configId, valueId);
  };

  const updateAtLatest = (next: boolean) => {
    if (atLatest() !== next) atLatest.set(next);
    if (lastNotifiedAtLatest !== next) {
      lastNotifiedAtLatest = next;
      props.onAtLatestChange?.(next);
    }
  };

  const bottomStick = createBottomStickController({
    scrollToEnd: (opts: { animated?: boolean }) => listApi?.scrollToEnd?.(opts),
    setAtLatest: updateAtLatest,
  });
  cleanup(bottomStick.cancel);

  /** 用户主动发消息后立刻无动画跳到底，并在新气泡/回复落地后的布局帧持续补贴底。 */
  const scrollToBottomAfterSend = () => {
    bottomStick.trigger({ animated: false });
  };

  effect(() => {
    // 订阅行数与流式文本变化：仅在发送后贴底窗口激活时经 rAF 合并补贴底。
    displayRows().length;
    streamingText().length;
    bottomStick.onContentGrew();
  });

  const handleViewableRange = (firstRowIdx: number, _lastRowIdx: number) => {
    const rows = displayRows();
    const firstRow = rows[firstRowIdx];
    const firstGlobal = firstRow?.globalIndex ?? 0;
    const prompts = promptItems();
    let currentPrompt: PromptRailItem | null = null;
    for (const p of prompts) {
      if (p.globalIndex <= firstGlobal) {
        currentPrompt = p;
      } else {
        break;
      }
    }
    readingGlobalIndex.set(currentPrompt?.globalIndex ?? prompts[0]?.globalIndex ?? null);
    // 当最近的提问已经滚到视口上方时，在顶栏胶囊吸顶显示该提问
    if (currentPrompt && currentPrompt.rowIndex < firstRowIdx) {
      pinnedPrompt.set(currentPrompt.text);
    } else {
      pinnedPrompt.set(null);
    }
  };

  const visibleNotice = computed(() => {
    const n = snapshot()?.notice;
    if (!n || !n.text || n.id === dismissedNoticeId()) return null;
    return n;
  });

  const retryInfo = computed(() => snapshot()?.provider_retry ?? null);

  const showTitlePill = computed(() => !props.embedded && Boolean(props.title));
  const titleBarTopPad = computed(() => (showTitlePill() || props.safeArea?.title_bar ? 48 : 16));
  const currentAgentMeta = computed(() => agentMeta(agentId()));
  // 对话所在的项目名：cwd 落在打开的某个项目里（含它的工作树）才算，取最深的那个根。
  // 智能体自己的目录、工作台对话的托管目录都不是项目，不显示。
  const projectName = computed(() => {
    const dir = cwd();
    if (!dir) return null;
    const style = pathStyle(props.machine);
    const roots: string[] = workspaceDirectory(props.machine)?.projects ?? [];
    let best: string | null = null;
    for (const root of roots) {
      if (!pathStartsWith(style, dir, root)) continue;
      if (best == null || root.length > best.length) best = root;
    }
    return best ? pathFileName(style, best) : null;
  });
  const bringsContent = computed(() =>
    launchBringsContent({ dirAcp: props.directoryAcp, start: props.start }),
  );
  // 空对话：输入框移到中间，上面是标题。
  const showEmptyWelcome = computed(() =>
    showsEmptyWelcome({
      embedded: Boolean(props.embedded),
      phase: phase(),
      replayingHistory: Boolean(snapshot()?.replaying_history),
      bringsContent: bringsContent(),
      connectFailed: connectError() != null,
      loadingOlder: loadingOlder(),
      entryCount: entriesState().entries.length,
      loadedOffset: entriesState().window.offset,
    }),
  );
  // 居中和消息流之间切换（发出第一条、续接回来是空的、连接失败）不跳：输入框滑到新位置、
  // 收成一行。换会话是直接切，不过渡；减弱动态时 NSH 直接落位。
  let lastWelcome: { key: string; shown: boolean } | null = null;
  effect(() => {
    const shown = showEmptyWelcome();
    const key = `${props.machine ?? ""}\0${props.sessionId}`;
    const prev = lastWelcome;
    lastWelcome = { key, shown };
    if (prev?.key === key && prev.shown !== shown) {
      LayoutAnimation.configureNext(WELCOME_LAYOUT_TRANSITION);
    }
  });
  const noEntries = computed(
    () => entriesState().entries.length === 0 && entriesState().window.offset === 0,
  );
  const showEndedEmpty = computed(() => dead() && !reconnecting() && noEntries());
  /** 结束态的按钮只需重新订阅：流关了且只是连接断了，会话可能还活着，不能杀进程重启。 */
  const reconnectOnly = computed(() => streamClosed() && isTransportEnd(snapshot()));
  const restartOrReconnect = () => {
    if (reconnectOnly()) {
      reconnectFailures = 0;
      reconnectSeq.set(reconnectSeq() + 1);
      return;
    }
    acpRestart(props.machine ?? null, props.sessionId)
      .catch(() => {})
      .finally(() => reconnectSeq.set(reconnectSeq() + 1));
  };

  return (
    <View
      onPressDown={() => props.onFocusIn?.()}
      style={{
        position: "relative",
        flex: 1,
        width: "100%",
        height: "100%",
        minWidth: 0,
        minHeight: 0,
        display: "flex",
        flexDirection: "row",
        overflow: "hidden",
        ...(props.style ?? {}),
      }}
    >
      {/* 主对话列 */}
      <View
        style={{
          position: "relative",
          flex: 1,
          minWidth: 0,
          minHeight: 0,
          display: "flex",
          flexDirection: "column",
        }}
      >
        {/* 顶栏窗口拖曳带（非嵌入且占顶栏时） */}
        <If when={!props.embedded && Boolean(props.safeArea?.title_bar)}>
          <View
            style={{
              position: "absolute",
              top: 0,
              left: 0,
              right: 0,
              height: 34,
              zIndex: 5,
            }}
          >
            <SmeltNativeWindowDrag style={{ width: "100%", height: "100%" }} />
          </View>
        </If>

        {/* 浮动居中标题 / 吸顶提问胶囊 */}
        <If when={showTitlePill()}>
          <View
            style={{
              position: "absolute",
              top: 10,
              left: 0,
              right: 0,
              height: 28,
              alignItems: "center",
              justifyContent: "center",
              pointerEvents: "box-none",
              zIndex: 10,
            }}
          >
            <SmeltButton
              variant="floating"
              shape="pill"
              label={pinnedPrompt() ?? props.title ?? ct("title")}
              tooltip={
                effectiveModel()?.current_name
                  ? `${props.title ?? ct("title")} · ${effectiveModel().current_name}`
                  : (props.title ?? ct("title"))
              }
              class={pinnedPrompt() ? "max-w-120" : "max-w-60"}
              onPress={() => props.onRevealInList?.()}
            />
          </View>
        </If>

        {/* 空对话：输入框移到中间，标题贴在它上面 */}
        <If when={showEmptyWelcome()}>
          <View
            style={{
              flexGrow: 1,
              flexBasis: 0,
              minHeight: 0,
              alignItems: "center",
              justifyContent: "flex-end",
              paddingTop: titleBarTopPad(),
              paddingHorizontal: 32,
              paddingBottom: 20,
            }}
          >
            <View style={{ flexDirection: "row", alignItems: "center", gap: 12, maxWidth: 736 }}>
              <If when={Boolean(currentAgentMeta().icon)}>
                <SmeltIcon path={currentAgentMeta().icon!} size={26} color={look().palette.text} />
              </If>
              <Text
                style={{
                  numberOfLines: 1,
                  flexShrink: 1,
                  fontSize: 26,
                  lineHeight: 34,
                  fontWeight: 500,
                  color: look().palette.text,
                }}
              >
                {projectName()
                  ? ct("empty.heading_in", { name: projectName() ?? "" })
                  : ct("empty.heading")}
              </Text>
            </View>
          </View>
        </If>

        {/* 消息流主体或启动 / 结束 */}
        <If when={!showEmptyWelcome()}>
          <If
            when={
              (entriesState().entries.length > 0 || (snapshot() != null && !reconnecting())) &&
              !showEndedEmpty()
            }
            else={
              <View style={{ flex: 1, alignItems: "center", justifyContent: "center", gap: 12, padding: 24 }}>
                <If
                  when={showEndedEmpty()}
                  else={
                    <If
                      when={connectError() == null}
                      else={
                        <View style={{ alignItems: "center", gap: 12, maxWidth: 420 }}>
                          <View class="w-11 h-11 rounded-full items-center justify-center bg-bad/12 border border-bad">
                            <SmeltIcon path="icons/circle-x.svg" size={20} color={look().palette.bad} />
                          </View>
                          <Text
                            style={{
                              fontSize: look().type.ui.size,
                              color: look().palette.muted,
                              textAlign: "center",
                            }}
                          >
                            {connectError() ?? ""}
                          </Text>
                          <SmeltButton
                            variant="primary"
                            size="sm"
                            label={ct("action.retry")}
                            onPress={() => reconnectSeq.set(reconnectSeq() + 1)}
                          />
                        </View>
                      }
                    >
                      <SmeltIcon spin={visible()} size={16} color={look().palette.muted} />
                      <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
                        {reconnecting()
                          ? ct("status.reconnecting")
                          : props.placeholder || ct("status.opening")}
                      </Text>
                    </If>
                  }
                >
                  <View style={{ alignItems: "center", gap: 12, maxWidth: 420 }}>
                    <View class="w-12 h-12 rounded-full items-center justify-center bg-bad/12 border border-bad">
                      <SmeltIcon path="icons/circle-x.svg" size={22} color={look().palette.bad} />
                    </View>
                    <Text
                      style={{
                        fontSize: 18,
                        fontWeight: 600,
                        color: look().palette.text,
                        textAlign: "center",
                      }}
                    >
                      {ct("ended.title")}
                    </Text>
                    <Text
                      style={{
                        fontSize: look().type.ui.size,
                        color: look().palette.muted,
                        textAlign: "center",
                      }}
                    >
                      {snapshot()?.end_reason || ct("status.disconnected")}
                    </Text>
                    <SmeltButton
                      variant="primary"
                      label={
                        reconnectOnly()
                          ? ct("action.reconnect")
                          : snapshot()?.history_session_id
                            ? ct("ended.resume")
                            : ct("ended.new")
                      }
                      onPress={restartOrReconnect}
                    />
                  </View>
                </If>
              </View>
            }
          >
            <View style={{ position: "relative", flex: 1, width: "100%", minHeight: 0 }}>
              <MessageList
                rows={displayRows}
                visible={visible}
                machine={() => props.machine ?? null}
                sessionId={() => props.sessionId}
                cwd={cwd}
                hasOlderHistory={() => entriesState().window.offset > 0}
                loadingOlder={loadingOlder}
                atLatest={atLatest}
                turnActive={turnActive}
                settled={settled}
                streamingGlobalIndex={streamingGlobalIndex}
                streamingText={streamingText}
                latestUserGlobalIndex={latestUserGlobalIndex}
                canFork={canFork}
                supportsRewind={() => Boolean(snapshot()?.supports_rewind)}
                liveSubagents={() => snapshot()?.subagents ?? []}
                topPadding={titleBarTopPad}
                listRef={(api) => {
                  listApi = api;
                }}
                onLoadOlder={() => void handleLoadOlder()}
                onViewableRange={handleViewableRange}
                onScrollAtLatestChange={(nearEnd) => bottomStick.onViewableAtLatest(nearEnd)}
                onUserScroll={() => bottomStick.cancel()}
                onJumpToLatest={() => bottomStick.trigger({ animated: !turnActive() })}
                onToggleTurnFold={(key) => {
                  const next = new Set(expandedTurnFolds());
                  if (!next.delete(key)) next.add(key);
                  expandedTurnFolds.set(next);
                }}
                onToggleProcessGroup={(key) => {
                  const next = new Set(expandedProcessGroups());
                  if (!next.delete(key)) next.add(key);
                  expandedProcessGroups.set(next);
                }}
                onOpenDiff={(d) => diffDialog.set(d)}
                onPreviewImage={handlePreviewImage}
                onForkAt={(globalIndex, role) => {
                  const acpId = snapshot()?.acp_session_id;
                  const onFork = props.onFork;
                  if (!acpId || !onFork) return;
                  const editing = role === "user";
                  if (editing && (!snapshot()?.supports_rewind || latestUserGlobalIndex() === globalIndex)) {
                    return;
                  }
                  // 同文本提问的序号要数整段对话，手里只有最近几页，切点让运行时算。
                  void call(props.machine ?? null, "conversation.fork_cut", {
                    session_id: props.sessionId,
                    entry_index: globalIndex,
                    side: editing ? "before" : "after",
                  })
                    .then((res: any) => {
                      const cut = res?.cut ?? null;
                      if (editing && !cut) return;
                      onFork({ provider_session_id: acpId, cut, prompt: editing ? cut.text : "" });
                    })
                    .catch((err) => console.error("[conversation] 算分叉切点失败：", err));
                }}
                onEditResendAt={(globalIndex, _text) => {
                  sendAction({ RewindToMessage: { entry_index: globalIndex } });
                }}
                onRetryTurn={() => {
                  sendAction("RetryLastTurn");
                  scrollToBottomAfterSend();
                }}
                onInspectSubagent={(sub) => inspectedSubagent.set(sub)}
                onOpenConversation={props.onOpenConversation}
              />
            </View>
          </If>
        </If>

        {/* 左侧提问导航刻度列 */}
        <PromptRail
          prompts={promptItems}
          readingGlobalIndex={readingGlobalIndex}
          onJump={(item) => {
            bottomStick.cancel();
            readingGlobalIndex.set(item.globalIndex);
            listApi?.scrollToIndex?.({ index: item.rowIndex, animated: true });
          }}
          onScrub={(item) => {
            bottomStick.cancel();
            readingGlobalIndex.set(item.globalIndex);
            listApi?.scrollToIndex?.({ index: item.rowIndex, animated: false });
          }}
        />

        {/* 状态横幅（启动进度 / Provider 重试 / 一次性 Notice / 断连重启） */}
        <If when={Boolean(snapshot()?.status_line) && (connecting() || turnActive())}>
          <View style={{ width: "100%", alignItems: "center", paddingHorizontal: 16, paddingBottom: 6 }}>
            <View style={{ width: "100%", maxWidth: 768, paddingHorizontal: 6, flexDirection: "row", alignItems: "center", gap: 6 }}>
              <SmeltIcon spin={visible()} size={12} color={look().palette.muted} />
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {snapshot()?.status_line ?? ""}
              </Text>
            </View>
          </View>
        </If>

        <If when={retryInfo() != null}>
          <View style={{ width: "100%", alignItems: "center", paddingHorizontal: 16, paddingBottom: 6 }}>
            <View
              class="w-full px-3 py-2 rounded-xl bg-soft border border-border flex-row items-center gap-2"
              style={{ maxWidth: 768 }}
            >
              <SmeltIcon spin={visible()} size={12} color={look().palette.muted} />
              <Text style={{ flex: 1, fontSize: look().type.text_xsmall.size, color: look().palette.text }}>
                {ct("status.retrying", {
                  attempt: retryInfo()?.attempt ?? 1,
                  max: retryInfo()?.max_attempts ?? 1,
                })}{" "}
                {retryInfo()?.error ?? ""}
              </Text>
            </View>
          </View>
        </If>

        <If when={visibleNotice() != null}>
          <View style={{ width: "100%", alignItems: "center", paddingHorizontal: 16, paddingBottom: 6 }}>
            <View
              class="w-full px-3 py-1.5 rounded-xl bg-need/12 border border-border flex-row items-center gap-2"
              style={{ maxWidth: 768 }}
            >
              <Text style={{ flex: 1, fontSize: look().type.text_xsmall.size, color: look().palette.need }}>
                {visibleNotice()?.text ?? ""}
              </Text>
              <SmeltButton
                variant="dismiss"
                size="xs"
                icon="icons/close.svg"
                onPress={() => {
                  const id = visibleNotice()?.id;
                  if (id != null) dismissedNoticeId.set(id);
                }}
              />
            </View>
          </View>
        </If>

        <If when={dead() && !noEntries()}>
          <View style={{ width: "100%", alignItems: "center", paddingHorizontal: 16, paddingBottom: 6 }}>
            <If
              when={!reconnecting()}
              else={
                <View class="w-full px-3.5 py-2 flex-row items-center gap-2.5" style={{ maxWidth: 768 }}>
                  <SmeltIcon spin={visible()} size={14} color={look().palette.muted} />
                  <Text style={{ flex: 1, fontSize: look().type.ui.size, color: look().palette.muted }}>
                    {ct("status.reconnecting")}
                  </Text>
                </View>
              }
            >
              <View
                class="w-full px-3.5 py-2 rounded-xl bg-bad/12 border border-bad flex-row items-center gap-2.5"
                style={{ maxWidth: 768 }}
              >
                <SmeltIcon path="icons/circle-x.svg" size={14} color={look().palette.bad} />
                <Text style={{ flex: 1, fontSize: look().type.ui.size, color: look().palette.text }}>
                  {snapshot()?.end_reason || ct("status.disconnected")}
                </Text>
                <SmeltButton
                  variant="primary"
                  size="sm"
                  label={reconnectOnly() ? ct("action.reconnect") : ct("action.restart")}
                  onPress={restartOrReconnect}
                />
              </View>
            </If>
          </View>
        </If>

        {/* 本轮记忆变更审核卡 */}
        <MemoryChangesCard
          machine={() => props.machine ?? null}
          changes={() => snapshot()?.memory_changes ?? null}
          onOpenAgentMemory={props.onOpenAgentMemory}
        />

        {/* 底部输入栏 */}
        <Composer
          machine={() => props.machine ?? null}
          cwd={cwd}
          visible={visible}
          readOnly={() => Boolean(props.readOnly)}
          turnActive={turnActive}
          dead={dead}
          supportsImage={() => snapshot()?.supports_image !== false}
          supportsCompaction={() => Boolean(snapshot()?.supports_compaction)}
          compacting={() => Boolean(snapshot()?.compacting)}
          fallbackModelLabel={() => agentShortLabel(agentId())}
          model={effectiveModel}
          configs={effectiveConfigs}
          pendingConfigCount={() => pendingConfigs().length}
          availableCommands={() => snapshot()?.available_commands ?? []}
          skills={loadedSkills}
          mcpServers={loadedMcpServers}
          capabilitiesByAgent={capabilitiesByAgent}
          permissions={() => snapshot()?.pending_permissions ?? []}
          permissionSubmitting={permissionSubmitting}
          elicitation={() => snapshot()?.pending_elicitation ?? null}
          plan={() => snapshot()?.plan ?? null}
          pendingPrompts={() => snapshot()?.pending_prompts ?? []}
          queuedSteering={() => snapshot()?.queued_steering ?? []}
          queuedFollowUp={() => snapshot()?.queued_follow_up ?? []}
          subagents={() => snapshot()?.subagents ?? []}
          backgroundTasks={() => snapshot()?.background_tasks ?? []}
          usage={() => snapshot()?.usage ?? null}
          usageBreakdown={() => snapshot()?.usage_breakdown ?? null}
          focusSeq={() => props.focusSeq ?? 0}
          roomy={showEmptyWelcome}
          composerRef={(api) => {
            composerApi = api;
            props.conversationRef?.({ insertText: (text) => api.insert(text) });
          }}
          onSend={(text, imageRefs, followUp) => {
            const payload = followUp
              ? { FollowUp: { text, images: [], delivery_id: null } }
              : { Prompt: { text, images: [], delivery_id: null } };
            sendAction(payload, imageRefs);
            scrollToBottomAfterSend();
          }}
          onCancel={() => sendAction("Cancel")}
          onForceRestart={() => {
            void acpRestart(props.machine ?? null, props.sessionId)
              .catch(() => {})
              .finally(() => reconnectSeq.set(reconnectSeq() + 1));
          }}
          onSelectConfig={handleSelectConfig}
          onCompact={() => sendAction("Compact")}
          onNewSession={props.onNewSession}
          onPermissionSelect={(toolCallId, optionId) => {
            permissionSubmitting.set(true);
            sendAction({ PermissionSelect: { tool_call_id: toolCallId, option_id: optionId } });
          }}
          onElicitChoose={(fieldIx, optIx) =>
            sendAction({ ElicitationChoose: { field_ix: fieldIx, opt_ix: optIx } })
          }
          onElicitText={(fieldIx, value) =>
            sendAction({ ElicitationText: { field_ix: fieldIx, value } })
          }
          onElicitSubmit={() => sendAction("ElicitationSubmit")}
          onElicitDismiss={() => sendAction("ElicitationDismiss")}
          onDeliverQueuedNow={(text) => {
            sendAction({ DeliverQueuedNow: { text } });
            scrollToBottomAfterSend();
          }}
          onWithdrawPendingPrompt={(id) => sendAction({ WithdrawPendingPrompt: { id } })}
          onClearQueue={() => sendAction("ClearQueue")}
          onStopSubagent={(id) => sendAction({ StopSubagent: { id } })}
          onInspectSubagent={(sub) => inspectedSubagent.set({ id: sub.id, title: sub.title })}
          onStopBackgroundTask={(id) => sendAction({ StopBackgroundTask: { id } })}
          onManageCapabilities={
            props.onManageCapabilities
              ? () => props.onManageCapabilities?.(capabilitiesByAgent())
              : undefined
          }
          onPreviewAttachment={(att) => handlePreviewImage(att)}
          onFocusIn={props.onFocusIn}
        />

        {/* 空对话：和上面的标题区等高，把输入框撑在中间 */}
        <If when={showEmptyWelcome()}>
          <View style={{ flexGrow: 1, flexBasis: 0, minHeight: 0 }} />
        </If>
      </View>

      {/* 子智能体详情侧抽屉 */}
      <SubagentDrawer
        inspected={inspectedSubagent}
        entries={() => entriesState().entries}
        subagents={() => snapshot()?.subagents ?? []}
        cwd={cwd}
        machine={() => props.machine ?? null}
        onClose={() => inspectedSubagent.set(null)}
      />

      {/* 文件差异弹窗 */}
      <EditDiffDialog dialog={diffDialog} onClose={() => diffDialog.set(null)} />

      {/* 大图预览弹窗 */}
      <SmeltDialog
        open={imagePreview() != null}
        onOpenChange={({ open }: any) => {
          if (!open) closeImagePreview();
        }}
        class="m-auto"
      >
        <If when={imagePreview() != null}>
          <View
            class={`${DIALOG_SURFACE_CLASS} p-3.5 gap-2.5`}
            style={{
              width: 860,
              maxWidth: "90%",
              height: 620,
              maxHeight: "84%",
            }}
          >
            <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between" }}>
              <Text style={{ fontSize: look().type.small.size, color: look().palette.muted }}>
                {ct("image.preview")}
              </Text>
              <View style={{ flexDirection: "row", alignItems: "center", gap: 4 }}>
                <SmeltButton
                  variant="quiet"
                  size="sm"
                  icon={imagePreviewCopied() ? "icons/check.svg" : "icons/copy.svg"}
                  label={imagePreviewCopied() ? ct("action.copied") : ct("action.copy_image")}
                  onPress={handleCopyPreviewImage}
                />
                <SmeltButton variant="quiet" size="sm" icon="icons/close.svg" onPress={closeImagePreview} />
              </View>
            </View>
            <View style={{ flex: 1, minHeight: 0, alignItems: "center", justifyContent: "center" }}>
              <If when={Boolean(imagePreview()?.uri)}>
                <Image
                  src={imagePreview()!.uri}
                  style={{ width: "100%", height: "100%", objectFit: "contain" }}
                />
              </If>
            </View>
          </View>
        </If>
      </SmeltDialog>
    </View>
  );
}
