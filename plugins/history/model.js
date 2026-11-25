// 历史插件的纯逻辑模块（不依赖 NSH/宿主，可直接用 bun test 单测）。

/**
 * 格式化 token 数量。
 * @param {number} n
 */
export function formatCount(n) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(n);
}

/**
 * 格式化时间戳为本地 `MM-DD HH:MM`。
 * @param {number} ms
 */
export function formatMonthDayTime(ms) {
  const d = new Date(ms);
  const pad = (v) => String(v).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/**
 * 有明显跨度（>= 1 分钟）才标耗时，否则只写时间点。
 * @param {{ started_at_ms?: number | null, last_active_at_ms?: number | null }} session
 * @param {(key: string, params?: Record<string, unknown>) => string} t
 */
export function sessionWhen(session, t) {
  const start = session.started_at_ms;
  const last = session.last_active_at_ms;
  if (typeof start === "number" && typeof last === "number") {
    const minutes = Math.trunc((last - start) / 60_000);
    if (minutes >= 1) {
      return t("row.when_ran", { time: formatMonthDayTime(last), minutes });
    }
  }
  if (typeof last === "number") {
    return formatMonthDayTime(last);
  }
  return "";
}

/**
 * 行元素键：有 resume_id 用它，空 id 退回路径。
 * @param {{ resume_id?: string, path: string }} session
 */
export function rowKey(session) {
  return session.resume_id ? session.resume_id : session.path;
}

/**
 * 主行展示标题；有自定义名时副行留原始标题 + 时间，否则副行是时间。
 * @param {{ title: string, agent_title: string, custom_title?: string | null, started_at_ms?: number | null, last_active_at_ms?: number | null }} session
 * @param {(key: string, params?: Record<string, unknown>) => string} t
 */
export function rowLabels(session, t) {
  const title = session.title;
  const when = sessionWhen(session, t);
  let secondary = when;
  if (session.custom_title != null) {
    secondary = when ? `${session.agent_title} · ${when}` : session.agent_title;
  }
  return { title, secondary };
}

/**
 * 运行时返回的一条历史概览 → 展示形状。
 * @param {any} wire
 */
export function summaryFromWire(wire) {
  const custom = wire.custom_title ?? null;
  const agentTitle = wire.title ?? "";
  return {
    title: custom ?? agentTitle,
    path: wire.path ?? "",
    agent_title: agentTitle,
    custom_title: custom,
    resume_id: wire.resume_id ?? "",
    started_at_ms: typeof wire.started_at_ms === "number" ? wire.started_at_ms : null,
    last_active_at_ms: typeof wire.last_active_at_ms === "number" ? wire.last_active_at_ms : null,
    message_count: Number(wire.message_count ?? 0),
    total_tokens: Number(wire.total_tokens ?? 0),
  };
}

/**
 * 运行时返回的一份对话内容 → 展示形状。
 * @param {any} wire
 */
export function detailFromWire(wire) {
  return {
    model: wire?.model ?? null,
    turns: Array.isArray(wire?.turns)
      ? wire.turns.map((turn) => ({
          is_user: Boolean(turn.is_user),
          timestamp_ms: typeof turn.timestamp_ms === "number" ? turn.timestamp_ms : null,
          text: String(turn.text ?? ""),
          tools: Array.isArray(turn.tools) ? turn.tools.map(String) : [],
          tool_paths: Array.isArray(turn.tool_paths) ? turn.tool_paths.map(String) : [],
        }))
      : [],
  };
}

/**
 * 列表缓存键：各家 agent、自定义 Agent、机器、目录互相隔离。
 * @param {string | null | undefined} machine
 * @param {string} agentId
 * @param {string | null | undefined} profileId
 * @param {string} cwd
 */
export function listKey(machine, agentId, profileId, cwd) {
  const host = machine == null ? "local" : String(machine);
  return `${host}:${agentId}:${profileId ?? "default"}:${cwd}`;
}

/**
 * 顶栏该列出的来源标签。智能体 space 只留其引擎；普通项目列出所有历史来源加可用自定义 Agent。
 * @param {string | null} engineId
 * @param {Array<{ id: string, label: string, kind: string | null, in_history_sources: boolean }>} profiles
 * @param {Array<{ id: string, short_label: string, acp_id: string | null }>} historySources
 */
export function sourceTabs(engineId, profiles, historySources) {
  const tabs = [];
  for (const source of historySources) {
    if (engineId != null && source.acp_id !== engineId) continue;
    tabs.push({
      kind: source.id,
      profile_id: null,
      label: source.short_label,
    });
  }
  if (engineId == null && Array.isArray(profiles)) {
    for (const profile of profiles) {
      if (!profile.kind || !profile.in_history_sources) continue;
      tabs.push({
        kind: profile.kind,
        profile_id: profile.id,
        label: profile.label,
      });
    }
  }
  return tabs;
}

/**
 * 标签栏只留有历史会话的来源，但始终保留当前选中的那个（避免选中态落空）。
 * @param {Array<{ kind: string, profile_id: string | null, label: string }>} tabs
 * @param {{ kind: string, profile_id: string | null }} current
 * @param {(tab: { kind: string, profile_id: string | null, label: string }) => boolean} hasSessions
 */
export function tabsWithSessions(tabs, current, hasSessions) {
  return tabs.filter(
    (tab) =>
      (tab.kind === current.kind && (tab.profile_id ?? null) === (current.profile_id ?? null)) ||
      hasSessions(tab),
  );
}

/**
 * 默认选中的来源已经读完且是空的时，挑第一个有会话的来源顶上，免得打开历史页一直停在空的
 * 默认标签上（比如没装 Claude、只用 Antigravity 的项目）。用户自己点的标签空着也不换；
 * 选中的来源已经不在标签里（配置删了）时照样换。当前来源还没读完时不切，防止别的来源先回来
 * 抢走选中。
 * @param {Array<{ kind: string, profile_id: string | null, label: string }>} tabs
 * @param {{ kind: string, profile_id: string | null }} current
 * @param {boolean} currentLoaded
 * @param {boolean} pickedByUser 当前来源是用户点标签选的
 * @param {(tab: { kind: string, profile_id: string | null, label: string }) => boolean} hasSessions
 * @returns {{ kind: string, profile_id: string | null } | null}
 */
export function fallbackSource(tabs, current, currentLoaded, pickedByUser, hasSessions) {
  if (!currentLoaded) return null;
  const currentTab = tabs.find(
    (tab) =>
      tab.kind === current.kind && (tab.profile_id ?? null) === (current.profile_id ?? null),
  );
  if (currentTab && (pickedByUser || hasSessions(currentTab))) return null;
  const next = tabs.find((tab) => hasSessions(tab));
  return next ? { kind: next.kind, profile_id: next.profile_id ?? null } : null;
}

/**
 * 智能体 space 里的历史，或项目里绑过智能体定义的那几条，都按智能体续接。
 * @param {boolean} inAgentSpace
 * @param {string | null | undefined} boundDefinition
 */
export function isAgentSession(inAgentSpace, boundDefinition) {
  return inAgentSpace || boundDefinition != null;
}

/**
 * @param {boolean} isAgent
 * @param {(key: string) => string} t
 */
export function continueLabel(isAgent, t) {
  return isAgent ? t("menu.continue_agent") : t("menu.continue_acp");
}

/**
 * 改名框提交结果：`undefined` = 没改；`null` = 清空恢复默认名；`string` = 新名称。
 * @param {string} initial
 * @param {string} text
 * @returns {string | null | undefined}
 */
export function submittedTitle(initial, text) {
  const trimmed = String(text ?? "").trim();
  if (trimmed === String(initial ?? "").trim()) return undefined;
  return trimmed.length > 0 ? trimmed : null;
}

/**
 * 「迁移到」目标列表：各家基础 ACP agent + 可启动自定义 Agent，跳过源自己。
 * @param {string} sourceAcpId
 * @param {string | null} sourceProfileId
 * @param {Array<{ id: string, label: string }>} conversationAgents
 * @param {Array<{ id: string, label: string, kind: string | null, launchable: boolean }>} profiles
 */
export function handoffTargets(sourceAcpId, sourceProfileId, conversationAgents, profiles) {
  const targets = [];
  for (const agent of conversationAgents) {
    if (agent.id === sourceAcpId && sourceProfileId == null) continue;
    targets.push({
      agent: agent.id,
      profile_id: null,
      profile_label: null,
      label: agent.label,
    });
  }
  if (Array.isArray(profiles)) {
    for (const profile of profiles) {
      if (sourceProfileId === profile.id || !profile.launchable || !profile.kind) continue;
      targets.push({
        agent: profile.kind,
        profile_id: profile.id,
        profile_label: profile.label,
        label: profile.label,
      });
    }
  }
  return targets;
}

/**
 * 运行时随列表给的「resume_id -> 智能体定义 id」，只留这台机器上仍在的定义。
 * @param {Record<string, string> | null | undefined} bindings
 * @param {any} settings
 */
export function knownBindings(bindings, settings) {
  if (!bindings || !settings || !Array.isArray(settings.agents)) return {};
  const known = new Set(settings.agents.map((a) => a.id));
  const out = {};
  for (const [resumeId, defId] of Object.entries(bindings)) {
    if (known.has(defId)) out[resumeId] = defId;
  }
  return out;
}

/**
 * 这一轮用过的工具，按首次出现排序，重复的记次数：`Read ×2 · Edit`。
 * @param {string[]} tools
 */
export function toolSummary(tools) {
  if (!Array.isArray(tools) || tools.length === 0) return null;
  const order = [];
  const counts = new Map();
  for (const name of tools) {
    if (!counts.has(name)) {
      order.push(name);
      counts.set(name, 1);
    } else {
      counts.set(name, counts.get(name) + 1);
    }
  }
  return order
    .map((name) => {
      const n = counts.get(name);
      return n === 1 ? name : `${name} ×${n}`;
    })
    .join(" · ");
}

/**
 * 按名称或原始标题过滤列表（不区分大小写）。
 * @param {Array<{ title: string, agent_title: string }>} sessions
 * @param {string} query
 */
export function filterSessions(sessions, query) {
  const q = String(query ?? "").trim().toLowerCase();
  if (!q) return sessions;
  return sessions.filter(
    (s) => s.title.toLowerCase().includes(q) || s.agent_title.toLowerCase().includes(q),
  );
}
