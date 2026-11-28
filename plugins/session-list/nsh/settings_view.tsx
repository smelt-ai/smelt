// 设置「通用 → 会话列表」：行首显示会话头像或图标。

import { computed, Text, View } from "nsh";
import {
  MENU_ITEM_CLASS,
  SmeltIcon,
  SmeltPageScroll,
  SmeltSection,
  SmeltSectionCell,
  SmeltSelect,
  SmeltSettingRow,
} from "smelt/sdk";

import { show_avatar } from "../model.js";
import { icon } from "../util.js";
import { list, t, theme, tick } from "./bridge.ts";

const ANIMATED = "animated";
const ICON = "icon";

export function SessionListSettings() {
  const look = () => theme();
  const options = computed(() => {
    tick();
    return [
      { value: ANIMATED, title: t("settings.avatar_animated") },
      { value: ICON, title: t("settings.avatar_icon") },
    ];
  });

  const currentValue = computed(() => {
    tick();
    return show_avatar(list.avatar) ? ANIMATED : ICON;
  });

  const currentTitle = computed(() => {
    const val = currentValue();
    return options().find((item) => item.value === val)?.title ?? t("settings.avatar_icon");
  });

  return (
    <SmeltPageScroll>
      <SmeltSection title={t("settings.section_list")}>
        <SmeltSectionCell>
          <SmeltSettingRow
            title={t("settings.avatar")}
            description={t("settings.avatar_description")}
          >
            <SmeltSelect
              items={options()}
              value={currentValue()}
              accessibilityLabel={t("settings.avatar")}
              onChange={({ value }: { value: string | null }) => {
                if (!value) return;
                list.set_avatar(value === ICON ? { kind: "icon" } : { kind: "unset" });
              }}
              itemClass={`${MENU_ITEM_CLASS} py-1 text-sm text-foreground`}
              trigger={
                // 下拉列表和触发器一样宽，按最长的选项（「不要头像（显示图标）」）定宽，免得列表里被截断。
                <View
                  style={{
                    minWidth: 180,
                    height: 28,
                    paddingHorizontal: 10,
                    borderRadius: look().radius.sm,
                    borderWidth: 1,
                    borderColor: look().palette.hair_strong,
                    backgroundColor: look().palette.soft,
                    flexDirection: "row",
                    alignItems: "center",
                    justifyContent: "space-between",
                    gap: 8,
                  }}
                >
                  <Text
                    style={{ fontSize: look().type.ui.size, color: look().palette.text, numberOfLines: 1 }}
                  >
                    {currentTitle()}
                  </Text>
                  <SmeltIcon path={icon("chevron-down")} size={12} color={look().palette.muted} />
                </View>
              }
            />
          </SmeltSettingRow>
        </SmeltSectionCell>
      </SmeltSection>
    </SmeltPageScroll>
  );
}
