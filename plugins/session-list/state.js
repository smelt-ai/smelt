// 会话列表的全部内存状态，和改它的事件、操作。不 import 宿主模块：宿主函数由 main.js 收成一个
// host 对象交进来，测试里换成假的。
//
// 和会话插件的分工：会话本身归会话插件（smelt.sessions，约定照抄在 peers.js）——会话视图、选中
// 哪段会话或哪个项目、把新会话写进目录再打开。这边只画、只发请求：
// - 点会话行是打开会话视图（open-view 带 { machine, conversation_id }），会话视图打开到那段时由
//   会话插件改选中，再经 selection-changed 告诉这边。
// - 点项目行发 select-project；新建会话发 create-conversation。
// - 关会话、归档、打开 / 关掉项目、工作树的操作直接调那台机器的运行时，目录推回来列表跟着变。
//
// 状态与权威见 model.js 开头。目录、会话状态、派生事实是宿主推来的只读缓存，用时现取，不另存。
//
// 出错给人看：跟某段会话、项目、工作树有关的，挂在列表的那一行上（report_row_failure、fail_row，
// 不管是行上、菜单还是快捷键发起的）；找不到那一行的（添加项目、拖放、别的插件调过来的）才发
// 系统通知（notify_error、report_failure）。
//
// 全靠事件，不起定时器、不轮询（见 on_event）。

import * as peers from "./peers.js";
import { LIST } from "./contract.js";
import {
  folds_over,
  forget_machine_ui,
  known_ids,
  listed_session,
  machine_key,
  prune_ui,
  same_avatar_choice,
  same_conversation,
  selection_from_wire,
  selected_session,
  session_ids,
  set_collapsed,
  set_conversation_order,
  set_pinned,
  set_project_order,
} from "./model.js";
import { initial_location, NewSession } from "./new_session.js";
import { Repositories } from "./repositories.js";
import { bucket_flip, build_sidebar, project_flip, session_flip, Sidebar, ValuePool, workset_flip } from "./sidebar.js";
import { due_checks, WORKSET_GIT_TIMEOUT, workset_key } from "./worksets.js";
import { message } from "./util.js";

/** 运行时推「哪些会话有没看过的提醒」的话题。 */
/** 运行时推「新建会话菜单可用目录变了」的话题。 */
export const NEW_SESSION_CATALOG_TOPIC = "new_session.catalog_changed";
/** 运行时推「智能体群组状态变了」的话题（用于隐藏群组私有 `group_owned` 会话）。 */
export const GROUPS_CHANGED_TOPIC = "group.changed";

/**
 * 列表里能挂失败的一行，键同那一行的滑动身份：
 * { kind: "conversation", key } | { kind: "project", machine, root } | { kind: "workset", machine, id }
 */
export function fail_key(target) {
  switch (target.kind) {
    case "conversation":
      return session_flip(target.key);
    case "project":
      return project_flip(target.machine, target.root);
    case "workset":
      return workset_flip(target.machine, target.id);
  }
  throw new Error(`不认识的行：${target.kind}`);
}

/**
 * host（main.js 拼，测试里换成假的）：
 * - machines()、connections()、local_os、path_style(m)、workspace_directory(m)、session_states(m)、
 *   conversation_facts(m)、session_attention(m)、active_context()、path：{ clean, file_name, join, is_absolute, starts_with }、
 *   project_labels(style, roots)
 * - call(m, method, params, timeout_ms)、run(command, args)
 * - t(key, params)、redraw()（重画本插件的视图和插件的根）、
 *   path_kind(path) → Promise<"dir" | "file" | null>（本机路径是什么，读不了是 null）、parent_dir(path)（本机）
 * - now() → Date、uuid()、input(placeholder)（输入框的状态，见 repositories.js）、
 *   conversation_acp(settings, option_id, sid, cwd)（新对话的启动规格，见 new_session.js）
 */
