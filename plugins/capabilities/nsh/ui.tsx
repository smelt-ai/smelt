// 能力库共享 UI 原语（严格遵循 docs/ui-design-language.md 与 smelt-ui::settings 视觉语言）。

import { computed, Scroll, signal, Text, View } from "nsh";
import {
  Button as BaseButton,
  Checkbox,
  Dialog,
  Input,
  Menu,
  MenuItem,
  MenuSeparator,
  Popover,
  Submenu,
  Tooltip,
} from "nsh/base";
import { clipboardWrite, SmeltButton, SmeltIcon, t } from "smelt/sdk";
import { agentUsesItem } from "../model.js";
import {
  agentSettingsOn,
  allMachineList,
  type CapabilitiesTab,
  type ConfirmRequest,
  currentTheme,
  type PageController,
} from "./state.ts";

export const POPUP_CLASS = "rounded-(--radius) border border-border bg-popover p-1";
export const MENU_ITEM_CLASS =
  "flex-row items-center rounded-(--radius) px-2 hover:bg-accent focus:bg-accent";
export const INPUT_CLASS = "rounded-(--radius) border border-input bg-card px-2 text-foreground";

export function Tip(props: { text: string; maxWidth?: number }) {
  const look = currentTheme;
  return (
    <Tooltip class="rounded-(--radius) border border-border bg-popover px-2 py-1">
      <Text
        style={{
          color: look().palette.text,
          fontSize: look().type.text_xsmall.size,
          lineHeight: look().type.text_xsmall.line_height,
          ...(props.maxWidth != null ? { maxWidth: props.maxWidth } : {}),
        }}
      >
        {props.text}
      </Text>
    </Tooltip>
  );
}

export function StatusLine(props: { text: string; error?: boolean; tone?: "muted" | "bad" | "ok" }) {
  const look = currentTheme;
  const color = () => {
    const p = look().palette;
    if (props.tone === "ok") return p.ok;
    if (props.tone === "bad" || props.error) return p.bad;
    return p.muted;
  };
  return (
    <Text
      style={{
        fontSize: look().type.text_xsmall.size,
        lineHeight: look().type.text_xsmall.line_height,
        color: color(),
      }}
    >
      {props.text}
    </Text>
  );
}

export function BadgePill(props: { label: string; tone?: "neutral" | "ok" | "need" | "bad" }) {
  const look = currentTheme;
  const colors = computed(() => {
    const p = look().palette;
    if (props.tone === "ok") return { bg: p.soft, fg: p.ok, border: p.hair };
    if (props.tone === "need") return { bg: p.soft, fg: p.need, border: p.hair };
    if (props.tone === "bad") return { bg: p.soft, fg: p.bad, border: p.bad };
    return { bg: p.soft, fg: p.muted, border: p.hair };
  });
  return (
    <View
      style={{
        paddingLeft: 6,
        paddingRight: 6,
        height: 18,
        borderRadius: look().radius.sm,
        backgroundColor: colors().bg,
        borderWidth: 1,
        borderColor: colors().border,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        flexShrink: 0,
      }}
    >
      <Text
        style={{
          fontSize: look().type.text_xsmall.size,
          lineHeight: look().type.text_xsmall.line_height,
          color: colors().fg,
          numberOfLines: 1,
        }}
      >
        {props.label}
      </Text>
    </View>
  );
}

export function PageIntro(props: { text: string }) {
  const look = currentTheme;
  return (
    <View style={{ paddingHorizontal: 16 }}>
      <Text
        style={{
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
          color: look().palette.muted,
        }}
      >
        {props.text}
      </Text>
    </View>
  );
}

