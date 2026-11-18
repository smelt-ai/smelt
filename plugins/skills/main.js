// 「技能」面板：用 gpui-shell 原生绘制的技能管理，设计见同目录 DESIGN.md。
//
// 磁盘操作在脚本里完成（官方 `fs/promises` 加 smelt 的通用模块 `smelt-fs`）；
// 界面是 GPUI 元素。磁盘操作都是异步的，在事件里 `cx.spawn` 等它们，结果缓存在
// 视图上，render 只画缓存。
//
// 只有一层列表，不切页：点一行就地展开，作用范围表单在展开区里再展开一层；「导入」
// 展开在列表顶部。问题就标在出问题的那一行上，不另设汇总横幅。表单见 `editors.js`，
// 小部件见 `ui.js`。只有删除、清理这类确认在窗口中间问一句（`SmeltConfirmButton`）。
//
// 当前项目读 `active_context().project`，技能读写的是这台电脑的磁盘，只认本机项目
// （`machine` 为 null）。活动上下文变了宿主发 `active_context_changed`（切会话、翻历史
// 也会发），这里 await `next_event()` 等它，本机项目真的换了才重扫，不轮询。

import { View, div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { SmeltAlert, SmeltInputState, SmeltMenuButton, SmeltRadio, SmeltTextInput } from "smelt-ui";
import { homedir } from "os";
import { realpath } from "smelt-fs";
import { active_context, next_event } from "smelt-plugin-api";
import {
  scan,
  repairBridges,
  trashSkill,
  pruneLinks,
  message,
  applyScope,
  resolveConflict,
  migrateLegacy,
  importSkill,
} from "./core.js";
import { ScopeSession, ImportSession, scopeForm, planView, importSource, importWarnings } from "./editors.js";
import { SPACE, MONO, TRASH, button, tag, hint, box, section, confirm, edited, pretty } from "./ui.js";
import { t, tn, list } from "./i18n.js";

/// 当前项目在本机时的根目录；远程项目或没有项目是 null。
function localProjectRoot() {
  const project = active_context().project;
  return project !== null && project.machine === null ? project.root : null;
}

export default class Skills extends View {
  init(_props, cx) {
    this.state = { skills: [], broken: [], installed: [] };
    // 取真实路径：扫描结果里的目录都是解开软链后的，比较时两边要一致。
    this.home = realpath(homedir()) ?? homedir();
    this.projectRoot = null;
    this.notice = null;
    // 展开的是哪一行（skill 目录）。一次只展开一行。
    this.expanded = null;
    // 展开的那一行的作用范围表单开着没有。
    this.editing = false;
    // 导入面板开着没有。
    this.importing = false;
    // 同名冲突里各自选了留哪一份：主条目目录 → 选中的副本目录。
    this.keep = new Map();
    this.query = "";
    this.search = SmeltInputState.new(t("search.placeholder"));
    this.search.on("change", (_event, cx) => {
      this.query = this.search.value();
      cx.notify();
    });

    // 两个表单的状态。
    const env = {
      home: () => this.home,
      projectRoot: () => this.projectRoot,
      installed: () => this.state.installed,
    };
    this.scope = new ScopeSession(env);
    this.import = new ImportSession(env);

    cx.spawn(async (cx) => {
      await this.load();
      cx.notify();
      // 事件从插件挂载完就在记，扫描期间换了项目也不会漏。视图卸载时任务跟着取消。
      for (;;) {
        const event = await next_event();
        // 文案都在 render 里按当前语言取，重画就换过来。搜索框的占位字建状态时就定了，
        // 要等插件重新加载才换。
        if (event.type === "locale_changed") {
          cx.notify();
          continue;
        }
        if (event.type !== "active_context_changed") continue;
        if (localProjectRoot() === this.projectRoot) continue;
        try {
          await this.reload(cx);
        } catch (error) {
          this.notice = { level: "error", text: t("notice.refresh_failed", { error: message(error) }) };
        }
        cx.notify();
      }
    });
  }

  // ---------------------------------------------------------------- 数据

  /// 重扫磁盘。只在 init、事件里调用，不在 render 里。
  async load() {
    this.projectRoot = localProjectRoot();
    try {
      this.state = await scan(this.home, this.projectRoot);
    } catch (error) {
      this.notice = { level: "error", text: t("notice.scan_failed", { error: message(error) }) };
    }
  }

  /// 重扫，然后让展开的那一行和它的作用范围表单对上磁盘。
  async reload(cx) {
    await this.load();
    if (this.expanded !== null && this.skillAt(this.expanded) === undefined) {
      this.expanded = null;
      this.editing = false;
    }
    if (this.editing) await this.prepareScope();
    else if (this.importing) await this.import.refresh();
  }

  skillAt(dir) {
    return this.state.skills.find((skill) => skill.dir === dir);
  }

  /// 作用范围表单以展开的那一行现在的作用范围作初值，并算好预览。
  async prepareScope() {
    const skill = this.expanded === null ? undefined : this.skillAt(this.expanded);
    if (skill === undefined) return;
    this.scope.begin(skill);
    await this.scope.refresh();
  }

  /// 缺兼容链接的 skill。有同名冲突的先选保留哪份，不在这里补。
  missingSkills() {
    return this.state.skills.filter((skill) => skill.missing_bridges.length > 0 && skill.conflicts.length === 0);
  }

  pretty(path) {
    return pretty(this.home, path);
  }

  // ---------------------------------------------------------------- 写操作

  /// 执行一次写操作，然后重扫：磁盘才是唯一真相，界面不留业务副本。
  /// `body` 可以返回 `{ text, level }` 来替换默认的「完成」。返回 `body` 的结果，失败时
  /// 是 undefined。
  async perform(cx, label, body) {
    let result;
    try {
      result = await body();
      const text = result && result.text ? result.text : t("notice.done", { action: label });
      const level = result && result.level ? result.level : "info";
      this.notice = { level, text };
    } catch (error) {
      result = undefined;
      this.notice = { level: "error", text: t("notice.failed", { action: label, error: message(error) }) };
    }
    await this.reload(cx);
    return result;
  }

  /// 从点击里发起 `perform`，做完再重画。
  run(cx, label, body) {
    cx.spawn(async (cx) => {
      await this.perform(cx, label, body);
      cx.notify();
    });
  }

  /// 写操作把 skill 挪到了新目录：展开跟过去。
  follow(destination) {
    if (typeof destination === "string" && this.skillAt(destination) !== undefined) {
      this.expanded = destination;
    }
  }

  /// 表单变了：预览要重算（读盘，放后台）。
  formChanged(session, cx) {
    cx.spawn(async (cx) => {
      await session.refresh();
      cx.notify();
    });
    cx.notify();
  }

  /// 打开或收起展开那一行的作用范围表单。打开时以磁盘上的状态作初值。
  toggleScope(cx) {
    this.editing = !this.editing;
    if (this.editing) {
      this.importing = false;
      cx.spawn(async (cx) => {
        await this.prepareScope();
        cx.notify();
      });
    }
    cx.notify();
  }

  /// 打开或收起导入面板。每次打开都从头来。
  toggleImport(cx) {
    this.importing = !this.importing;
    if (this.importing) {
      this.editing = false;
      this.import.reset();
    }
    cx.notify();
  }

  /// 调整作用范围，然后展开 skill 的新位置。
  applyScope(cx) {
    const skill = this.scope.skill;
    if (skill === null || !this.scope.ready()) return;
    const request = this.scope.request();
    cx.spawn(async (cx) => {
      const destination = await this.perform(cx, t("action.rescope", { name: skill.name }), () =>
        applyScope(this.home, this.projectRoot, skill.dir, request),
      );
      if (destination !== undefined) this.editing = false;
      this.follow(destination);
      cx.notify();
    });
  }

  pickImport(cx) {
    if (this.import.picking) return;
    this.import.picking = true;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        await this.import.pick();
        await this.import.refresh();
      } catch (error) {
        this.notice = { level: "error", text: t("notice.pick_failed", { error: message(error) }) };
      } finally {
        this.import.picking = false;
        cx.notify();
      }
    });
  }

  /// 导入，然后展开装好的那个 skill。
  submitImport(cx) {
    if (!this.import.ready()) return;
    const source = this.import.source;
    const name = this.import.sourceInfo ? this.import.sourceInfo.name : "";
    const request = this.import.request();
    cx.spawn(async (cx) => {
      const destination = await this.perform(cx, t("action.import", { name }), () =>
        importSkill(this.home, this.projectRoot, request, source),
      );
      if (destination !== undefined) this.importing = false;
      this.follow(destination);
      cx.notify();
    });
  }

  repairAll(missing, cx) {
    this.run(cx, t("action.repair_all"), async () => {
      for (const skill of missing) await repairBridges(this.home, skill);
    });
  }

  openPath(path, cx) {
    try {
      cx.open_path(path);
    } catch (error) {
      this.notice = { level: "error", text: message(error) };
      cx.notify();
    }
  }

  /// 点一行：展开或收起。换一行时，上一行没提交的作用范围表单一起丢掉。
  toggleRow(dir, cx) {
    this.expanded = this.expanded === dir ? null : dir;
    this.editing = false;
    cx.notify();
  }

  // ---------------------------------------------------------------- 界面

  render(cx) {
    const colors = cx.theme().colors;
    const query = this.query.trim().toLowerCase();
    const shown = this.state.skills.filter(
      (skill) =>
        query.length === 0 ||
        skill.name.toLowerCase().includes(query) ||
        skill.description.toLowerCase().includes(query),
    );
    const groups = [
      { project: true, skills: shown.filter((skill) => skill.project_scope) },
      { project: false, skills: shown.filter((skill) => !skill.project_scope) },
    ].filter((group) => group.skills.length > 0);

    return v_flex()
      .size_full()
      .text_color(colors.foreground)
      .child(this.header(cx))
      .when(this.notice !== null, (el) => el.child(this.noticeBar(cx)))
      .child(
        v_flex()
          .flex_1()
          .min_h_0()
          .overflow_y_scrollbar()
          .pb(SPACE.lg)
          .when(this.importing, (el) => el.child(this.importPanel(cx)))
          .when(this.state.skills.length === 0, (el) =>
            el.child(
              this.empty(
                t("empty.none_title"),
                t("empty.none_body"),
                cx,
              ),
            ),
          )
          .when(this.state.skills.length > 0 && shown.length === 0, (el) =>
            el.child(this.empty(t("empty.no_match_title"), t("empty.no_match_body", { query: this.query.trim() }), cx)),
          )
          .children(groups.flatMap((group) => [this.groupLabel(group, cx), ...group.skills.map((skill) => this.row(skill, cx))]))
          // 失效链接不属于任何 skill，搜索时不列。
          .when(query.length === 0 && this.state.broken.length > 0, (el) => el.child(this.brokenGroup(cx))),
      );
  }

  /// 顶上一条：标题、数量、搜索、导入、更多。
  header(cx) {
    const colors = cx.theme().colors;
    const missing = this.missingSkills();
    let more = SmeltMenuButton.new("more")
      .label("⋯")
      .accessibility_label(t("menu.more"))
      .small()
      .quiet()
      .item(t("menu.refresh"), (cx) => {
        this.notice = null;
        cx.spawn(async (cx) => {
          await this.reload(cx);
          cx.notify();
        });
      })
      .item(t("menu.open_trash"), (cx) => this.openPath(`${this.home}/${TRASH}`, cx));
    if (missing.length > 0) {
      more = more.item(t("action.repair_all"), (cx) => this.repairAll(missing, cx));
    }
    return h_flex()
      .items_center()
      .gap(SPACE.sm)
      .px(SPACE.lg)
      .py(SPACE.sm)
      .border_b(1)
      .border_color(colors.border)
      .child(div().text_sm().font_semibold().child(t("title")))
      .child(div().text_xs().text_color(colors.muted_foreground).child(String(this.state.skills.length)))
      .child(div().flex_1())
      .child(SmeltTextInput.new(this.search).aria_label(t("search.label")).small().cleanable(true).w(180))
      .child(
        button(
          "import",
          this.importing ? t("import.hide") : t("import.button"),
          (cx) => this.toggleImport(cx),
          this.importing ? "secondary" : "primary",
        ),
      )
      .child(more);
  }

  /// 上一次操作的结果：出错用提示块，做完了只要一行弱色的字。
  noticeBar(cx) {
    const colors = cx.theme().colors;
    const text = this.notice.text;
    return h_flex()
      .items_center()
      .gap(SPACE.sm)
      .px(SPACE.lg)
      .pt(SPACE.sm)
      .child(
        this.notice.level === "error"
          ? SmeltAlert.new("notice", text).flex_1().min_w_0()
          : div().flex_1().min_w_0().text_xs().whitespace_normal().text_color(colors.muted_foreground).child(text),
      )
      .child(
        button("notice-close", t("notice.close"), (cx) => {
          this.notice = null;
          cx.notify();
        }),
      );
  }

  empty(title, description, cx) {
    return v_flex()
      .gap(SPACE.xs)
      .p(SPACE.lg)
      .child(div().text_sm().font_semibold().child(title))
      .child(hint(description, cx));
  }

  groupLabel(group, cx) {
    const colors = cx.theme().colors;
    return h_flex()
      .gap(SPACE.xs)
      .px(SPACE.lg)
      .pt(SPACE.md)
      .pb(SPACE.xs)
      .text_xs()
      .text_color(colors.muted_foreground)
      .child(group.project ? t("scope.project") : t("scope.user"))
      .child(String(group.skills.length))
      .when(group.project && this.projectRoot !== null, (el) => el.child(`· ${this.pretty(this.projectRoot)}`));
  }

  // ------------------------------------------------ 行

  /// 一行，点开就地展开详情。收起时不建展开区：每次重绘都给每一行建一份，行多了
  /// 白白耗时。
  row(skill, cx) {
    const open = this.expanded === skill.dir;
    return v_flex()
      .child(this.rowHeader(skill, open, cx))
      .when(open, (el) => el.child(this.rowDetail(skill, cx)));
  }

  /// 名字、标签、一行描述；右侧固定是覆盖情况。悬停只改底色，不换内容。
  rowHeader(skill, open, cx) {
    const colors = cx.theme().colors;
    return h_flex()
      .id(`row-${skill.dir}`)
      .items_center()
      .gap(SPACE.md)
      .px(SPACE.lg)
      .py(SPACE.sm)
      .cursor_pointer()
      .when(open, (el) => el.bg(colors.accent))
      .hover((el) => el.bg(colors.accent))
      .on_click((_event, cx) => this.toggleRow(skill.dir, cx))
      .child(
        v_flex()
          .flex_1()
          .min_w_0()
          .gap(2)
          .child(
            h_flex()
              .items_center()
              .gap(SPACE.sm)
              .child(div().min_w_0().truncate().text_sm().font_semibold().child(skill.name))
              .child(this.chip(skill)),
          )
          // 展开后下面有完整描述，这里不再重复一行截断的。
          .when(!open, (el) =>
            el.child(
              div()
                .text_xs()
                .truncate()
                .text_color(colors.muted_foreground)
                .child(skill.description || t("no_description")),
            ),
          ),
      )
      .child(this.summary(skill, cx));
  }

  /// 列表右侧的一句话：有几个 agent 能用，或者哪里不对。
  summary(skill, cx) {
    const colors = cx.theme().colors;
    const missing = skill.coverage.filter((row) => row.state === "missing");
    if (skill.conflicts.length > 0) {
      return div().flex_none().text_xs().text_color(colors.destructive).child(t("status.choose"));
    }
    if (skill.placement === "legacy") return tag(t("status.unread"), "need");
    if (missing.length === 1) return tag(t("status.missing_one", { agent: missing[0].label }), "need");
    if (missing.length > 1) return tag(t("status.missing_many", { count: missing.length }), "need");
    return div()
      .flex_none()
      .text_xs()
      .text_color(colors.muted_foreground)
      .child(tn("status.agents", skill.coverage.length));
  }

  /// 展开区，从上到下：完整描述 → 可用 agent（缺链接就在这一行补）→ 作用范围（就在
  /// 这一行下面改）→ 路径和删除。看到什么就在什么旁边改。
  rowDetail(skill, cx) {
    const colors = cx.theme().colors;
    const conflicted = skill.conflicts.length > 0;
    return v_flex()
      .gap(SPACE.md)
      .px(SPACE.lg)
      .pt(SPACE.xs)
      .pb(SPACE.md)
      .border_b(1)
      .border_color(colors.border)
      .when(skill.description.length > 0, (el) =>
        el.child(div().text_xs().whitespace_normal().child(skill.description)),
      )
      .when(conflicted, (el) => el.child(this.conflictSection(skill, cx)))
      .child(this.coverage(skill, cx))
      .when(!conflicted, (el) => el.child(this.scopeLine(skill, cx)))
      .child(this.footerLine(skill, cx));
  }

  /// 小标题一行：左边标题和现状，右边一个操作。
  labelLine(title, value, action, cx) {
    const colors = cx.theme().colors;
    return h_flex()
      .items_center()
      .gap(SPACE.sm)
      .child(div().flex_none().text_xs().font_semibold().text_color(colors.muted_foreground).child(title))
      .when(value !== null, (el) => el.child(div().min_w_0().truncate().text_xs().child(value)))
      .child(div().flex_1())
      .when(action !== null, (el) => el.child(action));
  }

  /// 可用 agent：逐个列出读 `.agents` / 经链接 / 本体 / 缺链接；缺链接就在标题右边补。
  coverage(skill, cx) {
    if (skill.placement === "legacy") {
      return v_flex()
        .gap(SPACE.xs)
        .child(
          this.labelLine(
            t("coverage.title"),
            null,
            button(
              `migrate-${skill.dir}`,
              t("migrate.button"),
              (cx) => this.run(cx, t("migrate.action"), () => migrateLegacy(this.home, this.projectRoot)),
              "primary",
            ),
            cx,
          ),
        )
        .child(hint(t("coverage.legacy"), cx));
    }
    const missing = skill.coverage.filter((row) => row.state === "missing");
    const names = list(missing.map((row) => row.label));
    const repair =
      missing.length > 0
        ? button(
            `repair-${skill.dir}`,
            missing.length === 1 ? t("coverage.repair_one", { agent: names }) : t("coverage.repair_many"),
            (cx) => this.run(cx, t("action.repair", { name: skill.name, agents: names }), () => repairBridges(this.home, skill)),
            "primary",
          )
        : null;
    // 能用的只写名字：读 .agents、经链接、本体对用户都是「能用」。
    return v_flex()
      .gap(SPACE.xs)
      .child(this.labelLine(t("coverage.title"), null, repair, cx))
      .child(
        skill.coverage.length === 0
          ? hint(t("coverage.none"), cx)
          : h_flex()
              .flex_wrap()
              .gap(SPACE.xs)
              .children(
                skill.coverage.map((row) =>
                  row.state === "missing" ? tag(t("coverage.row_missing", { agent: row.label }), "need") : tag(row.label, "ok"),
                ),
              ),
      );
  }

  /// 作用范围：现状一句话，右边「修改」，表单就在这一行下面展开。
  scopeLine(skill, cx) {
    const who =
      skill.placement === "agent"
        ? t("scope.only", { agent: skill.agent ?? t("scope.some_agent") })
        : t("scope.universal_all");
    const level = skill.project_scope ? t("scope.project") : t("scope.user");
    return v_flex()
      .gap(SPACE.sm)
      .child(
        this.labelLine(
          t("scope.title"),
          `${who} · ${level}`,
          button(`scope-${skill.dir}`, this.editing ? t("scope.hide") : t("scope.change"), (cx) => this.toggleScope(cx), "secondary"),
          cx,
        ),
      )
      .when(this.editing, (el) => el.child(this.scopeEditor(cx)));
  }

  /// 最后一行：路径和修改时间（点路径在访达里打开），右边删除。
  footerLine(skill, cx) {
    const colors = cx.theme().colors;
    return h_flex()
      .items_center()
      .gap(SPACE.sm)
      .pt(SPACE.sm)
      .border_t(1)
      .border_color(colors.border)
      .child(
        div()
          .id(`path-${skill.dir}`)
          .flex_1()
          .min_w_0()
          .truncate()
          .text_xs()
          .font_family(MONO)
          .text_color(colors.muted_foreground)
          .cursor_pointer()
          .hover((el) => el.text_color(colors.foreground).underline())
          .on_click((_event, cx) => this.openPath(skill.dir, cx))
          .child(`${this.pretty(skill.dir)} · ${edited(skill.modified_ms)}`),
      )
      .child(
        confirm(
          `trash-${skill.dir}`,
          t("delete.button"),
          t("delete.title", { name: skill.name }),
          t("delete.body", { trash: TRASH }),
          t("delete.confirm"),
          (cx) => this.run(cx, t("delete.action", { name: skill.name }), () => trashSkill(this.home, skill)),
        ),
      );
  }

  /// 作用范围表单：将执行的文件操作、取消和应用。
  scopeEditor(cx) {
    return box(
      [
        ...scopeForm(this.scope, cx, (cx) => this.formChanged(this.scope, cx)),
        planView(this.scope, t("scope.unchanged"), cx),
        h_flex()
          .justify_end()
          .gap(SPACE.sm)
          .child(button("cancel-scope", t("cancel"), (cx) => this.toggleScope(cx), "secondary"))
          .child(button("apply-scope", t("scope.apply"), (cx) => this.applyScope(cx), "primary").disabled(!this.scope.ready())),
      ],
      cx,
    );
  }

  /// 列表顶部的导入面板：来源 → 装到哪里 → 将执行。
  importPanel(cx) {
    const session = this.import;
    return v_flex()
      .gap(SPACE.md)
      .m(SPACE.lg)
      .mb(0)
      .child(div().text_sm().font_semibold().child(t("import.title")))
      .child(
        box(
          [
            section(t("import.source"), null, [importSource(session, cx, (cx) => this.pickImport(cx))], cx),
            section(t("import.destination"), null, scopeForm(session, cx, (cx) => this.formChanged(session, cx)), cx),
            section(
              t("plan.title"),
              null,
              [
                session.source === null ? hint(t("import.plan_pending"), cx) : planView(session, "", cx),
                ...importWarnings(session),
              ],
              cx,
            ),
            h_flex()
              .justify_end()
              .gap(SPACE.sm)
              .child(button("cancel-import", t("cancel"), (cx) => this.toggleImport(cx), "secondary"))
              .child(button("submit-import", t("import.button"), (cx) => this.submitImport(cx), "primary").disabled(!session.ready())),
          ],
          cx,
        ),
      );
  }

  conflictSection(skill, cx) {
    const copies = [
      {
        dir: skill.dir,
        placement: skill.placement,
        agent: skill.agent,
        description: skill.description,
        modified_ms: skill.modified_ms,
      },
      ...skill.conflicts,
    ];
    const keep = this.keep.get(skill.dir) ?? skill.dir;
    return section(
      t("conflict.title", { name: skill.name }),
      button(
        `keep-${skill.dir}`,
        t("conflict.keep"),
        (cx) =>
          this.run(cx, t("conflict.action", { name: skill.name }), () =>
            resolveConflict(this.home, this.projectRoot, skill.dir, keep),
          ),
        "primary",
      ),
      [
        hint(t("conflict.hint", { count: copies.length, trash: TRASH }), cx),
        box(copies.map((copy) => this.copyChoice(skill, copy, keep, cx)), cx),
      ],
      cx,
    );
  }

  /// 一份副本：单选「以它为准」，下面是它在哪。每份一个受控的 `SmeltRadio`，选中哪份由
  /// `this.keep` 定。
  copyChoice(skill, copy, keep, cx) {
    const colors = cx.theme().colors;
    const where =
      copy.placement === "universal"
        ? t("place.universal")
        : copy.placement === "legacy"
          ? t("place.legacy")
          : copy.agent || "agent";
    return v_flex()
      .gap(2)
      .child(
        SmeltRadio.new(`keep-${copy.dir}`)
          .label(`${where} · ${edited(copy.modified_ms)}`)
          .checked(keep === copy.dir)
          .on_select((cx) => {
            this.keep.set(skill.dir, copy.dir);
            cx.notify();
          }),
      )
      .child(
        div()
          .pl(24)
          .text_xs()
          .font_family(MONO)
          .whitespace_normal()
          .text_color(colors.muted_foreground)
          .child(this.pretty(copy.dir)),
      );
  }

  // ------------------------------------------------ 失效链接

  /// 列表最后一组：失效链接。指向的目录已经不存在，只删链接本身。
  brokenGroup(cx) {
    const colors = cx.theme().colors;
    const broken = this.state.broken;
    const shown = broken.slice(0, 5).map((link) => this.pretty(link.path));
    if (broken.length > shown.length) shown.push(t("broken.more", { count: broken.length }));
    return v_flex()
      .child(
        h_flex()
          .items_center()
          .gap(SPACE.xs)
          .px(SPACE.lg)
          .pt(SPACE.md)
          .pb(SPACE.xs)
          .text_xs()
          .text_color(colors.muted_foreground)
          .child(t("broken.title"))
          .child(String(broken.length))
          .child(div().flex_1())
          .child(
            confirm(
              "prune",
              t("broken.button"),
              tn("broken.confirm_title", broken.length),
              t("broken.confirm_body", { paths: shown.join("\n") }),
              t("broken.confirm"),
              (cx) => this.run(cx, t("broken.action"), () => pruneLinks(broken)),
            ),
          ),
      )
      .children(
        broken.map((link) =>
          div()
            .px(SPACE.lg)
            .py(2)
            .text_xs()
            .font_family(MONO)
            .truncate()
            .text_color(colors.muted_foreground)
            .child(this.pretty(link.path)),
        ),
      );
  }

  // ---------------------------------------------------------------- 小部件

  /// 作用范围标签：通用 / 指定 agent / 旧位置 / 冲突。
  chip(skill) {
    if (skill.conflicts.length > 0) return tag(t("place.conflict", { count: skill.conflicts.length + 1 }), "bad");
    if (skill.placement === "agent") return tag(skill.agent ?? "agent");
    if (skill.placement === "legacy") return tag(t("place.legacy"), "need");
    return tag(t("place.universal"));
  }
}
