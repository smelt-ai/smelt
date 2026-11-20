// 侧栏算好的样子（Sidebar），和排序用的纯函数。只在事件里算（目录、会话状态、机器、界面状态
// 变了），render 只读它。不 import 宿主模块：宿主的数据、路径函数、t() 都从参数进来。
//
// 项目里的会话按项目（默认）、状态或时间分组；按项目时可以藏起没有会话的项目。默认所有机器的
// 项目排在一份列表里：机器只是项目的一个属性，远程项目在行上标机器名，机器连不上时它的项目变
// 灰、写为什么。挂着远程机器时可以打开「按机器分开」：一台机器一片（area），机器的标题行带连接
// 状态和菜单，片里照常按分组依据排，行上不再标机器名。

import { buckets } from "./group.js";
import * as machine_links from "./machine.js";
import {
  highest_status,
  is_collapsed,
  is_pinned,
  last_activity,
  machine_id,
  machine_key,
  machine_ui,
  same_conversation,
  session_ids,
  title,
} from "./model.js";
import { add_tally, empty_tally, tally_is_empty } from "./offscreen.js";
import { describe, merge_summary, workset_key } from "./worksets.js";

/** 「对话」一段标题的身份（它不跟着滑，只在折起来时当要盯的行用）。 */
export const CONVERSATIONS_KEY = "conversations";

export function session_flip(key) {
  return `session:${machine_id(key.machine)}:${key.id}`;
}

export function project_flip(machine, root) {
  return `project:${machine_id(machine)}:${root}`;
}

export function workset_flip(machine, id) {
  return `workset:${machine_id(machine)}:${id}`;
}

/** 按状态、时间分出来的一组的身份：不按机器分开时 machine 是 undefined。也是折叠存的键。 */
export function bucket_flip(key) {
  return key.machine === undefined
    ? `bucket:${key.bucket}`
    : `bucket:${machine_id(key.machine)}:${key.bucket}`;
}

export function machine_flip(machine) {
  return `machine:${machine_id(machine)}`;
}

/**
 * 侧栏算好的样子。选中哪行不在这里（选中变了不重算），画的时候对着选中看。
 *
 * - conversations：「对话」一段 { collapsed, status, rows }；一段跟智能体的对话都没有时是 null。
 * - projects：所有机器的项目排成一份，跨机器的手动顺序，置顶的在前。
 * - areas：「项目」标题下的几片，从上到下：不按机器分开时只有一片，分开时一台机器一片。
 * - machines：机器表里的每台机器 { machine, name, os_icon, link, received }（received：工作区目录
 *   收到了没有）。
 */
export class Sidebar {
  constructor(fields = {}) {
    this.conversations = fields.conversations ?? null;
    this.projects = fields.projects ?? [];
    this.areas = fields.areas ?? [];
    this.by_machine = fields.by_machine ?? false;
    this.machines = fields.machines ?? [];
    /** machine_key → 那台机器上项目里的会话 id，从上到下（拖会话、置顶时整份写回）。 */
    this.conversation_ids_by_machine = fields.conversation_ids_by_machine ?? new Map();
    this.grouping = fields.grouping ?? "project";
    /** 按项目分组时藏起没有会话的项目（project_visible）。 */
    this.hide_empty = fields.hide_empty ?? false;
    /** 正按名字搜项目：一个都没有时写「没有匹配的项目」而不是「没有打开的项目」。 */
    this.filtering = fields.filtering ?? false;
    /** 列表上看得见的会话，从上到下（折起来的不算）。上一段 / 下一段按它走。 */
    this.order = [];
    /** 列表上要你看一眼的行，从上到下。 */
    this.notices = [];
  }

  /** 按状态、时间分组时这段会话落在哪一组；按项目分组、或不在侧栏里时是 null。 */
  bucket_of(key) {
    for (const area of this.areas) {
      for (const group of area.buckets) {
        if (group.rows.some((row) => same_conversation(row.key, key))) return group.key;
      }
    }
    return null;
  }

  /** 这片的项目，从上到下。 */
  area_projects(area) {
    return area.projects.map((ix) => this.projects[ix]);
  }

  /** 这片里所有的会话行（折起来的也算），只取当前分组用的那一处。 */
  area_rows(area) {
    if (this.grouping !== "project") return area.buckets.flatMap((group) => group.rows);
    return this.area_projects(area).flatMap((project) => project.items.flatMap(item_rows));
  }

