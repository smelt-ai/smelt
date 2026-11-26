// 智能体群组插件（`smelt.groups`）响应式状态、跨机器群组聚合、事件驱动投影与 Control API 动作。
import { signal } from "nsh";
import {
  agentCatalog,
  bootTask,
  call,
  currentTheme,
  initHost,
  machineMeta,
  newSessionId,
  onCommand,
  onEvent,
  onSlices,
  registerCommands,
  registerContextMenuItems,
  registerTopics,
  registerViews,
  run,
  t,
  text,
} from "smelt/sdk";
import {
  ADD_CONVERSATION_CMD,
  NEW_GROUP_CMD,
  NOTIFY_CMD,
  OPEN_VIEW_CMD,
  PLUGIN_ID,
  SESSION_LIST_CONVERSATION_MENU,
  SESSION_LIST_PROJECT_MENU,
  SET_VIEW_BADGE_CMD,
  SIDEBAR_NAME,
  TOPIC_GROUP_MESSAGE_APPENDED,
  TOPIC_GROUPS_CHANGED,
  VIEW,
  VIEW_NAME,
} from "../contract.js";
import {
  buildMemberAddParams,
  buildMentionDecisionCall,
  GROUP_TEMPLATES,
  machineKey,
  mergeGroupsAcrossMachines,
  parseComposerCommand,
  resolveBlindParticipantIds,
  sameGroupRef,
  totalGroupAttention,
} from "../model.js";

export { currentTheme };

export interface GroupRef {
  machine: string | null;
  id: string;
}

export interface CreateGroupDraft {
  hostMachine: string | null;
  title: string;
  description: string;
  mode: "mention" | "coordinator";
  worksetId: string | null;
  cwd: string;
  templateId: string;
  submitting: boolean;
  error: string | null;
}

export interface AddMemberDraft {
  alias: string;
  agentOptionId: string;
  roleKind: "coordinator" | "worker" | "reviewer" | "observer";
  roleTitle: string;
  workspaceMode: "isolated_worktree" | "shared_readonly" | "shared_leased";
  ownership: "group_owned" | "linked";
  runtimeId: string | null;
  sessionId: string | null;
  cwd: string;
  submitting: boolean;
  error: string | null;
}

export type DrawerTab = "session" | "tasks" | "branches";

// ---- 响应式信号 ----

export const groupsVersion = signal(0);
export const selectedGroup = signal<GroupRef | null>(null);
export const activeDetail = signal<any | null>(null);
export const activeMessages = signal<any[]>([]);
export const detailLoading = signal(false);
export const pageError = signal<string | null>(null);

export const sidebarCollapsed = signal(false);
export const composerText = signal("");
export const sending = signal(false);

export const drawerOpen = signal(false);
export const drawerTab = signal<DrawerTab>("session");
export const inspectedMemberId = signal<string | null>(null);

export const worktreeDiffs = signal<Record<string, any>>({});
export const worktreeBusyMember = signal<string | null>(null);

export const createDraft = signal<CreateGroupDraft | null>(null);
export const addMemberDraft = signal<AddMemberDraft | null>(null);

// ---- 模块内多机器投影 ----

const groupsByMachine: Record<string, any[]> = {};
let lastBadge: string | null | undefined = undefined;
let booted = false;

function errMsg(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err ?? "");
}

function bumpGroups() {
  groupsVersion.set(groupsVersion() + 1);
  refreshBadge();
}

function refreshBadge() {
  const count = totalGroupAttention(groupsByMachine);
  const label = count > 0 ? String(count) : null;
  if (label === lastBadge) return;
  lastBadge = label;
  run(SET_VIEW_BADGE_CMD, { view: VIEW, badge: label }).catch(() => {});
}

function notifyError(detail: string) {
  pageError.set(detail);
  run(NOTIFY_CMD, {
    title: t("notify.error"),
    body: detail,
  }).catch(() => {});
}

export function allGroups(): any[] {
  groupsVersion();
  return mergeGroupsAcrossMachines(groupsByMachine);
}

export function availableMachines(): Array<{ machine: string | null; name: string }> {
  const list = machineMeta();
  if (!Array.isArray(list) || list.length === 0) {
    return [{ machine: null, name: t("sidebar.local_host") }];
  }
  return list.map((m: any) => ({
    machine: m.machine ?? null,
    name: m.machine == null ? t("sidebar.local_host") : m.name || String(m.machine),
  }));
}

