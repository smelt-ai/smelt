// 自动化插件共用 UI 组件：严格遵循 `docs/ui-design-language.md` 与 NSH 规范。
import { computed, Text, View } from "nsh";
import {
  Button as BaseButton,
  Dialog,
  Input,
  MenuItem,
  MenuSeparator,
  Switch,
} from "nsh/base";
import {
  DIALOG_SURFACE_CLASS,
  MENU_ITEM_CLASS,
  POPUP_CLASS,
  SmeltButton,
  SmeltIcon,
  t,
} from "smelt/sdk";
export { MENU_ITEM_CLASS, POPUP_CLASS };
import { deleteConfirmDescription } from "../model.js";
import { confirmDeleteAutomation, currentTheme, deletePrompt } from "./state.ts";

export function PageToolbar(props: {
  children?: any;
  justify?: "between" | "start";
  style?: any;
}) {
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
        justifyContent: props.justify === "start" ? "flex-start" : "space-between",
        gap: 12,
        flexShrink: 0,
        ...(props.style ?? {}),
      }}
    >
      {props.children}
    </View>
  );
}

export function ContentCard(props: { children?: any; style?: any; testID?: string }) {
  const look = currentTheme;
  return (
    <View
      testID={props.testID}
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

export function StatusDot(props: { size?: number; color: string; style?: any }) {
  const s = () => props.size ?? 7;
  return (
    <View
      style={{
        width: s(),
        height: s(),
        borderRadius: s() / 2,
        backgroundColor: props.color,
        flexShrink: 0,
        ...(props.style ?? {}),
      }}
    />
  );
}

export function NeutralIconMark(props: { icon: string; size?: number }) {
  const look = currentTheme;
  const box = () => props.size ?? 28;
  const glyph = () => Math.round(box() * 0.5);
  return (
    <View
      style={{
        width: box(),
        height: box(),
        borderRadius: look().radius.row,
        backgroundColor: look().palette.soft,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        flexShrink: 0,
      }}
    >
      <SmeltIcon path={props.icon} size={glyph()} color={look().palette.muted} />
    </View>
  );
}

export function SectionHeading(props: { label: string; hint?: string | null }) {
  const look = currentTheme;
  return (
    <View style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      <Text
        style={{
          fontSize: look().type.ui_medium.size,
          lineHeight: look().type.ui_medium.line_height,
          fontWeight: look().type.ui_medium.weight,
          color: look().palette.text,
        }}
      >
        {props.label}
      </Text>
      <If when={Boolean(props.hint)}>
        <Text
          style={{
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            color: look().palette.faint,
          }}
        >
          {props.hint ?? ""}
        </Text>
      </If>
    </View>
  );
}

export function FieldHint(props: { text: string }) {
  const look = currentTheme;
  return (
    <Text
      style={{
        fontSize: look().type.text_xsmall.size,
        lineHeight: look().type.text_xsmall.line_height,
        color: look().palette.faint,
      }}
    >
      {props.text}
    </Text>
  );
}

export function ErrorAlert(props: {
  message: string;
  onRetry?: () => void;
  onDismiss?: () => void;
  testID?: string;
}) {
  const look = currentTheme;
  return (
    <View
      testID={props.testID}
      style={{
        width: "100%",
        paddingLeft: 12,
        paddingRight: 12,
        paddingTop: 10,
        paddingBottom: 10,
        borderRadius: look().radius.row,
        borderWidth: 1,
        borderColor: look().palette.bad,
        backgroundColor: look().palette.surface,
        display: "flex",
        flexDirection: "row",
        alignItems: "flex-start",
        gap: 8,
      }}
    >
      <View style={{ paddingTop: 2, flexShrink: 0 }}>
        <SmeltIcon path="icons/circle-x.svg" size={14} color={look().palette.bad} />
      </View>
      <Text
        style={{
          flex: 1,
          minWidth: 0,
          fontSize: look().type.small.size,
          lineHeight: look().type.small.line_height,
          color: look().palette.text,
        }}
      >
        {props.message}
      </Text>
      <If when={Boolean(props.onRetry)}>
        <SmeltButton
          small={true}
          icon="icons/refresh.svg"
          label={t("run.retry")}
          onPress={() => props.onRetry?.()}
        />
      </If>
      <If when={Boolean(props.onDismiss)}>
        <SmeltButton
          dismiss={true}
          icon="icons/close.svg"
          accessibilityLabel={t("error.dismiss")}
          onPress={() => props.onDismiss?.()}
        />
      </If>
    </View>
  );
}

export function SegmentedControl<T extends string>(props: {
  options: Array<{ value: T; label: string }>;
  selected: T;
  onSelect: (value: T) => void;
  testID?: string;
}) {
  const look = currentTheme;
  return (
    <View
      testID={props.testID}
      style={{
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        padding: 2,
        borderRadius: look().radius.md,
        backgroundColor: look().palette.soft,
        borderWidth: 1,
        borderColor: look().palette.hair,
        gap: 2,
        flexShrink: 0,
      }}
    >
      <Each of={props.options} key={(opt: { value: T; label: string }) => opt.value}>
        {(opt: () => { value: T; label: string }) => {
          const active = () => props.selected === opt().value;
          return (
            <BaseButton
              selected={active()}
              onPress={() => props.onSelect(opt().value)}
              style={{
                height: 24,
                paddingLeft: 10,
                paddingRight: 10,
                borderRadius: look().radius.sm,
                backgroundColor: active() ? look().palette.bg : "transparent",
                borderWidth: 1,
                borderColor: active() ? look().palette.hair_strong : "transparent",
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                cursor: "default",
              }}
              hoverStyle={{
                backgroundColor: active() ? look().palette.bg : look().palette.row_hover,
              }}
            >
              <Text
                style={{
                  fontSize: look().type.small.size,
                  lineHeight: look().type.small.line_height,
                  fontWeight: active() ? look().type.small_medium.weight : look().type.small.weight,
                  color: active() ? look().palette.text : look().palette.muted,
                  numberOfLines: 1,
                }}
              >
                {opt().label}
              </Text>
            </BaseButton>
          );
        }}
      </Each>
    </View>
  );
}

export function MultiSegmentedControl(props: {
  items: Array<{ key: string; label: string; active: boolean }>;
  onToggle: (key: string) => void;
  testID?: string;
}) {
  const look = currentTheme;
  return (
    <View
      testID={props.testID}
      style={{
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        padding: 2,
        borderRadius: look().radius.md,
        backgroundColor: look().palette.soft,
        borderWidth: 1,
        borderColor: look().palette.hair,
        gap: 2,
        flexShrink: 0,
      }}
    >
      <Each of={props.items} key={(it: { key: string }) => it.key}>
        {(item: () => { key: string; label: string; active: boolean }) => (
          <BaseButton
            selected={item().active}
            onPress={() => props.onToggle(item().key)}
            style={{
              height: 24,
              minWidth: 32,
              paddingLeft: 8,
              paddingRight: 8,
              borderRadius: look().radius.sm,
              backgroundColor: item().active ? look().palette.bg : "transparent",
              borderWidth: 1,
              borderColor: item().active ? look().palette.hair_strong : "transparent",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              cursor: "default",
            }}
            hoverStyle={{
              backgroundColor: item().active ? look().palette.bg : look().palette.row_hover,
            }}
          >
            <Text
              style={{
                fontSize: look().type.small.size,
                lineHeight: look().type.small.line_height,
                fontWeight: item().active
                  ? look().type.small_medium.weight
                  : look().type.small.weight,
                color: item().active ? look().palette.text : look().palette.muted,
                numberOfLines: 1,
              }}
            >
              {item().label}
            </Text>
          </BaseButton>
        )}
      </Each>
    </View>
  );
}

export function RowItem(props: {
  selected?: boolean;
  disabled?: boolean;
  onPress?: () => void;
  style?: any;
  testID?: string;
  children?: any;
}) {
  const look = currentTheme;
  const bg = () => (props.selected ? look().palette.row_selected : "transparent");
  return (
    <BaseButton
      testID={props.testID}
      disabled={props.disabled}
      selected={Boolean(props.selected)}
      onPress={() => {
        if (!props.disabled) props.onPress?.();
      }}
      style={{
        width: "100%",
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        backgroundColor: bg(),
        cursor: "default",
        ...(props.style ?? {}),
      }}
      hoverStyle={
        props.disabled
          ? undefined
          : {
              backgroundColor: props.selected
                ? look().palette.row_selected
                : look().palette.row_hover,
            }
      }
      activeStyle={props.disabled ? undefined : { backgroundColor: look().palette.row_selected }}
    >
      {props.children}
    </BaseButton>
  );
}

export function MenuRow(props: {
  label: string;
  icon?: string | null;
  danger?: boolean;
  disabled?: boolean;
  onSelect: () => void;
}) {
  const look = currentTheme;
  const fg = () => (props.danger ? look().palette.bad : look().palette.text);
  const iconColor = () => (props.danger ? look().palette.bad : look().palette.muted);
  return (
    <MenuItem
      closeOnSelect={true}
      disabled={props.disabled}
      onSelect={() => {
        if (!props.disabled) props.onSelect();
      }}
      class={MENU_ITEM_CLASS}
      style={{
        height: 28,
        gap: 8,
        opacity: props.disabled ? 0.45 : 1,
      }}
    >
      <If when={Boolean(props.icon)}>
        <View style={{ width: 14, height: 14, flexShrink: 0 }}>
          <SmeltIcon path={props.icon!} size={14} color={iconColor()} />
        </View>
      </If>
      <Text
        class="flex-1 min-w-0"
        style={{
          color: fg(),
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
          numberOfLines: 1,
        }}
      >
        {props.label}
      </Text>
    </MenuItem>
  );
}

export function MenuSectionHeader(props: { label: string }) {
  const look = currentTheme;
  return (
    <View style={{ paddingLeft: 8, paddingRight: 8, paddingTop: 4, paddingBottom: 4 }}>
      <Text
        style={{
          fontSize: look().type.text_xsmall_medium.size,
          lineHeight: look().type.text_xsmall_medium.line_height,
          fontWeight: look().type.text_xsmall_medium.weight,
          color: look().palette.faint,
        }}
      >
        {props.label}
      </Text>
    </View>
  );
}

export function MenuDivider() {
  return <MenuSeparator class="my-1 bg-border" style={{ height: 1 }} />;
}

export function AutomationSwitch(props: {
  checked: boolean;
  disabled?: boolean;
  accessibilityLabel: string;
  onChange: (checked: boolean) => void;
  testID?: string;
}) {
  const look = currentTheme;
  return (
    <Switch
      testID={props.testID}
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
      }}
      thumbStyle={{
        width: 16,
        height: 16,
        borderRadius: 8,
        backgroundColor: look().palette.on_ink,
      }}
    />
  );
}

