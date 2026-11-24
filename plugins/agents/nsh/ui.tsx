// 智能体插件的共用基础 UI 组件（卡片、工具行、菜单项、分段控件、开关、徽标、确认框）。

import { signal, Text, View } from "nsh";
import {
  MenuItem,
  MenuSeparator,
  Switch,
} from "nsh/base";
import {
  DIALOG_SURFACE_CLASS,
  INPUT_CLASS,
  MENU_ITEM_CLASS,
  POPUP_CLASS,
  SmeltButton,
  SmeltDialog,
  SmeltIcon,
  SmeltMenu,
  SmeltTooltip,
  t,
} from "smelt/sdk";
export { INPUT_CLASS, MENU_ITEM_CLASS, POPUP_CLASS };

import { agentCatalogMonogram } from "../model.js";
import { ConfirmRequest, currentTheme, findEngineMeta } from "./state.ts";

export function Tip(props: { text: string }) {
  const look = currentTheme;
  return (
    <SmeltTooltip>
      <Text
        style={{
          color: look().palette.text,
          fontSize: look().type.text_xsmall.size,
          lineHeight: look().type.text_xsmall.line_height,
        }}
      >
        {props.text}
      </Text>
    </SmeltTooltip>
  );
}

export function PageToolbar(props: { children?: any; style?: any }) {
  const look = currentTheme;
  return (
    <View
      style={{
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
        ...(props.style ?? {}),
      }}
    >
      {props.children}
    </View>
  );
}

export function SurfaceCard(props: { children?: any; style?: any }) {
  const look = currentTheme;
  return (
    <View
      style={{
        borderRadius: 12,
        borderWidth: 1,
        borderColor: look().palette.hair,
        backgroundColor: look().palette.surface,
        display: "flex",
        flexDirection: "column",
        ...(props.style ?? {}),
      }}
    >
      {props.children}
    </View>
  );
}

export function ErrorAlert(props: { message: string }) {
  const look = currentTheme;
  return (
    <View
      style={{
        paddingLeft: 12,
        paddingRight: 12,
        paddingTop: 10,
        paddingBottom: 10,
        borderRadius: look().radius.md,
        borderWidth: 1,
        borderColor: look().palette.bad,
        backgroundColor: look().palette.surface,
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        gap: 8,
      }}
    >
      <SmeltIcon path="icons/circle-alert.svg" size={14} color={look().palette.bad} />
      <Text
        style={{
          flex: 1,
          minWidth: 0,
          color: look().palette.bad,
          fontSize: look().type.text_xsmall.size,
          lineHeight: look().type.text_xsmall.line_height,
        }}
      >
        {props.message}
      </Text>
    </View>
  );
}

export function EngineMark(props: { engineId?: string | null; size?: number }) {
  const look = currentTheme;
  const path = () => {
    const meta = props.engineId ? findEngineMeta(props.engineId) : null;
    return meta?.icon ?? "icons/bot.svg";
  };
  return <SmeltIcon path={path()} size={props.size ?? 14} color={look().palette.muted} />;
}

export function EngineChip(props: { label: string; engineId?: string | null }) {
  const look = currentTheme;
  return (
    <View
      style={{
        height: 20,
        paddingLeft: 6,
        paddingRight: 6,
        borderRadius: look().radius.sm,
        backgroundColor: look().palette.soft,
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        gap: 5,
        flexShrink: 0,
      }}
    >
      <EngineMark engineId={props.engineId} size={12} />
      <Text
        style={{
          color: look().palette.muted,
          fontSize: look().type.small.size,
          lineHeight: look().type.small.line_height,
          fontWeight: look().type.small_medium.weight,
          numberOfLines: 1,
        }}
      >
        {props.label}
      </Text>
    </View>
  );
}

export function QuietBadge(props: { label: string; onBg?: boolean }) {
  const look = currentTheme;
  return (
    <View
      style={{
        height: 18,
        paddingLeft: 6,
        paddingRight: 6,
        borderRadius: look().radius.sm,
        backgroundColor: props.onBg ? look().palette.bg : look().palette.soft,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        flexShrink: 0,
      }}
    >
      <Text
        style={{
          color: look().palette.faint,
          fontSize: look().type.small.size,
          lineHeight: look().type.small.line_height,
          fontWeight: look().type.small_medium.weight,
          numberOfLines: 1,
        }}
      >
        {props.label}
      </Text>
    </View>
  );
}

