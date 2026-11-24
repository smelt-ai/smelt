// 对话左侧的提问导航刻度列：最多 11 条刻度随当前阅读位置滑动，悬停出提问预览卡，点击跳转。
import { computed, signal, Text, View } from "nsh";
import { theme } from "smelt/sdk";
import { computePromptRailWindow, PROMPT_RAIL_MAX_TICKS } from "./model.js";

export interface PromptRailItem {
  globalIndex: number;
  rowIndex: number;
  text: string;
}

export function PromptRail(props: {
  prompts: () => PromptRailItem[];
  readingGlobalIndex: () => number | null;
  onJump: (item: PromptRailItem) => void;
}) {
  const look = theme;
  const hovered = signal<PromptRailItem | null>(null);

  const windowData = computed(() =>
    computePromptRailWindow(props.prompts(), props.readingGlobalIndex(), PROMPT_RAIL_MAX_TICKS),
  );

  return (
    <If when={windowData().items.length >= 2}>
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
        <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
          <View style={{ width: 24, gap: 4, paddingVertical: 6 }}>
            <Each of={windowData().items}>
              {(item) => {
                const isReading = computed(
                  () => props.readingGlobalIndex() === item().globalIndex,
                );
                const isHovered = computed(
                  () => hovered()?.globalIndex === item().globalIndex,
                );
                const lit = computed(() => isReading() || isHovered());
                const width = computed(() =>
                  lit() ? 14 : Math.max(4.5, 8 * item().edgeFactor),
                );
                const opacity = computed(() =>
                  lit() ? 1 : Math.max(0.35, 0.55 * item().edgeFactor),
                );
                return (
                  <View
                    onPress={() => props.onJump(item())}
                    onMouseEnter={() => hovered.set(item())}
                    onMouseLeave={() => {
                      if (hovered()?.globalIndex === item().globalIndex) hovered.set(null);
                    }}
                    style={{
                      height: 8,
                      justifyContent: "center",
                    }}
                  >
                    <View
                      style={{
                        width: width(),
                        height: 2,
                        borderRadius: 1,
                        backgroundColor: lit() ? look().palette.text : look().palette.faint,
                        opacity: opacity(),
                        transitionProperty: "width, opacity, backgroundColor",
                        transitionDuration: 150,
                      }}
                    />
                  </View>
                );
              }}
            </Each>
          </View>

          <If when={hovered() != null}>
            <View
              class="w-72 px-3 py-2 rounded-[10px] border border-border bg-popover shadow-md pointer-events-none"
            >
              <Text
                class="text-foreground"
                style={{
                  fontSize: look().type.text_xsmall.size,
                  lineHeight: look().type.text_xsmall.line_height,
                  numberOfLines: 3,
                }}
              >
                {hovered()?.text ?? ""}
              </Text>
            </View>
          </If>
        </View>
      </View>
    </If>
  );
}
