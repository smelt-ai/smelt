// Smelt 宿主设置导航与 8 个宿主内置设置页（运行在不可卸载的 NSH 主壳 VM 中）。
// 所有数据与本地化文案由宿主 `crates/smelt` 经 `smelt.state.shell.host_settings` 推送，
// 交互经 `shell.host_settings_action`（受 `SHELL_OWNER` 校验）回传宿主。
import { computed, effect, host, Scroll, signal, Text, View } from "nsh";
import { Button as BaseButton, Combobox, Input, Progress, Slider } from "nsh/base";
import {
  DIALOG_SURFACE_CLASS,
  INPUT_CLASS,
  MENU_CLASS,
  MENU_ITEM_CLASS,
  safeArea,
  shellState,
  SmeltButton,
  SmeltDialog,
  SmeltIcon,
  SmeltNativeWindowDrag,
  SmeltSelect,
  SmeltSwitch,
  stableMemo,
  theme as currentTheme,
} from "smelt/sdk";

const TOP_BAR_HEIGHT = 34;

export const HOST_SETTINGS_IDS = new Set([
  "smelt/appearance",
  "smelt/shortcuts",
  "smelt/panel-plugins",
  "smelt.language/settings",
  "smelt.terminal/settings",
  "smelt.maintenance/cli",
  "smelt.maintenance/cleanup",
  "smelt.updater/about",
]);

export function isHostSettingsPage(id: string): boolean {
  return HOST_SETTINGS_IDS.has(id);
}

function sendSettingsAction(payload: Record<string, unknown>) {
  host.call("shell.host_settings_action", payload).catch(() => {});
}

// ===================== 类型定义 =====================

interface NavLeafSnapshot {
  id: string;
  title: string;
  icon: string;
  keywords: string[];
  attention: boolean;
}

interface NavCategorySnapshot {
  title: string;
  keywords: string[];
  children: NavLeafSnapshot[];
}

interface NavSnapshot {
  selected: string | null;
  categories: NavCategorySnapshot[];
  labels: {
    back: string;
    search: string;
    results_none: string;
    results_one: string;
    results_other: string;
  };
}

interface SelectOption {
  value: string;
  title: string;
}

interface ThemeOption {
  value: string;
  title: string;
  group: string;
  keywords: string[];
  swatch: [string, string, string] | null;
}

interface HostSettingsSnapshot {
  nav: NavSnapshot;
  page: any;
}

function hostSettingsState(): HostSettingsSnapshot | null {
  const snap = shellState() as any;
  return (snap?.host_settings as HostSettingsSnapshot | null | undefined) ?? null;
}

// ===================== 通用设置骨架 =====================

function SettingsPageLayout(props: { pageKey?: string; children: any }) {
  let scrollRef: any = null;
  effect(() => {
    const k = props.pageKey;
    if (k) scrollRef?.scrollTo?.({ y: 0, animated: false });
  });
  return (
    <Scroll
      ref={(api: any) => {
        scrollRef = api;
      }}
      style={{ flex: 1, width: "100%", height: "100%" }}
    >
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
            width: "100%",
            maxWidth: 720,
            paddingLeft: 24,
            paddingRight: 24,
            paddingTop: 16,
            paddingBottom: 32,
            display: "flex",
            flexDirection: "column",
            gap: 24,
          }}
        >
          {props.children}
        </View>
      </View>
    </Scroll>
  );
}

function SettingsSectionCard(props: {
  title?: string | null;
  description?: string | null;
  children?: any;
}) {
  const theme = currentTheme;
  const hasHeader = () => Boolean(props.title || props.description);
  return (
    <View style={{ display: "flex", flexDirection: "column", gap: 8, width: "100%" }}>
      <If when={hasHeader()}>
        <View
          style={{
            display: "flex",
            flexDirection: "column",
            gap: 2,
            paddingLeft: 16,
            paddingRight: 16,
          }}
        >
          <If when={Boolean(props.title)}>
            <Text
              style={{
                fontSize: theme().type.ui_medium.size,
                lineHeight: theme().type.ui_medium.line_height,
                fontWeight: theme().type.ui_medium.weight,
                color: theme().palette.muted,
              }}
            >
              {props.title ?? ""}
            </Text>
          </If>
          <If when={Boolean(props.description)}>
            <Text
              style={{
                fontSize: theme().type.text_xsmall.size,
                lineHeight: theme().type.text_xsmall.line_height,
                color: theme().palette.muted,
              }}
            >
              {props.description ?? ""}
            </Text>
          </If>
        </View>
      </If>
      <View
        style={{
          display: "flex",
          flexDirection: "column",
          paddingTop: 4,
          paddingBottom: 4,
          backgroundColor: theme().palette.surface,
          borderWidth: 1,
          borderColor: theme().palette.hair,
          borderRadius: theme().radius.xl,
          width: "100%",
        }}
      >
        {props.children}
      </View>
    </View>
  );
}

function SectionDivider() {
  const theme = currentTheme;
  return (
    <View
      style={{
        marginLeft: 16,
        marginRight: 16,
        height: 1,
        backgroundColor: theme().palette.hair,
      }}
    />
  );
}

function SectionCell(props: { dividerAbove?: boolean; children: any }) {
  return (
    <>
      <If when={Boolean(props.dividerAbove)}>
        <SectionDivider />
      </If>
      <View style={{ paddingLeft: 16, paddingRight: 16, paddingTop: 8, paddingBottom: 8 }}>
        {props.children}
      </View>
    </>
  );
}

function SettingsRowItem(props: {
  title: string;
  description?: string | null;
  error?: string | null;
  children?: any;
}) {
  const theme = currentTheme;
  return (
    <View
      style={{
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        justifyContent: "space-between",
        gap: 16,
        minHeight: 28,
        width: "100%",
      }}
    >
      <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
        <Text
          style={{
            fontSize: theme().type.ui.size,
            lineHeight: theme().type.ui.line_height,
            color: theme().palette.text,
          }}
        >
          {props.title}
        </Text>
        <If when={Boolean(props.description)}>
          <Text
            style={{
              fontSize: theme().type.text_small.size,
              lineHeight: theme().type.text_small.line_height,
              color: theme().palette.muted,
            }}
          >
            {props.description ?? ""}
          </Text>
        </If>
        <If when={Boolean(props.error)}>
          <Text
            style={{
              fontSize: theme().type.text_small.size,
              lineHeight: theme().type.text_small.line_height,
              color: theme().palette.bad,
            }}
          >
            {props.error ?? ""}
          </Text>
        </If>
      </View>
      <If when={Boolean(props.children)}>
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            flexShrink: 0,
            alignItems: "center",
            gap: 8,
          }}
        >
          {props.children}
        </View>
      </If>
    </View>
  );
}

