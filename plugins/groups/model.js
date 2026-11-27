// 本地智能体群组纯逻辑（零宿主依赖，可独立单测）。

const RESERVED_ALIASES = new Set(["all", "everyone", "here", "user", "system", "group"]);

/** 校验并规范化成员 alias，与后端 `smelt_model::agent_group::validate_member_alias` 保持一致。 */
export function normalizeAlias(raw) {
  const cleaned = String(raw ?? "")
    .trim()
    .replace(/^@+/, "")
    .toLowerCase();
  if (!cleaned || cleaned.length > 32) {
    return { ok: false, code: "length" };
  }
  if (!/^[a-z0-9][a-z0-9_-]*$/.test(cleaned)) {
    return { ok: false, code: "format" };
  }
  if (RESERVED_ALIASES.has(cleaned)) {
    return { ok: false, code: "reserved", value: cleaned };
  }
  return { ok: true, value: cleaned };
}

/** 从一段输入中把 `@alias` 补全词切出来，用于渲染候选成员提示。 */
export function activeMentionPrefix(text) {
  const str = String(text ?? "");
  const match = str.match(/(?:^|\s)@([a-zA-Z0-9_-]*)$/);
  return match ? match[1].toLowerCase() : null;
}

/** 把候选 `@alias` 插入输入框末尾正在输入的 `@...` 片段。 */
export function applyMentionSuggestion(text, alias) {
  const str = String(text ?? "");
  return str.replace(/(^|\s)@[a-zA-Z0-9_-]*$/, `$1@${alias} `);
}

/** 按更新时间降序、未归档在前排列群组列表。 */
export function sortGroupSummaries(groups) {
  return [...(Array.isArray(groups) ? groups : [])].sort((a, b) => {
    const aArch = a.archived_at_ms != null ? 1 : 0;
    const bArch = b.archived_at_ms != null ? 1 : 0;
    if (aArch !== bArch) return aArch - bArch;
    const diff = (b.updated_at_ms ?? 0) - (a.updated_at_ms ?? 0);
    if (diff !== 0) return diff;
    return String(a.id ?? "").localeCompare(String(b.id ?? ""));
  });
}

/** 增量合并 `group.message_appended` 推送来的单条消息（按 id/seq 去重、按 seq 升序）。 */
export function mergeAppendedMessage(existing, incoming) {
  if (!incoming || typeof incoming !== "object") return existing;
  const list = Array.isArray(existing) ? existing : [];
  const next = list.filter((item) => item.id !== incoming.id && item.seq !== incoming.seq);
  next.push(incoming);
  next.sort((a, b) => (a.seq ?? 0) - (b.seq ?? 0));
  return next;
}

/** 在列表头部合并翻页加载出来的更早消息（按 seq 去重升序）。 */
export function prependOlderMessages(existing, older) {
  const bySeq = new Map();
  for (const msg of Array.isArray(older) ? older : []) {
    bySeq.set(msg.seq, msg);
  }
  for (const msg of Array.isArray(existing) ? existing : []) {
    bySeq.set(msg.seq, msg);
  }
  return [...bySeq.values()].sort((a, b) => (a.seq ?? 0) - (b.seq ?? 0));
}

/**
 * 判断 `group.changed` 到达时是否需要刷新当前打开群组的详情元数据（`group.get`）。
 * 绝不在每次无关事件里重拉全部消息；只有当前群组的摘要元数据（成员数、暂停/归档态、
 * 待审批数、未完任务数、标题或模式）变化时才刷新 `group.get`。
 */
export function shouldRefreshActiveGroupDetail(currentDetail, nextSummary) {
  if (!currentDetail || !nextSummary) return false;
  if (currentDetail.id !== nextSummary.id) return false;
  const activeMembers = (currentDetail.members ?? []).filter((m) => m.removed_at_ms == null).length;
  const openTasks = (currentDetail.open_tasks ?? []).length;
  return (
    currentDetail.title !== nextSummary.title ||
    currentDetail.description !== nextSummary.description ||
    currentDetail.mode !== nextSummary.mode ||
    currentDetail.paused !== nextSummary.paused ||
    (currentDetail.archived_at_ms ?? null) !== (nextSummary.archived_at_ms ?? null) ||
    (currentDetail.coordinator_member_id ?? null) !== (nextSummary.coordinator_member_id ?? null) ||
    activeMembers !== nextSummary.active_member_count ||
    openTasks !== nextSummary.open_task_count ||
    (nextSummary.pending_approval_count ?? 0) > 0
  );
}

/** 筛出所有等待用户审批的 `@` 投递条目。 */
export function pendingApprovalsInMessages(messages) {
  const out = [];
  for (const msg of Array.isArray(messages) ? messages : []) {
    for (const mention of Array.isArray(msg.mentions) ? msg.mentions : []) {
      if (mention.delivery_status === "pending_approval") {
        out.push({ message: msg, mention });
      }
    }
  }
  return out;
}

/** 按时间格式化短时间戳（如 `14:05`）。 */
export function formatShortTime(ms) {
  if (!Number.isFinite(ms) || ms <= 0) return "";
  const d = new Date(ms);
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  return `${hh}:${mm}`;
}

/** 列出当前可挂接（`linked`）的本机 ACP 会话候选，过滤掉已经在当前群里的会话。 */
export function linkableConversations(directory, existingSessionIds) {
  const used = existingSessionIds instanceof Set ? existingSessionIds : new Set(existingSessionIds ?? []);
  const conversations = Array.isArray(directory?.conversations) ? directory.conversations : [];
  const out = [];
  for (const conv of conversations) {
    const sid = conv?.acp?.sid;
    if (!sid || used.has(sid)) continue;
    const title = (conv.custom_title || conv.acp.session_title || conv.acp.agent || sid).trim();
    out.push({
      session_id: sid,
      title,
      agent: conv.acp.agent ?? "",
      cwd: conv.acp.cwd ?? "",
    });
  }
  return out;
}

/** 根据成员 `display_name` / `title` 推一个干净的默认 alias。 */
export function suggestAliasFromTitle(raw) {
  const slug = String(raw ?? "")
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^[-_]+|[-_]+$/g, "")
    .slice(0, 24);
  if (!slug || RESERVED_ALIASES.has(slug)) return "member";
  return slug;
}