  visible_order() {
    const order = [];
    if (this.conversations && !this.conversations.collapsed) {
      order.push(...this.conversations.rows.map((row) => row.key));
    }
    for (const area of this.areas) {
      if (area.collapsed) continue;
      if (this.grouping !== "project") {
        for (const group of area.buckets) {
          if (!group.collapsed) order.push(...group.rows.map((row) => row.key));
        }
        continue;
      }
      // 藏起来的空项目里没有会话，不用另外跳过。
      for (const project of this.area_projects(area)) {
        if (project.collapsed) continue;
        for (const item of project.items) {
          if (item.kind === "session") order.push(item.row.key);
          else if (!item.workset.collapsed) order.push(...item.workset.sessions.map((row) => row.key));
        }
      }
    }
    return order;
  }

  /**
   * 列表上各行（项目、组、工作树、会话）从上到下的身份，折起来的也算。列表视图拿前后两份比
   * （reordered），先后变了就让各行滑到新位置；身份也是各行滑动状态挂的 id。
   */
  flip_order() {
    const order = [];
    if (this.conversations) order.push(...this.conversations.rows.map((row) => session_flip(row.key)));
    for (const area of this.areas) {
      if (area.machine !== undefined) order.push(machine_flip(area.machine));
      if (this.grouping !== "project") {
        for (const group of area.buckets) {
          order.push(bucket_flip(group.key));
          order.push(...group.rows.map((row) => session_flip(row.key)));
        }
        continue;
      }
      for (const project of this.area_projects(area)) {
        order.push(project_flip(project.machine, project.root));
        for (const item of project.items) {
          if (item.kind === "session") {
            order.push(session_flip(item.row.key));
          } else {
            order.push(workset_flip(item.workset.machine, item.workset.id));
            order.push(...item.workset.sessions.map((row) => session_flip(row.key)));
          }
        }
      }
    }
    return order;
  }

  /** 列表上要你看一眼的行，从上到下：展开着的会话行各算各的，折起来的一组算在标题上。 */
  collect_notices() {
    const notices = [];
    const push = (notice) => {
      if (notice) notices.push(notice);
    };
    const each_row = (rows) => rows.forEach((row) => push(row_notice(row)));
    if (this.conversations) {
      if (this.conversations.collapsed) push(folded(CONVERSATIONS_KEY, this.conversations.rows));
      else each_row(this.conversations.rows);
    }
    for (const area of this.areas) {
      if (area.machine !== undefined && area.collapsed) {
        push(folded(machine_flip(area.machine), this.area_rows(area)));
        continue;
      }
      if (this.grouping !== "project") {
        for (const group of area.buckets) {
          if (group.collapsed) push(folded(bucket_flip(group.key), group.rows));
          else each_row(group.rows);
        }
        continue;
      }
      for (const project of this.area_projects(area)) {
        if (project.collapsed) {
          push(folded(project_flip(project.machine, project.root), project.items.flatMap(item_rows)));
          continue;
        }
        for (const item of project.items) {
          if (item.kind === "session") push(row_notice(item.row));
          else if (item.workset.collapsed) {
            push(folded(workset_flip(item.workset.machine, item.workset.id), item.workset.sessions));
          } else each_row(item.workset.sessions);
        }
      }
    }
    return notices;
  }

  /** 要你看一眼的行，去掉正选中的那段会话：人正看着它。 */
  notices_besides(selected) {
    const key = selected ? session_flip(selected) : null;
    return this.notices.filter((notice) => notice.key !== key);
  }

  /** 侧栏里所有的会话行（折起来的也算），只取当前分组用的那一处。 */
  session_rows() {
    return [
      ...(this.conversations?.rows ?? []),
      ...this.areas.flatMap((area) => this.area_rows(area)),
    ];
  }

  /** 侧栏里有没有这段会话（折起来的也算；搜索滤掉的项目里的不算）。 */
  has_session(key) {
    return this.session_rows().some((row) => same_conversation(row.key, key));
  }

  /** 侧栏里正在跑的会话的滑动身份。 */
  running() {
    return new Set(
      this.session_rows()
        .filter((row) => row.status === "running")
        .map((row) => session_flip(row.key)),
    );
  }

