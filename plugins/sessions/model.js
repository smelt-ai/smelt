// 会话插件纯业务逻辑（零宿主 UI 依赖，可直接由 bun test 单测）。

export const CONVERSATION_VIEW = "smelt.conversation/conversation";

export function terminalPaneKey(machine, sessionId) {
  const host = machine == null || machine === "" ? "__local__" : `remote:${machine}`;
  return `${host}\0${sessionId}`;
}

export function terminalWriteErrorKey(kind) {
  switch (kind) {
    case "overflow":
      return "terminal.input_overflow";
    case "session_ended":
      return "terminal.input_session_ended";
    case "reopened":
      return "terminal.input_reopened";
    default:
      return null;
  }
}

export function machineKey(machine) {
  return machine == null || machine === "" ? "" : String(machine);
}

export function machineFromColumn(column) {
  return !column ? null : String(column);
}

export function sameMachine(a, b) {
  return machineKey(a) === machineKey(b);
}

export function convKeyStr(machine, id) {
  return `${machineKey(machine)}\0${id}`;
}

export function revisionIsOlder(current, target) {
  if (!current || !target) return false;
  return current.epoch === target.epoch && current.revision < target.revision;
}

export function directoryVersion(directory) {
  if (!directory) return null;
  return { epoch: directory.epoch ?? "", revision: directory.revision ?? 0 };
}

export function factMapOf(facts) {
  const map = new Map();
  for (const f of facts ?? []) {
    if (f?.id) map.set(f.id, f);
  }
  return map;
}

export function conversationOfSession(directory, sessionId) {
  for (const conv of directory?.conversations ?? []) {
    if (conv.acp?.sid === sessionId) return conv;
    if ((conv.terminals ?? []).some((term) => term?.id === sessionId)) return conv;
  }
  return null;
}

export function shownConversation(directory, facts, conversationId) {
  const conv = (directory?.conversations ?? []).find((c) => c.id === conversationId);
  if (!conv) return null;
  const fact = (facts ?? []).find((f) => f.id === conversationId);
  if (fact?.placement?.kind === "hidden") return null;
  return conv;
}

export function listedRoots(directory, facts) {
  const roots = new Set();
  for (const project of directory?.projects ?? []) {
    if (project) roots.add(project);
  }
  for (const fact of facts ?? []) {
    if (fact?.placement?.kind === "project") {
      roots.add(fact.placement.root ?? "");
    }
  }
  return roots;
}

export function projectListed(directory, facts, cleanRoot, cleanFn) {
  for (const root of listedRoots(directory, facts)) {
    if (cleanFn(root) === cleanRoot) return true;
  }
  return false;
}

/**
 * 定位会话：返回 { kind: "found", machine, id } | { kind: "wait" } | { kind: "missing" }
 */
export function locateSession(machine, at, after, directory, facts) {
  if (!directory) {
    return after ? { kind: "wait" } : { kind: "missing" };
  }
  let conv = null;
  if (at?.session_id != null) {
    conv = conversationOfSession(directory, at.session_id);
  } else if (at?.conversation_id != null) {
    conv = (directory.conversations ?? []).find((c) => c.id === at.conversation_id) ?? null;
  }
  if (conv) {
    const fact = (facts ?? []).find((f) => f.id === conv.id);
    if (fact?.placement?.kind !== "hidden") {
      return { kind: "found", machine: machine ?? null, id: conv.id };
    }
  }
  if (after && revisionIsOlder(directoryVersion(directory), after)) {
    return { kind: "wait" };
  }
  return { kind: "missing" };
}

export function worksetTaskRoot(workset, joinFn) {
  return joinFn(joinFn(workset.project_root, "worktrees"), workset.task);
}

export function worksetOfCwd(worksets, cwd, { isAbsolute, clean, join, startsWith }) {
  if (!cwd || !isAbsolute(cwd)) return null;
  const cleanCwd = clean(cwd);
  for (const ws of worksets ?? []) {
    const root = clean(worksetTaskRoot(ws, join));
    if (startsWith(cleanCwd, root)) return ws.id;
  }
  return null;
}

export function contextRoot(directory, fact, joinFn) {
  const placement = fact?.placement;
  if (!placement || placement.kind === "hidden") return null;
  if (placement.kind === "project") {
    if (placement.workset) {
      const ws = (directory?.worksets ?? []).find((w) => w.id === placement.workset);
      return ws ? worksetTaskRoot(ws, joinFn) : null;
    }
    return placement.root ? placement.root : null;
  }
  if (placement.kind === "conversations") {
    return fact.cwd || null;
  }
  return null;
}

export function leadPane(conversation) {
  if (conversation?.acp?.sid) {
    return { session_id: conversation.acp.sid, kind: "conversation" };
  }
  for (const term of conversation?.terminals ?? []) {
    if (term?.id) {
      return { session_id: term.id, kind: "terminal" };
    }
  }
  return null;
}

