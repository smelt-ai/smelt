// 远程机器（来源 `smelt.machines`）：
// - 连出去：设置「远程」页管理配对过的远程机器，并在会话列表机器行右键菜单提供重连/升级/重命名/改中继/移除；
// - 连进来：远程访问开关、自建 relay、生成一次性配对串与二维码、管理已配对设备；
// - 角标：有机器离线、不兼容、高延迟或机器表读取失败时在「远程」页挂 "!" 角标。

import { Canvas, computed, Scroll, signal, Text, View } from "nsh";
import { Dialog, Input, Switch, Tooltip } from "nsh/base";
import {
  bootTask,
  call,
  clipboardWrite,
  connections,
  initHost,
  onCommand,
  onEvent,
  qrMatrix,
  registerCommands,
  registerContextMenuItems,
  registerViews,
  run,
  SmeltButton,
  SmeltIcon,
  smeltVersion,
  state as hostState,
  t,
  text,
  viewShown,
  waitOnline,
} from "smelt/sdk";

import {
  ADD_MACHINE_NAME,
  EDIT_RELAY_NAME,
  MACHINE_MENU,
  RECONNECT_NAME,
  REMOVE_NAME,
  RENAME_NAME,
  UPGRADE_NAME,
  VIEW,
  VIEW_NAME,
} from "./contract.js";
import {
  access_status,
  last_seen_text,
  machine_menu_items,
  machine_status,
  needs_attention,
  short_id,
} from "./status.js";

const LOCAL = null;
// 运行时 remote.status.wait 最长挂 20s，读超时需长于它。
const STATUS_WAIT_TIMEOUT_MS = 30_000;

const KEYWORDS = [
  "多机器",
  "机器",
  "远程机器",
  "添加机器",
  "添加远程机器",
  "其他运行时",
  "配对串",
  "smeltd pair",
  "离线",
  "版本不兼容",
  "远程访问",
  "开启远程",
  "Relay 地址",
  "自建 relay",
  "手机",
  "移动端",
  "配对",
  "二维码",
  "iroh",
  "relay",
  "中继",
  "remote",
  "machines",
  "remote machine",
  "add machine",
  "pairing",
  "offline",
  "incompatible",
  "remote access",
  "phone",
  "mobile",
  "QR code",
];

function errText(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error ?? "");
}

function notifyFailure(what: string, error: unknown) {
  const message = errText(error);
  console.warn(`[smelt.machines] ${what}：${message}`);
  void run("smelt.app/notify", { subtitle: what, body: message }).catch(() => {});
}

// ---- 全局对话框状态（挂在 `root` 上，不依赖设置页打开） ----

interface RemovePrompt {
  machine: string;
  name: string;
}

interface RenamePrompt {
  machine: string;
  original: string;
  value: string;
}

interface RelayPrompt {
  machine: string;
  name: string;
  online: boolean;
  value: string;
  saving: boolean;
  error: string | null;
}

interface PairingOffer {
  uri: string;
  qr: { size: number; bits: string } | null;
}

const removePrompt = signal<RemovePrompt | null>(null);
const renamePrompt = signal<RenamePrompt | null>(null);
const relayPrompt = signal<RelayPrompt | null>(null);
let relayToken = 0;

const pairingOpen = signal(false);
const pairingCopied = signal(false);

// ---- 设置「远程」页状态 ----

const pairingInput = signal("");
const adding = signal(false);
const addError = signal<string | null>(null);

const remoteConfig = signal<{ enabled: boolean; iroh_relay: string } | null>(null);
const relayInput = signal("");
let relayShown: string | null = null;
let configGeneration = 0;
let configLoadToken = 0;
let saveChain: Promise<void> = Promise.resolve();

const accessState = signal<any>({ state: "off" });
const devices = signal<any[]>([]);
const offer = signal<PairingOffer | null>(null);
const generatingOffer = signal(false);
const remoteError = signal<string | null>(null);

let watchToken = 0;
let pageWasShown = false;
let initializedPage = false;

// ---- 菜单与角标同步 ----

let lastBadge: string | null | undefined = undefined;
let lastMenuJson: string | null = null;

function updateBadge() {
  const label = needs_attention(connections()) ? "!" : null;
  if (label === lastBadge) return;
  lastBadge = label;
  void run("smelt.app/set-view-badge", { view: VIEW, label }).catch((error) => {
    console.warn("[smelt.machines] set-view-badge 失败：", errText(error));
  });
}

