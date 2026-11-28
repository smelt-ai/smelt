// 会话插件（smelt.sessions）：中间主区的对话 + 终端二元分屏容器、选中态、活动上下文与 16 条命令。
// 重设计要点：
// - 用 NSH 一等 Rust `<ResizablePanelGroup>` + 纯二元切分树替代旧版嵌套 `gpui-base::DockArea`（砍掉双层子标签）；
// - 活体会话按 `display: none/flex` 常驻，切会话零重连、终端不丢屏；
// - 分屏重构时通过 `smelt.view` 的 `reuseKey` 同帧接管活体 `ViewInstance`，分屏/关格不丢输入草稿与滚动位置；
// - 等待一律事件驱动（目录版本号比对 + 宿主状态推送），无轮询定时器。
import { computed, signal, Text, View } from "nsh";
import { ResizablePanel, ResizablePanelGroup } from "nsh/base";
import {
  bootTask,
  call,
  clipboardWrite,
  command,
  connections,
  conversationFacts,
  currentTheme,
  daemonStateStatus,
  emitEvent,
  initHost,
  machineMeta,
  machines,
  newSessionId,
  onCommand,
  onEvent,
  onSlices,
  pathClean,
  pathIsAbsolute,
  pathJoin,
  pathStartsWith,
  pathStyle,
  registerCommands,
  registerTopics,
  registerViews,
  resolveCliResume,
  resolveHandoffAcp,
  resolveResumeAcp,
  resolveStartAcp,
  run,
  safeArea,
  sessionStates,
  setActiveContext,
  SmeltButton,
  SmeltConfirmDialog,
  SmeltIcon,
  SmeltNativeWindowDrag,
  SmeltWindowBar,
  stableMemo,
  store,
  t,
  text,
  viewShown,
  workspaceDirectory,
} from "smelt/sdk";
import {
  ACTIVATE,
  CLOSE_PANE,
  CONVERSATION_CREATED,
  COPY_SESSION_ID,
  CREATE_CONVERSATION,
  FORK,
  HANDOFF,
  NEXT_PANE,
  OWNER,
  PREV_PANE,
  RESUME,
  RESUME_IN_TERMINAL,
  SELECT_PROJECT,
  SELECTION,
  SELECTION_CHANGED,
  SEND_TEXT,
  SESSION_VIEW,
  SESSION_VIEW_NAME,
  SPLIT_DOWN,
  SPLIT_RIGHT,
  START_CONVERSATION,
} from "./contract.js";
import {
  blankConversation,
  childSafeArea,
  contextRoot,
  conversationOfSession,
  conversationTitle,
  convKeyStr,
  directoryVersion,
  extractMachineSettings,
  findResumedConversation,
  forkedAcp,
  formatResolveError,
  idToCopy,
  leadPane,
  locateSession,
  machineKey,
  paneSpecs,
  paneTitle,
  parseSessionArgs,
  projectListed,
  revisionIsOlder,
  sameMachine,
  selectionToWire,
  shouldOfferAddMachine,
  shownConversation,
  worksetOfCwd,
} from "./model.js";
import {
  cyclePaneId,
  defaultTree,
  encodeTree,
  leafOrder,
  paneTouches,
  reconcileTree,
  restoreTree,
  updateSplitSizes,
} from "./split_tree.js";
import { createSessionsStore } from "./store.js";
import {
  applyAgentSettingsChanged,
  ConversationPane,
  notifyPiConfigChanged,
  sendTextToConversation,
} from "./conversation_pane.tsx";
import { sendTextToTerminal, TerminalPane } from "./terminal_pane.tsx";

// ---- 外部插件约定 ----

const SHOW_MAIN_WINDOW_CMD = "smelt.app/show-main-window";
const OPEN_VIEW_CMD = "smelt.app/open-view";
const NOTIFY_CMD = "smelt.app/notify";
const AGENT_SETTINGS_CMD = "smelt.agents/agent-settings";
const AGENT_SETTINGS_CHANGED_EVENT = "smelt.agents/agent-settings-changed";
const ADD_MACHINE_CMD = "smelt.machines/add-machine";
const REVEAL_IN_LIST_CMD = "smelt.session-list/reveal-in-list";
const TOPIC_PI_CONFIG_CHANGED = "pi_config.changed";

const TOP_BAR_HEIGHT = 34;
const PANE_BAR_HEIGHT = 26;
const CHROME_ISLAND_BAND_PX = 38;
const CHROME_ISLAND_HEIGHT_PX = 28;
const CHROME_ISLAND_TOP_PX = 5;
const TITLE_ISLAND_MAX_WIDTH = 200;

// ---- 数据类型 ----

interface ConversationKey {
  machine: string | null;
  id: string;
}

type SelectionState =
  | { kind: "none" }
  | { kind: "conversation"; machine: string | null; conversation_id: string }
  | {
      kind: "project";
      machine: string | null;
      root: string;
      since: { epoch: string; revision: number } | null;
    };

interface PaneSpec {
  session_id: string;
  kind: "conversation" | "terminal";
  view: string | null;
  args: Record<string, unknown>;
}

interface LiveSession {
  keyStr: string;
  key: ConversationKey;
  tree: ReturnType<typeof signal<any>>;
  latestTree: any;
  activePaneId: ReturnType<typeof signal<string | null>>;
  specs: ReturnType<typeof signal<Map<string, PaneSpec>>>;
  expectedPanes: Map<string, { beside: string | null; axis: "horizontal" | "vertical" }>;
  userEditedLayout: boolean;
  lastSaved: { active_pane: string | null; layout: string | null };
}

interface CloseConfirmState {
  question: string;
  description: string;
  actionLabel: string;
  onConfirm: () => void;
}

// ---- 全局状态（跨 root 与 session 视图共享） ----

// 后台常驻的活体会话上限（含当前选中）：防止长时切换会话导致 DOM、订阅与消息数组无限增长。
const MAX_LIVE_SESSIONS = 4;

const sessionsStore = createSessionsStore(store);
const selection = signal<SelectionState>({ kind: "none" });
const liveKeys = signal<string[]>([]);
const liveSessions = new Map<string, LiveSession>();
const liveLastUsed = new Map<string, number>();
let liveUseSeq = 0;

function touchLiveSession(ks: string) {
  if (liveSessions.has(ks)) {
    liveLastUsed.set(ks, ++liveUseSeq);
  }
}

function evictOverflowLiveSessions(keepKeyStr: string | null) {
  if (liveSessions.size <= MAX_LIVE_SESSIONS) return false;
  const sel = selection();
  const selectedKey =
    sel.kind === "conversation" ? convKeyStr(sel.machine, sel.conversation_id) : null;
  let evicted = false;
  while (liveSessions.size > MAX_LIVE_SESSIONS) {
    let victim: string | null = null;
    let oldest = Infinity;
    for (const ks of liveSessions.keys()) {
      if (ks === keepKeyStr || ks === selectedKey) continue;
      const used = liveLastUsed.get(ks) ?? 0;
      if (used < oldest) {
        oldest = used;
        victim = ks;
      }
    }
    if (!victim) break;
    liveSessions.delete(victim);
    liveLastUsed.delete(victim);
    evicted = true;
  }
  return evicted;
}
const focusPulse = signal<{ keyStr: string; paneId: string; seq: number }>({
  keyStr: "",
  paneId: "",
  seq: 0,
});
let nextFocusSeq = 1;

