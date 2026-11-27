// 设置里的「Agent」页：本机 Agent CLI、Hooks、自定义 Agent、项目「+」新建菜单启动项。

import { cleanup, computed, Scroll, Text, View } from "nsh";
import {
  locale,
  SmeltButton,
  SmeltInput,
  SmeltMenuItem,
  SmeltMenuSeparator,
  t,
} from "smelt/sdk";

import {
  cliExecutableForAgent,
  isCliExecutableAvailable,
  launchEntryIsBuiltin,
  profileKindMeta,
  validateProfile,
} from "../model.js";
import {
  addLaunchEntry,
  addProfile,
  agentHostsSignal,
  catalog,
  cliCheckedAt,
  cliDiagnostics,
  cliInstalling,
  cliInstallResult,
  cliRefreshing,
  currentTheme,
  ensureSettingsInitialized,
  flushAllSettingsDrafts,
  flushLaunchEdits,
  flushProfileDraft,
  getProfileDraft,
  hooksBusy,
  hooksStatus,
  hostSettings,
  hostUnavailable,
  installCli,
  launchError,
  launchRows,
  LaunchRowState,
  profileDrafts,
  refreshCli,
  removeLaunchEntry,
  removeProfile,
  setHooks,
  setProfileKind,
  updateLaunchRow,
  updateProfileDraft,
} from "./state.ts";
import { DropdownButton } from "./ui.tsx";

function SettingsSection(props: { title: string; description?: string; children?: any }) {
  const look = currentTheme;
  return (
    <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <View style={{ display: "flex", flexDirection: "column", gap: 2 }}>
        <Text
          style={{
            fontSize: look().type.text_xsmall_medium.size,
            lineHeight: look().type.text_xsmall_medium.line_height,
            fontWeight: look().type.text_xsmall_medium.weight,
            color: look().palette.muted,
          }}
        >
          {props.title}
        </Text>
        <If when={Boolean(props.description)}>
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: look().palette.faint,
            }}
          >
            {props.description ?? ""}
          </Text>
        </If>
      </View>
      <View
        style={{
          borderRadius: look().radius.md,
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

function StatusDot(props: { size: number; color: string }) {
  return (
    <View
      class="rounded-full"
      style={{
        width: props.size,
        height: props.size,
        backgroundColor: props.color,
        flexShrink: 0,
      }}
    />
  );
}

// ---- 本机 Agent CLI ----

function CliSectionContent() {
  const look = currentTheme;
  const busy = () => cliRefreshing() || cliInstalling() != null;
  const checkedText = () => {
    const stamp = cliCheckedAt();
    return stamp ? t("cli.checked_at", { time: stamp }) : t("cli.never_checked");
  };

  const rows = computed(() => {
    const diag = cliDiagnostics();
    if (!diag) return [];
    const out: Array<{ agent: any; executable: any }> = [];
    for (const agent of catalog().conversation_agents) {
      const executable = cliExecutableForAgent(diag, agent.id);
      if (executable) {
        out.push({ agent, executable });
      }
    }
    return out;
  });

  return (
    <View
      style={{
        padding: 14,
        borderBottomWidth: 1,
        borderBottomColor: look().palette.hair,
        display: "flex",
        flexDirection: "column",
        gap: 12,
      }}
    >
      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 8,
        }}
      >
        <Text
          style={{
            color: look().palette.muted,
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
          }}
        >
          {checkedText()}
        </Text>
        <SmeltButton
          variant="quiet"
          size="sm"
          icon="icons/refresh.svg"
          loading={cliRefreshing()}
          disabled={busy()}
          tooltip={t("cli.refresh")}
          onPress={refreshCli}
        />
      </View>

      <If
        when={cliDiagnostics() != null}
        else={
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
            }}
          >
            {cliRefreshing() ? t("cli.detecting") : t("cli.no_status")}
          </Text>
        }
      >
        <Each of={rows()} key={(r: any) => r.agent.id}>
          {(row: () => { agent: any; executable: any }) => {
            const avail = () => isCliExecutableAvailable(row().executable);
            const statusText = () => {
              const ex = row().executable;
              if (!avail()) return t("cli.not_installed");
              if (ex.error) return t("cli.detect_failed", { error: ex.error });
              return ex.version || t("cli.installed_no_version");
            };
            const statusColor = () => {
              const p = look().palette;
              // 没装是常态（用不着的智能体），不算错；装了却探测失败才是错。
              if (!avail()) return p.muted;
              if (row().executable.error) return p.bad;
              return p.ok;
            };
            const pathText = () => row().executable.path || t("cli.not_on_path");
            const installingThis = () => cliInstalling() === row().agent.id;

            return (
              <View
                style={{
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  gap: 12,
                }}
              >
                <Text
                  style={{
                    width: 102,
                    flexShrink: 0,
                    color: look().palette.text,
                    fontSize: look().type.ui.size,
                    lineHeight: look().type.ui.line_height,
                    numberOfLines: 1,
                  }}
                >
                  {row().agent.label}
                </Text>
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
                      color: statusColor(),
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                      numberOfLines: 1,
                    }}
                  >
                    {statusText()}
                  </Text>
                  <Text
                    style={{
                      color: look().palette.muted,
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                      numberOfLines: 1,
                    }}
                  >
                    {pathText()}
                  </Text>
                </View>
                <If when={!avail()}>
                  <SmeltButton
                    size="sm"
                    icon="icons/plus.svg"
                    label={installingThis() ? t("cli.installing") : t("cli.install")}
                    loading={installingThis()}
                    disabled={busy()}
                    tooltip={t("cli.install_tooltip", { agent: row().agent.label })}
                    onPress={() => installCli(row().agent.id, row().agent.label)}
                  />
                </If>
              </View>
            );
          }}
        </Each>
      </If>

      <If when={cliInstallResult() != null}>
        <Text
          style={{
            color: cliInstallResult()?.failed ? look().palette.bad : look().palette.ok,
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
          }}
        >
          {cliInstallResult()?.message ?? ""}
        </Text>
      </If>
    </View>
  );
}

