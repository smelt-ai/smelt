// 新建会话的菜单：从那台机器的新建目录里挑一项，点一下就在选好的位置建好。这里是菜单的状态和
// 规则，画在 new_session_view.js。不 import 宿主模块，宿主函数经 list.host 拿。
//
// 两个入口，同一份内容：
// - 项目行、工作树行的「+」：贴着按钮弹出，位置就是那一行，不显示位置。
// - 「新建会话」命令、项目菜单：没有按钮可贴，钉在窗口上方，顶上多一行位置，能换机器、
//   项目、工作树。
//
// 排法：常用在最上面、不加组标题，行尾标类型和数字键（1~9 直接建，位置固定好记，所以不按最近
// 用过重排）；下面按终端、对话分组，组里不再写类型。星标只在悬停的那行出现，与数字键共用行尾
// 同一格原位切换（鼠标移上去同时更新选中，键盘和鼠标共用一个高亮）：常用按机器存，设置页只管
// 本机，所以就地改。
//
// 新建目录（new_session.catalog）由那台机器的运行时按它自己的启动项、装没装、常用项拼好，打开时
// 和换机器时各读一次；点星标改的是那台机器的常用项，改完重读。对话的启动规格按那台机器的智能体
// 配置现算（宿主的 conversation_acp），选中时才算，配置在菜单开着期间变了也以最新的为准。
//
// 位置的名字在打开、选位置、目录和机器变了时算好存着，画的时候不读目录。

import { CREATE_CONVERSATION } from "./peers.js";
import { machine_key } from "./model.js";
import { project_name } from "./sidebar.js";
import { message } from "./util.js";

/** 段的下标：0 常用（不加标题），1 终端，2 对话。 */
export const SECTIONS = ["common", "terminal", "conversation"];

/**
 * 预选的位置：{ machine, root, workset }。给了位置路径的，是项目根就选那个项目，落在某个工作树里
 * 就选那个工作树；目录里两样都对不上（还没收到）就按项目根用。没给的按活动项目，再不行取那台
 * 机器上第一个打开的项目。
 */
export function initial_location(host, machine, path) {
  const active = host.active_context().project;
  if (machine === undefined) machine = active?.machine ?? null;
  const directory = host.workspace_directory(machine);
  if (path != null) {
    const style = host.path_style(machine);
    const clean = (value) => host.path.clean(style, value);
    const is_project = directory?.projects.some((root) => clean(root) === clean(path));
    const workset = is_project ? null : directory?.worksets.find((workset) => contains_cwd(host, style, workset, path));
    return workset
      ? { machine, root: workset.project_root, workset: workset.id }
      : { machine, root: path, workset: null };
  }
  const root =
    active && machine_key(active.machine) === machine_key(machine)
      ? active.root
      : directory?.projects[0] ?? null;
  return { machine, root, workset: null };
}

/** 工作树的任务目录：<项目根>/worktrees/<任务名>。 */
export function task_root(host, style, workset) {
  const { join } = host.path;
  return join(style, join(style, workset.project_root, "worktrees"), workset.task);
}

function contains_cwd(host, style, workset, cwd) {
  const { clean, is_absolute, starts_with } = host.path;
  return is_absolute(style, cwd) && starts_with(style, clean(style, cwd), clean(style, task_root(host, style, workset)));
}

/**
 * 按行排出各格：常用在前、不加标题；常用和下面隔一条线；终端、对话各带组标题。
 * 格是 { kind: "separator" } | { kind: "header", section } | { kind: "row", index }。
 */
export function slots(rows) {
  const out = [];
  let section = null;
  rows.forEach((row, index) => {
    if (section !== row.section) {
      if (section !== null && row.section > 0 && rows[index - 1].section === 0) out.push({ kind: "separator" });
      if (row.section > 0) out.push({ kind: "header", section: row.section });
      section = row.section;
    }
    out.push({ kind: "row", index });
  });
  return out;
}