/** 居中限宽的设置/能力库滚动骨架（对齐 smelt-ui::SettingsPage）。 */
export function SettingsPageShell(props: { maxWidth?: number; children?: any }) {
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
            width: "100%",
            maxWidth: props.maxWidth ?? 720,
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

/** 一组设置：可选标题/说明 + surface 描边卡片。 */
export function SettingsSection(props: {
  title?: string;
  description?: string;
  children?: any;
}) {
  const look = currentTheme;
  const hasHeader = () => Boolean(props.title || props.description);
  return (
    <View style={{ display: "flex", flexDirection: "column", gap: 8, width: "100%" }}>
      <If when={hasHeader()}>
        <View
          style={{
            display: "flex",
            flexDirection: "column",
            gap: 2,
            paddingHorizontal: 16,
          }}
        >
          <If when={Boolean(props.title)}>
            <Text
              style={{
                fontSize: look().type.ui_medium.size,
                lineHeight: look().type.ui_medium.line_height,
                fontWeight: look().type.ui_medium.weight,
                color: look().palette.muted,
              }}
            >
              {props.title ?? ""}
            </Text>
          </If>
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
      </If>
      <View
        style={{
          width: "100%",
          paddingVertical: 4,
          borderRadius: look().radius.lg ?? 10,
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

export function SectionDivider() {
  const look = currentTheme;
  return (
    <View
      style={{
        marginHorizontal: 16,
        height: 1,
        backgroundColor: look().palette.hair,
      }}
    />
  );
}

export function SectionCell(props: { divider?: boolean; children?: any }) {
  return (
    <>
      <If when={Boolean(props.divider)}>
        <SectionDivider />
      </If>
      <View style={{ paddingHorizontal: 16, paddingVertical: 8 }}>{props.children}</View>
    </>
  );
}

/** 一行设置：左文字（标题、可选说明、可选附注）+ 右控件。 */
export function SettingsRow(props: {
  title: string;
  description?: string | null;
  clampDescription?: number;
  note?: string | null;
  children?: any;
}) {
  const look = currentTheme;
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
      <View
        style={{
          display: "flex",
          flexDirection: "column",
          flex: 1,
          minWidth: 0,
          gap: 2,
        }}
      >
        <Text
          style={{
            fontSize: look().type.ui.size,
            lineHeight: look().type.ui.line_height,
            color: look().palette.text,
          }}
        >
          {props.title}
        </Text>
        <If when={Boolean(props.description)}>
          <View
            tooltip={
              props.clampDescription && props.description ? (
                <Tip text={props.description} maxWidth={440} />
              ) : undefined
            }
          >
            <Text
              style={{
                fontSize: look().type.small.size,
                lineHeight: look().type.small.line_height,
                color: look().palette.muted,
                ...(props.clampDescription ? { numberOfLines: props.clampDescription } : {}),
              }}
            >
              {props.description ?? ""}
            </Text>
          </View>
        </If>
        <If when={Boolean(props.note)}>
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: look().palette.muted,
            }}
          >
            {props.note ?? ""}
          </Text>
        </If>
      </View>
      <If when={props.children != null}>
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            flexShrink: 0,
            gap: 8,
          }}
        >
          {props.children}
        </View>
      </If>
    </View>
  );
}

export function TextField(props: {
  label?: string;
  value: string;
  placeholder?: string;
  masked?: boolean;
  disabled?: boolean;
  onChange: (value: string) => void;
  onSubmit?: () => void;
}) {
  const look = currentTheme;
  return (
    <View style={{ display: "flex", flexDirection: "column", flex: 1, minWidth: 0, gap: 4 }}>
      <If when={Boolean(props.label)}>
        <Text
          style={{
            fontSize: look().type.text_xsmall_medium.size,
            lineHeight: look().type.text_xsmall_medium.line_height,
            fontWeight: look().type.text_xsmall_medium.weight,
            color: look().palette.text,
          }}
        >
          {props.label ?? ""}
        </Text>
      </If>
      <Input
        value={props.value}
        placeholder={props.placeholder ?? ""}
        masked={Boolean(props.masked)}
        disabled={Boolean(props.disabled)}
        accessibilityLabel={props.label || props.placeholder || undefined}
        onChange={({ value }: { value: string }) => props.onChange(value)}
        onSubmit={props.onSubmit}
        class={INPUT_CLASS}
        style={{ height: 28, fontSize: look().type.ui.size, width: "100%" }}
      />
    </View>
  );
}

export function CopyButton(props: { value: string; tooltip: string }) {
  const look = currentTheme;
  const copiedValue = signal<string | null>(null);
  const isCopied = () => Boolean(props.value) && copiedValue() === props.value;

  return (
    <SmeltButton
      quiet={true}
      small={true}
      icon={isCopied() ? "icons/check.svg" : "icons/copy.svg"}
      accessibilityLabel={props.tooltip}
      tooltip={isCopied() ? t("auth.copied") : props.tooltip}
      style={isCopied() ? { color: look().palette.ok } : undefined}
      onPress={() => {
        const val = props.value;
        if (!val) return;
        void clipboardWrite(val).then(() => copiedValue.set(val));
      }}
    />
  );
}