// ---- Agent Hooks ----

function HooksSectionContent() {
  const look = currentTheme;
  const status = () => hooksStatus();
  const busy = () => hooksBusy();

  const allInstalled = computed(() => {
    const s = status();
    return Boolean(s && s.providers.length > 0 && s.providers.every((p) => p.installed));
  });

  // 仅当至少一个 provider 已接入或本机 hooks_enabled 为 true 时才显示「移除」按钮
  const showRemove = computed(() => {
    agentHostsSignal();
    const s = status();
    const anyInstalled = Boolean(s?.providers?.some((p) => p.installed));
    const hooksEnabled = hostSettings(null)?.hooks_enabled === true;
    return anyInstalled || hooksEnabled;
  });

  return (
    <View
      style={{
        padding: 14,
        display: "flex",
        flexDirection: "column",
        gap: 10,
      }}
    >
      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          gap: 12,
        }}
      >
        <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
            }}
          >
            Hooks
          </Text>
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
            }}
          >
            {t("hooks.description")}
          </Text>
        </View>
        <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 6 }}>
          <If when={showRemove()}>
            <SmeltButton
              variant="quiet"
              size="sm"
              label={t("hooks.remove")}
              disabled={busy()}
              onPress={() => setHooks(false)}
            />
          </If>
          <SmeltButton
            size="sm"
            label={allInstalled() ? t("hooks.reinstall") : t("hooks.install")}
            disabled={busy()}
            onPress={() => setHooks(true)}
          />
        </View>
      </View>

      <If
        when={status() != null}
        else={
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
            }}
          >
            {t("hooks.loading")}
          </Text>
        }
      >
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            flexWrap: "wrap",
            gap: 12,
          }}
        >
          <Each
            of={status()?.providers ?? []}
            key={(p: { label: string }) => p.label}
          >
            {(provider: () => { label: string; installed: boolean }) => (
              <View
                style={{
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  gap: 5,
                }}
              >
                <StatusDot
                  size={6}
                  color={provider().installed ? look().palette.ok : look().palette.muted}
                />
                <Text
                  style={{
                    color: look().palette.text,
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                  }}
                >
                  {provider().label}
                </Text>
                <Text
                  style={{
                    color: look().palette.muted,
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                  }}
                >
                  {provider().installed ? t("hooks.connected") : t("hooks.not_connected")}
                </Text>
              </View>
            )}
          </Each>
        </View>
      </If>

      <If when={Boolean(status()?.notify_path)}>
        <Text
          style={{
            color: look().palette.muted,
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            fontFamily: look().fonts?.mono,
          }}
        >
          {`smelt-notify: ${status()?.notify_path ?? ""}`}
        </Text>
      </If>
    </View>
  );
}

// ---- 自定义 Agent ----