export function StyledInput(props: {
  value: string;
  placeholder?: string;
  accessibilityLabel?: string;
  autoFocus?: boolean;
  invalid?: boolean;
  multiline?: boolean;
  bare?: boolean;
  onChange: (value: string) => void;
  onSubmit?: () => void;
  style?: any;
  testID?: string;
}) {
  const look = currentTheme;
  return (
    <Input
      testID={props.testID}
      value={props.value}
      placeholder={props.placeholder}
      accessibilityLabel={props.accessibilityLabel}
      autoFocus={props.autoFocus}
      multiline={props.multiline}
      onChange={({ value }: { value: string }) => props.onChange(value)}
      onSubmit={props.onSubmit}
      class={
        props.bare
          ? "text-foreground"
          : "rounded-(--radius) border border-input bg-card px-2 text-foreground"
      }
      style={{
        ...(props.multiline ? { minHeight: 112 } : { height: 28 }),
        fontSize: look().type.ui.size,
        lineHeight: look().type.ui.line_height,
        ...(props.invalid ? { borderColor: look().palette.bad } : {}),
        ...(props.style ?? {}),
      }}
    />
  );
}

/** 运行条目三行字：标题、次级时间/来源，以及失败原因（红色）。 */
export function RunLines(props: {
  title: string;
  detail: string;
  error?: string | null;
  selected?: boolean;
}) {
  const look = currentTheme;
  return (
    <View
      style={{
        flex: 1,
        minWidth: 0,
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
          color: look().palette.text,
          numberOfLines: 1,
        }}
      >
        {props.title}
      </Text>
      <Text
        style={{
          fontSize: look().type.text_xsmall.size,
          lineHeight: look().type.text_xsmall.line_height,
          color: props.selected ? look().palette.muted_selected : look().palette.muted,
          numberOfLines: 1,
        }}
      >
        {props.detail}
      </Text>
      <If when={Boolean(props.error)}>
        <Text
          style={{
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            color: look().palette.bad,
            numberOfLines: 1,
          }}
        >
          {props.error ?? ""}
        </Text>
      </If>
    </View>
  );
}

