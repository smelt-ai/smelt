// 「新技能」面板：用 gpui-shell 原生绘制的技能管理（可行性原型）。
//
// 和旧的「技能」插件（plugins/skills，bun + WebView）完全独立：没有插件进程，
// 磁盘操作在脚本里同步完成，经 smelt 借出的 `smelt-fs`；界面是 GPUI 元素。
//
// smelt 的窗口根不是 gpui-shell 的 ShellRoot，所以 dialog / toast 用不了：
// 需要确认的操作在行内展开「确认 / 取消」，结果写在顶部的提示条里。
//
// 当前项目由宿主写进 `smelt-workspace.context()`。脚本没法被宿主直接通知，
// 所以每秒看一眼 `generation`，变了才重扫——轮询读的是内存，不碰磁盘。

import { View, div } from "gpui-kit";
import { v_flex, h_flex, Button } from "gpui-base";
import { context, reveal } from "smelt-workspace";
import { scan, repairBridges, trashSkill, pruneLinks, message } from "./core.js";

const SPACE = { xs: 4, sm: 8, md: 12, lg: 16 };

export default class Skills extends View {
  init(_props, cx) {
    this.state = { skills: [], broken: [] };
    this.home = "";
    this.notice = null;
    // 正在等待确认的操作，形如 "trash:<dir>" / "prune"。同一时刻只问一件事。
    this.pending = null;
    this.generation = -1;
    this.load();
    this.watch = cx.timer.every(1000, (cx) => {
      if (context().generation !== this.generation) {
        this.load();
        cx.notify();
      }
    });
  }

  /// 重扫磁盘。只在 init、定时器、事件里调用，不在 render 里。
  load() {
    const ctx = context();
    this.generation = ctx.generation;
    this.home = ctx.home;
    try {
      this.state = scan(ctx.home, ctx.project_root);
    } catch (error) {
      this.notice = { level: "error", text: `扫描失败：${message(error)}` };
    }
  }

  /// 执行一次写操作，然后重扫：磁盘才是唯一真相，界面不留业务副本。
  run(cx, label, body) {
    this.pending = null;
    try {
      body();
      this.notice = { level: "info", text: `${label}完成` };
    } catch (error) {
      this.notice = { level: "error", text: `${label}失败：${message(error)}` };
    }
    this.load();
    cx.notify();
  }

  ask(key, cx) {
    this.pending = key;
    cx.notify();
  }

  pretty(path) {
    return this.home && path.startsWith(`${this.home}/`) ? `~${path.slice(this.home.length)}` : path;
  }

  render(cx) {
    const colors = cx.theme().colors;
    const { skills, broken } = this.state;
    const rows = [];
    let lastScope = null;
    for (const skill of skills) {
      if (skill.project_scope !== lastScope) {
        lastScope = skill.project_scope;
        rows.push(
          div()
            .px(SPACE.lg)
            .pt(SPACE.md)
            .pb(SPACE.xs)
            .text_size(11)
            .text_color(colors.muted_foreground)
            .child(skill.project_scope ? "项目级" : "用户级"),
        );
      }
      rows.push(this.row(skill, cx));
    }

    return v_flex()
      .size_full()
      .bg(colors.background)
      .text_color(colors.foreground)
      .child(this.header(skills.length, cx))
      .when(this.notice !== null, (el) => el.child(this.noticeBar(cx)))
      .when(broken.length > 0, (el) => el.child(this.brokenBar(broken, cx)))
      .child(
        v_flex()
          .flex_1()
          .min_h_0()
          .overflow_y_scrollbar()
          .pb(SPACE.lg)
          .when(skills.length === 0, (el) =>
            el.child(
              div()
                .p(SPACE.lg)
                .text_size(12)
                .text_color(colors.muted_foreground)
                .child("还没有 skill：~/.agents/skills/<名字>/SKILL.md"),
            ),
          )
          .children(rows),
      );
  }

  header(count, cx) {
    const colors = cx.theme().colors;
    return h_flex()
      .items_center()
      .gap(SPACE.sm)
      .px(SPACE.lg)
      .py(SPACE.sm)
      .border_b(1)
      .border_color(colors.border)
      .child(div().text_size(13).font_semibold().child("新技能"))
      .child(div().text_size(12).text_color(colors.muted_foreground).child(count ? String(count) : ""))
      .child(div().flex_1())
      .child(
        this.button("refresh", "刷新", cx, (cx) => {
          this.notice = null;
          this.load();
          cx.notify();
        }),
      );
  }

  noticeBar(cx) {
    const colors = cx.theme().colors;
    const error = this.notice.level === "error";
    return h_flex()
      .items_center()
      .gap(SPACE.sm)
      .px(SPACE.lg)
      .py(SPACE.xs)
      .bg(colors.muted)
      .child(
        div()
          .flex_1()
          .min_w_0()
          .text_size(12)
          .text_color(error ? colors.destructive : colors.foreground)
          .child(this.notice.text),
      )
      .child(
        this.button("notice-close", "关闭", cx, (cx) => {
          this.notice = null;
          cx.notify();
        }),
      );
  }

