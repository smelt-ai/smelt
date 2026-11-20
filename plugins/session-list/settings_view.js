// 设置「通用 → 会话列表」：会话列表的偏好。眼下只有行首头像用哪个。
//
// 行首头像是插槽 SESSION_AVATAR，只放一个。好几个插件都填时用哪个只有用户知道，这边不按登记顺序、
// 来源去猜，在这里让人选；没选过用第一个。选择改了、填插槽的插件来了走了，都经 host.redraw 重画。

import { View } from "gpui-kit";
import { slot_views, t } from "smelt-plugin-api";
import { SmeltSelect, SmeltSettingsPage, SmeltSettingsRow, SmeltSettingsSection } from "smelt-ui";

import { SESSION_AVATAR } from "./contract.js";
import { pick_avatar } from "./model.js";

/** 下拉里「不要头像」那项的值；其余是 "view:<视图 id>"。 */
const ICON = "icon";

/** list 是插件共用的 SessionList。 */
export function settings_view_class(list) {
  return class SessionsSettings extends View {
    render() {
      const filled = slot_views(SESSION_AVATAR);
      // 下拉里显示的是眼下实际用的那个：没选过、选的卸了时是第一个。
      const current = pick_avatar(
        list.avatar,
        filled.map((view) => view.id),
      );
      let select = SmeltSelect.new("session-avatar")
        .value(current == null ? ICON : `view:${current}`)
        .on_change((value) => list.set_avatar(value === ICON ? { kind: "icon" } : { kind: "view", id: value.slice(5) }));
      for (const view of filled) select = select.option(`view:${view.id}`, view.title);
      select = select.option(ICON, t("settings.avatar_icon"));
      return SmeltSettingsPage.new("sessions-settings").child(
        SmeltSettingsSection.new()
          .title(t("settings.section_list"))
          .child(SmeltSettingsRow.new(t("settings.avatar")).description(t("settings.avatar_description")).child(select)),
      );
    }
  };
}
