// 通知插件的纯逻辑（不 import NSH，供 bun test 单测）：偏好开关、发过的通知、菜单栏和角标、
// 系统授权那一行。
//
// 提醒是运行时判定、宿主镜像的（sessionAttention），会话名、状态、智能体图标是宿主算好的
// （conversationFacts），这里只挑出要列的、排好、写成给人看的字。

import { ACTIVATE_SESSION, OWNER, SHOW_MAIN_WINDOW } from "./contract.js";

export const DEFAULT_PREFS = Object.freeze({
  approval: true,
  input: true,
  success: true,
  failure: true,
  bell: true,
});

/** 这类提醒发不发。终端通知、智能体发的消息是人家主动要告诉你的，没有开关；认不出的新类型也发。 */
export function prefsAllows(prefs, kind) {
  switch (kind) {
    case "approval":
    case "input":
    case "success":
    case "failure":
    case "bell":
      return Boolean(prefs[kind]);
    default:
      return true;
  }
}

/** 库里 `delivered.machine` 列：本机存空串，远程存运行时 id。 */
export function machineColumn(machine) {
  return machine == null ? "" : String(machine);
}

export function machineFromColumn(column) {
  return column ? String(column) : null;
}

/** 发过的一条通知在内存里的键：哪台机器的哪段会话。 */
export function deliveredKey(machine, sessionId) {
  return `${machineColumn(machine)}\0${sessionId}`;
}

/** 通知 id：`smelt.notifications/<机器>/<会话>`，同一段会话再来就换掉上一条。 */
export function notificationId(machine, sessionId) {
  const host = machine == null ? "local" : `remote:${machine}`;
  return `${OWNER}/${host}/${sessionId}`;
}

/**
 * 各台机器的提醒合起来：
 * - primed：收到过快照的机器（machineColumn）。没收到的说不准谁看过了。
 * - unread：有没看过的提醒的会话（deliveredKey）。
 * - bySession：要你看的会话（所有机器的），会话 id → 还等不等你处理。
 *
 * machines 是 sdk 的 machines()：[{ machine, attention }]。
 */
export function attentionIndex(machines) {
  const primed = new Set();
  const unread = new Set();
  const bySession = new Map();
  for (const entry of machines) {
    if (!Array.isArray(entry?.attention)) continue;
    const machine = entry.machine ?? null;
    primed.add(machineColumn(machine));
    for (const session of entry.attention) {
      if (session.unread != null) unread.add(deliveredKey(machine, session.session_id));
      bySession.set(session.session_id, session.action != null);
    }
  }
  return { primed, unread, bySession };
}

export const TERMINAL_MENU_ICON = "icons/terminal.svg";

const STATUS_RANK = { needs_you: 0, running: 1, idle: 2 };

/**
 * 一台机器上没归档的对话，菜单里要用到的：
 * { machine, ids（守护会话 id：对话一个，终端每格一个）, statuses（跟 ids 一一对应）, status,
 *   title（远程的接上机器名）, icon }。一格都没起过的终端不算。
 *
 * entry 是 sdk 的 machines() 一项；statusOf(state) 是宿主的 daemonStateStatus。
 */
export function machineRows(entry, deps) {
  const { t, statusOf } = deps;
  const directory = entry?.directory;
  if (!Array.isArray(directory?.conversations)) return [];
  const machine = entry.machine ?? null;
  const states = new Map((entry.states ?? []).map((state) => [state.id, state]));
  const facts = new Map((entry.facts ?? []).map((fact) => [fact.id, fact]));
  const remote = machine != null ? String(entry.name ?? machine) : null;
  const rows = [];
  for (const conversation of directory.conversations) {
    if (conversation?.archived_at != null) continue;
    const ids = conversation.acp
      ? [conversation.acp.sid]
      : (conversation.terminals ?? []).map((terminal) => terminal?.id).filter((id) => typeof id === "string" && id);
    if (ids.length === 0) continue;
    const fact = facts.get(conversation.id);
    const name =
      fact?.name || (conversation.acp ? t("fallback.conversation") : t("fallback.terminal"));
    rows.push({
      machine,
      ids,
      statuses: ids.map((id) => (states.has(id) ? statusOf(states.get(id)) : null) ?? "idle"),
      status: fact?.status ?? "idle",
      title: remote ? `${name} · ${remote}` : name,
      icon: fact?.agent?.icon ?? TERMINAL_MENU_ICON,
    });
  }
  return rows;
}

