// 历史会话（NSH A2 独立 VM 版本）：复用纯逻辑 model.js 与 Rust 一等 <TextView>（SmeltMarkdown）。
import { computed, Scroll, signal, Text, View } from "nsh";
import { Input, MenuItem, MenuSeparator, Submenu } from "nsh/base";
import {
  activeContext,
  agentCatalog,
  agentForSpace,
  bootTask,
  call,
  clipboardWrite,
  command,
  currentTheme,
  DIALOG_SURFACE_CLASS,
  handoffPrompt,
  initHost,
  isInterruptMarker,
  launchableProfiles,
  machineMetaEntry,
  MENU_ITEM_CLASS,
  onEvent,
  onSlices,
  registerTopics,
  registerViews,
  request,
  run,
  SmeltButton,
  SmeltContextMenu,
  SmeltDialog,
  SmeltIcon,
  SmeltMarkdown,
  stableMemo,
  t,
  text,
  viewShown,
} from "smelt/sdk";

import {
  continueLabel,
  detailFromWire,
  filterSessions,
  formatCount,
  handoffTargets,
  isAgentSession,
  knownBindings,
  listKey,
  rowKey,
  rowLabels,
  sessionWhen,
  fallbackSource,
  sourceTabs,
  submittedTitle,
  summaryFromWire,
  tabsWithSessions,
  toolSummary,
} from "./model.js";

const VIEW_ID = "smelt.history/main";
const TOPIC_HISTORY_CHANGED = "history.changed";

// 约定：smelt.agents 的命令与事件（权威定义在 crates/plugins/agents/src/contract.rs）。
const AGENT_SETTINGS_CMD = "smelt.agents/agent-settings";
const AGENT_SETTINGS_CHANGED_EVENT = "smelt.agents/agent-settings-changed";

// 约定：smelt.sessions 的命令（权威定义在 crates/plugins/sessions/src/contract.rs）。
const RESUME_CMD = "smelt.sessions/resume";
const RESUME_IN_TERMINAL_CMD = "smelt.sessions/resume-in-terminal";
const HANDOFF_CMD = "smelt.sessions/handoff";

const CURRENT_MAX_AGE_MS = 10_000;
const PREFETCH_MAX_AGE_MS = 60_000;
const LIST_MIN_WIDTH = 190;
const LIST_MAX_WIDTH = 320;
const CONVERSATION_MAX_WIDTH = 760;

interface Project {
  machine: string | null;
  root: string;
}

interface SessionSummary {
  title: string;
  path: string;
  agent_title: string;
  custom_title: string | null;
  resume_id: string;
  started_at_ms: number | null;
  last_active_at_ms: number | null;
  message_count: number;
  total_tokens: number;
}

interface Turn {
  is_user: boolean;
  timestamp_ms: number | null;
  text: string;
  tools: string[];
  tool_paths: string[];
}

interface SessionDetail {
  model: string | null;
  turns: Turn[];
}

interface CachedList {
  machine: string | null;
  fetchedAt: number | null;
  sessions: SessionSummary[];
  bindings: Record<string, string>;
}

interface RenameDialogState {
  machine: string | null;
  agent: string;
  profileId: string | null;
  cwd: string;
  resumeId: string;
  initialTitle: string;
  value: string;
}

interface DeleteDialogState {
  machine: string | null;
  agent: string;
  agentShortLabel: string;
  profileId: string | null;
  cwd: string;
  resumeId: string;
  path: string;
  title: string;
}

function machineKey(machine: string | null | undefined): string {
  return machine == null ? "__local__" : String(machine);
}

function sameProject(a: Project | null, b: Project | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return (a.machine ?? null) === (b.machine ?? null) && a.root === b.root;
}

function errMsg(error: unknown): string {
  return error instanceof Error ? error.message : String(error ?? "");
}

function notify(subtitle: string, body: string) {
  void run("smelt.app/notify", { subtitle, body }).catch(() => {});
}

function notifyError(body: string) {
  console.error(`[history] ${body}`);
  notify(t("notify.error"), body);
}

// ---- 智能体配置只读缓存（事件驱动同步 smelt.agents） ----

const agentSettingsByMachine = signal<Record<string, any>>({});
let agentFetchState: "idle" | "loading" | "done" = "idle";
const agentTouched = new Set<string>();

function settingsOn(machine: string | null | undefined): any | null {
  return agentSettingsByMachine()[machineKey(machine)] ?? null;
}

function syncAgentSettingsRegistry() {
  const registered = command(AGENT_SETTINGS_CMD) != null;
  if (registered && agentFetchState === "idle") {
    agentFetchState = "loading";
    agentTouched.clear();
    void run(AGENT_SETTINGS_CMD, null).then(
      (machines: any) => {
        const next = { ...agentSettingsByMachine() };
        if (Array.isArray(machines)) {
          for (const entry of machines) {
            const key = machineKey(entry?.machine);
            if (agentTouched.has(key)) continue;
            if (entry?.state === "ready" && entry?.settings) {
              next[key] = entry.settings;
            }
          }
        }
        agentTouched.clear();
        agentFetchState = "done";
        agentSettingsByMachine.set(next);
        onAgentSettingsChanged();
      },
      (error: unknown) => {
        console.error("[history] 取智能体配置失败：", errMsg(error));
        agentFetchState = "idle";
      },
    );
  } else if (!registered && agentFetchState === "done") {
    agentFetchState = "idle";
    agentTouched.clear();
    agentSettingsByMachine.set({});
    onAgentSettingsChanged();
  }
}

