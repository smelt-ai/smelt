// 账户额度：那一条（每家一枚额度环，悬停看各窗口和重置时间，点开看全部，点哪家查哪家）和设置里每家查不查。
//
// 额度是账户状态，不属于哪段会话。凭据在本机运行时上，查询和跨启动缓存都由运行时做（quota.*）。这里只管
// 节奏：启动先显示运行时缓存里的结果，同时立即查一轮；之后每 REFRESH_MS 全量查一次，用户点哪家就单查哪家
// （只有这时运行时才可能弹钥匙串窗口）。每家查不查的开关也存在运行时。
//
// 状态在模块里一份，两个视图都只读它；变了叫开着的视图重画。

import { PathBuilder, div, svg, View } from "gpui-kit";
import { DockArea } from "gpui-base";
import * as api from "smelt-plugin-api";
import {
  SmeltButton,
  SmeltListRow,
  SmeltPopover,
  SmeltSettingsPage,
  SmeltSettingsRow,
  SmeltSettingsSection,
  SmeltSpinner,
  SmeltSwitch,
  SmeltTooltip,
  SmeltTruncatedLine,
} from "smelt-ui";

import { BAR, BAR_NAME, SETTINGS_NAME, SETTINGS_VIEW } from "./contract.js";
import {
  REFRESH_MS,
  REFRESH_TIMEOUT_MS,
  apply,
  begin_refresh,
  chips,
  empty_state,
  enabled_targets,
  next_manual,
  popover_entries,
  restore,
  ring_fraction,
  ring_tone,
  setting_row,
} from "./model.js";

/** 本机。 */
const LOCAL = null;
/** 点开的列表宽度：名字一栏、用量条、百分比放得下。 */
const POPOVER_WIDTH = 268;
/** 弹层里名字那一栏：放得下「Copilot · Pi」。 */
const NAME_WIDTH = 84;
/** 弹层标题那一行的高（smelt-ui 的 height::sm）。 */
const ROW_SM = 28;

const host = { quota_target: api.quota_target, quota_facts: api.quota_facts, t: api.t };

const state = empty_state();
/** 设置页：每家的开关和钥匙串拒绝状态，error 是最近一次读取或保存失败的原因（失败时保留上次的列表）。 */
let settings = { providers: [], error: null };
/** 用户点了要单查的账户，排给刷新循环；wake 叫醒等着的循环。 */
const queue = [];
let wake = null;
/** 设置页上一次是不是露着：露出来那一下读开关。 */
let settings_shown = false;

const bar_views = new Set();
const settings_views = new Set();

function redraw(instances) {
  for (const instance of [...instances]) {
    if (!api.notify_view(instance)) instances.delete(instance);
  }
}

function changed() {
  redraw(bar_views);
  redraw(settings_views);
}

function message(error) {
  return error?.message ?? String(error);
}

/** 套上 theme().type 里的一种字号：字号、行高、字重一起给。 */
function typed(el, style) {
  return el.text_size(style.text_size).line_height(style.line_height).font_weight(style.font_weight);
}

// ---- 刷新 ----

/** 单查一家。刷新进行中再点不会丢：排进队列，这一轮结束后合并成一轮处理。 */
function request_refresh(id) {
  queue.push(id);
  wake?.();
}

/** 等下一轮：全关了只等用户点；否则等到定时或用户点，先到先算。返回这一轮单查哪家，null 是全量。 */
async function next_round(cx) {
  if (queue.length === 0) {
    /** @type {import("gpui-kit").Task | null} */
    let timer = null;
    await new Promise((resolve) => {
      wake = resolve;
      // 轮询例外：额度在远端账户上变化，没有事件可等；周期 5 分钟，用户关掉全部账户后不再醒。
      if (state.targets.length > 0) timer = cx.timer.after(REFRESH_MS, () => resolve());
    });
    wake = null;
    timer?.cancel();
  }
  return next_manual(queue);
}

/** 刷新循环。只依赖本机运行时，和有没有会话无关；跟应用一样长。 */
async function refresh_loop(cx) {
  // 运行时可能还在起来：等它在线再读，读失败就退避后回去等。
  let failures = 0;
  let cached;
  for (;;) {
    if (!(await api.wait_online(LOCAL, failures))) return;
    try {
      cached = await api.call(LOCAL, "quota.get");
      break;
    } catch (error) {
      console.error(`[quota] 读额度缓存失败：${message(error)}`);
    }
    failures += 1;
  }
  restore(state, cached.targets, cached.statuses);
  changed();
  // 先显示缓存，同时立即查一轮。
  let first = true;
  for (;;) {
    const target = first ? null : await next_round(cx);
    first = false;
    begin_refresh(state, target);
    changed();
    try {
      const result = await api.call(LOCAL, "quota.refresh", { target }, REFRESH_TIMEOUT_MS);
      // 全量刷新顺带更新范围：用户在 Pi 里新登录一家订阅 provider 后，这是它出现的唯一入口。
      if (target == null) restore(state, result.targets, []);
      apply(state, result.statuses);
    } catch (error) {
      // 连不上运行时不是额度失效：保留上次的数字，只结束「查询中」。
      console.error(`[quota] 刷新额度失败：${message(error)}`);
      apply(state, []);
    }
    changed();
    // 这一轮可能触发或解除钥匙串熔断，设置页露着就跟着重读。
    if (api.view_shown(SETTINGS_VIEW)) refresh_settings();
  }
}