function SettingsSelectControl(props: {
  items: SelectOption[];
  value: string;
  label: string;
  minWidth?: number;
  onChange: (value: string) => void;
}) {
  const theme = currentTheme;
  const currentTitle = computed(() => {
    const found = props.items.find((o) => o.value === props.value);
    return found?.title ?? props.value;
  });
  return (
    <SmeltSelect
      items={props.items}
      value={props.value}
      accessibilityLabel={props.label}
      onChange={({ value }: { value: string | null }) => {
        if (value !== null) props.onChange(value);
      }}
      itemClass={`${MENU_ITEM_CLASS} py-1 text-sm text-foreground`}
      trigger={
        <View
          style={{
            minWidth: props.minWidth ?? 180,
            maxWidth: 260,
            height: 28,
            paddingLeft: 10,
            paddingRight: 10,
            borderRadius: theme().radius.sm,
            borderWidth: 1,
            borderColor: theme().palette.hair_strong,
            backgroundColor: theme().palette.soft,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 8,
          }}
        >
          <Text
            style={{
              flex: 1,
              minWidth: 0,
              fontSize: theme().type.ui.size,
              color: theme().palette.text,
              numberOfLines: 1,
            }}
          >
            {currentTitle()}
          </Text>
          <SmeltIcon path="icons/chevron-down.svg" size={12} color={theme().palette.muted} />
        </View>
      }
    />
  );
}

function SettingsLinkButton(props: { label: string; url: string; onPress?: () => void }) {
  const theme = currentTheme;
  return (
    <BaseButton
      onPress={() => {
        if (props.onPress) {
          props.onPress();
        } else {
          sendSettingsAction({ page: "common", kind: "open_url", url: props.url });
        }
      }}
      style={{
        paddingLeft: 4,
        paddingRight: 4,
        paddingTop: 2,
        paddingBottom: 2,
        borderRadius: theme().radius.sm,
        backgroundColor: "transparent",
        cursor: "pointer",
      }}
      hoverStyle={{
        backgroundColor: theme().palette.row_hover,
      }}
    >
      <Text
        style={{
          fontSize: theme().type.ui.size,
          lineHeight: theme().type.ui.line_height,
          color: theme().palette.accent,
        }}
      >
        {props.label}
      </Text>
    </BaseButton>
  );
}

function SettingsSliderControl(props: {
  value: number;
  min: number;
  max: number;
  step?: number;
  label: string;
  formattedValue: string;
  onChange: (value: number) => void;
}) {
  const theme = currentTheme;
  return (
    <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
      <Slider
        value={props.value}
        min={props.min}
        max={props.max}
        step={props.step ?? 1}
        accessibilityLabel={props.label}
        onChange={({ value }: { value: number | [number, number] }) => {
          if (typeof value === "number") props.onChange(value);
        }}
        style={{ width: 200, display: "flex", flexDirection: "row", alignItems: "center" }}
        trackStyle={{
          height: 16,
          flex: 1,
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          paddingLeft: 6,
          paddingRight: 6,
        }}
        indicatorStyle={{
          height: 4,
          flex: 1,
          borderRadius: 999,
          backgroundColor: theme().palette.soft,
        }}
        rangeStyle={{
          height: 4,
          borderRadius: 999,
          backgroundColor: theme().palette.accent,
        }}
        thumbStyle={{
          width: 12,
          height: 12,
          borderRadius: 999,
          backgroundColor: theme().palette.text,
          borderWidth: 1,
          borderColor: theme().palette.hair_strong,
        }}
      />
      <Text
        style={{
          width: 44,
          textAlign: "right",
          fontSize: theme().type.text_xsmall.size,
          color: theme().palette.muted,
        }}
      >
        {props.formattedValue}
      </Text>
    </View>
  );
}

// ===================== 设置导航（smelt/settings-nav） =====================

function filterNavCategories(
  categories: NavCategorySnapshot[],
  rawQuery: string,
): NavCategorySnapshot[] {
  const query = rawQuery.trim().toLowerCase();
  if (!query) return categories;
  const terms = query.split(/\s+/).filter(Boolean);
  const out: NavCategorySnapshot[] = [];
  for (const cat of categories) {
    const catValues = [cat.title, ...cat.keywords].map((s) => s.toLowerCase());
    const catExact = catValues.some((v) => v.includes(query));
    const children = cat.children.filter((child) => {
      if (catExact) return true;
      const childValues = [child.title, ...child.keywords].map((s) => s.toLowerCase());
      const allValues = [...catValues, ...childValues];
      const allMatch = terms.every((term) => allValues.some((v) => v.includes(term)));
      const childMatch = terms.some((term) => childValues.some((v) => v.includes(term)));
      return allMatch && childMatch;
    });
    if (children.length > 0) {
      out.push({ ...cat, children });
    }
  }
  return out;
}

