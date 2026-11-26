// 输入栏工具弹层：模型与推理强度/配置弹层、能力（Skills/MCP）弹层、上下文用量弹层。
import { computed, Scroll, signal, Text, View } from "nsh";
import { Input } from "nsh/base";
import { SmeltButton, SmeltIcon, SmeltPopover, SmeltSwitch, theme } from "smelt/sdk";
import { ct } from "./i18n.ts";
import {
  buildVisibleModels,
  colorWithAlpha,
  compactTokenCount,
  composerModelLabel,
  isThinkingConfig,
  selectedConfigIndex,
  selectedProviderGroup,
  usagePercent,
} from "./model.js";

export function ModelConfigPopover(props: {
  model: () => any | null;
  configs: () => any[];
  fallbackLabel: () => string;
  hasPendingNotice?: () => boolean;
  onSelectConfig: (configId: string, valueId: string, boolVal?: boolean) => void;
}) {
  const look = theme;
  const open = signal(false);
  const query = signal("");
  const activeProvider = signal<string | null>(null);

  const label = computed(() => composerModelLabel(props.model(), props.fallbackLabel()));
  const providerGroups = computed(() => props.model()?.provider_groups ?? []);
  const currentGroupId = computed(
    () => activeProvider() ?? selectedProviderGroup(props.model())?.id ?? null,
  );
  const visibleModels = computed(() => buildVisibleModels(props.model(), currentGroupId(), query()));

  const thinkingConfig = computed(
    () => (props.configs() ?? []).find((c) => isThinkingConfig(c)) ?? null,
  );

  const thinkingMeta = computed(() => {
    const cfg = thinkingConfig();
    if (!cfg || !Array.isArray(cfg.options) || cfg.options.length === 0) return null;
    const index = selectedConfigIndex(cfg);
    return { currentName: cfg.current_name ?? cfg.options[index]?.[1] ?? "" };
  });

  const pendingNotice = () => Boolean(props.hasPendingNotice?.());

  return (
    <SmeltPopover
      open={open()}
      onOpenChange={({ open: next }: any) => {
        open.set(Boolean(next));
        if (!next) {
          query.set("");
          activeProvider.set(null);
        }
      }}
      placement="top"
      align="start"
      offset={8}
      popupClass="rounded-xl border border-border bg-popover p-0 shadow-md overflow-hidden"
      popupStyle={{ width: 348 }}
      trigger={
        <SmeltButton
          quiet
          small
          selected={open()}
          trailingIcon="icons/chevron-down.svg"
          accessibilityLabel={label()}
          style={{
            borderRadius: 999,
            paddingLeft: 10,
            paddingRight: 8,
          }}
        >
          <View style={{ flexDirection: "row", alignItems: "center", gap: 6, minWidth: 0 }}>
            <Text
              style={{
                fontSize: look().type.text_xsmall_medium.size,
                fontWeight: look().type.text_xsmall_medium.weight,
                // 改了模型要等下一轮才生效：字提亮一档，不用要你的橙。
                color: pendingNotice() || open() ? look().palette.text : look().palette.muted,
                numberOfLines: 1,
              }}
            >
              {label()}
            </Text>
            <If when={thinkingMeta() != null && Boolean(thinkingMeta()?.currentName)}>
              <Text
                style={{
                  fontSize: look().type.text_xsmall.size,
                  color: pendingNotice() ? look().palette.muted : look().palette.faint,
                  numberOfLines: 1,
                }}
              >
                · {thinkingMeta()?.currentName ?? ""}
              </Text>
            </If>
          </View>
        </SmeltButton>
      }
    >
      <View style={{ width: "100%" }}>
        {/* 1. 无底框搜索栏（对齐设计语言可搜索下拉：38px 高 + 左侧放大镜 + 底部分隔线） */}
        <If when={props.model() != null}>
          <View>
            <View
              style={{
                height: 38,
                paddingHorizontal: 12,
                flexDirection: "row",
                alignItems: "center",
                gap: 8,
                borderBottomWidth: 1,
                borderBottomColor: look().palette.hair,
              }}
            >
              <SmeltIcon path="icons/search.svg" size={14} color={look().palette.faint} />
              <Input
                value={query()}
                placeholder={ct("model.search")}
                onChange={({ value }: any) => query.set(value)}
                style={{
                  flex: 1,
                  height: 34,
                  backgroundColor: "transparent",
                  color: look().palette.text,
                  placeholderColor: look().palette.faint,
                  fontSize: look().type.ui.size,
                }}
              />
            </View>

            {/* 2. 多 Provider 胶囊切换条 */}
            <If when={providerGroups().length > 1 && !query().trim()}>
              <Scroll horizontal style={{ maxHeight: 36 }}>
                <View
                  role="tablist"
                  style={{
                    flexDirection: "row",
                    alignItems: "center",
                    gap: 2,
                    paddingHorizontal: 10,
                    paddingTop: 8,
                    paddingBottom: 2,
                  }}
                >
                  <Each of={providerGroups()}>
                    {(group) => {
                      const active = computed(() => currentGroupId() === group().id);
                      // 当前模型在这家：切到别家看时也认得出在用哪家的。
                      const holdsCurrent = computed(() =>
                        (group().options ?? []).some(([value]: any) => value === props.model()?.current_value),
                      );
                      return (
                        <View
                          onPress={() => activeProvider.set(group().id)}
                          class={active() ? undefined : "hover:bg-accent"}
                          role="tab"
                          accessibilityState={{ selected: active() }}
                          style={{
                            height: 24,
                            paddingHorizontal: 10,
                            borderRadius: 99,
                            flexDirection: "row",
                            justifyContent: "center",
                            alignItems: "center",
                            gap: 6,
                            backgroundColor: active() ? look().palette.row_selected : "transparent",
                          }}
                        >
                          <Text
                            style={{
                              fontSize: look().type.small_medium.size,
                              fontWeight: active() ? look().type.small_medium.weight : look().type.small.weight,
                              color: active() ? look().palette.text : look().palette.muted,
                              numberOfLines: 1,
                            }}
                          >
                            {group().name}
                          </Text>
                          <If when={holdsCurrent()}>
                            <View
                              style={{
                                width: 5,
                                height: 5,
                                borderRadius: 3,
                                backgroundColor: active() ? look().palette.text : look().palette.muted,
                              }}
                            />
                          </If>
                        </View>
                      );
                    }}
                  </Each>
                </View>
              </Scroll>
            </If>

            {/* 3. 模型列表 */}
            <Scroll style={{ maxHeight: 216 }}>
              <View role="listbox" accessibilityLabel={label()} style={{ padding: 6, gap: 2 }}>
                <If
                  when={visibleModels().length > 0}
                  else={
                    <Text
                      style={{
                        paddingHorizontal: 10,
                        paddingVertical: 14,
                        fontSize: look().type.text_xsmall.size,
                        color: look().palette.faint,
                        textAlign: "center",
                      }}
                    >
                      {ct("model.empty")}
                    </Text>
                  }
                >
                  <Each of={visibleModels()}>
                    {(item) => {
                      const selected = computed(() => props.model()?.current_value === item().value);
                      return (
                        <View
                          onPress={() => {
                            const cfgId = props.model()?.config_id || "model";
                            props.onSelectConfig(cfgId, item().value);
                          }}
                          class="hover:bg-accent"
                          role="option"
                          accessibilityLabel={item().label}
                          accessibilityState={{ selected: selected() }}
                          style={{
                            height: 30,
                            paddingHorizontal: 10,
                            borderRadius: look().radius.row,
                            flexDirection: "row",
                            alignItems: "center",
                            gap: 8,
                          }}
                        >
                          <Text
                            style={{
                              flex: 1,
                              minWidth: 0,
                              fontSize: look().type.ui.size,
                              fontWeight: selected()
                                ? look().type.ui_medium.weight
                                : look().type.ui.weight,
                              color: look().palette.text,
                              numberOfLines: 1,
                            }}
                          >
                            {item().label}
                          </Text>
                          <If when={Boolean(item().providerTag)}>
                            <Text
                              style={{
                                fontSize: look().type.text_xsmall.size,
                                color: look().palette.faint,
                              }}
                            >
                              {item().providerTag ?? ""}
                            </Text>
                          </If>
                          <If when={selected()}>
                            <SmeltIcon path="icons/check.svg" size={13} color={look().palette.text} />
                          </If>
                        </View>
                      );
                    }}
                  </Each>
                </If>
              </View>
            </Scroll>
          </View>
        </If>

        {/* 4. 会话设置：按智能体给的顺序一行一项，名字在左、控件在右；放不下时控件折到下一行。 */}
        <If when={(props.configs() ?? []).length > 0}>
          <View
            style={{
              paddingHorizontal: 6,
              paddingVertical: 4,
              borderTopWidth: props.model() != null ? 1 : 0,
              borderTopColor: look().palette.hair,
            }}
          >
            <Each of={props.configs() ?? []}>
              {(cfg) => (
                <ConfigRow
                  cfg={cfg}
                  pending={() => pendingNotice() && isThinkingConfig(cfg())}
                  onSelectConfig={props.onSelectConfig}
                />
              )}
            </Each>
          </View>
        </If>
      </View>
    </SmeltPopover>
  );
}


