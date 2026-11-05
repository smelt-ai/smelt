// 「队伍」：放在左栏的会话列表样例（面板 `roster`）。机器 → 项目 → 智能体卡片，卡片上是头像、状态和
// 体力条（上下文还剩多少）。点卡片跳到会话，点项目名切项目，项目旁的「+」从这台机器
// 能新建的东西里挑一个起会话。
//
// 数据全部来自 `smelt-workspace`：两个主题各一个 `changed()` 循环等着，变了才重排，
// 结果存在视图上，render 只画缓存。空闲时没有任何定时器。

import { View, div } from "gpui-kit";
import { v_flex, h_flex, DockArea } from "gpui-base";
import {
  changed,
  machines,
  new_session,
  new_session_options,
  open_session,
  select_project,
  sessions,
} from "smelt-workspace";
import { LOOSE, STATUS, launchChoices, roster } from "./roster.js";

const SPACE = { xs: 4, sm: 8, md: 12 };
const AVATAR = 28;
const STAMINA_HEIGHT = 4;

/// 面板 `roster`：机器 → 项目 → 智能体卡片。宿主按布局建它。
class Party extends View {
  init(_props, cx) {
    this.sessions = [];
    this.machines = [];
    this.roster = [];
    /** 正在挑新建选项的项目：{ machine, root, choices }，choices 为 null 表示还在读。 */
    this.picking = null;
    this.follow(cx, "sessions", () => {
      this.sessions = sessions().sessions;
    });
    this.follow(cx, "machines", () => {
      this.machines = machines().machines;
    });
  }

  /// 等 `topic` 变化，变了先读快照、再重排。`since` 从 -1 起，第一次立刻兑现。
  follow(cx, topic, read) {
    cx.spawn(async (cx) => {
      let seen = -1;
      for (;;) {
        seen = await changed(topic, seen);
        read();
        this.roster = roster(this.machines, this.sessions);
        cx.notify();
      }
    });
  }

