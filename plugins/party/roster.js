// 「队伍」的数据整理：机器 → 项目 → 智能体。纯函数，不碰宿主，Node 里能直接测。
//
// 只用宿主给的事实：状态是宿主的三态，「体力」是上下文还剩多少（宿主没报用量就不画），
// 不按时间推算任何东西，所以界面不需要定时器。

/** @typedef {import("smelt-workspace").AgentSession} AgentSession */
/** @typedef {import("smelt-workspace").MachineProjects} MachineProjects */

/** 宿主三态的显示名和色调（色调是主题色名，界面去取）。 */
export const STATUS = {
  needs_you: { label: "要你", tone: "destructive" },
  running: { label: "干活中", tone: "primary" },
  idle: { label: "待命", tone: "muted_foreground" },
};

/** 项目以外的会话（没挂到任何项目上的）归到这一组。 */
export const LOOSE = "";

/**
 * 按侧栏顺序排好的队伍。每台机器下是它的项目，每个项目下是挂在上面的会话；
 * 项目以外的会话放在这台机器最后一组。没有会话、也不是打开过的项目不显示。
 * @param {MachineProjects[]} machines
 * @param {AgentSession[]} sessions
 */
export function roster(machines, sessions) {
  return machines.map((machine) => {
    const own = sessions.filter((session) => session.machine.id === machine.id);
    const projects = machine.projects
      .map((project) => ({
        root: project.root,
        name: project.name,
        active: project.active,
        opened: project.opened,
        members: own.filter((session) => session.project === project.root).map(member),
      }))
      .filter((project) => project.opened || project.members.length > 0);
    const known = new Set(machine.projects.map((project) => project.root));
    const loose = own.filter((session) => session.project === null || !known.has(session.project)).map(member);
    if (loose.length > 0) {
      projects.push({ root: LOOSE, name: "其他", active: false, opened: false, members: loose });
    }
    return {
      id: machine.id,
      name: machine.name,
      local: machine.local,
      online: machine.online,
      projects,
      // 机器标题上的小结：多少个在等你。
      waiting: own.filter((session) => session.status === "needs_you").length,
    };
  });
}

/** 一张成员卡要显示的东西，全部算好，render 只管画。 */
export function member(session) {
  return {
    id: session.id,
    title: session.title,
    agent: session.agent,
    initial: initial(session.agent ?? session.kind),
    status: session.status,
    unread: session.unread,
    viewing: session.viewing,
    model: session.model,
    stamina: stamina(session.context),
  };
}

/**
 * 体力 = 上下文还剩多少，0 到 1。宿主没报用量（或窗口大小未知）时是 null，界面不画条。
 * @param {import("smelt-workspace").ContextUsage | null} context
 */
export function stamina(context) {
  if (context === null || !(context.size > 0)) return null;
  return Math.min(1, Math.max(0, 1 - context.used / context.size));
}

/** 头像上的字：智能体 id 的首字母。 */
function initial(name) {
  return name.slice(0, 1).toUpperCase();
}

/**
 * 新建选项排成一列：常用在前，其后对话、终端；同一个 key 只出现一次。
 * @param {import("smelt-workspace").NewSessionOptions} options
 */
export function launchChoices(options) {
  const seen = new Set();
  const choices = [];
  for (const option of [...options.common, ...options.conversation, ...options.terminal]) {
    if (seen.has(option.key)) continue;
    seen.add(option.key);
    choices.push({ key: option.key, label: `${option.label} · ${option.kind === "conversation" ? "对话" : "终端"}` });
  }
  return choices;
}