/** 一项会话设置：布尔给开关；两档以上给分段；只有一档没得选，直接写值。 */
function ConfigRow(props: {
  cfg: () => any;
  pending: () => boolean;
  onSelectConfig: (configId: string, valueId: string, boolVal?: boolean) => void;
}) {
  const look = theme;
  const name = () => props.cfg().name || props.cfg().config_id;
  const options = (): any[] => props.cfg().options ?? [];
  const currentName = () => {
    const cfg = props.cfg();
    return cfg.current_name ?? options()[selectedConfigIndex(cfg)]?.[1] ?? "";
  };
  return (
    <View
      style={{
        minHeight: 34,
        paddingLeft: 10,
        paddingRight: 4,
        paddingVertical: 4,
        flexDirection: "row",
        flexWrap: "wrap",
        alignItems: "center",
        justifyContent: "space-between",
        columnGap: 12,
        rowGap: 4,
      }}
    >
      <View style={{ flexDirection: "row", alignItems: "center", gap: 6, flexShrink: 1, minWidth: 0 }}>
        <Text
          style={{
            fontSize: look().type.text_xsmall.size,
            color: look().palette.muted,
            numberOfLines: 1,
          }}
        >
          {name()}
        </Text>
        <If when={props.pending()}>
          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.faint, numberOfLines: 1 }}>
            · {ct("model.next_turn")}
          </Text>
        </If>
      </View>
      <If
        when={props.cfg().boolean != null}
        else={
          <If
            when={options().length > 1}
            else={
              <Text
                style={{
                  paddingRight: 6,
                  fontSize: look().type.text_xsmall.size,
                  color: look().palette.faint,
                  numberOfLines: 1,
                }}
              >
                {currentName()}
              </Text>
            }
          >
            <ConfigSegments
              label={name()}
              options={options}
              current={currentName}
              onSelect={(value) => props.onSelectConfig(props.cfg().config_id, value)}
            />
          </If>
        }
      >
        <SmeltSwitch
          checked={Boolean(props.cfg().boolean)}
          accessibilityLabel={name()}
          onChange={(checked: boolean) => {
            const val = Boolean(checked);
            props.onSelectConfig(props.cfg().config_id, val ? "true" : "false", val);
          }}
        />
      </If>
    </View>
  );
}