  has_remote() {
    return has_remote(this.machines);
  }

  /** 列表里项目从上到下的身份（拖项目时在它上面挪）。 */
  project_keys() {
    return this.projects.map((project) => ({ machine: project.machine, root: project.root }));
  }

  /** 那台机器上项目里的会话从上到下。 */
  conversation_ids(machine) {
    return this.conversation_ids_by_machine.get(machine_key(machine)) ?? [];
  }

  /** 那一组里的会话，从上到下。 */
  group_rows(group) {
    if (group.kind === "conversations") return this.conversations?.rows ?? [];
    const project = this.projects.find(
      (candidate) => candidate.machine === group.machine && candidate.root === group.root,
    );
    if (!project) return [];
    return project.items.flatMap((item) => {
      if (item.kind === "session") return group.workset == null ? [item.row] : [];
      return item.workset.id === group.workset ? item.workset.sessions : [];
    });
  }
}

/** 机器表里有没有远程机器。 */
function has_remote(machines) {
  return machines.some((machine) => machine.machine != null);
}

/** 项目里的一项下面的会话行：一行会话，或工作集里的那些。 */
export function item_rows(item) {
  return item.kind === "session" ? [item.row] : item.workset.sessions;
}

/** 这一行算不算要你看：需要你处理的，或有没看过的提醒的。 */
export function row_tally(row) {
  if (row.status === "needs_you") return { need: 1, unread: 0 };
  if (row.unread) return { need: 0, unread: 1 };
  return empty_tally();
}

/** 这一行要你看就是一条提示，点胶囊打开它自己。 */
function row_notice(row) {
  const tally = row_tally(row);
  return tally_is_empty(tally) ? null : { key: session_flip(row.key), tally, session: row.key };
}

/**
 * 折起来的一组（标题的滑动身份是 key）合起来有几段要你看；点胶囊时打开组里第一段要你看的
 * （打开时展开这一组）。一段都没有是 null。
 */
function folded(key, rows) {
  const tally = empty_tally();
  let first = null;
  for (const row of rows) {
    const own = row_tally(row);
    if (!tally_is_empty(own) && !first) first = row.key;
    add_tally(tally, own);
  }
  return first ? { key, tally, session: first } : null;
}

/**
 * 按眼下的数据算一遍。只读内存（目录、会话状态都是宿主的内存缓存），不碰库。
 *
 * input：
 * - machines：宿主的机器表 [{ machine, name, unavailable }]。
 * - connections：宿主的连接表。local_os：这台电脑的系统（os.platform()）。
 * - data(machine)：那台机器的 { style, directory, states, facts }；目录还没收到时 directory 是 null。
 * - agent_names：machine_key → (智能体定义 id → 名字)。
 * - unread：attention.js 的 Unread。workset_checks：workset_key → check。
 * - ui：SidebarUi。project_filter：项目搜索框里的字。now：Date，按时间分组的日界按它算。
 * - t：文案。path：{ clean(style, path), file_name(style, path), join(style, base, child) }。
 */
export function build_sidebar(input) {
  const { ui, t } = input;
  const agent_rows = [];
  const projects = [];
  const machines = [];
  const conversation_ids_by_machine = new Map();
  for (const machine of input.machines) {
    const connection = input.connections.remotes.find((remote) => remote.machine === machine.machine);
    const link = machine_links.link(machine, connection, t);
    const data = input.data(machine.machine);
    if (data?.directory) {
      const machine_input = {
        machine: machine.machine,
        style: data.style,
        directory: data.directory,
        states: data.states,
        facts: data.facts,
        agents: input.agent_names.get(machine_key(machine.machine)) ?? new Map(),
        unread: input.unread,
        link,
        remote_name: machine.machine != null ? machine.name : null,
        checks: input.workset_checks,
        t,
        path: input.path,
      };
      const ids = fill_machine(machine_input, ui, projects, agent_rows);
      conversation_ids_by_machine.set(machine_key(machine.machine), ids);
    }
    machines.push({
      machine: machine.machine,
      name: machine.name,
      os_icon: machine_links.os_icon(machine.machine, input.connections, input.local_os),
      link,
      received: !!data?.directory,
    });
  }
  const ordered = pinned_first(
    manual_first(
      projects,
      ui.project_order,
      (project, key) => project.machine === key.machine && project.root === key.root,
    ),
    (project) => project.pinned,
  );
  const by_machine = ui.by_machine && has_remote(machines);
  const filter = input.project_filter;
  const areas = by_machine
    ? machines.map((machine) => area(machine.machine, ordered, ui, filter, input.now))
    : [area(undefined, ordered, ui, filter, input.now)];
  let conversations = null;
  if (agent_rows.length > 0) {
    // 稳定排序：一样新的保留目录里的顺序。
    const rows = pinned_first(
      [...agent_rows].sort((a, b) => b.activity - a.activity),
      (row) => row.pinned,
    );
    conversations = {
      collapsed: is_collapsed(ui, { kind: "conversations" }),
      status: highest_status(rows.map((row) => row.status)),
      rows,
    };
  }
  const sidebar = new Sidebar({
    conversations,
    projects: ordered,
    areas,
    by_machine,
    machines,
    conversation_ids_by_machine,
    grouping: ui.grouping,
    hide_empty: ui.hide_empty_projects,
    filtering: filter.trim() !== "",
  });
  sidebar.order = sidebar.visible_order();
  sidebar.notices = sidebar.collect_notices();
  return sidebar;
}

