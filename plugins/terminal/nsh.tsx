// 终端设置与主题配色发布插件（来源 `smelt.terminal`）：
// - 终端字号设置页与 SQLite 存储（`~/.smelt/plugin-data/smelt.terminal/store.sqlite3`），修改后经
//   `setTerminalFontPx` 通知宿主刷新所有窗口内活体 `<SmeltNativeTerminal>`；
// - 监听主题变化与本机重连，向本机 smeltd 发布 `terminal_theme.publish` 配色快照。

import { Scroll, signal, Text, View } from "nsh";
import {
  bootTask,
  call,
  initHost,
  onEvent,
  onState,
  registerViews,
  setTerminalFontPx,
  SmeltButton,
  stableMemo,
  state as hostState,
  store,
  t,
  currentTheme,
  terminalThemeSnapshot,
  text,
} from "smelt/sdk";

import { DEFAULT_FONT_PX, MAX_FONT_PX, MIN_FONT_PX, SETTINGS_VIEW_NAME } from "./contract.js";
import { clampFontPx, createThemePublisher } from "./model.js";

const SETTINGS_KEYWORDS = ["terminal", "终端", "字号", "字体大小", "font size"];

const MIGRATIONS = [
  `CREATE TABLE settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    font_px INTEGER NOT NULL
  );`,
];

const fontPx = signal<number>(DEFAULT_FONT_PX);
let storeReady = false;

const themePublisher = createThemePublisher(async (snapshot: unknown) => {
  await call(null, "terminal_theme.publish", snapshot);
});

let lastThemeJson = "";

function publishTerminalTheme(force = false) {
  try {
    const snap = terminalThemeSnapshot();
    const json = JSON.stringify(snap);
    if (!force && json === lastThemeJson) return;
    lastThemeJson = json;
    themePublisher.enqueue(snap);
  } catch (error) {
    console.error("[smelt.terminal] terminalThemeSnapshot failed:", error);
  }
}

async function saveFontPx(next: number) {
  const clamped = clampFontPx(next);
  if (clamped === fontPx()) return;
  fontPx.set(clamped);
  void setTerminalFontPx(clamped).catch((error) => {
    console.error("[smelt.terminal] setTerminalFontPx failed:", error);
  });
  if (!storeReady) return;
  try {
    await store.execute(
      `INSERT INTO settings (id, font_px) VALUES (1, ?1)
       ON CONFLICT (id) DO UPDATE SET font_px = excluded.font_px`,
      [clamped],
    );
  } catch (error) {
    console.error("[smelt.terminal] save font_px failed:", error);
  }
}

onState(() => {
  publishTerminalTheme(false);
});

onEvent((event) => {
  if (event?.type === "themes_changed") {
    publishTerminalTheme(true);
  } else if (event?.type === "reconnected" && (event.machine ?? null) === null) {
    publishTerminalTheme(true);
  }
});

bootTask(async () => {
  await initHost();
  try {
    await store.migrate(MIGRATIONS);
    storeReady = true;
    const rows = await store.query("SELECT font_px FROM settings WHERE id = 1");
    if (Array.isArray(rows) && rows.length > 0 && rows[0]?.font_px != null) {
      const clamped = clampFontPx(rows[0].font_px);
      fontPx.set(clamped);
      await setTerminalFontPx(clamped);
    } else {
      await setTerminalFontPx(DEFAULT_FONT_PX);
    }
  } catch (error) {
    console.error("[smelt.terminal] open store failed:", error);
    await setTerminalFontPx(DEFAULT_FONT_PX).catch(() => {});
  }

  await registerViews([
    {
      name: SETTINGS_VIEW_NAME,
      export: SETTINGS_VIEW_NAME,
      title: text("title"),
      icon: "icons/terminal.svg",
      draws_top_bar: false,
      instances: false,
      settings: {
        group: "general",
        keywords: SETTINGS_KEYWORDS,
      },
    },
  ]);

  publishTerminalTheme(true);
});


export function settings() {
  const look = currentTheme;
  return (
    <Scroll style={{ flex: 1, width: "100%", height: "100%" }}>
      <View
        style={{
          width: "100%",
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
        }}
      >
        <View
          style={{
            maxWidth: 720,
            width: "100%",
            paddingLeft: 24,
            paddingRight: 24,
            paddingTop: 16,
            paddingBottom: 32,
            display: "flex",
            flexDirection: "column",
            gap: 24,
          }}
        >
          <View style={{ width: "100%", display: "flex", flexDirection: "column", gap: 8 }}>
            <Text
              style={{
                fontSize: look().type.small_medium.size,
                lineHeight: look().type.small_medium.line_height,
                fontWeight: look().type.small_medium.weight,
                color: look().palette.muted,
              }}
            >
              {t("settings.display")}
            </Text>
            <View
              style={{
                width: "100%",
                borderRadius: look().radius.lg ?? 8,
                backgroundColor: look().palette.surface,
                borderWidth: 1,
                borderColor: look().palette.hair,
                paddingLeft: 16,
                paddingRight: 16,
                paddingTop: 12,
                paddingBottom: 12,
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 16,
              }}
            >
              <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
                <Text
                  style={{
                    fontSize: look().type.ui.size,
                    lineHeight: look().type.ui.line_height,
                    color: look().palette.text,
                  }}
                >
                  {t("settings.font_size")}
                </Text>
                <Text
                  style={{
                    fontSize: look().type.small.size,
                    lineHeight: look().type.small.line_height,
                    color: look().palette.muted,
                  }}
                >
                  {`${MIN_FONT_PX}–${MAX_FONT_PX}px`}
                </Text>
              </View>
              <View
                style={{
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  gap: 6,
                  flexShrink: 0,
                }}
              >
                <SmeltButton
                  small={true}
                  label="−"
                  accessibilityLabel={t("settings.smaller")}
                  disabled={fontPx() <= MIN_FONT_PX}
                  onPress={() => {
                    void saveFontPx(fontPx() - 1);
                  }}
                />
                <View style={{ minWidth: 40, display: "flex", alignItems: "center" }}>
                  <Text
                    style={{
                      fontSize: look().type.ui.size,
                      lineHeight: look().type.ui.line_height,
                      color: look().palette.text,
                      fontVariant: ["tabular-nums"],
                    }}
                  >
                    {`${fontPx()}px`}
                  </Text>
                </View>
                <SmeltButton
                  small={true}
                  label="+"
                  accessibilityLabel={t("settings.larger")}
                  disabled={fontPx() >= MAX_FONT_PX}
                  onPress={() => {
                    void saveFontPx(fontPx() + 1);
                  }}
                />
              </View>
            </View>
          </View>
        </View>
      </View>
    </Scroll>
  );
}

export default settings;