export function SettingsNavPanel() {
  const theme = currentTheme;
  const area = stableMemo(() => safeArea("smelt/settings-nav") as any);
  const query = signal("");
  const nav = computed(() => hostSettingsState()?.nav ?? null);
  const filtered = computed(() => filterNavCategories(nav()?.categories ?? [], query()));
  const resultCount = computed(() =>
    filtered().reduce((sum, cat) => sum + cat.children.length, 0),
  );
  const resultText = computed(() => {
    const labels = nav()?.labels;
    if (!labels) return "";
    const n = resultCount();
    if (n === 0) return labels.results_none;
    if (n === 1) return labels.results_one;
    return labels.results_other.replace("%{count}", String(n));
  });

  return (
    <View
      style={{
        display: "flex",
        flexDirection: "column",
        flex: 1,
        width: "100%",
        height: "100%",
        minHeight: 0,
        minWidth: 0,
      }}
    >
      {/* 顶栏安全区（红黄绿按钮 + 窗口拖拽，无底线） */}
      <If when={Boolean(area().title_bar)}>
        <View style={{ width: "100%", height: TOP_BAR_HEIGHT, flexShrink: 0 }}>
          <SmeltNativeWindowDrag style={{ width: "100%", height: "100%" }} />
        </View>
      </If>

      {/* 固定顶部的返回与搜索框 */}
      <View
        style={{
          display: "flex",
          flexDirection: "column",
          gap: 6,
          paddingTop: 12,
          paddingLeft: 12,
          paddingRight: 12,
          paddingBottom: 8,
          flexShrink: 0,
        }}
      >
        <BaseButton
          onPress={() => sendSettingsAction({ page: "nav", kind: "back" })}
          style={{
            height: 30,
            paddingLeft: 8,
            paddingRight: 8,
            borderRadius: theme().radius.row,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 8,
            backgroundColor: "transparent",
            cursor: "default",
          }}
          hoverStyle={{ backgroundColor: theme().palette.row_hover }}
          activeStyle={{ backgroundColor: theme().palette.row_selected }}
        >
          <SmeltIcon path="icons/chevron-left.svg" size={16} color={theme().palette.muted} />
          <Text
            style={{
              flex: 1,
              minWidth: 0,
              fontSize: theme().type.ui.size,
              lineHeight: theme().type.ui.line_height,
              color: theme().palette.text,
              numberOfLines: 1,
            }}
          >
            {nav()?.labels?.back ?? ""}
          </Text>
        </BaseButton>

        <View
          class={INPUT_CLASS}
          style={{
            height: 30,
            paddingLeft: 8,
            paddingRight: 8,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 6,
          }}
        >
          <SmeltIcon path="icons/search.svg" size={14} color={theme().palette.muted} />
          <Input
            value={query()}
            placeholder={nav()?.labels?.search ?? ""}
            onChange={({ value }: { value: string }) => query.set(value)}
            style={{
              flex: 1,
              minWidth: 0,
              height: 28,
              fontSize: theme().type.ui.size,
              color: theme().palette.text,
            }}
          />
        </View>

        <If when={query().trim().length > 0}>
          <Text
            style={{
              paddingLeft: 4,
              paddingRight: 4,
              fontSize: theme().type.text_xsmall.size,
              lineHeight: theme().type.text_xsmall.line_height,
              color: theme().palette.muted,
            }}
          >
            {resultText()}
          </Text>
        </If>
      </View>

      {/* 可滚动的分组列表 */}
      <Scroll style={{ flex: 1, minHeight: 0, width: "100%" }}>
        <View
          style={{
            display: "flex",
            flexDirection: "column",
            gap: 16,
            paddingLeft: 12,
            paddingRight: 12,
            paddingBottom: 12,
          }}
        >
          <Each of={filtered()} key={(cat: NavCategorySnapshot) => cat.title}>
            {(cat: () => NavCategorySnapshot) => (
              <View style={{ display: "flex", flexDirection: "column" }}>
                <View
                  style={{
                    height: 24,
                    paddingLeft: 8,
                    paddingRight: 8,
                    display: "flex",
                    flexDirection: "row",
                    alignItems: "center",
                  }}
                >
                  <Text
                    style={{
                      fontSize: theme().type.text_xsmall_medium.size,
                      lineHeight: theme().type.text_xsmall_medium.line_height,
                      fontWeight: theme().type.text_xsmall_medium.weight,
                      color: theme().palette.muted,
                    }}
                  >
                    {cat().title}
                  </Text>
                </View>
                <Each of={cat().children} key={(leaf: NavLeafSnapshot) => leaf.id}>
                  {(leaf: () => NavLeafSnapshot) => {
                    const selected = computed(() => nav()?.selected === leaf().id);
                    return (
                      <BaseButton
                        onPress={() =>
                          sendSettingsAction({ page: "nav", kind: "select", id: leaf().id })
                        }
                        style={{
                          height: 30,
                          paddingLeft: 8,
                          paddingRight: 8,
                          borderRadius: theme().radius.row,
                          display: "flex",
                          flexDirection: "row",
                          alignItems: "center",
                          gap: 8,
                          backgroundColor: selected()
                            ? theme().palette.row_selected
                            : "transparent",
                          cursor: "default",
                        }}
                        hoverStyle={{
                          backgroundColor: selected()
                            ? theme().palette.row_selected
                            : theme().palette.row_hover,
                        }}
                        activeStyle={{
                          backgroundColor: theme().palette.row_selected,
                        }}
                      >
                        <SmeltIcon
                          path={leaf().icon}
                          size={16}
                          color={selected() ? theme().palette.text : theme().palette.muted}
                        />
                        <Text
                          style={{
                            flex: 1,
                            minWidth: 0,
                            fontSize: theme().type.ui.size,
                            lineHeight: theme().type.ui.line_height,
                            fontWeight: selected()
                              ? theme().type.ui_medium.weight
                              : theme().type.ui.weight,
                            color: theme().palette.text,
                            numberOfLines: 1,
                          }}
                        >
                          {leaf().title}
                        </Text>
                        <If when={leaf().attention}>
                          <View
                            style={{
                              width: 6,
                              height: 6,
                              borderRadius: 999,
                              backgroundColor: theme().palette.bad,
                              flexShrink: 0,
                            }}
                          />
                        </If>
                      </BaseButton>
                    );
                  }}
                </Each>
              </View>
            )}
          </Each>
        </View>
      </Scroll>
    </View>
  );
}

// ===================== 8 个宿主设置页 =====================

function LanguageSettingsView(props: { data: () => any }) {
  return (
    <SettingsPageLayout pageKey="language">
      <SettingsSectionCard title={props.data().section_title}>
        <SectionCell>
          <SettingsRowItem
            title={props.data().row_title}
            description={props.data().row_description}
          >
            <SettingsSelectControl
              items={props.data().options ?? []}
              value={props.data().selected ?? ""}
              label={props.data().row_title}
              onChange={(value) => sendSettingsAction({ page: "language", kind: "set", value })}
            />
          </SettingsRowItem>
        </SectionCell>
      </SettingsSectionCard>
    </SettingsPageLayout>
  );
}

function TerminalSettingsView(props: { data: () => any }) {
  const theme = currentTheme;
  return (
    <SettingsPageLayout pageKey="terminal">
      <SettingsSectionCard title={props.data().section_title}>
        <SectionCell>
          <SettingsRowItem
            title={props.data().row_title}
            description={props.data().row_description}
          >
            <SmeltButton
              small={true}
              label="−"
              accessibilityLabel={props.data().smaller_label}
              disabled={props.data().current <= props.data().min}
              onPress={() =>
                sendSettingsAction({
                  page: "terminal",
                  kind: "set_font_px",
                  px: props.data().current - 1,
                })
              }
            />
            <Text
              style={{
                minWidth: 40,
                textAlign: "center",
                fontSize: theme().type.ui.size,
                color: theme().palette.text,
              }}
            >
              {`${props.data().current}px`}
            </Text>
            <SmeltButton
              small={true}
              label="+"
              accessibilityLabel={props.data().larger_label}
              disabled={props.data().current >= props.data().max}
              onPress={() =>
                sendSettingsAction({
                  page: "terminal",
                  kind: "set_font_px",
                  px: props.data().current + 1,
                })
              }
            />
          </SettingsRowItem>
        </SectionCell>
      </SettingsSectionCard>
    </SettingsPageLayout>
  );
}