/** 破坏性确认对话框：无右上角 ×，m-auto 居中，安静取消 + 危险删除。 */
export function DeleteConfirmDialog() {
  const look = currentTheme;
  const prompt = computed(() => deletePrompt());
  const description = computed(() => {
    const p = prompt();
    if (!p) return "";
    return deleteConfirmDescription(p.name, p.runCount, t);
  });

  return (
    <Dialog
      open={prompt() != null}
      role="alertdialog"
      animation="fade"
      accessibilityLabel={t("delete.title")}
      backdropClass="bg-black/40"
      onOpenChange={({ open }: { open: boolean }) => {
        if (!open) deletePrompt.set(null);
      }}
      class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
      style={{ width: 420, gap: 14 }}
    >
      <If when={prompt() != null}>
        <Text
          style={{
            color: look().palette.text,
            fontSize: look().type.ui_medium.size,
            lineHeight: look().type.ui_medium.line_height,
            fontWeight: look().type.ui_medium.weight,
          }}
        >
          {t("delete.title")}
        </Text>
        <Text
          style={{
            color: look().palette.muted,
            fontSize: look().type.ui.size,
            lineHeight: look().type.ui.line_height,
          }}
        >
          {description()}
        </Text>
        <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
          <SmeltButton
            quiet={true}
            small={true}
            label={t("delete.cancel")}
            onPress={() => deletePrompt.set(null)}
          />
          <SmeltButton
            danger={true}
            small={true}
            label={t("delete.confirm")}
            onPress={confirmDeleteAutomation}
          />
        </View>
      </If>
    </Dialog>
  );
}

export function runStatusColor(status: string, palette: any): string {
  switch (status) {
    case "completed":
      return palette.ok;
    case "failed":
      return palette.bad;
    case "cancelled":
    case "skipped":
      return palette.faint;
    case "awaiting_approval":
    case "waiting_for_user":
      return palette.need;
    default:
      return palette.run;
  }
}
