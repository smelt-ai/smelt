// 「看板」插件：智能体会话按宿主给的三态分三列，点卡片跳到会话（#258）。
//
// 根视图只负责注册面板 `board`（见 smelt.json），自己不显示。布局里摆
// `plugin:com.smelt.kanban/board`，`props` 写 `{"machine": "<机器 id>"}` 只看那台机器，
// 不写看全部；摆两份、各给一台机器，就是并排的两块看板。
//
// 数据全部来自 `smelt-workspace`：`sessions()` 和 `machines()` 是内存快照，
// `changed()` 在它们变化时兑现。两个主题各一个循环等着，变了才重整一次，结果存在视图上，
// render 只画缓存。空闲时没有任何定时器。

import { View, div } from "gpui-kit";
import { v_flex, h_flex, DockArea } from "gpui-base";
import { Tag } from "gpui-component";
import { changed, machines, open_session, sessions } from "smelt-workspace";
import { columns } from "./board.js";

const SPACE = { xs: 4, sm: 8, md: 12 };
/** 一列最窄多宽。面板窄时三列折成上下排。 */
const COLUMN_MIN_WIDTH = 220;

export default class Kanban extends View {
  init() {
    DockArea.register_panel("board", Board);
  }

  render() {
    return div();
  }
}

/// 一块看板。宿主按布局建它，建好后把 `props` 交给 `deserialize`。
class Board extends View {
  init(_props, cx) {
    this.sessions = [];
    this.machines = [];
    /** 只看这台机器（机器 id）；`null` 看全部。 */
    this.machine = null;
    this.board = columns([], []);
    this.follow(cx, "sessions", () => {
      this.sessions = sessions().sessions;
    });
    this.follow(cx, "machines", () => {
      this.machines = machines().machines;
    });
  }

  /// 等 `topic` 变化，变了先读快照、再重整看板。`since` 从 -1 起，第一次立刻兑现。
  follow(cx, topic, read) {
    cx.spawn(async (cx) => {
      let seen = -1;
      for (;;) {
        seen = await changed(topic, seen);
        read();
        this.regroup();
        cx.notify();
      }
    });
  }

  /// 布局里的 `props`。`{}` 或不写就是看全部机器。
  deserialize(props) {
    const machine = props?.machine;
    this.machine = typeof machine === "string" && machine.length > 0 ? machine : null;
    this.regroup();
  }

  regroup() {
    this.board = columns(this.sessions, this.machines, this.machine);
  }

  render(cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .id("kanban")
      .size_full()
      .overflow_y_scrollbar()
      .text_color(colors.foreground)
      .child(
        h_flex()
          .flex_wrap()
          .items_start()
          .gap(SPACE.md)
          .p(SPACE.md)
          .children(this.board.map((column) => this.column(column, cx))),
      );
  }

  column(column, cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .flex_1()
      .min_w(COLUMN_MIN_WIDTH)
      .gap(SPACE.sm)
      .child(
        h_flex()
          .items_center()
          .gap(SPACE.sm)
          .child(div().text_xs().font_semibold().child(column.title))
          .child(div().text_xs().text_color(colors.muted_foreground).child(String(column.cards.length)))
          .child(div().flex_1())
          .child(div().text_xs().text_color(colors.muted_foreground).truncate().child(column.hint)),
      )
      .when(column.cards.length === 0, (el) =>
        el.child(
          div()
            .py(SPACE.md)
            .text_xs()
            .text_color(colors.muted_foreground)
            .child("没有会话"),
        ),
      )
      .children(column.cards.map((card) => this.card(card, cx)));
  }

  card(card, cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .id(`card-${card.id}`)
      .gap(SPACE.xs)
      .p(SPACE.sm)
      .rounded(6)
      .border(1)
      .border_color(card.viewing ? colors.ring : colors.border)
      .cursor_pointer()
      .hover((el) => el.bg(colors.accent))
      // 会话已经关掉时宿主什么也不做，下一次快照到了卡片自然消失。
      .on_click((_event, cx) => open_session(card.id))
      .child(
        h_flex()
          .items_center()
          .gap(SPACE.sm)
          .child(div().flex_1().min_w_0().text_sm().truncate().child(card.title))
          .when(card.unread, (el) => el.child(tag("未读", "info")))
          .when(card.offline, (el) => el.child(tag("离线", "warning"))),
      )
      .when(card.place.length > 0 || card.agent !== null, (el) =>
        el.child(
          div()
            .text_xs()
            .text_color(colors.muted_foreground)
            .truncate()
            .child([card.agent, card.place].filter((part) => part && part.length > 0).join(" · ")),
        ),
      )
      .when(card.detail.length > 0, (el) =>
        el.child(div().text_xs().text_color(colors.muted_foreground).truncate().child(card.detail)),
      );
  }
}

/// @param {"info" | "warning"} variant
function tag(text, variant) {
  return Tag.new().variant(variant).outline().size("xsmall").flex_none().child(text);
}
