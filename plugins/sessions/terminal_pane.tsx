// 会话内终端分屏格：直接使用原子能力 `<SmeltNativeTerminal>`（`crates/smelt-terminal`），
// 零跨 VM 插槽跳转；在 NSH 侧承载连接态/失败重试、断线提示、Cmd+F 搜索条、右键菜单、
// 写入错误通知与 `send-text` 直投。

import { cleanup, computed, Image, signal, Text, View } from "nsh";
import { Input, Submenu } from "nsh/base";
import {
  clipboardRead,
  clipboardWrite,
  contextMenuItems,
  currentTheme,
  machineMetaEntry,
  MENU_ITEM_CLASS,
  run,
  SmeltButton,
  SmeltContextMenu,
  SmeltIcon,
  SmeltMenuItem,
  SmeltMenuSeparator,
  SmeltNativeTerminal,
  SmeltTooltip,
  stableMemo,
  t,
} from "smelt/sdk";
import { terminalPaneKey, terminalWriteErrorKey } from "./model.js";

const TERMINAL_MENU = "smelt.terminal/menu";
const NOTIFY_CMD = "smelt.app/notify";

const openTerminals = new Map<string, Set<(chunk: string) => void>>();
let nextSendSeq = 0;

interface ContributedMenuItem {
  label: string;
  image?: string | null;
  icon?: string | null;
  command?: string;
  args?: unknown;
  submenu?: ContributedMenuItem[];
}

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

function errText(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error ?? "");
}

function ContributedEntries(props: {
  items: ContributedMenuItem[];
  onRun: (item: ContributedMenuItem) => void;
}) {
  const look = currentTheme;
  return (
    <>
      <Each of={props.items} key={(item: ContributedMenuItem, idx: number) => `${item.label}:${idx}`}>
        {(item: () => ContributedMenuItem) => {
          const cur = item();
          if (cur.submenu && cur.submenu.length > 0) {
            return (
              <Submenu
                trigger={
                  <View class={MENU_ITEM_CLASS} style={{ height: 28, gap: 8 }}>
                    {cur.image ? (
                      <View style={{ width: 14, height: 14, flexShrink: 0 }}>
                        <Image src={cur.image} style={{ width: 14, height: 14, objectFit: "contain" }} />
                      </View>
                    ) : cur.icon ? (
                      <View style={{ width: 14, height: 14, flexShrink: 0 }}>
                        <SmeltIcon path={cur.icon} size={14} color={look().palette.muted} />
                      </View>
                    ) : null}
                    <Text
                      class="flex-1 min-w-0"
                      style={{
                        color: look().palette.text,
                        fontSize: look().type.ui.size,
                        lineHeight: look().type.ui.line_height,
                        numberOfLines: 1,
                      }}
                    >
                      {cur.label}
                    </Text>
                    <SmeltIcon path="icons/chevron-right.svg" size={12} color={look().palette.muted} />
                  </View>
                }
              >
                <ContributedEntries items={cur.submenu} onRun={props.onRun} />
              </Submenu>
            );
          }
          return (
            <SmeltMenuItem
              label={cur.label}
              image={cur.image}
              icon={cur.icon}
              onSelect={() => props.onRun(cur)}
            />
          );
        }}
      </Each>
    </>
  );
}

interface TerminalSpec {
  machine: string | null;
  session_id: string;
  cwd: string | null;
  launch: string | null;
  reuseKey?: string;
}

/** 哪个终端：同一台机器上的同一段会话。原生终端也按它认画布（别的字段变了画布照旧）。 */
interface TerminalIdentity {
  machine: string | null;
  session_id: string;
}

