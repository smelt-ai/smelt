// 账户额度（NSH A2 独立 VM 版本）：复用纯逻辑 model.js 与 contract.js，使用 NSH <Canvas> 画抗锯齿额度环。
import { Canvas, computed, Scroll, signal, Text, View } from "nsh";
import { Popover, Switch, Tooltip } from "nsh/base";
import {
  call,
  initHost,
  onEvent,
  quotaFacts,
  quotaTarget,
  registerViews,
  safeArea,
  SmeltButton,
  SmeltIcon,
  state as hostState,
  t,
  text,
  theme,
  TOOLTIP_CLASS,
  viewShown,
  waitOnline,
} from "smelt/sdk";

import { BAR, BAR_NAME, SETTINGS_NAME, SETTINGS_VIEW } from "./contract.js";
import {
  REFRESH_MS,
  REFRESH_TIMEOUT_MS,
  apply,
  begin_refresh,
  chips,
  decorate_head,
  empty_state,
  enabled_targets,
  next_manual,
  popover_entries,
  restore,
  ring_fraction,
  ring_tone,
  setting_row,
} from "./model.js";

const LOCAL = null;
const POPOVER_WIDTH = 268;
const NAME_WIDTH = 84;
const ROW_SM = 28;

const modelHost = {
  quota_target: quotaTarget,
  quota_facts: quotaFacts,
  t,
};

const quotaState = empty_state();
const version = signal(0);
const settingsState = signal<{ providers: any[]; error: string | null }>({
  providers: [],
  error: null,
});

const queue: string[] = [];
let wake: (() => void) | null = null;
let settingsWasShown = false;
let loopStarted = false;

function changed() {
  version.set(version() + 1);
}

function msgOf(error: any): string {
  return error?.message ?? String(error);
}

function requestRefresh(id: string) {
  queue.push(id);
  wake?.();
}

async function nextRound(): Promise<string | null> {
  if (queue.length === 0) {
    let timer: any = null;
    await new Promise<void>((resolve) => {
      wake = resolve;
      // 轮询例外：额度在远端账户上变化，没有事件可等；周期 5 分钟，用户关掉全部账户后不再醒。
      if (quotaState.targets.length > 0) {
        timer = setTimeout(() => resolve(), REFRESH_MS);
      }
    });
    wake = null;
    if (timer != null) clearTimeout(timer);
  }
  return next_manual(queue);
}

async function refreshLoop() {
  let failures = 0;
  let cached: any;
  for (;;) {
    if (!(await waitOnline(LOCAL, failures))) return;
    try {
      cached = await call(LOCAL, "quota.get");
      break;
    } catch (error) {
      console.error(`[quota] 读额度缓存失败：${msgOf(error)}`);
    }
    failures += 1;
  }
  restore(quotaState, cached.targets, cached.statuses);
  changed();
  let first = true;
  for (;;) {
    const target = first ? null : await nextRound();
    first = false;
    begin_refresh(quotaState, target);
    changed();
    try {
      const result = await call(LOCAL, "quota.refresh", { target }, REFRESH_TIMEOUT_MS);
      if (target == null) restore(quotaState, result.targets, []);
      apply(quotaState, result.statuses);
    } catch (error) {
      console.error(`[quota] 刷新额度失败：${msgOf(error)}`);
      apply(quotaState, []);
    }
    changed();
    if (viewShown(SETTINGS_VIEW)) refreshSettings();
  }
}

function refreshSettings() {
  call(LOCAL, "quota.settings.get")
    .then((result: any) => {
      settingsState.set({ providers: result.providers ?? [], error: null });
    })
    .catch((error: any) => {
      settingsState.set({ ...settingsState(), error: msgOf(error) });
    });
}

function setEnabled(id: string, enabled: boolean) {
  call(LOCAL, "quota.set_enabled", { target: id, enabled })
    .then((result: any) => {
      restore(quotaState, enabled_targets(result.providers ?? []), []);
      settingsState.set({ providers: result.providers ?? [], error: null });
      changed();
      if (enabled) requestRefresh(id);
    })
    .catch((error: any) => {
      settingsState.set({ ...settingsState(), error: msgOf(error) });
    });
}

onEvent((event) => {
  if (event?.type === "shown_views_changed") {
    const shown = viewShown(SETTINGS_VIEW);
    if (shown && !settingsWasShown) refreshSettings();
    settingsWasShown = shown;
  }
});

