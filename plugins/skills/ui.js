// 技能插件各视图共用的小部件。控件一律用 `"gpui-component"` 里带样式的组件，和 smelt
// 主界面同一套主题；布局用 gpui-base 的 `v_flex` / `h_flex`。
//
// 视觉规格跟 smelt 自己的面板走：字号只用跟随界面缩放的 `text_xs` / `text_sm`，不写
// 像素；按钮用紧凑档；不画页面底色（宿主铺实底），悬停、选中用 `accent`。

import { div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { AlertDialog, Button, Tag } from "gpui-component";

export const SPACE = { xs: 4, sm: 8, md: 12, lg: 16 };
export const MONO = "SF Mono, Menlo, monospace";
/** 回收目录，相对主目录。 */
export const TRASH = ".smelt/trash/skills";

/// gpui-component 的按钮，默认 `ghost`、`xsmall`：和 smelt 自己面板里的紧凑按钮同一档。
/// 回调要用它自己收到的 cx：render 的 cx 只在那一次 render 里有效。
/**
 * @param {"ghost" | "outline" | "primary" | "danger"} [variant]
 * @param {"xsmall" | "small"} [size]
 */
export function button(id, label, onClick, variant = "ghost", size = /** @type {"xsmall" | "small"} */ ("xsmall")) {
  return Button.new(id)
    .label(label)
    .size(size)
    [variant]()
    .on_click((_event, cx) => onClick(cx));
}

/// @param {"secondary" | "danger" | "info" | "warning" | "success"} variant
export function tag(text, variant) {
  // 描边：实底的 warning/danger 在深色面板上太抢眼，压过旁边的正文。
  return Tag.new().variant(variant).outline().size("xsmall").flex_none().child(text);
}

export function hint(text, cx) {
  const colors = cx.theme().colors;
  return div().text_xs().whitespace_normal().text_color(colors.muted_foreground).child(text);
}

export function mono(text) {
  return div().text_xs().font_family(MONO).whitespace_normal().child(text);
}

/// 有边框的一块，放表单或列表。
export function box(children, cx) {
  const colors = cx.theme().colors;
  return v_flex()
    .gap(SPACE.sm)
    .p(SPACE.md)
    .border(1)
    .border_color(colors.border)
    .rounded(8)
    .children(children);
}

/// 小节：标题一行，右边可以放一个操作。
export function section(title, action, children, cx) {
  const colors = cx.theme().colors;
  return v_flex()
    .gap(SPACE.sm)
    .child(
      h_flex()
        .items_center()
        .gap(SPACE.sm)
        .child(div().flex_1().text_xs().font_semibold().text_color(colors.muted_foreground).child(title))
        .when(action !== null, (el) => el.child(action)),
    )
    .children(children);
}

/// 危险操作的确认：居中的模态框。`AlertDialog` 本身就是那个触发按钮，点「确定」时
/// `onOk(cx)` 收到的是这次点击的 cx。`onError` 收弹窗自己出错的消息。
export function confirm(id, label, title, description, onOk, onError) {
  return AlertDialog.new(id, label, onError)
    .title(title)
    .description(description)
    .show_cancel(true)
    .on_ok((cx) => onOk(cx));
}

export function edited(ms) {
  if (!ms) return "未知时间";
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return "未知时间";
  return date.toLocaleString();
}

/// 把主目录前缀缩成 `~`。
export function pretty(home, path) {
  return home && path.startsWith(`${home}/`) ? `~${path.slice(home.length)}` : path;
}