function ProfileRowCell(props: { profile: () => any }) {
  const look = currentTheme;
  const agents = () => catalog().conversation_agents;

  const draft = () => {
    profileDrafts();
    return getProfileDraft(props.profile());
  };

  const kindMeta = () => profileKindMeta(props.profile().kind_id, agents());

  const kindLabel = () => {
    const m = kindMeta();
    if (!m) return t("profile.kind_unknown", { id: props.profile().kind_id });
    if (m.isCustom) return t("profile.kind_generic");
    return t("profile.kind_based", { agent: m.short_label });
  };

  const commandPlaceholder = () => {
    const m = kindMeta();
    if (!m || m.isCustom) return t("profile.command_placeholder");
    return t("profile.command_inherit_placeholder", { agent: m.label });
  };

  const problem = () => {
    const d = draft();
    if (d.envError) return d.envError;
    return validateProfile(props.profile(), agents(), t);
  };

  const captionStyle = () => ({
    width: 76,
    flexShrink: 0,
    color: look().palette.muted,
    fontSize: look().type.text_xsmall.size,
    lineHeight: look().type.text_xsmall.line_height,
  });

  return (
    <View
      style={{
        padding: 14,
        borderBottomWidth: 1,
        borderBottomColor: look().palette.hair,
        display: "flex",
        flexDirection: "column",
        gap: 8,
      }}
    >
      {/* 第一行：类型下拉 + 名称 + 删除 */}
      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          gap: 8,
        }}
      >
        <View style={{ width: 152, flexShrink: 0 }}>
          <DropdownButton label={kindLabel()} small={true} fill={true} minWidth={184}>
            <SmeltMenuItem
              label={t("profile.kind_generic")}
              checked={props.profile().kind_id === "custom"}
              onSelect={() => setProfileKind(props.profile().id, "custom")}
            />
            <SmeltMenuSeparator />
            <Each of={agents()} key={(a: any) => a.id}>
              {(agent: () => any) => (
                <SmeltMenuItem
                  label={t("profile.kind_based", { agent: agent().label })}
                  checked={props.profile().kind_id === agent().id}
                  onSelect={() => setProfileKind(props.profile().id, agent().id)}
                />
              )}
            </Each>
          </DropdownButton>
        </View>
        <View style={{ flex: 1, minWidth: 0 }}>
          <SmeltInput
            value={draft().label}
            placeholder={t("profile.label_placeholder")}
            accessibilityLabel={t("profile.label_placeholder")}
            onChange={({ value }: { value: string }) =>
              updateProfileDraft(props.profile().id, { label: value })
            }
            onBlur={() => flushProfileDraft(props.profile().id)}
          />
        </View>
        <SmeltButton
          variant="quiet"
          size="sm"
          icon="icons/close.svg"
          tooltip={t("profile.delete")}
          onPress={() => removeProfile(props.profile().id)}
        />
      </View>

      {/* 启动命令 */}
      <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
        <Text style={captionStyle()}>{t("profile.command")}</Text>
        <View style={{ flex: 1, minWidth: 0 }}>
          <SmeltInput
            value={draft().command}
            placeholder={commandPlaceholder()}
            accessibilityLabel={t("profile.command")}
            onChange={({ value }: { value: string }) =>
              updateProfileDraft(props.profile().id, { command: value })
            }
            onBlur={() => flushProfileDraft(props.profile().id)}
            mono
          />
        </View>
      </View>

      {/* 环境变量 */}
      <View style={{ display: "flex", flexDirection: "row", alignItems: "flex-start", gap: 8 }}>
        <View style={{ paddingTop: 4 }}>
          <Text style={captionStyle()}>{t("profile.env")}</Text>
        </View>
        <View style={{ flex: 1, minWidth: 0 }}>
          <SmeltInput
            multiline={true}
            value={draft().envText}
            placeholder={t("profile.env_placeholder")}
            accessibilityLabel={t("profile.env")}
            onChange={({ value }: { value: string }) =>
              updateProfileDraft(props.profile().id, { envText: value })
            }
            onBlur={() => flushProfileDraft(props.profile().id)}
            size="sm"
            mono
            class="min-h-10 max-h-40"
          />
        </View>
      </View>

      {/* 数据目录（仅当底层 agent 支持 config_dir_env_var 时显示） */}
      <If when={Boolean(kindMeta()?.config_dir_env_var)}>
        <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
          <Text style={captionStyle()}>{t("profile.dir")}</Text>
          <View style={{ flex: 1, minWidth: 0 }}>
            <SmeltInput
              value={draft().workspace_dir}
              placeholder={t("profile.dir_placeholder")}
              accessibilityLabel={t("profile.dir")}
              onChange={({ value }: { value: string }) =>
                updateProfileDraft(props.profile().id, { workspace_dir: value })
              }
              onBlur={() => flushProfileDraft(props.profile().id)}
              mono
            />
          </View>
          <Text
            style={{
              flexShrink: 0,
              color: look().palette.muted,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              fontFamily: look().fonts?.mono,
            }}
          >
            {kindMeta()?.config_dir_env_var ?? ""}
          </Text>
        </View>
      </If>

      {/* 校验提示 */}
      <If when={problem() != null}>
        <Text
          style={{
            color: look().palette.bad,
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
          }}
        >
          {problem() ?? ""}
        </Text>
      </If>
    </View>
  );
}

