// 通知插件纯逻辑（不 import NSH，供 bun test 单测）：
// 覆盖提醒镜像（attention）、偏好开关、通知 id、菜单栏构建与系统授权提示文案。

import { ACTIVATE_SESSION, OWNER, SHOW_MAIN_WINDOW } from "./contract.js";

export const DEFAULT_PREFS = Object.freeze({
  approval: true,
  input: true,
  success: true,
  failure: true,
  bell: true,
});

/** 这类提醒发不发。终端通知、智能体消息与认不出的新类型默认始终发。 */
export function prefsAllows(prefs, kind) {
  switch (kind) {
    case "approval":
      return Boolean(prefs.approval);
    case "input":
      return Boolean(prefs.input);
    case "success":
      return Boolean(prefs.success);
    case "failure":
      return Boolean(prefs.failure);
    case "bell":
      return Boolean(prefs.bell);
    default:
      return true;
  }
}

/** SQLite `delivered.machine` 列：本机存空串，远程存 runtime id。 */
export function machineColumn(machine) {
  return machine == null ? "" : String(machine);
}

export function machineFromColumn(column) {
  return !column ? null : String(column);
}

export function deliveredKey(machine, sessionId) {
  return `${machine == null ? "" : `remote:${machine}`}\0${sessionId}`;
}

/** 通知 id：`smelt.notifications/<机器>/<会话>`，同一段会话再来就换掉上一条。 */
export function notificationId(machine, sessionId) {
  const host = machine == null ? "local" : `remote:${machine}`;
  return `${OWNER}/${host}/${sessionId}`;
}

function sessionAttentionIsEmpty(session) {
  return !session || (session.action == null && session.unread == null);
}

/** 各台机器的会话提醒镜像（只照运行时推来的 `attention.changed` 留一份）。 */
export class AttentionMirror {
  constructor() {
    this.machines = new Map();
  }

  _key(machine) {
    return machine == null ? "" : String(machine);
  }

  apply(machine, message) {
    if (!message || typeof message !== "object") return { kind: "nothing" };
    const mk = this._key(machine);
    if (message.kind === "snapshot") {
      const snap = message.payload;
      if (!snap || typeof snap !== "object" || !Array.isArray(snap.sessions)) {
        return { kind: "nothing" };
      }
      const sessions = new Map();
      for (const s of snap.sessions) {
        if (s && typeof s.session_id === "string" && !sessionAttentionIsEmpty(s)) {
          sessions.set(s.session_id, s);
        }
      }
      this.machines.set(mk, {
        primed: true,
        revision: Number(snap.revision ?? 0),
        sessions,
      });
      return { kind: "replaced" };
    }
    if (message.kind === "event") {
      const changed = message.payload;
      if (
        !changed ||
        typeof changed !== "object" ||
        !changed.session ||
        typeof changed.session.session_id !== "string"
      ) {
        return { kind: "nothing" };
      }
      let entry = this.machines.get(mk);
      if (!entry) {
        entry = { primed: false, revision: 0, sessions: new Map() };
        this.machines.set(mk, entry);
      }
      const rev = Number(changed.revision ?? 0);
      if (entry.primed && rev <= entry.revision) {
        return { kind: "nothing" };
      }
      entry.revision = rev;
      const session = changed.session;
      if (sessionAttentionIsEmpty(session)) {
        entry.sessions.delete(session.session_id);
      } else {
        entry.sessions.set(session.session_id, session);
      }
      return {
        kind: "changed",
        session,
        alert: changed.alert ?? null,
      };
    }
    return { kind: "nothing" };
  }

  retainMachines(keepFn) {
    let removed = false;
    for (const mk of [...this.machines.keys()]) {
      const machine = mk === "" ? null : mk;
      if (!keepFn(machine)) {
        this.machines.delete(mk);
        removed = true;
      }
    }
    return removed;
  }

  primed(machine) {
    return Boolean(this.machines.get(this._key(machine))?.primed);
  }

  hasUnread(machine, sessionId) {
    const s = this.machines.get(this._key(machine))?.sessions.get(sessionId);
    return Boolean(s && s.unread != null);
  }

  /** 要你看的会话（所有机器）：session_id -> 是否需要你处理（action != null）。 */
  bySession() {
    const out = new Map();
    for (const entry of this.machines.values()) {
      for (const s of entry.sessions.values()) {
        out.set(s.session_id, s.action != null);
      }
    }
    return out;
  }
}

export function statusRank(status) {
  switch (status) {
    case "needs_you":
      return 0;
    case "running":
      return 1;
    default:
      return 2;
  }
}

export function highestStatus(statuses) {
  let best = "idle";
  for (const s of statuses) {
    if (statusRank(s) < statusRank(best)) best = s;
  }
  return best;
}

export function terminalAgentStatus(state, statusOf) {
  if (!state) return "idle";
  return statusOf(state) ?? "idle";
}

/** 构造一段对话在菜单栏里需要的行数据；还没起过任何会话的终端返回 null。 */
export function conversationRow(machine, conversation, statesMap, remoteName, deps) {
  const { t, statusOf, agentFromLaunch } = deps;
  let ids;
  let title;
  let agent;

  if (conversation?.acp) {
    const acp = conversation.acp;
    const st = statesMap.get(acp.sid);
    title = st?.conversation_name || t("fallback.conversation");
    ids = [acp.sid];
    agent = acp.agent || null;
  } else {
    const started = (conversation?.terminals ?? [])
      .filter((term) => typeof term?.id === "string" && term.id.length > 0)
      .map((term) => ({ id: term.id, terminal: term }));
    if (started.length === 0) return null;
    const first = started[0];
    const firstState = statesMap.get(first.id);
    const named = started.find(({ id }) => Boolean(statesMap.get(id)?.conversation_name));
    title = named ? statesMap.get(named.id).conversation_name : t("fallback.terminal");
    const launch = first.terminal.launch_cmd || firstState?.launch || null;
    agent = (launch ? agentFromLaunch(launch) : null) || firstState?.provider || null;
    ids = started.map(({ id }) => id);
  }

  const fullTitle = remoteName ? `${title} · ${remoteName}` : title;
  const statuses = ids.map((id) => terminalAgentStatus(statesMap.get(id), statusOf));
  return {
    machine,
    ids,
    statuses,
    status: highestStatus(statuses),
    title: fullTitle,
    agent,
  };
}