function applyAgentSettingsEvent(payload: any) {
  if (!payload || typeof payload !== "object") return;
  const key = machineKey(payload.machine);
  if (agentFetchState === "loading") {
    agentTouched.add(key);
  }
  const next = { ...agentSettingsByMachine() };
  if (payload.state === "ready" && payload.settings) {
    next[key] = payload.settings;
  } else {
    delete next[key];
  }
  agentSettingsByMachine.set(next);
  onAgentSettingsChanged();
}

// ---- 历史面板状态 ----

const currentProject = signal<Project | null>(null);
const spaceEngineId = signal<string | null>(null);
const pickedSource = signal<{ kind: string; profile_id: string | null }>({
  kind: "claude",
  profile_id: null,
});
const listsMap = signal<Record<string, CachedList>>({});
const filterText = signal("");
const openedDetail = signal<{ path: string; detail: SessionDetail } | null>(null);
const renameDialog = signal<RenameDialogState | null>(null);
const deleteDialog = signal<DeleteDialogState | null>(null);

const inflight = new Map<string, string | null>();
const invalidated = new Set<string>();
let detailGen = 0;
let wasShown = false;

const activeSource = computed(() => {
  const engine = spaceEngineId();
  return engine != null ? { kind: engine, profile_id: null } : pickedSource();
});

function closeDetail() {
  detailGen += 1;
  openedDetail.set(null);
}

function syncProject() {
  const raw = activeContext().project;
  const next: Project | null = raw
    ? { machine: raw.machine ?? null, root: String(raw.root ?? "") }
    : null;
  const settings = next ? settingsOn(next.machine) : null;
  const engine = next && settings ? agentForSpace(settings, next.root) : null;
  if (spaceEngineId() !== engine) {
    spaceEngineId.set(engine);
  }
  if (!sameProject(currentProject(), next)) {
    currentProject.set(next);
    closeDetail();
  }
}

function onAgentSettingsChanged() {
  syncProject();
  refresh();
}

function markStale(machine: string | null | undefined) {
  const target = machine ?? null;
  const current = listsMap();
  let changed = false;
  const next: Record<string, CachedList> = {};
  for (const [key, list] of Object.entries(current)) {
    if ((list.machine ?? null) === target) {
      changed = true;
      next[key] = { ...list, fetchedAt: null };
      if (inflight.has(key)) invalidated.add(key);
    } else {
      next[key] = list;
    }
  }
  for (const [key, m] of inflight.entries()) {
    if ((m ?? null) === target) invalidated.add(key);
  }
  if (changed) listsMap.set(next);
  const project = currentProject();
  if (project && (project.machine ?? null) === target) {
    refresh();
  }
}

function refreshNow() {
  const project = currentProject();
  if (project) markStale(project.machine);
}

function profilesFor(machine: string | null) {
  const settings = settingsOn(machine);
  if (!settings) return [];
  try {
    return launchableProfiles(settings);
  } catch {
    return [];
  }
}

function refresh(forceShown = false) {
  if (!forceShown && !wasShown) return;
  const project = currentProject();
  if (!project) return;
  const catalog = agentCatalog();
  const current = activeSource();
  const tabs = sourceTabs(spaceEngineId(), profilesFor(project.machine), catalog.history_sources);
  for (const tab of tabs) {
    if (tab.kind !== current.kind || (tab.profile_id ?? null) !== (current.profile_id ?? null)) {
      ensureList(project, tab.kind, tab.profile_id, PREFETCH_MAX_AGE_MS);
    }
  }
  ensureList(project, current.kind, current.profile_id, CURRENT_MAX_AGE_MS);
  fallbackSourceIfEmpty();
}

/** 选中的来源读完是空的，换到第一个有历史的来源。智能体 space 只有一家，不换。 */
function fallbackSourceIfEmpty() {
  if (spaceEngineId() != null) return;
  const project = currentProject();
  if (!project) return;
  const current = pickedSource();
  const lists = listsMap();
  const keyOf = (kind: string, profileId: string | null) =>
    listKey(project.machine, kind, profileId, project.root);
  const currentKey = keyOf(current.kind, current.profile_id);
  const currentLoaded = !inflight.has(currentKey) && lists[currentKey]?.fetchedAt != null;
  const tabs = sourceTabs(null, profilesFor(project.machine), agentCatalog().history_sources);
  const next = fallbackSource(
    tabs,
    current,
    currentLoaded,
    (tab) => (lists[keyOf(tab.kind, tab.profile_id)]?.sessions.length ?? 0) > 0,
  );
  if (next) selectSource(next.kind, next.profile_id);
}

