// 更新与关于（来源 `smelt.updater`）：设置「系统 → 关于」页、应用/帮助菜单与检查更新命令。
//
// 下载、校验、替换 App 包与退出守卫由宿主无界面生命周期提供（`smelt.app/update-*`），
// 本插件只负责界面呈现、命令/菜单登记与角标同步。

import { computed, Scroll, signal, Text, View } from "nsh";
import { Button as BaseButton, Switch } from "nsh/base";
import {
  bootTask,
  initHost,
  localOs,
  onCommand,
  onEvent,
  openUrl,
  registerCommands,
  registerMenuItems,
  registerViews,
  revealPath,
  run,
  SmeltButton,
  smeltVersion,
  stableMemo,
  state as hostState,
  t,
  text,
} from "smelt/sdk";

import {
  channelLabel,
  checkButtonLabel,
  formatVersionHeader,
  progressModel,
  statusPresentation,
} from "./model.js";

const ABOUT_NAME = "about";
const ABOUT_VIEW = "smelt.updater/about";
const UPDATE_STATE_EVENT = "smelt.app/update-state-changed";

const DEFAULT_DOCS_URL = "https://smelt.onoo.io/";
const DEFAULT_FEEDBACK_URL = "https://github.com/smelt-ai/smelt/issues/new/choose";
const DEFAULT_SOURCE_URL = "https://github.com/smelt-ai/smelt";

const KEYWORDS = [
  "软件版本",
  "应用版本",
  "版本",
  "更新",
  "检查更新",
  "更新通道",
  "自动下载安装",
  "升级",
  "下载",
  "通道",
  "生产版",
  "内测版",
  "自动更新",
  "帮助",
  "帮助文档",
  "文档",
  "反馈",
  "反馈问题",
  "日志",
  "源码",
  "GitHub",
  "about",
  "version",
  "update",
  "check for updates",
  "update channel",
  "beta",
  "auto update",
  "download",
  "help",
  "docs",
  "feedback",
  "report issue",
  "log",
  "source code",
];

interface UpdateSnapshot {
  supported: boolean;
  version: string;
  channel: "prod" | "dev";
  auto_install: boolean;
  status: {
    kind: string;
    version?: string;
    url?: string;
    received?: number;
    total?: number | null;
    failure?: string;
  };
  can_check: boolean;
  can_request_check: boolean;
  ready: boolean;
  update_available: boolean;
  moving_app: boolean;
  move_app_error: string | null;
  manual_download_url: string | null;
  docs_url: string;
  feedback_url: string;
  source_url: string;
  app_log_path: string | null;
}

const DEFAULT_SNAPSHOT: UpdateSnapshot = {
  supported: localOs() === "darwin" || localOs() === "win32",
  version: smeltVersion(),
  channel: "prod",
  auto_install: true,
  status: { kind: "idle" },
  can_check: true,
  can_request_check: true,
  ready: false,
  update_available: false,
  moving_app: false,
  move_app_error: null,
  manual_download_url: null,
  docs_url: DEFAULT_DOCS_URL,
  feedback_url: DEFAULT_FEEDBACK_URL,
  source_url: DEFAULT_SOURCE_URL,
  app_log_path: null,
};

const snapshot = signal<UpdateSnapshot>(DEFAULT_SNAPSHOT);
let lastBadgeLabel: string | null | undefined = undefined;

function syncBadge(snap: UpdateSnapshot) {
  const nextLabel = snap.supported && snap.update_available ? t("badge") : null;
  if (nextLabel === lastBadgeLabel) return;
  lastBadgeLabel = nextLabel;
  void run("smelt.app/set-view-badge", { view: ABOUT_VIEW, label: nextLabel }).catch(() => {});
}

function applySnapshot(raw: any) {
  if (!raw || typeof raw !== "object") return;
  const next: UpdateSnapshot = {
    ...DEFAULT_SNAPSHOT,
    ...raw,
    status: raw.status && typeof raw.status === "object" ? raw.status : { kind: "idle" },
  };
  snapshot.set(next);
  syncBadge(next);
}

async function fetchSnapshot() {
  try {
    const raw = await run("smelt.app/update-state", null);
    applySnapshot(raw);
  } catch (error) {
    console.error("[smelt.updater] fetch update-state failed:", error);
  }
}

onEvent((event) => {
  if (event?.type === "plugin" && event?.name === UPDATE_STATE_EVENT) {
    applySnapshot(event.payload);
  } else if (event?.type === "locale_changed") {
    lastBadgeLabel = undefined;
    syncBadge(snapshot());
  }
});