export function AgentMonogram(props: { name: string; size: number; onCard?: boolean }) {
  const look = currentTheme;
  const monogram = () => agentCatalogMonogram(props.name, t);
  const compact = () => [...monogram()].length > 1;
  return (
    <View
      style={{
        width: props.size,
        height: props.size,
        borderRadius: 10,
        flexShrink: 0,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        backgroundColor: props.onCard ? look().palette.bg : look().palette.surface,
      }}
    >
      <Text
        style={{
          color: look().palette.text,
          fontSize: compact() ? props.size * 0.34 : props.size * 0.42,
          fontWeight: 500,
          numberOfLines: 1,
        }}
      >
        {monogram()}
      </Text>
    </View>
  );
}

export function ToggleSwitch(props: {
  checked: boolean;
  disabled?: boolean;
  accessibilityLabel: string;
  tooltip?: string;
  onChange: (checked: boolean) => void;
}) {
  const look = currentTheme;
  return (
    <View
      tooltip={props.tooltip ? <Tip text={props.tooltip} /> : undefined}
      style={{ flexShrink: 0 }}
    >
      <Switch
        checked={props.checked}
        disabled={props.disabled}
        accessibilityLabel={props.accessibilityLabel}
        onChange={({ checked }: { checked: boolean }) => props.onChange(Boolean(checked))}
        trackStyle={{
          width: 34,
          height: 20,
          borderRadius: 10,
          padding: 2,
          backgroundColor: props.checked ? look().palette.link : look().palette.soft,
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          justifyContent: props.checked ? "flex-end" : "flex-start",
          opacity: props.disabled ? 0.45 : 1,
        }}
        thumbStyle={{
          width: 16,
          height: 16,
          borderRadius: 8,
          backgroundColor: look().palette.on_ink,
        }}
      />
    </View>
  );
}