function ensureList(
  project: Project,
  agent: string,
  profileId: string | null,
  maxAgeMs: number,
) {
  const key = listKey(project.machine, agent, profileId, project.root);
  const existing = listsMap()[key];
  if (
    existing &&
    existing.fetchedAt != null &&
    Date.now() - existing.fetchedAt < maxAgeMs
  ) {
    return;
  }
  if (inflight.has(key)) return;
  if (machineMetaEntry(project.machine)?.online !== true) return;
  inflight.set(key, project.machine);
  void call(project.machine, "history.list", {
    agent,
    profile_id: profileId,
    cwd: project.root,
  }).then(
    (result: any) => {
      inflight.delete(key);
      const stale = invalidated.delete(key);
      const sessions = Array.isArray(result?.sessions)
        ? result.sessions.map(summaryFromWire)
        : [];
      const bindings =
        result?.agent_definition_ids && typeof result.agent_definition_ids === "object"
          ? result.agent_definition_ids
          : {};
      listsMap.set({
        ...listsMap(),
        [key]: {
          machine: project.machine,
          fetchedAt: stale ? null : Date.now(),
          sessions,
          bindings,
        },
      });
      if (stale) refresh();
      else fallbackSourceIfEmpty();
    },
    (error: unknown) => {
      inflight.delete(key);
      const stale = invalidated.delete(key);
      console.error("[history] 读取历史会话失败：", errMsg(error));
      const old = listsMap()[key];
      listsMap.set({
        ...listsMap(),
        [key]: {
          machine: project.machine,
          fetchedAt: null,
          sessions: old?.sessions ?? [],
          bindings: old?.bindings ?? {},
        },
      });
      if (stale) refresh();
    },
  );
}

function selectSource(kind: string, profileId: string | null) {
  const prev = pickedSource();
  if (prev.kind === kind && (prev.profile_id ?? null) === (profileId ?? null)) return;
  pickedSource.set({ kind, profile_id: profileId });
  closeDetail();
  refresh();
}

function openSessionDetail(path: string) {
  const project = currentProject();
  if (!project) return;
  closeDetail();
  const gen = detailGen;
  const source = activeSource();
  void call(project.machine, "history.detail", {
    agent: source.kind,
    path,
  }).then(
    (res: any) => {
      if (detailGen !== gen) return;
      if (res?.detail) {
        openedDetail.set({ path, detail: detailFromWire(res.detail) });
      } else {
        console.error(`[history] 这份存档读不出内容：${path}`);
      }
    },
    (error: unknown) => {
      if (detailGen !== gen) return;
      console.error("[history] 读取会话内容失败：", errMsg(error));
    },
  );
}

function resumeSession(
  session: SessionSummary,
  inTerminal: boolean,
  boundDefinition: string | null,
) {
  const project = currentProject();
  if (!project) return;
  const source = activeSource();
  const cmd = inTerminal ? RESUME_IN_TERMINAL_CMD : RESUME_CMD;
  void run(cmd, {
    machine: project.machine,
    agent: source.kind,
    profile_id: source.profile_id,
    cwd: project.root,
    resume_id: session.resume_id,
    agent_definition_id: inTerminal ? null : boundDefinition,
  }).catch((error) => notifyError(errMsg(error)));
}

function handOffSession(
  session: SessionSummary,
  sourceAcpId: string,
  target: { agent: string; profile_id: string | null; profile_label: string | null },
) {
  const project = currentProject();
  if (!project) return;
  const source = activeSource();
  const settings = settingsOn(project.machine);
  const sourceProfileLabel =
    source.profile_id && Array.isArray(settings?.profiles)
      ? (settings.profiles.find((p: any) => p.id === source.profile_id)?.label ?? null)
      : null;
  void call(project.machine, "history.detail", {
    agent: source.kind,
    path: session.path,
  }).then(
    (res: any) => {
      if (!res?.detail) {
        notifyError(t("handoff.unreadable", { path: session.path }));
        return;
      }
      const detail = detailFromWire(res.detail);
      let prompt: string;
      try {
        prompt = handoffPrompt({
          source_agent: sourceAcpId,
          source_profile_label: sourceProfileLabel,
          source_title: session.title,
          cwd: project.root,
          target_agent: target.agent,
          target_profile_label: target.profile_label,
          detail,
        });
      } catch (error) {
        notifyError(t("handoff.read_failed", { error: errMsg(error) }));
        return;
      }
      void run(HANDOFF_CMD, {
        machine: project.machine,
        cwd: project.root,
        agent: target.agent,
        profile_id: target.profile_id,
        prompt,
      }).catch((error) => notifyError(errMsg(error)));
    },
    (error: unknown) => {
      notifyError(t("handoff.read_failed", { error: errMsg(error) }));
    },
  );
}

function renameHistory(
  target: {
    machine: string | null;
    agent: string;
    profileId: string | null;
    cwd: string;
    resumeId: string;
  },
  title: string | null,
) {
  const payload: Record<string, unknown> = {
    op: "history_rename",
    agent: target.agent,
    resume_id: target.resumeId,
    title,
    cwd: target.cwd,
  };
  if (target.profileId != null) {
    payload.profile_id = target.profileId;
  }
  void request(target.machine, payload).catch((error) => {
    notifyError(t("rename_failed", { error: errMsg(error) }));
  });
}