onCommand(async ({ name }) => {
  const snap = snapshot();
  if (name === "check-update") {
    void run("smelt.app/update-check", null).catch(() => {});
    await run("smelt.app/open-view", { view: ABOUT_VIEW, args: null });
    return null;
  }
  if (name === "docs") {
    await openUrl(snap.docs_url || DEFAULT_DOCS_URL);
    return null;
  }
  if (name === "report-issue") {
    await openUrl(snap.feedback_url || DEFAULT_FEEDBACK_URL);
    return null;
  }
  return null;
});

bootTask(async () => {
  await initHost();
  await fetchSnapshot();
  const snap = snapshot();

  await registerViews([
    {
      name: ABOUT_NAME,
      export: ABOUT_NAME,
      title: text("about.title"),
      icon: "icons/info.svg",
      draws_top_bar: false,
      instances: false,
      settings: {
        group: "system",
        keywords: KEYWORDS,
      },
    },
  ]);

  const commands: any[] = [];
  if (snap.supported) {
    commands.push({
      name: "check-update",
      title: text("check.idle"),
      palette: true,
    });
  }
  commands.push(
    {
      name: "docs",
      title: text("docs"),
      palette: true,
    },
    {
      name: "report-issue",
      title: text("report_issue"),
      palette: true,
    },
  );
  await registerCommands(commands, text("group"));

  const menuItems: any[] = [];
  if (localOs() === "darwin") {
    menuItems.push({
      slot: "app",
      title: text("check_menu"),
      command: "smelt.updater/check-update",
    });
  }
  menuItems.push(
    {
      slot: "help",
      title: text("docs"),
      command: "smelt.updater/docs",
    },
    {
      slot: "help",
      title: text("report_issue"),
      command: "smelt.updater/report-issue",
    },
  );
  await registerMenuItems(menuItems);
  syncBadge(snapshot());
});

const currentTheme = stableMemo(() => hostState().theme);

function SectionCard(props: { title?: string; children?: any }) {
  const look = currentTheme;
  return (
    <View style={{ width: "100%", display: "flex", flexDirection: "column", gap: 8 }}>
      <If when={Boolean(props.title)}>
        <Text
          style={{
            fontSize: look().type.small_medium.size,
            lineHeight: look().type.small_medium.line_height,
            fontWeight: look().type.small_medium.weight,
            color: look().palette.muted,
          }}
        >
          {props.title ?? ""}
        </Text>
      </If>
      <View
        style={{
          width: "100%",
          borderRadius: look().radius.lg ?? 8,
          backgroundColor: look().palette.surface,
          borderWidth: 1,
          borderColor: look().palette.hair,
          overflow: "hidden",
          display: "flex",
          flexDirection: "column",
        }}
      >
        {props.children}
      </View>
    </View>
  );
}

function SettingsRow(props: {
  label: string;
  description?: string | null;
  last?: boolean;
  children?: any;
}) {
  const look = currentTheme;
  return (
    <View
      style={{
        paddingLeft: 14,
        paddingRight: 14,
        paddingTop: 12,
        paddingBottom: 12,
        borderBottomWidth: props.last ? 0 : 1,
        borderBottomColor: look().palette.hair,
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        justifyContent: "space-between",
        gap: 12,
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
          {props.label}
        </Text>
        <If when={Boolean(props.description)}>
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: look().palette.muted,
            }}
          >
            {props.description ?? ""}
          </Text>
        </If>
      </View>
      <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8, flexShrink: 0 }}>
        {props.children}
      </View>
    </View>
  );
}

function TextLink(props: { label: string; url: string }) {
  const look = currentTheme;
  return (
    <BaseButton
      accessibilityLabel={props.label}
      onPress={() => {
        void openUrl(props.url).catch(() => {});
      }}
      style={{
        paddingLeft: 4,
        paddingRight: 4,
        paddingTop: 2,
        paddingBottom: 2,
        borderRadius: look().radius.sm ?? 4,
        backgroundColor: "transparent",
        color: look().palette.text,
        cursor: "pointer",
      }}
      hoverStyle={{
        backgroundColor: look().palette.row_hover,
      }}
      activeStyle={{
        backgroundColor: look().palette.row_selected,
      }}
    >
      <Text
        style={{
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
          color: look().palette.text,
          textDecorationLine: "underline",
        }}
      >
        {props.label}
      </Text>
    </BaseButton>
  );
}

