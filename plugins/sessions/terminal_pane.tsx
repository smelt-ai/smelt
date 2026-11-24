// 会话内终端分屏格：直接使用原子能力 `<SmeltNativeTerminal>`（`crates/smelt-terminal`），
// 零跨 VM 插槽跳转，同时承载连接态/失败重试、断线提示、写入错误通知与 `send-text` 直投。

import { cleanup, computed, effect, signal, Text, View } from "nsh";
import {
  machineEntry,
  run,
  SmeltButton,
  SmeltNativeTerminal,
  stableMemo,
  currentTheme,
  locale,
  t,
} from "smelt/sdk";
import { terminalPaneKey, terminalWriteErrorKey } from "./model.js";

const TERMINAL_MENU = "smelt.terminal/menu";
const NOTIFY_CMD = "smelt.app/notify";


const openTerminals = new Map<string, Set<(chunk: string) => void>>();
let nextSendSeq = 0;

function registerOpenTerminal(key: string, send: (chunk: string) => void): () => void {
  let set = openTerminals.get(key);
  if (!set) {
    set = new Set();
    openTerminals.set(key, set);
  }
  set.add(send);
  return () => {
    const cur = openTerminals.get(key);
    if (!cur) return;
    cur.delete(send);
    if (cur.size === 0) {
      openTerminals.delete(key);
    }
  };
}

export function sendTextToTerminal(machine: string | null, sessionId: string, text: string) {
  const set = openTerminals.get(terminalPaneKey(machine, sessionId));
  if (!set || set.size === 0) {
    throw new Error(t("terminal.not_open", { session: sessionId }));
  }
  for (const send of set) {
    send(text);
    return;
  }
}

export function TerminalPane(props: {
  args: { machine?: string | null; session_id?: string; cwd?: string | null; launch?: string | null } | null;
  reuseKey?: string;
  focusSeq?: number;
  onFocusIn?: () => void;
}) {
  const look = currentTheme;
  const status = signal<"idle" | "connecting" | "live" | "failed">("connecting");
  const connectError = signal<string | null>(null);
  const reconnecting = signal(false);
  const retrySeq = signal(0);
  const sendTextProp = signal<{ seq: number; text: string } | null>(null);

  const spec = stableMemo(() => {
    const raw = props.args;
    const sessionId = typeof raw?.session_id === "string" ? raw.session_id : "";
    if (!sessionId) return null;
    const machine = raw?.machine ?? null;
    return {
      machine,
      session_id: sessionId,
      cwd: typeof raw?.cwd === "string" && raw.cwd.length > 0 ? raw.cwd : null,
      launch: typeof raw?.launch === "string" && raw.launch.length > 0 ? raw.launch : null,
      ...(props.reuseKey ? { reuseKey: props.reuseKey } : {}),
    };
  });

  const labels = stableMemo(() => {
    void locale();
    return {
      copy: t("terminal.copy"),
      paste: t("terminal.paste"),
      search_placeholder: t("terminal.search_placeholder"),
      search_none: t("terminal.search_none"),
    };
  });

  const runtimeName = computed(() => {
    const s = spec();
    if (!s) return "";
    const entry = machineEntry(s.machine);
    if (entry?.name) return String(entry.name);
    return typeof s.machine === "string" ? s.machine : "";
  });

  effect(() => {
    const s = spec();
    if (!s) return;
    const key = terminalPaneKey(s.machine, s.session_id);
    const unreg = registerOpenTerminal(key, (chunk) => {
      sendTextProp.set({ seq: ++nextSendSeq, text: chunk });
    });
    cleanup(unreg);
  });

  return (
    <View
      style={{
        position: "relative",
        flex: 1,
        width: "100%",
        height: "100%",
        minWidth: 0,
        minHeight: 0,
        overflow: "hidden",
      }}
    >
      <If when={spec() != null}>
        <SmeltNativeTerminal
          spec={spec()}
          contextMenu={TERMINAL_MENU}
          labels={labels()}
          focusSeq={props.focusSeq ?? 0}
          retrySeq={retrySeq()}
          sendText={sendTextProp()}
          onStateChange={(ev: { state?: string; error?: string | null }) => {
            const next = ev?.state;
            if (next === "idle" || next === "connecting" || next === "live" || next === "failed") {
              status.set(next);
            }
            connectError.set(typeof ev?.error === "string" ? ev.error : null);
          }}
          onReconnecting={(ev: { active?: boolean }) => {
            reconnecting.set(Boolean(ev?.active));
          }}
          onWriteError={(ev: { kind?: string }) => {
            const key = terminalWriteErrorKey(ev?.kind);
            if (!key) return;
            void run(NOTIFY_CMD, {
              subtitle: t("error.title"),
              body: t(key),
            }).catch(() => {});
          }}
          onFocusIn={() => {
            props.onFocusIn?.();
          }}
          style={{ flex: 1, width: "100%", height: "100%" }}
        />

        <If when={status() === "idle" || status() === "connecting"}>
          <View
            style={{
              position: "absolute",
              top: 0,
              left: 0,
              right: 0,
              bottom: 0,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              pointerEvents: "none",
            }}
          >
            <Text
              style={{
                fontSize: look().type.ui.size,
                lineHeight: look().type.ui.line_height,
                color: look().palette.muted,
              }}
            >
              {t("terminal.connecting")}
            </Text>
          </View>
        </If>

        <If when={status() === "failed"}>
          <View
            style={{
              position: "absolute",
              top: 0,
              left: 0,
              right: 0,
              bottom: 0,
              display: "flex",
              flexDirection: "column",
              alignItems: "center",
              justifyContent: "center",
              gap: 12,
              padding: 16,
            }}
          >
            <Text
              style={{
                fontSize: look().type.ui.size,
                lineHeight: look().type.ui.line_height,
                color: look().palette.muted,
              }}
            >
              {t("terminal.connect_error", { error: connectError() ?? "" })}
            </Text>
            <SmeltButton
              small={true}
              label={t("terminal.retry")}
              onPress={() => retrySeq.set(retrySeq() + 1)}
            />
          </View>
        </If>

        <If when={reconnecting()}>
          <View
            style={{
              position: "absolute",
              top: 8,
              left: 8,
              paddingLeft: 8,
              paddingRight: 8,
              paddingTop: 4,
              paddingBottom: 4,
              borderRadius: look().radius.md ?? 6,
              backgroundColor: look().palette.surface,
              borderWidth: 1,
              borderColor: look().palette.hair,
              pointerEvents: "none",
            }}
          >
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: look().palette.muted,
              }}
            >
              {t("terminal.reconnecting", { runtime: runtimeName() })}
            </Text>
          </View>
        </If>
      </If>
    </View>
  );
}