function publishMenu() {
  const items = machine_menu_items(connections(), text);
  const encoded = JSON.stringify(items);
  if (encoded === lastMenuJson) return;
  lastMenuJson = encoded;
  void registerContextMenuItems(MACHINE_MENU, items).catch((error) => {
    console.warn("[smelt.machines] 机器菜单项登记失败：", errText(error));
  });
}

function findRemote(machine: string) {
  const list = connections()?.remotes ?? [];
  return list.find((conn: any) => conn.machine === machine) ?? null;
}

// ---- 本机运行时调用（连进来） ----

function syncRelayInput(config: { enabled: boolean; iroh_relay: string }) {
  if (relayShown !== config.iroh_relay) {
    relayShown = config.iroh_relay;
    relayInput.set(config.iroh_relay);
  }
}

function loadConfig() {
  const gen = configGeneration;
  const token = ++configLoadToken;
  void (async () => {
    let failures = 0;
    for (;;) {
      if (token !== configLoadToken) return;
      if (!(await waitOnline(LOCAL, failures))) return;
      if (token !== configLoadToken) return;
      try {
        const cfg = await call(LOCAL, "remote.config.get");
        if (token !== configLoadToken) return;
        if (configGeneration === gen && cfg) {
          const normalized = {
            enabled: Boolean(cfg.enabled),
            iroh_relay: typeof cfg.iroh_relay === "string" ? cfg.iroh_relay : "",
          };
          remoteConfig.set(normalized);
          syncRelayInput(normalized);
        }
        return;
      } catch (error) {
        console.warn("[smelt.machines] 读远程访问配置失败：", errText(error));
        failures += 1;
      }
    }
  })();
}

function setAccessStatus(next: any) {
  const status = next ?? { state: "off" };
  accessState.set(status);
  if (status.state === "starting") {
    watchStatus();
  } else {
    watchToken += 1;
  }
}

function watchStatus() {
  const token = ++watchToken;
  void (async () => {
    let known: any = { state: "starting" };
    for (;;) {
      if (token !== watchToken) return;
      try {
        const next = await call(LOCAL, "remote.status.wait", { known }, STATUS_WAIT_TIMEOUT_MS);
        if (token !== watchToken) return;
        accessState.set(next);
        if (next?.state === "starting" && viewShown(VIEW)) {
          known = next;
          continue;
        }
        return;
      } catch (error) {
        if (token !== watchToken) return;
        remoteError.set(errText(error));
        return;
      }
    }
  })();
}

function refreshStatus() {
  void call(LOCAL, "remote.status")
    .then((status) => setAccessStatus(status))
    .catch((error) => remoteError.set(errText(error)));
}

function refreshDevices() {
  void call(LOCAL, "device.list")
    .then((result) => devices.set(Array.isArray(result?.devices) ? result.devices : []))
    .catch((error) => remoteError.set(errText(error)));
}

function refreshRemote() {
  loadConfig();
  refreshStatus();
  refreshDevices();
  void run("smelt.app/refresh-connections", null).catch(() => {});
}

function saveConfig(next: { enabled: boolean; iroh_relay: string }) {
  configGeneration += 1;
  configLoadToken += 1;
  remoteConfig.set(next);
  syncRelayInput(next);
  remoteError.set(null);
  // 配置变了，旧配对串里的 endpoint 或 relay 可能已经不对。
  offer.set(null);
  saveChain = saveChain.then(async () => {
    try {
      const status = await call(LOCAL, "remote.config.set", next);
      setAccessStatus(status);
    } catch (error) {
      remoteError.set(t("error.save_config", { error: errText(error) }));
      loadConfig();
    }
  });
}

function setEnabled(enabled: boolean) {
  const cfg = remoteConfig();
  if (!cfg) {
    remoteError.set(t("error.config_not_loaded"));
    return;
  }
  saveConfig({ ...cfg, enabled });
}

function commitRelay(raw: string) {
  const cfg = remoteConfig();
  if (!cfg) {
    remoteError.set(t("error.config_not_loaded"));
    return;
  }
  const trimmed = raw.trim();
  if (cfg.iroh_relay === trimmed) return;
  saveConfig({ ...cfg, iroh_relay: trimmed });
}

