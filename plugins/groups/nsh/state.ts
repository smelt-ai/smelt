// 本地智能体群组插件（`smelt.groups`）响应式状态、事件驱动更新与宿主命令/菜单登记。
// 零定时器、零轮询；`group.changed` 只更新摘要（仅当当前群元数据变更时刷新 `group.get`，不重拉消息列表），
// `group.message_appended` 增量追加单条消息。

import { signal } from "nsh";
import {
  bootTask,
  call,
  currentTheme,
  initHost,
  onCommand,
  onEvent,
  registerCommands,
  registerContextMenuItems,
  registerTopics,
  registerViews,
  run,
  t,
  text,
  workspaceDirectory,
} from "smelt/sdk";
import {
  CMD_ADD_CONVERSATION,
  CMD_CREATE_FOR_PROJECT,
  CMD_OPEN_GROUP,
  FULL_CMD_ADD_CONVERSATION,
  FULL_CMD_CREATE_FOR_PROJECT,
  MAIN_VIEW,
  MAIN_VIEW_NAME,
  OPEN_VIEW_CMD,
  PROJECT_MENU,
  SESSION_MENU,
  TOPIC_GROUP_MESSAGE_APPENDED,
  TOPIC_GROUPS_CHANGED,
} from "../contract.js";
import {
  applyMentionSuggestion,
  linkableConversations,
  mergeAppendedMessage,
  normalizeAlias,
  prependOlderMessages,
  shouldRefreshActiveGroupDetail,
  sortGroupSummaries,
  suggestAliasFromTitle,
} from "../model.js";

export { currentTheme };

