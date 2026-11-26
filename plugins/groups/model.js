// 纯逻辑模块（零 NSH / 宿主依赖，可直接用 `bun test` 单测）。

export function machineKey(machine) {
  return machine == null || machine === "" ? "__local__" : String(machine);
}

export function groupKey(machine, groupId) {
  return `${machineKey(machine)}::${groupId}`;
}

export function sameGroupRef(a, b) {
  if (!a || !b) return false;
  return machineKey(a.machine) === machineKey(b.machine) && a.id === b.id;
}

/** 聚合所有机器上的活跃群组，按最近消息时间 / 更新时间倒序排列。 */
export function mergeGroupsAcrossMachines(byMachine) {
  const merged = [];
  for (const [mKey, list] of Object.entries(byMachine || {})) {
    const machine = mKey === "__local__" ? null : mKey;
    for (const summary of list || []) {
      merged.push({
        machine,
        ...summary,
        sortAt: summary.last_message_at_ms ?? summary.group?.updated_at_ms ?? 0,
      });
    }
  }
  merged.sort((a, b) => b.sortAt - a.sortAt);
  return merged;
}

/** 汇总所有群组的待审批与待放行数量，用于视图角标。 */
export function totalGroupAttention(byMachine) {
  let count = 0;
  for (const list of Object.values(byMachine || {})) {
    for (const summary of list || []) {
      count += (summary.waiting_approval_count ?? 0) + (summary.pending_mention_approval_count ?? 0);
    }
  }
  return count;
}

/** 检测输入框末尾是否正在输入 `@xxx`，返回待补全前缀（不含 `@`），否则返回 null。 */
export function activeMentionQuery(text) {
  if (typeof text !== "string") return null;
  const match = text.match(/(?:^|\s)@([a-zA-Z0-9_-]*)$/);
  return match ? match[1].toLowerCase() : null;
}

/** 按前缀过滤可点名成员（含 `@all`）。 */
export function filterMentionCandidates(members, query) {
  const q = (query ?? "").toLowerCase();
  const candidates = (members || []).map((m) => ({
    alias: m.alias,
    label: m.display_name || `@${m.alias}`,
    role: m.role_title || m.role_kind || "worker",
    agent: m.agent_label || m.agent || "agent",
    live_status: m.live_status || "idle",
    isAll: false,
  }));
  candidates.push({
    alias: "all",
    label: "@all",
    role: "all",
    agent: "broadcast",
    live_status: "idle",
    isAll: true,
  });
  if (!q) return candidates;
  return candidates.filter((c) => c.alias.toLowerCase().startsWith(q));
}

/** 将输入框末尾的 `@prefix` 替换为 `@alias `。 */
export function applyMentionCompletion(text, alias) {
  return text.replace(/(^|\s)@[a-zA-Z0-9_-]*$/, `$1@${alias} `);
}

/**
 * 解析输入框快捷命令：
 * - `/blind @claude @codex 评审内容` -> `{ kind: "blind", aliases: ["claude", "codex"], prompt: "评审内容" }`
 * - `/task @codex 实现刷新逻辑` -> `{ kind: "task", content: "@codex 实现刷新逻辑" }`
 * - `/decision 结论正文` -> `{ kind: "decision", content: "结论正文" }`
 * - 普通文本 -> `{ kind: "chat", content }`
 */
export function parseComposerCommand(raw, members) {
  const trimmed = (raw ?? "").trim();
  if (!trimmed) return null;
  if (trimmed.startsWith("/blind")) {
    const rest = trimmed.slice("/blind".length).trim();
    const aliases = [];
    const tokens = rest.split(/\s+/);
    let idx = 0;
    while (idx < tokens.length && tokens[idx].startsWith("@")) {
      const alias = tokens[idx].slice(1).toLowerCase();
      if (alias === "all") {
        for (const m of members || []) {
          if (!aliases.includes(m.alias)) aliases.push(m.alias);
        }
      } else if (alias) {
        if (!aliases.includes(alias)) aliases.push(alias);
      }
      idx += 1;
    }
    const prompt = tokens.slice(idx).join(" ").trim();
    const targetAliases =
      aliases.length > 0 ? aliases : (members || []).map((m) => m.alias);
    return { kind: "blind", aliases: targetAliases, prompt };
  }
  if (trimmed.startsWith("/task ")) {
    return { kind: "task", content: trimmed.slice("/task ".length).trim() };
  }
  if (trimmed.startsWith("/decision ")) {
    return { kind: "decision", content: trimmed.slice("/decision ".length).trim() };
  }
  return { kind: "chat", content: trimmed };
}