function CliSettingsView(props: { data: () => any }) {
  return (
    <SettingsPageLayout pageKey="cli">
      <SettingsSectionCard>
        <SectionCell>
          <SettingsRowItem title={props.data().row_title} description={props.data().description}>
            <SmeltButton
              small={true}
              label={props.data().button_label}
              loading={Boolean(props.data().installing)}
              disabled={Boolean(props.data().disabled)}
              onPress={() => sendSettingsAction({ page: "cli", kind: "install" })}
            />
          </SettingsRowItem>
        </SectionCell>
      </SettingsSectionCard>
    </SettingsPageLayout>
  );
}

function CleanupSettingsView(props: { data: () => any }) {
  const theme = currentTheme;
  const confirmOpen = signal(false);
  return (
    <>
      <SettingsPageLayout pageKey="cleanup">
        <SettingsSectionCard>
          <SectionCell>
            <SettingsRowItem title={props.data().row_title} description={props.data().summary}>
              <SmeltButton
                small={true}
                label={props.data().action_label}
                loading={Boolean(props.data().cleaning)}
                disabled={!props.data().can_clean}
                onPress={() => confirmOpen.set(true)}
              />
            </SettingsRowItem>
          </SectionCell>
          <Each of={props.data().items ?? []} key={(item: any) => item.path}>
            {(item: () => any) => (
              <SectionCell dividerAbove={true}>
                <SettingsRowItem title={item().label} description={item().error}>
                  <Text style={{ fontSize: theme().type.ui.size, color: theme().palette.muted }}>
                    {item().size}
                  </Text>
                </SettingsRowItem>
              </SectionCell>
            )}
          </Each>
        </SettingsSectionCard>
      </SettingsPageLayout>

      <SmeltDialog
        open={confirmOpen()}
        role="alertdialog"
        accessibilityLabel={props.data().confirm?.title ?? ""}
        accessibilityDescription={props.data().confirm?.description ?? ""}
        onOpenChange={({ open }: { open: boolean }) => confirmOpen.set(open)}
        class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
        style={{ width: 440, gap: 12 }}
      >
        <If when={confirmOpen()}>
          <Text
            style={{
              fontSize: theme().type.ui_medium.size,
              lineHeight: theme().type.ui_medium.line_height,
              fontWeight: theme().type.ui_medium.weight,
              color: theme().palette.text,
            }}
          >
            {props.data().confirm?.title ?? ""}
          </Text>
          <Text
            style={{
              fontSize: theme().type.ui.size,
              lineHeight: theme().type.ui.line_height,
              color: theme().palette.muted,
            }}
          >
            {props.data().confirm?.description ?? ""}
          </Text>
          <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
            <SmeltButton
              quiet={true}
              small={true}
              label={props.data().confirm?.cancel ?? ""}
              onPress={() => confirmOpen.set(false)}
            />
            <SmeltButton
              danger={true}
              small={true}
              label={props.data().confirm?.action ?? ""}
              onPress={() => {
                confirmOpen.set(false);
                sendSettingsAction({ page: "cleanup", kind: "clean" });
              }}
            />
          </View>
        </If>
      </SmeltDialog>
    </>
  );
}