function beginPairing() {
  if (generatingOffer()) return;
  generatingOffer.set(true);
  remoteError.set(null);
  pairingCopied.set(false);
  void call(LOCAL, "device.pair_begin")
    .then((result) => {
      const uri = String(result?.pairing_uri ?? "");
      const qr = uri ? qrMatrix(uri) : null;
      offer.set({ uri, qr });
    })
    .catch((error) => {
      remoteError.set(t("error.pairing", { error: errText(error) }));
    })
    .finally(() => {
      generatingOffer.set(false);
    });
}

function revokeDevice(endpointId: string) {
  void call(LOCAL, "device.revoke", { endpoint_id: endpointId })
    .catch((error) => {
      remoteError.set(t("error.revoke", { error: errText(error) }));
    })
    .finally(() => {
      refreshDevices();
    });
}

function submitAddMachine() {
  if (adding()) return;
  const uri = pairingInput().trim();
  if (!uri) {
    addError.set(t("add.empty"));
    return;
  }
  adding.set(true);
  addError.set(null);
  void run("smelt.app/add-machine", { pairing_uri: uri })
    .then(() => {
      pairingInput.set("");
    })
    .catch((error) => {
      addError.set(errText(error));
    })
    .finally(() => {
      adding.set(false);
    });
}

// ---- 对话框动作 ----

function openRemoveDialog(machine: string, name: string) {
  removePrompt.set({ machine, name });
}

function confirmRemove() {
  const req = removePrompt();
  if (!req) return;
  removePrompt.set(null);
  void run("smelt.app/remove-machine", { machine: req.machine }).catch((error) => {
    notifyFailure(t("remove.failed"), error);
  });
}

function openRenameDialog(conn: any) {
  renamePrompt.set({
    machine: conn.machine,
    original: conn.name,
    value: conn.name,
  });
}

function submitRename() {
  const req = renamePrompt();
  if (!req) return;
  renamePrompt.set(null);
  const name = req.value.trim();
  if (!name || name === req.original) return;
  void run("smelt.app/rename-machine", { machine: req.machine, name }).catch((error) => {
    notifyFailure(t("rename.failed"), error);
  });
}

function openRelayDialog(conn: any) {
  relayToken += 1;
  relayPrompt.set({
    machine: conn.machine,
    name: conn.name,
    online: conn.state?.state === "online",
    value: conn.relay ?? "",
    saving: false,
    error: null,
  });
}

function closeRelayDialog() {
  relayToken += 1;
  relayPrompt.set(null);
}

function submitRelay() {
  const req = relayPrompt();
  if (!req || req.saving) return;
  const token = relayToken;
  relayPrompt.set({ ...req, saving: true, error: null });
  void run("smelt.app/set-machine-relay", { machine: req.machine, relay: req.value })
    .then(() => {
      if (token === relayToken) {
        relayPrompt.set(null);
      }
    })
    .catch((error) => {
      const msg = errText(error);
      if (token === relayToken && relayPrompt() != null) {
        relayPrompt.set({ ...relayPrompt()!, saving: false, error: msg });
      } else {
        notifyFailure(t("relay.failed"), msg);
      }
    });
}

// ---- 事件、命令与启动注册 ----

onCommand(async ({ name, args }) => {
  switch (name) {
    case ADD_MACHINE_NAME: {
      const opened = await run("smelt.app/open-view", { view: VIEW, args: null });
      if (!opened) throw new Error(t("error.page_unavailable"));
      return null;
    }
    case RECONNECT_NAME: {
      await run("smelt.app/reconnect-machine", { machine: args?.machine ?? null });
      return null;
    }
    case UPGRADE_NAME: {
      await run("smelt.app/upgrade-machine", { machine: args?.machine ?? null });
      return null;
    }
    case RENAME_NAME: {
      const conn = findRemote(args?.machine);
      if (!conn) throw new Error(t("error.machine_removed"));
      openRenameDialog(conn);
      return null;
    }
    case EDIT_RELAY_NAME: {
      const conn = findRemote(args?.machine);
      if (!conn) throw new Error(t("error.machine_removed"));
      openRelayDialog(conn);
      return null;
    }
    case REMOVE_NAME: {
      const conn = findRemote(args?.machine);
      if (!conn) throw new Error(t("error.machine_removed"));
      openRemoveDialog(conn.machine, conn.name);
      return null;
    }
    default:
      throw new Error(`Unknown command: ${name}`);
  }
});