export function paneSpecs(machine, conversation, start = null) {
  const out = [];
  const m = machine ?? null;
  if (conversation?.acp?.sid) {
    out.push({
      session_id: conversation.acp.sid,
      kind: "conversation",
      view: CONVERSATION_VIEW,
      args: {
        machine: m,
        session_id: conversation.acp.sid,
        placeholder: null,
        start: start ?? null,
      },
    });
  }
  for (const term of conversation?.terminals ?? []) {
    if (!term?.id) continue;
    out.push({
      session_id: term.id,
      kind: "terminal",
      view: null,
      args: {
        machine: m,
        session_id: term.id,
        cwd: term.cwd ?? null,
        launch: term.launch_cmd ?? null,
      },
    });
  }
  return out;
}

export function conversationTitle(conversation, fact, t) {
  if (fact?.name) return fact.name;
  if (conversation?.custom_title?.trim()) return conversation.custom_title.trim();
  return conversation?.acp ? t("kind.conversation") : t("kind.terminal");
}

export function paneTitle(conversation, kind, sessionId, states, t) {
  const state = (states ?? []).find((s) => s.id === sessionId);
  let named = null;
  if (kind === "terminal") {
    const term = (conversation?.terminals ?? []).find((item) => item?.id === sessionId);
    named = term?.custom_title || state?.name || null;
  } else {
    named = state?.conversation_name || state?.title || null;
  }
  const trimmed = typeof named === "string" ? named.trim() : "";
  if (trimmed) return trimmed;
  return kind === "terminal" ? t("kind.terminal") : t("kind.conversation");
}

export function idToCopy(directory, states, sessionId) {
  const conv = conversationOfSession(directory, sessionId);
  const history = conv?.acp?.sid === sessionId ? conv.acp.history_session_id : null;
  const reported = (states ?? []).find((s) => s.id === sessionId)?.conversation_id ?? null;
  const candidate = history || reported;
  const trimmed = typeof candidate === "string" ? candidate.trim() : "";
  return trimmed || sessionId;
}

export function blankConversation(id, worksetId = null) {
  return {
    id,
    custom_title: null,
    workset_id: worksetId ?? null,
    acp: null,
    terminals: [],
    archived_at: null,
    archived_title: null,
  };
}

export function forkedAcp(source, sid) {
  return {
    ...source,
    sid,
    history_session_id: null,
    pending_prompt: null,
    automation_id: null,
  };
}

export function findResumedConversation(directory, facts, agentId, cwd, resumeId, cleanFn) {
  const targetCwd = cleanFn(cwd);
  const factById = factMapOf(facts);
  for (const conv of directory?.conversations ?? []) {
    const acp = conv.acp;
    if (!acp) continue;
    if (acp.agent !== agentId) continue;
    if (acp.history_session_id !== resumeId) continue;
    if (!acp.cwd || cleanFn(acp.cwd) !== targetCwd) continue;
    if (factById.get(conv.id)?.placement?.kind === "hidden") continue;
    return conv.id;
  }
  return null;
}

export function extractMachineSettings(allSettings, machine, t) {
  const entry = (allSettings ?? []).find((item) => sameMachine(item?.machine, machine));
  if (!entry) {
    return { ok: false, error: t("error.agent_settings_loading") };
  }
  if (entry.state === "ready") {
    return { ok: true, settings: entry.settings };
  }
  if (entry.state === "unavailable") {
    return {
      ok: false,
      error: t("error.agent_settings_unavailable", { reason: entry.reason ?? "" }),
    };
  }
  return { ok: false, error: t("error.machine_removed") };
}

export function formatResolveError(res, t) {
  if (!res || res.ok) return "";
  if (res.code === "error.raw") return String(res.params?.message ?? "");
  return t(res.code, res.params ?? {});
}

export function shouldOfferAddMachine(hasAddMachineCmd, remotesCount) {
  return Boolean(hasAddMachineCmd) && remotesCount === 0;
}

export function childSafeArea(parentArea, touches) {
  const a = parentArea ?? {};
  const t = touches ?? { top: true, right: true, bottom: true, left: true };
  return {
    title_bar: Boolean(a.title_bar && t.top),
    top_left: t.top && t.left ? (a.top_left ?? 0) : 0,
    top_right: t.top && t.right ? (a.top_right ?? 0) : 0,
    bottom_left: t.bottom && t.left ? (a.bottom_left ?? 0) : 0,
    bottom_right: t.bottom && t.right ? (a.bottom_right ?? 0) : 0,
  };
}

export function selectionToWire(sel) {
  if (!sel || sel.kind === "none") return { kind: "none" };
  if (sel.kind === "conversation") {
    return {
      kind: "conversation",
      machine: sel.machine ?? null,
      conversation_id: sel.conversation_id,
    };
  }
  return {
    kind: "project",
    machine: sel.machine ?? null,
    root: sel.root,
  };
}

export function parseSessionArgs(raw) {
  if (!raw || typeof raw !== "object") return null;
  const machine = raw.machine === undefined ? null : raw.machine;
  if (machine !== null && typeof machine !== "string") return null;
  let at = null;
  if (typeof raw.session_id === "string" && raw.session_id.length > 0) {
    at = { session_id: raw.session_id };
  } else if (typeof raw.conversation_id === "string" && raw.conversation_id.length > 0) {
    at = { conversation_id: raw.conversation_id };
  } else {
    return null;
  }
  const after =
    raw.after && typeof raw.after === "object" && typeof raw.after.epoch === "string"
      ? { epoch: raw.after.epoch, revision: Number(raw.after.revision ?? 0) }
      : null;
  return { machine, at, after };
}
