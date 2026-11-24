// 资源占用（来源 `smelt.resources`）：设置「系统 → 资源占用」页。
//
// 桌面进程（GPUI）通过宿主 `processUsage()` 在本进程里量；各台机器的运行时与会话通过
// `call(machine, "resource.usage", {}, 5000)` 并发拉取，再按工作区目录把守护会话归拢到
// 侧栏对话。
//
// 采样严格绑定可见性（`viewShown("smelt.resources/main")`）：
// - 不可见时立即清除所有定时器，零后台轮询，完全睡在 `shown_views_changed` 事件上；
// - 从隐藏变为可见时立即 `resetMonitor` 清空旧数字与旧曲线，先跑一轮垫底采样，等 500ms 后
//   每 2s 采一轮；
// - 可见期间收到 `sessions_changed` / `machines_changed` / `locale_changed` 立即重建快照。

import { cleanup, computed, effect, Scroll, signal, Text, View } from "nsh";
import { Button as BaseButton, Collapsible } from "nsh/base";
import {
  bootTask,
  call,
  initHost,
  locale,
  machineMeta,
  machines,
  onEvent,
  processUsage,
  registerViews,
  run,
  SmeltButton,
  SmeltDisclosureChevron,
  SmeltVector,
  t,
  text,
  theme,
  viewShown,
} from "smelt/sdk";

import {
  applySample,
  buildAreaChartSvg,
  buildSnapshot,
  CALL_TIMEOUT_MS,
  createMonitor,
  formatBytes,
  formatCpu,
  INTERVAL_MS,
  PRIME_DELAY_MS,
  processSummary,
  resetMonitor,
  SORT_CPU,
  SORT_MEMORY,
  sortGroups,
} from "./model.js";

const VIEW_NAME = "main";
const VIEW = "smelt.resources/main";
const ACTIVATE_COMMAND = "smelt.sessions/activate";

const KEYWORDS = [
  "资源",
  "性能",
  "内存",
  "内存占用",
  "CPU",
  "占用",
  "卡",
  "慢",
  "活动监视器",
  "GPUI",
  "resources",
  "performance",
  "memory",
  "slow",
  "Activity Monitor",
];

const CPU_COLUMN = 64;
const MEMORY_COLUMN = 80;
const DISCLOSURE_COLUMN = 20;
const SORT_HEADER_PAD = 4;

function errText(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error ?? "");
}

// ---- 页面状态与事件驱动采样循环 ----

const monitor = createMonitor();
const snapshot = signal(buildSnapshot(monitor, [], t));
const sortBy = signal<string>(SORT_MEMORY);
const expanded = signal<Set<string>>(new Set());

let loopVisible = false;
let loopGen = 0;
let pendingTimer: ReturnType<typeof setTimeout> | null = null;
let cancelPendingSleep: (() => void) | null = null;

function rebuildSnapshot() {
  snapshot.set(buildSnapshot(monitor, machines(), t));
}

function toggleRow(id: string) {
  const next = new Set(expanded());
  if (next.has(id)) {
    next.delete(id);
  } else {
    next.add(id);
  }
  expanded.set(next);
}

function jumpToSession(machine: string | null, sessionId: string) {
  void run(ACTIVATE_COMMAND, { machine: machine ?? null, session_id: sessionId }).catch(
    (error) => {
      console.error("[smelt.resources] 跳到会话失败：", errText(error));
    },
  );
}

function clearLoopTimer() {
  if (pendingTimer !== null) {
    clearTimeout(pendingTimer);
    pendingTimer = null;
  }
  if (cancelPendingSleep !== null) {
    const cancel = cancelPendingSleep;
    cancelPendingSleep = null;
    cancel();
  }
}

function stopLoop() {
  loopVisible = false;
  loopGen += 1;
  clearLoopTimer();
}

function sleepWhileVisible(ms: number, gen: number): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    if (gen !== loopGen || !loopVisible) {
      resolve(false);
      return;
    }
    clearLoopTimer();
    let settled = false;
    const finish = (ok: boolean) => {
      if (settled) return;
      settled = true;
      if (pendingTimer !== null) {
        clearTimeout(pendingTimer);
        pendingTimer = null;
      }
      if (cancelPendingSleep === cancel) {
        cancelPendingSleep = null;
      }
      resolve(ok);
    };
    const cancel = () => finish(false);
    cancelPendingSleep = cancel;
    // 轮询例外：CPU 和内存占用没有「变了」的事件，只能定时采；只在这一页看得见时转，
    // 看不见时立刻 clearTimeout 睡在 shown_views_changed 上；周期 2s，上限是这一页开着的时长。
    pendingTimer = setTimeout(() => {
      pendingTimer = null;
      finish(gen === loopGen && loopVisible);
    }, ms);
  });
}