onEvent((event) => {
  if (event?.type === "machines_changed") {
    publishMenu();
    updateBadge();
  } else if (event?.type === "shown_views_changed") {
    const shown = viewShown(VIEW);
    if (shown && !pageWasShown) {
      refreshRemote();
    } else if (!shown) {
      watchToken += 1;
    }
    pageWasShown = shown;
  }
});

bootTask(async () => {
  await initHost();
  await registerViews([
    {
      name: VIEW_NAME,
      export: VIEW_NAME,
      title: text("page.title"),
      icon: "icons/globe.svg",
      draws_top_bar: false,
      instances: false,
      settings: {
        group: "agents",
        keywords: KEYWORDS,
      },
    },
  ]);
  await registerCommands(
    [
      {
        name: ADD_MACHINE_NAME,
        title: text("command.add"),
        palette: true,
        requires_args: false,
        args: [],
      },
      {
        name: RECONNECT_NAME,
        title: text("command.reconnect"),
        palette: false,
        requires_args: true,
        args: ["machine"],
      },
      {
        name: UPGRADE_NAME,
        title: text("command.upgrade"),
        palette: false,
        requires_args: true,
        args: ["machine"],
      },
      {
        name: RENAME_NAME,
        title: text("command.rename"),
        palette: false,
        requires_args: true,
        args: ["machine"],
      },
      {
        name: EDIT_RELAY_NAME,
        title: text("command.edit_relay"),
        palette: false,
        requires_args: true,
        args: ["machine"],
      },
      {
        name: REMOVE_NAME,
        title: text("command.remove"),
        palette: false,
        requires_args: true,
        args: ["machine"],
      },
    ],
    text("commands"),
  );
  publishMenu();
  updateBadge();
});

// ---- 界面组件 ----

function toneColor(palette: any, tone: string): string {
  switch (tone) {
    case "ok":
      return palette.ok;
    case "warning":
      return palette.need;
    case "bad":
      return palette.bad;
    default:
      return palette.muted;
  }
}

function StatusDot(props: { size: number; color: string }) {
  return (
    <View
      style={{
        width: props.size,
        height: props.size,
        borderRadius: props.size / 2,
        backgroundColor: props.color,
        flexShrink: 0,
      }}
    />
  );
}

function QrCanvas(props: { matrix: { size: number; bits: string }; displaySize?: number }) {
  const pxSize = props.displaySize ?? 132;
  const quiet = 2;
  return (
    <View
      style={{
        flexShrink: 0,
        padding: 8,
        borderRadius: 8,
        // 二维码底必须是纯白，两种主题都一样：深色底上的二维码扫不出来。
        backgroundColor: "#ffffff",
      }}
    >
      <Canvas
        style={{ width: pxSize, height: pxSize }}
        draw={(cx: any) => {
          const { size, bits } = props.matrix;
          const total = size + quiet * 2;
          const cell = pxSize / total;
          cx.fillStyle = "#ffffff";
          cx.fillRect(0, 0, pxSize, pxSize);
          cx.fillStyle = "#000000";
          for (let row = 0; row < size; row++) {
            const offset = row * size;
            for (let col = 0; col < size; col++) {
              if (bits.charCodeAt(offset + col) === 49) {
                const x = Math.floor((col + quiet) * cell);
                const y = Math.floor((row + quiet) * cell);
                const x2 = Math.ceil((col + quiet + 1) * cell);
                const y2 = Math.ceil((row + quiet + 1) * cell);
                cx.fillRect(x, y, Math.max(1, x2 - x), Math.max(1, y2 - y));
              }
            }
          }
        }}
      />
    </View>
  );
}

function SectionCard(props: { title: string; description: string; children?: any }) {
  const look = computed(() => hostState().theme);
  return (
    <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <View style={{ display: "flex", flexDirection: "column", gap: 2 }}>
        <Text
          style={{
            fontSize: look().type.text_xsmall_medium.size,
            fontWeight: look().type.text_xsmall_medium.weight,
            color: look().palette.muted,
          }}
        >
          {props.title}
        </Text>
        <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
          {props.description}
        </Text>
      </View>
      <View
        style={{
          borderRadius: look().radius.md,
          borderWidth: 1,
          borderColor: look().palette.hair,
          backgroundColor: look().palette.surface,
          display: "flex",
          flexDirection: "column",
        }}
      >
        {props.children}
      </View>
    </View>
  );
}