export class SessionList {
  constructor(host, store, ui, avatar) {
    this.host = host;
    this.store = store;
    /** 侧栏的置顶、顺序、折叠（库里那份的缓存）。只经下面的 set_* 改。 */
    this.ui = ui;
    /** 选中的会话或项目：会话插件那份的只读缓存，只由 on_selection 换。 */
    this.selection = { kind: "none" };
    /**
     * 刚由会话插件写进目录、还没在列表里露过面的会话：列表等它出现在侧栏里时取走，播一次落进
     * 列表的动画。只由 on_created 放、列表视图取，不重画。
     */
    this.fresh = null;
    /** 要在列表里找出来的会话（reveal_in_list）：列表视图取走，滚到那一行、闪一下。 */
    this.located = null;
    /** 侧栏算好的行（列表画它，上一段 / 下一段按它的顺序）。render 只读它。 */
    this.sidebar = new Sidebar();
    /** 重算侧栏时没变的行、机器沿用上一份的对象（见 ValuePool）。 */
    this.sidebar_pool = new ValuePool();
    /** machine_key → (智能体定义 id → 名字)。权威在智能体插件，这里是只读缓存。 */
    this.agent_names = new Map();
    /**
     * machine_key → 那台机器的智能体配置（AGENT_SETTINGS 的一项：{ state: "ready", settings } |
     * { state: "unavailable", reason } | { state: "absent" }）。权威在智能体插件，这里是只读缓存，
     * 新建会话拼对话的启动规格用。还没收到的机器没有。
     */
    this.agent_settings = new Map();
    /** 上次整份读智能体配置失败的原因（给人看的一句）；读成了是 null。新建会话菜单打开时据此重读。 */
    this.agent_settings_failure = null;
    /** 开着的新建会话菜单（new_session.js），宿主事件转给它们。 */
    this.launchers = new Set();
    /** ⌘N 打开的、钉在窗口上方的那份新建会话菜单（插件的根画它）。 */
    this.palette = null;
    /** 开着的仓库对话框（repositories.js，插件的根画它）。 */
    this.repositories = null;
    /**
     * 用完的输入框状态（改名、搜索、仓库对话框的）：刚收起时绘制里还可能画到它（对话框关时再画一帧），
     * 不当场释放，下次建输入框前再放。
     */
    this.retired_inputs = [];
    /** 上一份机器表（machine_key）：机器表变了时和它比，认出真的移除了的。 */
    this.known_machines = new Set();
    /** machine_key → 上次处理过的目录版本。会话状态变了也发 sessions_changed，目录没变就不重清库。 */
    this.seen = new Map();
    /** 已经收到过 selection-changed：起来时问的那次回来得晚，就不拿旧的盖掉新的。 */
    this.heard_selection = false;
    /** workset_key → 上一次问到的合并情况（权威在运行时）。只由 inspect 改。 */
    this.workset_checks = new Map();
    /** 行首头像用哪个（库里那份的缓存）。只经 set_avatar 改。 */
    this.avatar = avatar;
    /** 「项目」标题上搜索框里的字。只在内存里。 */
    this.project_filter = "";
    /** fail_key → { what, error }：在某一行上发起、失败了的操作，画在那一行上。 */
    this.failures = new Map();
    /** machine_key → { catalog, fetchedAt }：已缓存的新建会话目录。 */
    this.catalogs = new Map();
    /** machine_key → Set<session_id>：群组私有的 `group_owned` 会话集合，主会话列表自动隐藏。 */
    this.group_owned_sessions = new Map();
  }

  /** 库读回来了：侧栏的置顶、顺序、折叠和行首头像换成库里那份。起来时调一次，在 start 之前。 */
  loaded(ui, avatar) {
    this.ui = ui;
    this.avatar = avatar;
    this.fetched_selection = false;
    this.fetched_agent_settings = false;
    this.rebuild();
  }

  has_peer_command(id) {
    return typeof this.host.command !== "function" || this.host.command(id) != null;
  }

  sync_peer_commands() {
    const host = this.host;
    const tasks = [];
    if (!this.heard_selection && !this.fetched_selection && this.has_peer_command(peers.SELECTION)) {
      this.fetched_selection = true;
      tasks.push(
        host
          .run(peers.SELECTION)
          .then((selected) => {
            if (!this.heard_selection) this.on_selection(selected);
          })
          .catch((error) => {
            this.fetched_selection = false;
            console.error(`[session-list] 问会话插件选中了什么失败：${message(error)}`);
          }),
      );
    }
    if (!this.fetched_agent_settings && this.has_peer_command(peers.AGENT_SETTINGS)) {
      this.fetched_agent_settings = true;
      tasks.push(
        this.read_agent_settings().catch((error) => {
          this.fetched_agent_settings = false;
          this.notify_error(this.failed(host.t("action.read_agent_settings"), message(error)));
        }),
      );
    }
    return Promise.all(tasks);
  }

  /** 起来时调一次：记下机器表，按眼下的目录走一遍，问会话插件选中了什么、智能体插件的配置。 */
  async start() {
    const host = this.host;
    this.known_machines = new Set(host.machines().map((machine) => machine_key(machine.machine)));
    // 宿主可能在本插件起来之前就收到了目录：先按眼下的走一遍。
    this.on_sessions_changed();
    this.refresh_worksets(false);
    await this.sync_peer_commands();
  }

