// 维护（来源 `smelt.maintenance`）：设置「系统 → 清理残留」与（macOS）「系统 → 命令行」。
//
// 文件系统扫描/删除与 `/usr/local/bin/smelt` 软链提权安装走宿主受控命令
// （`smelt.app/storage-residue-*`、`smelt.app/cli-link-*`），本插件只管界面与交互。

import { computed, Scroll, signal, Text, View } from "nsh";
import { Dialog } from "nsh/base";
import {
  bootTask,
  initHost,
  localOs,
  onEvent,
  registerViews,
  run,
  SmeltButton,
  stableMemo,
  state as hostState,
  t,
  currentTheme,
  text,
  viewShown,
} from "smelt/sdk";

import {
  cleanupSummary,
  cliActionLabel,
  cliInstallErrorReason,
  cliStatusDescription,
  formatBytes,
  totalResidueBytes,
} from "./model.js";

const CLEANUP_NAME = "cleanup";
const CLEANUP_VIEW = "smelt.maintenance/cleanup";
const CLI_NAME = "cli";
const CLI_VIEW = "smelt.maintenance/cli";

const CLEANUP_KEYWORDS = [
  "清理",
  "缓存",
  "残留",
  "磁盘",
  "空间",
  "~/.smelt",
  "clean",
  "cache",
  "disk",
  "storage",
];

const CLI_KEYWORDS = ["smelt 命令", "CLI", "终端命令", "command line", "PATH", "/usr/local/bin"];

const CONFIRM_WIDTH = 440;

interface ResidueItem {
  path: string;
  label: string;
  bytes: number;
}

type ScanState =
  | { kind: "running" }
  | { kind: "done"; items: ResidueItem[] }
  | { kind: "failed"; error: string };

const scanState = signal<ScanState>({ kind: "running" });
const cleaning = signal(false);
const failures = signal<Record<string, string>>({});
const confirmOpen = signal(false);

const cliLink = signal("/usr/local/bin/smelt");
const cliStatus = signal<string>("unknown");
const cliInstalling = signal(false);

let cleanupWasShown = false;
let cliWasShown = false;

function errText(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error ?? "");
}

async function rescanResidue() {
  if (cleaning()) return;
  scanState.set({ kind: "running" });
  try {
    const items = await run("smelt.app/storage-residue-scan", null);
    scanState.set({ kind: "done", items: Array.isArray(items) ? items : [] });
  } catch (error) {
    scanState.set({ kind: "failed", error: errText(error) });
  }
}

async function performClean() {
  const cur = scanState();
  if (cur.kind !== "done" || cur.items.length === 0 || cleaning()) return;
  confirmOpen.set(false);
  cleaning.set(true);
  failures.set({});
  const paths = cur.items.map((item) => item.path);
  try {
    const res = await run("smelt.app/storage-residue-remove", { paths });
    const nextFailures: Record<string, string> = {};
    for (const f of res?.failures ?? []) {
      if (f?.path) nextFailures[String(f.path)] = String(f.error ?? "");
    }
    failures.set(nextFailures);
    if (res?.scan_error) {
      scanState.set({ kind: "failed", error: String(res.scan_error) });
    } else {
      scanState.set({ kind: "done", items: Array.isArray(res?.items) ? res.items : [] });
    }
  } catch (error) {
    scanState.set({ kind: "failed", error: errText(error) });
  } finally {
    cleaning.set(false);
  }
}

async function checkCliStatus() {
  if (localOs() !== "darwin") return;
  try {
    const res = await run("smelt.app/cli-link-status", null);
    if (res?.link) cliLink.set(String(res.link));
    if (res?.status) cliStatus.set(String(res.status));
  } catch (error) {
    console.error("[smelt.maintenance] check cli status failed:", errText(error));
  }
}

async function installCliLink() {
  if (cliInstalling() || localOs() !== "darwin") return;
  cliInstalling.set(true);
  try {
    const res = await run("smelt.app/cli-link-install", null);
    if (res?.link) cliLink.set(String(res.link));
    if (res?.status) cliStatus.set(String(res.status));
    if (res?.ok) {
      void run("smelt.app/notify", {
        subtitle: t("notify.done"),
        body: t("cli.install.done"),
      }).catch(() => {});
    } else {
      const reason = cliInstallErrorReason(res, t);
      void run("smelt.app/notify", {
        subtitle: t("notify.error"),
        body: t("cli.install.failed", { error: reason }),
      }).catch(() => {});
    }
  } catch (error) {
    void run("smelt.app/notify", {
      subtitle: t("notify.error"),
      body: t("cli.install.failed", { error: errText(error) }),
    }).catch(() => {});
  } finally {
    cliInstalling.set(false);
  }
}

function syncVisibility() {
  const showCleanup = viewShown(CLEANUP_VIEW);
  if (showCleanup && !cleanupWasShown) {
    void rescanResidue();
  }
  cleanupWasShown = showCleanup;

  if (localOs() === "darwin") {
    const showCli = viewShown(CLI_VIEW);
    if (showCli && !cliWasShown) {
      void checkCliStatus();
    }
    cliWasShown = showCli;
  }
}

onEvent((event) => {
  if (event?.type === "shown_views_changed") {
    syncVisibility();
  }
});