/** 含这个守护会话的对话叫什么，和菜单里同一个名字。 */
export function sessionTitle(rows, sessionId) {
  return rows.find((row) => row.ids.includes(sessionId))?.title ?? null;
}

/** `N@·M`：N 是要你看的会话数，M 是运行中的会话数。哪项是 0 就连同分隔点一起省掉，都是 0 只剩图标。 */
export function statusTitle(attention, running) {
  if (attention === 0 && running === 0) return "";
  if (running === 0) return `${attention}@`;
  if (attention === 0) return String(running);
  return `${attention}@·${running}`;
}

export function statusTooltip(attention, running, t) {
  const attentionText =
    attention === 1 ? t("tooltip.attention.one") : t("tooltip.attention.other", { count: attention });
  const runningText =
    running === 1 ? t("tooltip.running.one") : t("tooltip.running.other", { count: running });
  if (attention === 0 && running === 0) return "smelt";
  if (running === 0) return attentionText;
  if (attention === 0) return runningText;
  return t("tooltip.both", { attention: attentionText, running: runningText });
}

/**
 * 菜单栏的菜单：列出运行中、要你处理、有没看提醒的对话，要你处理的排前面。运行数按所有对话算，
 * 要你看的数按所有提醒算（对话已经关了的提醒也算，系统通知还在）。点一条跳到那段会话；分屏的
 * 终端跳到挂着提醒的那格，其次是和整段状态一样的那格。
 *
 * bySession 见 attentionIndex；palette 是主题色（need、run、faint）。
 */
export function buildMenu(rows, bySession, palette, t) {
  let running = 0;
  const listed = [];
  for (const row of rows) {
    if (row.status === "running") running += 1;
    const raised = row.ids.find((id) => bySession.has(id)) ?? null;
    if (raised === null && row.status === "idle") continue;
    const needsYou = row.status === "needs_you" || row.ids.some((id) => bySession.get(id) === true);
    const own = row.ids[row.statuses.indexOf(row.status)];
    const sessionId = raised ?? own ?? row.ids[0];
    const shown = needsYou ? "needs_you" : row.status === "running" ? "running" : "idle";
    const label = shown === "idle" ? t("status.unread") : t(`status.${shown}`);
    listed.push({
      shown,
      item: {
        type: "action",
        label: `${row.title} — ${label}`,
        icon: { png: row.icon, color: statusColor(shown, palette) },
        command: ACTIVATE_SESSION,
        args: { machine: row.machine, session_id: sessionId },
      },
    });
  }
  // sort 是稳定的：同一档里保持对话的顺序。
  listed.sort((a, b) => STATUS_RANK[a.shown] - STATUS_RANK[b.shown]);
  const items = listed.map((entry) => entry.item);
  if (items.length > 0) items.push({ type: "separator" });
  items.push({
    type: "action",
    label: t("menu.show_main_window"),
    icon: null,
    command: SHOW_MAIN_WINDOW,
    args: null,
  });
  return {
    // 用应用图标：这个就是 Smelt 自己的会话菜单。
    icon: null,
    title: statusTitle(bySession.size, running),
    tooltip: statusTooltip(bySession.size, running, t),
    items,
  };
}

function statusColor(status, palette) {
  if (status === "needs_you") return palette.need;
  if (status === "running") return palette.muted;
  return palette.faint;
}

/** Dock 角标：要你看的会话数，没有就不挂。 */
export function dockBadge(bySession) {
  return bySession.size > 0 ? String(bySession.size) : null;
}

/**
 * 「系统通知」那一行：{ text, severity: ok | warning | info | error, fix: open_settings | request | null }。
 * 还在查或这个系统没有系统通知时是 null。
 */
export function permissionLine(permission, t) {
  switch (permission?.kind) {
    case "granted": {
      const banners = Boolean(permission.banners);
      const badge = Boolean(permission.badge);
      const key =
        banners && badge
          ? "permission.granted"
          : badge
            ? "permission.no_banners"
            : banners
              ? "permission.no_badge"
              : "permission.no_banners_or_badge";
      // 全开着也给去系统设置的路：声音、锁屏这些细项在那里改。
      return { text: t(key), severity: banners && badge ? "ok" : "warning", fix: "open_settings" };
    }
    case "not_requested":
      return { text: t("permission.not_requested"), severity: "info", fix: "request" };
    case "denied":
      return { text: t("permission.denied"), severity: "error", fix: "open_settings" };
    case "unavailable":
      return { text: String(permission.reason ?? ""), severity: "error", fix: null };
    default:
      return null;
  }
}