  // ---- 重算 ----

  /** 重算侧栏并重画。只在事件里调。 */
  rebuild() {
    const host = this.host;
    this.sidebar = build_sidebar({
      machines: host.machines(),
      connections: host.connections(),
      local_os: host.local_os,
      data: (machine) => {
        const rawDirectory = host.workspace_directory(machine);
        if (!rawDirectory) return null;
        const hidden = this.group_owned_sessions.get(machine_key(machine));
        const directory =
          hidden && hidden.size > 0 && Array.isArray(rawDirectory.conversations)
            ? {
                ...rawDirectory,
                conversations: rawDirectory.conversations.filter(
                  (conv) => !conv?.acp?.sid || !hidden.has(conv.acp.sid),
                ),
              }
            : rawDirectory;
        return {
          style: host.path_style(machine),
          directory,
          states: host.session_states(machine),
          facts: host.conversation_facts(machine),
          attention: host.session_attention(machine),
        };
      },
      agent_names: this.agent_names,
      workset_checks: this.workset_checks,
      ui: this.ui,
      project_filter: this.project_filter,
      now: host.now(),
      t: host.t,
      path: host.path,
      pool: this.sidebar_pool,
    });
    host.redraw();
  }

  // ---- 改状态 ----

  /** 给一行挂上（failure 是 { what, error }）或摘掉（null）失败。没变就不重画。 */
  set_failure(target, failure) {
    const key = fail_key(target);
    const before = this.failures.get(key);
    if (!failure) {
      if (!before) return;
      this.failures.delete(key);
    } else {
      if (before && before.what === failure.what && before.error === failure.error) return;
      this.failures.set(key, failure);
    }
    this.host.redraw();
  }

  /** 换行首头像：写库、重画，列表跟着重建头像。 */
  set_avatar(choice) {
    if (same_avatar_choice(this.avatar, choice)) return;
    this.store.save_avatar_choice(choice);
    this.avatar = choice;
    this.host.redraw();
  }

  /** 置顶或取消。新置顶的排在同类已置顶的最后。 */
  set_pinned(pin, pinned) {
    if (!set_pinned(this.ui, pin, pinned)) return;
    this.store.set_pinned(pin, pinned);
    this.rebuild();
  }

  set_collapsed(fold, collapsed) {
    if (!set_collapsed(this.ui, fold, collapsed)) return;
    this.store.set_collapsed(fold, collapsed);
    this.rebuild();
  }

  /** 换掉项目的手动顺序（所有机器一份，整份给，拖完一次给一次）。 */
  set_project_order(order) {
    if (!set_project_order(this.ui, order)) return;
    this.store.set_project_order(order);
    this.rebuild();
  }

  /** 换掉那台机器上会话的手动顺序（整份）。 */
  set_conversation_order(machine, ids) {
    if (!set_conversation_order(this.ui, machine, ids)) return;
    this.store.set_conversation_order(machine, ids);
    this.rebuild();
  }

  /**
   * 换分组依据（「筛选与分组」菜单）。
   * TODO(待确认表结构)：分组依据、隐藏空项目还没有库里的列，每次启动是默认（按项目、全部显示）。
   */
  set_grouping(grouping) {
    if (this.ui.grouping === grouping) return;
    this.ui.grouping = grouping;
    this.rebuild();
  }

  /** 藏起或显示没有会话的项目（「筛选与分组」菜单、「已隐藏 N 个」那一行）。 */
  set_hide_empty_projects(hide) {
    if (this.ui.hide_empty_projects === hide) return;
    this.ui.hide_empty_projects = hide;
    this.rebuild();
  }

  /** 按名字搜项目（「项目」标题上的搜索框，边打边搜）。空是不搜。 */
  set_project_filter(query) {
    if (this.project_filter === query) return;
    this.project_filter = query;
    this.rebuild();
  }