function AboutSettingsView(props: { data: () => any }) {
  const theme = currentTheme;
  const upd = computed(() => props.data().update ?? null);
  const statusColor = computed(() => {
    const tone = upd()?.status_tone;
    if (tone === "ok") return theme().palette.ok;
    if (tone === "bad") return theme().palette.bad;
    return theme().palette.muted;
  });

  return (
    <SettingsPageLayout pageKey="about">
      <SettingsSectionCard>
        <SectionCell>
          <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            <Text
              style={{
                fontSize: theme().type.ui_medium.size + 2,
                fontWeight: "600",
                color: theme().palette.text,
              }}
            >
              Smelt
            </Text>
            <Text style={{ fontSize: theme().type.ui.size, color: theme().palette.muted }}>
              {props.data().version ?? ""}
            </Text>
          </View>
        </SectionCell>
      </SettingsSectionCard>

      <If when={upd() != null}>
        <SettingsSectionCard title={upd()?.section_title}>
          <SectionCell>
            <View style={{ display: "flex", flexDirection: "column", gap: 4, width: "100%" }}>
              <View
                style={{
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  flexWrap: "wrap",
                  minHeight: 28,
                  gap: 8,
                  width: "100%",
                }}
              >
                <View
                  style={{
                    flex: 1,
                    minWidth: 0,
                    display: "flex",
                    flexDirection: "row",
                    alignItems: "center",
                    gap: 8,
                  }}
                >
                  <View
                    style={{
                      width: 7,
                      height: 7,
                      borderRadius: 999,
                      backgroundColor: statusColor(),
                      flexShrink: 0,
                    }}
                  />
                  <Text
                    style={{
                      fontSize: theme().type.ui_medium.size,
                      fontWeight: theme().type.ui_medium.weight,
                      color: statusColor(),
                      numberOfLines: 1,
                    }}
                  >
                    {upd()?.status_title ?? ""}
                  </Text>
                </View>

                <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
                  <SmeltButton
                    small={true}
                    label={upd()?.check_label ?? ""}
                    disabled={!upd()?.can_check_request}
                    onPress={() => sendSettingsAction({ page: "about", kind: "check" })}
                  />
                  <If when={Boolean(upd()?.move_label)}>
                    <SmeltButton
                      primary={true}
                      small={true}
                      label={upd()?.move_label ?? ""}
                      disabled={Boolean(upd()?.moving_app)}
                      onPress={() => sendSettingsAction({ page: "about", kind: "move_app" })}
                    />
                  </If>
                  <If when={Boolean(upd()?.download_label)}>
                    <SmeltButton
                      primary={true}
                      small={true}
                      label={upd()?.download_label ?? ""}
                      onPress={() => sendSettingsAction({ page: "about", kind: "download" })}
                    />
                  </If>
                  <If when={Boolean(upd()?.restart_label)}>
                    <SmeltButton
                      primary={true}
                      small={true}
                      label={upd()?.restart_label ?? ""}
                      onPress={() => sendSettingsAction({ page: "about", kind: "restart" })}
                    />
                  </If>
                  <If when={Boolean(upd()?.manual_download_label && upd()?.manual_download_url)}>
                    <SmeltButton
                      small={true}
                      label={upd()?.manual_download_label ?? ""}
                      onPress={() =>
                        sendSettingsAction({
                          page: "common",
                          kind: "open_url",
                          url: upd()?.manual_download_url,
                        })
                      }
                    />
                  </If>
                </View>
              </View>

              <Text
                style={{
                  paddingLeft: 15,
                  fontSize: theme().type.text_xsmall.size,
                  lineHeight: theme().type.text_xsmall.line_height,
                  color: theme().palette.muted,
                }}
              >
                {upd()?.status_detail ?? ""}
              </Text>

              <If when={upd()?.progress != null}>
                <View style={{ width: "100%", paddingTop: 4 }}>
                  <Progress
                    value={upd()?.progress?.value ?? 0}
                    indeterminate={Boolean(upd()?.progress?.indeterminate)}
                    trackStyle={{
                      height: 4,
                      borderRadius: 999,
                      overflow: "hidden",
                      backgroundColor: theme().palette.soft,
                    }}
                    indicatorClass={
                      upd()?.progress?.indeterminate
                        ? "h-full w-1/3 rounded-full animate-pulse motion-reduce:animate-none"
                        : "h-full rounded-full"
                    }
                    indicatorStyle={{
                      backgroundColor: theme().palette.accent,
                    }}
                  />
                </View>
              </If>
            </View>
          </SectionCell>

          <SectionCell dividerAbove={true}>
            <SettingsRowItem
              title={upd()?.channel_label ?? ""}
              description={upd()?.channel_description ?? ""}
            >
              <View
                style={{
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  gap: 2,
                  padding: 2,
                  borderRadius: theme().radius.md,
                  backgroundColor: theme().palette.soft,
                  borderWidth: 1,
                  borderColor: theme().palette.hair,
                }}
              >
                <Each of={upd()?.channels ?? []} key={(ch: any) => ch.value}>
                  {(ch: () => any) => {
                    const active = computed(() => upd()?.selected_channel === ch().value);
                    return (
                      <SmeltButton
                        quiet={!active()}
                        small={true}
                        selected={active()}
                        disabled={!upd()?.can_change_channel}
                        label={ch().label}
                        onPress={() =>
                          sendSettingsAction({
                            page: "about",
                            kind: "set_channel",
                            value: ch().value,
                          })
                        }
                      />
                    );
                  }}
                </Each>
              </View>
            </SettingsRowItem>
          </SectionCell>

          <SectionCell dividerAbove={true}>
            <SettingsRowItem
              title={upd()?.auto_install_label ?? ""}
              description={upd()?.auto_install_description ?? ""}
            >
              <SmeltSwitch
                checked={Boolean(upd()?.auto_install)}
                accessibilityLabel={upd()?.auto_install_label ?? ""}
                onChange={(checked) =>
                  sendSettingsAction({ page: "about", kind: "set_auto_install", checked })
                }
              />
            </SettingsRowItem>
          </SectionCell>
        </SettingsSectionCard>
      </If>

      <SettingsSectionCard title={props.data().help?.section_title}>
        <SectionCell>
          <SettingsRowItem title={props.data().help?.docs_row ?? ""}>
            <SettingsLinkButton
              label={props.data().help?.docs_label ?? ""}
              url={props.data().help?.docs_url ?? ""}
            />
          </SettingsRowItem>
        </SectionCell>
        <SectionCell dividerAbove={true}>
          <SettingsRowItem
            title={props.data().help?.report_row ?? ""}
            description={props.data().help?.report_description ?? ""}
          >
            <SmeltButton
              quiet={true}
              small={true}
              label={props.data().help?.open_log_label ?? ""}
              onPress={() => sendSettingsAction({ page: "about", kind: "open_log" })}
            />
            <SettingsLinkButton
              label={props.data().help?.report_label ?? ""}
              url={props.data().help?.report_url ?? ""}
            />
          </SettingsRowItem>
        </SectionCell>
        <SectionCell dividerAbove={true}>
          <SettingsRowItem title={props.data().help?.source_row ?? ""}>
            <SettingsLinkButton
              label={props.data().help?.source_label ?? ""}
              url={props.data().help?.source_url ?? ""}
            />
          </SettingsRowItem>
        </SectionCell>
      </SettingsSectionCard>
    </SettingsPageLayout>
  );
}

