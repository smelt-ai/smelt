// 启动栏：侧栏顶上的「新对话」和入口列表，侧栏底下的「设置」。
//
// 功能插件不声明自己摆在哪：布局摆了的视图在那页看；没摆的、不用参数就能开的（不是设置页、不按实例建、
// 不是哪页出口的默认内容），在入口列表里一行，点了用 "smelt.app/open-view" 打开——宿主放进出口。不想要
// 这些的布局不摆这两个视图，换成自己的入口。
//
// 什么时候重算全靠事件：视图、页、命令的登记变了，摆在布局里的视图变了，角标、看得见的视图变了，换了
// 语言。算好的结果留在模块里，render 只读它。

import { div, svg, View } from "gpui-kit";
import { DockArea } from "gpui-base";
import * as api from "smelt-plugin-api";
import { SmeltButton, SmeltListRow, SmeltTooltip, SmeltTruncatedLine, SmeltWindowBar } from "smelt-ui";

import { NAV, NAV_NAME, SETTINGS_NAME, SETTINGS_VIEW } from "./contract.js";
import {
  NEW_CONVERSATION,
  SETTINGS,
  affects_launcher,
  nav_state,
  same_state,
  settings_state,
} from "./model.js";

const host = {
  pages: api.pages,
  views: api.views,
  command: api.command,
  view_badge: api.view_badge,
  view_laid_out: api.view_laid_out,
  view_shown: api.view_shown,
};

/** 行高（smelt-ui 的 height::md）。 */
const ROW_MD = 32;

/** 算好的两份状态，render 只读它；插件根 init 时第一次算（宿主函数只能在那之后调）。 */
let nav = { new_conversation: null, entries: [] };
let settings = { available: false, attention: false };
/** 开着的两种视图的实例，状态变了叫它们重画。 */
const nav_views = new Set();
const settings_views = new Set();

function redraw(instances) {
  for (const instance of [...instances]) {
    if (!api.notify_view(instance)) instances.delete(instance);
  }
}

function refresh() {
  const next_nav = nav_state(host);
  if (!same_state(next_nav, nav)) {
    nav = next_nav;
    redraw(nav_views);
  }
  const next_settings = settings_state(host);
  if (!same_state(next_settings, settings)) {
    settings = next_settings;
    redraw(settings_views);
  }
}

/** 套上 theme().type 里的一种字号：字号、行高、字重一起给。 */
function typed(el, style) {
  return el.text_size(style.text_size).line_height(style.line_height).font_weight(style.font_weight);
}

// ---- 执行 ----

/** 失败了报出来（命令自己不报）：没有落点，发系统通知。 */
function notify_error(error) {
  const message = error?.message ?? String(error);
  console.error(`[launcher] ${message}`);
  api.run("smelt.app/notify", { subtitle: api.t("notify.error"), body: message }).catch(() => {});
}

function open(view) {
  api.run("smelt.app/open-view", { view }).catch(notify_error);
}

function run(command) {
  api.run(command).catch(notify_error);
}

// ---- 入口 ----

/** 占满一行的「新对话」：名字写着默认智能体，悬停带快捷键。 */
function new_conversation_button(title) {
  return SmeltTooltip.new("launcher-new-conversation-tooltip", title)
    .command(NEW_CONVERSATION)
    .w_full()
    .min_w_0()
    .child(
      SmeltButton.new("launcher-new-conversation")
        .w_full()
        .min_w_0()
        .icon("icons/plus.svg")
        .accessibility_label(title)
        .child(div().min_w_0().overflow_hidden().child(SmeltTruncatedLine.new(title)))
        .on_click(() => run(NEW_CONVERSATION)),
    );
}

/** 一个视图的入口：图标、名字、要处理时的红点；出口正显示着它时亮着。 */
function entry_row(entry, look) {
  const p = look.palette;
  const selected = entry.shown;
  return (
    SmeltListRow.new(`launcher-${entry.id}`)
      .selected(selected)
      .h(ROW_MD)
      .px_2()
      .flex()
      .items_center()
      .gap_2()
      .rounded(look.radius.row)
      .child(
        svg(entry.icon)
          .flex_shrink_0()
          .size(16)
          .text_color(selected ? p.muted_selected : p.muted),
      )
      // 名字的字色由 SmeltListRow 给；选中加中等字重。
      .child(
        typed(div().flex_1().min_w_0(), look.type.ui)
          .when(selected, (label) => label.font_weight(look.type.ui_medium.font_weight))
          .child(SmeltTruncatedLine.new(entry.title)),
      )
      .when(entry.attention, (row) => row.child(div().flex_shrink_0().size(6).rounded_full().bg(p.bad)))
      .on_click(() => open(entry.id))
  );
}

class Nav extends View {
  init() {
    nav_views.add(api.opened_view().instance);
  }

  render() {
    const look = api.theme();
    // 不撑满：高度跟着内容，布局按内容给这一格定高。贴着窗口顶时先画标题栏那一条（让开红黄绿按钮、
    // 拖窗口），入口从它下面开始；不贴时它什么都不画。
    return div()
      .w_full()
      .min_w_0()
      .flex()
      .flex_col()
      .child(SmeltWindowBar.new(NAV))
      .child(
        div()
          .w_full()
          .min_w_0()
          .px_2()
          .pt_2()
          .pb_1()
          .flex()
          .flex_col()
          .gap_1()
          .when(nav.new_conversation != null, (list) => list.child(new_conversation_button(nav.new_conversation)))
          .children(nav.entries.map((entry) => entry_row(entry, look))),
      );
  }
}

// ---- 设置 ----

class SettingsButton extends View {
  init() {
    settings_views.add(api.opened_view().instance);
  }

  render() {
    const look = api.theme();
    // 自己就是一行：贴着窗口下面的角时两头给窗口圆角让位，不贴时留标准边距。
    const insets = api.safe_area(SETTINGS_VIEW).bar_insets;
    // 普通入口，不做成通栏的大按钮。
    return div()
      .size_full()
      .min_w_0()
      .pl(insets.left)
      .pr(insets.right)
      .flex()
      .items_center()
      .when(settings.available, (bar) =>
        bar.child(
          div()
            .id("launcher-settings")
            .relative()
            .flex_shrink_0()
            .child(
              SmeltButton.new("launcher-settings-button")
                .quiet()
                .icon("icons/settings.svg")
                .label(api.t("settings"))
                .on_click(() => run(SETTINGS)),
            )
            .when(settings.attention, (slot) =>
              slot.child(div().absolute().top(4).right(4).size(5).rounded_full().bg(look.palette.bad)),
            ),
        ),
      );
  }
}

async function follow_events() {
  for (;;) {
    const event = await api.next_event();
    try {
      if (affects_launcher(event)) refresh();
      // 设置按钮两头的边距在 render 里现读 safe_area。
      if (event.type === "safe_area_changed") redraw(settings_views);
    } catch (error) {
      console.error(`[launcher] 处理宿主事件 ${event.type} 失败：`, error);
    }
  }
}

export default class Root extends View {
  init() {
    DockArea.register_panel(NAV_NAME, Nav);
    DockArea.register_panel(SETTINGS_NAME, SettingsButton);
    api.register_views([
      // 一列入口，不要外壳的标题带。
      { name: NAV_NAME, title: api.text("view.nav"), icon: "icons/layout-grid.svg", draws_top_bar: true },
      { name: SETTINGS_NAME, title: api.text("view.settings"), icon: "icons/settings.svg", draws_top_bar: true },
    ]);
    refresh();
    follow_events();
  }

  render() {
    return div();
  }
}