const failureBanner = signal<string | null>(null);
const addMachineError = signal<string | null>(null);
const closeConfirm = signal<CloseConfirmState | null>(null);

const pendingStarts = new Map<string, { fork: any; prefill: string }>();
const seenVersions = new Map<string, string>();
let knownMachines = new Set<string>();
const behindSessions = new Set<string>();
let waitingOpen: {
  machine: string | null;
  at: { session_id?: string; conversation_id?: string };
  after: { epoch: string; revision: number } | null;
} | null = null;
let lastPublishedContextJson = "";
let initializedStore = false;


function newAcpId(): string {
  return `acp-${newSessionId()}`;
}

function errorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err ?? "");
}

function notifyError(message: string) {
  console.error(`[sessions] ${message}`);
  run(NOTIFY_CMD, {
    subtitle: t("error.title"),
    body: message,
  }).catch(() => {});
}

function setFailure(whatKey: string, err: unknown) {
  const message = t("error.failed", { what: t(whatKey), error: errorMessage(err) });
  console.error(`[sessions] ${message}`);
  failureBanner.set(message);
}

function clearFailure() {
  if (failureBanner() != null) failureBanner.set(null);
}

function pathOpsFor(machine: string | null) {
  const style = pathStyle(machine);
  return {
    style,
    clean: (p: string) => pathClean(style, p),
    join: (a: string, b: string) => pathJoin(style, a, b),
    isAbsolute: (p: string) => pathIsAbsolute(style, p),
    startsWith: (p: string, base: string) => pathStartsWith(style, p, base),
  };
}

function updateConversationAtLatest(
  machine: string | null,
  sessionId: string,
  atLatest: boolean,
) {
  const bk = convKeyStr(machine ?? null, sessionId);
  const changed = atLatest
    ? behindSessions.delete(bk)
    : !behindSessions.has(bk) && Boolean(behindSessions.add(bk));
  if (changed) publishContext();
}

// ---- 选中、活动上下文与事件发布 ----

function announceSelection() {
  emitEvent(SELECTION_CHANGED, selectionToWire(selection())).catch(() => {});
}

function activePaneFor(key: ConversationKey): { session_id: string; kind: "conversation" | "terminal" } | null {
  const live = liveSessions.get(convKeyStr(key.machine, key.id));
  if (!live) return null;
  const activeId = live.activePaneId();
  if (!activeId) return null;
  const spec = live.specs().get(activeId);
  if (!spec) return null;
  return { session_id: spec.session_id, kind: spec.kind };
}

function currentActivePane(): {
  key: ConversationKey;
  pane: { session_id: string; kind: "conversation" | "terminal" };
} | null {
  const sel = selection();
  if (sel.kind !== "conversation") return null;
  const key: ConversationKey = { machine: sel.machine, id: sel.conversation_id };
  const pane = activePaneFor(key);
  if (pane) return { key, pane };
  const dir = workspaceDirectory(key.machine);
  const conv = (dir?.conversations ?? []).find((c: any) => c.id === key.id);
  const lead = conv ? leadPane(conv) : null;
  return lead ? { key, pane: lead } : null;
}

function computeActiveContext() {
  const sel = selection();
  if (sel.kind === "none") {
    return { project: null, session: null };
  }
  if (sel.kind === "project") {
    return {
      project: { machine: sel.machine ?? null, root: sel.root },
      session: null,
    };
  }
  const key: ConversationKey = { machine: sel.machine, id: sel.conversation_id };
  const dir = workspaceDirectory(key.machine);
  const conv = (dir?.conversations ?? []).find((c: any) => c.id === key.id);
  if (!dir || !conv) {
    return { project: null, session: null };
  }
  const facts = conversationFacts(key.machine);
  const fact = facts.find((f: any) => f.id === key.id);
  const ops = pathOpsFor(key.machine);
  const root = contextRoot(dir, fact, ops.join);
  const project = root ? { machine: key.machine ?? null, root } : null;

  let session: { machine: string | null; id: string; at_latest: boolean } | null = null;
  if (viewShown(SESSION_VIEW)) {
    const pane = activePaneFor(key) ?? leadPane(conv);
    if (pane) {
      const behindKey = convKeyStr(key.machine, pane.session_id);
      session = {
        machine: key.machine ?? null,
        id: pane.session_id,
        at_latest: pane.kind === "terminal" || !behindSessions.has(behindKey),
      };
    }
  }
  return { project, session };
}

function publishContext() {
  const ctx = computeActiveContext();
  const nextJson = JSON.stringify(ctx);
  if (nextJson === lastPublishedContextJson) return;
  lastPublishedContextJson = nextJson;
  setActiveContext(ctx).catch(() => {});
}

function selectConversation(key: ConversationKey) {
  const cur = selection();
  const changed =
    cur.kind !== "conversation" ||
    !sameMachine(cur.machine, key.machine) ||
    cur.conversation_id !== key.id;
  if (changed) {
    selection.set({
      kind: "conversation",
      machine: key.machine ?? null,
      conversation_id: key.id,
    });
    void sessionsStore.saveSelection(key);
  }
  const ks = convKeyStr(key.machine, key.id);
  touchLiveSession(ks);
  if (evictOverflowLiveSessions(ks)) {
    liveKeys.set([...liveSessions.keys()]);
  }
  // 即使选中的是同一段会话，也发一次通知（可能项目被折起，列表需展开露出）。
  announceSelection();
  publishContext();
}

function selectProject(
  machine: string | null,
  rawRoot: string,
  since: { epoch: string; revision: number } | null,
) {
  const ops = pathOpsFor(machine);
  const root = ops.clean(String(rawRoot ?? "").trim());
  if (!root) {
    throw new Error(t("error.no_project_dir"));
  }
  const dir = workspaceDirectory(machine);
  const facts = conversationFacts(machine);
  const caughtUp = !dir
    ? since == null
    : since == null || !revisionIsOlder(directoryVersion(dir), since);
  const listed = Boolean(dir) && projectListed(dir, facts, root, ops.clean);
  if (caughtUp && !listed) {
    throw new Error(t("error.project_not_listed"));
  }
  if (selection().kind === "conversation") {
    void sessionsStore.saveSelection(null);
  }
  selection.set({
    kind: "project",
    machine: machine ?? null,
    root,
    since,
  });
  announceSelection();
  publishContext();
}

// ---- 活体会话管理（分屏树与持久化） ----

function persistLiveLayout(live: LiveSession) {
  const tree = live.latestTree;
  const nextSaved = {
    active_pane: live.activePaneId(),
    layout: tree ? encodeTree(tree) : null,
  };
  if (
    nextSaved.active_pane === live.lastSaved.active_pane &&
    nextSaved.layout === live.lastSaved.layout
  ) {
    return;
  }
  live.lastSaved = nextSaved;
  void sessionsStore.saveLayout(live.key, nextSaved);
}