/** 配置项的档位：紧凑分段控件，选中的那段浮起（`surface` 底、`hair_strong` 边），其余悬停变底。 */
function ConfigSegments(props: {
  label: string;
  options: () => any[];
  current: () => string | null | undefined;
  onSelect: (value: string) => void;
}) {
  const look = theme;
  return (
    <View
      role="radiogroup"
      accessibilityLabel={props.label}
      style={{
        flexDirection: "row",
        flexWrap: "wrap",
        maxWidth: "100%",
        gap: 2,
        padding: 2,
        borderRadius: 8,
        backgroundColor: look().palette.bg,
        borderWidth: 1,
        borderColor: look().palette.hair,
      }}
    >
      <Each of={props.options()}>
        {(opt) => {
          const active = computed(() => props.current() === opt()[1]);
          return (
            <View
              onPress={() => props.onSelect(opt()[0])}
              class={active() ? undefined : "hover:bg-accent"}
              role="radio"
              accessibilityLabel={opt()[1]}
              accessibilityState={{ checked: active() }}
              style={{
                height: 22,
                paddingHorizontal: 9,
                borderRadius: 6,
                alignItems: "center",
                justifyContent: "center",
                borderWidth: 1,
                borderColor: active() ? look().palette.hair_strong : "transparent",
                backgroundColor: active() ? look().palette.surface : "transparent",
                transitionProperty: "backgroundColor, borderColor",
                transitionDuration: 150,
              }}
            >
              <Text
                style={{
                  fontSize: look().type.text_xsmall.size,
                  fontWeight: active() ? 530 : 440,
                  color: active() ? look().palette.text : look().palette.muted,
                  numberOfLines: 1,
                }}
              >
                {opt()[1]}
              </Text>
            </View>
          );
        }}
      </Each>
    </View>
  );
}