bootTask(async () => {
  await initHost();
  const views: any[] = [
    {
      name: CLEANUP_NAME,
      export: CLEANUP_NAME,
      title: text("cleanup.title"),
      icon: "icons/trash.svg",
      draws_top_bar: false,
      instances: false,
      settings: {
        group: "system",
        keywords: CLEANUP_KEYWORDS,
      },
    },
  ];
  if (localOs() === "darwin") {
    views.push({
      name: CLI_NAME,
      export: CLI_NAME,
      title: text("cli.title"),
      icon: "icons/terminal.svg",
      draws_top_bar: false,
      instances: false,
      settings: {
        group: "system",
        keywords: CLI_KEYWORDS,
      },
    });
  }
  await registerViews(views);
  syncVisibility();
});


function PageShell(props: { children?: any }) {
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
          {props.children}
        </View>
      </View>
    </Scroll>
  );
}

function SectionCard(props: { children?: any }) {
  const look = currentTheme;
  return (
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
  );
}

export function cleanup() {
  const look = currentTheme;
  const items = computed<ResidueItem[]>(() => {
    const cur = scanState();
    return cur.kind === "done" ? cur.items : [];
  });
  const summary = computed(() => cleanupSummary(scanState(), t));
  const totalBytes = computed(() => totalResidueBytes(items()));
  const disabled = computed(() => cleaning() || items().length === 0);

  return (
    <PageShell>
      <SectionCard>
        <View
          style={{
            paddingLeft: 14,
            paddingRight: 14,
            paddingTop: 12,
            paddingBottom: 12,
            borderBottomWidth: items().length > 0 ? 1 : 0,
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
              {t("cleanup.row")}
            </Text>
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: look().palette.muted,
              }}
            >
              {summary()}
            </Text>
          </View>
          <SmeltButton
            small={true}
            label={t("cleanup.action")}
            loading={cleaning()}
            disabled={disabled()}
            onPress={() => {
              if (!disabled()) confirmOpen.set(true);
            }}
          />
        </View>

        <Each of={items()} key={(item: ResidueItem) => item.path}>
          {(item: () => ResidueItem, ix: () => number) => {
            const failMsg = () => failures()[item().path] ?? null;
            const isLast = () => ix() === items().length - 1;
            return (
              <View
                style={{
                  paddingLeft: 14,
                  paddingRight: 14,
                  paddingTop: 10,
                  paddingBottom: 10,
                  borderBottomWidth: isLast() ? 0 : 1,
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
                      numberOfLines: 1,
                    }}
                  >
                    {item().label}
                  </Text>
                  <If when={Boolean(failMsg())}>
                    <Text
                      style={{
                        fontSize: look().type.text_xsmall.size,
                        lineHeight: look().type.text_xsmall.line_height,
                        color: look().palette.bad,
                      }}
                    >
                      {t("cleanup.remove_failed", { error: failMsg() ?? "" })}
                    </Text>
                  </If>
                </View>
                <Text
                  style={{
                    fontSize: look().type.ui.size,
                    lineHeight: look().type.ui.line_height,
                    color: look().palette.muted,
                    flexShrink: 0,
                    fontVariant: ["tabular-nums"],
                  }}
                >
                  {formatBytes(item().bytes)}
                </Text>
              </View>
            );
          }}
        </Each>
      </SectionCard>

      <Dialog
        open={confirmOpen()}
        role="alertdialog"
        animation="fade"
        backdropClass="bg-black/40"
        onOpenChange={({ open }: any) => {
          if (!open) confirmOpen.set(false);
        }}
        class="m-auto"
      >
        <View
          style={{
            width: CONFIRM_WIDTH,
            padding: 16,
            borderRadius: look().radius.md ?? 6,
            backgroundColor: look().palette.surface,
            borderWidth: 1,
            borderColor: look().palette.hair_strong,
            display: "flex",
            flexDirection: "column",
            gap: 12,
          }}
        >
          <Text
            style={{
              fontSize: 15,
              lineHeight: 20,
              fontWeight: 530,
              color: look().palette.text,
            }}
          >
            {t("cleanup.confirm.title", { count: items().length })}
          </Text>
          <Text
            style={{
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
              color: look().palette.muted,
            }}
          >
            {t("cleanup.confirm.description", { size: formatBytes(totalBytes()) })}
          </Text>
          <View
            style={{
              display: "flex",
              flexDirection: "row",
              justifyContent: "flex-end",
              alignItems: "center",
              gap: 8,
              paddingTop: 4,
            }}
          >
            <SmeltButton
              quiet={true}
              small={true}
              label={t("cleanup.confirm.cancel")}
              onPress={() => confirmOpen.set(false)}
            />
            <SmeltButton
              danger={true}
              small={true}
              label={t("cleanup.confirm.action")}
              onPress={() => {
                void performClean();
              }}
            />
          </View>
        </View>
      </Dialog>
    </PageShell>
  );
}

export function cli() {
  const look = currentTheme;
  const description = computed(() => cliStatusDescription(cliStatus(), cliLink(), t));
  const label = computed(() => cliActionLabel(cliStatus(), cliInstalling(), t));
  const disabled = computed(() => cliInstalling() || cliStatus() === "taken");

  return (
    <PageShell>
      <SectionCard>
        <View
          style={{
            paddingLeft: 14,
            paddingRight: 14,
            paddingTop: 12,
            paddingBottom: 12,
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
              {t("cli.row")}
            </Text>
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: look().palette.muted,
              }}
            >
              {description()}
            </Text>
          </View>
          <SmeltButton
            small={true}
            label={label()}
            loading={cliInstalling()}
            disabled={disabled()}
            onPress={() => {
              void installCliLink();
            }}
          />
        </View>
      </SectionCard>
    </PageShell>
  );
}

export default cleanup;