function requestFocusPane(keyStr: string, paneId: string) {
  const seq = nextFocusSeq++;
  focusPulse.set({ keyStr, paneId, seq });
}

function setLiveActivePane(live: LiveSession, paneId: string, shouldFocus = false) {
  if (!live.specs().has(paneId)) return;
  const changed = live.activePaneId() !== paneId;
  if (changed) {
    live.activePaneId.set(paneId);
    persistLiveLayout(live);
    publishContext();
  }
  if (shouldFocus) {
    requestFocusPane(live.keyStr, paneId);
  }
}

function ensureLiveSession(key: ConversationKey, conv: any): LiveSession {
  const ks = convKeyStr(key.machine, key.id);
  const existing = liveSessions.get(ks);
  if (existing) {
    touchLiveSession(ks);
    return existing;
  }

  const start = pendingStarts.get(ks) ?? null;
  pendingStarts.delete(ks);

  const initialSpecs = paneSpecs(key.machine, conv, start);
  const specMap = new Map<string, PaneSpec>();
  const ids: string[] = [];
  for (const s of initialSpecs) {
    specMap.set(s.session_id, s);
    ids.push(s.session_id);
  }
  const initialTree = defaultTree(ids);
  const initialActive = ids[0] ?? null;

  const live: LiveSession = {
    keyStr: ks,
    key,
    tree: signal(initialTree),
    latestTree: initialTree,
    activePaneId: signal<string | null>(initialActive),
    specs: signal(specMap),
    expectedPanes: new Map(),
    userEditedLayout: false,
    lastSaved: {
      active_pane: initialActive,
      layout: initialTree ? encodeTree(initialTree) : null,
    },
  };
  liveSessions.set(ks, live);
  liveLastUsed.set(ks, ++liveUseSeq);
  evictOverflowLiveSessions(ks);
  liveKeys.set([...liveSessions.keys()]);

  // 异步回读 SQLite 存档分屏（若用户还没在内存里手动改过分屏，则恢复上次布局与焦点格）。
  void sessionsStore.readLayout(key).then((saved) => {
    if (!saved || liveSessions.get(ks) !== live || live.userEditedLayout) return;
    const curIds = [...live.specs().keys()];
    if (curIds.length === 0) return;
    const restored = restoreTree(saved.layout, curIds);
    live.latestTree = restored;
    live.tree.set(restored);
    const restoredActive =
      saved.active_pane && live.specs().has(saved.active_pane)
        ? saved.active_pane
        : leafOrder(restored)[0] ?? null;
    live.activePaneId.set(restoredActive);
    live.lastSaved = {
      active_pane: restoredActive,
      layout: restored ? encodeTree(restored) : null,
    };
    publishContext();
  });

  return live;
}

function syncLiveSession(live: LiveSession, conv: any) {
  const freshSpecs = paneSpecs(live.key.machine, conv, null);
  const oldSpecs = live.specs();
  const nextSpecs = new Map<string, PaneSpec>();
  const ids: string[] = [];
  let arrivedId: string | null = null;
  let specsChanged = freshSpecs.length !== oldSpecs.size;

  for (const spec of freshSpecs) {
    ids.push(spec.session_id);
    const existing = oldSpecs.get(spec.session_id);
    if (existing) {
      // 保留首次建实例时的冻结 args，避免切标题或目录刷新重建底层 ViewInstance。
      nextSpecs.set(spec.session_id, existing);
    } else {
      specsChanged = true;
      nextSpecs.set(spec.session_id, spec);
      if (live.expectedPanes.has(spec.session_id)) {
        arrivedId = spec.session_id;
      }
    }
  }
  if (specsChanged) {
    live.specs.set(nextSpecs);
  }

  const prevTree = live.latestTree;
  const nextTree = reconcileTree(prevTree, ids, live.expectedPanes);
  for (const id of ids) {
    live.expectedPanes.delete(id);
  }
  if (JSON.stringify(nextTree) !== JSON.stringify(prevTree)) {
    live.latestTree = nextTree;
    live.tree.set(nextTree);
  }

  const order = leafOrder(nextTree);
  const curActive = live.activePaneId();
  if (!curActive || !order.includes(curActive)) {
    const fallback = order[0] ?? null;
    live.activePaneId.set(fallback);
  }

  persistLiveLayout(live);

  if (arrivedId) {
    const sel = selection();
    const isSelected =
      sel.kind === "conversation" &&
      sameMachine(sel.machine, live.key.machine) &&
      sel.conversation_id === live.key.id;
    setLiveActivePane(live, arrivedId, isSelected);
  }
}

// ---- 打开 / 跳到会话 ----

function revealSessionArgs(rawArgs: unknown) {
  if (rawArgs == null) {
    syncFromHost();
    return;
  }
  const parsed = parseSessionArgs(rawArgs);
  if (!parsed) {
    notifyError(t("error.bad_open_args", { error: "invalid SessionArgs" }));
    return;
  }
  revealParsed(parsed);
}

function revealParsed(args: {
  machine: string | null;
  at: { session_id?: string; conversation_id?: string };
  after: { epoch: string; revision: number } | null;
}) {
  const dir = workspaceDirectory(args.machine);
  const facts = conversationFacts(args.machine);
  const loc = locateSession(args.machine, args.at, args.after, dir, facts);
  if (loc.kind === "wait") {
    waitingOpen = args;
    return;
  }
  if (loc.kind === "missing") {
    waitingOpen = null;
    notifyError(t("error.session_gone"));
    return;
  }
  waitingOpen = null;
  const key: ConversationKey = { machine: loc.machine, id: loc.id };
  selectConversation(key);
  syncFromHost();
  const live = liveSessions.get(convKeyStr(key.machine, key.id));
  if (live) {
    const targetPane = args.at.session_id ?? live.activePaneId() ?? leafOrder(live.tree())[0];
    if (targetPane) {
      setLiveActivePane(live, targetPane, true);
    }
  }
}

async function openSessionView(args: Record<string, unknown>) {
  await run(SHOW_MAIN_WINDOW_CMD, null);
  const ok = await run(OPEN_VIEW_CMD, { view: SESSION_VIEW, args });
  if (!ok) {
    throw new Error(t("error.no_room"));
  }
}

async function createAndOpenConversation(
  machine: string | null,
  conversation: any,
  start: { fork: any; prefill: string } | null,
): Promise<ConversationKey> {
  const key: ConversationKey = { machine: machine ?? null, id: conversation.id };
  const ks = convKeyStr(key.machine, key.id);
  if (start) {
    pendingStarts.set(ks, start);
  }
  emitEvent(CONVERSATION_CREATED, {
    machine: key.machine,
    conversation_id: key.id,
  }).catch(() => {});

  let revision: any;
  try {
    revision = await call(key.machine, "workspace.conversation.create", conversation);
  } catch (err) {
    pendingStarts.delete(ks);
    throw err;
  }
  await openSessionView({
    machine: key.machine,
    conversation_id: key.id,
    after: revision ?? undefined,
  });
  return key;
}

// ---- 宿主状态同步（纯事件驱动） ----