/** 将盲评提交归类到对应 `blind_prompt` 的 `seq` 下，用于并排对比卡片展示。 */
export function groupBlindSubmissions(messages) {
  const byPromptSeq = new Map();
  for (const msg of messages || []) {
    if (msg.kind === "blind_submission" && msg.reply_to_seq != null) {
      const list = byPromptSeq.get(msg.reply_to_seq) ?? [];
      list.push(msg);
      byPromptSeq.set(msg.reply_to_seq, list);
    }
  }
  return byPromptSeq;
}

/** 构造 `group.member.add` 的强类型 wire payload（嵌套 `member` 对象，与 `GroupMemberAddParams` 严格对齐）。 */
export function buildMemberAddParams(groupId, draft) {
  const alias = String(draft.alias ?? "").trim().toLowerCase();
  return {
    group_id: groupId,
    member: {
      alias,
      display_name: (draft.roleTitle ?? "").trim() || alias,
      ownership: draft.ownership ?? "group_owned",
      runtime_id: draft.runtimeId ?? null,
      session_id: draft.sessionId ?? null,
      agent_option_id: (draft.ownership ?? "group_owned") === "group_owned" ? draft.agentOptionId : null,
      cwd: (draft.cwd ?? "").trim() || null,
      role_kind: draft.roleKind ?? "worker",
      role_title: (draft.roleTitle ?? "").trim() || (draft.roleKind ?? "worker"),
      role_prompt: "",
      workspace_mode: draft.workspaceMode ?? "shared_readonly",
      tool_restriction_profile: draft.roleKind === "reviewer" ? "readonly" : "default",
    },
  };
}

/** 构造 `group.mention.approve` / `group.mention.reject` 的方法名与参数。 */
export function buildMentionDecisionCall(groupId, messageId, targetMemberId, approve, allowChain) {
  return {
    method: approve ? "group.mention.approve" : "group.mention.reject",
    params: {
      group_id: groupId,
      message_id: messageId,
      target_member_id: targetMemberId,
      allow_chain: Boolean(approve && allowChain),
    },
  };
}

/** 根据 `/blind` 解析出来的 alias 列表映射出 `group.blind.start` 需要的 `participant_member_ids`。 */
export function resolveBlindParticipantIds(aliases, members) {
  const byAlias = new Map((members || []).map((m) => [String(m.alias).toLowerCase(), m.id]));
  const ids = [];
  for (const alias of aliases || []) {
    const id = byAlias.get(String(alias).toLowerCase());
    if (id && !ids.includes(id)) ids.push(id);
  }
  if (ids.length === 0) {
    for (const m of members || []) {
      if (m.id && !ids.includes(m.id)) ids.push(m.id);
    }
  }
  return ids;
}

/** 预置异构多智能体建群阵容模板（用户可按需增删改引擎）。 */
export const GROUP_TEMPLATES = [
  {
    id: "dev-trio",
    mode: "coordinator",
    members: [
      {
        alias: "architect",
        agent_option_id: "claude",
        role_kind: "coordinator",
        role_title: "Architect",
        workspace_mode: "shared_readonly",
        tool_restriction_profile: "readonly",
      },
      {
        alias: "builder",
        agent_option_id: "codex",
        role_kind: "worker",
        role_title: "Builder",
        workspace_mode: "isolated_worktree",
        tool_restriction_profile: "default",
      },
      {
        alias: "reviewer",
        agent_option_id: "antigravity",
        role_kind: "reviewer",
        role_title: "Reviewer",
        workspace_mode: "shared_readonly",
        tool_restriction_profile: "readonly",
      },
    ],
  },
  {
    id: "blind-panel",
    mode: "mention",
    members: [
      {
        alias: "claude",
        agent_option_id: "claude",
        role_kind: "reviewer",
        role_title: "Claude",
        workspace_mode: "shared_readonly",
        tool_restriction_profile: "readonly",
      },
      {
        alias: "codex",
        agent_option_id: "codex",
        role_kind: "reviewer",
        role_title: "Codex",
        workspace_mode: "shared_readonly",
        tool_restriction_profile: "readonly",
      },
      {
        alias: "pi",
        agent_option_id: "pi",
        role_kind: "reviewer",
        role_title: "Pi",
        workspace_mode: "shared_readonly",
        tool_restriction_profile: "readonly",
      },
    ],
  },
];