function PanelPluginsSettingsView(props: { data: () => any }) {
  const theme = currentTheme;
  const pendingDelete = signal<string | null>(null);
  const plugins = computed(() => (props.data().plugins ?? []) as any[]);

  return (
    <SettingsPageLayout pageKey="panel_plugins">
      <If when={Boolean(props.data().error)}>
        <SettingsSectionCard title={props.data().last_failed_title}>
          <SectionCell>
            <Text
              style={{
                fontSize: theme().type.text_xsmall.size,
                lineHeight: theme().type.text_xsmall.line_height,
                color: theme().palette.bad,
              }}
            >
              {props.data().error ?? ""}
            </Text>
          </SectionCell>
        </SettingsSectionCard>
      </If>

      <SettingsSectionCard title={props.data().list_title} description={props.data().list_hint}>
        <SectionCell>
          <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            <SettingsRowItem
              title={props.data().import_title ?? ""}
              description={props.data().import_hint ?? ""}
            >
              <SmeltButton
                quiet={true}
                small={true}
                label={props.data().open_dir_label ?? ""}
                onPress={() => sendSettingsAction({ page: "panel_plugins", kind: "open_dir" })}
              />
              <SmeltButton
                small={true}
                label={props.data().import_button_label ?? ""}
                loading={Boolean(props.data().busy)}
                disabled={Boolean(props.data().busy)}
                onPress={() => sendSettingsAction({ page: "panel_plugins", kind: "import" })}
              />
            </SettingsRowItem>
            <Text
              style={{
                fontSize: theme().type.text_xsmall.size,
                lineHeight: theme().type.text_xsmall.line_height,
                color: theme().palette.need,
              }}
            >
              {props.data().trust_warning ?? ""}
            </Text>
          </View>
        </SectionCell>

        <If when={plugins().length === 0}>
          <SectionCell dividerAbove={true}>
            <Text style={{ fontSize: theme().type.ui.size, color: theme().palette.muted }}>
              {props.data().empty_label ?? ""}
            </Text>
          </SectionCell>
        </If>

        <Each of={plugins()} key={(p: any) => p.id}>
          {(plugin: () => any) => {
            const isPending = computed(() => pendingDelete() === plugin().id);
            return (
              <SectionCell dividerAbove={true}>
                <SettingsRowItem
                  title={plugin().name}
                  description={plugin().detail}
                  error={plugin().error}
                >
                  <If
                    when={isPending()}
                    else={
                      <SmeltButton
                        quiet={true}
                        small={true}
                        label={plugin().action_label}
                        disabled={Boolean(props.data().busy)}
                        onPress={() => pendingDelete.set(plugin().id)}
                      />
                    }
                  >
                    <SmeltButton
                      danger={true}
                      small={true}
                      label={plugin().confirm_label}
                      disabled={Boolean(props.data().busy)}
                      onPress={() => {
                        const id = plugin().id;
                        pendingDelete.set(null);
                        sendSettingsAction({ page: "panel_plugins", kind: "delete", id });
                      }}
                    />
                    <SmeltButton
                      quiet={true}
                      small={true}
                      label={props.data().cancel_label ?? ""}
                      onPress={() => pendingDelete.set(null)}
                    />
                  </If>
                </SettingsRowItem>
              </SectionCell>
            );
          }}
        </Each>
      </SettingsSectionCard>

      <SettingsSectionCard title={props.data().authoring?.section_title}>
        <SectionCell>
          <SettingsRowItem
            title={props.data().authoring?.row_title ?? ""}
            description={props.data().authoring?.description ?? ""}
          >
            <SmeltButton
              quiet={true}
              small={true}
              label={props.data().authoring?.open_log_label ?? ""}
              onPress={() => sendSettingsAction({ page: "panel_plugins", kind: "open_log" })}
            />
            <SettingsLinkButton
              label={props.data().authoring?.examples_label ?? ""}
              url={props.data().authoring?.examples_url ?? ""}
            />
          </SettingsRowItem>
        </SectionCell>
      </SettingsSectionCard>
    </SettingsPageLayout>
  );
}

function ThemeDots(props: { swatch: [string, string, string] | null }) {
  const theme = currentTheme;
  if (!props.swatch) return null;
  const [bg, fg, accent] = props.swatch;
  const dot = (color: string) => (
    <View
      style={{
        width: 10,
        height: 10,
        borderRadius: 999,
        backgroundColor: color,
        borderWidth: 1,
        borderColor: theme().palette.hair_strong,
      }}
    />
  );
  return (
    <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 4, flexShrink: 0 }}>
      {dot(bg)}
      {dot(fg)}
      {dot(accent)}
    </View>
  );
}