function syncFromHost() {
  const list = machineMeta();
  const currentMachines = new Set<string>(list.map((m: any) => machineKey(m.machine)));

  // 1. 清理被彻底移除的机器
  let selectionCleared = false;
  for (const prev of knownMachines) {
    if (currentMachines.has(prev)) continue;
    const mKey = prev === "" ? null : prev;
    void sessionsStore.forgetMachine(mKey);
    for (const ks of [...pendingStarts.keys()]) {
      if (ks.startsWith(`${prev}\0`)) pendingStarts.delete(ks);
    }
    for (const bk of [...behindSessions]) {
      if (bk.startsWith(`${prev}\0`)) behindSessions.delete(bk);
    }
    seenVersions.delete(prev);
    const sel = selection();
    if (sel.kind !== "none" && machineKey(sel.machine) === prev) {
      selection.set({ kind: "none" });
      selectionCleared = true;
    }
  }
  knownMachines = currentMachines;

  // 2. 按机器目录版本对账（清理库 + 核对选中 + 清理 behind）
  for (const entry of list) {
    const machine: string | null = entry.machine ?? null;
    const mk = machineKey(machine);
    const dir = workspaceDirectory(machine);
    if (!dir) continue;
    const facts = conversationFacts(machine);
    const ver = `${dir.epoch ?? ""}:${dir.revision ?? 0}`;
    if (seenVersions.get(mk) !== ver) {
      seenVersions.set(mk, ver);
      const knownIds = (dir.conversations ?? []).map((c: any) => c.id);
      void sessionsStore.prune(machine, knownIds);

      const sel = selection();
      let gone = false;
      if (sel.kind === "conversation" && sameMachine(sel.machine, machine)) {
        gone = !shownConversation(dir, facts, sel.conversation_id);
      } else if (sel.kind === "project" && sameMachine(sel.machine, machine)) {
        const caughtUp = !sel.since || !revisionIsOlder(directoryVersion(dir), sel.since);
        const ops = pathOpsFor(machine);
        gone = caughtUp && !projectListed(dir, facts, sel.root, ops.clean);
      }
      if (gone) {
        if (sel.kind === "conversation") {
          void sessionsStore.saveSelection(null);
        }
        selection.set({ kind: "none" });
        selectionCleared = true;
      }

      const liveSessionIds = new Set<string>();
      for (const conv of dir.conversations ?? []) {
        if (conv.acp?.sid) liveSessionIds.add(conv.acp.sid);
        for (const term of conv.terminals ?? []) {
          if (term?.id) liveSessionIds.add(term.id);
        }
      }
      for (const bk of [...behindSessions]) {
        if (bk.startsWith(`${mk}\0`) && !liveSessionIds.has(bk.slice(mk.length + 1))) {
          behindSessions.delete(bk);
        }
      }
    }
  }

  // 3. 同步所有已建好的活体会话，扔掉目录中没了或机器移除的
  let liveListChanged = false;
  for (const [ks, live] of [...liveSessions.entries()]) {
    const mk = machineKey(live.key.machine);
    if (!currentMachines.has(mk)) {
      liveSessions.delete(ks);
      liveLastUsed.delete(ks);
      liveListChanged = true;
      continue;
    }
    const dir = workspaceDirectory(live.key.machine);
    if (!dir) continue;
    const facts = conversationFacts(live.key.machine);
    const conv = shownConversation(dir, facts, live.key.id);
    if (!conv) {
      liveSessions.delete(ks);
      liveLastUsed.delete(ks);
      liveListChanged = true;
    } else {
      syncLiveSession(live, conv);
    }
  }
  if (liveListChanged) {
    liveKeys.set([...liveSessions.keys()]);
  }

  // 4. 兑现等待目录推到的打开请求
  if (waitingOpen) {
    const pending = waitingOpen;
    waitingOpen = null;
    revealParsed(pending);
  }

  // 5. 若当前选中会话还没建活体，现在补建
  const sel = selection();
  if (sel.kind === "conversation") {
    const key: ConversationKey = { machine: sel.machine, id: sel.conversation_id };
    const ks = convKeyStr(key.machine, key.id);
    if (!liveSessions.has(ks)) {
      const dir = workspaceDirectory(key.machine);
      const facts = conversationFacts(key.machine);
      const conv = dir ? shownConversation(dir, facts, key.id) : null;
      if (conv) {
        ensureLiveSession(key, conv);
      }
    }
  }

  if (selectionCleared) {
    announceSelection();
  }
  publishContext();
}

// ---- 分屏命令与关闭逻辑 ----

function requireSelectedLive(): { key: ConversationKey; live: LiveSession; conv: any } {
  const sel = selection();
  if (sel.kind !== "conversation") {
    throw new Error(t("error.no_selection"));
  }
  const key: ConversationKey = { machine: sel.machine, id: sel.conversation_id };
  const live = liveSessions.get(convKeyStr(key.machine, key.id));
  if (!live) {
    throw new Error(t("error.not_open"));
  }
  const dir = workspaceDirectory(key.machine);
  const conv = (dir?.conversations ?? []).find((c: any) => c.id === key.id);
  if (!conv) {
    throw new Error(t("error.not_in_directory"));
  }
  return { key, live, conv };
}

function splitCurrentPane(axis: "horizontal" | "vertical") {
  const { key, live, conv } = requireSelectedLive();
  const activeId = live.activePaneId();
  const states = sessionStates(key.machine);
  const facts = conversationFacts(key.machine);
  const fact = facts.find((f: any) => f.id === key.id);
  const paneCwd = activeId
    ? states.find((s: any) => s.id === activeId)?.cwd ?? null
    : null;
  const cwd = paneCwd ?? fact?.cwd ?? null;
  const sessionId = newSessionId();

  const updatedConv = {
    ...conv,
    terminals: [
      ...(conv.terminals ?? []),
      {
        id: sessionId,
        cwd,
        custom_title: null,
        launch_label: null,
        launch_cmd: null,
      },
    ],
  };

  live.userEditedLayout = true;
  live.expectedPanes.set(sessionId, { beside: activeId, axis });
  clearFailure();

  void call(key.machine, "workspace.conversation.update", updatedConv).catch((err) => {
    live.expectedPanes.delete(sessionId);
    setFailure("action.split", err);
  });
}

function cycleCurrentPane(forward: boolean) {
  const { live } = requireSelectedLive();
  const order = leafOrder(live.tree());
  const nextId = cyclePaneId(order, live.activePaneId(), forward);
  if (nextId) {
    setLiveActivePane(live, nextId, true);
  }
}

function closeTerminalInDirectory(key: ConversationKey, sessionId: string) {
  clearFailure();
  const dir = workspaceDirectory(key.machine);
  const conv = (dir?.conversations ?? []).find((c: any) => c.id === key.id);
  if (!conv) {
    setFailure("action.close_pane", t("error.not_in_directory"));
    return;
  }
  const nextTerminals = (conv.terminals ?? []).filter((term: any) => term?.id !== sessionId);
  if (nextTerminals.length === (conv.terminals ?? []).length) return;
  const live = liveSessions.get(convKeyStr(key.machine, key.id));
  if (live) live.userEditedLayout = true;
  void call(key.machine, "workspace.conversation.update", {
    ...conv,
    terminals: nextTerminals,
  }).catch((err) => setFailure("action.close_pane", err));
}