  brokenBar(broken, cx) {
    const colors = cx.theme().colors;
    const confirming = this.pending === "prune";
    return v_flex()
      .gap(SPACE.xs)
      .px(SPACE.lg)
      .py(SPACE.sm)
      .border_b(1)
      .border_color(colors.border)
      .child(
        h_flex()
          .items_center()
          .gap(SPACE.sm)
          .child(
            div()
              .flex_1()
              .text_size(12)
              .child(`${broken.length} 条兼容链接指向已经不存在的目标`),
          )
          .when(!confirming, (el) => el.child(this.button("prune", "查看并清理", cx, (cx) => this.ask("prune", cx)))),
      )
      .when(confirming, (el) =>
        el
          .children(
            broken.map((link) =>
              div().text_size(11).text_color(colors.muted_foreground).child(this.pretty(link.path)),
            ),
          )
          .child(
            this.confirmRow("prune", "只删除这些链接本身，不会动任何真实目录。", cx, (cx) =>
              this.run(cx, "清理失效链接", () => pruneLinks(broken)),
            ),
          ),
      );
  }

  row(skill, cx) {
    const colors = cx.theme().colors;
    const key = `trash:${skill.dir}`;
    const confirming = this.pending === key;
    return v_flex()
      .gap(SPACE.xs)
      .px(SPACE.lg)
      .py(SPACE.sm)
      .hover((style) => style.bg(colors.muted))
      .child(
        h_flex()
          .items_center()
          .gap(SPACE.sm)
          .child(div().min_w_0().truncate().text_size(13).font_semibold().child(skill.name))
          .child(this.chip(skill, cx))
          .child(div().flex_1()),
      )
      .when(skill.description.length > 0, (el) =>
        el.child(div().text_size(12).text_color(colors.muted_foreground).child(skill.description)),
      )
      .child(div().text_size(11).text_color(colors.muted_foreground).child(this.pretty(skill.dir)))
      .when(skill.missing_bridges.length > 0, (el) =>
        el.child(
          div()
            .text_size(11)
            .text_color(colors.destructive)
            .child(`缺 ${skill.missing_bridges.join("、")} 的兼容链接`),
        ),
      )
      .child(
        h_flex()
          .gap(SPACE.xs)
          .child(
            this.button(`reveal-${skill.dir}`, "访达", cx, (cx) => {
              try {
                reveal(skill.dir);
              } catch (error) {
                this.notice = { level: "error", text: message(error) };
                cx.notify();
              }
            }),
          )
          .when(skill.missing_bridges.length > 0, (el) =>
            el.child(
              this.button(`repair-${skill.dir}`, "修复链接", cx, (cx) =>
                this.run(cx, `修复 ${skill.name} 的兼容链接`, () => repairBridges(this.home, skill)),
              ),
            ),
          )
          .when(!confirming, (el) => el.child(this.button(`trash-${skill.dir}`, "删除", cx, (cx) => this.ask(key, cx)))),
      )
      .when(confirming, (el) =>
        el.child(
          this.confirmRow(key, "移到 ~/.smelt/trash/skills，兼容链接一并清掉。", cx, (cx) =>
            this.run(cx, `删除 ${skill.name}`, () => trashSkill(this.home, skill)),
          ),
        ),
      );
  }

  /// 作用范围 chip：通用 / 指定 agent / 冲突。
  chip(skill, cx) {
    const colors = cx.theme().colors;
    let text = "通用";
    let color = colors.muted_foreground;
    if (skill.conflicts.length > 0) {
      text = `冲突 ×${skill.conflicts.length + 1}`;
      color = colors.destructive;
    } else if (skill.placement === "agent") {
      text = skill.agent ?? "agent";
    } else if (skill.placement === "legacy") {
      text = "旧位置";
    }
    return div()
      .flex_none()
      .px(6)
      .py(1)
      .rounded(4)
      .border(1)
      .border_color(colors.border)
      .text_size(10)
      .text_color(color)
      .child(text);
  }

  /// 行内确认，替代 dialog。`onConfirm(cx)` 收到的是点击事件的 cx。
  confirmRow(key, hint, cx, onConfirm) {
    const colors = cx.theme().colors;
    return h_flex()
      .items_center()
      .gap(SPACE.xs)
      .child(div().flex_1().text_size(11).text_color(colors.muted_foreground).child(hint))
      .child(this.button(`${key}-confirm`, "确认", cx, onConfirm, { danger: true }))
      .child(
        this.button(`${key}-cancel`, "取消", cx, (cx) => {
          this.pending = null;
          cx.notify();
        }),
      );
  }

  button(id, caption, cx, onClick, options = {}) {
    const colors = cx.theme().colors;
    return Button.new(id)
      .flex()
      .items_center()
      .h(22)
      .px(SPACE.sm)
      .rounded(4)
      .border(1)
      .border_color(colors.border)
      .bg(colors.background)
      .text_size(11)
      .text_color(options.danger ? colors.destructive : colors.foreground)
      .hover((style) => style.bg(colors.muted))
      // 回调要用它自己收到的 cx：render 的 cx 只在那一次 render 里有效。
      .on_click((_event, cx) => onClick(cx))
      .child(caption);
  }
}
