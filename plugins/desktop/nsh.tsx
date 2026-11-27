// 远程桌面（来源 `smelt.desktop`）：
// - 原生画面与本地转发口交给原子能力组件 `<SmeltNativeDesktop>`（`crates/smelt-desktop`）；
// - 机器探测、右键菜单贡献、工具栏、接管切换、退避重连与输入法文字顺序注入全在本 JS 插件里完成。

import { cleanup, computed, effect, signal, Text, View } from "nsh";
import {
  bootTask,
  call,
  clipboardWrite,
  initHost,
  machineMeta,
  machineMetaEntry,
  onCommand,
  onEvent,
  registerCommands,
  registerContextMenuItems,
  registerViews,
  run,
  SmeltButton,
  SmeltNativeDesktop,
  stableMemo,
  t,
  currentTheme,
  text,
  viewShown,
} from "smelt/sdk";

import { MACHINE_MENU, OPEN_COMMAND, OPEN_COMMAND_NAME, VIEW, VIEW_NAME } from "./contract.js";
import {
  buildMachineMenuItems,
  planMachineProbes,
  retryDelayMs,
  statusPresentation,
} from "./model.js";

const TYPE_TEXT_TIMEOUT_MS = 10_000;
const PROBE_TIMEOUT_MS = 5_000;

type ProbeEntry = { kind: "asking"; gen: number } | { kind: "known"; desktop: boolean };

const probes = new Map<string, ProbeEntry>();
let probeGen = 0;

function errText(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error ?? "");
}

function publishMenu() {
  const items = buildMachineMenuItems(probes, text("title"), OPEN_COMMAND);
  void registerContextMenuItems(MACHINE_MENU, items).catch((error) => {
    console.error("[smelt.desktop] registerContextMenuItems failed:", errText(error));
  });
}

function syncMachineProbes() {
  const { toAsk, removed } = planMachineProbes(machineMeta(), probes);
  if (removed) {
    publishMenu();
  }
  for (const id of toAsk) {
    const gen = ++probeGen;
    probes.set(id, { kind: "asking", gen });
    void call(id, "system.describe", {}, PROBE_TIMEOUT_MS)
      .then((info: any) => {
        const cur = probes.get(id);
        if (cur?.kind !== "asking" || cur.gen !== gen) return;
        const desktop = info?.daemon?.desktop === true;
        probes.set(id, { kind: "known", desktop });
        publishMenu();
      })
      .catch(() => {
        const cur = probes.get(id);
        if (cur?.kind === "asking" && cur.gen === gen) {
          probes.delete(id);
        }
      });
  }
}

onEvent((event) => {
  if (event?.type === "machines_changed" || event?.type === "reconnected") {
    syncMachineProbes();
  }
}, ["machines_changed", "reconnected"]);

onCommand(async ({ name, args }) => {
  if (name === OPEN_COMMAND_NAME) {
    const machine = args?.machine ?? null;
    const opened = await run("smelt.app/open-view", {
      view: VIEW,
      args: { machine },
    });
    if (opened === false) {
      throw new Error(t("error.no_place"));
    }
    return null;
  }
  return null;
});

bootTask(async () => {
  await initHost(["machines_meta", "views"]);
  await registerViews([
    {
      name: VIEW_NAME,
      export: VIEW_NAME,
      title: text("title"),
      icon: "icons/monitor.svg",
      draws_top_bar: false,
      instances: true,
    },
  ]);
  await registerCommands(
    [
      {
        name: OPEN_COMMAND_NAME,
        title: text("command.open"),
        requires_args: true,
      },
    ],
    text("title"),
  );
  syncMachineProbes();
});


type ClosedState =
  | { kind: "proxy"; error: string }
  | { kind: "webview"; error: string }
  | { kind: "by_user" }
  | { kind: "ended" }
  | { kind: "lost" };

type ConnectionStatus =
  | { kind: "connecting" }
  | { kind: "connected" }
  | { kind: "reconnecting" }
  | { kind: "closed"; closed: ClosedState };

function Placeholder(props: { message: string }) {
  const look = currentTheme;
  return (
    <View
      style={{
        flex: 1,
        width: "100%",
        height: "100%",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 24,
      }}
    >
      <Text
        style={{
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
          color: look().palette.muted,
        }}
      >
        {props.message}
      </Text>
    </View>
  );
}