  /**
   * 让侧栏露出这段会话：展开它所在的「对话」段，按项目分组时展开它的项目和工作集，按状态、时间
   * 分组时展开它那一组。从通知、命令这些侧栏外面的地方跳过来时它可能正折着；侧栏里点得到的本来
   * 就露着，什么都不改。
   */
  reveal(key) {
    const host = this.host;
    const directory = host.workspace_directory(key.machine);
    if (!directory) return;
    const ix = directory.conversations.findIndex((conversation) => conversation.id === key.id);
    if (ix < 0) return;
    const at = host.conversation_facts(key.machine)[ix]?.placement;
    if (!at) return;
    let changed = false;
    // 它在搜索滤掉的项目里：清掉搜索，不然展开了也看不见。
    if (this.project_filter !== "" && !this.sidebar.has_session(key)) {
      this.project_filter = "";
      changed = true;
    }
    for (const fold of folds_over(key.machine, at, this.ui.grouping)) {
      if (set_collapsed(this.ui, fold, false)) {
        this.store.set_collapsed(fold, false);
        changed = true;
      }
    }
    if (changed) this.rebuild();
    if (this.ui.by_machine) this.set_machine_collapsed(key.machine, false);
    const bucket = this.sidebar.bucket_of(key);
    if (bucket != null) this.set_bucket_collapsed(bucket_flip(bucket), false);
  }

  /** 折起或展开按状态、时间分出来的一组（bucket_flip 的写法）。只改内存。 */
  set_bucket_collapsed(bucket, collapsed) {
    if (toggle(this.ui.collapsed_buckets, bucket, collapsed)) this.rebuild();
  }

  /** 打开或关掉「按机器分开」。只改内存。 */
  set_by_machine(by_machine) {
    if (this.ui.by_machine === by_machine) return;
    this.ui.by_machine = by_machine;
    this.rebuild();
  }

  /** 按机器分开时折起或展开一台机器那片。只改内存。 */
  set_machine_collapsed(machine, collapsed) {
    if (toggle(this.ui.collapsed_machines, machine_key(machine), collapsed)) this.rebuild();
  }

  // ---- 跟着事件走 ----

  /**
   * 宿主的事件流（HostEvent）：
   * - sessions_changed（目录、会话状态或提醒变了）：那台机器的目录版本变了才清库；侧栏重算。工作树
   *   顺便看一眼合没合进去。
   * - window_activated：人常常是去浏览器合完 PR 回来，工作树再看一眼。
   * - machines_changed：机器表里真的消失了的（和上一份比，不是离线）忘掉；重算侧栏。
   * - locale_changed：侧栏缓存里有按语言算好的字，重算。
   * - views_changed、context_menu_items_changed：行首头像、菜单项是画的时候现取的，重画。
   * - open_paths（从访达拖进来的）：本机上是目录的当项目打开。
   * - plugin：会话插件的选中换了、新会话写进目录了，智能体插件的配置换了。
   *
   * 每一条也转给开着的新建会话菜单（launchers），它们自己挑要的。
   */
  on_event(event) {
    for (const launcher of this.launchers) launcher.on_event(event);
    switch (event.type) {
      case "sessions_changed":
        this.on_sessions_changed();
        this.refresh_worksets(false);
        return;
      case "window_activated":
        this.refresh_worksets(true);
        return;
      case "machines_changed":
        this.on_machines_changed();
        return;
      case "locale_changed":
        this.rebuild();
        return;
      // 填行首插槽的视图、别的插件登记的菜单项都在画的时候现取：变了重画就行。
      case "views_changed":
      case "context_menu_items_changed":
        this.host.redraw();
        return;
      case "commands_changed":
        void this.sync_peer_commands();
        return;
      case "runtime_topic":
        if (event.topic === NEW_SESSION_CATALOG_TOPIC) {
          this.on_catalog_topic(event.machine, event.message);
        } else if (event.topic === GROUPS_CHANGED_TOPIC) {
          this.on_groups_topic(event.machine, event.message);
        }
        return;
      case "open_paths":
        this.open_paths(event.machine, event.paths);
        return;
      case "plugin":
        if (event.name === peers.SELECTION_CHANGED) {
          this.heard_selection = true;
          this.on_selection(event.payload);
        } else if (event.name === peers.CONVERSATION_CREATED) {
          this.on_created(event.payload);
        } else if (event.name === peers.AGENT_SETTINGS_CHANGED) {
          this.on_agent_settings(event.payload);
        }
        return;
    }
  }

  /** 宿主每台机器只留最新的一份目录，所以这里不用防旧推送：目录里没了就是真没了。 */
  on_sessions_changed() {
    const host = this.host;
    for (const { machine } of host.machines()) {
      const directory = host.workspace_directory(machine);
      if (!directory) continue;
      const version = `${directory.epoch}\u0000${directory.revision}`;
      const key = machine_key(machine);
      if (this.seen.get(key) === version) continue;
      const known = known_ids(directory, host.conversation_facts(machine));
      prune_ui(this.ui, machine, known);
      this.store.prune(machine, known);
      this.seen.set(key, version);
    }
    this.rebuild();
  }