export function remote() {
  if (!initializedPage) {
    initializedPage = true;
    pageWasShown = true;
    refreshRemote();
  }
  const look = computed(() => hostState().theme);
  const connTable = computed(() => connections());
  const remotes = computed(() => connTable()?.remotes ?? []);
  const loadError = computed(() => connTable()?.load_error ?? null);
  const cfg = computed(() => remoteConfig());
  const loaded = computed(() => cfg() != null);
  const enabled = computed(() => Boolean(cfg()?.enabled));
  const running = computed(() => accessState()?.state === "running");
  const accessInfo = computed(() => {
    hostState();
    return access_status(accessState(), t);
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
          gap: 24,
        }}
      >
        {/* 连到其他机器 */}
        <SectionCard title={t("outgoing.title")} description={t("outgoing.description")}>
          <View
            style={{
              padding: 14,
              borderBottomWidth: 1,
              borderBottomColor: look().palette.hair,
              display: "flex",
              flexDirection: "column",
              gap: 10,
            }}
          >
            <If when={loadError() != null}>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
                {loadError() ?? ""}
              </Text>
            </If>
            <If when={remotes().length === 0 && loadError() == null}>
              <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
                {t("outgoing.empty")}
              </Text>
            </If>
            <Each of={remotes()} key={(conn: any) => conn.machine}>
              {(conn: () => any) => {
                const status = () => {
                  hostState();
                  return machine_status(conn(), t);
                };
                const isOnline = () => conn().state?.state === "online";
                const p = () => look().palette;
                return (
                  <View
                    style={{
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 10,
                    }}
                  >
                    <StatusDot size={8} color={isOnline() ? p().ok : p().muted} />
                    <View
                      style={{
                        flex: 1,
                        minWidth: 0,
                        display: "flex",
                        flexDirection: "column",
                        gap: 2,
                      }}
                    >
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: p().text }}>
                        {conn().name}
                      </Text>
                      <Text
                        style={{
                          fontSize: look().type.text_xsmall.size,
                          color: toneColor(p(), status().tone),
                        }}
                      >
                        {status().text}
                      </Text>
                      <If when={Boolean(conn().upgrade_note)}>
                        <Text style={{ fontSize: look().type.text_xsmall.size, color: p().need }}>
                          {conn().upgrade_note ?? ""}
                        </Text>
                      </If>
                    </View>
                    <If when={Boolean(conn().can_upgrade)}>
                      <SmeltButton
                        small={true}
                        label={t("upgrade.action")}
                        tooltip={t("upgrade.tooltip", { version: smeltVersion() })}
                        onPress={() => {
                          const target = conn().machine;
                          void run("smelt.app/upgrade-machine", { machine: target }).catch(
                            (error) => notifyFailure(t("upgrade.failed"), error),
                          );
                        }}
                      />
                    </If>
                    <SmeltButton
                      small={true}
                      label={t("remove.action")}
                      onPress={() => openRemoveDialog(conn().machine, conn().name)}
                    />
                  </View>
                );
              }}
            </Each>
          </View>

          <View style={{ padding: 14, display: "flex", flexDirection: "column", gap: 8 }}>
            <View style={{ display: "flex", flexDirection: "column", gap: 2 }}>
              <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                {t("add.title")}
              </Text>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {t("add.description")}
              </Text>
            </View>
            <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
              <View style={{ flex: 1, minWidth: 0 }}>
                <Input
                  value={pairingInput()}
                  placeholder="smelt+iroh://…"
                  disabled={adding()}
                  onChange={({ value }: { value: string }) => pairingInput.set(value)}
                  onSubmit={submitAddMachine}
                  class="rounded-(--radius) border border-input bg-card px-2 text-foreground"
                  style={{ height: 28, fontSize: look().type.ui.size }}
                />
              </View>
              <SmeltButton
                small={true}
                label={adding() ? t("add.adding") : t("add.action")}
                loading={adding()}
                disabled={adding()}
                onPress={submitAddMachine}
              />
            </View>
            <If when={addError() != null}>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
                {addError() ?? ""}
              </Text>
            </If>
          </View>
        </SectionCard>

        {/* 让其他设备连进来 */}
        <SectionCard title={t("incoming.title")} description={t("incoming.description")}>
          {/* 远程访问开关 */}
          <View
            style={{
              padding: 14,
              borderBottomWidth: 1,
              borderBottomColor: look().palette.hair,
              display: "flex",
              flexDirection: "row",
              alignItems: "center",
              gap: 12,
            }}
          >
            <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
              <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                {t("access.title")}
              </Text>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {t("access.description")}
              </Text>
            </View>
            <View
              style={{
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                gap: 6,
                maxWidth: 260,
              }}
            >
              <StatusDot size={6} color={toneColor(look().palette, accessInfo().tone)} />
              <Text
                style={{
                  fontSize: look().type.text_xsmall.size,
                  color: toneColor(look().palette, accessInfo().tone),
                  numberOfLines: 1,
                }}
              >
                {accessInfo().text}
              </Text>
            </View>
            <Switch
              checked={enabled()}
              disabled={!loaded()}
              accessibilityLabel={t("access.title")}
              onChange={({ checked }: { checked: boolean }) => setEnabled(Boolean(checked))}
              trackStyle={{
                width: 34,
                height: 20,
                borderRadius: 10,
                padding: 2,
                backgroundColor: enabled() ? look().palette.link : look().palette.soft,
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                justifyContent: enabled() ? "flex-end" : "flex-start",
              }}
              thumbStyle={{
                width: 16,
                height: 16,
                borderRadius: 8,
                backgroundColor: look().palette.on_ink,
              }}
            />
          </View>

          {/* 已配对设备 + 配对新设备按钮 */}
          <View
            style={{
              padding: 14,
              borderBottomWidth: 1,
              borderBottomColor: look().palette.hair,
              display: "flex",
              flexDirection: "column",
              gap: 10,
            }}
          >
            <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 12 }}>
              <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
                <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                  {t("devices.title")}
                </Text>
                <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                  {t("pairing.warning")}
                </Text>
              </View>
              <SmeltButton
                small={true}
                label={t("pairing.begin")}
                disabled={!running()}
                tooltip={running() ? undefined : t("pairing.needs_access")}
                onPress={() => {
                  beginPairing();
                  pairingOpen.set(true);
                }}
              />
            </View>

            <If when={remoteError() != null}>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
                {remoteError() ?? ""}
              </Text>
            </If>

            <If when={devices().length === 0}>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {t("devices.empty")}
              </Text>
            </If>

            <Each of={devices()} key={(dev: any) => dev.endpoint_id}>
              {(dev: () => any) => {
                const p = () => look().palette;
                const seen = () => {
                  hostState();
                  return `${short_id(dev().endpoint_id)} · ${last_seen_text(dev(), Date.now(), t)}`;
                };
                return (
                  <View
                    style={{
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 10,
                    }}
                  >
                    <StatusDot size={8} color={dev().connected ? p().ok : p().muted} />
                    <View
                      style={{
                        flex: 1,
                        minWidth: 0,
                        display: "flex",
                        flexDirection: "column",
                        gap: 2,
                      }}
                    >
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: p().text }}>
                        {dev().name}
                      </Text>
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: p().muted }}>
                        {seen()}
                      </Text>
                    </View>
                    <SmeltButton
                      small={true}
                      label={t("devices.revoke")}
                      onPress={() => revokeDevice(dev().endpoint_id)}
                    />
                  </View>
                );
              }}
            </Each>
          </View>

          {/* 自建 relay */}
          <View
            style={{
              padding: 14,
              display: "flex",
              flexDirection: "row",
              alignItems: "center",
              gap: 12,
            }}
          >
            <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
              <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                {t("relay.custom")}
              </Text>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {t("relay.custom_description")}
              </Text>
            </View>
            <View style={{ width: 220, flexShrink: 0 }}>
              <Input
                value={relayInput()}
                disabled={!loaded()}
                accessibilityLabel={t("relay.custom")}
                onChange={({ value }: { value: string }) => relayInput.set(value)}
                onBlur={() => commitRelay(relayInput())}
                onSubmit={() => commitRelay(relayInput())}
                class="rounded-(--radius) border border-input bg-card px-2 text-foreground"
                style={{ height: 28, fontSize: look().type.ui.size }}
              />
            </View>
          </View>
        </SectionCard>
      </View>
    </Scroll>
  );
}