function closeWholeSession(key: ConversationKey) {
  clearFailure();
  void call(key.machine, "workspace.conversation.delete", {
    conversation_id: key.id,
  }).catch((err) => setFailure("action.close_session", err));
}

function closePaneById(targetPaneId?: string) {
  const { key, live, conv } = requireSelectedLive();
  const order = leafOrder(live.tree());
  const paneId = targetPaneId ?? live.activePaneId();
  const spec = paneId ? live.specs().get(paneId) ?? null : null;
  const many = order.length > 1;
  const states = sessionStates(key.machine);
  const facts = conversationFacts(key.machine);
  const fact = facts.find((f: any) => f.id === key.id);

  if (many && spec && spec.kind === "terminal") {
    const st = states.find((s: any) => s.id === spec.session_id);
    const running = st ? daemonStateStatus(st) === "running" : false;
    const title = paneTitle(conv, "terminal", spec.session_id, states, t);
    if (!running) {
      closeTerminalInDirectory(key, spec.session_id);
      return;
    }
    closeConfirm.set({
      question: t("close.pane_running"),
      description: t("close.running_description", { title }),
      actionLabel: t("close.pane"),
      onConfirm: () => closeTerminalInDirectory(key, spec.session_id),
    });
    return;
  }

  const running = fact?.status === "running";
  const title = conversationTitle(conv, fact, t);
  if (!running) {
    closeWholeSession(key);
    return;
  }
  closeConfirm.set({
    question: t("close.session_running"),
    description: t("close.running_description", { title }),
    actionLabel: t("close.session"),
    onConfirm: () => closeWholeSession(key),
  });
}

// ---- 其余 10 条带参/功能命令 ----

async function fetchMachineSettings(machine: string | null) {
  const all = await run(AGENT_SETTINGS_CMD, null);
  const parsed = extractMachineSettings(all, machine, t);
  if (!parsed.ok) {
    throw new Error(parsed.error);
  }
  return parsed.settings;
}

function worksetFor(machine: string | null, cwd: string | null): string | null {
  if (!cwd) return null;
  const dir = workspaceDirectory(machine);
  if (!dir?.worksets?.length) return null;
  return worksetOfCwd(dir.worksets, cwd, pathOpsFor(machine));
}

async function handleCommand(name: string, args: any): Promise<any> {
  switch (name) {
    case "selection":
      return selectionToWire(selection());

    case "activate":
      await openSessionView({
        machine: args?.machine ?? null,
        session_id: args?.session_id,
        after: args?.after ?? undefined,
      });
      return null;

    case "create-conversation": {
      const machine: string | null = args?.machine ?? null;
      const conversation = args?.conversation;
      const start = conversation?.acp ? { fork: null, prefill: "" } : null;
      await createAndOpenConversation(machine, conversation, start);
      return null;
    }

    case "select-project": {
      selectProject(args?.machine ?? null, args?.root ?? "", args?.after ?? null);
      return null;
    }

    case "start-conversation": {
      const machine: string | null = args?.machine ?? null;
      const cwd: string = args?.cwd ?? "";
      const withSpec =
        args?.agent_definition_id != null
          ? { agent_definition_id: args.agent_definition_id }
          : { agent: args?.agent, profile_id: args?.profile_id ?? null };
      const settings = await fetchMachineSettings(machine);
      const res = resolveStartAcp(settings, withSpec, cwd, newAcpId());
      if (!res.ok) throw new Error(formatResolveError(res, t));
      const conv = {
        ...blankConversation(newSessionId(), worksetFor(machine, res.acp.cwd ?? null)),
        acp: res.acp,
      };
      await createAndOpenConversation(machine, conv, { fork: null, prefill: "" });
      return null;
    }

    case "fork": {
      const machine: string | null = args?.machine ?? null;
      const dir = workspaceDirectory(machine);
      const source = dir ? conversationOfSession(dir, args?.session_id) : null;
      if (!source) throw new Error(t("error.fork_source_missing"));
      if (!source.acp || source.acp.sid !== args?.session_id) {
        throw new Error(t("error.fork_only_conversation"));
      }
      const conv = {
        ...blankConversation(newSessionId(), source.workset_id ?? null),
        acp: forkedAcp(source.acp, newAcpId()),
      };
      const start = {
        fork: {
          provider_session_id: args?.provider_session_id ?? "",
          cut: args?.cut ?? null,
        },
        prefill: args?.prompt ?? "",
      };
      await createAndOpenConversation(machine, conv, start);
      return null;
    }

    case "resume": {
      const machine: string | null = args?.machine ?? null;
      const dir = workspaceDirectory(machine);
      const facts = conversationFacts(machine);
      const ops = pathOpsFor(machine);
      // 若侧栏里已有同一段历史会话，直接跳过去。
      if (dir) {
        const existingId = findResumedConversation(
          dir,
          facts,
          args?.agent,
          args?.cwd ?? "",
          args?.resume_id ?? "",
          ops.clean,
        );
        if (existingId) {
          await openSessionView({ machine, conversation_id: existingId });
          return null;
        }
      }
      const settings = await fetchMachineSettings(machine);
      const res = resolveResumeAcp(settings, args, newAcpId());
      if (!res.ok) throw new Error(formatResolveError(res, t));
      const conv = {
        ...blankConversation(newSessionId(), worksetFor(machine, res.acp.cwd ?? null)),
        acp: res.acp,
      };
      await createAndOpenConversation(machine, conv, { fork: null, prefill: "" });
      return null;
    }

    case "resume-in-terminal": {
      const machine: string | null = args?.machine ?? null;
      const [launchResp, settings] = await Promise.all([
        call(machine, "launch.get", {}),
        args?.profile_id ? fetchMachineSettings(machine) : Promise.resolve(null),
      ]);
      const entries = launchResp?.entries ?? [];
      const res = resolveCliResume(
        pathStyle(machine),
        entries,
        settings,
        args?.agent ?? "",
        args?.profile_id ?? null,
        args?.resume_id ?? "",
      );
      if (!res.ok) throw new Error(formatResolveError(res, t));
      const conv = {
        ...blankConversation(newSessionId(), worksetFor(machine, args?.cwd ?? null)),
        terminals: [
          {
            id: newSessionId(),
            cwd: args?.cwd ?? null,
            custom_title: null,
            launch_label: res.label,
            launch_cmd: res.command,
          },
        ],
      };
      await createAndOpenConversation(machine, conv, null);
      return null;
    }

    case "handoff": {
      const machine: string | null = args?.machine ?? null;
      const settings = await fetchMachineSettings(machine);
      const res = resolveHandoffAcp(settings, args, newAcpId());
      if (!res.ok) throw new Error(formatResolveError(res, t));
      const conv = {
        ...blankConversation(newSessionId(), worksetFor(machine, res.acp.cwd ?? null)),
        acp: res.acp,
      };
      await createAndOpenConversation(machine, conv, { fork: null, prefill: "" });
      return null;
    }

    case "send-text": {
      const active = currentActivePane();
      if (!active) throw new Error(t("error.send_no_session"));
      if (active.pane.kind === "terminal") {
        sendTextToTerminal(active.key.machine, active.pane.session_id, args?.text ?? "");
      } else {
        sendTextToConversation(active.key.machine, active.pane.session_id, args?.text ?? "");
      }
      return null;
    }

    case "prev-pane":
      cycleCurrentPane(false);
      return null;

    case "next-pane":
      cycleCurrentPane(true);
      return null;

    case "split-right":
      splitCurrentPane("horizontal");
      return null;

    case "split-down":
      splitCurrentPane("vertical");
      return null;

    case "close-pane":
      closePaneById();
      return null;

    case "copy-session-id": {
      const active = currentActivePane();
      if (!active) throw new Error(t("error.no_open_session"));
      const dir = workspaceDirectory(active.key.machine);
      const states = sessionStates(active.key.machine);
      const copied = idToCopy(dir, states, active.pane.session_id);
      await clipboardWrite(copied);
      return null;
    }

    default:
      throw new Error(`unknown command: ${name}`);
  }
}

