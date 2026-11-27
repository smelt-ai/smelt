// 对话左侧的提问导航刻度列：静息态保持极简悬浮刻度并自动居中当前阅读位置；
// 提问超窗时直接支持原生平滑滚动，无边框、无箭头，仅在悬停时浮现极细位置指示条与微距波形反馈。
import { computed, effect, Scroll, signal, Text, View } from "nsh";
import { theme } from "smelt/sdk";
import {
  computePromptRailScrollMetrics,
  PROMPT_RAIL_EDGE_FADE,
  PROMPT_RAIL_MAX_TICKS,
  promptRailEdgeFactor,
} from "./model.js";

const TICK_HEIGHT = 10;
const TICK_GAP = 2;
const TICK_PITCH = TICK_HEIGHT + TICK_GAP;
const RAIL_PAD_Y = 6;
const VIEWPORT_MAX_H = PROMPT_RAIL_MAX_TICKS * TICK_PITCH - TICK_GAP + RAIL_PAD_Y * 2;
const THUMB_INSET = 8;
/** 刻度间距仅 12px，直接 1:1 吃触控板位移会一滑飞过十几问；加 0.25 阻尼（约 48pt 一问），鼠标滚轮一格一问。 */
const TRACKPAD_DAMP = 0.25;

export interface PromptRailItem {
  globalIndex: number;
  rowIndex: number;
  text: string;
  answer?: string;
}

