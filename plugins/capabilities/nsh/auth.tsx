// 就地展开的登录面板：内置服务商的订阅 / API key 登录，和 MCP 服务器的 OAuth 登录共用。

import { computed, Text, View } from "nsh";
import { openUrl, SmeltButton, SmeltInput, t } from "smelt/sdk";
import { currentTheme, type LoginViewState, type PageController } from "./state.ts";
import { CopyButton } from "./ui.tsx";

export function LoginPanel(props: { page: PageController; view: () => LoginViewState }) {
  const look = currentTheme;
  const view = props.view;
  const failed = computed(() => view().error != null);
  const waiting = computed(
    () =>
      !failed() &&
      view().authUrl == null &&
      view().deviceCode == null &&
      view().prompt == null,
  );
  const recentMessages = computed(() => view().messages.slice(-3));

  return (
    <View
      style={{
        display: "flex",
        flexDirection: "column",
        width: "100%",
        gap: 10,
        paddingVertical: 4,
      }}
    >
      {/* 顶行：状态 / 错误 + 取消 / 关闭 */}
      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "space-between",
          width: "100%",
          gap: 12,
        }}
      >
        <View style={{ flex: 1, minWidth: 0 }}>
          <If when={failed()}>
            <Text
              style={{
                fontSize: look().type.small.size,
                lineHeight: look().type.small.line_height,
                color: look().palette.bad,
              }}
            >
              {t("auth.failed", { error: view().error ?? "" })}
            </Text>
          </If>
          <If when={waiting()}>
            <Text
              style={{
                fontSize: look().type.small.size,
                lineHeight: look().type.small.line_height,
                color: look().palette.muted,
              }}
            >
              {t("auth.waiting")}
            </Text>
          </If>
        </View>
        <SmeltButton
          variant="quiet"
          size="sm"
          label={failed() ? t("common.close") : t("common.cancel")}
          onPress={() => props.page.cancelLogin()}
        />
      </View>

      {/* Device code 流程 */}
      <If when={view().deviceCode != null}>
        <View style={{ display: "flex", flexDirection: "column", width: "100%", gap: 8 }}>
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              color: look().palette.muted,
            }}
          >
            {t("auth.device_code")}
          </Text>
          <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
            <View
              style={{
                paddingLeft: 12,
                paddingRight: 12,
                paddingTop: 4,
                paddingBottom: 4,
                borderRadius: look().radius.md,
                backgroundColor: look().palette.soft,
                borderWidth: 1,
                borderColor: look().palette.hair,
              }}
            >
              <Text
                style={{
                  fontFamily: look().fonts?.mono,
                  fontSize: 16,
                  fontWeight: 600,
                  color: look().palette.text,
                }}
              >
                {view().deviceCode?.userCode ?? ""}
              </Text>
            </View>
            <CopyButton
              value={view().deviceCode?.userCode ?? ""}
              tooltip={t("common.copy")}
            />
            <SmeltButton
              size="sm"
              label={t("auth.open_verification")}
              onPress={() => {
                const uri = view().deviceCode?.verificationUri;
                if (uri) void openUrl(uri).catch(() => {});
              }}
            />
          </View>
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              color: look().palette.muted,
            }}
          >
            {view().deviceCode?.verificationUri ?? ""}
          </Text>
        </View>
      </If>

      {/* 浏览器授权链接 */}
      <If when={view().authUrl != null}>
        <View style={{ display: "flex", flexDirection: "column", width: "100%", gap: 6 }}>
          <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
            <SmeltButton
              size="sm"
              label={t("auth.open_authorization")}
              onPress={() => {
                const url = view().authUrl;
                if (url) void openUrl(url).catch(() => {});
              }}
            />
            <CopyButton value={view().authUrl ?? ""} tooltip={t("auth.copy_link")} />
          </View>
          <If when={Boolean(view().instructions)}>
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: look().palette.muted,
              }}
            >
              {view().instructions ?? ""}
            </Text>
          </If>
        </View>
      </If>

      {/* 交互提问（选项或输入） */}
      <If when={view().prompt != null}>
        <View style={{ display: "flex", flexDirection: "column", width: "100%", gap: 6 }}>
          <Text
            style={{
              fontSize: look().type.text_xsmall_medium.size,
              fontWeight: look().type.text_xsmall_medium.weight,
              color: look().palette.text,
            }}
          >
            {view().prompt?.message ?? ""}
          </Text>
          <If when={Boolean(view().prompt?.placeholder)}>
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                color: look().palette.muted,
              }}
            >
              {t("auth.example", { placeholder: view().prompt?.placeholder ?? "" })}
            </Text>
          </If>
          <If
            when={view().prompt?.kind === "select"}
            else={
              <View
                style={{
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  width: "100%",
                  gap: 8,
                }}
              >
                <View style={{ flex: 1, minWidth: 0 }}>
                  <SmeltInput
                    value={view().inputValue}
                    masked={view().prompt?.kind === "secret"}
                    placeholder={view().prompt?.placeholder ?? ""}
                    accessibilityLabel={view().prompt?.message ?? ""}
                    onChange={({ value }) => props.page.setLoginInputValue(value)}
                    onSubmit={() => props.page.submitLoginPrompt()}
                    class="w-full"
                  />
                </View>
                <SmeltButton
                  size="sm"
                  label={
                    view().inputValue.trim().length === 0
                      ? t("auth.submit_empty")
                      : t("auth.submit")
                  }
                  onPress={() => props.page.submitLoginPrompt()}
                />
              </View>
            }
          >
            <View
              style={{
                display: "flex",
                flexDirection: "row",
                flexWrap: "wrap",
                alignItems: "center",
                gap: 8,
              }}
            >
              <Each of={view().prompt?.options ?? []} key={(opt: any) => opt.id}>
                {(opt: () => { id: string; label: string }) => (
                  <SmeltButton
                    size="sm"
                    label={opt().label}
                    onPress={() => props.page.chooseLoginOption(opt().id)}
                  />
                )}
              </Each>
            </View>
          </If>
        </View>
      </If>

      {/* 最近 3 条进度流水 */}
      <Each of={recentMessages()} key={(msg: string, ix: number) => `${ix}:${msg}`}>
        {(msg: () => string) => (
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: look().palette.muted,
            }}
          >
            {msg()}
          </Text>
        )}
      </Each>
    </View>
  );
}
