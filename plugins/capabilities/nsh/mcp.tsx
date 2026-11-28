// 能力库「MCP」页签：内置预设一键添加、已配置服务器启停/登录/移除/智能体勾选、mcp.json 入口。

import { computed, Text, View } from "nsh";
import {
  openUrl,
  piMcpPresets,
  SmeltButton,
  SmeltPageScroll,
  SmeltSection,
  SmeltSectionCell,
  SmeltSettingRow,
  t,
} from "smelt/sdk";
import {
  formatMcpTransportDetail,
  formatUsedByLine,
  isMcpServerHttp,
} from "../model.js";
import { LoginPanel } from "./auth.tsx";
import { agentSettingsOn, currentTheme, type PageController } from "./state.ts";
import { BadgePill, StatusLine, UsesPopover } from "./ui.tsx";

export function McpTab(props: { page: PageController }) {
  const look = currentTheme;
  const cfg = computed(() => props.page.piConfig());
  const mcpResult = computed(() =>
    cfg() ? cfg()!.mcpServers : { ok: false, error: props.page.pendingText() },
  );
  const servers = computed(() => (mcpResult().ok ? (mcpResult() as any).value : []));
  const presets = computed(() => {
    const names = new Set(servers().map((server: any) => server.name));
    return piMcpPresets(props.page.remoteDesktop()).map((preset) => ({
      ...preset,
      added: names.has(preset.id),
    }));
  });
  const isRemote = computed(() => props.page.machine() !== null);
  const mcpPath = computed(() => cfg()?.paths.mcp ?? "");

  const runningMcpLogin = computed(() => {
    const view = props.page.loginView();
    if (!view || view.error != null || view.target.kind !== "mcp") return null;
    return view.target.name;
  });

  return (
    <SmeltPageScroll maxWidth={760}>
      {/* 内置预设 */}
      <SmeltSection title={t("mcp.builtin")}>
        <Each of={presets()} key={(p: any) => p.id}>
          {(preset: () => any, ix: () => number) => (
            <SmeltSectionCell divider={ix() > 0}>
              <SmeltSettingRow title={preset().name} description={preset().summary}>
                <If when={Boolean(preset().extension_url)}>
                  <SmeltButton
                    variant="quiet"
                    size="sm"
                    label={t("mcp.install_extension")}
                    tooltip={t("mcp.install_extension_tooltip")}
                    onPress={() => {
                      const url = preset().extension_url;
                      if (url) void openUrl(url).catch(() => {});
                    }}
                  />
                </If>
                <If
                  when={preset().added}
                  else={
                    <SmeltButton
                      size="sm"
                      label={t("mcp.add")}
                      tooltip={
                        preset().needs_sign_in
                          ? t("mcp.add_tooltip_sign_in")
                          : t("mcp.add_tooltip")
                      }
                      onPress={() => props.page.addMcpPreset(preset().id)}
                    />
                  }
                >
                  <BadgePill label={t("mcp.added")} tone="neutral" />
                </If>
              </SmeltSettingRow>
            </SmeltSectionCell>
          )}
        </Each>
      </SmeltSection>

      {/* 已配置服务器 */}
      <SmeltSection title={t("mcp.configured")}>
        <If when={props.page.mcpError() != null}>
          <SmeltSectionCell>
            <StatusLine
              text={t("common.action_failed", { error: props.page.mcpError() ?? "" })}
              error={true}
            />
          </SmeltSectionCell>
        </If>

        <If when={props.page.agentEditError() != null}>
          <SmeltSectionCell divider={props.page.mcpError() != null}>
            <StatusLine
              text={t("uses.failed", { error: props.page.agentEditError() ?? "" })}
              error={true}
            />
          </SmeltSectionCell>
        </If>

        <If
          when={mcpResult().ok}
          else={
            <SmeltSectionCell divider={props.page.mcpError() != null || props.page.agentEditError() != null}>
              <StatusLine
                text={
                  cfg() == null
                    ? props.page.pendingText()
                    : t("mcp.read_failed", { error: (mcpResult() as any).error ?? "" })
                }
                error={cfg() != null || props.page.piConfigError() != null}
              />
            </SmeltSectionCell>
          }
        >
          <If
            when={servers().length > 0}
            else={
              <SmeltSectionCell divider={props.page.mcpError() != null || props.page.agentEditError() != null}>
                <StatusLine text={t("mcp.empty")} />
              </SmeltSectionCell>
            }
          >
            <Each of={servers()} key={(s: any) => s.name}>
              {(server: () => any, ix: () => number) => {
                const needsDivider = () =>
                  ix() > 0 || props.page.mcpError() != null || props.page.agentEditError() != null;
                const usable = () => ({ kind: "mcp" as const, id: server().name });
                const usedBy = () =>
                  formatUsedByLine(agentSettingsOn(props.page.machine()), usable(), t);
                const detail = () => formatMcpTransportDetail(server().transport, t);
                const loggingIn = () => runningMcpLogin() === server().name;
                const anyLogin = () => runningMcpLogin() != null;
                const loginTargetMatches = () => {
                  const view = props.page.loginView();
                  return view != null && view.target.kind === "mcp" && view.target.name === server().name;
                };

                return (
                  <>
                    <SmeltSectionCell divider={needsDivider()}>
                      <View
                        style={{
                          display: "flex",
                          flexDirection: "row",
                          alignItems: "center",
                          justifyContent: "space-between",
                          gap: 12,
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
                          <View
                            style={{
                              display: "flex",
                              flexDirection: "row",
                              alignItems: "center",
                              gap: 8,
                            }}
                          >
                            <Text
                              style={{
                                fontSize: look().type.ui.size,
                                lineHeight: look().type.ui.line_height,
                                color: look().palette.text,
                                numberOfLines: 1,
                              }}
                            >
                              {server().name}
                            </Text>
                            <If when={!server().enabled}>
                              <BadgePill label={t("mcp.disabled")} tone="neutral" />
                            </If>
                            <If when={server().enabled && server().signed_in}>
                              <BadgePill label={t("auth.status.oauth")} tone="ok" />
                            </If>
                          </View>
                          <Text
                            style={{
                              fontSize: look().type.text_xsmall.size,
                              lineHeight: look().type.text_xsmall.line_height,
                              color: look().palette.muted,
                              numberOfLines: 1,
                            }}
                          >
                            {detail()}
                          </Text>
                          <If when={usedBy() != null}>
                            <Text
                              style={{
                                fontSize: look().type.text_xsmall.size,
                                lineHeight: look().type.text_xsmall.line_height,
                                color: look().palette.muted,
                                numberOfLines: 1,
                              }}
                            >
                              {usedBy() ?? ""}
                            </Text>
                          </If>
                        </View>

                        <View
                          style={{
                            display: "flex",
                            flexDirection: "row",
                            alignItems: "center",
                            flexShrink: 0,
                            gap: 6,
                          }}
                        >
                          <UsesPopover page={props.page} usable={usable()} />
                          <If when={isMcpServerHttp(server()) && server().enabled}>
                            <SmeltButton
                              size="sm"
                              label={
                                loggingIn()
                                  ? t("mcp.waiting")
                                  : server().signed_in
                                    ? t("auth.sign_in_again")
                                    : t("auth.sign_in")
                              }
                              tooltip={t("mcp.sign_in_tooltip")}
                              disabled={anyLogin()}
                              onPress={() => props.page.startMcpLogin(server().name)}
                            />
                          </If>
                          <SmeltButton
                            variant="quiet"
                            size="sm"
                            label={server().enabled ? t("mcp.disable") : t("mcp.enable")}
                            disabled={loggingIn()}
                            onPress={() =>
                              props.page.setMcpServerEnabled(server().name, !server().enabled)
                            }
                          />
                          <SmeltButton
                            variant="quiet"
                            size="sm"
                            icon="icons/trash.svg"
                            accessibilityLabel={t("mcp.remove_tooltip")}
                            tooltip={t("mcp.remove_tooltip")}
                            disabled={loggingIn() || props.page.confirmDialog() != null}
                            onPress={() => props.page.requestRemoveMcpServer(server().name)}
                          />
                        </View>
                      </View>
                    </SmeltSectionCell>
                    <If when={loginTargetMatches()}>
                      <SmeltSectionCell divider={true}>
                        <LoginPanel page={props.page} view={() => props.page.loginView()!} />
                      </SmeltSectionCell>
                    </If>
                  </>
                );
              }}
            </Each>
          </If>
        </If>
      </SmeltSection>

      {/* 原生文件入口与生效说明 */}
      <View
        style={{
          display: "flex",
          flexDirection: "column",
          width: "100%",
          gap: 8,
          paddingTop: 12,
          borderTopWidth: 1,
          borderTopColor: look().palette.hair,
        }}
      >
        <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              color: look().palette.muted,
            }}
          >
            {t("config.native_files")}
          </Text>
          <SmeltButton
            variant="quiet"
            size="sm"
            label="mcp.json"
            tooltip={mcpPath() || undefined}
            disabled={isRemote()}
            onPress={() => props.page.openConfigFile("mcp")}
          />
        </View>
        <If when={isRemote()}>
          <StatusLine text={t("config.remote_file", { name: props.page.currentMachineName() })} />
        </If>
        <StatusLine text={t("mcp.footer")} />
      </View>
    </SmeltPageScroll>
  );
}