export function PromptRail(props: {
  prompts: () => PromptRailItem[];
  readingGlobalIndex: () => number | null;
  onJump: (item: PromptRailItem) => void;
  onScrub?: (item: PromptRailItem) => void;
}) {
  const look = theme;
  const railHovered = signal(false);
  const hoveredIndex = signal<number | null>(null);
  const scrollY = signal(0);
  let scrollRef: any = null;
  let pointerYInScroll: number | null = null;
  let lastScrubbedIndex: number | null = null;

  const total = computed(() => props.prompts().length);
  const viewportHeight = computed(() => {
    const count = total();
    if (count <= 0) return 0;
    const natural = count * TICK_PITCH - TICK_GAP + RAIL_PAD_Y * 2;
    return Math.min(VIEWPORT_MAX_H, natural);
  });

  const scrollMetrics = computed(() =>
    computePromptRailScrollMetrics({
      total: total(),
      scrollY: scrollY(),
      viewportHeight: viewportHeight(),
      pitch: TICK_PITCH,
      tickHeight: TICK_HEIGHT,
      padY: RAIL_PAD_Y,
      edgeFadeTicks: PROMPT_RAIL_EDGE_FADE,
      trackInset: THUMB_INSET,
    }),
  );

  const canScroll = computed(() => scrollMetrics().canScroll);

  const centerReadingPrompt = (animated: boolean) => {
    const list = props.prompts();
    if (list.length <= PROMPT_RAIL_MAX_TICKS) {
      if (scrollY() !== 0) scrollY.set(0);
      return;
    }
    const readingIx = props.readingGlobalIndex();
    const pos = Math.max(
      0,
      list.findIndex((p) => p.globalIndex === readingIx),
    );
    const vpH = viewportHeight();
    const maxS = scrollMetrics().maxScroll;
    const itemCenter = RAIL_PAD_Y + pos * TICK_PITCH + TICK_HEIGHT / 2;
    const targetY = Math.max(0, Math.min(maxS, Math.round(itemCenter - vpH / 2)));
    scrollY.set(targetY);
    scrollRef?.scrollTo?.({ y: targetY, animated });
  };

  // 非悬停状态下自动把当前阅读的提问刻度居中到视窗内。
  effect(() => {
    props.readingGlobalIndex();
    total();
    if (!railHovered()) {
      centerReadingPrompt(false);
    }
  });

  const indexAtPointer = (yInScroll: number, curScrollY: number): number | null => {
    const list = props.prompts();
    if (list.length === 0) return null;
    const raw = Math.floor((yInScroll + curScrollY - RAIL_PAD_Y) / TICK_PITCH);
    return Math.max(0, Math.min(list.length - 1, raw));
  };

  const syncHoveredFromPointer = (yInScroll: number, curScrollY: number) => {
    const idx = indexAtPointer(yInScroll, curScrollY);
    if (idx != null && hoveredIndex() !== idx) {
      hoveredIndex.set(idx);
    }
    return idx;
  };

  const hoveredItem = computed(() => {
    const idx = hoveredIndex();
    if (idx == null) return null;
    const item = props.prompts()[idx];
    return item ? { item, order: idx + 1, total: total() } : null;
  });

  const handleRailWheel = (e: { delta?: { y?: number }; deltaMode?: "points" | "lines" }) => {
    const maxS = scrollMetrics().maxScroll;
    if (maxS <= 1) return;
    const rawY = Number(e?.delta?.y ?? 0);
    if (!Number.isFinite(rawY) || rawY === 0) return;
    const deltaY = e.deltaMode === "lines" ? rawY * TICK_PITCH : rawY * TRACKPAD_DAMP;
    const curY = scrollY();
    const nextY = Math.max(0, Math.min(maxS, curY + deltaY));
    if (Math.abs(nextY - curY) <= 0.05) return;
    scrollY.set(nextY);
    scrollRef?.scrollTo?.({ y: nextY, animated: false });
    if (railHovered()) {
      const anchorY = pointerYInScroll ?? viewportHeight() / 2;
      const idx = syncHoveredFromPointer(anchorY, nextY);
      if (idx != null && idx !== lastScrubbedIndex) {
        lastScrubbedIndex = idx;
        const target = props.prompts()[idx];
        if (target) props.onScrub?.(target);
      }
    }
  };

  return (
    <If when={total() >= 2}>
      <View
        style={{
          position: "absolute",
          left: 10,
          top: 64,
          bottom: 96,
          justifyContent: "center",
          pointerEvents: "box-none",
        }}
      >
        <View style={{ flexDirection: "row", alignItems: "center", gap: 10 }}>
          {/* 纯净悬浮刻度列：无边框、无箭头，靠微距波形与极细悬浮进度丝线表达可滚动状态。 */}
          <View
            onMouseEnter={(e: any) => {
              railHovered.set(true);
              const y = Number(e?.position?.y);
              if (Number.isFinite(y)) {
                pointerYInScroll = y;
                lastScrubbedIndex = syncHoveredFromPointer(y, scrollY());
              } else {
                lastScrubbedIndex = hoveredIndex();
              }
            }}
            onMouseLeave={() => {
              railHovered.set(false);
              hoveredIndex.set(null);
              pointerYInScroll = null;
              lastScrubbedIndex = null;
              if (canScroll()) centerReadingPrompt(true);
            }}
            onMouseMove={(e: any) => {
              const y = Number(e?.position?.y);
              if (Number.isFinite(y)) {
                pointerYInScroll = y;
                syncHoveredFromPointer(y, scrollY());
              }
            }}
            style={{
              position: "relative",
              width: 24,
              height: viewportHeight(),
            }}
          >
            {/* 仅在悬停且超窗时左侧浮现 1.5px 无轨丝线指示当前滚动位置。 */}
            <If when={railHovered() && canScroll()}>
              <View
                class="rounded-full"
                style={{
                  position: "absolute",
                  left: 0,
                  top: scrollMetrics().thumbTop,
                  width: 1.5,
                  height: scrollMetrics().thumbHeight,
                  backgroundColor: look().palette.faint,
                  opacity: 0.45,
                  pointerEvents: "none",
                }}
              />
            </If>

            <Scroll
              ref={(el: any) => {
                scrollRef = el;
                if (el && !railHovered()) {
                  centerReadingPrompt(false);
                }
              }}
              scrollbar={false}
              style={{
                width: "100%",
                height: viewportHeight(),
              }}
            >
              <View
                onWheel={canScroll() ? handleRailWheel : undefined}
                style={{
                  width: "100%",
                  paddingLeft: 5,
                  paddingVertical: RAIL_PAD_Y,
                  gap: TICK_GAP,
                }}
              >
                <Each of={props.prompts()} key={(p: PromptRailItem) => p.globalIndex}>
                  {(item, idx) => {
                    const isReading = computed(
                      () => props.readingGlobalIndex() === item().globalIndex,
                    );
                    const isHovered = computed(() => hoveredIndex() === idx());
                    const isNeighbor = computed(() => {
                      const h = hoveredIndex();
                      return h != null && Math.abs(h - idx()) === 1;
                    });
                    const edgeFactor = computed(() => {
                      const m = scrollMetrics();
                      return promptRailEdgeFactor({
                        index: idx(),
                        scrollY: scrollY(),
                        viewportHeight: viewportHeight(),
                        pitch: TICK_PITCH,
                        tickHeight: TICK_HEIGHT,
                        padY: RAIL_PAD_Y,
                        fadeZone: m.fadeZone ?? PROMPT_RAIL_EDGE_FADE * TICK_PITCH,
                        canScrollUp: m.canScrollUp,
                        canScrollDown: m.canScrollDown,
                      });
                    });
                    const width = computed(() => {
                      if (isHovered()) return 16;
                      if (isReading()) return hoveredIndex() != null ? 11 : 14;
                      if (isNeighbor()) return Math.max(6, 10.5 * edgeFactor());
                      return Math.max(4, 8 * edgeFactor());
                    });
                    const opacity = computed(() => {
                      if (isHovered()) return 1;
                      if (isReading()) return hoveredIndex() != null ? 0.68 : 1;
                      if (isNeighbor()) return Math.max(0.42, 0.72 * edgeFactor());
                      return Math.max(0.28, 0.52 * edgeFactor());
                    });
                    const color = computed(() =>
                      isHovered() || isReading() ? look().palette.text : look().palette.faint,
                    );
                    return (
                      <View
                        onPress={() => props.onJump(item())}
                        onMouseEnter={() => hoveredIndex.set(idx())}
                        style={{
                          height: TICK_HEIGHT,
                          justifyContent: "center",
                        }}
                      >
                        <View
                          class="rounded-full"
                          style={{
                            width: width(),
                            height: 2,
                            backgroundColor: color(),
                            opacity: opacity(),
                            // 宽度是布局，NSH 不按样式过渡它；整串里有一个认不出整串作废，所以只写能过渡的。
                            transitionProperty: "opacity, backgroundColor",
                            transitionDuration: 140,
                          }}
                        />
                      </View>
                    );
                  }}
                </Each>
              </View>
            </Scroll>
          </View>

          <If when={hoveredItem() != null}>
            <View
              class="w-72 px-3 py-2.5 rounded-lg border border-border bg-popover shadow-md pointer-events-none"
              style={{ gap: 5 }}
            >
              <Text
                class="text-foreground"
                style={{
                  fontSize: look().type.text_small_medium.size,
                  lineHeight: look().type.text_small_medium.line_height,
                  fontWeight: look().type.text_small_medium.weight,
                  numberOfLines: 3,
                }}
              >
                {hoveredItem()?.item.text ?? ""}
              </Text>
              <If when={Boolean(hoveredItem()?.item.answer)}>
                <Text
                  style={{
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                    color: look().palette.muted,
                    numberOfLines: 2,
                  }}
                >
                  {hoveredItem()?.item.answer ?? ""}
                </Text>
              </If>
              <Text
                style={{
                  fontSize: look().type.small.size,
                  lineHeight: look().type.small.line_height,
                  color: look().palette.faint,
                  opacity: 0.72,
                  textAlign: "right",
                }}
              >
                {`${hoveredItem()?.order ?? 0} / ${hoveredItem()?.total ?? 0}`}
              </Text>
            </View>
          </If>
        </View>
      </View>
    </If>
  );
}