export function machineDisplayName(machine: string | null | undefined): string {
  if (machine == null || machine === "") return t("sidebar.local_host");
  const found = availableMachines().find((m) => m.machine === machine);
  return found?.name ?? String(machine);
}

export function availableConversationAgents(): Array<{ id: string; label: string }> {
  try {
    const list = agentCatalog()?.conversation_agents;
    if (Array.isArray(list) && list.length > 0) {
      return list.map((item: any) => ({
        id: String(item.id),
        label: String(item.label || item.name || item.id),
      }));
    }
  } catch {
    // fallback below
  }
  return [
    { id: "claude", label: "Claude Code" },
    { id: "codex", label: "Codex" },
    { id: "antigravity", label: "Antigravity" },
    { id: "pi", label: "Pi" },
    { id: "opencode", label: "OpenCode" },
  ];
}

// ---- 数据加载与增量同步 ----

export async function refreshGroupsOnMachine(machine: string | null): Promise<void> {
  try {
    const res = await call<any>(machine, "group.list", { include_archived: false });
    groupsByMachine[machineKey(machine)] = Array.isArray(res?.groups) ? res.groups : [];
    bumpGroups();
  } catch {
    // 离线远端机器或旧版本运行时忽略，等待重连事件恢复
  }
}

export async function refreshAllMachines(): Promise<void> {
  const targets = availableMachines().map((m) => m.machine);
  await Promise.all(targets.map((m) => refreshGroupsOnMachine(m)));
}

export async function openGroup(ref: GroupRef, focusView = true): Promise<void> {
  selectedGroup.set(ref);
  pageError.set(null);
  if (focusView) {
    run(OPEN_VIEW_CMD, { view: VIEW, args: { machine: ref.machine, group_id: ref.id } }).catch(
      () => {},
    );
  }
  await reloadActiveGroup();
}

export async function reloadActiveGroup(): Promise<void> {
  const ref = selectedGroup();
  if (!ref) {
    activeDetail.set(null);
    activeMessages.set([]);
    return;
  }
  detailLoading.set(true);
  try {
    const [detail, page] = await Promise.all([
      call<any>(ref.machine, "group.get", { group_id: ref.id }),
      call<any>(ref.machine, "group.message.list", { group_id: ref.id, limit: 150 }),
    ]);
    if (!sameGroupRef(selectedGroup(), ref)) return;
    activeDetail.set(detail);
    activeMessages.set(Array.isArray(page?.messages) ? page.messages : []);
  } catch (err) {
    if (sameGroupRef(selectedGroup(), ref)) {
      pageError.set(errMsg(err));
    }
  } finally {
    if (sameGroupRef(selectedGroup(), ref)) {
      detailLoading.set(false);
    }
  }
}

function applyGroupsTopic(machine: string | null, message: any) {
  const payload = message?.payload ?? message;
  if (Array.isArray(payload?.groups)) {
    groupsByMachine[machineKey(machine)] = payload.groups;
    bumpGroups();
  } else {
    void refreshGroupsOnMachine(machine);
  }
  const cur = selectedGroup();
  if (cur && machineKey(cur.machine) === machineKey(machine)) {
    void reloadActiveGroup();
  }
}

function applyMessageAppendedTopic(machine: string | null, message: any) {
  const payload = message?.payload ?? message;
  const groupId = payload?.group_id;
  const appended = payload?.message;
  const cur = selectedGroup();
  if (!cur || !groupId || cur.id !== groupId || machineKey(cur.machine) !== machineKey(machine)) {
    return;
  }
  if (appended && typeof appended === "object") {
    const prev = activeMessages();
    const idx = prev.findIndex((m) => m.id === appended.id);
    if (idx >= 0) {
      const next = prev.slice();
      next[idx] = appended;
      activeMessages.set(next);
    } else {
      activeMessages.set([...prev, appended]);
    }
  } else {
    void reloadActiveGroup();
  }
}

// ---- 建群 / 加成员 / 发言 / 审批 / 盲评 / 分支操作 ----

export function openCreateGroupDialog(opts?: {
  hostMachine?: string | null;
  worksetId?: string | null;
  cwd?: string;
}) {
  createDraft.set({
    hostMachine: opts?.hostMachine ?? null,
    title: "",
    description: "",
    mode: "mention",
    worksetId: opts?.worksetId ?? null,
    cwd: opts?.cwd ?? "",
    templateId: "dev-trio",
    submitting: false,
    error: null,
  });
}