// ---- 开关 ----

/** 读一次每家的开关和拒绝状态。 */
function refresh_settings() {
  api
    .call(LOCAL, "quota.settings.get")
    .then((result) => {
      settings = { providers: result.providers, error: null };
    })
    .catch((error) => {
      settings = { ...settings, error: message(error) };
    })
    .finally(() => redraw(settings_views));
}

/** 打开或关掉一家。范围立刻更新；打开时单查一次，可能弹钥匙串窗口，这正是用户刚做的操作。 */
function set_enabled(id, enabled) {
  api
    .call(LOCAL, "quota.set_enabled", { target: id, enabled })
    .then((result) => {
      restore(state, enabled_targets(result.providers), []);
      settings = { providers: result.providers, error: null };
      if (enabled) request_refresh(id);
    })
    .catch((error) => {
      settings = { ...settings, error: message(error) };
    })
    .finally(changed);
}

// ---- 那一条 ----

/** 一条圆周的路径，从正上方顺时针走 fraction 圈（满圈是两段半圆）。 */
function arc(center, radius, stroke, fraction) {
  const top = center - radius;
  let path = PathBuilder.stroke(stroke).move_to(center, top);
  if (fraction >= 0.999) {
    return path
      .arc_to(radius, radius, 0, false, true, center, center + radius)
      .arc_to(radius, radius, 0, false, true, center, top);
  }
  const theta = fraction * Math.PI * 2;
  return path.arc_to(
    radius,
    radius,
    0,
    fraction > 0.5,
    true,
    center + radius * Math.sin(theta),
    center - radius * Math.cos(theta),
  );
}

/** 一个圆点，给弧线两头当圆头：路径的弧线端点是平的。 */
function dot(x, y, radius) {
  return PathBuilder.fill()
    .move_to(x - radius, y)
    .arc_to(radius, radius, 0, false, true, x + radius, y)
    .arc_to(radius, radius, 0, false, true, x - radius, y);
}

/** 额度环：长度表示已用量，双环依次是 5 小时、周。画在 26×26 的格子里。 */
function rings_element(rings, palette) {
  const size = 26;
  const center = size / 2;
  const stroke = rings.length > 1 ? 1.75 : 2;
  const outer = rings.length > 1 ? 12.125 : 10;
  const layer = (path, color) => window.paint_path(path.build(), color).absolute().inset_0().size(size);
  const layers = [];
  rings.forEach((ring, index) => {
    const radius = outer - index * 3.5;
    layers.push(layer(arc(center, radius, stroke, 1), palette.hair));
    const fraction = ring_fraction(ring.used);
    if (fraction <= 0) return;
    const color = palette[ring_tone(ring.used)];
    layers.push(layer(arc(center, radius, stroke, fraction), color));
    if (fraction < 0.999) {
      const theta = fraction * Math.PI * 2;
      layers.push(layer(dot(center, center - radius, stroke / 2), color));
      layers.push(
        layer(dot(center + radius * Math.sin(theta), center - radius * Math.cos(theta), stroke / 2), color),
      );
    }
  });
  return div().absolute().inset_0().size(size).children(layers);
}

/** 一家账户的额度：环和服务图标（手点刷新时换成转圈），不知道总额时写剩余数字。 */
function chip_element(chip, look) {
  const p = look.palette;
  const tip = chip.refreshing ? api.t("refreshing", { hint: chip.hint }) : chip.hint;
  const icon_size = chip.rings.length > 1 ? 9 : 11;
  return SmeltTooltip.new(`quota-chip-tip-${chip.id}`, tip).child(
    div()
      .flex()
      .flex_shrink_0()
      .items_center()
      .gap_1()
      .child(
        div()
          .relative()
          .size(26)
          .flex()
          .items_center()
          .justify_center()
          .when(chip.rings.length > 0, (box) => box.child(rings_element(chip.rings, p)))
          .child(
            chip.refreshing
              ? SmeltSpinner.new(`quota-chip-refresh-${chip.id}`, p[chip.tone])
              : svg(chip.icon).size(icon_size).text_color(p.text),
          ),
      )
      .when(chip.rings.length === 0, (row) => row.child(chip.text)),
  );
}