export const TERMINAL_MENU_ICON = "icons/terminal.svg";

/** agent 菜单栏单色 SVG 资源路径（认不出的回退到终端图标）。 */
export function agentIconAsset(agent, knownAgentIds) {
  if (typeof agent === "string" && agent.length > 0 && knownAgentIds.has(agent)) {
    return `smelt-icons/agent-${agent}.svg`;
  }
  return TERMINAL_MENU_ICON;
}

export function statusTitle(attentionCount, runningCount) {
  if (attentionCount === 0 && runningCount === 0) return "";
  if (runningCount === 0) return `${attentionCount}@`;
  if (attentionCount === 0) return String(runningCount);
  return `${attentionCount}@·${runningCount}`;
}

export function statusTooltip(attentionCount, runningCount, t) {
  const attentionText =
    attentionCount === 1
      ? t("tooltip.attention.one")
      : t("tooltip.attention.other", { count: attentionCount });
  const runningText =
    runningCount === 1
      ? t("tooltip.running.one")
      : t("tooltip.running.other", { count: runningCount });
  if (attentionCount === 0 && runningCount === 0) return "smelt";
  if (runningCount === 0) return attentionText;
  if (attentionCount === 0) return runningText;
  return t("tooltip.both", { attention: attentionText, running: runningText });
}

export function statusColorHex(status, palette) {
  switch (status) {
    case "needs_you":
      return palette.need;
    case "running":
      return palette.run;
    default:
      return palette.faint;
  }
}

export function buildMenu(rows, attentionMap, palette, knownAgentIds, t) {
  let running = 0;
  const listed = [];

  for (const row of rows) {
    const st = row.status ?? highestStatus(row.statuses);
    if (st === "running") running += 1;

    const raised = row.ids.find((id) => attentionMap.has(id)) ?? null;
    const requiresAction =
      st === "needs_you" || row.ids.some((id) => attentionMap.get(id) === true);
    if (raised === null && st === "idle") continue;

    let sessionId = raised;
    if (!sessionId) {
      const idx = row.statuses.findIndex((own) => own === st);
      sessionId = idx >= 0 ? row.ids[idx] : row.ids[0];
    }
    if (!sessionId) continue;

    let shown;
    let statusLabel;
    if (requiresAction) {
      shown = "needs_you";
      statusLabel = t("status.needs_you");
    } else if (st === "running") {
      shown = "running";
      statusLabel = t("status.running");
    } else {
      shown = "idle";
      statusLabel = t("status.unread");
    }

    listed.push({
      rank: statusRank(shown),
      entry: {
        machine: row.machine,
        session_id: sessionId,
        label: `${row.title} — ${statusLabel}`,
        agent: row.agent ?? null,
        color: statusColorHex(shown, palette),
      },
    });
  }

  listed.sort((a, b) => a.rank - b.rank);
  const entries = listed.map((item) => item.entry);
  const title = statusTitle(attentionMap.size, running);
  const tooltip = statusTooltip(attentionMap.size, running, t);

  const items = entries.map((entry) => ({
    type: "action",
    label: entry.label,
    icon: {
      png: agentIconAsset(entry.agent, knownAgentIds),
      color: entry.color,
    },
    command: ACTIVATE_SESSION,
    args: {
      machine: entry.machine,
      session_id: entry.session_id,
    },
  }));
  if (items.length > 0) {
    items.push({ type: "separator" });
  }
  items.push({
    type: "action",
    label: t("menu.show_main_window"),
    icon: null,
    command: SHOW_MAIN_WINDOW,
    args: null,
  });

  return {
    title,
    tooltip,
    entries,
    statusMenu: {
      icon: null,
      title,
      tooltip,
      items,
    },
  };
}

/** 「系统通知」授权状态行；还在读或无系统通知时返回 null。 */
export function permissionLine(permission, t) {
  if (!permission || permission.kind === "checking" || permission.kind === "unsupported") {
    return null;
  }
  if (permission.kind === "granted") {
    const banners = Boolean(permission.banners);
    const badge = Boolean(permission.badge);
    if (banners && badge) {
      return { text: t("permission.granted"), severity: "ok", fix: "open_settings" };
    }
    if (!banners && badge) {
      return { text: t("permission.no_banners"), severity: "warning", fix: "open_settings" };
    }
    if (banners && !badge) {
      return { text: t("permission.no_badge"), severity: "warning", fix: "open_settings" };
    }
    return {
      text: t("permission.no_banners_or_badge"),
      severity: "warning",
      fix: "open_settings",
    };
  }
  if (permission.kind === "not_requested") {
    return { text: t("permission.not_requested"), severity: "info", fix: "request" };
  }
  if (permission.kind === "denied") {
    return { text: t("permission.denied"), severity: "error", fix: "open_settings" };
  }
  if (permission.kind === "unavailable") {
    return { text: String(permission.reason ?? ""), severity: "error", fix: null };
  }
  return null;
}