function startRename(session: SessionSummary) {
  const project = currentProject();
  if (!project) return;
  const source = activeSource();
  renameDialog.set({
    machine: project.machine,
    agent: source.kind,
    profileId: source.profile_id,
    cwd: project.root,
    resumeId: session.resume_id,
    initialTitle: session.title,
    value: session.title,
  });
}

function submitRename() {
  const dialog = renameDialog();
  if (!dialog) return;
  renameDialog.set(null);
  const nextTitle = submittedTitle(dialog.initialTitle, dialog.value);
  if (nextTitle === undefined) return;
  renameHistory(dialog, nextTitle);
}

function resetTitle(session: SessionSummary) {
  const project = currentProject();
  if (!project) return;
  const source = activeSource();
  renameHistory(
    {
      machine: project.machine,
      agent: source.kind,
      profileId: source.profile_id,
      cwd: project.root,
      resumeId: session.resume_id,
    },
    null,
  );
}

function startDelete(session: SessionSummary, agentShortLabel: string) {
  const project = currentProject();
  if (!project) return;
  const source = activeSource();
  deleteDialog.set({
    machine: project.machine,
    agent: source.kind,
    agentShortLabel,
    profileId: source.profile_id,
    cwd: project.root,
    resumeId: session.resume_id,
    path: session.path,
    title: session.title,
  });
}

function confirmDelete() {
  const dialog = deleteDialog();
  if (!dialog) return;
  deleteDialog.set(null);
  if (openedDetail()?.path === dialog.path) {
    closeDetail();
  }
  void call(dialog.machine, "history.delete", {
    agent: dialog.agent,
    profile_id: dialog.profileId,
    cwd: dialog.cwd,
    resume_id: dialog.resumeId,
    path: dialog.path,
  }).then(
    () => notify(t("notify.done"), t("delete.done", { title: dialog.title })),
    (error: unknown) => notifyError(t("delete.failed", { error: errMsg(error) })),
  );
}

// ---- 宿主事件与启动 ----

onSlices(["commands", "views", "active_context", "machines_meta"], () => {
  syncAgentSettingsRegistry();
  const shown = viewShown(VIEW_ID);
  const appeared = shown && !wasShown;
  wasShown = shown;
  const prev = currentProject();
  syncProject();
  if (appeared || !sameProject(prev, currentProject())) {
    refresh();
  }
});

onEvent(
  (event: any) => {
    if (!event || typeof event !== "object") return;
    if (event.type === "plugin" && event.name === AGENT_SETTINGS_CHANGED_EVENT) {
      applyAgentSettingsEvent(event.payload);
      return;
    }
    if (event.type === "view_opened" && event.view === VIEW_ID) {
      wasShown = true;
      syncProject();
      refresh(true);
      return;
    }
    if (event.type === "machines_changed") {
      syncProject();
      refresh();
      return;
    }
    if (event.type === "reconnected") {
      markStale(event.machine);
      return;
    }
    if (
      event.type === "runtime_topic" &&
      event.topic === TOPIC_HISTORY_CHANGED &&
      event.message?.kind !== "unavailable"
    ) {
      markStale(event.machine);
    }
  },
  [
    `plugin:${AGENT_SETTINGS_CHANGED_EVENT}`,
    "view_opened",
    "machines_changed",
    "reconnected",
    "runtime_topic",
  ],
);

bootTask(async () => {
  await initHost(["commands", "views", "active_context", "machines_meta"]);
  await registerTopics([TOPIC_HISTORY_CHANGED]);
  await registerViews([
    {
      name: "main",
      export: "Main",
      title: text("view.title"),
      icon: "icons/clock.svg",
      draws_top_bar: false,
      instances: false,
    },
  ]);
  syncAgentSettingsRegistry();
  wasShown = viewShown(VIEW_ID);
  syncProject();
  refresh();
});

// ---- 视图组件 ----


function MenuRow(props: {
  label: string;
  iconPath?: string | null;
  danger?: boolean;
  onSelect: () => void;
}) {
  const look = currentTheme;
  const fg = () => (props.danger ? look().palette.bad : look().palette.text);
  const iconColor = () => (props.danger ? look().palette.bad : look().palette.muted);
  return (
    <MenuItem
      closeOnSelect={true}
      onSelect={props.onSelect}
      class={MENU_ITEM_CLASS}
      style={{ height: 28, gap: 8 }}
    >
      <If when={Boolean(props.iconPath)}>
        <View style={{ width: 14, height: 14, flexShrink: 0 }}>
          <SmeltIcon path={props.iconPath!} size={14} color={iconColor()} />
        </View>
      </If>
      <Text
        class="flex-1 min-w-0"
        style={{
          color: fg(),
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
          numberOfLines: 1,
        }}
      >
        {props.label}
      </Text>
    </MenuItem>
  );
}

function Placeholder(props: { message: string }) {
  const look = currentTheme;
  return (
    <View
      style={{
        flex: 1,
        minWidth: 0,
        minHeight: 0,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        paddingLeft: 16,
        paddingRight: 16,
      }}
    >
      <Text
        style={{
          color: look().palette.muted,
          fontSize: look().type.ui_medium.size,
          lineHeight: look().type.ui_medium.line_height,
          fontWeight: look().type.ui_medium.weight,
          textAlign: "center",
        }}
      >
        {props.message}
      </Text>
    </View>
  );
}

