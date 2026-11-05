// 看板的数据整理：把宿主给的会话快照分到三列。纯函数，不碰宿主，Node 里能直接测。
//
// 列就是宿主给的三态，不往细里猜：「要你」合并了等批准、等输入和出错，「空闲」合并了
// 空闲、刚完成和已断开。宿主没给的细分，看板也不编。

/** @typedef {import("smelt-workspace").AgentSession} AgentSession */
/** @typedef {import("smelt-workspace").MachineProjects} MachineProjects */

export const COLUMNS = [
  { status: "needs_you", title: "要你处理", hint: "等批准、等输入或出错" },
  { status: "running", title: "运行中", hint: "正在干活" },
  { status: "idle", title: "空闲", hint: "空闲、已完成或已断开" },
];

/**
 * 分列。每列里未读的在前，其余按最近活动从新到旧。
 * @param {AgentSession[]} sessions
 * @param {MachineProjects[]} machines
 * @param {string | null} machine 只要这台机器（机器 id）上的会话；`null` 要全部。
 */
export function columns(sessions, machines, machine = null) {
  const names = projectNames(machines);
  const shown = machine === null ? sessions : sessions.filter((session) => session.machine.id === machine);
  return COLUMNS.map((column) => ({
    ...column,
    cards: shown
      .filter((session) => session.status === column.status)
      .map((session) => card(session, names))
      .sort((a, b) => Number(b.unread) - Number(a.unread) || b.updated_at - a.updated_at),
  }));
}

/** `机器 id\0项目根` → 侧栏显示名。 */
function projectNames(machines) {
  const names = new Map();
  for (const machine of machines) {
    for (const project of machine.projects) {
      names.set(`${machine.id}\0${project.root}`, project.name);
    }
  }
  return names;
}

/** 卡片要显示的东西，全部从快照里算好，render 只管画。 */
function card(session, names) {
  const project =
    session.project === null ? null : (names.get(`${session.machine.id}\0${session.project}`) ?? basename(session.project));
  return {
    id: session.id,
    title: session.title,
    agent: session.agent,
    unread: session.unread,
    viewing: session.viewing,
    updated_at: session.updated_at,
    // 本机不写机器名，远程的写上，离线的标出来。
    place: [project, session.machine.local ? null : session.machine.name].filter((part) => part !== null).join(" · "),
    offline: !session.machine.online,
    detail: [session.model, contextText(session.context)].filter((part) => part !== null).join(" · "),
  };
}

function contextText(context) {
  if (context === null || context.size <= 0) return null;
  return `上下文 ${Math.round((context.used / context.size) * 100)}%`;
}

function basename(path) {
  const trimmed = path.replace(/\/+$/, "");
  return trimmed.slice(trimmed.lastIndexOf("/") + 1) || trimmed;
}
