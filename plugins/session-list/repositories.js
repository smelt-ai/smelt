// 项目的仓库对话框的状态和规则（样子在 repositories_view.js）：看项目里纳入了哪些仓库，纳入已有的、
// 从 Git URL 克隆、取消关联，以及挑几个仓库建一个工作树（多仓库工作集）。
//
// 文件系统的活都在项目所在那台机器的运行时上做；这里只发调用，拿回整份目录更新列表。写入（克隆、
// 建工作树）可能要几分钟，跑着的时候框关不掉，也不接新的操作。
//
// 不 import 宿主模块：宿主的函数从 list.host 取（见 main.js），测试换成假的。输入框的状态也由宿主
// 建（host.input），这里只用 value / set_value / on / release。

import { message } from "./util.js";

/** 克隆、建工作树会跑 git 和初始化脚本，大仓库要很久。 */
export const WRITE_TIMEOUT_MS = 30 * 60 * 1000;

/**
 * manage：列出仓库，纳入、移除。create：建工作树。bind：纳入已有的仓库。clone：从 Git URL 克隆。
 * @typedef {"manage" | "create" | "bind" | "clone"} Mode
 */

export class Repositories {
  /**
   * @param {import("./state.js").SessionList} list
   * @param {(task: string) => void | null} created 工作树建好了，参数是任务名。新建会话的框据此等目录推到后选上它。
   */
  constructor(list, machine, project_root, mode, created) {
    const host = list.host;
    this.list = list;
    this.host = host;
    this.machine = machine;
    this.project_root = project_root;
    /** @type {Mode} */
    this.mode = mode;
    /** 打开时的样子：从这里进的建工作树没有「返回」。 */
    this.entry = mode;
    /** 纳入、克隆做完（或点「返回」）回到哪。 */
    this.return_mode = mode;
    this.created = created;
    this.busy = false;
    this.error = null;
    /** 纳入时选好的目录。 */
    this.source = null;
    /** 等人确认取消关联的仓库名。 */
    this.pending_remove = null;
    /** 框还开着。关了以后回来的结果不再去关框。 */
    this.open = true;
    const field = (placeholder) => {
      const state = host.input(host.t(placeholder));
      state.on("change", () => this.redraw());
      // 在输入框里回车等于点主按钮。
      state.on("submit", () => this.submit());
      return state;
    };
    this.task = field("repo.task_placeholder");
    this.branch = field("repo.branch_placeholder");
    this.base = field("repo.base_placeholder");
    this.name = field("repo.name_placeholder");
    this.url = field("repo.url_placeholder");
    // 先拿目录里现有的一份垫着，探查回来再换。
    const directory = host.workspace_directory(machine);
    this.repositories = (directory?.repositories ?? []).filter((repo) => repo.project_root === project_root);
    this.worksets = (directory?.worksets ?? []).filter((workset) => workset.project_root === project_root);
    /** 建工作树勾选的仓库名。 */
    this.selected = new Set(this.repositories.map((repo) => repo.name));
    this.repository_action({ kind: "discover" }, mode);
  }

  redraw() {
    this.host.redraw();
  }

  /** 输入框的状态；框关了交给列表，等不在绘制里时释放。 */
  fields() {
    return [this.task, this.branch, this.base, this.name, this.url];
  }

  repository_name() {
    const name = this.name.value().trim();
    if (name) return name;
    return suggested_name(this.mode === "clone" ? this.url.value() : (this.source ?? ""));
  }

  /** 「返回」回到哪；没有就不显示。 */
  back() {
    switch (this.mode) {
      case "manage":
        return null;
      case "create":
        return this.entry === "manage" ? "manage" : null;
      default:
        return this.return_mode;
    }
  }

  /** @param {Mode} mode */
  switch(mode) {
    if (this.busy) return;
    if (mode === "bind" || mode === "clone") {
      this.return_mode = this.mode;
      this.source = null;
      this.name.set_value("");
      this.url.set_value("");
    }
    this.mode = mode;
    this.error = null;
    this.pending_remove = null;
    this.redraw();
  }

  go_back() {
    if (this.busy) return;
    const mode = this.back();
    if (mode == null) return;
    this.mode = mode;
    this.error = null;
    this.redraw();
  }

  /** 建工作树时勾、取消一个仓库。 */
  toggle(name, checked) {
    if (this.busy) return;
    if (checked) this.selected.add(name);
    else this.selected.delete(name);
    this.redraw();
  }

  ask_remove(name) {
    if (this.busy) return;
    this.pending_remove = name;
    this.redraw();
  }

  keep() {
    if (this.busy) return;
    this.pending_remove = null;
    this.redraw();
  }

  confirm_remove() {
    if (this.pending_remove == null) return;
    this.repository_action({ kind: "remove", name: this.pending_remove }, "manage");
  }