/** 全局对话框（挂载在主壳无头根层，从会话列表右键菜单或设置页都能弹出）。 */
export function root() {
  const look = computed(() => hostState().theme);
  return (
    <>
      {/* 移除机器确认框 */}
      <Dialog
        open={removePrompt() != null}
        role="alertdialog"
        animation="fade"
        accessibilityLabel={t("remove.title")}
        backdropClass="bg-black/40"
        onOpenChange={({ open }: { open: boolean }) => {
          if (!open) removePrompt.set(null);
        }}
        class="m-auto rounded-(--radius) border border-border bg-popover p-4"
        style={{ width: 380, gap: 12 }}
      >
        <If when={removePrompt() != null}>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui_medium.size,
              fontWeight: look().type.ui_medium.weight,
            }}
          >
            {t("remove.title")}
          </Text>
          <Text style={{ color: look().palette.muted, fontSize: look().type.text_xsmall.size }}>
            {t("remove.description", { name: removePrompt()?.name ?? "" })}
          </Text>
          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton
              quiet={true}
              small={true}
              label={t("action.cancel")}
              onPress={() => removePrompt.set(null)}
            />
            <SmeltButton
              danger={true}
              small={true}
              label={t("remove.action")}
              onPress={confirmRemove}
            />
          </View>
        </If>
      </Dialog>

      {/* 重命名机器框 */}
      <Dialog
        open={renamePrompt() != null}
        role="dialog"
        animation="fade"
        accessibilityLabel={t("rename.title")}
        backdropClass="bg-black/40"
        onOpenChange={({ open }: { open: boolean }) => {
          if (!open) renamePrompt.set(null);
        }}
        class="m-auto rounded-(--radius) border border-border bg-popover p-4"
        style={{ width: 380, gap: 12 }}
      >
        <If when={renamePrompt() != null}>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui_medium.size,
              fontWeight: look().type.ui_medium.weight,
            }}
          >
            {t("rename.title")}
          </Text>
          <Text style={{ color: look().palette.muted, fontSize: look().type.text_xsmall.size }}>
            {t("rename.description")}
          </Text>
          <Input
            autoFocus={true}
            value={renamePrompt()?.value ?? ""}
            onChange={({ value }: { value: string }) => {
              const cur = renamePrompt();
              if (cur) renamePrompt.set({ ...cur, value });
            }}
            onSubmit={submitRename}
            class="rounded-(--radius) border border-input bg-card px-2 text-foreground"
            style={{ height: 28, fontSize: look().type.ui.size }}
          />
          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton
              quiet={true}
              small={true}
              label={t("action.cancel")}
              onPress={() => renamePrompt.set(null)}
            />
            <SmeltButton
              primary={true}
              small={true}
              label={t("rename.action")}
              onPress={submitRename}
            />
          </View>
        </If>
      </Dialog>

      {/* 改机器中继框 */}
      <Dialog
        open={relayPrompt() != null}
        role="dialog"
        animation="fade"
        accessibilityLabel={t("relay.title", { name: relayPrompt()?.name ?? "" })}
        backdropClass="bg-black/40"
        onOpenChange={({ open }: { open: boolean }) => {
          if (!open) closeRelayDialog();
        }}
        class="m-auto rounded-(--radius) border border-border bg-popover p-4"
        style={{ width: 420, gap: 12 }}
      >
        <If when={relayPrompt() != null}>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui_medium.size,
              fontWeight: look().type.ui_medium.weight,
            }}
          >
            {t("relay.title", { name: relayPrompt()?.name ?? "" })}
          </Text>
          <Text style={{ color: look().palette.muted, fontSize: look().type.text_xsmall.size }}>
            {relayPrompt()?.online ? t("relay.hint_online") : t("relay.hint_offline")}
          </Text>
          <Input
            autoFocus={true}
            disabled={Boolean(relayPrompt()?.saving)}
            placeholder={t("relay.placeholder")}
            value={relayPrompt()?.value ?? ""}
            onChange={({ value }: { value: string }) => {
              const cur = relayPrompt();
              if (cur) relayPrompt.set({ ...cur, value });
            }}
            onSubmit={submitRelay}
            class="rounded-(--radius) border border-input bg-card px-2 text-foreground"
            style={{ height: 28, fontSize: look().type.ui.size }}
          />
          <If when={Boolean(relayPrompt()?.saving)}>
            <Text style={{ color: look().palette.muted, fontSize: look().type.ui.size }}>
              {t("relay.saving")}
            </Text>
          </If>
          <If when={relayPrompt()?.error != null}>
            <Text style={{ color: look().palette.bad, fontSize: look().type.ui.size }}>
              {relayPrompt()?.error ?? ""}
            </Text>
          </If>
          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton
              quiet={true}
              small={true}
              label={t("action.cancel")}
              onPress={closeRelayDialog}
            />
            <SmeltButton
              primary={true}
              small={true}
              loading={Boolean(relayPrompt()?.saving)}
              disabled={Boolean(relayPrompt()?.saving)}
              label={t("relay.save")}
              onPress={submitRelay}
            />
          </View>
        </If>
      </Dialog>

      {/* 配对新设备二维码框 */}
      <Dialog
        open={pairingOpen()}
        role="dialog"
        animation="fade"
        accessibilityLabel={t("pairing.title")}
        backdropClass="bg-black/40"
        onOpenChange={({ open }: { open: boolean }) => {
          if (!open) {
            pairingOpen.set(false);
            refreshDevices();
          }
        }}
        class="m-auto rounded-(--radius) border border-border bg-popover p-4"
        style={{ width: 520, gap: 12 }}
      >
        <Text
          style={{
            color: look().palette.text,
            fontSize: look().type.ui_medium.size,
            fontWeight: look().type.ui_medium.weight,
          }}
        >
          {t("pairing.title")}
        </Text>
        <Text style={{ color: look().palette.need, fontSize: look().type.ui.size }}>
          {t("pairing.warning")}
        </Text>
        <If when={remoteError() != null}>
          <Text style={{ color: look().palette.bad, fontSize: look().type.text_xsmall.size }}>
            {remoteError() ?? ""}
          </Text>
        </If>
        <If
          when={offer() != null}
          else={
            <Text style={{ color: look().palette.muted, fontSize: look().type.ui.size }}>
              {generatingOffer() ? t("pairing.generating") : t("pairing.none")}
            </Text>
          }
        >
          <View style={{ display: "flex", flexDirection: "row", alignItems: "flex-start", gap: 12 }}>
            <If when={offer()?.qr != null}>
              <QrCanvas matrix={offer()!.qr!} displaySize={132} />
            </If>
            <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 8 }}>
              <Text style={{ color: look().palette.text, fontSize: look().type.ui.size }}>
                {t("pairing.instructions")}
              </Text>
              <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 6 }}>
                <Text
                  style={{
                    flex: 1,
                    minWidth: 0,
                    color: look().palette.muted,
                    fontSize: look().type.text_xsmall.size,
                    fontFamily: look().fonts?.mono,
                    numberOfLines: 1,
                  }}
                >
                  {offer()?.uri ?? ""}
                </Text>
                <View
                  role="button"
                  accessibilityLabel={t("pairing.copy")}
                  class="items-center justify-center rounded-(--radius) hover:bg-accent"
                  style={{ width: 24, height: 24, flexShrink: 0 }}
                  tooltip={
                    <Tooltip class="px-2 py-1 rounded bg-popover border border-border">
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.text }}>
                        {pairingCopied() ? t("pairing.copied") : t("pairing.copy")}
                      </Text>
                    </Tooltip>
                  }
                  onPress={() => {
                    const uri = offer()?.uri;
                    if (!uri) return;
                    void clipboardWrite(uri).then(() => pairingCopied.set(true));
                  }}
                >
                  <SmeltIcon
                    path={pairingCopied() ? "icons/check.svg" : "icons/copy.svg"}
                    size={13}
                    color={pairingCopied() ? look().palette.ok : look().palette.muted}
                  />
                </View>
              </View>
              <Text style={{ color: look().palette.muted, fontSize: look().type.text_xsmall.size }}>
                {t("pairing.expiry")}
              </Text>
            </View>
          </View>
        </If>
        <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
          <SmeltButton
            quiet={true}
            small={true}
            disabled={generatingOffer()}
            label={t("pairing.regenerate")}
            onPress={beginPairing}
          />
          <SmeltButton
            small={true}
            label={t("pairing.done")}
            onPress={() => {
              pairingOpen.set(false);
              refreshDevices();
            }}
          />
        </View>
      </Dialog>
    </>
  );
}