export function CapabilitiesPopover(props: {
  skills: () => { command: string; name: string; description: string }[];
  mcpServers: () => string[];
  byAgent: () => boolean;
  /** 插入技能命令（不带斜杠），例如 Pi 的 `skill:foo`。 */
  onUseSkill: (command: string) => void;
  onManage?: () => void;
}) {
  const look = theme;
  const open = signal(false);
  const total = computed(() => props.skills().length + props.mcpServers().length);

  // 会话什么都没报（还在启动、或引擎报不出来）就不画入口。
  return (
    <If when={total() > 0}>
      <SmeltPopover
        open={open()}
        onOpenChange={({ open: next }: any) => open.set(Boolean(next))}
        placement="top"
        align="start"
        offset={8}
        popupClass="rounded-xl border border-border bg-popover p-0 shadow-md overflow-hidden"
        popupStyle={{ width: 320 }}
        trigger={
          <SmeltButton
            quiet
            small
            selected={open()}
            icon="icons/puzzle.svg"
            label={String(total())}
            tooltip={ct("capabilities.title")}
            accessibilityLabel={ct("capabilities.title")}
            style={{ borderRadius: 999 }}
          />
        }
      >
        <View>
          <Scroll style={{ maxHeight: 340 }}>
            <View style={{ padding: 6 }}>
              {/* 技能：点了把它的斜杠命令插进输入框，悬停时右边写出来 */}
              <If when={props.skills().length > 0}>
                <CapabilityHeader icon="icons/sparkles.svg" label={ct("capabilities.skills")} count={props.skills().length} />
                <Each of={props.skills()}>
                  {(skill) => (
                    <View
                      onPress={() => {
                        open.set(false);
                        props.onUseSkill(skill().command);
                      }}
                      class="group/row hover:bg-accent"
                      role="button"
                      accessibilityLabel={skill().name}
                      accessibilityHint={ct("capabilities.insert")}
                      style={{
                        flexDirection: "row",
                        alignItems: "center",
                        gap: 10,
                        paddingHorizontal: 8,
                        paddingVertical: 6,
                        borderRadius: look().radius.row,
                      }}
                    >
                      <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
                        <Text style={{ fontSize: look().type.ui.size, color: look().palette.text, numberOfLines: 1 }}>
                          {skill().name}
                        </Text>
                        <If when={Boolean(skill().description)}>
                          <Text
                            style={{
                              fontSize: look().type.text_xsmall.size,
                              color: look().palette.faint,
                              numberOfLines: 1,
                            }}
                          >
                            {String(skill().description).replace(/\s+/g, " ").trim()}
                          </Text>
                        </If>
                      </View>
                      <Text
                        class="opacity-0 group-hover/row:opacity-100"
                        style={{ fontSize: look().type.small.size, color: look().palette.muted, numberOfLines: 1 }}
                      >
                        {ct("capabilities.insert")}
                      </Text>
                    </View>
                  )}
                </Each>
              </If>

              <If when={props.skills().length > 0 && props.mcpServers().length > 0}>
                <View style={{ height: 1, backgroundColor: look().palette.hair, marginVertical: 6, marginHorizontal: 2 }} />
              </If>

              {/* MCP：只列出来、点不了，所以不给悬停底色 */}
              <If when={props.mcpServers().length > 0}>
                <CapabilityHeader icon="icons/wrench.svg" label={ct("capabilities.mcp")} count={props.mcpServers().length} />
                <View style={{ paddingBottom: 2 }}>
                  <Each of={props.mcpServers()}>
                    {(srv) => (
                      <Text
                        style={{
                          height: 26,
                          paddingHorizontal: 8,
                          fontSize: look().type.ui.size,
                          lineHeight: 26,
                          color: look().palette.text,
                          numberOfLines: 1,
                        }}
                      >
                        {srv()}
                      </Text>
                    )}
                  </Each>
                </View>
              </If>
            </View>
          </Scroll>

          <If when={Boolean(props.onManage)}>
            <View
              style={{
                borderTopWidth: 1,
                borderTopColor: look().palette.hair,
                paddingLeft: 14,
                paddingRight: 6,
                paddingVertical: 6,
                flexDirection: "row",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 8,
              }}
            >
              <Text style={{ flex: 1, fontSize: look().type.text_xsmall.size, color: look().palette.faint, numberOfLines: 1 }}>
                {props.byAgent() ? ct("capabilities.by_agent") : ct("capabilities.by_pi")}
              </Text>
              <SmeltButton
                quiet
                small
                trailingIcon="icons/arrow-right.svg"
                label={props.byAgent() ? ct("capabilities.edit_agent") : ct("capabilities.manage_library")}
                onPress={() => {
                  open.set(false);
                  if (props.onManage) props.onManage();
                }}
              />
            </View>
          </If>
        </View>
      </SmeltPopover>
    </If>
  );
}