/** 数字键 1~9 对应的常用项：常用排在最前，第 n 项就是第 n 行。 */
export function pinned_by_digit(rows, key) {
  if (!/^[1-9]$/.test(key)) return null;
  const index = Number(key) - 1;
  return rows[index]?.section === 0 ? index : null;
}

/** 常用项第 index 行的数字键（只有前 9 项有）。 */
export function digit_key(row, index) {
  return row.section === 0 && index < 9 ? String(index + 1) : null;
}

/**
 * 一份打开着的菜单。owner：{ palette: bool, redraw(), close() }——close 关掉菜单（焦点回原处）。
 * 建好就挂进 SessionList.launchers，宿主事件经 on_event 转到这里；菜单关了（不管谁关的）调
 * dispose 摘下。
 */
export class NewSession {
  constructor(list, location, owner) {
    this.list = list;
    this.owner = owner;
    this.machine = location.machine;
    this.root = location.root;
    this.workset = location.workset;
    /** 刚从这里建的工作树 { root, task }：目录推到时选上它。 */
    this.awaiting_workset = null;
    /** 机器名、位置名、位置是不是工作树：在事件里算好，画的时候只读。 */
    this.machine_name = "";
    this.location = null;
    this.in_workset = false;
    /** 新建目录读到哪了：{ state: "loading" } | { state: "ready" } | { state: "failed", error }。 */
    this.catalog = { state: "loading" };
    /** 列表里的行：{ section, action }（action 同 NewSessionAction）。 */
    this.rows = [];
    this.slots = [];
    this.selected = null;
    /** 键盘选中后要滚到的那一行：serial 变了就滚一次。 */
    this.reveal_serial = 0;
    this.error = null;
    /** 换一次机器加一；回来的结果对不上就是上一台的，丢掉。 */
    this.generation = 0;
    /** 读一次新建目录加一；点星标后连着重读几次，只认最后一次。 */
    this.catalog_request = 0;
    this.refresh_location();
    this.load();
    list.launchers.add(this);
  }

  /** 菜单关了：不再收事件，晚回来的结果也不再改它（generation 对不上）。 */
  dispose() {
    this.list.launchers.delete(this);
    this.generation += 1;
  }

  get host() {
    return this.list.host;
  }

  redraw() {
    this.owner.redraw();
  }

  /** 宿主事件（SessionList 转来的）。 */
  on_event(event) {
    switch (event.type) {
      // 连不上时读失败的，重新连上就再读一次。
      case "reconnected":
        if (machine_key(event.machine) === machine_key(this.machine) && this.catalog.state === "failed") {
          this.load();
          this.redraw();
        }
        return;
      case "sessions_changed":
      case "machines_changed":
        this.refresh_location();
        this.redraw();
        return;
    }
  }

  /** 换了机器（或刚打开）：优先同步命中缓存，超过 10 秒再静默后台刷新；没缓存才进 loading。 */
  load() {
    this.generation += 1;
    const cached = this.list.cached_catalog?.(this.machine) ?? null;
    if (cached?.catalog?.sections) {
      this.catalog = { state: "ready" };
      this.apply_sections(cached.catalog.sections);
      if (Date.now() - cached.fetchedAt > 10_000) {
        this.fetch_catalog({ refresh: false, silent: true });
      }
    } else {
      this.catalog = { state: "loading" };
      this.rows = [];
      this.slots = [];
      this.selected = null;
      this.fetch_catalog({ refresh: false, silent: false });
    }
    if (this.list.agent_settings_failure != null) {
      const generation = this.generation;
      const redraw = () => {
        if (this.generation === generation) this.redraw();
      };
      this.list.read_agent_settings().then(redraw, redraw);
    }
  }

