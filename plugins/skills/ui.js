// 技能插件各视图共用的小部件。控件都是 smelt 自己的，从 `"smelt-ui"` 导入；布局用
// gpui-base 的 `v_flex` / `h_flex`。
//
// 视觉规格跟 smelt 自己的面板走：字号只用跟随界面缩放的 `text_xs` / `text_sm`，不写
// 像素；按钮用紧凑档；不画页面底色（宿主铺实底），悬停、选中用 `accent`。

import { div } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { SmeltBadge, SmeltButton, SmeltConfirmButton } from "smelt-ui";

export const SPACE = { xs: 4, sm: 8, md: 12, lg: 16 };
export const MONO = "SF Mono, Menlo, monospace";
/** 回收目录，相对主目录。 */
export const TRASH = ".smelt/trash/skills";

/// 默认安静角色、小号。回调要用它自己收到的 cx：render 的 cx 只在那一次 render 里有效。
/** @param {"quiet" | "secondary" | "primary" | "danger"} [variant] */
export function button(id, label, onClick, variant = "quiet") {
  const control = SmeltButton.new(id).label(label).small();
  return control[variant]().on_click((_event, cx) => onClick(cx));
}

/// 小标记。不给状态是中性灰；状态色只给状态：`ok` 正常、`need` 要你处理、`bad` 出错。
/** @param {"ok" | "need" | "bad"} [tone] */
export function tag(text, tone) {
  const badge = SmeltBadge.new();
  return (tone === undefined ? badge : badge[tone]()).flex_none().child(text);
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

/// 要先确认的操作：一个安静的小按钮，点了在窗口中间问一句，确认才 `onConfirm(cx)`。
/// `action` 是确认按钮上的动词（「删除」），不写「确定」。只给能找回的操作用：Enter 就是确认。
export function confirm(id, label, title, description, action, onConfirm) {
  return SmeltConfirmButton.new(id)
    .label(label)
    .small()
    .quiet()
    .title(title)
    .description(description)
    .action(action)
    .on_confirm((cx) => onConfirm(cx));
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