function CustomAgentsSection() {
  const look = currentTheme;
  const localSettings = computed(() => {
    locale();
    agentHostsSignal();
    return hostSettings(null);
  });
  const profiles = computed(() => localSettings()?.profiles ?? []);
  const unavailable = computed(() => {
    agentHostsSignal();
    return hostUnavailable(null);
  });

  return (
    <SettingsSection
      title={t("settings.custom_agents")}
      description={t("profile.section_description")}
    >
      <If
        when={localSettings() != null}
        else={
          <View style={{ padding: 14 }}>
            <Text
              style={{
                color: look().palette.muted,
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
              }}
            >
              {unavailable() ?? t("profile.loading")}
            </Text>
          </View>
        }
      >
        <Each of={profiles()} key={(p: any) => p.id}>
          {(profile: () => any) => <ProfileRowCell profile={profile} />}
        </Each>
        <View style={{ padding: 14, display: "flex", flexDirection: "row" }}>
          <SmeltButton
            size="sm"
            icon="icons/plus.svg"
            label={t("profile.add")}
            onPress={addProfile}
          />
        </View>
      </If>
    </SettingsSection>
  );
}

// ---- 项目「+」新建菜单启动项 ----

function LaunchMenuSection() {
  const look = currentTheme;
  const rows = () => launchRows();
  const terminalIds = () => catalog().terminal_agent_ids;

  return (
    <SettingsSection
      title={t("settings.new_menu")}
      description={t("launch.section_description")}
    >
      <If
        when={rows() != null}
        else={
          <View style={{ padding: 14 }}>
            <Text
              style={{
                color: look().palette.muted,
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
              }}
            >
              {launchError()
                ? t("launch.load_failed", { error: launchError() ?? "" })
                : t("launch.loading")}
            </Text>
          </View>
        }
      >
        <View
          style={{
            padding: 14,
            borderBottomWidth: 1,
            borderBottomColor: look().palette.hair,
            display: "flex",
            flexDirection: "column",
            gap: 8,
          }}
        >
          {/* 表头 */}
          <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
            <Text
              style={{
                width: 140,
                flexShrink: 0,
                color: look().palette.muted,
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
              }}
            >
              {t("launch.name_column")}
            </Text>
            <Text
              style={{
                flex: 1,
                minWidth: 0,
                color: look().palette.muted,
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
              }}
            >
              {t("launch.command_column")}
            </Text>
            <View style={{ width: 24, flexShrink: 0 }} />
          </View>

          <Each of={rows() ?? []} key={(r: LaunchRowState) => r.uid}>
            {(row: () => LaunchRowState) => {
              const removable = () => !launchEntryIsBuiltin(row(), terminalIds());
              return (
                <View
                  style={{
                    display: "flex",
                    flexDirection: "row",
                    alignItems: "center",
                    gap: 8,
                  }}
                >
                  <View style={{ width: 140, flexShrink: 0 }}>
                    <SmeltInput
                      value={row().label}
                      placeholder={t("launch.label_placeholder")}
                      accessibilityLabel={t("launch.name_column")}
                      onChange={({ value }: { value: string }) =>
                        updateLaunchRow(row().uid, { label: value })
                      }
                      onBlur={flushLaunchEdits}
                    />
                  </View>
                  <View style={{ flex: 1, minWidth: 0 }}>
                    <SmeltInput
                      value={row().command}
                      placeholder={t("launch.command_placeholder")}
                      accessibilityLabel={t("launch.command_column")}
                      onChange={({ value }: { value: string }) =>
                        updateLaunchRow(row().uid, { command: value })
                      }
                      onBlur={flushLaunchEdits}
                      mono
                    />
                  </View>
                  <If
                    when={removable()}
                    else={<View style={{ width: 24, height: 24, flexShrink: 0 }} />}
                  >
                    <SmeltButton
                      variant="quiet"
                      size="sm"
                      icon="icons/close.svg"
                      tooltip={t("launch.delete")}
                      onPress={() => removeLaunchEntry(row().uid)}
                    />
                  </If>
                </View>
              );
            }}
          </Each>
        </View>

        <View style={{ padding: 14, display: "flex", flexDirection: "row" }}>
          <SmeltButton
            size="sm"
            icon="icons/plus.svg"
            label={t("launch.add")}
            onPress={addLaunchEntry}
          />
        </View>
      </If>
    </SettingsSection>
  );
}

// ---- 设置页根视图 ----

export function AgentSettingsView() {
  ensureSettingsInitialized();
  cleanup(() => {
    flushAllSettingsDrafts();
  });

  return (
    <Scroll style={{ flex: 1, width: "100%", height: "100%" }}>
      <View
        style={{
          maxWidth: 680,
          width: "100%",
          padding: 24,
          display: "flex",
          flexDirection: "column",
          gap: 24,
        }}
      >
        <SettingsSection title={t("settings.local_agents")}>
          <CliSectionContent />
          <HooksSectionContent />
        </SettingsSection>

        <CustomAgentsSection />
        <LaunchMenuSection />
      </View>
    </Scroll>
  );
}