/** 弹层里的一行：图标、名字、用量条、百分比。点哪家查哪家；暂未查到或失败的也列，点一下主动查。 */
function popover_row(entry, look) {
  const p = look.palette;
  let row;
  if (entry.kind === "ready") {
    const chip = entry.chip;
    row = {
      id: chip.id,
      name: chip.name,
      icon: chip.icon,
      color: p[chip.tone],
      text: chip.text,
      used: chip.used,
      refreshing: chip.refreshing,
      name_color: p.text,
      tip: chip.refreshing
        ? api.t("refreshing", { hint: chip.hint })
        : api.t("click_to_refresh", { hint: chip.hint }),
    };
  } else {
    row = {
      id: entry.id,
      name: entry.name,
      icon: entry.icon,
      color: entry.error != null ? p.need : p.faint,
      text: "—",
      used: 0,
      refreshing: entry.refreshing,
      name_color: p.muted,
      tip: entry.refreshing
        ? api.t("refreshing", { hint: entry.name })
        : entry.error != null
          ? api.t("click_to_retry", { error: entry.error })
          : api.t("click_to_check"),
    };
  }
  const used = Math.min(Math.max(row.used / 100, 0), 1);
  return SmeltTooltip.new(`quota-tip-${row.id}`, row.tip)
    .w_full()
    .child(
      typed(SmeltListRow.new(`quota-${row.id}`), look.type.text_xsmall)
        .h(30)
        .px_2()
        .flex()
        .items_center()
        .gap_2()
        .rounded(6)
        .child(svg(row.icon).flex_shrink_0().size(14).text_color(row.color))
        .child(
          div()
            .w(NAME_WIDTH)
            .flex_shrink_0()
            .overflow_hidden()
            .text_color(row.name_color)
            .child(SmeltTruncatedLine.new(row.name)),
        )
        .child(
          div()
            .flex_1()
            .h(4)
            .rounded_full()
            .overflow_hidden()
            .bg(p.hair)
            .when(row.used > 0, (track) =>
              track.child(div().h_full().w(`${used * 100}%`).rounded_full().bg(row.color)),
            ),
        )
        // 点一下发起查询、还没回来时，右侧换成转圈：真在等，等完就停。
        .child(
          div()
            .w(36)
            .flex_shrink_0()
            .flex()
            .justify_end()
            .font_family(look.fonts.mono)
            .text_color(row.color)
            .child(row.refreshing ? SmeltSpinner.new(`quota-refresh-${row.id}`, row.color) : row.text),
        )
        // 刷新开始、结束都会重画这一行。
        .on_click(() => request_refresh(row.id)),
    );
}

class Bar extends View {
  init() {
    this.instance = api.opened_view().instance;
    this.open = false;
    bar_views.add(this.instance);
  }

  render() {
    const look = api.theme();
    const p = look.palette;
    const now = Date.now();
    const shown = chips(state, host, now);
    const rows = popover_entries(state, host, now);
    // 自己就是一行：贴着窗口下面的角时两头给窗口圆角让位，不贴时留标准边距。
    const insets = api.safe_area(BAR).bar_insets;
    const bar = typed(div(), look.type.text_xsmall)
      .size_full()
      .min_w_0()
      .pl(insets.left)
      .pr(insets.right)
      .flex()
      .items_center()
      .justify_end()
      .font_family(look.fonts.mono);
    // 没有能列的账户（都关了，或开着的都没登录）：空着。用户手点的那家会占一行，不用另给过渡。
    if (rows.length === 0) return bar;
    // 能折行的触发点：按钮高度固定折不了行，用列表行（悬停、弹层开着时的底跟别的行一样）。
    let trigger = SmeltListRow.new("quota-bar")
      .selected(this.open)
      .min_w_0()
      .px_1p5()
      .py_1()
      .rounded(look.radius.row)
      .flex()
      .flex_wrap()
      .justify_end()
      .items_center()
      .gap_x_2()
      .gap_y_1();
    if (shown.length === 0) {
      const loading = state.refreshing.size > 0;
      trigger = SmeltTooltip.new("quota-bar-tip", api.t(loading ? "loading" : "empty_trigger")).child(
        trigger.child(
          div()
            .flex()
            .items_center()
            .gap_1()
            .text_color(loading ? p.faint : p.muted)
            .child(svg("icons/gauge.svg").size(14).text_color(p.muted))
            .when(loading, (label) => label.child(api.t("loading"))),
        ),
      );
    } else {
      trigger = trigger.children(shown.map((chip) => chip_element(chip, look)));
    }
    return bar.child(
      SmeltPopover.new("quota-bar-popover")
        .side("bottom_right")
        // 触发点要能收窄，额度才会在里面折行，不把这一条撑宽。
        .shrink_trigger(true)
        .open(this.open)
        .on_open_change((open) => {
          this.open = open;
          api.notify_view(this.instance);
        })
        .w(POPOVER_WIDTH)
        .p_1()
        .trigger(trigger)
        .content(
          div()
            .w_full()
            .flex()
            .flex_col()
            .child(
              typed(div(), look.type.text_xsmall_medium)
                .h(ROW_SM)
                .px_2()
                .flex()
                .items_center()
                .text_color(p.muted)
                .child(api.t("title")),
            )
            .children(rows.map((entry) => popover_row(entry, look))),
        ),
    );
  }
}