export function closeCreateGroupDialog() {
  createDraft.set(null);
}

export function patchCreateDraft(patch: Partial<CreateGroupDraft>) {
  const cur = createDraft();
  if (!cur) return;
  createDraft.set({ ...cur, ...patch, error: patch.error ?? null });
}

export async function submitCreateGroup(): Promise<void> {
  const draft = createDraft();
  if (!draft || draft.submitting) return;
  const title = draft.title.trim();
  if (!title) {
    patchCreateDraft({ error: "Group title is required" });
    return;
  }
  patchCreateDraft({ submitting: true, error: null });
  try {
    const tpl = GROUP_TEMPLATES.find((x) => x.id === draft.templateId);
    const members = (tpl?.members ?? []).map((m) => ({
      alias: m.alias,
      display_name: m.role_title,
      ownership: "group_owned",
      runtime_id: null,
      session_id: null,
      agent_option_id: m.agent_option_id,
      cwd: draft.cwd.trim() || null,
      role_kind: m.role_kind,
      role_title: m.role_title,
      role_prompt: "",
      workspace_mode: m.workspace_mode,
      tool_restriction_profile: m.tool_restriction_profile,
    }));
    const detail = await call<any>(draft.hostMachine, "group.create", {
      title,
      description: draft.description.trim(),
      workset_id: draft.worksetId,
      mode: tpl?.mode ?? draft.mode,
      members,
    });
    createDraft.set(null);
    await refreshGroupsOnMachine(draft.hostMachine);
    if (detail?.group?.id) {
      await openGroup({ machine: draft.hostMachine, id: detail.group.id }, true);
    }
  } catch (err) {
    patchCreateDraft({ submitting: false, error: errMsg(err) });
  }
}

export function openAddMemberDialog(prefill?: {
  ownership?: "group_owned" | "linked";
  runtimeId?: string | null;
  sessionId?: string | null;
  alias?: string;
}) {
  addMemberDraft.set({
    alias: prefill?.alias ?? "",
    agentOptionId: "codex",
    roleKind: "worker",
    roleTitle: "",
    workspaceMode: "isolated_worktree",
    ownership: prefill?.ownership ?? "group_owned",
    runtimeId: prefill?.runtimeId ?? null,
    sessionId: prefill?.sessionId ?? null,
    cwd: "",
    submitting: false,
    error: null,
  });
}

export function closeAddMemberDialog() {
  addMemberDraft.set(null);
}

export function patchAddMemberDraft(patch: Partial<AddMemberDraft>) {
  const cur = addMemberDraft();
  if (!cur) return;
  addMemberDraft.set({ ...cur, ...patch, error: patch.error ?? null });
}

export async function submitAddMember(): Promise<void> {
  const ref = selectedGroup();
  const draft = addMemberDraft();
  if (!ref || !draft || draft.submitting) return;
  const alias = draft.alias.trim().toLowerCase();
  if (!alias) {
    patchAddMemberDraft({ error: "Member alias is required (e.g. codex, reviewer)" });
    return;
  }
  patchAddMemberDraft({ submitting: true, error: null });
  try {
    const added = await call<any>(
      ref.machine,
      "group.member.add",
      buildMemberAddParams(ref.id, { ...draft, alias }),
    );
    // 若把本机对话加入远端群主，顺手在本机登记零信任 remote_group_member_grant 白名单
    if (ref.machine != null && draft.runtimeId == null && draft.sessionId && added?.id) {
      await call(null, "group.remote_grant.upsert", {
        host_runtime_id: ref.machine,
        group_id: ref.id,
        member_id: added.id,
        local_session_id: draft.sessionId,
        allow_remote_full_access: false,
      }).catch(() => {});
    }
    addMemberDraft.set(null);
    await reloadActiveGroup();
  } catch (err) {
    patchAddMemberDraft({ submitting: false, error: errMsg(err) });
  }
}

