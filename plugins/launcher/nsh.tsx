// 启动栏（NSH A2 独立 VM 版本）：复用纯逻辑 model.js 与 contract.js。
import { computed, Text, View } from "nsh";
import {
  command,
  initHost,
  pages,
  registerViews,
  run,
  safeArea,
  SmeltButton,
  SmeltIcon,
  SmeltWindowBar,
  t,
  text,
  theme,
  viewBadge,
  viewLaidOut,
  views,
  viewShown,
} from "smelt/sdk";

import { NAV, NAV_NAME, SETTINGS_NAME, SETTINGS_VIEW } from "./contract.js";
import { NEW_CONVERSATION, SETTINGS, nav_state, settings_state } from "./model.js";

const ROW_MD = 32;

const hostFacade = {
  pages,
  views,
  command,
  view_badge: viewBadge,
  view_laid_out: viewLaidOut,
  view_shown: viewShown,
};

function notifyError(error: any) {
  const msg = error?.message ?? String(error);
  console.error(`[launcher] ${msg}`);
  run("smelt.app/notify", { subtitle: t("notify.error"), body: msg }).catch(() => {});
}

function openView(view: string) {
  run("smelt.app/open-view", { view }).catch(notifyError);
}

function runCmd(cmd: string) {
  run(cmd).catch(notifyError);
}

initHost().then(() => {
  registerViews([
    {
      name: NAV_NAME,
      export: NAV_NAME,
      title: text("view.nav"),
      icon: "icons/layout-dashboard.svg",
      draws_top_bar: true,
    },
    {
      name: SETTINGS_NAME,
      export: SETTINGS_NAME,
      title: text("settings"),
      icon: "icons/settings.svg",
      draws_top_bar: true,
    },
  ]).catch((error) => console.error("[launcher] registerViews 失败:", error));
});

export function nav() {
  const data = computed(() => nav_state(hostFacade));
  const look = theme;

  return (
    <View style={{ width: "100%", minWidth: 0, display: "flex", flexDirection: "column" }}>
      <SmeltWindowBar view={NAV} />
      <View
        style={{
          width: "100%",
          minWidth: 0,
          paddingLeft: 8,
          paddingRight: 8,
          paddingTop: 8,
          paddingBottom: 4,
          display: "flex",
          flexDirection: "column",
          gap: 4,
        }}
      >
        <If when={data().new_conversation != null}>
          <View style={{ width: "100%", minWidth: 0 }}>
            <SmeltButton
              fill={true}
              icon="icons/plus.svg"
              label={data().new_conversation ?? ""}
              tooltip={data().new_conversation ?? ""}
              command={NEW_CONVERSATION}
              onPress={() => runCmd(NEW_CONVERSATION)}
              style={{ width: "100%" }}
            />
          </View>
        </If>

        <Each of={data().entries} key={(entry: any) => entry.id}>
          {(entry: () => any) => {
            const item = () => entry();
            const p = () => look().palette;
            const uiType = () => (item().shown ? look().type.ui_medium : look().type.ui);
            return (
              <View
                class="hover:bg-accent"
                style={{
                  height: ROW_MD,
                  paddingLeft: 8,
                  paddingRight: 8,
                  borderRadius: look().radius.row,
                  backgroundColor: item().shown ? p().row_selected : "transparent",
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  gap: 8,
                }}
                onPress={() => openView(item().id)}
              >
                <SmeltIcon
                  path={item().icon}
                  size={16}
                  color={item().shown ? p().muted_selected : p().muted}
                />
                <Text
                  style={{
                    flex: 1,
                    minWidth: 0,
                    fontSize: uiType().size,
                    lineHeight: uiType().line_height,
                    fontWeight: uiType().weight,
                    color: p().text,
                    numberOfLines: 1,
                  }}
                >
                  {item().title}
                </Text>
                <If when={Boolean(item().attention)}>
                  <View
                    style={{
                      width: 6,
                      height: 6,
                      borderRadius: 3,
                      backgroundColor: p().bad,
                      flexShrink: 0,
                    }}
                  />
                </If>
              </View>
            );
          }}
        </Each>
      </View>
    </View>
  );
}

export function settings() {
  const data = computed(() => settings_state(hostFacade));
  const look = theme;
  const insets = computed(() => safeArea(SETTINGS_VIEW).bar_insets);

  return (
    <View
      style={{
        width: "100%",
        height: "100%",
        minWidth: 0,
        paddingLeft: insets().left,
        paddingRight: insets().right,
        paddingTop: 4,
        paddingBottom: 4,
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
      }}
    >
      <If when={Boolean(data().available)}>
        <View style={{ position: "relative", flexShrink: 0 }}>
          <SmeltButton
            quiet={true}
            icon="icons/settings.svg"
            label={t("settings")}
            onPress={() => runCmd(SETTINGS)}
          />
          <If when={Boolean(data().attention)}>
            <View
              style={{
                position: "absolute",
                top: 4,
                right: 4,
                width: 5,
                height: 5,
                borderRadius: 2.5,
                backgroundColor: look().palette.bad,
                pointerEvents: "none",
              }}
            />
          </If>
        </View>
      </If>
    </View>
  );
}

export default nav;