function HistoryTurnItem(props: { turn: Turn; cwd: string; machine: string | null }) {
  const look = currentTheme;
  const interrupted = computed(() => props.turn.is_user && isInterruptMarker(props.turn.text));
  const tools = computed(() => toolSummary(props.turn.tools));
  const hasBody = computed(() => props.turn.text.trim().length > 0);

  return (
    <View
      style={{
        width: "100%",
        display: "flex",
        flexDirection: "row",
        justifyContent: "center",
        paddingLeft: 16,
        paddingRight: 16,
        paddingBottom: 16,
      }}
    >
      <View style={{ width: "100%", minWidth: 0, maxWidth: CONVERSATION_MAX_WIDTH }}>
        <If
          when={interrupted()}
          else={
            <If
              when={props.turn.is_user}
              else={
                <View
                  style={{
                    width: "100%",
                    minWidth: 0,
                    display: "flex",
                    flexDirection: "column",
                    gap: 4,
                  }}
                >
                  <If when={hasBody()}>
                    <SmeltMarkdown
                      markdown={props.turn.text}
                      cwd={props.cwd}
                      machine={props.machine}
                      onLinkError={(error) => notifyError(errMsg(error))}
                    />
                  </If>
                  <If when={tools() != null}>
                    <View
                      style={{
                        display: "flex",
                        flexDirection: "row",
                        alignItems: "center",
                        gap: 6,
                      }}
                    >
                      <SmeltIcon path="icons/wrench.svg" size={12} color={look().palette.muted} />
                      <Text
                        style={{
                          color: look().palette.muted,
                          fontSize: look().type.text_xsmall.size,
                          lineHeight: look().type.text_xsmall.line_height,
                        }}
                      >
                        {tools() ?? ""}
                      </Text>
                    </View>
                  </If>
                </View>
              }
            >
              <View
                style={{
                  width: "100%",
                  display: "flex",
                  flexDirection: "row",
                  justifyContent: "flex-end",
                }}
              >
                <View
                  style={{
                    maxWidth: "72%",
                    minWidth: 160,
                    paddingLeft: 16,
                    paddingRight: 16,
                    paddingTop: 10,
                    paddingBottom: 10,
                    borderRadius: 12,
                    backgroundColor: look().palette.soft,
                  }}
                >
                  <SmeltMarkdown
                    markdown={props.turn.text}
                    cwd={props.cwd}
                    machine={props.machine}
                    onLinkError={(error) => notifyError(errMsg(error))}
                  />
                </View>
              </View>
            </If>
          }
        >
          <View
            style={{
              width: "100%",
              display: "flex",
              flexDirection: "row",
              alignItems: "center",
              gap: 8,
            }}
          >
            <View style={{ flex: 1, height: 1, backgroundColor: look().palette.hair }} />
            <Text
              style={{
                color: look().palette.muted,
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
              }}
            >
              {t("interrupted")}
            </Text>
            <View style={{ flex: 1, height: 1, backgroundColor: look().palette.hair }} />
          </View>
        </If>
      </View>
    </View>
  );
}

function RenameDialogInput(props: {
  value: () => string;
  onChange: (value: string) => void;
  onSubmit: () => void;
  onCancel: () => void;
}) {
  const look = currentTheme;
  let inputEl: any = null;
  let initialSelected = false;
  let edited = false;
  return (
    <Input
      ref={(el: any) => {
        inputEl = el ?? null;
        // 同步抢占焦点，避免右键菜单关闭时的 focus_back 把焦点拉回行。
        el?.focus?.();
      }}
      value={props.value()}
      autoFocus={true}
      onLayout={() => {
        if (initialSelected || edited || !inputEl) return;
        initialSelected = true;
        const len = props.value().length;
        if (len > 0) inputEl.setSelection?.(0, len);
      }}
      onChange={({ value }: { value: string }) => {
        edited = true;
        props.onChange(value);
      }}
      onSubmit={props.onSubmit}
      onKeyPress={(e: any) => {
        if (e?.key === "Escape") props.onCancel();
      }}
      class="rounded-(--radius) border border-input bg-card px-2 text-foreground"
      style={{ height: 28, fontSize: look().type.ui.size }}
    />
  );
}