export async function sendComposerMessage(): Promise<void> {
  const ref = selectedGroup();
  const detail = activeDetail();
  const raw = composerText().trim();
  if (!ref || !detail || !raw || sending()) return;

  const parsed = parseComposerCommand(raw, detail.members ?? []);
  if (!parsed) return;

  sending.set(true);
  pageError.set(null);
  try {
    if (parsed.kind === "blind") {
      const participantIds = resolveBlindParticipantIds(parsed.aliases, detail.members ?? []);
      await call(ref.machine, "group.blind.start", {
        group_id: ref.id,
        prompt: parsed.prompt || raw,
        participant_member_ids: participantIds,
        idempotency_key: `ui-${newSessionId()}`,
      });
    } else if (parsed.kind === "task") {
      await call(ref.machine, "group.message.post", {
        group_id: ref.id,
        content: parsed.content,
        kind: "task",
        idempotency_key: `ui-${newSessionId()}`,
      });
    } else if (parsed.kind === "decision") {
      await call(ref.machine, "group.message.post", {
        group_id: ref.id,
        content: parsed.content,
        kind: "decision",
        pinned: true,
        idempotency_key: `ui-${newSessionId()}`,
      });
    } else {
      await call(ref.machine, "group.message.post", {
        group_id: ref.id,
        content: parsed.content,
        kind: "chat",
        idempotency_key: `ui-${newSessionId()}`,
      });
    }
    composerText.set("");
    await reloadActiveGroup();
  } catch (err) {
    notifyError(errMsg(err));
  } finally {
    sending.set(false);
  }
}

export async function quickWakeFromMessage(seq: number, alias: string): Promise<void> {
  const ref = selectedGroup();
  if (!ref) return;
  try {
    await call(ref.machine, "group.message.post", {
      group_id: ref.id,
      content: `@${alias} 请针对 #${seq} 处理并回复`,
      kind: "chat",
      reply_to_seq: seq,
      mentions: alias === "all" ? ["all"] : [alias],
      idempotency_key: `ui-wake-${newSessionId()}`,
    });
    await reloadActiveGroup();
  } catch (err) {
    notifyError(errMsg(err));
  }
}

export async function revealBlindRound(promptSeq: number): Promise<void> {
  const ref = selectedGroup();
  if (!ref) return;
  try {
    await call(ref.machine, "group.blind.reveal", {
      group_id: ref.id,
      prompt_seq: promptSeq,
    });
    await reloadActiveGroup();
  } catch (err) {
    notifyError(errMsg(err));
  }
}

export async function decideMentionApproval(
  messageId: string,
  targetMemberId: string,
  approve: boolean,
  allowChain: boolean,
): Promise<void> {
  const ref = selectedGroup();
  if (!ref) return;
  try {
    const decision = buildMentionDecisionCall(ref.id, messageId, targetMemberId, approve, allowChain);
    await call(ref.machine, decision.method, decision.params);
    await reloadActiveGroup();
  } catch (err) {
    notifyError(errMsg(err));
  }
}

export async function toggleGroupPaused(): Promise<void> {
  const ref = selectedGroup();
  const group = activeDetail()?.group;
  if (!ref || !group) return;
  try {
    if (group.paused) {
      await call(ref.machine, "group.resume", { group_id: ref.id, extra_hops: 2 });
    } else {
      await call(ref.machine, "group.pause", { group_id: ref.id, abort_running: false });
    }
    await reloadActiveGroup();
  } catch (err) {
    notifyError(errMsg(err));
  }
}

export async function toggleMessagePinned(seq: number, pinned: boolean): Promise<void> {
  const ref = selectedGroup();
  if (!ref) return;
  try {
    await call(ref.machine, "group.message.pin", {
      group_id: ref.id,
      seq,
      pinned,
    });
    await reloadActiveGroup();
  } catch (err) {
    notifyError(errMsg(err));
  }
}

export function inspectMember(memberId: string) {
  inspectedMemberId.set(memberId);
  drawerTab.set("session");
  drawerOpen.set(true);
}

export async function loadMemberDiff(memberId: string): Promise<void> {
  const ref = selectedGroup();
  if (!ref) return;
  worktreeBusyMember.set(memberId);
  try {
    const diff = await call<any>(ref.machine, "group.worktree.diff", {
      group_id: ref.id,
      member_id: memberId,
    });
    worktreeDiffs.set({ ...worktreeDiffs(), [memberId]: diff });
  } catch (err) {
    notifyError(errMsg(err));
  } finally {
    worktreeBusyMember.set(null);
  }
}

export async function mergeMemberBranch(
  memberId: string,
  target: "integration" | "base",
): Promise<void> {
  const ref = selectedGroup();
  if (!ref) return;
  worktreeBusyMember.set(memberId);
  try {
    const res = await call<any>(ref.machine, "group.worktree.merge", {
      group_id: ref.id,
      member_id: memberId,
      target,
      strategy: "merge",
    });
    worktreeDiffs.set({
      ...worktreeDiffs(),
      [memberId]: {
        ...(worktreeDiffs()[memberId] ?? {}),
        merged_commit: res?.commit_sha,
        merged_target: res?.target_branch,
      },
    });
    await reloadActiveGroup();
  } catch (err) {
    notifyError(errMsg(err));
  } finally {
    worktreeBusyMember.set(null);
  }
}