export function MenuEntry(props: {
  label: string;
  checked?: boolean;
  danger?: boolean;
  disabled?: boolean;
  onSelect: () => void;
}) {
  const look = currentTheme;
  const color = () =>
    props.disabled
      ? look().palette.faint
      : props.danger
        ? look().palette.bad
        : look().palette.text;
  return (
    <MenuItem
      checked={props.checked}
      disabled={props.disabled}
      closeOnSelect={true}
      onSelect={props.onSelect}
      class={MENU_ITEM_CLASS}
      style={{ height: 28, gap: 8 }}
    >
      <Text
        class="flex-1 min-w-0"
        style={{
          color: color(),
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
          numberOfLines: 1,
        }}
      >
        {props.label}
      </Text>
      <If when={props.checked != null}>
        <View
          style={{
            width: 14,
            height: 14,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            flexShrink: 0,
          }}
        >
          <If when={Boolean(props.checked)}>
            <SmeltIcon path="icons/check.svg" size={13} color={look().palette.text} />
          </If>
        </View>
      </If>
    </MenuItem>
  );
}

export function MenuSub(props: {
  label: string;
  checked?: boolean;
  children?: any;
}) {
  const look = currentTheme;
  return (
    <Submenu
      popupClass={POPUP_CLASS}
      popupStyle={{ minWidth: 180 }}
      trigger={
        <View class={MENU_ITEM_CLASS} style={{ height: 28, gap: 8 }}>
          <Text
            class="flex-1 min-w-0"
            style={{
              color: look().palette.text,
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
              numberOfLines: 1,
            }}
          >
            {props.label}
          </Text>
          <If when={Boolean(props.checked)}>
            <SmeltIcon path="icons/check.svg" size={12} color={look().palette.text} />
          </If>
          <SmeltIcon path="icons/chevron-right.svg" size={12} color={look().palette.muted} />
        </View>
      }
    >
      {props.children}
    </Submenu>
  );
}

export function MenuDivider() {
  return <MenuSeparator class="my-1 bg-border" style={{ height: 1 }} />;
}

/** 带小箭头的下拉菜单触发器按钮（保持菜单打开态选中视觉）。 */
export function DropdownMenuButton(props: {
  label?: string;
  iconOnly?: string;
  accessibilityLabel?: string;
  quiet?: boolean;
  disabled?: boolean;
  align?: "start" | "end";
  minWidth?: number;
  children?: any;
}) {
  const look = currentTheme;
  const open = signal(false);

  const bg = () => {
    const p = look().palette;
    const neutral = p.button_secondary ?? [p.soft, p.soft_hover ?? p.soft, p.row_selected];
    if (open()) return p.soft_hover ?? p.row_selected;
    return props.quiet ? "transparent" : neutral[0];
  };
  const fg = () => {
    const p = look().palette;
    if (props.quiet) return open() ? p.text : p.muted;
    return p.button_secondary_fg ?? p.text;
  };

  const renderTrigger = () =>
    props.iconOnly ? (
      <SmeltButton
        quiet={true}
        small={true}
        selected={open()}
        disabled={props.disabled}
        icon={props.iconOnly}
        accessibilityLabel={props.accessibilityLabel}
      />
    ) : (
      <View
        role="button"
        accessibilityLabel={props.accessibilityLabel || props.label}
        class="hover:bg-accent"
        style={{
          height: 24,
          paddingLeft: 10,
          paddingRight: 8,
          borderRadius: look().radius.md,
          backgroundColor: bg(),
          opacity: props.disabled ? 0.45 : 1,
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          gap: 6,
          flexShrink: 0,
          cursor: "default",
        }}
      >
        <Text
          style={{
            fontSize: look().type.small_medium.size,
            lineHeight: look().type.small_medium.line_height,
            fontWeight: props.quiet
              ? look().type.small.weight
              : look().type.small_medium.weight,
            color: fg(),
            numberOfLines: 1,
          }}
        >
          {props.label ?? ""}
        </Text>
        <SmeltIcon path="icons/chevron-down.svg" size={12} color={look().palette.muted} />
      </View>
    );

  return (
    <If when={!props.disabled} else={renderTrigger()}>
      <Menu
        placement="bottom"
        align={props.align ?? "end"}
        popupClass={POPUP_CLASS}
        popupStyle={{ minWidth: props.minWidth ?? 168 }}
        onOpenChange={({ open: next }: { open: boolean }) => open.set(Boolean(next))}
        trigger={renderTrigger()}
      >
        {props.children}
      </Menu>
    </If>
  );
}