function ChannelSegmented(props: {
  channel: () => "prod" | "dev";
  disabled: () => boolean;
  onSelect: (channel: "prod" | "dev") => void;
}) {
  const look = currentTheme;
  const options: Array<"prod" | "dev"> = ["prod", "dev"];
  return (
    <View
      style={{
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        padding: 2,
        borderRadius: look().radius.md ?? 6,
        backgroundColor: look().palette.soft,
        borderWidth: 1,
        borderColor: look().palette.hair,
        opacity: props.disabled() ? 0.45 : 1,
      }}
    >
      {options.map((opt) => {
        const selected = () => props.channel() === opt;
        return (
          <BaseButton
            disabled={props.disabled()}
            selected={selected()}
            onPress={() => {
              if (!props.disabled() && !selected()) props.onSelect(opt);
            }}
            style={{
              height: 24,
              paddingLeft: 10,
              paddingRight: 10,
              borderRadius: look().radius.sm ?? 4,
              backgroundColor: selected() ? look().palette.bg : "transparent",
              borderWidth: selected() ? 1 : 0,
              borderColor: selected() ? look().palette.hair_strong : "transparent",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
            }}
            hoverStyle={{
              backgroundColor: selected() ? look().palette.bg : look().palette.row_hover,
            }}
          >
            <Text
              style={{
                fontSize: look().type.small_medium.size,
                lineHeight: look().type.small_medium.line_height,
                fontWeight: selected()
                  ? look().type.small_medium.weight
                  : look().type.small.weight,
                color: selected() ? look().palette.text : look().palette.muted,
              }}
            >
              {channelLabel(opt, t)}
            </Text>
          </BaseButton>
        );
      })}
    </View>
  );
}