/**
 * 把一台机器的目录摆成项目组接到 projects 后面（按目录的顺序，跨机器的手动顺序和置顶由调用方
 * 排），跟智能体的对话放进 agent_rows（「对话」一段）。返回这台机器上项目里的会话 id，从上到下。
 */
function fill_machine(input, ui, projects, agent_rows) {
  const manual_sessions = machine_ui(ui, input.machine)?.conversation_order ?? [];
  // 项目：打开的项目按目录的顺序，再接上只靠会话工作目录撑起来的组。
  const roots = [...input.directory.projects];
  const members = new Map();
  input.directory.conversations.forEach((conversation, ix) => {
    const fact = input.facts[ix];
    if (!fact || fact.id !== conversation.id) return;
    const placement = fact.placement;
    if (placement.kind === "conversations") {
      const row = session_row(conversation, fact, input, ui, { kind: "conversations" });
      row.machine_name = input.remote_name;
      agent_rows.push(row);
    } else if (placement.kind === "project") {
      if (!roots.includes(placement.root)) roots.push(placement.root);
      if (!members.has(placement.root)) members.set(placement.root, []);
      members.get(placement.root).push({ conversation, fact, workset: placement.workset });
    }
  });
  const ids = [];
  for (const root of roots) {
    const group = project_group(root, members.get(root) ?? [], manual_sessions, input, ui);
    for (const item of group.items) ids.push(...item_rows(item).map((row) => row.key.id));
    projects.push(group);
  }
  return ids;
}