  /// 展开或收起这个项目的新建选项。选项每次现取：设置改了、远程机器装了新东西都能看到。
  togglePicker(machine, root, cx) {
    if (this.picking?.machine === machine && this.picking.root === root) {
      this.picking = null;
      cx.notify();
      return;
    }
    const picking = { machine, root, choices: null, error: null };
    this.picking = picking;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        picking.choices = launchChoices(await new_session_options(machine));
      } catch (error) {
        picking.error = String(error);
      }
      // 读的时候用户可能已经收起或换了项目，那就不画到别处去。
      if (this.picking === picking) cx.notify();
    });
  }

  launch(machine, root, key, cx) {
    this.picking = null;
    new_session(machine, root, key);
    cx.notify();
  }

  render(cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .id("party")
      .size_full()
      .overflow_y_scrollbar()
      .gap(SPACE.md)
      .p(SPACE.sm)
      .text_color(colors.foreground)
      .when(this.roster.length === 0, (el) =>
        el.child(div().p(SPACE.md).text_xs().text_color(colors.muted_foreground).child("还没有机器")),
      )
      .children(this.roster.map((machine) => this.machine(machine, cx)));
  }

  machine(machine, cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .gap(SPACE.sm)
      .child(
        h_flex()
          .items_center()
          .gap(SPACE.sm)
          .px(SPACE.xs)
          .child(
            div()
              .size(6)
              .rounded_full()
              .flex_none()
              .bg(machine.online ? colors.primary : colors.muted_foreground),
          )
          .child(div().flex_1().min_w_0().text_xs().font_semibold().truncate().child(machine.name))
          .when(machine.waiting > 0, (el) =>
            el.child(div().text_xs().text_color(colors.destructive).child(`${machine.waiting} 个在等你`)),
          ),
      )
      .children(machine.projects.map((project) => this.project(machine, project, cx)));
  }

  project(machine, project, cx) {
    const colors = cx.theme().colors;
    const key = `${machine.id}\0${project.root}`;
    const picking =
      this.picking?.machine === machine.id && this.picking.root === project.root ? this.picking : null;
    return v_flex()
      .gap(SPACE.xs)
      .child(
        h_flex()
          .items_center()
          .gap(SPACE.xs)
          .child(
            div()
              .id(`project-${key}`)
              .flex_1()
              .min_w_0()
              .px(SPACE.xs)
              .rounded(4)
              .text_xs()
              .truncate()
              .text_color(project.active ? colors.foreground : colors.muted_foreground)
              .when(project.active, (el) => el.font_semibold())
              .when(project.root !== LOOSE, (el) =>
                el
                  .cursor_pointer()
                  .hover((el) => el.bg(colors.accent))
                  .on_click(() => select_project(machine.id, project.root)),
              )
              .child(project.name),
          )
          // 「其他」不是一个目录，没法在那儿新建；离线的机器起不了会话。
          .when(project.root !== LOOSE && machine.online, (el) =>
            el.child(
              div()
                .id(`new-${key}`)
                .flex_none()
                .px(SPACE.xs)
                .rounded(4)
                .text_xs()
                .text_color(colors.muted_foreground)
                .cursor_pointer()
                .hover((el) => el.bg(colors.accent))
                .on_click((_event, cx) => this.togglePicker(machine.id, project.root, cx))
                .child(picking ? "收起" : "+"),
            ),
          ),
      )
      .when(picking !== null, (el) => el.child(this.picker(machine.id, project.root, picking, cx)))
      .children(project.members.map((member) => this.member(member, cx)));
  }

  picker(machine, root, picking, cx) {
    const colors = cx.theme().colors;
    const note = (text) => div().px(SPACE.sm).py(SPACE.xs).text_xs().text_color(colors.muted_foreground).child(text);
    let body;
    if (picking.error !== null) body = [note(picking.error)];
    else if (picking.choices === null) body = [note("正在读这台机器能起什么…")];
    else
      body = picking.choices.map((choice) =>
        div()
          .id(`launch-${machine}\0${root}\0${choice.key}`)
          .px(SPACE.sm)
          .py(SPACE.xs)
          .rounded(4)
          .text_xs()
          .truncate()
          .cursor_pointer()
          .hover((el) => el.bg(colors.accent))
          .on_click((_event, cx) => this.launch(machine, root, choice.key, cx))
          .child(choice.label),
      );
    return v_flex().rounded(6).border(1).border_color(colors.border).py(SPACE.xs).children(body);
  }

  member(member, cx) {
    const colors = cx.theme().colors;
    const status = STATUS[member.status];
    return h_flex()
      .id(`member-${member.id}`)
      .items_center()
      .gap(SPACE.sm)
      .p(SPACE.sm)
      .rounded(6)
      .border(1)
      .border_color(member.viewing ? colors.ring : colors.border)
      .cursor_pointer()
      .hover((el) => el.bg(colors.accent))
      // 会话已经关掉时宿主什么也不做，下一次快照到了卡片自然消失。
      .on_click(() => open_session(member.id))
      .child(
        div()
          .size(AVATAR)
          .flex_none()
          .rounded(6)
          .border(2)
          .border_color(colors[status.tone])
          .bg(colors.muted)
          .flex()
          .items_center()
          .justify_center()
          .text_sm()
          .font_semibold()
          .child(member.initial),
      )
      .child(
        v_flex()
          .flex_1()
          .min_w_0()
          .gap(SPACE.xs)
          .child(
            h_flex()
              .items_center()
              .gap(SPACE.xs)
              .child(div().flex_1().min_w_0().text_sm().truncate().child(member.title))
              .when(member.unread, (el) => el.child(div().size(6).rounded_full().flex_none().bg(colors.primary))),
          )
          .child(
            h_flex()
              .items_center()
              .gap(SPACE.sm)
              .child(div().flex_none().text_xs().text_color(colors[status.tone]).child(status.label))
              .when(member.model !== null, (el) =>
                el.child(
                  div().flex_1().min_w_0().text_xs().text_color(colors.muted_foreground).truncate().child(member.model),
                ),
              ),
          )
          .when(member.stamina !== null, (el) => el.child(this.stamina(member.stamina, cx))),
      );
  }

  /// 体力条：上下文还剩多少。剩不到两成时换成警示色，提醒该压缩或开新对话了。
  stamina(value, cx) {
    const colors = cx.theme().colors;
    return div()
      .w_full()
      .h(STAMINA_HEIGHT)
      .rounded(STAMINA_HEIGHT / 2)
      .bg(colors.muted)
      .child(
        div()
          .h_full()
          .w(`${Math.round(value * 100)}%`)
          .rounded(STAMINA_HEIGHT / 2)
          .bg(value < 0.2 ? colors.destructive : colors.primary),
      );
  }
}

/// 插件的根：只注册面板 `roster`（见 smelt.json），自己不显示。
export default class PartyPlugin extends View {
  init() {
    DockArea.register_panel("roster", Party);
  }

  render() {
    return div();
  }
}