export function desktop(props: { args?: { machine?: string | null } | null }) {
  const look = currentTheme;

  const hasMachineArg = () => props.args != null && typeof props.args === "object" && "machine" in props.args;
  const targetMachine = () => (hasMachineArg() ? (props.args?.machine ?? null) : undefined);
  const entry = computed(() => {
    const m = targetMachine();
    if (m === undefined) return null;
    return machineMetaEntry(m);
  });
  const machineOnline = computed(() => {
    const e = entry();
    return Boolean(e && e.unavailable == null);
  });
  const shown = computed(() => viewShown(VIEW));

  const status = signal<ConnectionStatus>({ kind: "connecting" });
  const controlling = signal(true);
  const typeError = signal<string | null>(null);
  const userDisconnected = signal(false);
  const reconnectSeq = signal(0);

  let retries = 0;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  let prevOnline = false;
  let prevKey = "";
  let typingChain: Promise<void> = Promise.resolve();
  let disposed = false;

  const clearRetryTimer = () => {
    if (retryTimer !== null) {
      clearTimeout(retryTimer);
      retryTimer = null;
    }
  };

  const resetForFreshConnect = () => {
    clearRetryTimer();
    retries = 0;
    userDisconnected.set(false);
    controlling.set(true);
    typeError.set(null);
    status.set({ kind: "connecting" });
  };

  // 切机器时重新连接；同一台机器重开时外壳会直接新建视图实例。
  effect(() => {
    const m = targetMachine();
    const key = m === undefined ? "__none__" : m === null ? "__local__" : `remote:${m}`;
    if (key !== prevKey) {
      prevKey = key;
      resetForFreshConnect();
    }
  });

  // 从隐藏切回可见时（除非用户点过断开）重新进入连接中状态。
  let prevShown = false;
  effect(() => {
    const vis = shown();
    if (vis && !prevShown && !userDisconnected()) {
      resetForFreshConnect();
    } else if (!vis && prevShown) {
      clearRetryTimer();
    }
    prevShown = vis;
  });

  // 机器从离线回到在线时，取消退避立刻重连。
  effect(() => {
    const online = machineOnline();
    const cameBack = online && !prevOnline;
    prevOnline = online;
    const st = status();
    if (
      cameBack &&
      shown() &&
      !userDisconnected() &&
      (st.kind === "reconnecting" || (st.kind === "closed" && st.closed.kind !== "by_user"))
    ) {
      clearRetryTimer();
      retries = 0;
      status.set({ kind: "reconnecting" });
      reconnectSeq.set(reconnectSeq() + 1);
    }
  });

  cleanup(() => {
    disposed = true;
    clearRetryTimer();
  });

  const canvasActive = computed(
    () => Boolean(entry()) && shown() && !userDisconnected(),
  );

  const handleConnected = () => {
    clearRetryTimer();
    retries = 0;
    status.set({ kind: "connected" });
  };

  const handleDisconnected = (event: { clean?: boolean }) => {
    if (userDisconnected() || !shown()) return;
    const delay = retryDelayMs(retries);
    if (delay == null) {
      clearRetryTimer();
      status.set({
        kind: "closed",
        closed: { kind: event?.clean ? "ended" : "lost" },
      });
      return;
    }
    retries += 1;
    status.set({ kind: "reconnecting" });
    clearRetryTimer();
    // 轮询例外：VNC 远端断开后没有推回「可重连」的事件，只能按 [1,2,4,8,16,30]s 指数退避重试，
    // 最多 6 次后停下，机器重新上线会提前唤醒。
    retryTimer = setTimeout(() => {
      retryTimer = null;
      if (!disposed && canvasActive()) {
        reconnectSeq.set(reconnectSeq() + 1);
      }
    }, delay);
  };

  const handleError = (event: { kind?: string; message?: string }) => {
    clearRetryTimer();
    const kind = event?.kind === "webview" ? "webview" : "proxy";
    status.set({
      kind: "closed",
      closed: { kind, error: String(event?.message ?? "") },
    });
  };

  const handleClipboard = (event: { text?: string }) => {
    if (typeof event?.text === "string") {
      void clipboardWrite(event.text).catch(() => {});
    }
  };

  const handleText = (event: { text?: string }) => {
    if (typeof event?.text !== "string") return;
    if (!controlling() || status().kind !== "connected") return;
    const m = targetMachine();
    if (m === undefined) return;
    const chunk = event.text;
    // 顺序注入：不并发，避免多段输入法上屏文字顺序颠倒。
    typingChain = typingChain.then(async () => {
      if (disposed) return;
      try {
        await call(m, "desktop.type_text", { text: chunk }, TYPE_TEXT_TIMEOUT_MS);
        if (!disposed) typeError.set(null);
      } catch (error) {
        if (!disposed) typeError.set(errText(error));
      }
    });
  };

  const handleDisconnect = () => {
    clearRetryTimer();
    retries = 0;
    userDisconnected.set(true);
    status.set({ kind: "closed", closed: { kind: "by_user" } });
  };

  const handleReconnect = () => {
    const wasActive = canvasActive();
    resetForFreshConnect();
    if (wasActive) {
      reconnectSeq.set(reconnectSeq() + 1);
    }
  };

  const presentation = computed(() => statusPresentation(status(), controlling(), typeError(), t));
  const statusColor = computed(() => {
    const p = look().palette;
    switch (presentation().tone) {
      case "strong":
        return p.text;
      case "bad":
        return p.bad;
      default:
        return p.muted;
    }
  });
  const connected = computed(() => status().kind === "connected");
  const closed = computed(() => status().kind === "closed");

  return (
    <View
      style={{
        flex: 1,
        width: "100%",
        height: "100%",
        minWidth: 0,
        minHeight: 0,
        display: "flex",
        flexDirection: "column",
      }}
    >
      <If
        when={hasMachineArg()}
        else={<Placeholder message={t("no_machine")} />}
      >
        <If
          when={entry() != null}
          else={<Placeholder message={t("machine_removed")} />}
        >
          {/* 工具行（48px） */}
          <View
            style={{
              width: "100%",
              minHeight: 48,
              flexShrink: 0,
              paddingLeft: 20,
              paddingRight: 20,
              paddingTop: 8,
              paddingBottom: 8,
              borderBottomWidth: 1,
              borderBottomColor: look().palette.hair,
              display: "flex",
              flexDirection: "row",
              alignItems: "center",
              justifyContent: "space-between",
              gap: 12,
            }}
          >
            <View
              style={{
                flex: 1,
                minWidth: 0,
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                gap: 10,
              }}
            >
              <Text
                style={{
                  fontSize: look().type.ui_medium.size,
                  lineHeight: look().type.ui_medium.line_height,
                  fontWeight: look().type.ui_medium.weight,
                  color: look().palette.text,
                  flexShrink: 0,
                  numberOfLines: 1,
                }}
              >
                {entry()?.name ?? ""}
              </Text>
              <Text
                style={{
                  flex: 1,
                  minWidth: 0,
                  fontSize: look().type.text_xsmall.size,
                  lineHeight: look().type.text_xsmall.line_height,
                  color: statusColor(),
                  numberOfLines: 1,
                }}
              >
                {presentation().label}
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
              <If when={closed()}>
                <SmeltButton
                  variant="primary"
                  size="sm"
                  label={t("action.reconnect")}
                  onPress={handleReconnect}
                />
              </If>
              <If when={connected()}>
                <SmeltButton
                  size="sm"
                  variant={controlling() ? "secondary" : "primary"}
                  label={controlling() ? t("action.view_only") : t("action.take_control")}
                  onPress={() => controlling.set(!controlling())}
                />
              </If>
              <If when={!closed()}>
                <SmeltButton
                  variant="quiet"
                  size="sm"
                  label={t("action.disconnect")}
                  onPress={handleDisconnect}
                />
              </If>
            </View>
          </View>

          {/* 远程桌面画布区域 */}
          <View
            style={{
              flex: 1,
              width: "100%",
              minWidth: 0,
              minHeight: 0,
              display: "flex",
            }}
          >
            <SmeltNativeDesktop
              machine={targetMachine() ?? null}
              active={canvasActive()}
              control={controlling()}
              reconnectSeq={reconnectSeq()}
              onConnected={handleConnected}
              onDisconnected={handleDisconnected}
              onClipboard={handleClipboard}
              onText={handleText}
              onError={handleError}
              style={{ flex: 1, width: "100%", height: "100%" }}
            />
          </View>
        </If>
      </If>
    </View>
  );
}

export default desktop;