function project_group(root, members, manual_sessions, input, ui) {
  const { machine, t, path, style } = input;
  const ordered = manual_first(members, manual_sessions, (member, id) => member.conversation.id === id);
  const rows_of = (workset) =>
    pinned_first(
      ordered
        .filter((member) => (member.workset ?? null) === workset)
        .map((member) =>
          session_row(member.conversation, member.fact, input, ui, {
            kind: "project",
            machine,
            root,
            workset,
          }),
        ),
      (row) => row.pinned,
    );

  // 工作集：置顶的在普通会话前面，其余在后面；各自按目录里的位置。
  const worksets = input.directory.worksets
    .filter((workset) => workset.project_root === root)
    .sort((a, b) => a.position - b.position || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  const workset_groups = worksets.map((workset) => {
    const sessions = rows_of(workset.id);
    const check = input.checks.get(workset_key(machine, workset.id));
    let merge_lines = [];
    if (check?.last?.result) merge_lines = describe(check.last.result, t);
    else if (check?.last) {
      merge_lines = [t("error.failed", { what: t("action.check_merge"), error: check.last.error })];
    }
    const tooltip = [
      t("workset.tooltip", {
        task: workset.task,
        branch: workset.branch,
        repositories: workset.repositories.map((repository) => repository.name).join(t("list.separator")),
      }),
      ...merge_lines,
    ].join("\n");
    return {
      machine,
      id: workset.id,
      task: workset.task,
      /** 工作树的位置（新建会话时预选它）。 */
      path: path.join(style, path.join(style, workset.project_root, "worktrees"), workset.task),
      tooltip,
      pinned: is_pinned(ui, { kind: "workset", machine, id: workset.id }),
      collapsed: is_collapsed(ui, { kind: "workset", machine, id: workset.id }),
      status: highest_status(sessions.map((row) => row.status)),
      sessions,
      /** 各仓库：名字和 worktree 目录（「创建 PR」按仓库开）。 */
      repositories: workset.repositories.map((repository) => ({
        name: repository.name,
        worktree: repository.worktree,
      })),
      merge: merge_summary(check),
      checking: !!check?.running,
    };
  });
  const items = [
    ...workset_groups.filter((workset) => workset.pinned).map((workset) => ({ kind: "workset", workset })),
    ...rows_of(null).map((row) => ({ kind: "session", row })),
    ...workset_groups.filter((workset) => !workset.pinned).map((workset) => ({ kind: "workset", workset })),
  ];
  const status = highest_status(items.flatMap(item_rows).map((row) => row.status));
  // 项目根上的分支：从会话状态里工作目录就是根的那格读，不是 git 仓库是 null。
  let branch = null;
  if (root !== "") {
    const clean_root = path.clean(style, root);
    const state = input.states.find((state) => state.cwd != null && path.clean(style, state.cwd) === clean_root);
    if (state?.branch) branch = state.branch;
  }
  return {
    machine,
    root,
    name: project_name(style, root, path, t),
    /** 远程机器的名字，标在行上；本机的不标，是 null。 */
    machine_name: input.remote_name,
    /** 机器连得怎样：用不了时整行变灰、写为什么，悬停机器名看完整说明。 */
    link: input.link,
    /** 是打开的项目；不是的话关掉它只是关掉下面的会话。 */
    opened: input.directory.projects.includes(root),
    pinned: is_pinned(ui, { kind: "project", machine, root }),
    collapsed: is_collapsed(ui, { kind: "project", machine, root }),
    status,
    branch,
    /** 纳入项目的仓库数。 */
    repositories: input.directory.repositories.filter((repository) => repository.project_root === root).length,
    /** 下面的会话数，含工作集里的。 */
    sessions: members.length,
    items,
  };
}

/**
 * 一行会话：
 * - session_id：领头那一格的守护会话 id，「复制会话 ID」用。
 * - activity：最近一次动静（unix 秒）；守护里还没有它时用目录里的更新时间。
 * - turn_started_at：在跑的这一轮从什么时候开始；不在一轮里是 0。
 * - unread：有一格有没看过的提醒，运行时记的。
 * - unavailable：机器眼下用不了时行尾写的字。
 * - machine_name：不在项目下面画时标在行上的远程机器名。
 * - group：在哪一组里（拖会话只能在同一组里换位置）。
 * - origin：按状态、时间分组时行上补的来源 { project, branch, workset: { task, tooltip } | null }。
 */
function session_row(conversation, fact, input, ui, group) {
  const key = { machine: input.machine, id: conversation.id };
  const states = input.states;
  // 状态推送里没有它（守护重启后还没跑过、离线）时用守护记下的最近一轮结束时间。
  const activity = last_activity(conversation, states) || Math.max(conversation.last_updated_at ?? 0, 0);
  const status = fact.status;
  const ids = session_ids(conversation);
  // 只有在跑时行尾才写「这一轮跑了多久」；停下来的格里留着的开始时间不算。
  let turn_started_at = 0;
  if (status === "running") {
    for (const id of ids) {
      const state = states.find((state) => state.id === id);
      if (state) turn_started_at = Math.max(turn_started_at, state.turn_started_at ?? 0);
    }
  }
  const name = title(fact, !!conversation.acp, input.t);
  return {
    key,
    session_id: ids[0] ?? null,
    title: name,
    icon: row_icon(conversation, fact, input.agents),
    status,
    pinned: is_pinned(ui, { kind: "conversation", machine: input.machine, id: conversation.id }),
    activity,
    turn_started_at,
    unread: ids.some((id) => input.unread.has(input.machine, id)),
    unavailable: input.link.unavailable,
    /** 悬停的说明：完整标题（窄侧栏里常被截断）。 */
    tooltip: name,
    machine_name: null,
    group,
    origin: null,
  };
}

/** 跟哪个智能体的对话（智能体定义 id）。自动化运行的、不是跟智能体开的是 null。 */
export function agent_definition(conversation) {
  const acp = conversation.acp;
  if (!acp || acp.automation_id != null) return null;
  const id = (acp.agent_definition_id ?? "").trim();
  return id === "" ? null : id;
}

/**
 * 行首的图标：{ kind: "provider", path, label }（哪家 CLI / ACP 的单色图标，悬停看是哪家；跟智能体
 * 的对话悬停看「智能体名 · 引擎」）| { kind: "terminal" }（认不出是哪家的终端）|
 * { kind: "conversation" }（认不出是哪家的对话）。
 */
export function row_icon(conversation, fact, agents) {
  const agent = fact.agent;
  if (conversation.acp) {
    if (!agent) return { kind: "conversation" };
    // 智能体是谁看标题（建对话时写进去）和悬停；小标和别的对话一样只答「哪家」。
    const definition = agent_definition(conversation);
    let label = agent.label;
    if (definition != null) {
      const named = (agents.get(definition) ?? definition).trim();
      label = `${named === "" ? definition : named} · ${agent.label}`;
    }
    return { kind: "provider", path: agent.icon, label };
  }
  return agent ? { kind: "provider", path: agent.icon, label: agent.label } : { kind: "terminal" };
}

/**
 * 「项目」标题下的一片：machine 是 undefined 时装所有机器的项目，否则只装那台机器的。filter 是
 * 搜索框里的字，只装名字搜得到的项目；按状态、时间分组时组里也就只有这些项目的会话。
 */
export function area(machine, projects, ui, filter, now) {
  const members = [];
  projects.forEach((project, ix) => {
    if (machine !== undefined && project.machine !== machine) return;
    if (!project_matches(project.name, filter)) return;
    members.push(ix);
  });
  const in_area = members.map((ix) => projects[ix]);
  const rows = in_area.flatMap((project) => project.items.flatMap(item_rows));
  return {
    /** 按机器分开时是这片的机器（本机 null）；不分开是 undefined。 */
    machine,
    collapsed: machine !== undefined && ui.collapsed_machines.has(machine_key(machine)),
    /** 折起来时标在机器标题上的状态和会话数。 */
    status: highest_status(rows.map((row) => row.status)),
    sessions: rows.length,
    /** 这片的项目，projects 的下标，从上到下。 */
    projects: members,
    /** 按状态、时间分组时这片的组；按项目时空着。 */
    buckets: bucket_groups(machine, in_area, ui, now),
  };
}

/**
 * 按状态、时间分组时的组：projects 里的会话（含工作树里的）按项目从上到下摊平，带上来源再分组；
 * 组里置顶的在前。按项目分组时是空的。machine 是这片的机器：不按机器分开（undefined）时行上还要
 * 标远程机器名，分开时机器的标题行已经写了。
 */
export function bucket_groups(machine, projects, ui, now) {
  if (ui.grouping === "project") return [];
  const rows = [];
  for (const project of projects) {
    for (const item of project.items) {
      const workset = item.kind === "workset" ? item.workset : null;
      for (const row of item_rows(item)) {
        rows.push({
          ...row,
          origin: {
            project: project.name,
            branch: project.branch,
            workset: workset ? { task: workset.task, tooltip: workset.tooltip } : null,
          },
          machine_name: machine === undefined ? project.machine_name : row.machine_name,
        });
      }
    }
  }
  return buckets(
    ui.grouping,
    rows.map((row) => [row.status, row.activity]),
    now,
  ).map(([bucket, ixs]) => {
    const members = pinned_first(
      ixs.map((ix) => rows[ix]),
      (row) => row.pinned,
    );
    const key = { machine, bucket };
    return {
      key,
      collapsed: ui.collapsed_buckets.has(bucket_flip(key)),
      status: highest_status(members.map((row) => row.status)),
      rows: members,
    };
  });
}

/** 项目名搜不搜得到：去掉两头空白后不分大小写地包含就算；空的什么都搜得到。 */
export function project_matches(name, filter) {
  const query = filter.trim();
  return query === "" || name.toLowerCase().includes(query.toLowerCase());
}

/**
 * 按项目分组、「显示：有会话」时这个项目露不露出来：有会话或工作树的、置顶的、选中的照常显示。
 * 选中不在 Sidebar 里（选中变了不重算），所以画的时候判断。
 */
export function project_visible(hide_empty, has_items, pinned, selected) {
  return !hide_empty || has_items || pinned || selected;
}

/** 项目组的名字：根目录的最后一段；没有工作目录的那组叫「未指定目录」。 */
export function project_name(style, root, path, t) {
  if (root === "") return t("project.no_folder");
  const name = path.file_name(style, root);
  return name ? name : root;
}

// ---- 排序 ----

/**
 * 手动排过的在前（按手动顺序），其余按原来的顺序接在后面。is(item, m) 判断一项是不是手动顺序
 * 里的那一个。
 */
export function manual_first(items, manual, is) {
  return items
    .map((item, ix) => {
      const rank = manual.findIndex((m) => is(item, m));
      return { rank: rank < 0 ? Infinity : rank, ix, item };
    })
    .sort((a, b) => a.rank - b.rank || a.ix - b.ix)
    .map(({ item }) => item);
}

/** 置顶的提到前面，两边各自的相对顺序不变。 */
export function pinned_first(items, pinned) {
  return [...items.filter((item) => pinned(item)), ...items.filter((item) => !pinned(item))];
}

/**
 * 把 from 挪到 to 的前面或后面（原地改 items）。没动返回 false。eq 判断两项是不是同一个，默认 ===。
 */
export function move_near(items, from, to, before, eq = (a, b) => a === b) {
  if (eq(from, to)) return false;
  const from_ix = items.findIndex((item) => eq(item, from));
  if (from_ix < 0) return false;
  const [item] = items.splice(from_ix, 1);
  const to_ix = items.findIndex((candidate) => eq(candidate, to));
  if (to_ix < 0) {
    items.splice(from_ix, 0, item);
    return false;
  }
  const at = before ? to_ix : to_ix + 1;
  items.splice(at, 0, item);
  return at !== from_ix;
}

/**
 * 在整份顺序 all 里，把 group 那几项按 group 的新顺序换进它们原来占的位置。不在 all 里的接在
 * 最后。
 */
export function splice_group(all, group) {
  const next = group.filter((id) => all.includes(id))[Symbol.iterator]();
  const out = all.map((id) => {
    if (!group.includes(id)) return id;
    const step = next.next();
    return step.done ? id : step.value;
  });
  out.push(...group.filter((id) => !all.includes(id)));
  return out;
}

/**
 * 列表里项目换了顺序之后要存的整份顺序：列表里的（shown，所有收到目录的机器上的项目）按新顺序；
 * 存着的顺序里列表上没有的（机器没连上、目录还没收到）不丢，留在存着时它前面那一项的后面，等
 * 机器连上时回到差不多的位置。eq 判断两项是不是同一个。
 */
export function merge_project_order(stored, shown, eq = (a, b) => a === b) {
  const out = [...shown];
  let after = null;
  for (const item of stored) {
    const at = out.findIndex((candidate) => eq(candidate, item));
    if (at >= 0) {
      after = at;
    } else {
      const insert = after === null ? 0 : after + 1;
      out.splice(insert, 0, item);
      after = insert;
    }
  }
  return out;
}

/**
 * 置顶状态改了之后，把 item 挪到组里置顶区和其余之间的交界：新置顶的排在置顶区最后，取消置顶
 * 的排在其余的最前。不挪的话它会落在按原顺序算出的位置，点完要满屏找。order 是组里眼下从上到
 * 下的顺序，pinned 是改过之后的置顶判断，eq 判断两项是不是同一个（默认 ===）。
 */
export function move_to_pin_boundary(order, item, pinned, eq = (a, b) => a === b) {
  const rest = order.filter((id) => !eq(id, item));
  return [...rest.filter((id) => pinned(id)), item, ...rest.filter((id) => !pinned(id))];
}

/**
 * 前后两份身份里都有的那些，先后变了没有。只多了、少了几行（新建、关掉、折起）不算重排：那些行
 * 自己有落进、收起的动画，别的行跟着排版走。
 */
export function reordered(old_order, new_order) {
  const in_old = new Set(old_order);
  const in_new = new Set(new_order);
  const a = old_order.filter((key) => in_new.has(key));
  const b = new_order.filter((key) => in_old.has(key));
  return a.length !== b.length || a.some((key, ix) => key !== b[ix]);
}