export function TerminalPane(props: {
  args: { machine?: string | null; session_id?: string; cwd?: string | null; launch?: string | null } | null;
  reuseKey?: string;
  focusSeq?: number;
  onFocusIn?: () => void;
}) {
  const spec = stableMemo((): TerminalSpec | null => {
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

  const terminal = stableMemo((): TerminalIdentity | null => {
    const s = spec();
    return s ? { machine: s.machine, session_id: s.session_id } : null;
  });

  // 换了终端整块重建：搜索条、选区、菜单失败提示这些跟着终端的状态从头来。
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
      <Each of={terminal() ? [terminal()!] : []}>
        {(id: () => TerminalIdentity) => (
          <TerminalSession id={id()} spec={spec()} focusSeq={props.focusSeq} onFocusIn={props.onFocusIn} />
        )}
      </Each>
    </View>
  );
}

function TerminalSession(props: {
  /** 这一块的终端，挂上后不变（换终端是整块重建）。 */
  id: TerminalIdentity;
  spec: TerminalSpec | null;
  focusSeq?: number;
  onFocusIn?: () => void;
}) {
  const look = currentTheme;
  const { machine, session_id: sessionId } = props.id;
  const status = signal<"idle" | "connecting" | "live" | "failed">("connecting");
  const connectError = signal<string | null>(null);
  const reconnecting = signal(false);
  const retrySeq = signal(0);
  const localFocusSeq = signal(0);
  const sendTextProp = signal<{ seq: number; text: string } | null>(null);

  // 搜索条状态（Cmd+F 打开，Enter / Shift+Enter 跳转，Esc 关闭并归还焦点）
  const searchOpen = signal(false);
  const searchQuery = signal("");
  const searchStatus = signal<{ current: number; total: number }>({ current: 0, total: 0 });
  const searchStepProp = signal<{ seq: number; query: string; backward: boolean } | null>(null);
  let nextSearchStepSeq = 0;
  let searchInputEl: any = null;

  // 右键菜单状态
  const selection = signal<string | null>(null);
  const clipboardText = signal<string | null>(null);
  const contributedItems = signal<ContributedMenuItem[]>([]);
  const menuFailure = signal<string | null>(null);

  const effectiveFocusSeq = computed(() => (props.focusSeq ?? 0) + localFocusSeq());

  const searchQueryProp = stableMemo(() => ({
    open: searchOpen(),
    query: searchQuery(),
  }));

  const runtimeName = computed(() => {
    const entry = machineMetaEntry(machine);
    if (entry?.name) return String(entry.name);
    return typeof machine === "string" ? machine : "";
  });

  const closeSearch = () => {
    searchOpen.set(false);
    searchQuery.set("");
    searchStatus.set({ current: 0, total: 0 });
    localFocusSeq.set(localFocusSeq() + 1);
  };

  const stepSearch = (backward: boolean) => {
    const q = searchQuery();
    searchStepProp.set({ seq: ++nextSearchStepSeq, query: q, backward });
  };

  const loadContextMenu = async () => {
    const [clip, items] = await Promise.all([
      clipboardRead().catch(() => null),
      contextMenuItems(TERMINAL_MENU, machine, {
        machine,
        session_id: sessionId,
        selection: selection(),
      }).catch(() => []),
    ]);
    clipboardText.set(typeof clip === "string" && clip.length > 0 ? clip : null);
    contributedItems.set(Array.isArray(items) ? (items as ContributedMenuItem[]) : []);
  };

  const runContributed = (item: ContributedMenuItem) => {
    if (!item.command) return;
    menuFailure.set(null);
    void run(item.command, item.args ?? null).catch((error) => {
      menuFailure.set(t("error.failed", { what: item.label || item.command, error: errText(error) }));
    });
  };

  cleanup(
    registerOpenTerminal(terminalPaneKey(machine, sessionId), (chunk) => {
      sendTextProp.set({ seq: ++nextSendSeq, text: chunk });
    }),
  );

  return (
    <>
      <SmeltContextMenu
        popupStyle={{ minWidth: 168 }}
        onOpenChange={({ open }: { open: boolean }) => {
          if (open) void loadContextMenu();
        }}
        trigger={
          <View style={{ flex: 1, width: "100%", height: "100%", minWidth: 0, minHeight: 0 }}>
            <SmeltNativeTerminal
              spec={props.spec}
              focusSeq={effectiveFocusSeq()}
              retrySeq={retrySeq()}
              sendText={sendTextProp()}
              searchQuery={searchQueryProp()}
              searchStep={searchStepProp()}
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
              onSearchOpen={() => {
                if (searchOpen()) {
                  searchInputEl?.focus?.();
                } else {
                  searchStatus.set({ current: 0, total: 0 });
                  searchOpen.set(true);
                }
              }}
              onSearchStatus={(ev: { current?: number; total?: number }) => {
                searchStatus.set({
                  current: typeof ev?.current === "number" ? ev.current : 0,
                  total: typeof ev?.total === "number" ? ev.total : 0,
                });
              }}
              onSelectionChange={(ev: { text?: string | null }) => {
                selection.set(typeof ev?.text === "string" && ev.text.length > 0 ? ev.text : null);
              }}
              style={{ flex: 1, width: "100%", height: "100%" }}
            />
          </View>
        }
      >
        <SmeltMenuItem
          label={t("terminal.copy")}
          disabled={selection() == null}
          onSelect={() => {
            const text = selection();
            if (text) void clipboardWrite(text).catch(() => {});
          }}
        />
        <SmeltMenuItem
          label={t("terminal.paste")}
          disabled={clipboardText() == null}
          onSelect={() => {
            const text = clipboardText();
            if (text != null) {
              sendTextProp.set({ seq: ++nextSendSeq, text });
            }
          }}
        />
        <If when={contributedItems().length > 0}>
          <SmeltMenuSeparator />
          <ContributedEntries items={contributedItems()} onRun={runContributed} />
        </If>
      </SmeltContextMenu>

      {/* Cmd+F 终端内搜索条：叠在右上角，不抢网格布局 */}
      <If when={searchOpen()}>
        <View
          class="shadow-floating"
          onPressDown={(e: any) => e?.stopPropagation?.()}
          style={{
            position: "absolute",
            top: 8,
            right: 8,
            width: 300,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 6,
            paddingLeft: 8,
            paddingRight: 8,
            paddingTop: 4,
            paddingBottom: 4,
            borderRadius: look().radius.md,
            backgroundColor: look().palette.surface,
            borderWidth: 1,
            borderColor: look().palette.hair,
          }}
        >
          <Input
            ref={(el: any) => {
              searchInputEl = el ?? null;
              el?.focus?.();
            }}
            autoFocus={true}
            value={searchQuery()}
            placeholder={t("terminal.search_placeholder")}
            accessibilityLabel={t("terminal.search_placeholder")}
            onChange={({ value }: { value: string }) => searchQuery.set(value)}
            captureKeys={["enter", "shift+enter", "escape"]}
            onKeyDown={(ev: any) => {
              if (ev?.key === "Escape") {
                closeSearch();
              } else if (ev?.key === "Enter") {
                stepSearch(Boolean(ev?.modifiers?.shift));
              }
            }}
            class="flex-1 min-w-0 rounded-(--radius) border border-input bg-card px-2 text-foreground"
            style={{
              height: 24,
              fontSize: look().type.ui_compact.size,
              lineHeight: look().type.ui_compact.line_height,
            }}
          />
          <If when={searchQuery().length > 0}>
            <SmeltButton
              variant="quiet"
              size="sm"
              icon="icons/x.svg"
              accessibilityLabel={t("split.cancel")}
              onPress={() => {
                searchQuery.set("");
                searchInputEl?.focus?.();
              }}
            />
          </If>
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: look().palette.muted,
              flexShrink: 0,
            }}
          >
            {searchStatus().total === 0
              ? t("terminal.search_none")
              : `${searchStatus().current}/${searchStatus().total}`}
          </Text>
        </View>
      </If>

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
            size="sm"
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
            borderRadius: look().radius.md,
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

      {/* 右键菜单命令失败提示：挂在右下角，悬停看原因，点一下收起 */}
      <If when={menuFailure() != null}>
        <View
          class="shadow-floating"
          onPressDown={(e: any) => e?.stopPropagation?.()}
          tooltip={
            <SmeltTooltip>
              <Text
                style={{
                  color: look().palette.text,
                  fontSize: look().type.text_xsmall.size,
                  lineHeight: look().type.text_xsmall.line_height,
                }}
              >
                {menuFailure() ?? ""}
              </Text>
            </SmeltTooltip>
          }
          style={{
            position: "absolute",
            bottom: 8,
            right: 16,
            borderRadius: look().radius.md,
            backgroundColor: look().palette.surface,
            borderWidth: 1,
            borderColor: look().palette.hair,
          }}
        >
          <SmeltButton
            variant="danger-quiet"
            size="sm"
            icon="icons/circle-alert.svg"
            accessibilityLabel={menuFailure() ?? t("split.dismiss_failure")}
            onPress={() => menuFailure.set(null)}
          />
        </View>
      </If>
    </>
  );
}