export function Main() {
  const look = currentTheme;
  const catalog = agentCatalog();

  const sourceMeta = computed(() => {
    const cur = activeSource();
    return (
      catalog.history_sources.find((s) => s.id === cur.kind) ?? {
        id: cur.kind,
        label: cur.kind,
        short_label: cur.kind,
        acp_id: null,
        deletable: false,
        icon: "",
      }
    );
  });

  const currentList = computed<CachedList | null>(() => {
    const project = currentProject();
    if (!project) return null;
    const cur = activeSource();
    return listsMap()[listKey(project.machine, cur.kind, cur.profile_id, project.root)] ?? null;
  });

  const visibleTabs = computed(() => {
    const project = currentProject();
    if (!project) return [];
    const cur = activeSource();
    const all = sourceTabs(spaceEngineId(), profilesFor(project.machine), catalog.history_sources);
    const map = listsMap();
    return tabsWithSessions(all, cur, (tab) => {
      const list = map[listKey(project.machine, tab.kind, tab.profile_id, project.root)];
      return Boolean(list && list.sessions.length > 0);
    });
  });

  const bindings = computed(() => {
    const project = currentProject();
    const list = currentList();
    if (!project || !list) return {};
    return knownBindings(list.bindings, settingsOn(project.machine));
  });

  const filteredSessions = computed(() => {
    const list = currentList();
    if (!list) return [];
    return filterSessions(list.sessions, filterText());
  });

  const activeOpened = computed(() => {
    const opened = openedDetail();
    const list = currentList();
    if (!opened || !list) return null;
    const session = list.sessions.find((s) => s.path === opened.path) ?? null;
    if (!session) return null;
    return { ...opened, session };
  });

  const handoffList = computed(() => {
    const project = currentProject();
    const acpId = sourceMeta().acp_id;
    if (!project || !acpId) return [];
    return handoffTargets(
      acpId,
      activeSource().profile_id,
      catalog.conversation_agents,
      profilesFor(project.machine),
    );
  });

  return (
    <View
      style={{
        display: "flex",
        flexDirection: "column",
        width: "100%",
        height: "100%",
        minWidth: 0,
        minHeight: 0,
        backgroundColor: look().palette.bg,
      }}
    >
      <If
        when={currentProject() != null}
        else={<Placeholder message={t("empty.no_project")} />}
      >
        {/* 顶栏：刷新按钮 + 来源分段切换 */}
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            width: "100%",
            height: 38,
            minWidth: 0,
            flexShrink: 0,
            paddingLeft: 12,
            paddingRight: 12,
            gap: 8,
            borderBottomWidth: 1,
            borderColor: look().palette.hair,
          }}
        >
          <SmeltButton
            quiet={true}
            small={true}
            icon="icons/refresh.svg"
            tooltip={t("refresh")}
            onPress={refreshNow}
          />
          <View style={{ flex: 1, minWidth: 0 }} />
          <If when={spaceEngineId() == null && visibleTabs().length > 0}>
            <Scroll
              horizontal={true}
              style={{ maxWidth: "85%", minWidth: 0, flexShrink: 1 }}
            >
              <View
                style={{
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  padding: 2,
                  gap: 2,
                  borderRadius: look().radius.md,
                  backgroundColor: look().palette.soft,
                }}
              >
                <Each
                  of={visibleTabs()}
                  key={(tab: { kind: string; profile_id: string | null }) =>
                    `${tab.kind}:${tab.profile_id ?? ""}`
                  }
                >
                  {(tab: () => { kind: string; profile_id: string | null; label: string }) => {
                    const isSelected = computed(() => {
                      const cur = activeSource();
                      return (
                        tab().kind === cur.kind &&
                        (tab().profile_id ?? null) === (cur.profile_id ?? null)
                      );
                    });
                    return (
                      <View
                        role="button"
                        onPress={() => selectSource(tab().kind, tab().profile_id)}
                        style={{
                          paddingLeft: 8,
                          paddingRight: 8,
                          height: 22,
                          display: "flex",
                          flexDirection: "row",
                          alignItems: "center",
                          justifyContent: "center",
                          flexShrink: 0,
                          borderRadius: look().radius.sm,
                          backgroundColor: isSelected() ? look().palette.surface : "transparent",
                          cursor: "default",
                        }}
                      >
                        <Text
                          style={{
                            color: isSelected() ? look().palette.text : look().palette.muted,
                            fontSize: look().type.small.size,
                            lineHeight: look().type.small.line_height,
                            fontWeight: isSelected()
                              ? look().type.small_medium.weight
                              : look().type.small.weight,
                            numberOfLines: 1,
                          }}
                        >
                          {tab().label}
                        </Text>
                      </View>
                    );
                  }}
                </Each>
              </View>
            </Scroll>
          </If>
        </View>

        {/* 主体：左对话详情 + 右会话列表 */}
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            flex: 1,
            minWidth: 0,
            minHeight: 0,
            overflow: "hidden",
          }}
        >
          {/* 左：详情区 */}
          <View
            style={{
              display: "flex",
              flexDirection: "column",
              flex: 1,
              minWidth: 0,
              minHeight: 0,
              overflow: "hidden",
            }}
          >
            <If when={activeOpened() != null}>
              <View
                style={{
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  flexShrink: 0,
                  gap: 12,
                  paddingLeft: 12,
                  paddingRight: 12,
                  paddingTop: 8,
                  paddingBottom: 8,
                  borderBottomWidth: 1,
                  borderColor: look().palette.hair,
                }}
              >
                <Text
                  style={{
                    flex: 1,
                    minWidth: 0,
                    color: look().palette.text,
                    fontSize: look().type.ui_medium.size,
                    lineHeight: look().type.ui_medium.line_height,
                    fontWeight: look().type.ui_medium.weight,
                    numberOfLines: 1,
                  }}
                >
                  {activeOpened()?.session.title ?? ""}
                </Text>
                <Text
                  style={{
                    flexShrink: 0,
                    color: look().palette.muted,
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                  }}
                >
                  {activeOpened() ? sessionWhen(activeOpened()!.session, t) : ""}
                </Text>
                <Text
                  style={{
                    flexShrink: 0,
                    color: look().palette.muted,
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                  }}
                >
                  {activeOpened()
                    ? t("row.messages", { count: activeOpened()!.session.message_count })
                    : ""}
                </Text>
                <If when={(activeOpened()?.session.total_tokens ?? 0) > 0}>
                  <Text
                    style={{
                      flexShrink: 0,
                      color: look().palette.muted,
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                    }}
                  >
                    {formatCount(activeOpened()?.session.total_tokens ?? 0)}
                  </Text>
                </If>
              </View>
            </If>

            <If
              when={activeOpened() != null}
              else={<Placeholder message={t("empty.pick")} />}
            >
              <If
                when={(activeOpened()?.detail.turns.length ?? 0) > 0}
                else={<Placeholder message={t("empty.no_turns")} />}
              >
                <Scroll style={{ flex: 1, minWidth: 0, minHeight: 0 }}>
                  <View style={{ paddingTop: 12, width: "100%" }}>
                    <Each
                      of={activeOpened()?.detail.turns ?? []}
                      key={(_turn: Turn, idx: number) => `${activeOpened()?.path ?? ""}:${idx}`}
                    >
                      {(turn: () => Turn) => (
                        <HistoryTurnItem
                          turn={turn()}
                          cwd={currentProject()?.root ?? ""}
                          machine={currentProject()?.machine ?? null}
                        />
                      )}
                    </Each>
                  </View>
                </Scroll>
              </If>
            </If>
          </View>

          {/* 右：搜索 + 会话列表 */}
          <View
            style={{
              display: "flex",
              flexDirection: "column",
              width: "38%",
              minWidth: LIST_MIN_WIDTH,
              maxWidth: LIST_MAX_WIDTH,
              flexShrink: 0,
              minHeight: 0,
              borderLeftWidth: 1,
              borderColor: look().palette.hair,
              overflow: "hidden",
            }}
          >
            <View
              style={{
                flexShrink: 0,
                paddingLeft: 8,
                paddingRight: 8,
                paddingTop: 8,
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                gap: 4,
              }}
            >
              <Input
                value={filterText()}
                placeholder={t("search")}
                onChange={({ value }: { value: string }) => filterText.set(value)}
                class="flex-1 min-w-0 rounded-(--radius) border border-input bg-card px-2 text-foreground"
                style={{ height: 26, fontSize: look().type.small.size }}
              />
              <If when={filterText().length > 0}>
                <SmeltButton
                  dismiss={true}
                  icon="icons/close.svg"
                  onPress={() => filterText.set("")}
                />
              </If>
            </View>

            <If
              when={currentList() != null}
              else={<Placeholder message={t("empty.loading")} />}
            >
              <If
                when={(currentList()?.sessions.length ?? 0) > 0}
                else={<Placeholder message={t("empty.no_history")} />}
              >
                <If
                  when={filteredSessions().length > 0}
                  else={<Placeholder message={t("empty.no_match")} />}
                >
                  <Scroll style={{ flex: 1, minWidth: 0, minHeight: 0 }}>
                    <View style={{ paddingTop: 8, paddingBottom: 8 }}>
                      <Each
                        of={filteredSessions()}
                        key={(session: SessionSummary) => rowKey(session)}
                      >
                        {(session: () => SessionSummary) => {
                          const isSelected = computed(
                            () => activeOpened()?.path === session().path,
                          );
                          const labels = computed(() => rowLabels(session(), t));
                          const boundDef = computed(() => bindings()[session().resume_id] ?? null);
                          const isAgent = computed(() =>
                            isAgentSession(spaceEngineId() != null, boundDef()),
                          );
                          return (
                            <View style={{ paddingLeft: 8, paddingRight: 8, paddingBottom: 4 }}>
                              <SmeltContextMenu
                                popupStyle={{ minWidth: 184 }}
                                trigger={
                                  <View
                                    role="button"
                                    onPress={() => openSessionDetail(session().path)}
                                    style={{
                                      display: "flex",
                                      flexDirection: "column",
                                      justifyContent: "center",
                                      width: "100%",
                                      height: 54,
                                      gap: 2,
                                      paddingLeft: 8,
                                      paddingRight: 8,
                                      borderRadius: look().radius.row,
                                      backgroundColor: isSelected()
                                        ? look().palette.row_selected
                                        : "transparent",
                                      cursor: "default",
                                    }}
                                    hoverStyle={
                                      isSelected()
                                        ? undefined
                                        : { backgroundColor: look().palette.row_hover }
                                    }
                                  >
                                    <Text
                                      style={{
                                        color: look().palette.text,
                                        fontSize: look().type.ui.size,
                                        lineHeight: look().type.ui.line_height,
                                        numberOfLines: 1,
                                      }}
                                    >
                                      {labels().title}
                                    </Text>
                                    <Text
                                      style={{
                                        color: isSelected()
                                          ? look().palette.muted_selected
                                          : look().palette.muted,
                                        fontSize: look().type.text_xsmall.size,
                                        lineHeight: look().type.text_xsmall.line_height,
                                        numberOfLines: 1,
                                      }}
                                    >
                                      {labels().secondary}
                                    </Text>
                                  </View>
                                }
                              >
                                <If when={sourceMeta().acp_id != null}>
                                  <MenuRow
                                    label={continueLabel(isAgent(), t)}
                                    onSelect={() => resumeSession(session(), false, boundDef())}
                                  />
                                </If>
                                <If when={!isAgent()}>
                                  <MenuRow
                                    label={t("menu.continue_cli")}
                                    onSelect={() => resumeSession(session(), true, null)}
                                  />
                                  <If when={sourceMeta().acp_id != null && handoffList().length > 0}>
                                    <Submenu
                                      trigger={
                                        <View
                                          class={MENU_ITEM_CLASS}
                                          style={{ height: 28, gap: 8 }}
                                        >
                                          <Text
                                            class="flex-1 min-w-0"
                                            style={{
                                              color: look().palette.text,
                                              fontSize: look().type.ui.size,
                                              lineHeight: look().type.ui.line_height,
                                              numberOfLines: 1,
                                            }}
                                          >
                                            {t("menu.hand_off")}
                                          </Text>
                                          <SmeltIcon
                                            path="icons/chevron-right.svg"
                                            size={12}
                                            color={look().palette.muted}
                                          />
                                        </View>
                                      }
                                    >
                                      <Each
                                        of={handoffList()}
                                        key={(target: any) =>
                                          `${target.agent}:${target.profile_id ?? ""}`
                                        }
                                      >
                                        {(target: () => any) => (
                                          <MenuRow
                                            label={target().label}
                                            onSelect={() =>
                                              handOffSession(
                                                session(),
                                                sourceMeta().acp_id!,
                                                target(),
                                              )
                                            }
                                          />
                                        )}
                                      </Each>
                                    </Submenu>
                                  </If>
                                </If>
                                <MenuSeparator class="my-1 bg-border" style={{ height: 1 }} />
                                <MenuRow
                                  label={t("rename")}
                                  onSelect={() => startRename(session())}
                                />
                                <If when={session().custom_title != null}>
                                  <MenuRow
                                    label={t("menu.reset_title")}
                                    onSelect={() => resetTitle(session())}
                                  />
                                </If>
                                <MenuRow
                                  label={t("menu.copy_path")}
                                  onSelect={() => void clipboardWrite(session().path)}
                                />
                                <If when={sourceMeta().deletable}>
                                  <MenuSeparator class="my-1 bg-border" style={{ height: 1 }} />
                                  <MenuRow
                                    label={t("delete.action")}
                                    iconPath="icons/trash.svg"
                                    danger={true}
                                    onSelect={() =>
                                      startDelete(session(), sourceMeta().short_label)
                                    }
                                  />
                                </If>
                              </SmeltContextMenu>
                            </View>
                          );
                        }}
                      </Each>
                    </View>
                  </Scroll>
                </If>
              </If>
            </If>
          </View>
        </View>
      </If>

      {/* 重命名框 */}
      <SmeltDialog
        open={renameDialog() != null}
        role="dialog"
        accessibilityLabel={t("rename")}
        onOpenChange={({ open }: { open: boolean }) => {
          if (!open) renameDialog.set(null);
        }}
        class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
        style={{ width: 380, gap: 12 }}
      >
        <If when={renameDialog() != null}>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui_medium.size,
              fontWeight: look().type.ui_medium.weight,
            }}
          >
            {t("rename")}
          </Text>
          <RenameDialogInput
            value={() => renameDialog()?.value ?? ""}
            onChange={(value) => {
              const cur = renameDialog();
              if (cur) renameDialog.set({ ...cur, value });
            }}
            onSubmit={submitRename}
            onCancel={() => renameDialog.set(null)}
          />
          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton
              quiet={true}
              small={true}
              label={t("cancel")}
              onPress={() => renameDialog.set(null)}
            />
            <SmeltButton
              primary={true}
              small={true}
              label={t("rename")}
              onPress={submitRename}
            />
          </View>
        </If>
      </SmeltDialog>

      {/* 删除确认框 */}
      <SmeltDialog
        open={deleteDialog() != null}
        role="alertdialog"
        accessibilityLabel={t("delete.confirm")}
        onOpenChange={({ open }: { open: boolean }) => {
          if (!open) deleteDialog.set(null);
        }}
        class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
        style={{ width: 420, gap: 12 }}
      >
        <If when={deleteDialog() != null}>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui_medium.size,
              fontWeight: look().type.ui_medium.weight,
            }}
          >
            {t("delete.confirm")}
          </Text>
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
            }}
          >
            {t("delete.description", {
              agent: deleteDialog()?.agentShortLabel ?? "",
              title: deleteDialog()?.title ?? "",
            })}
          </Text>
          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton
              quiet={true}
              small={true}
              label={t("cancel")}
              onPress={() => deleteDialog.set(null)}
            />
            <SmeltButton
              danger={true}
              small={true}
              label={t("delete.action")}
              onPress={confirmDelete}
            />
          </View>
        </If>
      </SmeltDialog>
    </View>
  );
}

export default Main;