initHost().then(() => {
  registerViews([
    {
      name: BAR_NAME,
      export: BAR_NAME,
      title: text("title"),
      icon: "icons/gauge.svg",
      draws_top_bar: true,
    },
    {
      name: SETTINGS_NAME,
      export: SETTINGS_NAME,
      title: text("title"),
      icon: "icons/gauge.svg",
      settings: {
        group: "agents",
        keywords: ["quota", "usage", "limit", "keychain", "额度", "用量", "钥匙串"],
      },
    },
  ]).catch((error) => console.error("[quota] registerViews 失败:", error));

  if (!loopStarted) {
    loopStarted = true;
    refreshLoop().catch((error) => console.error(`[quota] 刷新循环停了：${msgOf(error)}`));
  }
});

function RingsCanvas(props: { rings: any[]; palette: any }) {
  const size = 26;
  const center = size / 2;
  const start = -Math.PI / 2;

  return (
    <Canvas
      style={{
        position: "absolute",
        top: 0,
        left: 0,
        width: size,
        height: size,
        pointerEvents: "none",
      }}
      draw={(cx: any) => {
        const rings = props.rings;
        const palette = props.palette;
        const stroke = rings.length > 1 ? 1.75 : 2;
        const outer = rings.length > 1 ? 12.125 : 10;
        for (let index = 0; index < rings.length; index++) {
          const ring = rings[index];
          const radius = outer - index * 3.5;
          cx.lineWidth = stroke;
          cx.lineCap = "round";
          cx.strokeStyle = palette.hair;
          cx.beginPath();
          cx.arc(center, center, radius, 0, Math.PI * 2);
          cx.stroke();

          const fraction = ring_fraction(ring.used);
          if (fraction <= 0) continue;
          cx.strokeStyle = palette[ring_tone(ring.used)];
          cx.beginPath();
          cx.arc(center, center, radius, start, start + Math.min(fraction, 1) * Math.PI * 2);
          cx.stroke();
        }
      }}
    />
  );
}

