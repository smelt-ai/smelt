// 「技能」面板：用 gpui-shell 原生绘制的技能管理，设计见同目录 DESIGN.md。
//
// 磁盘操作在脚本里完成（官方 `fs/promises` 加 smelt 的通用模块 `smelt-fs`）；
// 界面是 GPUI 元素。磁盘操作都是异步的，在事件里 `cx.spawn` 等它们，结果缓存在
// 视图上，render 只画缓存。
//
// 只有一层列表，不切页：点一行就地展开（`Collapsible`），作用范围表单在展开区里再
// 展开一层；「导入」展开在列表顶部。问题就标在出问题的那一行上，不另设汇总横幅。表单见 `editors.js`，小部件见
// `ui.js`。只有删除、清理这类确认用居中的 `AlertDialog`。
//
// 当前项目由宿主写进 `smelt-workspace.context()`。脚本没法被宿主直接通知，
// 所以每秒看一眼 `generation`，变了才重扫——轮询读的是内存，不碰磁盘。

import { View, div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import {
  Collapsible,
  DropdownMenu,
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
  ErrorAlert,
  InfoAlert,
  Input,
  InputState,
  Radio,
} from "gpui-component";
import { context } from "smelt-workspace";
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

export default class Skills extends View {
  init(_props, cx) {
    this.state = { skills: [], broken: [], installed: [] };
    this.home = "";
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
    this.search = InputState.new("搜索名称或描述");
    this.generation = -1;

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
    });
    this.watch = cx.timer.every(1000, (cx) => {
      if (context().generation === this.generation) return;
      cx.spawn(async (cx) => {
        await this.reload(cx);
        cx.notify();
      });
    });
  }

  // ---------------------------------------------------------------- 数据

  /// 重扫磁盘。只在 init、定时器、事件里调用，不在 render 里。
  /// `generation` 在第一个 await 之前就记下，扫描期间定时器不会再触发一次。
  async load() {
    const ctx = context();
    this.generation = ctx.generation;
    this.home = ctx.home;
    this.projectRoot = ctx.project_root;
    try {
      this.state = await scan(ctx.home, ctx.project_root);
    } catch (error) {
      this.notice = { level: "error", text: `扫描失败：${message(error)}` };
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
      const text = result && result.text ? result.text : `${label}完成`;
      const level = result && result.level ? result.level : "info";
      this.notice = { level, text };
    } catch (error) {
      result = undefined;
      this.notice = { level: "error", text: `${label}失败：${message(error)}` };
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
      const destination = await this.perform(cx, `调整 ${skill.name} 的作用范围`, () =>
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
        this.notice = { level: "error", text: `选择目录失败：${message(error)}` };
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
      const destination = await this.perform(cx, `导入 ${name}`, () =>
        importSkill(this.home, this.projectRoot, request, source),
      );
      if (destination !== undefined) this.importing = false;
      this.follow(destination);
      cx.notify();
    });
  }

  repairAll(missing, cx) {
    this.run(cx, "让所有技能在全部 agent 可用", async () => {
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

  /// 确认框自己出错（比如内容画不出来）时的消息。
  reportDialogError(error, cx) {
    this.notice = { level: "error", text: error };
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
      .when(this.notice !== null, (el) => el.child(this.noticeBar()))
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
                "还没有 skill",
                "点「导入」选一个含 SKILL.md 的目录，或放到 ~/.agents/skills/<名字>/。",
              ),
            ),
          )
          .when(this.state.skills.length > 0 && shown.length === 0, (el) =>
            el.child(this.empty("没有匹配的技能", `名称和描述里都没有「${this.query.trim()}」。`)),
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
    let more = DropdownMenu.new("more", "⋯")
      .item("刷新", (cx) => {
        this.notice = null;
        cx.spawn(async (cx) => {
          await this.reload(cx);
          cx.notify();
        });
      })
      .item("打开回收目录", (cx) => this.openPath(`${this.home}/${TRASH}`, cx));
    if (missing.length > 0) {
      more = more.item("让所有技能在全部 agent 可用", (cx) => this.repairAll(missing, cx));
    }
    return h_flex()
      .items_center()
      .gap(SPACE.sm)
      .px(SPACE.lg)
      .py(SPACE.sm)
      .border_b(1)
      .border_color(colors.border)
      .child(div().text_sm().font_semibold().child("技能"))
      .child(div().text_xs().text_color(colors.muted_foreground).child(String(this.state.skills.length)))
      .child(div().flex_1())
      .child(
        div()
          .w(180)
          .child(
            Input.new(this.search)
              .aria_label("搜索技能")
              .on_change((text, cx) => {
                this.query = text;
                cx.notify();
              }),
          ),
      )
      .child(
        button(
          "import",
          this.importing ? "收起导入" : "导入",
          (cx) => this.toggleImport(cx),
          this.importing ? "outline" : "primary",
        ),
      )
      .child(more);
  }

  noticeBar() {
    const Kind = this.notice.level === "error" ? ErrorAlert : InfoAlert;
    return h_flex()
      .items_center()
      .gap(SPACE.sm)
      .px(SPACE.lg)
      .pt(SPACE.sm)
      .child(div().flex_1().min_w_0().child(Kind.new("notice", this.notice.text).size("small")))
      .child(
        button("notice-close", "关闭", (cx) => {
          this.notice = null;
          cx.notify();
        }),
      );
  }

  empty(title, description) {
    return div()
      .p(SPACE.lg)
      .child(
        Empty.new().header(
          EmptyHeader.new()
            .title(EmptyTitle.new().child(title))
            .description(EmptyDescription.new().child(description)),
        ),
      );
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
      .child(group.project ? "项目级" : "用户级")
      .child(String(group.skills.length))
      .when(group.project && this.projectRoot !== null, (el) => el.child(`· ${this.pretty(this.projectRoot)}`));
  }

  // ------------------------------------------------ 行

  /// 一行，点开就地展开详情。收起时不建展开区：每次重绘都给每一行建一份，行多了
  /// 白白耗时。
  row(skill, cx) {
    const open = this.expanded === skill.dir;
    return Collapsible.new()
      .open(open)
      .child(this.rowHeader(skill, open, cx))
      .when(open, (el) => el.content(this.rowDetail(skill, cx)));
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
                .child(skill.description || "（没有描述）"),
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
      return div().flex_none().text_xs().text_color(colors.destructive).child("需选择");
    }
    if (skill.placement === "legacy") return tag("没有 agent 读到", "warning");
    if (missing.length === 1) return tag(`${missing[0].label} 还不能用`, "warning");
    if (missing.length > 1) return tag(`${missing.length} 个 agent 还不能用`, "warning");
    return div()
      .flex_none()
      .text_xs()
      .text_color(colors.muted_foreground)
      .child(`${skill.coverage.length} 个 agent`);
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
            "可用 agent",
            null,
            button(
              `migrate-${skill.dir}`,
              "迁移到 .agents/skills",
              (cx) => this.run(cx, "迁移旧目录", () => migrateLegacy(this.home, this.projectRoot)),
              "primary",
            ),
            cx,
          ),
        )
        .child(hint("放在 smelt 私有目录，没有 agent 读得到。", cx));
    }
    const missing = skill.coverage.filter((row) => row.state === "missing");
    const names = missing.map((row) => row.label).join("、");
    const repair =
      missing.length > 0
        ? button(
            `repair-${skill.dir}`,
            missing.length === 1 ? `让 ${names} 也能用` : "让它们也能用",
            (cx) => this.run(cx, `让 ${skill.name} 在 ${names} 可用`, () => repairBridges(this.home, skill)),
            "primary",
          )
        : null;
    // 能用的只写名字：读 .agents、经链接、本体对用户都是「能用」。
    return v_flex()
      .gap(SPACE.xs)
      .child(this.labelLine("可用 agent", null, repair, cx))
      .child(
        skill.coverage.length === 0
          ? hint("这台机器上在用的 agent 都读不到它。", cx)
          : h_flex()
              .flex_wrap()
              .gap(SPACE.xs)
              .children(
                skill.coverage.map((row) =>
                  row.state === "missing" ? tag(`${row.label} · 还不能用`, "warning") : tag(row.label, "success"),
                ),
              ),
      );
  }

  /// 作用范围：现状一句话，右边「修改」，表单就在这一行下面展开。
  scopeLine(skill, cx) {
    const who = skill.placement === "agent" ? `仅 ${skill.agent ?? "指定 agent"}` : "所有 agent（通用）";
    const level = skill.project_scope ? "项目级" : "用户级";
    return v_flex()
      .gap(SPACE.sm)
      .child(
        this.labelLine(
          "作用范围",
          `${who} · ${level}`,
          button(`scope-${skill.dir}`, this.editing ? "收起" : "修改", (cx) => this.toggleScope(cx), "outline"),
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
          "删除…",
          `删除「${skill.name}」？`,
          `技能目录会移到 ~/${TRASH}，可以手动找回。所有 agent 都不会再用到它。`,
          (cx) => this.run(cx, `删除 ${skill.name}`, () => trashSkill(this.home, skill)),
          (error, cx) => this.reportDialogError(error, cx),
        ),
      );
  }

  /// 作用范围表单：将执行的文件操作、取消和应用。
  scopeEditor(cx) {
    return box(
      [
        ...scopeForm(this.scope, cx, (cx) => this.formChanged(this.scope, cx)),
        planView(this.scope, "已经是这个状态，无需改动。", cx),
        h_flex()
          .justify_end()
          .gap(SPACE.sm)
          .child(button("cancel-scope", "取消", (cx) => this.toggleScope(cx), "outline"))
          .child(button("apply-scope", "应用", (cx) => this.applyScope(cx), "primary").disabled(!this.scope.ready())),
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
      .child(div().text_sm().font_semibold().child("导入技能"))
      .child(
        box(
          [
            section("来源", null, [importSource(session, cx, (cx) => this.pickImport(cx))], cx),
            section("装到哪里", null, scopeForm(session, cx, (cx) => this.formChanged(session, cx)), cx),
            section(
              "将执行",
              null,
              [
                session.source === null ? hint("选好来源后显示。", cx) : planView(session, "", cx),
                ...importWarnings(session),
              ],
              cx,
            ),
            h_flex()
              .justify_end()
              .gap(SPACE.sm)
              .child(button("cancel-import", "取消", (cx) => this.toggleImport(cx), "outline"))
              .child(button("submit-import", "导入", (cx) => this.submitImport(cx), "primary").disabled(!session.ready())),
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
      `同名冲突：${skill.name}`,
      button(
        `keep-${skill.dir}`,
        "保留选中的一份",
        (cx) =>
          this.run(cx, `处理 ${skill.name} 的同名副本`, () =>
            resolveConflict(this.home, this.projectRoot, skill.dir, keep),
          ),
        "primary",
      ),
      [
        hint(`有 ${copies.length} 份彼此独立的副本。选一份为准，其余移进 ~/${TRASH}。`, cx),
        box(copies.map((copy) => this.copyChoice(skill, copy, keep, cx)), cx),
      ],
      cx,
    );
  }

  /// 一份副本：单选「以它为准」，下面是它在哪。各份之间夹着路径，放不进只收 `Radio`
  /// 的单选组，所以每份一个独立的受控 `Radio`。
  copyChoice(skill, copy, keep, cx) {
    const colors = cx.theme().colors;
    const where =
      copy.placement === "universal" ? "通用" : copy.placement === "legacy" ? "旧位置" : copy.agent || "agent";
    return v_flex()
      .gap(2)
      .child(
        Radio.new(`keep-${copy.dir}`)
          .label(`${where} · ${edited(copy.modified_ms)}`)
          .checked(keep === copy.dir)
          .on_change((checked, cx) => {
            if (!checked) return;
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
    if (broken.length > shown.length) shown.push(`等共 ${broken.length} 条`);
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
          .child("已失效")
          .child(String(broken.length))
          .child(div().flex_1())
          .child(
            confirm(
              "prune",
              "清理…",
              `清理 ${broken.length} 个已失效的条目？`,
              `它们指向的技能已经不存在。只删这些条目本身，不会动任何真实目录。\n${shown.join("\n")}`,
              (cx) => this.run(cx, "清理已失效的条目", () => pruneLinks(broken)),
              (error, cx) => this.reportDialogError(error, cx),
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
    if (skill.conflicts.length > 0) return tag(`冲突 ×${skill.conflicts.length + 1}`, "danger");
    if (skill.placement === "agent") return tag(skill.agent ?? "agent", "info");
    if (skill.placement === "legacy") return tag("旧位置", "warning");
    return tag("通用", "secondary");
  }
}