// ---- 启动与宿主事件接线 ----

const commandName = (id: string) => id.slice(PLUGIN_ID.length + 1);

export function bootGroups() {
  if (booted) return;
  booted = true;

  onSlices(["machines_meta"], () => {
    void refreshAllMachines();
  });

  onEvent(
    (event: any) => {
      if (!event || typeof event !== "object") return;
      if (event.type === "runtime_topic") {
        if (event.topic === TOPIC_GROUPS_CHANGED) {
          applyGroupsTopic(event.machine ?? null, event.message);
        } else if (event.topic === TOPIC_GROUP_MESSAGE_APPENDED) {
          applyMessageAppendedTopic(event.machine ?? null, event.message);
        }
        return;
      }
      if (event.type === "machines_changed" || event.type === "reconnected") {
        void refreshAllMachines();
        return;
      }
      if (event.type === "view_opened" && event.view === VIEW) {
        const args = event.args;
        if (args && typeof args === "object" && typeof args.group_id === "string") {
          void openGroup({ machine: args.machine ?? null, id: args.group_id }, false);
        }
      }
    },
    ["runtime_topic", "machines_changed", "reconnected", "view_opened"],
  );

  onCommand(async ({ name, args }) => {
    if (name === commandName(NEW_GROUP_CMD)) {
      openCreateGroupDialog({
        hostMachine: args?.machine ?? null,
        worksetId: args?.workset_id ?? null,
        cwd: typeof args?.path === "string" ? args.path : typeof args?.root === "string" ? args.root : "",
      });
      await run(OPEN_VIEW_CMD, { view: VIEW, args: {} }).catch(() => {});
      return null;
    }
    if (name === commandName(ADD_CONVERSATION_CMD)) {
      const sessionId = typeof args?.session_id === "string" ? args.session_id : args?.id ?? null;
      const runtimeId = typeof args?.machine === "string" ? args.machine : null;
      const groups = allGroups();
      if (!selectedGroup() && groups.length > 0) {
        await openGroup({ machine: groups[0].machine ?? null, id: groups[0].group.id }, true);
      } else {
        await run(OPEN_VIEW_CMD, { view: VIEW, args: {} }).catch(() => {});
      }
      openAddMemberDialog({
        ownership: "linked",
        runtimeId,
        sessionId,
        alias: typeof args?.title === "string" ? args.title.toLowerCase().replace(/[^a-z0-9_-]+/g, "-").slice(0, 20) : "",
      });
      return null;
    }
    throw new Error(`Unknown groups command: ${name}`);
  });

  bootTask(async () => {
    await initHost(["machines_meta", "commands", "views", "locale"]);
    await Promise.all([
      registerTopics([TOPIC_GROUPS_CHANGED, TOPIC_GROUP_MESSAGE_APPENDED]),
      registerViews([
        {
          name: VIEW_NAME,
          export: "main",
          title: text("view.main"),
          icon: "icons/users.svg",
          draws_top_bar: false,
          instances: false,
        },
        {
          name: SIDEBAR_NAME,
          export: "sidebar",
          title: text("view.sidebar"),
          icon: "icons/users.svg",
          draws_top_bar: false,
          instances: false,
        },
      ]),
      registerCommands([
        {
          name: commandName(NEW_GROUP_CMD),
          title: text("menu.new_group"),
          context: "Workspace",
          args: ["machine", "workset_id", "path", "root"],
        },
        {
          name: commandName(ADD_CONVERSATION_CMD),
          title: text("menu.add_to_group"),
          requires_args: true,
          args: ["machine", "session_id", "id", "title"],
        },
      ]),
      registerContextMenuItems(SESSION_LIST_PROJECT_MENU, [
        {
          label: text("menu.new_group"),
          icon: "icons/users.svg",
          command: NEW_GROUP_CMD,
        },
      ]),
      registerContextMenuItems(SESSION_LIST_CONVERSATION_MENU, [
        {
          label: text("menu.add_to_group"),
          icon: "icons/users.svg",
          command: ADD_CONVERSATION_CMD,
        },
      ]),
    ]);
    await refreshAllMachines();
  });
}