  /** 读一次新建目录。silent 模式下已有缓存展示时出错不刷掉现有列表；成功则写入缓存并更新。 */
  fetch_catalog({ refresh = false, silent = false } = {}) {
    this.catalog_request += 1;
    const generation = this.generation;
    const request = this.catalog_request;
    const params = refresh ? { refresh: true } : {};
    return this.host.call(this.machine, "new_session.catalog", params).then(
      (catalog) => {
        this.list.cache_catalog?.(this.machine, catalog);
        if (this.generation !== generation || this.catalog_request !== request) return;
        this.catalog = { state: "ready" };
        this.apply_sections(catalog.sections);
        this.redraw();
      },
      (error) => {
        if (this.generation !== generation || this.catalog_request !== request) return;
        if (silent && this.catalog.state === "ready") return;
        this.catalog = { state: "failed", error: message(error) };
        this.rows = [];
        this.slots = [];
        this.selected = null;
        this.redraw();
      },
    );
  }

  /** 后台收到 `new_session.catalog_changed` 推送：若菜单正展示同一台机器则就地刷新。 */
  on_catalog_updated(catalog) {
    if (!catalog?.sections) return;
    if (this.catalog.state !== "ready" && this.catalog.state !== "loading") return;
    this.catalog = { state: "ready" };
    this.apply_sections(catalog.sections);
    this.redraw();
  }

  apply_sections(sections) {
    const previous = this.selected == null ? null : this.rows[this.selected];
    this.rows = SECTIONS.flatMap((name, section) => (sections[name] ?? []).map((action) => ({ section, action })));
    this.slots = slots(this.rows);
    let selected = -1;
    if (previous) {
      const key = previous.action.key;
      selected = this.rows.findIndex((row) => row.section === previous.section && row.action.key === key);
      if (selected < 0) selected = this.rows.findIndex((row) => row.action.key === key);
    }
    this.selected = selected >= 0 ? selected : this.rows.length > 0 ? 0 : null;
  }

  /** 位置下拉里选了一处。 */
  set_location(machine, root, workset) {
    const switched = machine_key(machine) !== machine_key(this.machine);
    this.machine = machine;
    this.root = root;
    this.workset = workset;
    this.awaiting_workset = null;
    this.error = null;
    this.refresh_location();
    if (switched) this.load();
    this.redraw();
  }

  /** 仓库对话框刚在 root 下建好了工作树 task：目录里有了就选上它。 */
  await_workset(root, task) {
    this.awaiting_workset = { root, task };
    this.refresh_location();
    this.redraw();
  }

  /** 按眼下的目录和机器表重算位置的名字；等着的工作树到了就选上。 */
  refresh_location() {
    const host = this.host;
    const t = host.t;
    // 本机的名字是主机名（常是一串序列号），不如「本机」好认。
    this.machine_name =
      this.machine == null
        ? t("machine.local")
        : host.machines().find((info) => info.machine === this.machine)?.name ?? t("machine.this");
    const style = host.path_style(this.machine);
    const worksets = host.workspace_directory(this.machine)?.worksets ?? [];
    const awaiting = this.awaiting_workset;
    if (awaiting) {
      const arrived = worksets.find((workset) => workset.project_root === awaiting.root && workset.task === awaiting.task);
      if (arrived) {
        this.root = awaiting.root;
        this.workset = arrived.id;
        this.awaiting_workset = null;
      }
    }
    this.in_workset = this.workset != null;
    if (this.root == null) {
      this.location = null;
      return;
    }
    const project = project_name(style, this.root, host.path, host.t);
    if (this.workset == null) {
      this.location = project;
      return;
    }
    const workset = worksets.find((workset) => workset.id === this.workset);
    this.location = `${project} / ${workset ? workset.task : t("new.workset_gone")}`;
  }

  /** 上下键：在各行里循环走，滚到选中的那行。 */
  step(delta) {
    const count = this.rows.length;
    if (count === 0) return;
    let next;
    if (this.selected != null) next = (((this.selected + delta) % count) + count) % count;
    else next = delta > 0 ? 0 : count - 1;
    this.selected = next;
    this.reveal_serial += 1;
    this.redraw();
  }

