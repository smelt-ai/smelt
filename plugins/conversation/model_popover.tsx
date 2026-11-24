// 输入栏工具弹层：模型与推理强度/配置弹层、能力（Skills/MCP）弹层、上下文用量弹层。
import { computed, Scroll, signal, Text, View } from "nsh";
import { Input, Switch } from "nsh/base";
import { SmeltButton, SmeltIcon, SmeltPopover, theme } from "smelt/sdk";
import { ct } from "./i18n.ts";
import {
  buildVisibleModels,
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
  turnActive: () => boolean;
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
  const otherConfigs = computed(() => (props.configs() ?? []).filter((c) => !isThinkingConfig(c)));

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
        if (!next) query.set("");
      }}
      placement="top"
      align="start"
      offset={8}
      popupClass="rounded-xl border border-border bg-popover p-0 shadow-lg overflow-hidden"
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
                color: pendingNotice()
                  ? look().palette.need
                  : open()
                    ? look().palette.text
                    : look().palette.muted,
                numberOfLines: 1,
              }}
            >
              {label()}
            </Text>
            <If when={thinkingMeta() != null && Boolean(thinkingMeta()?.currentName)}>
              <Text
                style={{
                  fontSize: look().type.text_xsmall.size,
                  color: pendingNotice() ? look().palette.need : look().palette.faint,
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
                  style={{
                    flexDirection: "row",
                    alignItems: "center",
                    gap: 4,
                    paddingHorizontal: 10,
                    paddingTop: 8,
                    paddingBottom: 2,
                  }}
                >
                  <Each of={providerGroups()}>
                    {(group) => {
                      const active = computed(() => currentGroupId() === group().id);
                      return (
                        <View
                          onPress={() => activeProvider.set(group().id)}
                          class={active() ? undefined : "hover:bg-accent"}
                          style={{
                            height: 24,
                            paddingHorizontal: 10,
                            borderRadius: 99,
                            justifyContent: "center",
                            alignItems: "center",
                            backgroundColor: active()
                              ? look().palette.ink
                              : look().palette.row_hover,
                          }}
                        >
                          <Text
                            style={{
                              fontSize: look().type.small_medium.size,
                              fontWeight: look().type.small_medium.weight,
                              color: active() ? look().palette.on_ink : look().palette.muted,
                            }}
                          >
                            {group().name}
                          </Text>
                        </View>
                      );
                    }}
                  </Each>
                </View>
              </Scroll>
            </If>

            {/* 3. 模型列表 */}
            <Scroll style={{ maxHeight: 192 }}>
              <View style={{ padding: 6, gap: 2 }}>
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
                          style={{
                            height: 30,
                            paddingHorizontal: 10,
                            borderRadius: look().radius.row,
                            flexDirection: "row",
                            alignItems: "center",
                            gap: 8,
                            backgroundColor: selected()
                              ? look().palette.row_selected
                              : "transparent",
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

        {/* 4. 推理强度：和下面的配置项同一种分段控件，标题旁带「下轮生效」 */}
        <If when={thinkingConfig() != null}>
          <View
            style={{
              paddingHorizontal: 12,
              paddingTop: 10,
              paddingBottom: 12,
              borderTopWidth: 1,
              borderTopColor: look().palette.hair,
              gap: 8,
            }}
          >
            <View style={{ flexDirection: "row", alignItems: "center", gap: 6 }}>
              <Text
                style={{
                  fontSize: look().type.text_xsmall_medium.size,
                  fontWeight: look().type.text_xsmall_medium.weight,
                  color: look().palette.muted,
                }}
              >
                {thinkingConfig()?.name || ct("model.thinking")}
              </Text>
              <If when={pendingNotice()}>
                <Text
                  style={{
                    fontSize: look().type.small_medium.size,
                    color: look().palette.need,
                  }}
                >
                  · {ct("model.next_turn")}
                </Text>
              </If>
            </View>
            <ConfigSegments
              options={() => thinkingConfig()?.options ?? []}
              current={() => thinkingConfig()?.current_name}
              onSelect={(value) => {
                const cfg = thinkingConfig();
                if (cfg) props.onSelectConfig(cfg.config_id, value);
              }}
            />
          </View>
        </If>

        {/* 5. 其余会话配置（权限模式分段控件 / 布尔开关） */}
        <If when={otherConfigs().length > 0}>
          <View
            style={{
              paddingHorizontal: 12,
              paddingTop: 10,
              paddingBottom: 12,
              borderTopWidth: 1,
              borderTopColor: look().palette.hair,
              gap: 10,
            }}
          >
            <Each of={otherConfigs()}>
              {(cfg) => (
                <If
                  when={cfg().boolean != null}
                  else={
                    <View style={{ gap: 6 }}>
                      <Text
                        style={{
                          fontSize: look().type.text_xsmall_medium.size,
                          fontWeight: look().type.text_xsmall_medium.weight,
                          color: look().palette.muted,
                        }}
                      >
                        {cfg().name || cfg().config_id}
                      </Text>
                      <ConfigSegments
                        options={() => cfg().options ?? []}
                        current={() => cfg().current_name}
                        onSelect={(value) => props.onSelectConfig(cfg().config_id, value)}
                      />
                    </View>
                  }
                >
                  <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between" }}>
                    <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                      {cfg().name || cfg().config_id}
                    </Text>
                    <Switch
                      checked={Boolean(cfg().boolean)}
                      accessibilityLabel={cfg().name || cfg().config_id}
                      onChange={({ checked }: any) => {
                        const val = Boolean(checked);
                        props.onSelectConfig(cfg().config_id, val ? "true" : "false", val);
                      }}
                      trackStyle={{
                        width: 34,
                        height: 20,
                        borderRadius: 10,
                        padding: 2,
                        backgroundColor: Boolean(cfg().boolean)
                          ? look().palette.link
                          : look().palette.soft,
                        display: "flex",
                        flexDirection: "row",
                        alignItems: "center",
                        justifyContent: Boolean(cfg().boolean) ? "flex-end" : "flex-start",
                      }}
                      thumbStyle={{
                        width: 16,
                        height: 16,
                        borderRadius: 8,
                        backgroundColor: look().palette.on_ink,
                      }}
                    />
                  </View>
                </If>
              )}
            </Each>
          </View>
        </If>
      </View>
    </SmeltPopover>
  );
}


/** 配置项的档位：分段控件，选中的那段浮起（`surface` 底、`hair_strong` 边），其余悬停变底。 */
function ConfigSegments(props: {
  options: () => any[];
  current: () => string | null | undefined;
  onSelect: (value: string) => void;
}) {
  const look = theme;
  return (
    <View
      style={{
        flexDirection: "row",
        flexWrap: "wrap",
        gap: 3,
        padding: 3,
        borderRadius: 9,
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
              style={{
                flexGrow: 1,
                height: 26,
                paddingHorizontal: 8,
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
  skills: () => any[];
  mcpServers: () => any[];
  byAgent: () => boolean;
  onUseSkill: (name: string) => void;
  onManage?: () => void;
}) {
  const look = theme;
  const open = signal(false);
  const total = computed(() => (props.skills()?.length ?? 0) + (props.mcpServers()?.length ?? 0));

  return (
    <SmeltPopover
      open={open()}
      onOpenChange={({ open: next }: any) => open.set(Boolean(next))}
      placement="top"
      align="start"
      offset={8}
      popupClass="rounded-xl border border-border bg-popover p-0 shadow-lg overflow-hidden"
      popupStyle={{ width: 320 }}
      trigger={
        <SmeltButton
          quiet
          small
          selected={open()}
          icon="icons/puzzle.svg"
          label={total() > 0 ? String(total()) : undefined}
          tooltip={ct("capabilities.title")}
          accessibilityLabel={ct("capabilities.title")}
          style={{ borderRadius: 999 }}
        />
      }
    >
      <View>
        <If
          when={total() > 0}
          else={
            <Text
              style={{
                paddingHorizontal: 14,
                paddingVertical: 18,
                fontSize: look().type.ui.size,
                color: look().palette.muted,
                textAlign: "center",
              }}
            >
              {ct("capabilities.none")}
            </Text>
          }
        >
          <Scroll style={{ maxHeight: 340 }}>
            <View style={{ padding: 8, gap: 4 }}>
              <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between", paddingHorizontal: 8, paddingTop: 2, paddingBottom: 2 }}>
                <Text
                  style={{
                    fontSize: look().type.small_medium.size,
                    fontWeight: look().type.small_medium.weight,
                    color: look().palette.faint,
                  }}
                >
                  {ct("capabilities.skills")}
                </Text>
                <Text style={{ fontSize: look().type.small.size, color: look().palette.faint }}>
                  {props.skills().length}
                </Text>
              </View>
              <If
                when={(props.skills()?.length ?? 0) > 0}
                else={
                  <Text style={{ paddingHorizontal: 8, paddingVertical: 4, fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                    {ct("capabilities.empty_group")}
                  </Text>
                }
              >
                <Each of={props.skills()}>
                  {(skill) => (
                    <View
                      onPress={() => {
                        open.set(false);
                        props.onUseSkill(skill().name);
                      }}
                      class="hover:bg-accent"
                      style={{ paddingHorizontal: 8, paddingVertical: 6, borderRadius: 6, gap: 2 }}
                    >
                      <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                        {skill().name}
                      </Text>
                      <If when={Boolean(skill().description)}>
                        <Text
                          style={{
                            fontSize: look().type.text_xsmall.size,
                            color: look().palette.muted,
                            numberOfLines: 1,
                          }}
                        >
                          {String(skill().description).replace(/\s+/g, " ").trim()}
                        </Text>
                      </If>
                    </View>
                  )}
                </Each>
              </If>

              <View style={{ height: 1, backgroundColor: look().palette.hair, marginVertical: 4 }} />

              <View style={{ flexDirection: "row", alignItems: "center", justifyContent: "space-between", paddingHorizontal: 8, paddingBottom: 2 }}>
                <Text
                  style={{
                    fontSize: look().type.small_medium.size,
                    fontWeight: look().type.small_medium.weight,
                    color: look().palette.faint,
                  }}
                >
                  {ct("capabilities.mcp")}
                </Text>
                <Text style={{ fontSize: look().type.small.size, color: look().palette.faint }}>
                  {props.mcpServers().length}
                </Text>
              </View>
              <If
                when={(props.mcpServers()?.length ?? 0) > 0}
                else={
                  <Text style={{ paddingHorizontal: 8, paddingVertical: 4, fontSize: look().type.text_xsmall.size, color: look().palette.faint }}>
                    {ct("capabilities.empty_group")}
                  </Text>
                }
              >
                <Each of={props.mcpServers()}>
                  {(srv) => (
                    <View style={{ paddingHorizontal: 8, paddingVertical: 5 }}>
                      <Text style={{ fontSize: look().type.ui.size, color: look().palette.text }}>
                        {srv().name}
                      </Text>
                    </View>
                  )}
                </Each>
              </If>
            </View>
          </Scroll>
        </If>

        <If when={Boolean(props.onManage)}>
          <View
            style={{
              borderTopWidth: 1,
              borderTopColor: look().palette.hair,
              paddingHorizontal: 10,
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
  );
}

export function UsagePopover(props: {
  usage: () => [number, number] | null;
  breakdown: () => any | null;
  compacting: () => boolean;
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
    const list: Array<{ label: string; tokens: number; colorClass: string }> = [
      { label: ct("usage.system"), tokens: b.systemPrompt ?? 0, colorClass: "bg-chart-1" },
      { label: ct("usage.tools"), tokens: b.toolsDefinition ?? 0, colorClass: "bg-chart-2" },
      { label: ct("usage.rules"), tokens: b.rules ?? 0, colorClass: "bg-chart-3" },
      { label: ct("usage.skills"), tokens: b.skills ?? 0, colorClass: "bg-chart-4" },
      { label: ct("usage.mcp"), tokens: b.mcpDynamic ?? 0, colorClass: "bg-chart-5" },
      { label: ct("usage.subagents"), tokens: b.subagent ?? 0, colorClass: "bg-chart-2" },
      { label: ct("usage.summarized"), tokens: b.summarized ?? 0, colorClass: "bg-chart-3" },
      { label: ct("usage.conversation"), tokens: b.conversation ?? 0, colorClass: "bg-chart-1" },
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
    if (p >= 85) return look().palette.bad;
    if (p >= 65) return look().palette.need;
    return look().palette.muted;
  });

  return (
    <If when={props.compacting() || props.usage() != null}>
      <SmeltPopover
        open={open()}
        onOpenChange={({ open: next }: any) => open.set(Boolean(next))}
        placement="top"
        align="end"
        offset={8}
        popupClass="rounded-xl border border-border bg-popover p-3 shadow-lg"
        popupStyle={{ width: 280 }}
        trigger={
          <SmeltButton
            quiet
            small
            selected={open()}
            tooltip={ct("usage.title")}
            accessibilityLabel={ct("usage.title")}
            style={{ borderRadius: 999, paddingLeft: 8, paddingRight: 8 }}
          >
            <View style={{ flexDirection: "row", alignItems: "center", gap: 5 }}>
              <If
                when={props.compacting()}
                else={
                  <View
                    style={{
                      width: 6,
                      height: 6,
                      borderRadius: 3,
                      backgroundColor: meterColor(),
                    }}
                  />
                }
              >
                <SmeltIcon spin size={11} color={look().palette.run} />
              </If>
              <Text
                style={{
                  fontSize: look().type.text_xsmall.size,
                  color: pct() >= 85 ? look().palette.bad : look().palette.muted,
                }}
              >
                {props.compacting() ? ct("usage.compacting") : `${pct()}%`}
              </Text>
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