/** 能力弹层的分组标题：图标、名字，数量靠右。 */
function CapabilityHeader(props: { icon: string; label: string; count: number }) {
  const look = theme;
  return (
    <View style={{ height: 26, paddingHorizontal: 8, flexDirection: "row", alignItems: "center", gap: 6 }}>
      <SmeltIcon path={props.icon} size={12} color={look().palette.faint} />
      <Text
        style={{
          flex: 1,
          fontSize: look().type.small_medium.size,
          fontWeight: look().type.small_medium.weight,
          color: look().palette.faint,
        }}
      >
        {props.label}
      </Text>
      <Text style={{ fontSize: look().type.small.size, color: look().palette.faint }}>{props.count}</Text>
    </View>
  );
}

/** 上下文用到这么多就提醒（`need` 色并写出百分比），到第二档换 `bad`。 */
const USAGE_WARN_PCT = 65;
const USAGE_BAD_PCT = 85;

export function UsagePopover(props: {
  usage: () => [number, number] | null;
  breakdown: () => any | null;
  compacting: () => boolean;
  visible?: () => boolean;
  supportsCompaction: () => boolean;
  onCompact: () => void;
}) {
  const look = theme;
  const open = signal(false);
  const pct = computed(() => {
    const u = props.usage();
    return u ? usagePercent(u[0], u[1]) : 0;
  });

  const rows = computed(() => {
    const b = props.breakdown();
    if (!b) return [];
    // 五类固定开销各占一个分类色；后三类是对话本身，用三档灰，相邻两段深浅错开。
    const list: Array<{ label: string; tokens: number; colorClass: string }> = [
      { label: ct("usage.system"), tokens: b.systemPrompt ?? 0, colorClass: "bg-chart-1" },
      { label: ct("usage.tools"), tokens: b.toolsDefinition ?? 0, colorClass: "bg-chart-2" },
      { label: ct("usage.rules"), tokens: b.rules ?? 0, colorClass: "bg-chart-3" },
      { label: ct("usage.skills"), tokens: b.skills ?? 0, colorClass: "bg-chart-4" },
      { label: ct("usage.mcp"), tokens: b.mcpDynamic ?? 0, colorClass: "bg-chart-5" },
      { label: ct("usage.subagents"), tokens: b.subagent ?? 0, colorClass: "bg-foreground" },
      { label: ct("usage.summarized"), tokens: b.summarized ?? 0, colorClass: "bg-faint" },
      { label: ct("usage.conversation"), tokens: b.conversation ?? 0, colorClass: "bg-muted-foreground" },
    ];
    return list.filter((r) => r.tokens > 0);
  });

  const totalBreakdown = computed(() => {
    const u = props.usage();
    const sum = rows().reduce((acc, r) => acc + r.tokens, 0);
    return Math.max(u?.[1] ?? 0, u?.[0] ?? 0, sum, 1);
  });

  const meterColor = computed(() => {
    const p = pct();
    if (p >= USAGE_BAD_PCT) return look().palette.bad;
    if (p >= USAGE_WARN_PCT) return look().palette.need;
    return look().palette.muted;
  });
  const meterTrack = computed(() => colorWithAlpha(look().palette.muted, 0.28));

  return (
    <If when={props.compacting() || props.usage() != null}>
      <SmeltPopover
        open={open()}
        onOpenChange={({ open: next }: any) => open.set(Boolean(next))}
        placement="top"
        align="end"
        offset={8}
        popupClass="rounded-xl border border-border bg-popover p-3 shadow-md"
        popupStyle={{ width: 280 }}
        trigger={
          <SmeltButton
            quiet
            small
            selected={open()}
            tooltip={`${ct("usage.title")} ${pct()}%`}
            accessibilityLabel={`${ct("usage.title")} ${pct()}%`}
            style={{ borderRadius: 999, paddingLeft: 7, paddingRight: 7 }}
          >
            <View style={{ flexDirection: "row", alignItems: "center", gap: 5 }}>
              <If
                when={props.compacting()}
                else={
                  // 用量饼：灰底圆上按比例填一块扇形，平时只画它，具体数字在悬停提示里
                  <View
                    style={{
                      width: 13,
                      height: 13,
                      borderRadius: 999,
                      backgroundImage: `conic-gradient(${meterColor()} 0%, ${meterColor()} ${pct()}%, ${meterTrack()} ${pct()}%, ${meterTrack()} 100%)`,
                    }}
                  />
                }
              >
                <SmeltIcon
                  spin={props.visible ? props.visible() : true}
                  size={12}
                  color={look().palette.muted}
                />
              </If>
              {/* 快满了才把百分比写出来；压缩中写「压缩中」 */}
              <If when={props.compacting() || pct() >= USAGE_WARN_PCT}>
                <Text
                  style={{
                    fontSize: look().type.text_xsmall.size,
                    color: props.compacting() ? look().palette.muted : meterColor(),
                  }}
                >
                  {props.compacting() ? ct("usage.compacting") : `${pct()}%`}
                </Text>
              </If>
            </View>
          </SmeltButton>
        }
      >
        <View style={{ gap: 10 }}>
          <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center" }}>
            <Text
              style={{
                fontSize: look().type.ui_medium.size,
                fontWeight: look().type.ui_medium.weight,
                color: look().palette.text,
              }}
            >
              {ct("usage.title")}
            </Text>
            <If when={props.usage() != null}>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {`${compactTokenCount(props.usage()![0])} / ${compactTokenCount(props.usage()![1])}`}
              </Text>
            </If>
          </View>

          {/* 分类占比进度条 */}
          <View
            style={{
              width: "100%",
              height: 6,
              borderRadius: 3,
              backgroundColor: look().palette.soft,
              flexDirection: "row",
              overflow: "hidden",
              gap: 1,
            }}
          >
            <If
              when={rows().length > 0}
              else={
                <View
                  style={{
                    width: `${pct()}%`,
                    height: "100%",
                    backgroundColor: meterColor(),
                    borderRadius: 3,
                  }}
                />
              }
            >
              <Each of={rows()}>
                {(row) => (
                  <View
                    class={row().colorClass}
                    style={{
                      width: `${Math.max(1.5, (row().tokens / totalBreakdown()) * 100)}%`,
                      height: "100%",
                    }}
                  />
                )}
              </Each>
            </If>
          </View>

          <If when={rows().length > 0}>
            <View style={{ gap: 5 }}>
              <Each of={rows()}>
                {(row) => (
                  <View style={{ flexDirection: "row", justifyContent: "space-between", alignItems: "center" }}>
                    <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
                      <View
                        class={`rounded-full ${row().colorClass}`}
                        style={{ width: 6, height: 6 }}
                      />
                      <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                        {row().label}
                      </Text>
                    </View>
                    <Text
                      style={{
                        fontFamily: look().fonts?.mono,
                        fontSize: look().type.text_xsmall.size,
                        color: look().palette.text,
                      }}
                    >
                      {compactTokenCount(row().tokens)}
                    </Text>
                  </View>
                )}
              </Each>
            </View>
          </If>

          <If when={props.supportsCompaction()}>
            <View style={{ borderTopWidth: 1, borderTopColor: look().palette.hair, paddingTop: 8 }}>
              <SmeltButton
                fill
                small
                disabled={props.compacting()}
                label={props.compacting() ? ct("usage.compacting") : ct("usage.compact")}
                onPress={() => {
                  open.set(false);
                  props.onCompact();
                }}
              />
            </View>
          </If>
        </View>
      </SmeltPopover>
    </If>
  );
}