  /** 鼠标移到某一行：同步选中它（键盘和鼠标共用一个高亮）。 */
  hover(index) {
    if (this.selected === index) return;
    this.selected = index;
    this.redraw();
  }

  toggle_pin(key) {
    const generation = this.generation;
    return this.host.call(this.machine, "new_session.pins.toggle", { key }).then(
      () => {
        if (this.generation === generation) {
          return this.fetch_catalog({ refresh: false, silent: true });
        }
      },
      (error) => {
        if (this.generation !== generation) return;
        this.error = this.list.failed(this.host.t("action.save_favorites"), message(error));
        this.redraw();
      },
    );
  }

  fail(text) {
    this.error = text;
    this.redraw();
    return null;
  }

  /** 点了第 index 行（或按了它的数字键、弹出层里按回车）：建得成就关掉菜单再建，建不成留着菜单写原因。 */
  launch(index) {
    this.selected = index;
    const launch = this.prepare();
    if (!launch) return false;
    this.dispose();
    this.owner.close();
    this.run(launch);
    return true;
  }

  /** 建：先关菜单（焦点回原处），再建，新会话拿到的焦点不会被关菜单抢回去。 */
  run(launch) {
    const ran = this.host.run(CREATE_CONVERSATION, { machine: launch.machine, conversation: launch.conversation });
    this.list.report_row_failure(launch.under, this.host.t("action.new_session"), ran);
  }

  /** 按选中的那项和选好的位置拼出要建的会话：{ machine, conversation, under }。拼不成把原因写在菜单里，返回 null。 */
  prepare() {
    const row = this.selected == null ? null : this.rows[this.selected];
    if (!row) return null;
    const action = row.action;
    const host = this.host;
    const t = host.t;
    const machine = this.machine;
    const reason = host.machines().find((info) => machine_key(info.machine) === machine_key(machine))?.unavailable;
    if (reason != null) return this.fail(t("new.machine_unavailable", { machine: this.machine_name, reason }));
    const root = this.root;
    if (root == null) return this.fail(t("new.choose_project"));
    const under =
      this.workset != null ? { kind: "workset", machine, id: this.workset } : { kind: "project", machine, root };
    let cwd = root;
    if (this.workset != null) {
      const style = host.path_style(machine);
      const workset = host.workspace_directory(machine)?.worksets.find((workset) => workset.id === this.workset);
      if (!workset) return this.fail(t("new.workset_gone_choose"));
      cwd = task_root(host, style, workset);
    }
    const conversation = {
      id: host.uuid(),
      custom_title: null,
      last_updated_at: 0,
      archived_at: null,
      workset_id: this.workset,
      closed_workset_root: null,
      terminals: [],
      acp: null,
    };
    switch (action.target) {
      case "conversation": {
        const settings = this.list.agent_settings.get(machine_key(machine));
        const unavailable = settings?.state === "unavailable" ? settings.reason : settings ? null : this.list.agent_settings_failure;
        if (unavailable != null) return this.fail(t("error.agent_settings_unavailable", { reason: unavailable }));
        if (settings?.state !== "ready") return this.fail(t("error.agent_settings_loading"));
        const acp = host.conversation_acp(settings.settings, action.agentOptionId, `acp-${host.uuid()}`, cwd);
        if (!acp) return this.fail(t("new.settings_changed"));
        conversation.acp = acp;
        break;
      }
      case "terminal":
        conversation.terminals = [new_terminal(host, cwd, action.label, action.command)];
        break;
      case "blankTerminal":
        conversation.terminals = [new_terminal(host, cwd, null, null)];
        break;
    }
    return { machine, conversation, under };
  }
}

function new_terminal(host, cwd, label, command) {
  return { id: host.uuid(), cwd, custom_title: null, launch_label: label, launch_cmd: command };
}
