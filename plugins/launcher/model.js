// 从宿主的各注册表算两个视图要画的东西。不 import 宿主模块：host 由 main.js 拼好传进来，测试换成假的。
//
// host：pages、views、command、view_badge、view_laid_out、view_shown（同 smelt-plugin-api 的同名函数）。

import { NAV, SETTINGS_VIEW } from "./contract.js";

/** 宿主的「设置…」。 */
export const SETTINGS = "smelt.app/settings";
/** 跟默认智能体开新对话，标题里写着是哪个智能体。 */
// 约定：smelt.agents 的 NEW_CONVERSATION，权威定义在 crates/plugins/agents/src/contract.rs。
export const NEW_CONVERSATION = "smelt.agents/new-conversation";
/** 出口（布局里摆的放打开的视图的那一格）。 */
const OUTLET = "smelt/outlet";
/**
 * 入口列表排最前的几个内置功能，按这个顺序；其余（用户装的插件，和布局里被拿掉的内置视图）跟在后面按
 * id 排。宿主视图表是登记顺序，各插件异步登记、谁先到谁在前，直接用会每次启动不一样。
 */
// 约定：各自 contract.js 的 VIEW（smelt.agents、smelt.automations、smelt.capabilities）。
const LEADING = ["smelt.agents/main", "smelt.automations/main", "smelt.capabilities/main"];

/** 这两个视图要据此重算的事件。标题是按当前语言取好的字，换了语言也要重算。 */
const AFFECTS = new Set([
  "views_changed",
  "laid_out_views_changed",
  "pages_changed",
  "commands_changed",
  "view_badges_changed",
  "shown_views_changed",
  "locale_changed",
]);

export function affects_launcher(event) {
  return AFFECTS.has(event.type);
}

/**
 * 不看布局时，这个视图能不能在入口列表里一行：不是设置页（设置里开），不按实例建（要别的插件带参数
 * 建），不是出口本身、不是启动栏自己，也不是哪页出口的默认内容（点会话就回去）。
 */
export function launchable(id, settings, instances, indexes) {
  return !settings && !instances && id !== OUTLET && id !== NAV && id !== SETTINGS_VIEW && !indexes.has(id);
}

/**
 * 入口列表上有什么：new_conversation 是新对话命令的标题（「新对话 · 写作助手」），命令没登记为 null、不出
 * 按钮；entries 是布局没摆、不用参数就能开的视图，attention 是挂着角标，shown 是出口正显示着它。
 */
export function nav_state(host) {
  const indexes = new Set(
    host
      .pages()
      .map((page) => page.index)
      .filter((index) => index != null),
  );
  return {
    new_conversation: host.command(NEW_CONVERSATION)?.title ?? null,
    entries: host
      .views()
      .filter(
        (view) =>
          launchable(view.id, view.settings != null, view.instances, indexes) && !host.view_laid_out(view.id),
      )
      .sort(by_nav_order)
      .map((view) => ({
        id: view.id,
        title: view.title,
        icon: view.icon,
        attention: host.view_badge(view.id) != null,
        shown: host.view_shown(view.id),
      })),
  };
}

/** 入口顺序：LEADING 里的按它排在前，其余按 id。 */
function by_nav_order(a, b) {
  const rank = (id) => {
    const at = LEADING.indexOf(id);
    return at < 0 ? LEADING.length : at;
  };
  return rank(a.id) - rank(b.id) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);
}

/** 设置按钮上有什么：available 是设置命令登记了，attention 是有设置页挂着角标。 */
export function settings_state(host) {
  return {
    available: host.command(SETTINGS) != null,
    attention: host.views().some((view) => view.settings != null && host.view_badge(view.id) != null),
  };
}

/** 两份状态一样：只由字、布尔组成，按字面比。 */
export function same_state(a, b) {
  return JSON.stringify(a) === JSON.stringify(b);
}

