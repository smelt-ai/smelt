// 自动化插件共用 UI 组件：严格遵循 `docs/ui-design-language.md` 与 NSH 规范。
import { Text, View } from "nsh";
import { SmeltButton, SmeltIcon, t } from "smelt/sdk";
import { currentTheme } from "./state.ts";

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
      class="rounded-xl"
      style={{
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
      class="rounded-full"
      style={{
        width: s(),
        height: s(),
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
          size="sm"
          icon="icons/refresh.svg"
          label={t("run.retry")}
          onPress={() => props.onRetry?.()}
        />
      </If>
      <If when={Boolean(props.onDismiss)}>
        <SmeltButton
          variant="dismiss"
          size="xs"
          icon="icons/close.svg"
          accessibilityLabel={t("error.dismiss")}
          onPress={() => props.onDismiss?.()}
        />
      </If>
    </View>
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
      return palette.muted;
  }
}