function AppearanceSettingsView(props: { data: () => any }) {
  const theme = currentTheme;
  const themeQuery = signal("");

  const allThemes = computed(() => (props.data().themes ?? []) as ThemeOption[]);
  const swatchByName = computed(() => {
    const map = new Map<string, [string, string, string] | null>();
    for (const item of allThemes()) map.set(item.value, item.swatch);
    return map;
  });
  const filteredThemes = computed(() => {
    const q = themeQuery().trim().toLowerCase();
    const list = allThemes();
    const matched = !q
      ? list
      : list.filter(
          (t) =>
            t.title.toLowerCase().includes(q) ||
            t.group.toLowerCase().includes(q) ||
            t.keywords.some((k) => k.toLowerCase().includes(q)),
        );
    return matched.map((t) => ({ value: t.value, title: t.title, group: t.group }));
  });
  const selectedThemeSwatch = computed(
    () => swatchByName().get(props.data().theme_selected ?? "") ?? null,
  );

  const uiFontOptions = computed<SelectOption[]>(() => [
    { value: "", title: props.data().ui_default_font_label ?? "" },
    ...((props.data().fonts ?? []) as string[]).map((name) => ({ value: name, title: name })),
  ]);
  const codeFontOptions = computed<SelectOption[]>(() => [
    { value: "", title: props.data().code_default_font_label ?? "" },
    ...((props.data().fonts ?? []) as string[]).map((name) => ({ value: name, title: name })),
  ]);
  const layouts = computed(() => (props.data().layouts ?? []) as SelectOption[]);

  return (
    <SettingsPageLayout pageKey="appearance">
      <SettingsSectionCard title={props.data().interface_title}>
        <SectionCell>
          <SettingsRowItem
            title={props.data().theme_label ?? ""}
            description={props.data().theme_hint ?? ""}
          >
            <Combobox
              items={filteredThemes()}
              value={props.data().theme_selected ? [props.data().theme_selected] : []}
              query={themeQuery()}
              onQueryChange={({ query }: { query: string }) => themeQuery.set(query)}
              onOpenChange={({ open }: { open: boolean }) => {
                if (!open) themeQuery.set("");
              }}
              onChange={({ value }: { value: string[] }) => {
                const picked = value?.[0];
                if (picked) {
                  sendSettingsAction({ page: "appearance", kind: "set_theme", value: picked });
                }
              }}
              accessibilityLabel={props.data().theme_label ?? ""}
              offset={4}
              class="h-7 flex-row items-center rounded-sm border border-border bg-soft px-2 gap-2"
              style={{ width: 220 }}
              inputStyle={{
                flex: 1,
                minWidth: 0,
                fontSize: theme().type.ui.size,
                color: theme().palette.text,
              }}
              popupClass={`${MENU_CLASS} w-[260px]`}
              listStyle={{ maxHeight: 320 }}
              groupClass="px-2 py-1 text-xs font-medium text-muted-foreground"
              itemClass={`${MENU_ITEM_CLASS} group py-1.5 px-2 text-sm text-foreground`}
              prefix={<ThemeDots swatch={selectedThemeSwatch()} />}
              triggerButton={
                <SmeltIcon path="icons/chevron-down.svg" size={12} color={theme().palette.muted} />
              }
              empty={
                <View style={{ padding: 12 }}>
                  <Text style={{ fontSize: theme().type.text_xsmall.size, color: theme().palette.muted }}>
                    {props.data().theme_empty ?? ""}
                  </Text>
                </View>
              }
              renderItem={(item: any) => (
                <View
                  style={{
                    display: "flex",
                    flexDirection: "row",
                    alignItems: "center",
                    gap: 8,
                    width: "100%",
                  }}
                >
                  <ThemeDots swatch={swatchByName().get(item.value) ?? null} />
                  <Text
                    style={{
                      flex: 1,
                      minWidth: 0,
                      fontSize: theme().type.ui.size,
                      color: theme().palette.text,
                      numberOfLines: 1,
                    }}
                  >
                    {item.title}
                  </Text>
                </View>
              )}
            />
          </SettingsRowItem>
        </SectionCell>

        <SectionCell dividerAbove={true}>
          <SettingsRowItem
            title={props.data().ui_font_size_label ?? ""}
            description={props.data().ui_font_size_hint ?? ""}
          >
            <SettingsSliderControl
              value={props.data().ui_font_px ?? 16}
              min={props.data().min_ui_font_px ?? 14}
              max={props.data().max_ui_font_px ?? 26}
              step={1}
              label={props.data().ui_font_size_label ?? ""}
              formattedValue={`${props.data().ui_font_px ?? 16}px`}
              onChange={(v) =>
                sendSettingsAction({
                  page: "appearance",
                  kind: "set_ui_font_px",
                  px: Math.round(v),
                })
              }
            />
          </SettingsRowItem>
        </SectionCell>

        <SectionCell dividerAbove={true}>
          <SettingsRowItem title={props.data().ui_font_label ?? ""}>
            <SettingsSelectControl
              items={uiFontOptions()}
              value={props.data().ui_font_family ?? ""}
              label={props.data().ui_font_label ?? ""}
              minWidth={200}
              onChange={(value) =>
                sendSettingsAction({ page: "appearance", kind: "set_ui_font_family", value })
              }
            />
          </SettingsRowItem>
        </SectionCell>
      </SettingsSectionCard>

      <SettingsSectionCard title={props.data().code_title}>
        <SectionCell>
          <SettingsRowItem
            title={props.data().code_font_label ?? ""}
            description={props.data().code_font_hint ?? ""}
          >
            <SettingsSelectControl
              items={codeFontOptions()}
              value={props.data().code_font_family ?? ""}
              label={props.data().code_font_label ?? ""}
              minWidth={200}
              onChange={(value) =>
                sendSettingsAction({ page: "appearance", kind: "set_code_font_family", value })
              }
            />
          </SettingsRowItem>
        </SectionCell>
      </SettingsSectionCard>

      <SettingsSectionCard title={props.data().layout_title}>
        <If
          when={layouts().length > 0}
          else={
            <SectionCell>
              <SettingsRowItem
                title={props.data().layout_label ?? ""}
                description={props.data().layout_none_hint ?? ""}
              >
                <Text
                  style={{
                    fontSize: theme().type.text_xsmall.size,
                    color: theme().palette.muted,
                  }}
                >
                  {props.data().no_layouts_label ?? ""}
                </Text>
              </SettingsRowItem>
            </SectionCell>
          }
        >
          <SectionCell>
            <SettingsRowItem
              title={props.data().layout_label ?? ""}
              description={props.data().layout_hint ?? ""}
            >
              <SettingsSelectControl
                items={layouts()}
                value={props.data().selected_layout ?? ""}
                label={props.data().layout_label ?? ""}
                onChange={(value) =>
                  sendSettingsAction({ page: "appearance", kind: "set_layout", value })
                }
              />
            </SettingsRowItem>
          </SectionCell>
          <SectionCell dividerAbove={true}>
            <SettingsRowItem
              title={props.data().reset_layout_label ?? ""}
              description={props.data().reset_layout_hint ?? ""}
            >
              <SmeltButton
                small={true}
                label={props.data().reset_action_label ?? ""}
                onPress={() => sendSettingsAction({ page: "appearance", kind: "reset_layout" })}
              />
            </SettingsRowItem>
          </SectionCell>
        </If>
      </SettingsSectionCard>

      <SettingsSectionCard title={props.data().background_title}>
        <If when={Boolean(props.data().backdrop_available)}>
          <SectionCell>
            <SettingsRowItem
              title={props.data().glass_label ?? ""}
              description={props.data().glass_hint ?? ""}
            >
              <SettingsSliderControl
                value={props.data().bg_opacity_pct ?? 80}
                min={0}
                max={100}
                step={1}
                label={props.data().glass_label ?? ""}
                formattedValue={`${props.data().bg_opacity_pct ?? 80}%`}
                onChange={(v) =>
                  sendSettingsAction({
                    page: "appearance",
                    kind: "set_bg_opacity",
                    pct: Math.round(v),
                  })
                }
              />
            </SettingsRowItem>
          </SectionCell>
        </If>

        <SectionCell dividerAbove={Boolean(props.data().backdrop_available)}>
          <SettingsRowItem
            title={props.data().image_label ?? ""}
            description={props.data().image_hint ?? ""}
          >
            <If when={Boolean(props.data().image_name)}>
              <Text
                style={{
                  maxWidth: 160,
                  fontSize: theme().type.text_xsmall.size,
                  color: theme().palette.muted,
                  numberOfLines: 1,
                }}
              >
                {props.data().image_name ?? ""}
              </Text>
            </If>
            <SmeltButton
              small={true}
              label={props.data().image_button_label ?? ""}
              onPress={() => sendSettingsAction({ page: "appearance", kind: "pick_bg_image" })}
            />
            <If when={Boolean(props.data().image_name)}>
              <SmeltButton
                quiet={true}
                small={true}
                label={props.data().clear_label ?? ""}
                onPress={() => sendSettingsAction({ page: "appearance", kind: "clear_bg_image" })}
              />
            </If>
          </SettingsRowItem>
        </SectionCell>

        <If when={Boolean(props.data().image_name)}>
          <SectionCell dividerAbove={true}>
            <SettingsRowItem
              title={props.data().image_opacity_label ?? ""}
              description={props.data().image_opacity_hint ?? ""}
            >
              <SettingsSliderControl
                value={props.data().bg_image_opacity_pct ?? 0}
                min={0}
                max={props.data().max_bg_image_opacity_pct ?? 35}
                step={1}
                label={props.data().image_opacity_label ?? ""}
                formattedValue={`${props.data().bg_image_opacity_pct ?? 0}%`}
                onChange={(v) =>
                  sendSettingsAction({
                    page: "appearance",
                    kind: "set_bg_image_opacity",
                    pct: Math.round(v),
                  })
                }
              />
            </SettingsRowItem>
          </SectionCell>
        </If>
      </SettingsSectionCard>
    </SettingsPageLayout>
  );
}