/** 破坏性确认对话框（alertdialog、居中 m-auto、无右上角 ×、安静取消 + 危险确认）。 */
export function ConfirmDialog(props: {
  request: () => ConfirmRequest | null;
  onDismiss: () => void;
}) {
  const look = currentTheme;
  const req = props.request;
  return (
    <Dialog
      open={req() != null}
      role="alertdialog"
      animation="fade"
      accessibilityLabel={req()?.title ?? ""}
      accessibilityDescription={req()?.description ?? ""}
      backdropClass="bg-black/40"
      onOpenChange={({ open }: { open: boolean }) => {
        if (!open) props.onDismiss();
      }}
      class="m-auto rounded-(--radius) border border-border bg-popover p-4"
      style={{ width: 420, gap: 12 }}
    >
      <If when={req() != null}>
        <Text
          style={{
            color: look().palette.text,
            fontSize: look().type.ui_medium.size,
            lineHeight: look().type.ui_medium.line_height,
            fontWeight: look().type.ui_medium.weight,
          }}
        >
          {req()?.title ?? ""}
        </Text>
        <If when={Boolean(req()?.description)}>
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
            }}
          >
            {req()?.description ?? ""}
          </Text>
        </If>
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            justifyContent: "flex-end",
            alignItems: "center",
            gap: 8,
            marginTop: 4,
          }}
        >
          <SmeltButton
            quiet={true}
            small={true}
            label={t("common.cancel")}
            onPress={props.onDismiss}
          />
          <SmeltButton
            primary={!req()?.danger}
            danger={Boolean(req()?.danger)}
            small={true}
            label={req()?.action ?? ""}
            onPress={() => {
              const fn = req()?.onConfirm;
              props.onDismiss();
              fn?.();
            }}
          />
        </View>
      </If>
    </Dialog>
  );
}

/** 技能 / 扩展 / MCP 行内的「给智能体用」弹层。 */
export function UsesPopover(props: {
  page: PageController;
  usable: { kind: "plugin" | "mcp"; id: string };
}) {
  const look = currentTheme;
  const open = signal(false);
  const settings = computed(() => agentSettingsOn(props.page.machine()));
  const agents = computed(() => settings()?.agents ?? []);

  return (
    <If when={settings() != null}>
      <Popover
        open={open()}
        onOpenChange={({ open: next }: { open: boolean }) => open.set(Boolean(next))}
        placement="bottom"
        align="end"
        offset={4}
        popupStyle={{
          width: 240,
          padding: 8,
          borderRadius: look().radius.md,
          backgroundColor: look().palette.surface,
          borderWidth: 1,
          borderColor: look().palette.hair,
        }}
        trigger={
          <View
            role="button"
            accessibilityLabel={t("uses.button")}
            class="hover:bg-accent"
            style={{
              height: 24,
              paddingLeft: 10,
              paddingRight: 8,
              borderRadius: look().radius.md,
              backgroundColor: open()
                ? look().palette.soft_hover ?? look().palette.row_selected
                : (look().palette.button_secondary?.[0] ?? look().palette.soft),
              display: "flex",
              flexDirection: "row",
              alignItems: "center",
              gap: 6,
              flexShrink: 0,
              cursor: "default",
            }}
          >
            <Text
              style={{
                fontSize: look().type.small_medium.size,
                lineHeight: look().type.small_medium.line_height,
                fontWeight: look().type.small_medium.weight,
                color: look().palette.button_secondary_fg ?? look().palette.text,
                numberOfLines: 1,
              }}
            >
              {t("uses.button")}
            </Text>
            <SmeltIcon path="icons/chevron-down.svg" size={12} color={look().palette.muted} />
          </View>
        }
      >
        <View style={{ display: "flex", flexDirection: "column", width: "100%", gap: 4 }}>
          <Text
            style={{
              paddingHorizontal: 6,
              paddingVertical: 2,
              fontSize: look().type.text_xsmall.size,
              color: look().palette.muted,
            }}
          >
            {t("uses.title")}
          </Text>
          <If
            when={agents().length > 0}
            else={
              <Text
                style={{
                  paddingHorizontal: 6,
                  paddingBottom: 4,
                  fontSize: look().type.text_xsmall.size,
                  color: look().palette.muted,
                }}
              >
                {t("uses.no_agents")}
              </Text>
            }
          >
            <Each of={agents()} key={(a: any) => a.id}>
              {(agent: () => any) => {
                const checked = () => agentUsesItem(agent(), props.usable);
                const pending = () => props.page.agentEditPending();
                const p = () => look().palette;
                return (
                  <View style={{ paddingHorizontal: 6, paddingVertical: 3 }}>
                    <Checkbox
                      checked={checked()}
                      disabled={pending()}
                      accessibilityLabel={agent().name}
                      onChange={({ checked: next }: { checked: boolean }) =>
                        props.page.setAgentUse(props.usable, agent().id, Boolean(next))
                      }
                      style={{
                        display: "flex",
                        flexDirection: "row",
                        alignItems: "center",
                        gap: 8,
                        opacity: pending() ? 0.45 : 1,
                      }}
                      indicatorStyle={{
                        width: 16,
                        height: 16,
                        borderRadius: look().radius.sm,
                        borderWidth: 1,
                        borderColor: checked() ? p().ink : p().hair_strong,
                        backgroundColor: checked() ? p().ink : "transparent",
                        display: "flex",
                        alignItems: "center",
                        justifyContent: "center",
                        flexShrink: 0,
                      }}
                      indicator={
                        checked() ? (
                          <SmeltIcon path="icons/check.svg" size={10} color={p().on_ink} />
                        ) : undefined
                      }
                    >
                      <Text
                        style={{
                          fontSize: look().type.small.size,
                          lineHeight: look().type.small.line_height,
                          color: p().text,
                          numberOfLines: 1,
                        }}
                      >
                        {agent().name}
                      </Text>
                    </Checkbox>
                  </View>
                );
              }}
            </Each>
          </If>
        </View>
      </Popover>
    </If>
  );
}

