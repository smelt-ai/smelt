// 设置「通用 → 语言」：选界面语言（跟随系统或指定语言），存在本插件自己的 SQLite 库里。

import { computed, Scroll, signal, Text, View } from "nsh";
import { Select } from "nsh/base";
import {
  bootTask,
  initHost,
  locales,
  registerViews,
  setLocale,
  SmeltIcon,
  state,
  store,
  systemLocale,
  t,
  text,
  theme,
} from "smelt/sdk";

import {
  buildOptions,
  choiceToSelectValue,
  MIGRATIONS,
  normalizeChoice,
  selectValueToChoice,
} from "./model.js";

const KEYWORDS = ["language", "语言", "locale", "界面语言", "English", "中文"];

const choice = signal<string | null>(null);
let storeReady = false;

async function persistChoice(next: string | null) {
  if (!storeReady) return;
  try {
    await store.execute(
      `INSERT INTO settings (id, locale) VALUES (1, ?1)
       ON CONFLICT (id) DO UPDATE SET locale = excluded.locale`,
      [next],
    );
  } catch (error) {
    console.error("[smelt.language] 保存语言选择失败：", error);
  }
}

function pickChoice(next: string | null) {
  if (choice() === next) return;
  choice.set(next);
  void persistChoice(next);
  void setLocale(next);
}

bootTask(async () => {
  await initHost();
  try {
    await store.migrate(MIGRATIONS);
    storeReady = true;
    const rows = await store.query("SELECT locale FROM settings WHERE id = 1");
    const saved = rows?.[0]?.locale ?? null;
    const normalized = normalizeChoice(saved, locales());
    choice.set(normalized);
    if (normalized !== null) {
      await setLocale(normalized);
    }
  } catch (error) {
    console.error("[smelt.language] 读取语言库失败，跟随系统：", error);
  }
  await registerViews([
    {
      name: "settings",
      title: text("page.title"),
      icon: "icons/languages.svg",
      draws_top_bar: false,
      instances: false,
      settings: {
        group: "general",
        keywords: KEYWORDS,
      },
    },
  ]);
});

export function settings() {
  const look = theme;
  const options = computed(() => buildOptions(locales(), systemLocale(), t));
  const currentValue = computed(() => choiceToSelectValue(choice()));
  const currentTitle = computed(() => {
    const val = currentValue();
    return options().find((item) => item.value === val)?.title ?? "";
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
          {t("section.interface")}
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
              {t("row.language")}
            </Text>
            <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
              {t("row.language_description")}
            </Text>
          </View>

          <Select
            items={options()}
            value={currentValue()}
            accessibilityLabel={t("row.language")}
            onChange={({ value }: { value: string | null }) => {
              pickChoice(selectValueToChoice(value, locales()));
            }}
            popupClass="rounded-(--radius) border border-border bg-popover p-1"
            itemClass="flex-row items-center rounded-(--radius) px-2 py-1 text-sm hover:bg-accent focus:bg-accent"
            trigger={
              <View
                style={{
                  minWidth: 160,
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
                  style={{
                    fontSize: look().type.ui.size,
                    color: look().palette.text,
                    numberOfLines: 1,
                  }}
                >
                  {currentTitle()}
                </Text>
                <SmeltIcon path="icons/chevron-down.svg" size={12} color={look().palette.muted} />
              </View>
            }
          />
        </View>
      </View>
    </Scroll>
  );
}