function KeycapRow(props: { caps: string[] }) {
  const theme = currentTheme;
  return (
    <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 2 }}>
      {props.caps.map((cap) => (
        <View
          style={{
            minWidth: 18,
            height: 20,
            paddingLeft: 5,
            paddingRight: 5,
            borderRadius: theme().radius.xs,
            backgroundColor: theme().palette.soft,
            borderWidth: 1,
            borderColor: theme().palette.hair,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
          }}
        >
          <Text
            style={{
              fontSize: theme().type.text_xsmall.size,
              fontWeight: theme().type.text_xsmall_medium.weight,
              color: theme().palette.muted,
            }}
          >
            {cap}
          </Text>
        </View>
      ))}
    </View>
  );
}

function KeycapTextBadge(props: { text: string; focused?: boolean }) {
  const theme = currentTheme;
  return (
    <View
      style={{
        minWidth: 34,
        height: 24,
        paddingLeft: 8,
        paddingRight: 8,
        borderRadius: theme().radius.md,
        borderWidth: 1,
        borderColor: props.focused ? theme().palette.focus : theme().palette.hair,
        backgroundColor: theme().palette.surface,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      <Text style={{ fontSize: theme().type.text_xsmall.size, color: theme().palette.muted }}>
        {props.text}
      </Text>
    </View>
  );
}

function ShortcutsSettingsView(props: { data: () => any }) {
  const theme = currentTheme;
  const labels = computed(() => props.data().labels ?? {});

  return (
    <SettingsPageLayout pageKey="shortcuts">
      <Text
        style={{
          paddingLeft: 16,
          paddingRight: 16,
          fontSize: theme().type.ui.size,
          lineHeight: theme().type.ui.line_height,
          color: theme().palette.muted,
        }}
      >
        {props.data().hint ?? ""}
      </Text>

      <Each of={props.data().groups ?? []} key={(g: any) => g.title}>
        {(group: () => any) => (
          <SettingsSectionCard title={group().title}>
            {(group().rows ?? []).map((row: any, ix: number) => (
              <SectionCell dividerAbove={ix > 0}>
                <View
                  style={{
                    display: "flex",
                    flexDirection: "row",
                    alignItems: "center",
                    justifyContent: "space-between",
                    gap: 16,
                    minHeight: 28,
                    width: "100%",
                  }}
                >
                  <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
                    <Text
                      style={{
                        fontSize: theme().type.ui.size,
                        lineHeight: theme().type.ui.line_height,
                        color: theme().palette.text,
                      }}
                    >
                      {row.title}
                    </Text>
                    <If when={row.note != null}>
                      <Text
                        style={{
                          fontSize: theme().type.text_xsmall.size,
                          lineHeight: theme().type.text_xsmall.line_height,
                          color:
                            row.note?.tone === "bad"
                              ? theme().palette.bad
                              : theme().palette.muted,
                        }}
                      >
                        {row.note?.text ?? ""}
                      </Text>
                    </If>
                  </View>

                  <View
                    style={{
                      display: "flex",
                      flexDirection: "row",
                      flexShrink: 0,
                      alignItems: "center",
                      gap: 8,
                    }}
                  >
                    <If when={Boolean(row.reassigning)}>
                      <SmeltButton
                        quiet={true}
                        small={true}
                        label={labels().never_mind ?? ""}
                        onPress={() =>
                          sendSettingsAction({ page: "shortcuts", kind: "cancel_reassign" })
                        }
                      />
                      <SmeltButton
                        small={true}
                        label={labels().reassign ?? ""}
                        onPress={() =>
                          sendSettingsAction({ page: "shortcuts", kind: "confirm_reassign" })
                        }
                      />
                    </If>
                    <If when={Boolean(row.overridden && !row.recording && !row.reassigning)}>
                      <SmeltButton
                        quiet={true}
                        small={true}
                        label={labels().reset ?? ""}
                        onPress={() =>
                          sendSettingsAction({ page: "shortcuts", kind: "reset", id: row.id })
                        }
                      />
                    </If>
                    <BaseButton
                      accessibilityLabel={row.aria_label}
                      onPress={() =>
                        sendSettingsAction({ page: "shortcuts", kind: "toggle_record", id: row.id })
                      }
                      style={{
                        borderRadius: theme().radius.md,
                        backgroundColor: "transparent",
                        cursor: "default",
                      }}
                      hoverStyle={{ opacity: 0.8 }}
                    >
                      {row.recording ? (
                        <KeycapTextBadge text={labels().press ?? ""} focused={true} />
                      ) : Array.isArray(row.caps) && row.caps.length > 0 ? (
                        <KeycapRow caps={row.caps} />
                      ) : (
                        <KeycapTextBadge text={labels().unset ?? ""} />
                      )}
                    </BaseButton>
                  </View>
                </View>
              </SectionCell>
            ))}
          </SettingsSectionCard>
        )}
      </Each>

      <SettingsSectionCard title={props.data().view_keys_title}>
        {(props.data().view_keys ?? []).map((item: any, ix: number) => (
          <SectionCell dividerAbove={ix > 0}>
            <View
              style={{
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 16,
                minHeight: 28,
                width: "100%",
              }}
            >
              <Text style={{ fontSize: theme().type.ui.size, color: theme().palette.text }}>
                {item.label}
              </Text>
              <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 6 }}>
                {(item.steps ?? []).map((caps: string[]) => (
                  <KeycapRow caps={caps} />
                ))}
              </View>
            </View>
          </SectionCell>
        ))}
      </SettingsSectionCard>
    </SettingsPageLayout>
  );
}

export function HostSettingsPage(props: { pageId: string }) {
  const pageData = computed(() => hostSettingsState()?.page ?? null);
  const kind = computed(() => pageData()?.kind ?? "");

  return (
    <View
      style={{
        display: "flex",
        flexDirection: "column",
        flex: 1,
        width: "100%",
        height: "100%",
        minHeight: 0,
        minWidth: 0,
      }}
    >
      <If when={kind() === "language"}>
        <LanguageSettingsView data={pageData} />
      </If>
      <If when={kind() === "terminal"}>
        <TerminalSettingsView data={pageData} />
      </If>
      <If when={kind() === "cli"}>
        <CliSettingsView data={pageData} />
      </If>
      <If when={kind() === "cleanup"}>
        <CleanupSettingsView data={pageData} />
      </If>
      <If when={kind() === "about"}>
        <AboutSettingsView data={pageData} />
      </If>
      <If when={kind() === "panel_plugins"}>
        <PanelPluginsSettingsView data={pageData} />
      </If>
      <If when={kind() === "appearance"}>
        <AppearanceSettingsView data={pageData} />
      </If>
      <If when={kind() === "shortcuts"}>
        <ShortcutsSettingsView data={pageData} />
      </If>
    </View>
  );
}