async function sampleRound() {
  const targets = machineMeta().filter(
    (machine: any) => (machine.machine ?? null) === null || machine.unavailable == null,
  );
  const desktopPromise = processUsage().catch(() => null);
  const runtimePromises = targets.map(async (machine: any) => {
    const key: string | null = machine.machine ?? null;
    try {
      const result = await call(key, "resource.usage", {}, CALL_TIMEOUT_MS);
      return { machine: key, ok: true, result };
    } catch (error) {
      return { machine: key, ok: false, error: errText(error) };
    }
  });
  const [desktop, runtimes] = await Promise.all([
    desktopPromise,
    Promise.all(runtimePromises),
  ]);
  return { desktop, runtimes };
}

async function runSamplingLoop(gen: number) {
  let primed = false;
  for (;;) {
    if (gen !== loopGen || !loopVisible || !viewShown(VIEW)) {
      if (gen === loopGen) stopLoop();
      return;
    }
    const { desktop, runtimes } = await sampleRound();
    if (gen !== loopGen || !loopVisible || !viewShown(VIEW)) {
      if (gen === loopGen) stopLoop();
      return;
    }
    if (!primed) {
      // CPU 是跟上一次采样比的，上一次可能是很久以前，比出来是那一大段的平均。
      // 这一轮只当垫底，隔 500ms 再采第一轮真的。
      primed = true;
      const keepGoing = await sleepWhileVisible(PRIME_DELAY_MS, gen);
      if (!keepGoing) return;
      continue;
    }
    applySample(monitor, desktop, runtimes, Date.now());
    rebuildSnapshot();
    const keepGoing = await sleepWhileVisible(INTERVAL_MS, gen);
    if (!keepGoing) return;
  }
}

function startLoop() {
  if (loopVisible) return;
  loopVisible = true;
  const gen = ++loopGen;
  clearLoopTimer();
  // 重新露出来：上次看到的数和曲线作废，马上换掉，不留到第一轮真的采完。
  resetMonitor(monitor);
  rebuildSnapshot();
  void runSamplingLoop(gen);
}

function syncVisibility() {
  if (viewShown(VIEW)) {
    startLoop();
  } else {
    stopLoop();
  }
}

onEvent((event) => {
  if (event?.type === "shown_views_changed") {
    syncVisibility();
  } else if (
    (event?.type === "sessions_changed" ||
      event?.type === "machines_changed" ||
      event?.type === "locale_changed") &&
    viewShown(VIEW)
  ) {
    rebuildSnapshot();
  }
}, ["shown_views_changed", "sessions_changed", "machines_changed", "locale_changed"]);

bootTask(async () => {
  await initHost(["views", "machines_meta", "sessions_data"]);
  await registerViews([
    {
      name: VIEW_NAME,
      export: VIEW_NAME,
      title: text("page.title"),
      icon: "icons/memory-stick.svg",
      draws_top_bar: false,
      instances: false,
      settings: {
        group: "system",
        keywords: KEYWORDS,
      },
    },
  ]);
  rebuildSnapshot();
  syncVisibility();
});

// ---- 界面组件 ----

function SectionCard(props: { title: string; description?: string; children?: any }) {
  const look = theme;
  return (
    <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <View
        style={{
          paddingLeft: 16,
          paddingRight: 16,
          display: "flex",
          flexDirection: "column",
          gap: 2,
        }}
      >
        <Text
          style={{
            fontSize: look().type.ui_medium.size,
            lineHeight: look().type.ui_medium.line_height,
            fontWeight: look().type.ui_medium.weight,
            color: look().palette.muted,
          }}
        >
          {props.title}
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
      <View
        style={{
          borderRadius: look().radius.lg ?? 8,
          borderWidth: 1,
          borderColor: look().palette.hair,
          backgroundColor: look().palette.surface,
          paddingLeft: 16,
          paddingRight: 16,
          paddingTop: 12,
          paddingBottom: 12,
          display: "flex",
          flexDirection: "column",
        }}
      >
        {props.children}
      </View>
    </View>
  );
}