export interface GroupSummary {
  id: string;
  title: string;
  description: string;
  project_root?: string | null;
  workset_id?: string | null;
  mode: "mention" | "coordinator";
  coordinator_member_id?: string | null;
  paused: boolean;
  archived_at_ms?: number | null;
  member_count: number;
  active_member_count: number;
  last_seq: number;
  last_message_preview?: string | null;
  last_message_at_ms?: number | null;
  pending_approval_count: number;
  open_task_count: number;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface GroupMember {
  id: string;
  group_id: string;
  alias: string;
  display_name: string;
  ownership: "group_owned" | "linked";
  session_id: string;
  role_kind: "coordinator" | "worker" | "reviewer" | "observer";
  role_title: string;
  role_prompt: string;
  tool_restriction_profile: string;
  briefing_sent: boolean;
  last_delivered_seq: number;
  last_read_seq: number;
  muted: boolean;
  removed_at_ms?: number | null;
  created_at_ms: number;
  agent_label?: string | null;
  cwd?: string | null;
  phase?: string | null;
  session_alive: boolean;
}

export interface GroupMention {
  message_id: string;
  target_member_id: string;
  target_alias: string;
  mention_kind: string;
  delivery_status: string;
  delivery_id: string;
  status_reason?: string | null;
  updated_at_ms: number;
}

export interface GroupMessage {
  id: string;
  group_id: string;
  seq: number;
  sender_type: "user" | "member" | "system";
  sender_member_id?: string | null;
  sender_alias?: string | null;
  sender_display_name: string;
  kind: "chat" | "task" | "decision" | "system";
  content: string;
  reply_to_seq?: number | null;
  thread_root_seq?: number | null;
  task_status?: "open" | "in_progress" | "done" | "blocked" | null;
  chain_id?: string | null;
  chain_depth: number;
  auto_bridged: boolean;
  pinned: boolean;
  created_at_ms: number;
  mentions: GroupMention[];
}

export interface GroupDetail {
  id: string;
  title: string;
  description: string;
  project_root?: string | null;
  workset_id?: string | null;
  mode: "mention" | "coordinator";
  coordinator_member_id?: string | null;
  allow_worker_cross_talk: boolean;
  max_chain_hops: number;
  max_chain_depth: number;
  max_pair_ping_pong: number;
  auto_bridge_mentions: boolean;
  paused: boolean;
  next_seq: number;
  archived_at_ms?: number | null;
  created_at_ms: number;
  updated_at_ms: number;
  members: GroupMember[];
  open_tasks: GroupMessage[];
}

export interface AgentOption {
  id: string;
  label: string;
}

export const groups = signal<GroupSummary[]>([]);
export const selectedGroupId = signal<string | null>(null);
export const activeGroup = signal<GroupDetail | null>(null);
export const messages = signal<GroupMessage[]>([]);
export const hasMoreMessages = signal<boolean>(false);
export const loadingMessages = signal<boolean>(false);
export const pageError = signal<string | null>(null);

export const composerText = signal<string>("");
export const composerKind = signal<"chat" | "task" | "decision">("chat");
export const replyToSeq = signal<number | null>(null);
export const sending = signal<boolean>(false);

export const drawerOpen = signal<boolean>(true);
export const drawerTab = signal<"member" | "tasks" | "settings">("member");
export const inspectedMemberId = signal<string | null>(null);

// 对话框状态
export const createDialogOpen = signal<boolean>(false);
export const createTitle = signal<string>("");
export const createDescription = signal<string>("");
export const createProjectRoot = signal<string | null>(null);
export const createMode = signal<"mention" | "coordinator">("mention");
export const createError = signal<string | null>(null);
export const createBusy = signal<boolean>(false);

export const addMemberDialogOpen = signal<boolean>(false);
export const addMemberOwnership = signal<"group_owned" | "linked">("group_owned");
export const addMemberAlias = signal<string>("");
export const addMemberAgent = signal<string>("pi");
export const addMemberSessionId = signal<string>("");
export const addMemberRole = signal<"coordinator" | "worker" | "reviewer" | "observer">("worker");
export const addMemberRolePrompt = signal<string>("");
export const addMemberError = signal<string | null>(null);
export const addMemberBusy = signal<boolean>(false);
export const availableAgents = signal<AgentOption[]>([]);

export const deleteDialogOpen = signal<boolean>(false);
export const deleteBusy = signal<boolean>(false);

const MESSAGE_PAGE_SIZE = 60;
let booted = false;
let loadSeq = 0;

function errText(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (error && typeof error === "object" && "message" in error) {
    return String((error as any).message ?? "");
  }
  return String(error ?? "");
}

function reportActionError(error: unknown) {
  pageError.set(t("error.action_failed", { error: errText(error) }));
}

export function clearError() {
  pageError.set(null);
}

export function getLinkableSessions() {
  const dir = workspaceDirectory(null);
  const used = new Set((activeGroup()?.members ?? []).map((m) => m.session_id));
  return linkableConversations(dir, used);
}

async function fetchAgentOptions() {
  try {
    const catalog = await call(null, "new_session.catalog", {});
    const list = Array.isArray(catalog?.agents)
      ? catalog.agents.map((a: any) => ({ id: String(a.id), label: String(a.label || a.id) }))
      : [];
    availableAgents.set(list);
    if (list.length > 0 && !list.some((a: AgentOption) => a.id === addMemberAgent())) {
      addMemberAgent.set(list[0].id);
    }
  } catch {
    // 保留默认列表，不阻塞对话框打开。
  }
}

export async function refreshGroupList() {
  try {
    const res = await call(null, "group.list", { include_archived: false });
    const sorted = sortGroupSummaries(res?.groups ?? []);
    groups.set(sorted);
    const currentId = selectedGroupId();
    if (!currentId && sorted.length > 0) {
      await selectGroup(sorted[0].id);
    } else if (currentId && !sorted.some((g: GroupSummary) => g.id === currentId)) {
      selectedGroupId.set(sorted[0]?.id ?? null);
      if (sorted[0]) {
        await selectGroup(sorted[0].id);
      } else {
        activeGroup.set(null);
        messages.set([]);
      }
    }
  } catch (error) {
    pageError.set(t("error.load_failed", { error: errText(error) }));
  }
}

export async function selectGroup(groupId: string) {
  selectedGroupId.set(groupId);
  replyToSeq.set(null);
  pageError.set(null);
  const req = ++loadSeq;
  loadingMessages.set(true);
  try {
    const [detail, page] = await Promise.all([
      call(null, "group.get", { group_id: groupId, include_removed_members: false }),
      call(null, "group.message.list", { group_id: groupId, limit: MESSAGE_PAGE_SIZE }),
    ]);
    if (req !== loadSeq) return;
    activeGroup.set(detail);
    messages.set(Array.isArray(page?.messages) ? page.messages : []);
    hasMoreMessages.set(Boolean(page?.has_more));
    const currentMember = inspectedMemberId();
    const members = Array.isArray(detail?.members) ? detail.members : [];
    if (!currentMember || !members.some((m: GroupMember) => m.id === currentMember)) {
      inspectedMemberId.set(members[0]?.id ?? null);
    }
  } catch (error) {
    if (req === loadSeq) reportActionError(error);
  } finally {
    if (req === loadSeq) loadingMessages.set(false);
  }
}

async function refreshActiveGroupDetailOnly(groupId: string) {
  try {
    const detail = await call(null, "group.get", {
      group_id: groupId,
      include_removed_members: false,
    });
    if (selectedGroupId() !== groupId) return;
    activeGroup.set(detail);
    const members = Array.isArray(detail?.members) ? detail.members : [];
    const currentMember = inspectedMemberId();
    if (!currentMember || !members.some((m: GroupMember) => m.id === currentMember)) {
      inspectedMemberId.set(members[0]?.id ?? null);
    }
  } catch {
    // 增量刷新失败不打断用户当前阅读。
  }
}

export async function loadEarlierMessages() {
  const groupId = selectedGroupId();
  const list = messages();
  if (!groupId || list.length === 0 || loadingMessages() || !hasMoreMessages()) return;
  const earliest = list[0].seq;
  loadingMessages.set(true);
  try {
    const page = await call(null, "group.message.list", {
      group_id: groupId,
      before_seq: earliest,
      limit: MESSAGE_PAGE_SIZE,
    });
    if (selectedGroupId() !== groupId) return;
    messages.set(prependOlderMessages(messages(), page?.messages ?? []));
    hasMoreMessages.set(Boolean(page?.has_more));
  } catch (error) {
    reportActionError(error);
  } finally {
    loadingMessages.set(false);
  }
}

export function openCreateDialog(projectRoot: string | null = null) {
  createTitle.set("");
  createDescription.set("");
  createProjectRoot.set(projectRoot);
  createMode.set("mention");
  createError.set(null);
  createDialogOpen.set(true);
}

export async function submitCreateGroup() {
  const title = createTitle().trim();
  if (!title) {
    createError.set(t("error.title_required"));
    return;
  }
  createBusy.set(true);
  createError.set(null);
  try {
    const created: GroupDetail = await call(null, "group.create", {
      title,
      description: createDescription().trim(),
      project_root: createProjectRoot(),
      mode: createMode(),
    });
    createDialogOpen.set(false);
    await refreshGroupList();
    await selectGroup(created.id);
  } catch (error) {
    createError.set(t("error.action_failed", { error: errText(error) }));
  } finally {
    createBusy.set(false);
  }
}

export function openAddMemberDialog(prefillSession?: { session_id: string; title?: string } | null) {
  addMemberError.set(null);
  addMemberRole.set("worker");
  addMemberRolePrompt.set("");
  if (prefillSession?.session_id) {
    addMemberOwnership.set("linked");
    addMemberSessionId.set(prefillSession.session_id);
    addMemberAlias.set(suggestAliasFromTitle(prefillSession.title ?? prefillSession.session_id));
  } else {
    addMemberOwnership.set("group_owned");
    addMemberSessionId.set(getLinkableSessions()[0]?.session_id ?? "");
    addMemberAlias.set("");
  }
  addMemberDialogOpen.set(true);
  void fetchAgentOptions();
}

export async function submitAddMember() {
  const groupId = selectedGroupId();
  if (!groupId) return;
  const parsed = normalizeAlias(addMemberAlias());
  if (!parsed.ok) {
    if (parsed.code === "length") addMemberError.set(t("error.alias_length"));
    else if (parsed.code === "reserved") {
      addMemberError.set(t("error.alias_reserved", { alias: parsed.value ?? "" }));
    } else {
      addMemberError.set(t("error.alias_format"));
    }
    return;
  }
  const ownership = addMemberOwnership();
  if (ownership === "linked" && !addMemberSessionId().trim()) {
    addMemberError.set(t("error.session_required"));
    return;
  }
  addMemberBusy.set(true);
  addMemberError.set(null);
  try {
    const member: GroupMember = await call(null, "group.member.add", {
      group_id: groupId,
      alias: parsed.value,
      display_name: parsed.value,
      ownership,
      session_id: ownership === "linked" ? addMemberSessionId().trim() : null,
      agent: ownership === "group_owned" ? addMemberAgent() : null,
      role_kind: addMemberRole(),
      role_prompt: addMemberRolePrompt().trim(),
    });
    addMemberDialogOpen.set(false);
    inspectedMemberId.set(member.id);
    await refreshActiveGroupDetailOnly(groupId);
  } catch (error) {
    addMemberError.set(t("error.action_failed", { error: errText(error) }));
  } finally {
    addMemberBusy.set(false);
  }
}

export async function sendGroupMessage() {
  const groupId = selectedGroupId();
  const content = composerText().trim();
  if (!groupId || !content || sending()) return;
  sending.set(true);
  pageError.set(null);
  try {
    const posted: GroupMessage = await call(null, "group.message.post", {
      group_id: groupId,
      content,
      kind: composerKind(),
      reply_to_seq: replyToSeq(),
      task_status: composerKind() === "task" ? "open" : null,
    });
    composerText.set("");
    composerKind.set("chat");
    replyToSeq.set(null);
    messages.set(mergeAppendedMessage(messages(), posted));
  } catch (error) {
    reportActionError(error);
  } finally {
    sending.set(false);
  }
}

export function insertMention(alias: string) {
  composerText.set(applyMentionSuggestion(composerText(), alias));
}

export async function togglePauseGroup() {
  const g = activeGroup();
  if (!g) return;
  try {
    const updated: GroupDetail = g.paused
      ? await call(null, "group.resume", { group_id: g.id })
      : await call(null, "group.pause", { group_id: g.id, abort_running: false });
    activeGroup.set(updated);
  } catch (error) {
    reportActionError(error);
  }
}

export async function toggleGroupMode() {
  const g = activeGroup();
  if (!g) return;
  const nextMode = g.mode === "coordinator" ? "mention" : "coordinator";
  try {
    const updated: GroupDetail = await call(null, "group.update", {
      group_id: g.id,
      mode: nextMode,
    });
    activeGroup.set(updated);
  } catch (error) {
    reportActionError(error);
  }
}

export async function updateGroupSetting(patch: Record<string, unknown>) {
  const g = activeGroup();
  if (!g) return;
  try {
    const updated: GroupDetail = await call(null, "group.update", {
      group_id: g.id,
      ...patch,
    });
    activeGroup.set(updated);
  } catch (error) {
    reportActionError(error);
  }
}

export async function archiveActiveGroup() {
  const g = activeGroup();
  if (!g) return;
  try {
    await call(null, "group.archive", { group_id: g.id, archived: true });
    await refreshGroupList();
  } catch (error) {
    reportActionError(error);
  }
}

export async function confirmDeleteActiveGroup() {
  const g = activeGroup();
  if (!g || deleteBusy()) return;
  deleteBusy.set(true);
  try {
    await call(null, "group.delete", { group_id: g.id });
    deleteDialogOpen.set(false);
    selectedGroupId.set(null);
    activeGroup.set(null);
    messages.set([]);
    await refreshGroupList();
  } catch (error) {
    reportActionError(error);
  } finally {
    deleteBusy.set(false);
  }
}

export async function approvePendingMention(messageId: string, targetMemberId: string) {
  const groupId = selectedGroupId();
  if (!groupId) return;
  try {
    await call(null, "group.mention.approve", {
      group_id: groupId,
      message_id: messageId,
      target_member_id: targetMemberId,
    });
    messages.set(
      messages().map((m) =>
        m.id !== messageId
          ? m
          : {
              ...m,
              mentions: m.mentions.map((mn) =>
                mn.target_member_id === targetMemberId ? { ...mn, delivery_status: "queued" } : mn,
              ),
            },
      ),
    );
  } catch (error) {
    reportActionError(error);
  }
}

export async function rejectPendingMention(messageId: string, targetMemberId: string) {
  const groupId = selectedGroupId();
  if (!groupId) return;
  try {
    await call(null, "group.mention.reject", {
      group_id: groupId,
      message_id: messageId,
      target_member_id: targetMemberId,
    });
    messages.set(
      messages().map((m) =>
        m.id !== messageId
          ? m
          : {
              ...m,
              mentions: m.mentions.map((mn) =>
                mn.target_member_id === targetMemberId ? { ...mn, delivery_status: "rejected" } : mn,
              ),
            },
      ),
    );
  } catch (error) {
    reportActionError(error);
  }
}

export async function togglePinMessage(seq: number, pinned: boolean) {
  const groupId = selectedGroupId();
  if (!groupId) return;
  try {
    await call(null, "group.message.pin", { group_id: groupId, seq, pinned });
    messages.set(messages().map((m) => (m.seq === seq ? { ...m, pinned } : m)));
  } catch (error) {
    reportActionError(error);
  }
}

export async function setTaskStatus(seq: number, status: "open" | "in_progress" | "done" | "blocked") {
  const groupId = selectedGroupId();
  if (!groupId) return;
  try {
    await call(null, "group.task.update", { group_id: groupId, seq, task_status: status });
    messages.set(messages().map((m) => (m.seq === seq ? { ...m, task_status: status } : m)));
    await refreshActiveGroupDetailOnly(groupId);
  } catch (error) {
    reportActionError(error);
  }
}

export async function wakeMember(memberId: string) {
  const groupId = selectedGroupId();
  if (!groupId) return;
  try {
    await call(null, "group.member.wake", { group_id: groupId, member_id: memberId });
  } catch (error) {
    reportActionError(error);
  }
}

export async function toggleMuteMember(member: GroupMember) {
  try {
    await call(null, "group.member.update", {
      group_id: member.group_id,
      member_id: member.id,
      muted: !member.muted,
    });
    await refreshActiveGroupDetailOnly(member.group_id);
  } catch (error) {
    reportActionError(error);
  }
}

export async function removeMember(member: GroupMember) {
  try {
    await call(null, "group.member.remove", {
      group_id: member.group_id,
      member_id: member.id,
      delete_owned_session: member.ownership === "group_owned",
    });
    await refreshActiveGroupDetailOnly(member.group_id);
  } catch (error) {
    reportActionError(error);
  }
}

function handleGroupsChangedPayload(payload: any) {
  if (!payload || !Array.isArray(payload.groups)) return;
  const sorted = sortGroupSummaries(payload.groups);
  groups.set(sorted);

  const currentId = selectedGroupId();
  if (!currentId && sorted.length > 0) {
    void selectGroup(sorted[0].id);
    return;
  }
  if (!currentId) return;

  const summary = sorted.find((g: GroupSummary) => g.id === currentId);
  if (!summary) {
    selectedGroupId.set(sorted[0]?.id ?? null);
    if (sorted[0]) void selectGroup(sorted[0].id);
    else {
      activeGroup.set(null);
      messages.set([]);
    }
    return;
  }

  // 仅当当前群元数据发生变化时刷新 `group.get`；绝不在每次事件重拉消息列表。
  if (shouldRefreshActiveGroupDetail(activeGroup(), summary)) {
    void refreshActiveGroupDetailOnly(currentId);
  }
}

function handleMessageAppendedPayload(payload: any) {
  if (!payload || typeof payload !== "object") return;
  if (payload.group_id !== selectedGroupId() || !payload.message) return;
  messages.set(mergeAppendedMessage(messages(), payload.message));
}

export function bootGroups() {
  if (booted) return;
  booted = true;

  onEvent(
    (event: any) => {
      if (!event || typeof event !== "object") return;
      if (event.type === "runtime_topic" && event.machine == null) {
        if (event.topic === TOPIC_GROUPS_CHANGED) {
          handleGroupsChangedPayload(event.message?.payload);
        } else if (event.topic === TOPIC_GROUP_MESSAGE_APPENDED) {
          handleMessageAppendedPayload(event.message?.payload);
        }
      } else if (event.type === "reconnected" && event.machine == null) {
        void refreshGroupList();
      }
    },
    ["runtime_topic", "reconnected"],
  );

  onCommand(async ({ name, args }: { name: string; args?: any }) => {
    if (name === CMD_CREATE_FOR_PROJECT) {
      const path = typeof args?.path === "string" ? args.path : null;
      await run(OPEN_VIEW_CMD, { view: MAIN_VIEW });
      openCreateDialog(path);
      return null;
    }
    if (name === CMD_ADD_CONVERSATION) {
      const sessionId = typeof args?.session_id === "string" ? args.session_id : "";
      await run(OPEN_VIEW_CMD, { view: MAIN_VIEW });
      if (groups().length === 0) {
        pageError.set(t("error.no_groups_to_add"));
        openCreateDialog(null);
        return null;
      }
      if (!selectedGroupId() && groups()[0]) {
        await selectGroup(groups()[0].id);
      }
      openAddMemberDialog(sessionId ? { session_id: sessionId } : null);
      return null;
    }
    if (name === CMD_OPEN_GROUP) {
      const groupId = typeof args?.group_id === "string" ? args.group_id : null;
      await run(OPEN_VIEW_CMD, { view: MAIN_VIEW });
      if (groupId) await selectGroup(groupId);
      return null;
    }
    throw new Error(`Unknown command: ${name}`);
  });

  bootTask(async () => {
    await initHost(["sessions_data", "locale", "commands"]);
    await Promise.all([
      registerViews([
        {
          name: MAIN_VIEW_NAME,
          export: MAIN_VIEW_NAME,
          title: text("view.title"),
          icon: "icons/messages-square.svg",
        },
      ]),
      registerCommands([
        {
          name: CMD_CREATE_FOR_PROJECT,
          title: text("menu.create_for_project"),
          requires_args: true,
          args: ["path"],
        },
        {
          name: CMD_ADD_CONVERSATION,
          title: text("menu.add_conversation"),
          requires_args: true,
          args: ["session_id"],
        },
        {
          name: CMD_OPEN_GROUP,
          title: text("view.title"),
          requires_args: true,
          args: ["group_id"],
        },
      ]),
      // 仅本机可见（`machine: null`），远程项目/会话右键菜单不显示。
      registerContextMenuItems(PROJECT_MENU, [
        {
          label: text("menu.create_for_project"),
          icon: "icons/messages-square.svg",
          image: null,
          machine: null,
          command: FULL_CMD_CREATE_FOR_PROJECT,
        },
      ]),
      registerContextMenuItems(SESSION_MENU, [
        {
          label: text("menu.add_conversation"),
          icon: "icons/messages-square.svg",
          image: null,
          machine: null,
          command: FULL_CMD_ADD_CONVERSATION,
        },
      ]),
      registerTopics([TOPIC_GROUPS_CHANGED, TOPIC_GROUP_MESSAGE_APPENDED]),
    ]);
    void refreshGroupList();
  });
}