export function bar() {
  const popoverOpen = signal(false);
  const look = theme;
  const insets = computed(() => safeArea(BAR).bar_insets);
  const data = computed(() => {
    version();
    hostState();
    const now = Date.now();
    return {
      shown: chips(quotaState, modelHost, now),
      rows: popover_entries(quotaState, modelHost, now),
      loading: quotaState.refreshing.size > 0,
    };
  });

  return (
    <View
      style={{
        width: "100%",
        height: "100%",
        minWidth: 0,
        paddingLeft: insets().left,
        paddingRight: insets().right,
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        justifyContent: "flex-end",
      }}
    >
      <If when={data().rows.length > 0}>
        <Popover
          open={popoverOpen()}
          onOpenChange={({ open }: any) => popoverOpen.set(Boolean(open))}
          placement="top"
          align="end"
          offset={6}
          style={{ minWidth: 0, flexShrink: 1, maxWidth: "100%" }}
          popupClass="rounded-xl border border-border bg-popover p-1 shadow-lg"
          popupStyle={{ width: POPOVER_WIDTH }}
          trigger={
            <View
              class="hover:bg-accent"
              style={{
                minWidth: 0,
                paddingLeft: 6,
                paddingRight: 6,
                paddingTop: 4,
                paddingBottom: 4,
                borderRadius: look().radius.row,
                backgroundColor: popoverOpen() ? look().palette.row_selected : "transparent",
                display: "flex",
                flexDirection: "row",
                flexWrap: "wrap",
                justifyContent: "flex-end",
                alignItems: "center",
                gap: 6,
              }}
              tooltip={
                data().shown.length === 0 ? (
                  <Tooltip class={TOOLTIP_CLASS}>
                    <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.text }}>
                      {t(data().loading ? "loading" : "empty_trigger")}
                    </Text>
                  </Tooltip>
                ) : undefined
              }
            >
              <If
                when={data().shown.length === 0}
                else={
                  <Each of={data().shown} key={(chip: any) => chip.id}>
                    {(chip: () => any) => {
                      const p = () => look().palette;
                      const tip = () =>
                        chip().refreshing
                          ? decorate_head(chip().hint, (h: string) => t("refreshing", { hint: h }))
                          : chip().hint;
                      const iconSize = () => (chip().rings.length > 1 ? 9 : 11);
                      return (
                        <View
                          testID={`quota-chip-${chip().id}`}
                          accessible={true}
                          accessibilityLabel={chip().name}
                          style={{
                            display: "flex",
                            flexDirection: "row",
                            flexShrink: 0,
                            alignItems: "center",
                            gap: 4,
                          }}
                          tooltip={
                            <Tooltip class={TOOLTIP_CLASS}>
                              <Text style={{ fontSize: look().type.text_xsmall.size, color: p().text }}>
                                {tip()}
                              </Text>
                            </Tooltip>
                          }
                        >
                          <View
                            style={{
                              position: "relative",
                              width: 26,
                              height: 26,
                              display: "flex",
                              alignItems: "center",
                              justifyContent: "center",
                            }}
                          >
                            <If when={chip().rings.length > 0}>
                              <RingsCanvas rings={chip().rings} palette={p()} />
                            </If>
                            <SmeltIcon
                              path={chip().refreshing ? undefined : chip().icon}
                              spin={chip().refreshing}
                              size={chip().refreshing ? 12 : iconSize()}
                              color={chip().refreshing ? p()[chip().tone] : p().text}
                            />
                          </View>
                          <If when={chip().rings.length === 0}>
                            <Text
                              style={{
                                fontSize: look().type.text_xsmall.size,
                                fontFamily: look().fonts?.mono,
                                color: p().text,
                              }}
                            >
                              {chip().text}
                            </Text>
                          </If>
                        </View>
                      );
                    }}
                  </Each>
                }
              >
                <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 4 }}>
                  <SmeltIcon path="icons/gauge.svg" size={14} color={look().palette.muted} />
                  <If when={data().loading}>
                    <Text
                      style={{
                        fontSize: look().type.text_xsmall.size,
                        color: look().palette.faint,
                      }}
                    >
                      {t("loading")}
                    </Text>
                  </If>
                </View>
              </If>
            </View>
          }
        >
          <View style={{ width: "100%", display: "flex", flexDirection: "column" }}>
            <View
              style={{
                height: ROW_SM,
                paddingLeft: 8,
                paddingRight: 8,
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
              }}
            >
              <Text
                style={{
                  fontSize: look().type.text_xsmall_medium.size,
                  fontWeight: look().type.text_xsmall_medium.weight,
                  color: look().palette.muted,
                }}
              >
                {t("title")}
              </Text>
            </View>

            <Each
              of={data().rows}
              key={(entry: any) => (entry.kind === "ready" ? entry.chip.id : entry.id)}
            >
              {(entry: () => any) => {
                const p = () => look().palette;
                const row = () => {
                  const e = entry();
                  const pal = p();
                  return e.kind === "ready"
                    ? {
                        id: e.chip.id,
                        name: e.chip.name,
                        icon: e.chip.icon,
                        color: pal[e.chip.tone],
                        text: e.chip.text,
                        used: e.chip.used,
                        refreshing: e.chip.refreshing,
                        nameColor: pal.text,
                        tip: decorate_head(e.chip.hint, (hint: string) =>
                          e.chip.refreshing
                            ? t("refreshing", { hint })
                            : t("click_to_refresh", { hint }),
                        ),
                      }
                    : {
                        id: e.id,
                        name: e.name,
                        icon: e.icon,
                        color: e.error != null ? pal.need : pal.faint,
                        text: "—",
                        used: 0,
                        refreshing: e.refreshing,
                        nameColor: pal.muted,
                        tip: e.refreshing
                          ? t("refreshing", { hint: e.name })
                          : e.error != null
                            ? t("click_to_retry", { error: e.error })
                            : t("click_to_check"),
                      };
                };
                const usedPct = () => Math.min(Math.max(row().used, 0), 100);
                return (
                  <View
                    class="hover:bg-accent"
                    style={{
                      height: 30,
                      paddingLeft: 8,
                      paddingRight: 8,
                      borderRadius: 6,
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 8,
                    }}
                    tooltip={
                      <Tooltip class={TOOLTIP_CLASS}>
                        <Text style={{ fontSize: look().type.text_xsmall.size, color: p().text }}>
                          {row().tip}
                        </Text>
                      </Tooltip>
                    }
                    onPress={() => requestRefresh(row().id)}
                  >
                    <SmeltIcon path={row().icon} size={14} color={row().color} />
                    <Text
                      style={{
                        width: NAME_WIDTH,
                        flexShrink: 0,
                        fontSize: look().type.text_xsmall.size,
                        color: row().nameColor,
                        numberOfLines: 1,
                      }}
                    >
                      {row().name}
                    </Text>
                    <View
                      style={{
                        flex: 1,
                        height: 4,
                        borderRadius: 2,
                        overflow: "hidden",
                        backgroundColor: p().hair,
                      }}
                    >
                      {usedPct() > 0 ? (
                        <View
                          style={{
                            height: "100%",
                            width: `${usedPct()}%`,
                            borderRadius: 2,
                            backgroundColor: row().color,
                          }}
                        />
                      ) : null}
                    </View>
                    <View
                      style={{
                        width: 36,
                        flexShrink: 0,
                        display: "flex",
                        flexDirection: "row",
                        justifyContent: "flex-end",
                        alignItems: "center",
                      }}
                    >
                      {row().refreshing ? (
                        <SmeltIcon spin={true} size={12} color={row().color} />
                      ) : (
                        <Text
                          style={{
                            fontSize: look().type.text_xsmall.size,
                            fontFamily: look().fonts?.mono,
                            color: row().color,
                          }}
                        >
                          {row().text}
                        </Text>
                      )}
                    </View>
                  </View>
                );
              }}
            </Each>
          </View>
        </Popover>
      </If>
    </View>
  );
}