  cached_catalog(machine) {
    return this.catalogs.get(machine_key(machine)) ?? null;
  }

  cache_catalog(machine, catalog) {
    if (!catalog || typeof catalog !== "object" || !catalog.sections) return;
    this.catalogs.set(machine_key(machine), { catalog, fetchedAt: Date.now() });
  }

  on_groups_topic(machine, message) {
    const payload = message?.payload ?? message;
    const ids = Array.isArray(payload?.group_owned_session_ids)
      ? payload.group_owned_session_ids.filter((id) => typeof id === "string" && id.length > 0)
      : null;
    if (ids == null) return;
    const key = machine_key(machine);
    const prev = this.group_owned_sessions.get(key);
    const next = new Set(ids);
    if (prev && prev.size === next.size && [...next].every((id) => prev.has(id))) {
      return;
    }
    if (next.size === 0) {
      this.group_owned_sessions.delete(key);
    } else {
      this.group_owned_sessions.set(key, next);
    }
    this.rebuild();
  }

  apply_catalog(machine, catalog) {
    if (!catalog || typeof catalog !== "object" || !catalog.sections) return;
    this.cache_catalog(machine, catalog);
    const key = machine_key(machine);
    for (const launcher of this.launchers) {
      if (machine_key(launcher.machine) === key && typeof launcher.on_catalog_updated === "function") {
        launcher.on_catalog_updated(catalog);
      }
    }
  }

  on_catalog_topic(machine, message) {
    if (!message || (message.kind !== "snapshot" && message.kind !== "event")) return;
    const catalog = message.payload;
    if (!catalog || typeof catalog !== "object" || !catalog.sections || typeof catalog.sections !== "object") {
      return;
    }
    this.apply_catalog(machine, catalog);
  }

  on_machines_changed() {
    const current = new Set(this.host.machines().map((machine) => machine_key(machine.machine)));
    for (const key of this.known_machines) {
      if (current.has(key)) continue;
      const machine = key === "" ? null : key;
      forget_machine_ui(this.ui, machine);
      this.store.forget_machine(machine);
      this.seen.delete(key);
      this.catalogs.delete(key);
    }
    this.known_machines = current;
    // 离线、改名也会走到这里：机器行要跟着变。
    this.rebuild();
  }

  /** 会话插件那边的选中换了（或又选了一次同一段）：换掉缓存，选中的会话在侧栏里露出来。 */
  on_selection(selected) {
    this.selection = selection_from_wire(selected);
    const key = selected_session(this.selection);
    if (key) this.reveal(key);
    this.host.redraw();
  }

  on_created(created) {
    this.fresh = { machine: created.machine, id: created.conversation_id };
  }

  /** 换掉一台机器的智能体名字。给不了、移除了的那台就没有名字，会话行不标。 */
  /**
   * 整份读一次智能体配置。推送先到的机器不拿这份盖掉（推送更新）。失败记下原因再 reject：新建
   * 会话菜单写出原因，下次打开再读。
   */
  read_agent_settings() {
    const host = this.host;
    return host.run(peers.AGENT_SETTINGS).then(
      (all) => {
        this.agent_settings_failure = null;
        for (const machine of all ?? []) {
          if (!this.agent_settings.has(machine_key(machine.machine))) this.on_agent_settings(machine);
        }
      },
      (error) => {
        this.agent_settings_failure = host.t("error.read_agent_settings", { error: message(error) });
        throw error;
      },
    );
  }

  on_agent_settings(changed) {
    const key = machine_key(changed.machine);
    this.agent_settings.set(key, changed);
    const names =
      changed.state === "ready"
        ? new Map(changed.settings.agents.map((agent) => [agent.id, agent.name.trim()]))
        : null;
    const before = this.agent_names.get(key);
    const unchanged = names ? before != null && same_map(before, names) : before == null;
    if (unchanged) return;
    if (names) this.agent_names.set(key, names);
    else this.agent_names.delete(key);
    this.rebuild();
  }

  // ---- 工作树的合并情况 ----
  //
  // 关闭前、窗口回到前台时问运行时（workspace.workset.inspect，会先 fetch 各仓库的基线），结果
  // 缓存在 workset_checks 里给侧栏画。什么时候问全由事件决定：目录里出现了还没问过的工作树、
  // 窗口回到前台（上次问过超过 RECHECK_AFTER 的再问）、用户点「检查合并状态」「关闭工作树」。
  // fetch 走网络，所以同一棵树同时只问一次，间隔也有下限。