// ---- 启动与登记 ----

onCommand(({ name, args }) => handleCommand(name, args));

onSlices(["machines_meta", "sessions_data"], () => {
  syncFromHost();
});

onEvent(
  (event: any) => {
    if (!event) return;
    if (event.type === "view_opened" && event.view === SESSION_VIEW) {
      revealSessionArgs(event.args);
      return;
    }
    if (event.type === "plugin" && event.name === AGENT_SETTINGS_CHANGED_EVENT) {
      applyAgentSettingsChanged(event.payload);
      return;
    }
    if (
      event.type === "runtime_topic" &&
      event.topic === TOPIC_PI_CONFIG_CHANGED &&
      event.message?.kind !== "unavailable"
    ) {
      notifyPiConfigChanged();
    }
  },
  ["view_opened", `plugin:${AGENT_SETTINGS_CHANGED_EVENT}`, "runtime_topic"],
);

bootTask(async () => {
  await initHost(["machines_meta", "sessions_data", "connections", "views", "safe_areas"]);
  knownMachines = new Set(machineMeta().map((m: any) => machineKey(m.machine)));

  await Promise.all([
    registerTopics([TOPIC_PI_CONFIG_CHANGED]),
    registerViews([
      {
        name: SESSION_VIEW_NAME,
        export: SESSION_VIEW_NAME,
        title: text("view.session"),
        icon: "icons/app-window.svg",
        draws_top_bar: true,
      },
    ]),
    registerCommands(
      [
        {
          name: "activate",
          title: text("command.activate"),
          requires_args: true,
        },
        {
          name: "start-conversation",
          title: text("command.start_conversation"),
          requires_args: true,
        },
        {
          name: "fork",
          title: text("command.fork"),
          requires_args: true,
        },
        {
          name: "resume",
          title: text("command.resume"),
          requires_args: true,
        },
        {
          name: "resume-in-terminal",
          title: text("command.resume_in_terminal"),
          requires_args: true,
        },
        {
          name: "handoff",
          title: text("command.handoff"),
          requires_args: true,
        },
        {
          name: "send-text",
          title: text("command.send_text"),
          requires_args: true,
        },
        {
          name: "selection",
          title: text("command.selection"),
          requires_args: false,
        },
        {
          name: "create-conversation",
          title: text("command.create_conversation"),
          requires_args: true,
        },
        {
          name: "select-project",
          title: text("command.select_project"),
          requires_args: true,
        },
        {
          name: "prev-pane",
          title: text("command.prev_pane"),
          key: "cmd-[",
          context: "Workspace",
          requires_args: false,
          args: [],
          when_view_shown: SESSION_VIEW,
          when_active_session: true,
        },
        {
          name: "next-pane",
          title: text("command.next_pane"),
          key: "cmd-]",
          context: "Workspace",
          requires_args: false,
          args: [],
          when_view_shown: SESSION_VIEW,
          when_active_session: true,
        },
        {
          name: "split-right",
          title: text("command.split_right"),
          key: "cmd-d",
          context: "Workspace",
          requires_args: false,
          args: [],
          when_view_shown: SESSION_VIEW,
          when_active_session: true,
        },
        {
          name: "split-down",
          title: text("command.split_down"),
          key: "shift-cmd-d",
          context: "Workspace",
          requires_args: false,
          args: [],
          when_view_shown: SESSION_VIEW,
          when_active_session: true,
        },
        {
          name: "close-pane",
          title: text("command.close_pane"),
          key: "cmd-w",
          context: "Workspace",
          requires_args: false,
          args: [],
          when_view_shown: SESSION_VIEW,
          when_active_session: true,
        },
        {
          name: "copy-session-id",
          title: text("menu.copy_session_id"),
          context: "Workspace",
          requires_args: false,
          args: [],
        },
      ],
      text("view.session"),
    ),
    (async () => {
      try {
        await sessionsStore.open();
        if (!initializedStore) {
          initializedStore = true;
          const saved = await sessionsStore.readSelection();
          if (saved && selection().kind === "none") {
            selection.set({
              kind: "conversation",
              machine: saved.machine,
              conversation_id: saved.id,
            });
            announceSelection();
          }
        }
      } catch (err) {
        console.error("[sessions] 打开 SQLite 存储失败：", err);
      }
    })(),
  ]);
  syncFromHost();
});

// ---- UI 组件 ----