export function about() {
  const look = currentTheme;
  const snap = computed(() => snapshot());
  const versionText = computed(() =>
    formatVersionHeader(snap().version, snap().supported, snap().channel, t),
  );
  const presentation = computed(() => statusPresentation(snap(), t));
  const toneColor = computed(() => {
    const p = look().palette;
    switch (presentation().tone) {
      case "run":
        return p.run;
      case "ok":
        return p.ok;
      case "bad":
        return p.bad;
      default:
        return p.muted;
    }
  });
  const progress = computed(() => progressModel(snap().status));
  const checkLabel = computed(() => checkButtonLabel(snap().status, t));

  const handleCheck = () => {
    void run("smelt.app/update-check", null).then(applySnapshot).catch(() => {});
  };
  const handleDownload = () => {
    const st = snap().status;
    if (st.kind !== "available" || !st.version || !st.url) return;
    void run("smelt.app/update-download", { version: st.version, url: st.url }).catch(() => {});
  };
  const handleInstall = () => {
    void run("smelt.app/update-install", null).catch(() => {});
  };
  const handleMoveApp = () => {
    void run("smelt.app/update-move-app", null).catch(() => {});
  };
  const handleChannel = (channel: "prod" | "dev") => {
    void run("smelt.app/update-set-channel", { channel }).catch(() => {});
  };
  const handleAutoInstall = (autoInstall: boolean) => {
    void run("smelt.app/update-set-auto-install", { auto_install: autoInstall }).catch(() => {});
  };
  const handleOpenLog = () => {
    const path = snap().app_log_path;
    if (path) {
      void revealPath(path).catch(() => {});
    }
  };

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
          {/* 页头：名称与版本 */}
          <SectionCard>
            <View
              style={{
                paddingLeft: 14,
                paddingRight: 14,
                paddingTop: 14,
                paddingBottom: 14,
                display: "flex",
                flexDirection: "column",
                gap: 4,
              }}
            >
              <Text
                style={{
                  fontSize: 15,
                  lineHeight: 20,
                  fontWeight: 600,
                  color: look().palette.text,
                }}
              >
                Smelt
              </Text>
              <Text
                style={{
                  fontSize: look().type.ui.size,
                  lineHeight: look().type.ui.line_height,
                  color: look().palette.muted,
                }}
              >
                {versionText()}
              </Text>
            </View>
          </SectionCard>

          {/* 在线更新（macOS / Windows） */}
          <If when={snap().supported}>
            <SectionCard title={t("section.update")}>
              {/* 状态单元格 */}
              <View
                style={{
                  paddingLeft: 14,
                  paddingRight: 14,
                  paddingTop: 12,
                  paddingBottom: 12,
                  borderBottomWidth: 1,
                  borderBottomColor: look().palette.hair,
                  display: "flex",
                  flexDirection: "column",
                  gap: 6,
                }}
              >
                <View
                  style={{
                    width: "100%",
                    display: "flex",
                    flexDirection: "row",
                    alignItems: "center",
                    justifyContent: "space-between",
                    gap: 8,
                    flexWrap: "wrap",
                  }}
                >
                  <View
                    style={{
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 8,
                      flex: 1,
                      minWidth: 0,
                    }}
                  >
                    <View
                      style={{
                        width: 7,
                        height: 7,
                        borderRadius: 4,
                        backgroundColor: toneColor(),
                        flexShrink: 0,
                      }}
                    />
                    <Text
                      style={{
                        fontSize: look().type.ui_medium.size,
                        lineHeight: look().type.ui_medium.line_height,
                        fontWeight: look().type.ui_medium.weight,
                        color: toneColor(),
                        numberOfLines: 1,
                      }}
                    >
                      {presentation().title}
                    </Text>
                  </View>

                  <View
                    style={{
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 8,
                      flexShrink: 0,
                    }}
                  >
                    <SmeltButton
                      small={true}
                      label={checkLabel()}
                      disabled={!snap().can_request_check}
                      onPress={handleCheck}
                    />
                    <If
                      when={
                        localOs() === "darwin" &&
                        snap().status.kind === "failed" &&
                        snap().status.failure === "location"
                      }
                    >
                      <SmeltButton
                        primary={true}
                        small={true}
                        label={snap().moving_app ? t("move.moving") : t("action.move")}
                        disabled={snap().moving_app}
                        onPress={handleMoveApp}
                      />
                    </If>
                    <If when={snap().status.kind === "available"}>
                      <SmeltButton
                        primary={true}
                        small={true}
                        label={t("action.download")}
                        onPress={handleDownload}
                      />
                    </If>
                    <If when={snap().ready}>
                      <SmeltButton
                        primary={true}
                        small={true}
                        label={
                          snap().status.kind === "install_failed"
                            ? t("action.retry_restart")
                            : t("action.restart")
                        }
                        onPress={handleInstall}
                      />
                    </If>
                    <If when={Boolean(snap().manual_download_url)}>
                      <SmeltButton
                        small={true}
                        label={t("action.manual_download")}
                        onPress={() => {
                          const url = snap().manual_download_url;
                          if (url) void openUrl(url).catch(() => {});
                        }}
                      />
                    </If>
                  </View>
                </View>

                <Text
                  style={{
                    paddingLeft: 15,
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                    color: look().palette.muted,
                  }}
                >
                  {presentation().detail}
                </Text>

                <If when={progress() != null}>
                  <View
                    style={{
                      width: "100%",
                      height: 4,
                      marginTop: 4,
                      borderRadius: 2,
                      backgroundColor: look().palette.soft,
                      overflow: "hidden",
                    }}
                  >
                    <View
                      style={{
                        height: 4,
                        borderRadius: 2,
                        backgroundColor: look().palette.run,
                        width:
                          progress()?.kind === "determinate"
                            ? `${Math.max(2, Math.round((progress() as any).percent))}%`
                            : "40%",
                      }}
                    />
                  </View>
                </If>
              </View>

              {/* 更新通道 */}
              <SettingsRow label={t("channel.label")} description={t("channel.description")}>
                <ChannelSegmented
                  channel={() => snap().channel}
                  disabled={() => !snap().can_check}
                  onSelect={handleChannel}
                />
              </SettingsRow>

              {/* 自动下载安装 */}
              <SettingsRow
                label={t("auto_install.label")}
                description={t("auto_install.description")}
                last={true}
              >
                <Switch
                  checked={snap().auto_install}
                  accessibilityLabel={t("auto_install.label")}
                  onChange={({ checked }: any) => handleAutoInstall(Boolean(checked))}
                  trackStyle={{
                    width: 34,
                    height: 20,
                    borderRadius: 10,
                    padding: 2,
                    backgroundColor: snap().auto_install
                      ? look().palette.link
                      : look().palette.soft,
                    display: "flex",
                    flexDirection: "row",
                    alignItems: "center",
                    justifyContent: snap().auto_install ? "flex-end" : "flex-start",
                  }}
                  thumbStyle={{
                    width: 16,
                    height: 16,
                    borderRadius: 8,
                    backgroundColor: look().palette.on_ink,
                  }}
                />
              </SettingsRow>
            </SectionCard>
          </If>

          {/* 帮助与反馈 */}
          <SectionCard title={t("section.help")}>
            <SettingsRow label={t("help.docs_row")}>
              <TextLink label={`${t("docs")} ↗`} url={snap().docs_url || DEFAULT_DOCS_URL} />
            </SettingsRow>
            <SettingsRow label={t("report_issue")} description={t("help.report_description")}>
              <If when={Boolean(snap().app_log_path)}>
                <SmeltButton
                  quiet={true}
                  small={true}
                  label={t("help.open_log")}
                  onPress={handleOpenLog}
                />
              </If>
              <TextLink
                label={`${t("report_issue")} ↗`}
                url={snap().feedback_url || DEFAULT_FEEDBACK_URL}
              />
            </SettingsRow>
            <SettingsRow label={t("help.source")} last={true}>
              <TextLink label="GitHub ↗" url={snap().source_url || DEFAULT_SOURCE_URL} />
            </SettingsRow>
          </SectionCard>
        </View>
      </View>
    </Scroll>
  );
}

export default about;