function Disclosure(props: { open: boolean }) {
  const look = theme;
  return (
    <View
      style={{
        width: DISCLOSURE_COLUMN,
        flexShrink: 0,
        display: "flex",
        alignItems: "center",
        justifyContent: "flex-start",
      }}
    >
      <SmeltDisclosureChevron
        open={props.open}
        size={14}
        color={look().palette.muted}
      />
    </View>
  );
}

function UsageColumns(props: {
  cpuPercent: number;
  memoryBytes: number;
  color: string;
}) {
  const look = theme;
  return (
    <View
      style={{
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        flexShrink: 0,
      }}
    >
      <Text
        style={{
          width: CPU_COLUMN,
          textAlign: "right",
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
          color: props.color,
          fontVariant: ["tabular-nums"],
          numberOfLines: 1,
        }}
      >
        {formatCpu(props.cpuPercent)}
      </Text>
      <Text
        style={{
          width: MEMORY_COLUMN,
          textAlign: "right",
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
          color: props.color,
          fontVariant: ["tabular-nums"],
          numberOfLines: 1,
        }}
      >
        {formatBytes(props.memoryBytes)}
      </Text>
    </View>
  );
}

function RowLabel(props: { label: string; detail?: string | null }) {
  const look = theme;
  return (
    <View
      style={{
        flex: 1,
        minWidth: 0,
        display: "flex",
        flexDirection: "column",
      }}
    >
      <Text
        style={{
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
          color: look().palette.text,
          numberOfLines: 1,
        }}
      >
        {props.label}
      </Text>
      <If when={Boolean(props.detail)}>
        <Text
          style={{
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            color: look().palette.muted,
            numberOfLines: 1,
          }}
        >
          {props.detail ?? ""}
        </Text>
      </If>
    </View>
  );
}

function SortHeader(props: { label: string; column: string; width: number }) {
  const look = theme;
  const active = computed(() => sortBy() === props.column);
  return (
    <BaseButton
      onPress={() => sortBy.set(props.column)}
      style={{
        width: props.width + 2 * SORT_HEADER_PAD,
        marginLeft: -SORT_HEADER_PAD,
        marginRight: -SORT_HEADER_PAD,
        paddingLeft: SORT_HEADER_PAD,
        paddingRight: SORT_HEADER_PAD,
        paddingTop: 2,
        paddingBottom: 2,
        borderRadius: look().radius.sm ?? 4,
        backgroundColor: "transparent",
        display: "flex",
        flexDirection: "row",
        justifyContent: "flex-end",
        alignItems: "center",
        flexShrink: 0,
        cursor: "default",
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
          fontSize: look().type.text_xsmall.size,
          lineHeight: look().type.text_xsmall.line_height,
          fontWeight: active() ? look().type.ui_medium.weight : look().type.text_xsmall.weight,
          color: active() ? look().palette.text : look().palette.muted,
          textAlign: "right",
          numberOfLines: 1,
        }}
      >
        {active() ? `${props.label} ↓` : props.label}
      </Text>
    </BaseButton>
  );
}

function SummarySection() {
  const look = theme;
  const snap = computed(() => snapshot());
  const local = computed(() => snap().local);
  const shareText = computed(() => {
    locale();
    const cur = local();
    const total = snap().totalMemory;
    if (!cur || total == null || total <= 0) return null;
    return t("summary.share", {
      total: formatBytes(total),
      percent: ((cur.memory_bytes * 100) / total).toFixed(0),
    });
  });
  const svgMarkup = computed(() => {
    const chart = snap().chart;
    if (!chart || chart.length < 2) return "";
    const p = look().palette;
    return buildAreaChartSvg(chart, {
      width: 600,
      height: 96,
      stroke: p.muted,
      fill: p.muted,
      strokeOpacity: 0.9,
      fillOpacity: 0.16,
      strokeWidth: 1.5,
    });
  });

  return (
    <SectionCard title={t("summary.title")} description={t("summary.refresh")}>
      <If
        when={local() != null}
        else={
          <Text
            style={{
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
              color: look().palette.muted,
            }}
          >
            {t("loading")}
          </Text>
        }
      >
        <View style={{ display: "flex", flexDirection: "column", width: "100%", gap: 8 }}>
          <View
            style={{
              display: "flex",
              flexDirection: "row",
              alignItems: "baseline",
              flexWrap: "wrap",
              gap: 12,
            }}
          >
            <Text
              style={{
                fontSize: 24,
                lineHeight: 30,
                fontWeight: 600,
                color: look().palette.text,
                fontVariant: ["tabular-nums"],
              }}
            >
              {formatBytes(local()?.memory_bytes ?? 0)}
            </Text>
            <Text
              style={{
                fontSize: look().type.ui.size,
                lineHeight: look().type.ui.line_height,
                color: look().palette.text,
                fontVariant: ["tabular-nums"],
              }}
            >
              {`CPU ${formatCpu(local()?.cpu_percent ?? 0)}`}
            </Text>
            <If when={shareText() != null}>
              <Text
                style={{
                  fontSize: look().type.ui.size,
                  lineHeight: look().type.ui.line_height,
                  color: look().palette.muted,
                  fontVariant: ["tabular-nums"],
                }}
              >
                {shareText() ?? ""}
              </Text>
            </If>
          </View>

          <If when={svgMarkup().length > 0}>
            <View style={{ width: "100%", height: 96 }}>
              <SmeltVector markup={svgMarkup()} height={96} style={{ width: "100%" }} />
            </View>
          </If>

          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: look().palette.muted,
            }}
          >
            {t("chart.note")}
          </Text>
        </View>
      </If>
    </SectionCard>
  );
}