function tabLabel(tab: CapabilitiesTab): string {
  switch (tab) {
    case "models":
      return t("tab.models");
    case "skills":
      return t("tab.skills");
    case "extensions":
      return t("tab.extensions");
    case "mcp":
      return t("tab.mcp");
    case "memory":
      return t("tab.memory");
  }
}

/** 页面顶部工具行：多页签分段切换 + 多机器时的机器选择器（对齐 smelt-ui::page_toolbar）。 */
export function PageToolbar(props: { page: PageController }) {
  const look = currentTheme;
  const machineList = computed(() => allMachineList());
  const severalMachines = computed(() => machineList().length > 1);
  const showTabs = props.page.tabs.length > 1;
  const showBar = computed(() => showTabs || severalMachines());

  return (
    <If when={showBar()}>
      <View
        style={{
          width: "100%",
          minHeight: 48,
          paddingLeft: 20,
          paddingRight: 20,
          paddingTop: 8,
          paddingBottom: 8,
          borderBottomWidth: 1,
          borderBottomColor: look().palette.hair,
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          gap: 12,
          flexShrink: 0,
        }}
      >
        <If when={showTabs}>
          <View
            style={{
              display: "flex",
              flexDirection: "row",
              alignItems: "center",
              height: 28,
              padding: 2,
              borderRadius: look().radius.md,
              backgroundColor: look().palette.soft,
              borderWidth: 1,
              borderColor: look().palette.hair,
              gap: 2,
            }}
          >
            <Each of={[...props.page.tabs]} key={(item: CapabilitiesTab) => item}>
              {(item: () => CapabilitiesTab) => {
                const selected = () => props.page.tab() === item();
                const p = () => look().palette;
                return (
                  <BaseButton
                    selected={selected()}
                    onPress={() => props.page.selectTab(item())}
                    style={{
                      height: 22,
                      paddingLeft: 12,
                      paddingRight: 12,
                      borderRadius: look().radius.sm,
                      backgroundColor: selected() ? p().bg : "transparent",
                      borderWidth: selected() ? 1 : 0,
                      borderColor: selected() ? p().hair_strong : "transparent",
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                      cursor: "default",
                    }}
                    hoverStyle={{
                      backgroundColor: selected() ? p().bg : p().row_hover,
                    }}
                  >
                    <Text
                      style={{
                        fontSize: look().type.ui.size,
                        lineHeight: look().type.ui.line_height,
                        fontWeight: selected()
                          ? look().type.ui_medium.weight
                          : look().type.ui.weight,
                        color: selected() ? p().text : p().muted,
                        numberOfLines: 1,
                      }}
                    >
                      {tabLabel(item())}
                    </Text>
                  </BaseButton>
                );
              }}
            </Each>
          </View>
        </If>

        <View style={{ flex: 1 }} />

        <If when={severalMachines()}>
          <DropdownMenuButton
            quiet={true}
            align="end"
            label={t("machine_picker", { name: props.page.currentMachineName() })}
          >
            <Each of={machineList()} key={(m: any) => m.machine ?? "__local__"}>
              {(m: () => any) => {
                const key = () => m().machine ?? null;
                const label = () =>
                  m().unavailable ? `${m().name}（${m().unavailable}）` : m().name;
                return (
                  <MenuEntry
                    label={label()}
                    checked={props.page.machine() === key()}
                    onSelect={() => props.page.selectMachine(key())}
                  />
                );
              }}
            </Each>
          </DropdownMenuButton>
        </If>
      </View>
    </If>
  );
}