// ---- 设置 ----

/** 每家账户一行：查询开关、主动刷新按钮，钥匙串被拒时换成重试。 */
function provider_row(provider) {
  const target = api.quota_target(provider.target);
  if (target == null) return null;
  const facts = setting_row(state, provider);
  const id = facts.id;
  let row = SmeltSettingsRow.new(target.name);
  if (facts.denied) {
    row = row.description(api.t("keychain_denied")).child(
      SmeltButton.new(`quota-retry-${id}`)
        .quiet()
        .small()
        .loading(facts.refreshing)
        .label(api.t("retry"))
        .on_click(() => request_refresh(id)),
    );
  } else if (facts.enabled) {
    if (facts.error != null) row = row.description(facts.error).clamp_description(`quota-error-${id}`, 2);
    const name = api.t("refresh_one", { name: target.name });
    row = row.child(
      SmeltButton.new(`quota-refresh-${id}`)
        .quiet()
        .small()
        .icon("icons/refresh.svg")
        .loading(facts.refreshing)
        .accessibility_label(name)
        .tooltip(name)
        .on_click(() => request_refresh(id)),
    );
  }
  return row.child(
    SmeltSwitch.new(`quota-enabled-${id}`)
      .checked(facts.enabled)
      .accessibility_label(api.t("enable_label", { name: target.name }))
      .on_change((checked) => set_enabled(id, checked)),
  );
}

class Settings extends View {
  init() {
    settings_views.add(api.opened_view().instance);
    // 建出来就露着时等不到「露出来」的事件，先读一次。
    refresh_settings();
  }

  render() {
    const look = api.theme();
    // 页名就是「账户额度」，这一组不再带标题。
    let section = SmeltSettingsSection.new();
    if (settings.error != null) {
      section = section.child(
        SmeltSettingsRow.new(api.t("read_failed")).child(
          typed(div(), look.type.text_xsmall)
            .max_w(320)
            .overflow_hidden()
            .text_color(look.palette.muted)
            .child(SmeltTruncatedLine.new(settings.error)),
        ),
      );
    }
    for (const provider of settings.providers) {
      const row = provider_row(provider);
      if (row != null) section = section.child(row);
    }
    return SmeltSettingsPage.new("quota-settings").child(section);
  }
}

// ---- 宿主事件 ----

async function follow_events() {
  for (;;) {
    const event = await api.next_event();
    try {
      if (event.type === "shown_views_changed") {
        const shown = api.view_shown(SETTINGS_VIEW);
        // 设置页露出来时读一次开关，render 只读它。
        if (shown && !settings_shown) refresh_settings();
        settings_shown = shown;
      }
      // 两头的边距在 render 里现读 safe_area；文案按当前语言现取。
      if (event.type === "safe_area_changed") redraw(bar_views);
      if (event.type === "locale_changed") changed();
    } catch (error) {
      console.error(`[quota] 处理宿主事件 ${event.type} 失败：`, error);
    }
  }
}

export default class Root extends View {
  init(_props, cx) {
    DockArea.register_panel(BAR_NAME, Bar);
    DockArea.register_panel(SETTINGS_NAME, Settings);
    api.register_views([
      // 一条额度，不要外壳的标题带。
      { name: BAR_NAME, title: api.text("title"), icon: "icons/gauge.svg", draws_top_bar: true },
      {
        name: SETTINGS_NAME,
        title: api.text("title"),
        icon: "icons/gauge.svg",
        settings: { group: "agents", keywords: ["quota", "usage", "limit", "keychain", "额度", "用量", "钥匙串"] },
      },
    ]);
    follow_events();
    refresh_loop(cx).catch((error) => console.error(`[quota] 刷新循环停了：${message(error)}`));
  }

  render() {
    return div();
  }
}