function MachineSection(props: { section: () => any }) {
  const look = theme;
  const displayGroups = computed(() => {
    const sec = props.section();
    const sorted = sortGroups(sec.sessions, sortBy());
    const rows = sorted.map((group: any) => ({
      id: `${sec.idPrefix}-${group.jump}`,
      group,
    }));
    if (sec.others) {
      rows.push({
        id: `${sec.idPrefix}-others`,
        group: sec.others,
      });
    }
    return rows;
  });

  return (
    <SectionCard title={props.section().name}>
      <View style={{ display: "flex", flexDirection: "column", width: "100%" }}>
        {/* 列头 */}
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            paddingLeft: 8,
            paddingRight: 8,
            paddingBottom: 4,
            gap: 12,
          }}
        >
          <View style={{ width: DISCLOSURE_COLUMN, flexShrink: 0 }} />
          <Text
            style={{
              flex: 1,
              minWidth: 0,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: look().palette.muted,
              numberOfLines: 1,
            }}
          >
            {t("column.name")}
          </Text>
          <SortHeader label="CPU" column={SORT_CPU} width={CPU_COLUMN} />
          <SortHeader label={t("column.memory")} column={SORT_MEMORY} width={MEMORY_COLUMN} />
        </View>

        {/* 读取中 / 离线 / 出错说明 */}
        <If when={props.section().note != null}>
          <View
            style={{
              display: "flex",
              flexDirection: "row",
              alignItems: "center",
              paddingLeft: 8,
              paddingRight: 8,
              paddingTop: 4,
              paddingBottom: 4,
              gap: 12,
            }}
          >
            <View style={{ width: DISCLOSURE_COLUMN, flexShrink: 0 }} />
            <Text
              style={{
                flex: 1,
                minWidth: 0,
                fontSize: look().type.ui.size,
                lineHeight: look().type.ui.line_height,
                color: look().palette.muted,
              }}
            >
              {props.section().note ?? ""}
            </Text>
          </View>
        </If>

        {/* 固定行（本机桌面进程、守护进程） */}
        <Each of={props.section().fixed} key={(row: any) => row.id}>
          {(row: () => any) => {
            const hasParts = () => (row().parts?.length ?? 0) > 0;
            const isOpen = () => expanded().has(row().id);
            return (
              <Collapsible
                open={hasParts() && isOpen()}
                reveal={true}
                style={{ display: "flex", flexDirection: "column" }}
                content={
                  <View
                    style={{
                      display: "flex",
                      flexDirection: "column",
                      paddingLeft: DISCLOSURE_COLUMN + 12,
                      paddingBottom: 4,
                    }}
                  >
                    <Each of={row().parts} key={(part: any) => part.id}>
                      {(part: () => any) => (
                        <View
                          style={{
                            display: "flex",
                            flexDirection: "row",
                            alignItems: "center",
                            paddingLeft: 8,
                            paddingRight: 8,
                            paddingTop: 3,
                            paddingBottom: 3,
                            gap: 12,
                          }}
                        >
                          <RowLabel label={part().label} detail={part().detail} />
                          <View
                            style={{
                              display: "flex",
                              flexDirection: "row",
                              alignItems: "center",
                              flexShrink: 0,
                            }}
                          >
                            <View style={{ width: CPU_COLUMN, flexShrink: 0 }} />
                            <Text
                              style={{
                                width: MEMORY_COLUMN,
                                textAlign: "right",
                                fontSize: look().type.ui.size,
                                lineHeight: look().type.ui.line_height,
                                color: look().palette.muted,
                                fontVariant: ["tabular-nums"],
                                numberOfLines: 1,
                              }}
                            >
                              {formatBytes(part().bytes)}
                            </Text>
                          </View>
                        </View>
                      )}
                    </Each>
                  </View>
                }
              >
                <If
                  when={hasParts()}
                  else={
                    <View
                      style={{
                        display: "flex",
                        flexDirection: "row",
                        alignItems: "center",
                        paddingLeft: 8,
                        paddingRight: 8,
                        paddingTop: 4,
                        paddingBottom: 4,
                        gap: 12,
                      }}
                    >
                      <View style={{ width: DISCLOSURE_COLUMN, flexShrink: 0 }} />
                      <RowLabel label={row().label} detail={row().detail} />
                      <UsageColumns
                        cpuPercent={row().usage.cpu_percent}
                        memoryBytes={row().usage.memory_bytes}
                        color={look().palette.text}
                      />
                    </View>
                  }
                >
                  <BaseButton
                    onPress={() => toggleRow(row().id)}
                    style={{
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      paddingLeft: 8,
                      paddingRight: 8,
                      paddingTop: 4,
                      paddingBottom: 4,
                      gap: 12,
                      borderRadius: look().radius.row ?? 6,
                      backgroundColor: "transparent",
                      cursor: "default",
                    }}
                    hoverStyle={{ backgroundColor: look().palette.row_hover }}
                    activeStyle={{ backgroundColor: look().palette.row_selected }}
                  >
                    <Disclosure open={isOpen()} />
                    <RowLabel label={row().label} detail={row().detail} />
                    <UsageColumns
                      cpuPercent={row().usage.cpu_percent}
                      memoryBytes={row().usage.memory_bytes}
                      color={look().palette.text}
                    />
                  </BaseButton>
                </If>
              </Collapsible>
            );
          }}
        </Each>

        {/* 会话行与侧栏之外的会话 */}
        <Each of={displayGroups()} key={(item: any) => item.id}>
          {(item: () => any) => {
            const isOpen = () => expanded().has(item().id);
            const detail = () => {
              locale();
              const g = item().group;
              const summary = processSummary(g.usage.process_count, g.processes, t);
              return g.jump != null ? summary : t("others.detail", { summary });
            };
            const unlisted = () => {
              const g = item().group;
              return Math.max(0, Number(g.usage.process_count ?? 0) - (g.processes?.length ?? 0));
            };

            return (
              <Collapsible
                open={isOpen()}
                reveal={true}
                style={{ display: "flex", flexDirection: "column" }}
                content={
                  <View
                    style={{
                      display: "flex",
                      flexDirection: "column",
                      paddingLeft: DISCLOSURE_COLUMN + 12,
                      paddingBottom: 4,
                    }}
                  >
                    <Each of={item().group.processes} key={(p: any) => `${p.pid}-${p.name}`}>
                      {(proc: () => any) => (
                        <View
                          style={{
                            display: "flex",
                            flexDirection: "row",
                            alignItems: "center",
                            paddingLeft: 8,
                            paddingRight: 8,
                            paddingTop: 3,
                            paddingBottom: 3,
                            gap: 12,
                          }}
                        >
                          <View
                            style={{
                              flex: 1,
                              minWidth: 0,
                              display: "flex",
                              flexDirection: "column",
                            }}
                          >
                            <View
                              style={{
                                display: "flex",
                                flexDirection: "row",
                                alignItems: "baseline",
                                gap: 8,
                                minWidth: 0,
                              }}
                            >
                              <Text
                                style={{
                                  fontSize: look().type.ui.size,
                                  lineHeight: look().type.ui.line_height,
                                  color: look().palette.text,
                                  flexShrink: 1,
                                  numberOfLines: 1,
                                }}
                              >
                                {proc().name}
                              </Text>
                              <Text
                                style={{
                                  fontSize: look().type.text_xsmall.size,
                                  lineHeight: look().type.text_xsmall.line_height,
                                  color: look().palette.muted,
                                  flexShrink: 0,
                                  fontVariant: ["tabular-nums"],
                                  numberOfLines: 1,
                                }}
                              >
                                {`pid ${proc().pid}`}
                              </Text>
                            </View>
                            <If when={Boolean(proc().command)}>
                              <Text
                                style={{
                                  fontSize: look().type.text_xsmall.size,
                                  lineHeight: look().type.text_xsmall.line_height,
                                  fontFamily: look().fonts?.mono,
                                  color: look().palette.muted,
                                  numberOfLines: 1,
                                }}
                              >
                                {proc().command}
                              </Text>
                            </If>
                          </View>
                          <UsageColumns
                            cpuPercent={proc().usage.cpu_percent}
                            memoryBytes={proc().usage.memory_bytes}
                            color={look().palette.muted}
                          />
                        </View>
                      )}
                    </Each>

                    <If when={unlisted() > 0}>
                      <Text
                        style={{
                          paddingLeft: 8,
                          paddingRight: 8,
                          paddingTop: 4,
                          paddingBottom: 4,
                          fontSize: look().type.text_xsmall.size,
                          lineHeight: look().type.text_xsmall.line_height,
                          color: look().palette.muted,
                        }}
                      >
                        {unlisted() === 1
                          ? t("unlisted.one")
                          : t("unlisted.other", { count: unlisted() })}
                      </Text>
                    </If>
                  </View>
                }
              >
                <View
                  class="hover:bg-accent active:bg-sidebar-accent"
                  onPress={() => toggleRow(item().id)}
                  style={{
                    display: "flex",
                    flexDirection: "row",
                    alignItems: "center",
                    paddingLeft: 8,
                    paddingRight: 8,
                    paddingTop: 4,
                    paddingBottom: 4,
                    gap: 12,
                    borderRadius: look().radius.row ?? 6,
                    cursor: "default",
                  }}
                >
                  <Disclosure open={isOpen()} />
                  <RowLabel label={item().group.title} detail={detail()} />
                  <If when={item().group.jump != null}>
                    <View
                      style={{ flexShrink: 0 }}
                      onPress={(e: any) => e?.stopPropagation?.()}
                      onPressDown={(e: any) => e?.stopPropagation?.()}
                    >
                      <SmeltButton
                        quiet={true}
                        small={true}
                        icon="icons/external-link.svg"
                        tooltip={t("jump")}
                        accessibilityLabel={t("jump")}
                        onPress={(e: any) => {
                          e?.stopPropagation?.();
                          const jumpId = item().group.jump;
                          if (jumpId != null) {
                            jumpToSession(props.section().machine, jumpId);
                          }
                        }}
                      />
                    </View>
                  </If>
                  <UsageColumns
                    cpuPercent={item().group.usage.cpu_percent}
                    memoryBytes={item().group.usage.memory_bytes}
                    color={look().palette.text}
                  />
                </View>
              </Collapsible>
            );
          }}
        </Each>

        {/* 合计行 */}
        <If when={props.section().total != null}>
          <View
            style={{
              display: "flex",
              flexDirection: "row",
              alignItems: "center",
              marginTop: 4,
              paddingLeft: 8,
              paddingRight: 8,
              paddingTop: 6,
              gap: 12,
              borderTopWidth: 1,
              borderTopColor: look().palette.hair,
            }}
          >
            <View style={{ width: DISCLOSURE_COLUMN, flexShrink: 0 }} />
            <Text
              style={{
                flex: 1,
                minWidth: 0,
                fontSize: look().type.ui_medium.size,
                lineHeight: look().type.ui_medium.line_height,
                fontWeight: look().type.ui_medium.weight,
                color: look().palette.text,
                numberOfLines: 1,
              }}
            >
              {t("column.total")}
            </Text>
            <UsageColumns
              cpuPercent={props.section().total?.cpu_percent ?? 0}
              memoryBytes={props.section().total?.memory_bytes ?? 0}
              color={look().palette.text}
            />
          </View>
        </If>
      </View>
    </SectionCard>
  );
}

export function main() {
  syncVisibility();
  effect(() => {
    cleanup(() => {
      stopLoop();
    });
  });

  const snap = computed(() => snapshot());

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
          <SummarySection />
          <Each of={snap().sections} key={(section: any) => section.idPrefix}>
            {(section: () => any) => <MachineSection section={section} />}
          </Each>
        </View>
      </View>
    </Scroll>
  );
}