function SplitLeafPane(props: {
  live: LiveSession;
  paneId: string;
  visible: () => boolean;
  many: () => boolean;
  touches: () => { top: boolean; right: boolean; bottom: boolean; left: boolean };
  parentArea: () => any;
}) {
  const look = currentTheme;
  const spec = computed(() => props.live.specs().get(props.paneId) ?? null);
  const isActive = computed(() => props.live.activePaneId() === props.paneId);
  const title = computed(() => {
    const s = spec();
    if (!s) return "";
    const dir = workspaceDirectory(props.live.key.machine);
    const conv = (dir?.conversations ?? []).find((c: any) => c.id === props.live.key.id);
    const states = sessionStates(props.live.key.machine);
    return paneTitle(conv, s.kind, s.session_id, states, t);
  });
  const groupArea = stableMemo(() => childSafeArea(props.parentArea(), props.touches()));
  const contentArea = stableMemo(() => {
    const base = groupArea();
    if (!props.many()) return base;
    return {
      ...base,
      title_bar: false,
      top_left: 0,
      top_right: 0,
    };
  });
  const paneFocusSeq = computed(() => {
    const p = focusPulse();
    return p.keyStr === props.live.keyStr && p.paneId === props.paneId ? p.seq : 0;
  });
  const reuseKey = computed(() => `${props.live.keyStr}\0${props.paneId}`);
  const barHeight = computed(() => (groupArea().title_bar ? TOP_BAR_HEIGHT : PANE_BAR_HEIGHT));

  return (
    <View
      style={{
        display: "flex",
        flexDirection: "column",
        flex: 1,
        width: "100%",
        height: "100%",
        minWidth: 0,
        minHeight: 0,
      }}
    >
      <If when={props.many()}>
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            height: barHeight(),
            flexShrink: 0,
            paddingLeft: Math.max(8, groupArea().top_left),
            paddingRight: Math.max(6, groupArea().top_right),
            gap: 6,
            borderBottomWidth: 1,
            borderBottomColor: look().palette.hair,
          }}
        >
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: isActive() ? look().palette.muted : look().palette.faint,
              numberOfLines: 1,
              flexShrink: 1,
              minWidth: 0,
            }}
          >
            {title()}
          </Text>
          <View style={{ flex: 1, height: "100%", minWidth: 8 }}>
            <If when={groupArea().title_bar}>
              <SmeltNativeWindowDrag style={{ width: "100%", height: "100%" }} />
            </If>
          </View>
          <SmeltButton
            variant="dismiss"
            size="xs"
            icon="icons/close.svg"
            tooltip={t("close.pane")}
            onPress={() => {
              setLiveActivePane(props.live, props.paneId, false);
              closePaneById(props.paneId);
            }}
          />
        </View>
      </If>
      <View
        style={{
          position: "relative",
          flex: 1,
          width: "100%",
          height: "100%",
          minWidth: 0,
          minHeight: 0,
          display: "flex",
          flexDirection: "column",
          overflow: "hidden",
        }}
      >
        <If
          when={spec()?.kind === "terminal"}
          else={
            <ConversationPane
              args={(spec()?.args as any) ?? null}
              visible={props.visible()}
              many={props.many()}
              safeArea={contentArea()}
              reuseKey={reuseKey()}
              focusSeq={paneFocusSeq()}
              onFocusIn={() => setLiveActivePane(props.live, props.paneId, false)}
              onAtLatestChange={updateConversationAtLatest}
              onFork={(machine, sessionId, req) => {
                void handleCommand("fork", {
                  machine,
                  session_id: sessionId,
                  provider_session_id: req.provider_session_id,
                  cut: req.cut,
                  prompt: req.prompt,
                }).catch((err) => console.error("[sessions] 分叉失败：", err));
              }}
              onNewSession={(machine, acp) => {
                if (!acp?.cwd) return;
                const payload: Record<string, unknown> = { machine, cwd: acp.cwd };
                if (acp.agent_definition_id) {
                  payload.agent_definition_id = acp.agent_definition_id;
                } else {
                  payload.agent = acp.agent;
                  payload.profile_id = acp.profile_id ?? null;
                }
                void handleCommand("start-conversation", payload).catch((err) =>
                  console.error("[sessions] 开新对话失败：", err),
                );
              }}
              onOpenConversation={(machine, sid) => {
                void handleCommand("activate", { machine, session_id: sid }).catch((err) =>
                  console.error("[sessions] 切到那段对话失败：", err),
                );
              }}
            />
          }
        >
          <TerminalPane
            args={(spec()?.args as any) ?? null}
            reuseKey={reuseKey()}
            focusSeq={paneFocusSeq()}
            onFocusIn={() => setLiveActivePane(props.live, props.paneId, false)}
          />
        </If>
        <If when={props.many() && !isActive()}>
          <View
            class="bg-scrim opacity-40"
            style={{
              position: "absolute",
              top: 0,
              left: 0,
              right: 0,
              bottom: 0,
              pointerEvents: "none",
            }}
          />
        </If>
      </View>
    </View>
  );
}

function treeShapeKey(node: any): string {
  if (!node) return "";
  if (node.kind === "leaf") return `L:${node.id}`;
  return `S:${node.axis}(${treeShapeKey(node.first)},${treeShapeKey(node.second)})`;
}

function SplitTreeView(props: {
  live: LiveSession;
  node: any;
  path: number[];
  visible: () => boolean;
  many: () => boolean;
  touchesMap: () => Map<string, { top: boolean; right: boolean; bottom: boolean; left: boolean }>;
  parentArea: () => any;
  onResizeAt: (path: number[], sizes: number[]) => void;
}) {
  const look = currentTheme;
  if (!props.node) return null;
  if (props.node.kind === "leaf") {
    const paneId = props.node.id;
    return (
      <SplitLeafPane
        live={props.live}
        paneId={paneId}
        visible={props.visible}
        many={props.many}
        touches={() =>
          props.touchesMap().get(paneId) ?? {
            top: true,
            right: true,
            bottom: true,
            left: true,
          }
        }
        parentArea={props.parentArea}
      />
    );
  }
  const sizes = props.node.sizes;
  return (
    <ResizablePanelGroup
      axis={props.node.axis}
      handleStyle={{ backgroundColor: look().palette.hair }}
      onResize={({ sizes: nextSizes }: { sizes: number[] }) => {
        if (Array.isArray(nextSizes) && nextSizes.length === 2) {
          props.onResizeAt(props.path, nextSizes);
        }
      }}
      style={{ flex: 1, width: "100%", height: "100%", minWidth: 0, minHeight: 0 }}
    >
      <ResizablePanel
        size={sizes?.[0]}
        minSize={120}
        style={{ display: "flex", minWidth: 0, minHeight: 0 }}
      >
        <SplitTreeView
          live={props.live}
          node={props.node.first}
          path={[...props.path, 0]}
          visible={props.visible}
          many={props.many}
          touchesMap={props.touchesMap}
          parentArea={props.parentArea}
          onResizeAt={props.onResizeAt}
        />
      </ResizablePanel>
      <ResizablePanel
        size={sizes?.[1]}
        minSize={120}
        style={{ display: "flex", minWidth: 0, minHeight: 0 }}
      >
        <SplitTreeView
          live={props.live}
          node={props.node.second}
          path={[...props.path, 1]}
          visible={props.visible}
          many={props.many}
          touchesMap={props.touchesMap}
          parentArea={props.parentArea}
          onResizeAt={props.onResizeAt}
        />
      </ResizablePanel>
    </ResizablePanelGroup>
  );
}