export function settings() {
  settingsWasShown = viewShown(SETTINGS_VIEW);
  refreshSettings();
  const look = theme;
  const current = computed(() => {
    version();
    return settingsState();
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
          gap: 16,
        }}
      >
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
          {current().error != null ? (
            <View
              style={{
                padding: 14,
                borderBottomWidth: 1,
                borderBottomColor: look().palette.hair,
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 12,
              }}
            >
              <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                {t("read_failed")}
              </Text>
              <Text
                style={{
                  maxWidth: 320,
                  fontSize: look().type.text_xsmall.size,
                  color: look().palette.muted,
                  numberOfLines: 1,
                }}
              >
                {current().error}
              </Text>
            </View>
          ) : null}

          <Each of={current().providers} key={(p: any) => p.target}>
            {(provider: () => any) => {
              const target = () => quotaTarget(provider().target);
              const facts = () => {
                version();
                return setting_row(quotaState, provider());
              };
              const p = () => look().palette;
              const desc = () =>
                facts().denied ? t("keychain_denied") : facts().enabled ? facts().error : null;
              return target() ? (
                <View
                  style={{
                    paddingLeft: 14,
                    paddingRight: 14,
                    paddingTop: 12,
                    paddingBottom: 12,
                    borderBottomWidth: 1,
                    borderBottomColor: p().hair,
                    display: "flex",
                    flexDirection: "row",
                    alignItems: "center",
                    gap: 12,
                  }}
                >
                  <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
                    <Text style={{ fontSize: look().type.ui.size, color: p().text }}>
                      {target()!.name}
                    </Text>
                    {desc() ? (
                      <Text
                        style={{
                          fontSize: look().type.text_xsmall.size,
                          color: p().muted,
                          numberOfLines: 2,
                        }}
                      >
                        {desc()}
                      </Text>
                    ) : null}
                  </View>

                  {facts().denied ? (
                    <SmeltButton
                      quiet={true}
                      small={true}
                      loading={facts().refreshing}
                      label={t("retry")}
                      onPress={() => requestRefresh(facts().id)}
                    />
                  ) : facts().enabled ? (
                    <SmeltButton
                      quiet={true}
                      small={true}
                      loading={facts().refreshing}
                      icon="icons/refresh.svg"
                      tooltip={t("refresh_one", { name: target()!.name })}
                      onPress={() => requestRefresh(facts().id)}
                    />
                  ) : null}

                  <Switch
                    checked={facts().enabled}
                    accessibilityLabel={t("enable_label", { name: target()!.name })}
                    onChange={({ checked }: any) => setEnabled(facts().id, Boolean(checked))}
                    trackStyle={{
                      width: 34,
                      height: 20,
                      borderRadius: 10,
                      padding: 2,
                      backgroundColor: facts().enabled ? p().link : p().soft,
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      justifyContent: facts().enabled ? "flex-end" : "flex-start",
                    }}
                    thumbStyle={{
                      width: 16,
                      height: 16,
                      borderRadius: 8,
                      backgroundColor: p().on_ink,
                    }}
                  />
                </View>
              ) : null;
            }}
          </Each>
        </View>
      </View>
    </Scroll>
  );
}

export default bar;