  /** 在那台机器上选一个已有仓库的根目录。 */
  async choose_folder() {
    if (this.busy) return;
    const t = this.host.t;
    let result;
    try {
      const paths = await this.host.run("smelt.app/pick-paths", {
        machine: this.machine,
        title: t("repo.choose_title"),
        confirm: t("repo.choose_confirm"),
        files: false,
        multiple: false,
      });
      result = { paths };
    } catch (error) {
      result = { error: message(error) };
    }
    if (this.busy || this.mode !== "bind") return;
    if (result.error != null) this.error = result.error;
    else if (result.paths?.[0] != null) {
      this.source = result.paths[0];
      this.error = null;
    }
    this.redraw();
  }

  async repository_action(action, next_mode) {
    if (this.busy) return;
    this.busy = true;
    this.error = null;
    const first = action.kind === "discover";
    const params = { project_root: this.project_root, action };
    const ran = this.host.call(this.machine, "workspace.project.repository.action", params, WRITE_TIMEOUT_MS);
    this.redraw();
    try {
      const directory = await ran;
      this.update_catalog(directory, first);
      this.mode = next_mode;
      this.pending_remove = null;
    } catch (error) {
      this.error = message(error);
    } finally {
      this.busy = false;
      this.redraw();
    }
  }

  /** 主按钮、回车。管理时是「完成」，关框。 */
  submit() {
    if (this.busy) return;
    const t = this.host.t;
    switch (this.mode) {
      case "manage":
        this.close();
        return;
      case "create":
        this.create_workset();
        return;
      case "bind":
        if (this.source == null) {
          this.error = t("repo.choose_first");
          this.redraw();
          return;
        }
        this.repository_action({ kind: "bind", source: this.source, name: this.repository_name() }, this.return_mode);
        return;
      case "clone":
        this.repository_action({ kind: "clone", url: this.url.value(), name: this.repository_name() }, this.return_mode);
    }
  }

  async create_workset() {
    const t = this.host.t;
    const task = this.task.value().trim();
    if (!valid_task(task) || this.selected.size === 0) {
      this.error = t("repo.invalid_task");
      this.redraw();
      return;
    }
    const branch = this.branch.value().trim();
    const base = this.base.value().trim();
    const params = {
      project_root: this.project_root,
      task,
      branch_prefix: "feat",
      branch: branch || null,
      // 留空不带：运行时按各仓库远端的默认分支拉。
      ...(base && { base }),
      repository_names: [...this.selected].sort(),
    };
    const ran = this.host.call(this.machine, "workspace.workset.create_with_branch", params, WRITE_TIMEOUT_MS);
    this.busy = true;
    this.error = null;
    this.redraw();
    let created;
    try {
      created = await ran;
    } catch (error) {
      this.busy = false;
      this.error = message(error);
      this.redraw();
      return;
    }
    // 工作树已经建好，初始化脚本失败不算新建失败：框照常关，失败单独报。
    for (const setup of created?.setup ?? []) {
      if (!setup.success) {
        this.list.notify_error(t("repo.setup_failed", { repository: setup.repository, output: setup.output }));
      }
    }
    this.busy = false;
    this.created?.(task);
    if (this.open) this.close();
    else this.redraw();
  }

  update_catalog(directory, first) {
    const old = new Set(this.repositories.map((repo) => repo.name));
    this.repositories = directory.repositories.filter((repo) => repo.project_root === this.project_root);
    const names = new Set(this.repositories.map((repo) => repo.name));
    for (const name of [...this.selected]) if (!names.has(name)) this.selected.delete(name);
    // 第一次探查全勾上；之后只勾新纳入的，留着人取消掉的。
    for (const name of names) if (first || !old.has(name)) this.selected.add(name);
    this.worksets = directory.worksets.filter((workset) => workset.project_root === this.project_root);
  }

  /** Esc、「关闭」。写入跑着时不理。 */
  dismiss() {
    if (this.busy) return;
    this.close();
  }

  close() {
    if (!this.open) return;
    this.open = false;
    this.list.close_repositories(this);
  }
}

/** 任务名是工作树目录名：不能空、不能是 `.` `..`，不含路径分隔符和控制字符。 */
export function valid_task(task) {
  return task !== "" && task !== "." && task !== ".." && !/[/\\]/.test(task) && !/[\u0000-\u001f\u007f-\u009f]/.test(task);
}

/**
 * 从 Git URL 或目录推一个仓库名：最后一段，去掉 `.git`。URL 只看路径部分，免得把带在主机名里的
 * 凭据当成名字。
 */
export function suggested_name(source) {
  let rest = source.trim();
  const scheme = rest.indexOf("://");
  if (scheme >= 0) {
    const after = rest.slice(scheme + 3);
    const slash = after.indexOf("/");
    if (slash < 0) return "";
    rest = after.slice(slash + 1).split(/[?#]/)[0];
  }
  rest = rest.replace(/[/\\]+$/, "");
  let last = rest.split(/[/\\:]/).pop() ?? "";
  while (last.endsWith(".git")) last = last.slice(0, -4);
  return last;
}

