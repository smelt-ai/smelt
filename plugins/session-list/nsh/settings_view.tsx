// 设置「通用 → 会话列表」（NSH A2 版）：行首显示会话头像或图标。

import { computed, Scroll, Text, View } from "nsh";
import { MENU_ITEM_CLASS, SmeltIcon, SmeltSelect } from "smelt/sdk";

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
    <Scroll style={{ flex: 1, width: "100%", height: "100%" }}>
      <View
        style={{
          maxWidth: 680,
          width: "100%",
          padding: 24,
          display: "flex",
          flexDirection: "column",
          gap: 12,
        }}
      >
        <Text
          style={{
            fontSize: look().type.text_xsmall_medium.size,
            fontWeight: look().type.text_xsmall_medium.weight,
            color: look().palette.muted,
          }}
        >
          {t("settings.section_list")}
        </Text>
        <View
          style={{
            borderRadius: look().radius.md,
            borderWidth: 1,
            borderColor: look().palette.hair,
            backgroundColor: look().palette.surface,
            paddingHorizontal: 14,
            paddingVertical: 12,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 16,
          }}
        >
          <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
            <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
              {t("settings.avatar")}
            </Text>
            <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
              {t("settings.avatar_description")}
            </Text>
          </View>

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
                  display: "flex",
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
        </View>
      </View>
    </Scroll>
  );
}