  /** 问一次合并情况，结果记进缓存、重算侧栏，也交给调用方。同一棵树已经在问时也照问：关闭要等这次的结果。 */
  async inspect(machine, id) {
    const key = workset_key(machine, id);
    const check = this.workset_checks.get(key) ?? { running: false, last: null };
    check.running = true;
    this.workset_checks.set(key, check);
    let result;
    let error;
    try {
      result = await this.host.call(machine, "workspace.workset.inspect", { workset_id: id }, WORKSET_GIT_TIMEOUT);
    } catch (failure) {
      error = message(failure);
    }
    const done = this.workset_checks.get(key) ?? check;
    done.running = false;
    done.last = error === undefined ? { at: Date.now(), result } : { at: Date.now(), error };
    this.workset_checks.set(key, done);
    this.rebuild();
    if (error !== undefined) throw new Error(error);
    return result;
  }

  /** 目录里每棵工作树要不要问：stale 时连问过但超过 RECHECK_AFTER 的也问，否则只问还没问过的。顺手丢掉不在目录里的。 */
  refresh_worksets(stale) {
    const present = [];
    for (const { machine } of this.host.machines()) {
      const directory = this.host.workspace_directory(machine);
      if (!directory) continue;
      present.push(...directory.worksets.map((workset) => ({ machine, id: workset.id })));
    }
    const keep = new Set(present.map(({ machine, id }) => workset_key(machine, id)));
    for (const key of [...this.workset_checks.keys()]) {
      if (!keep.has(key)) this.workset_checks.delete(key);
    }
    for (const { machine, id } of due_checks(present, this.workset_checks, stale, Date.now())) {
      this.inspect(machine, id).catch(() => {});
    }
  }

  // ---- 列表上发起的事 ----
  //
  // 都回一个 Promise，失败由调用方挂到发起的那一行上。

  /** 在主窗口里露出这段会话、焦点给它：主窗口拿到前面（关着就先开），再打开会话视图。 */
  async open_conversation(key) {
    await this.host.run("smelt.app/show-main-window");
    const opened = await this.host.run("smelt.app/open-view", {
      view: peers.SESSION,
      args: { machine: key.machine, conversation_id: key.id },
    });
    if (!opened) throw new Error(this.host.t("error.no_room"));
  }

  /** 选中一个已经在列表里的项目。 */
  select_project(machine, root) {
    return this.host.run(peers.SELECT_PROJECT, { machine, root });
  }

  /** 关会话：从目录里删掉。运行时删之前先收掉它的终端和对话，这边不另外杀。要不要先确认归调用方。 */
  async close_conversation(key) {
    await this.host.call(key.machine, "workspace.conversation.delete", { conversation_id: key.id });
  }

  /** 归档一段会话。自动化运行的运行时会拒绝。 */
  async archive_conversation(key) {
    await this.host.call(key.machine, "workspace.conversation.archive", {
      conversation_id: key.id,
      archived: true,
    });
  }

  /**
   * 行内改名存下：target 同 fail_key 的那几种（会话、工作树），original 是开始改时的名字，value 是
   * 输入框里的字。没改就什么都不做。会话清空是不要自己起的名字，回到运行时算的；工作树一定要有
   * 名字，清空不存。失败挂在那一行上。
   */
  commit_rename(target, original, value) {
    const text = value.trim();
    if (text === original.trim()) return;
    const t = this.host.t;
    if (target.kind === "conversation") {
      const renamed = this.host.call(target.key.machine, "workspace.rename", {
        conversation_id: target.key.id,
        terminal_id: null,
        title: text === "" ? null : text,
      });
      this.report_row_failure(target, t("action.rename"), renamed);
      return;
    }
    if (text === "") return;
    const renamed = this.host.call(target.machine, "workspace.workset.rename_preserving_branch", {
      workset_id: target.id,
      task: text,
    });
    this.report_row_failure(target, t("action.rename_workset"), renamed);
  }

  /** 打开一个项目并选中它（已经开着就只选中）。 */
  async open_project(machine, raw_root) {
    const host = this.host;
    const style = host.path_style(machine);
    const root = host.path.clean(style, raw_root.trim());
    if (root === "") throw new Error(host.t("error.no_project_dir"));
    const open = (host.workspace_directory(machine)?.projects ?? []).some(
      (project) => host.path.clean(style, project) === root,
    );
    if (open) return this.select_project(machine, root);
    const revision = await host.call(machine, "workspace.project.open", { root });
    // 刚打开的项目要等目录推到这一版才在列表里：带上版本，会话插件等到了再认。
    return host.run(peers.SELECT_PROJECT, { machine, root, after: revision });
  }