function LiveSessionView(props: { keyStr: string }) {
  const look = currentTheme;
  const live = liveSessions.get(props.keyStr);
  if (!live) return null;

  const isSelected = computed(() => {
    const sel = selection();
    return (
      sel.kind === "conversation" && convKeyStr(sel.machine, sel.conversation_id) === props.keyStr
    );
  });
  const isVisible = computed(() => isSelected() && viewShown(SESSION_VIEW));
  const conv = computed(() => {
    const dir = workspaceDirectory(live.key.machine);
    return (dir?.conversations ?? []).find((c: any) => c.id === live.key.id) ?? null;
  });
  const bare = computed(() => !conv()?.acp);
  const title = computed(() => {
    const c = conv();
    if (!c) return "";
    const fact = conversationFacts(live.key.machine).find((f: any) => f.id === live.key.id);
    return conversationTitle(c, fact, t);
  });
  const order = computed(() => leafOrder(live.tree()));
  const many = computed(() => order().length > 1);
  const viewArea = stableMemo(() => safeArea(SESSION_VIEW));
  const innerArea = stableMemo(() => {
    const a = viewArea();
    if (!bare()) return a;
    return {
      ...a,
      title_bar: false,
      top_left: 0,
      top_right: 0,
    };
  });
  const shapeKey = computed(() => treeShapeKey(live.tree()));
  const touchesMap = computed(() => paneTouches(live.tree()));

  const onResizeAt = (path: number[], sizes: number[]) => {
    live.userEditedLayout = true;
    // 只更新持久化比例而不替换结构树信号，避免拖曳分屏边界时整树销毁重挂对话与终端。
    live.latestTree = updateSplitSizes(live.latestTree, path, sizes);
    persistLiveLayout(live);
  };

  return (
    <View
      style={{
        position: "absolute",
        top: 0,
        left: 0,
        right: 0,
        bottom: 0,
        display: isSelected() ? "flex" : "none",
        flexDirection: "column",
        minWidth: 0,
        minHeight: 0,
        paddingTop: bare() ? CHROME_ISLAND_BAND_PX : 0,
      }}
    >
      {/* 只有终端时：顶栏拖曳带 + 居中标题胶囊 */}
      <If when={bare() && viewArea().title_bar}>
        <View
          style={{
            position: "absolute",
            top: 0,
            left: 0,
            right: 0,
            height: TOP_BAR_HEIGHT,
          }}
        >
          <SmeltNativeWindowDrag style={{ width: "100%", height: "100%" }} />
        </View>
      </If>

      <View style={{ flex: 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "column" }}>
        {(() => {
          if (!shapeKey()) return null;
          const tree = live.latestTree;
          if (!tree) return null;
          return (
            <SplitTreeView
              live={live}
              node={tree}
              path={[]}
              visible={isVisible}
              many={many}
              touchesMap={touchesMap}
              parentArea={innerArea}
              onResizeAt={onResizeAt}
            />
          );
        })()}
      </View>

      <If when={bare() && order().length > 0}>
        <View
          style={{
            position: "absolute",
            top: CHROME_ISLAND_TOP_PX,
            left: 0,
            right: 0,
            height: CHROME_ISLAND_HEIGHT_PX,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            pointerEvents: "box-none",
          }}
        >
          <SmeltButton
            variant="floating"
            shape="pill"
            label={title()}
            tooltip={title()}
            class="max-w-50"
            onPress={() => {
              const firstId = order()[0];
              if (!firstId) return;
              run(REVEAL_IN_LIST_CMD, {
                machine: live.key.machine,
                session_id: firstId,
              }).catch((err) => console.error("[sessions] 在会话列表里定位失败：", err));
            }}
          />
        </View>
      </If>
    </View>
  );
}

function EmptyState() {
  const look = currentTheme;
  const offerAdd = computed(() =>
    shouldOfferAddMachine(command(ADD_MACHINE_CMD) != null, (connections()?.remotes ?? []).length),
  );

  return (
    <View style={{ flex: 1 }}>
      <SmeltWindowBar view={SESSION_VIEW} />
      <View
        style={{
          flex: 1,
          alignItems: "center",
          justifyContent: "center",
          padding: 24,
        }}
      >
        <View
          style={{
            alignItems: "center",
            gap: 10,
            maxWidth: 360,
          }}
        >
          <View
            class="rounded-(--radius-md) bg-soft items-center justify-center"
            style={{ width: 32, height: 32 }}
          >
            <SmeltIcon path="icons/coffee.svg" size={16} color={look().palette.text} />
          </View>
          <View class="items-center gap-1">
            <Text
              style={{
                fontSize: look().type.ui_medium.size,
                lineHeight: look().type.ui_medium.line_height,
                fontWeight: look().type.ui_medium.weight,
                color: look().palette.text,
              }}
            >
              {t("empty.title")}
            </Text>
            <Text
              style={{
                fontSize: look().type.ui.size,
                lineHeight: look().type.ui.line_height,
                color: look().palette.muted,
              }}
            >
              {t("empty.no_session")}
            </Text>
          </View>
          <If when={offerAdd()}>
            <SmeltButton
              variant="primary"
              label={t("empty.add_machine")}
              onPress={() => {
                addMachineError.set(null);
                run(ADD_MACHINE_CMD, null).catch((err) => {
                  addMachineError.set(
                    t("error.failed", {
                      what: t("action.add_machine"),
                      error: errorMessage(err),
                    }),
                  );
                });
              }}
            />
          </If>
          <If when={addMachineError() != null}>
            <View
              class="rounded-(--radius-md) border border-bad bg-card px-2.5 py-1.5"
            >
              <Text
                style={{
                  fontSize: look().type.small.size,
                  lineHeight: look().type.small.line_height,
                  color: look().palette.bad,
                }}
              >
                {addMachineError() ?? ""}
              </Text>
            </View>
          </If>
        </View>
      </View>
    </View>
  );
}

export function session() {
  const look = currentTheme;
  const hasSelectedLive = computed(() => {
    const sel = selection();
    return (
      sel.kind === "conversation" &&
      liveKeys().includes(convKeyStr(sel.machine, sel.conversation_id))
    );
  });

  return (
    <View
      style={{
        position: "relative",
        display: "flex",
        flexDirection: "column",
        flex: 1,
        width: "100%",
        height: "100%",
        minWidth: 0,
        minHeight: 0,
      }}
    >
      <If when={!hasSelectedLive()}>
        <EmptyState />
      </If>

      <Each of={liveKeys()} key={(k: string) => k}>
        {(k: () => string) => <LiveSessionView keyStr={k()} />}
      </Each>

      <If when={failureBanner() != null}>
        <View
          style={{
            position: "absolute",
            bottom: 12,
            left: 12,
            right: 12,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 8,
            paddingLeft: 12,
            paddingRight: 8,
            paddingTop: 8,
            paddingBottom: 8,
            borderRadius: look().radius.md,
            borderWidth: 1,
            borderColor: look().palette.bad,
            backgroundColor: look().palette.surface,
          }}
        >
          <Text
            style={{
              flex: 1,
              minWidth: 0,
              fontSize: look().type.small.size,
              lineHeight: look().type.small.line_height,
              color: look().palette.bad,
            }}
          >
            {failureBanner() ?? ""}
          </Text>
          <SmeltButton
            variant="dismiss"
            size="xs"
            icon="icons/close.svg"
            tooltip={t("split.dismiss_failure")}
            onPress={() => clearFailure()}
          />
        </View>
      </If>
    </View>
  );
}

/** 无头根：挂在主壳全局槽位，承载运行中关闭确认对话框并维持后台生命周期。 */
export function root() {
  return (
    <View style={{ width: 0, height: 0, overflow: "hidden" }}>
      <SmeltConfirmDialog
        open={closeConfirm() != null}
        title={closeConfirm()?.question ?? ""}
        description={closeConfirm()?.description ?? ""}
        confirmLabel={closeConfirm()?.actionLabel ?? ""}
        cancelLabel={t("split.cancel")}
        danger
        onConfirm={() => {
          const action = closeConfirm()?.onConfirm;
          closeConfirm.set(null);
          action?.();
        }}
        onCancel={() => closeConfirm.set(null)}
      />
    </View>
  );
}

export default session;