export function MenuEntry(props: {
  label: string;
  iconPath?: string | null;
  checked?: boolean;
  danger?: boolean;
  disabled?: boolean;
  onSelect: () => void;
}) {
  const look = currentTheme;
  const color = () => (props.danger ? look().palette.bad : look().palette.text);
  const iconColor = () => (props.danger ? look().palette.bad : look().palette.muted);
  return (
    <MenuItem
      checked={props.checked}
      disabled={props.disabled}
      closeOnSelect={true}
      onSelect={props.onSelect}
      class={MENU_ITEM_CLASS}
      style={{ height: 28, gap: 8, opacity: props.disabled ? 0.45 : 1 }}
    >
      <If when={Boolean(props.iconPath)}>
        <View style={{ width: 14, height: 14, flexShrink: 0 }}>
          <SmeltIcon path={props.iconPath ?? ""} size={14} color={iconColor()} />
        </View>
      </If>
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

export function MenuDivider() {
  return <MenuSeparator class="my-1 bg-border" style={{ height: 1 }} />;
}

export function MenuSectionTitle(props: { text: string }) {
  const look = currentTheme;
  return (
    <View style={{ paddingLeft: 8, paddingRight: 8, paddingTop: 4, paddingBottom: 4 }}>
      <Text
        style={{
          color: look().palette.faint,
          fontSize: look().type.small.size,
          lineHeight: look().type.small.line_height,
          fontWeight: look().type.small_medium.weight,
          numberOfLines: 1,
        }}
      >
        {props.text}
      </Text>
    </View>
  );
}

export function DropdownButton(props: {
  label: string;
  iconPath?: string | null;
  leading?: any;
  tooltip?: string;
  quiet?: boolean;
  small?: boolean;
  align?: "start" | "end";
  minWidth?: number;
  fill?: boolean;
  children?: any;
}) {
  const look = currentTheme;
  const open = signal(false);
  const bg = () => {
    if (open()) return look().palette.soft_hover;
    if (props.quiet) return "transparent";
    return look().palette.button_secondary[0];
  };
  return (
    <SmeltMenu
      placement="bottom"
      align={props.align ?? "start"}
      popupStyle={{ minWidth: props.minWidth ?? 168 }}
      onOpenChange={({ open: next }: { open: boolean }) => open.set(Boolean(next))}
      trigger={
        <View
          role="button"
          accessibilityLabel={props.label}
          tooltip={props.tooltip ? <Tip text={props.tooltip} /> : undefined}
          style={{
            height: props.small ? 24 : 28,
            paddingLeft: props.small ? 8 : 10,
            paddingRight: props.small ? 6 : 8,
            gap: 6,
            borderRadius: look().radius.md,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            backgroundColor: bg(),
            cursor: "default",
            flexShrink: props.fill ? 1 : 0,
            ...(props.fill ? { width: "100%", minWidth: 0 } : {}),
          }}
          hoverStyle={{
            backgroundColor: props.quiet
              ? look().palette.row_hover
              : look().palette.button_secondary[1],
          }}
        >
          {props.leading}
          <If when={Boolean(props.iconPath)}>
            <SmeltIcon
              path={props.iconPath ?? ""}
              size={props.small ? 13 : 14}
              color={props.quiet ? look().palette.muted : look().palette.text}
            />
          </If>
          <Text
            style={{
              flex: props.fill ? 1 : undefined,
              minWidth: 0,
              color: props.quiet ? look().palette.muted : look().palette.text,
              fontSize: props.small ? look().type.small.size : look().type.ui.size,
              lineHeight: props.small ? look().type.small.line_height : look().type.ui.line_height,
              fontWeight: props.quiet
                ? look().type.ui.weight
                : props.small
                  ? look().type.small_medium.weight
                  : look().type.ui_medium.weight,
              numberOfLines: 1,
            }}
          >
            {props.label}
          </Text>
          <SmeltIcon path="icons/chevron-down.svg" size={12} color={look().palette.muted} />
        </View>
      }
    >
      {props.children}
    </SmeltMenu>
  );
}

export interface SegmentedOption {
  key: string;
  label: string;
  tooltip?: string;
}

export function SegmentedControl(props: {
  options: SegmentedOption[];
  selectedKey: string;
  onSelect: (key: string) => void;
}) {
  const look = currentTheme;
  return (
    <View
      style={{
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        padding: 2,
        gap: 2,
        borderRadius: look().radius.md,
        borderWidth: 1,
        borderColor: look().palette.hair,
        backgroundColor: look().palette.soft,
        flexShrink: 0,
      }}
    >
      <Each of={props.options} key={(opt: SegmentedOption) => opt.key}>
        {(opt: () => SegmentedOption) => {
          const active = () => opt().key === props.selectedKey;
          return (
            <View
              role="button"
              accessibilityLabel={opt().label}
              tooltip={opt().tooltip ? <Tip text={opt().tooltip!} /> : undefined}
              onPress={() => props.onSelect(opt().key)}
              style={{
                height: 24,
                paddingLeft: 10,
                paddingRight: 10,
                borderRadius: look().radius.sm,
                borderWidth: active() ? 1 : 0,
                borderColor: active() ? look().palette.hair_strong : "transparent",
                backgroundColor: active() ? look().palette.bg : "transparent",
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                justifyContent: "center",
                flexShrink: 0,
                cursor: "default",
              }}
            >
              <Text
                style={{
                  color: active() ? look().palette.text : look().palette.muted,
                  fontSize: look().type.small.size,
                  lineHeight: look().type.small.line_height,
                  fontWeight: active()
                    ? look().type.small_medium.weight
                    : look().type.small.weight,
                  numberOfLines: 1,
                }}
              >
                {opt().label}
              </Text>
            </View>
          );
        }}
      </Each>
    </View>
  );
}

export function ConfirmDialog(props: {
  request: () => ConfirmRequest | null;
  onDismiss: () => void;
}) {
  const look = currentTheme;
  const req = props.request;
  return (
    <SmeltDialog
      open={req() != null}
      role="alertdialog"
      accessibilityLabel={req()?.title ?? ""}
      accessibilityDescription={req()?.description ?? ""}
      onOpenChange={({ open }: { open: boolean }) => {
        if (!open) props.onDismiss();
      }}
      class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
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
          {req()!.title}
        </Text>
        <If when={Boolean(req()!.description)}>
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
            }}
          >
            {req()!.description}
          </Text>
        </If>
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            justifyContent: "flex-end",
            gap: 8,
          }}
        >
          <SmeltButton
            quiet={true}
            small={true}
            label={t("common.cancel")}
            onPress={props.onDismiss}
          />
          <SmeltButton
            primary={!req()!.danger}
            danger={req()!.danger}
            small={true}
            label={req()!.action}
            onPress={() => {
              const fn = req()!.onConfirm;
              props.onDismiss();
              fn();
            }}
          />
        </View>
      </If>
    </SmeltDialog>
  );
}