  /**
   * 关掉一个项目：先一段段关掉侧栏里摆在它下面的会话（含它的工作集里的），都关掉了再从目录里
   * 关项目；中途有关不掉的就停下报错，项目留着。只靠会话工作目录撑起来的组（不在打开的项目里）
   * 关完会话就没了。正选着它的话，目录推到后会话插件自己清掉选中。要不要先确认归调用方。
   */
  async close_project(machine, raw_root) {
    const host = this.host;
    const style = host.path_style(machine);
    const root = host.path.clean(style, raw_root.trim());
    if (root === "") throw new Error(host.t("error.no_project_dir"));
    const directory = host.workspace_directory(machine);
    if (!directory) throw new Error(host.t("error.directory_pending"));
    const closing = conversations_under(directory, host.conversation_facts(machine), (placed) =>
      host.path.clean(style, placed) === root,
    );
    const opened = directory.projects.find((project) => host.path.clean(style, project) === root);
    for (const conversation_id of closing) {
      await host.call(machine, "workspace.conversation.delete", { conversation_id });
    }
    if (opened != null) await host.call(machine, "workspace.project.close", { root: opened });
  }

  /** 「复制会话 ID」放进剪贴板的字，见 id_to_copy。剪贴板在点击回调的 cx 上，由菜单写。 */
  session_id_to_copy(machine, session_id) {
    return id_to_copy(this.host.workspace_directory(machine), this.host.session_states(machine), session_id);
  }

  /**
   * 访达拖进来、「用 Smelt 打开」的路径：是目录的当项目打开，文件留给文件打开者。只认本机：别的
   * 机器上的路径这边判断不了是不是目录。
   */
  async open_paths(machine, paths) {
    if (machine != null) {
      console.error(`[session-list] 只认本机上拖进来的目录，忽略 ${paths.length} 条路径`);
      return;
    }
    const dirs = [];
    for (const path of paths) {
      if (!dirs.includes(path) && (await this.host.path_kind(path).catch(() => null)) === "dir") dirs.push(path);
    }
    for (const dir of dirs) {
      this.report_failure(this.host.t("action.open_project"), this.open_project(null, dir));
    }
  }

  // ---- 输入框状态 ----

  retire_input(state) {
    this.retired_inputs.push(state);
  }

  /** 放掉收起了的输入框状态。只在事件里调，不在绘制里。 */
  release_retired_inputs() {
    for (const state of this.retired_inputs.splice(0)) state.release();
  }

  // ---- 插件的根画的对话框 ----

  /** ⌘N：钉在窗口上方的新建会话菜单，预选眼下活动项目。已经开着就换一份新的（重读目录）。 */
  open_palette() {
    this.palette?.dispose();
    const owner = { palette: true, redraw: () => this.host.redraw(), close: () => this.close_palette(menu) };
    const menu = new NewSession(this, initial_location(this.host, undefined, null), owner);
    this.palette = menu;
    this.host.redraw();
  }

  close_palette(menu) {
    menu.dispose();
    if (this.palette !== menu) return;
    this.palette = null;
    this.host.redraw();
  }

  /**
   * 项目的仓库对话框：manage 管理仓库，create 直接到建工作树那一步。created(task) 在工作树建好后
   * 调（新建会话菜单据此等目录推到后选上它）。叠在已开的对话框上面。
   */
  open_repositories(machine, root, mode, created = null) {
    this.release_retired_inputs();
    this.repositories?.close();
    this.repositories = new Repositories(this, machine, root, mode, created);
    this.host.redraw();
  }

  close_repositories(dialog) {
    for (const state of dialog.fields()) this.retire_input(state);
    if (this.repositories !== dialog) return;
    this.repositories = null;
    this.host.redraw();
  }

  // ---- 命令 ----

  /**
   * 在列表里找出这段会话：展开它所在的组（搜索滤掉了就清掉搜索），列表视图滚过去、闪一下；侧栏
   * 收着时 open-view 把它打开。
   */
  async reveal_in_list(args) {
    const host = this.host;
    const key = listed_session(
      args.machine,
      args.session_id,
      host.workspace_directory(args.machine),
      host.conversation_facts(args.machine),
    );
    if (!key) throw new Error(host.t("error.not_in_list"));
    this.reveal(key);
    this.located = key;
    host.redraw();
    const opened = await host.run("smelt.app/open-view", { view: LIST, args: null });
    if (!opened) throw new Error(host.t("error.no_room_list"));
  }

  /** 焦点交给会话列表，列表被挡着就先让它露出来。 */
  async focus_list() {
    const opened = await this.host.run("smelt.app/open-view", { view: LIST, args: { focus: true } });
    if (!opened) throw new Error(this.host.t("error.no_room_list"));
  }

  /** 在本机选一个或几个目录当项目打开。没选是撤销，不算失败。 */
  async pick_projects() {
    const t = this.host.t;
    const roots =
      (await this.host.run("smelt.app/pick-paths", {
        machine: null,
        title: t("pick.title"),
        confirm: t("pick.confirm"),
        files: false,
        multiple: true,
      })) ?? [];
    const results = await Promise.allSettled(roots.map((root) => this.open_project(null, root)));
    const failed = results.filter((result) => result.status === "rejected").map((result) => message(result.reason));
    if (failed.length) throw new Error(failed.join("；"));
  }

  /** 按列表上的顺序切到上一段 / 下一段会话，到头绕回。 */
  async step_session(forward) {
    const next = step(this.sidebar.order, selected_session(this.selection), forward);
    if (next) await this.open_conversation(next);
  }

  // ---- 出错给人看 ----

  failed(what, error) {
    return this.host.t("error.failed", { what, error });
  }

  /** 记日志，发一条系统通知。只给找不到落点的失败用。 */
  notify_error(text) {
    console.error(`[session-list] ${text}`);
    this.host
      .run("smelt.app/notify", { subtitle: this.host.t("error.title"), body: text })
      .catch((error) => console.error(`[session-list] 发通知失败：${message(error)}`));
  }

  /** 等一件事做完，失败了按 what（「关会话」这类）报给人看。 */
  report_failure(what, ran) {
    Promise.resolve(ran).catch((error) => this.notify_error(this.failed(what, message(error))));
  }

  /**
   * 列表里某一行发起的操作：先摘掉这一行上次的失败（这是新的一次），等它做完，失败了挂在那一行
   * 上，不发系统通知——人就在看这一行。
   */
  report_row_failure(target, what, ran) {
    this.set_failure(target, null);
    Promise.resolve(ran).catch((error) => this.fail_row(target, what, message(error)));
  }

  /** 把一次失败挂到列表的那一行上（记日志，不发系统通知）。 */
  fail_row(target, what, error) {
    console.error(`[session-list] ${this.failed(what, error)}`);
    this.set_failure(target, { what, error });
  }
}

/** 侧栏里摆在项目下面的会话（is_root 认放着的根是不是那个项目）。 */
export function conversations_under(directory, facts, is_root) {
  return directory.conversations
    .filter((_, ix) => {
      const placement = facts[ix]?.placement;
      return placement?.kind === "project" && is_root(placement.root);
    })
    .map((conversation) => conversation.id);
}

/**
 * 复制会话 id 时给的：agent 自己的对话 id（续接身份、或 hook 报来的那段对话），拿去 --resume、
 * 找存档才用得上；agent 还没报上来时是守护会话 id。握手、TUI 里 /resume 都会换 id，所以用时现取。
 * 和会话插件的「复制会话 id」同一个规则，两边各算一份。
 */
export function id_to_copy(directory, states, session_id) {
  const conversation = directory?.conversations.find((candidate) =>
    session_ids(candidate).includes(session_id),
  );
  const acp = conversation?.acp;
  const history = acp && acp.sid === session_id ? acp.history_session_id : null;
  const reported = () => states.find((state) => state.id === session_id)?.conversation_id;
  const id = (history ?? reported() ?? "").trim();
  return id === "" ? session_id : id;
}

/**
 * 上一段 / 下一段：按列表上从上到下的顺序，到头绕回。还没选中的（或选中的已经不在列表里），下一段
 * 是第一段，上一段是最后一段。列表是空的是 null。
 */
export function step(order, current, forward) {
  const count = order.length;
  if (count === 0) return null;
  const at = current ? order.findIndex((key) => same_conversation(key, current)) : -1;
  if (at < 0) return order[forward ? 0 : count - 1];
  return order[forward ? (at + 1) % count : (at + count - 1) % count];
}

function toggle(set, key, on) {
  if (set.has(key) === on) return false;
  if (on) set.add(key);
  else set.delete(key);
  return true;
}

function same_map(a, b) {
  if (a.size !== b.size) return false;
  for (const [key, value] of a) if (b.get(key) !== value) return false;
  return true;
}

