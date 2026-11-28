// 智能体编辑页：页头（返回/缩写/标题/页签分段/开始对话）、配置页签、自动化页签与记忆页签。

import { cleanup, computed, Scroll, Text, View } from "nsh";
import { Submenu } from "nsh/base";
import {
  locale,
  MENU_ITEM_CLASS,
  pathJoin,
  pathStyle,
  SmeltButton,
  SmeltDisclosure,
  SmeltDisclosureChevron,
  SmeltIcon,
  SmeltInput,
  SmeltMenuItem,
  SmeltMenuSeparator,
  SmeltRow,
  SmeltSegmented,
  t,
  views,
  workspaceDirectory,
} from "smelt/sdk";

import {
  agentConversationCount,
  agentDisplayName,
  agentEngineId,
  agentInstructionsFooter,
  agentModelLabel,
  agentUsageSummary,
  buildCapabilitiesList,
  CORE_MEMORY_MAX_BYTES,
  filterCapabilities,
  memoryPendingCount,
  memorySummaryText,
  pendingReasonText,
  visibleMemories,
  WORKSPACE_SKILL_DIRS,
} from "../model.js";
import { AgentAutomationsTab } from "./automations_tab.tsx";
import {
  addAgentContextLink,
  agentHostsSignal,
  AgentRef,
  approveAgentMemory,
  automationCountFor,
  automationNamesFor,
  automationsViewRegistered,
  capabilitiesViewRegistered,
  capabilityQuery,
  catalog,
  closeAgentSettings,
  commitConversationDraft,
  contextLinkInput,
  currentTheme,
  editingAgent,
  editingConversationDraft,
  editorName,
  editorPrompt,
  editorTab,
  expandedMemory,
  findEngineMeta,
  flushEditorText,
  getAgentDefinition,
  hostSettings,
  hostUnavailable,
  memoriesState,
  memoryFilter,
  MemoryFilter,
  MemoryLayer,
  memoryQuery,
  openAgentMemoryFile,
  openAgentMemoryFolder,
  openCapabilities,
  pageError,
  pickAgentContextFolders,
  piSnapshot,
  refreshAgentMemories,
  refreshAgentPiConfig,
  removeAgentContextFolder,
  removeAgentContextLink,
  requestDeleteAgent,
  requestDeleteMemory,
  selectEditorTab,
  setAgentEngine,
  setAgentMemoryEnabled,
  setAgentModel,
  setEditorNameInput,
  setEditorPromptInput,
  startAgentConversation,
  toggleAgentMcpServer,
  toggleAgentPlugin,
  toggleMemoryExpanded,
} from "./state.ts";
import {
  AgentMonogram,
  DropdownButton,
  EngineChip,
  EngineMark,
  ErrorAlert,
  PageToolbar,
  QuietBadge,
  SurfaceCard,
  ToggleSwitch,
} from "./ui.tsx";

function memoryKindLabel(kind: string | null | undefined): string {
  switch (kind) {
    case "user":
      return t("memory.kind.user");
    case "feedback":
      return t("memory.kind.feedback");
    case "project":
      return t("memory.kind.project");
    case "reference":
      return t("memory.kind.reference");
    default:
      return t("memory.kind.invalid");
  }
}

function memoryUsageInfo(entry: any): { label: string; alert: boolean } | null {
  if (entry?.pending_reason != null) {
    return { label: t("memory.filter.pending"), alert: true };
  }
  if (entry?.stale) {
    return { label: t("memory.stale"), alert: true };
  }
  const count = Number(entry?.read_count ?? 0);
  if (count > 0) {
    return {
      label: count === 1 ? t("memory.used.one") : t("memory.used.other", { count }),
      alert: false,
    };
  }
  return null;
}

function memorySectionTitle(title: string, entries: any[], shown: number | null): string {
  const total = entries.length;
  const count =
    shown != null ? t("memory.count_shown", { shown, total }) : t("memory.count", { total });
  const pending = entries.filter((e) => e.pending_reason != null).length;
  if (pending === 0) {
    return `${title} · ${count}`;
  }
  return t("memory.section_pending", { title, count, pending });
}

function modelChoiceShortLabel(choice: any): string {
  const name = typeof choice?.name === "string" ? choice.name.trim() : "";
  return name.length > 0 ? name : String(choice?.id ?? "");
}

// ---- 引擎与模型选择控件 ----

function EngineControl(props: { agentId: () => string; engineId: () => string }) {
  const engines = () => catalog().agent_engines;
  const label = () => findEngineMeta(props.engineId())?.short_label ?? props.engineId();

  return (
    <If
      when={engines().length > 1}
      else={<EngineChip label={label()} engineId={props.engineId()} />}
    >
      <DropdownButton
        label={label()}
        leading={<EngineMark engineId={props.engineId()} size={12} />}
        tooltip={t("engine.tooltip")}
        quiet={true}
        small={true}
        minWidth={160}
      >
        <Each of={engines()} key={(eng: any) => eng.id}>
          {(eng: () => any) => (
            <SmeltMenuItem
              label={eng().label}
              icon={eng().icon}
              checked={eng().id === props.engineId()}
              onSelect={() => setAgentEngine(props.agentId(), eng().id)}
            />
          )}
        </Each>
      </DropdownButton>
    </If>
  );
}

function ModelControl(props: {
  agentId: () => string;
  modelProvider: () => string;
  modelId: () => string;
}) {
  const look = currentTheme;
  const inherit = () => !props.modelProvider().trim() || !props.modelId().trim();
  const label = () =>
    agentModelLabel(
      { model_provider: props.modelProvider(), model_id: props.modelId() },
      t("model.inherit"),
    );
  const groups = computed(() => {
    const list = piSnapshot().config?.model_choice_groups;
    return Array.isArray(list) ? list : [];
  });

  return (
    <DropdownButton
      label={label()}
      tooltip={t("model.tooltip")}
      quiet={true}
      small={true}
      minWidth={196}
    >
      <SmeltMenuItem
        label={t("model.inherit")}
        checked={inherit()}
        onSelect={() => setAgentModel(props.agentId(), "", "")}
      />
      <If when={groups().length > 0}>
        <SmeltMenuSeparator />
        <Each of={groups()} key={(g: any) => String(g.provider)}>
          {(group: () => any) => (
            <Submenu
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
                    {group().provider_name ?? group().provider}
                  </Text>
                  <SmeltIcon
                    path="icons/chevron-right.svg"
                    size={12}
                    color={look().palette.muted}
                  />
                </View>
              }
            >
              <Each
                of={Array.isArray(group().models) ? group().models : []}
                key={(m: any) => `${m.provider}:${m.id}`}
              >
                {(choice: () => any) => (
                  <SmeltMenuItem
                    label={modelChoiceShortLabel(choice())}
                    checked={
                      choice().provider === props.modelProvider() &&
                      choice().id === props.modelId()
                    }
                    onSelect={() =>
                      setAgentModel(props.agentId(), choice().provider, choice().id)
                    }
                  />
                )}
              </Each>
            </Submenu>
          )}
        </Each>
      </If>
    </DropdownButton>
  );
}

// ---- 绑定的上下文卡片 ----

function ContextCard(props: { ref: () => AgentRef | null; agent: () => any | null }) {
  const look = currentTheme;

  const workspaceText = computed(() => {
    locale();
    agentHostsSignal();
    const ref = props.ref();
    const agent = props.agent();
    if (!ref || !agent) return "";
    const root = hostSettings(ref.machine)?.agent_spaces_root;
    const space =
      typeof root === "string" && root.length > 0
        ? t("context.workspace_path", {
            path: pathJoin(pathStyle(ref.machine), root, agent.id),
          })
        : ref.machine == null
          ? t("context.workspace")
          : t("context.workspace_remote");
    return t("context.workspace_note", {
      workspace: space,
      dirs: WORKSPACE_SKILL_DIRS.join(" / "),
    });
  });

  const rows = computed(() => {
    const a = props.agent();
    if (!a) return [];
    const out: Array<{ key: string; value: string; isFolder: boolean; kindLabel: string }> = [];
    for (const folder of a.context_folders ?? []) {
      out.push({
        key: `folder:${folder}`,
        value: folder,
        isFolder: true,
        kindLabel: t("context.folder"),
      });
    }
    for (const link of a.context_links ?? []) {
      out.push({
        key: `link:${link}`,
        value: link,
        isFolder: false,
        kindLabel: t("context.link"),
      });
    }
    return out;
  });

  return (
    <SurfaceCard>
      <View
        style={{
          paddingLeft: 20,
          paddingRight: 20,
          paddingTop: 16,
          paddingBottom: 8,
          display: "flex",
          flexDirection: "row",
          alignItems: "flex-start",
          gap: 8,
        }}
      >
        <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 4 }}>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui_medium.size,
              lineHeight: look().type.ui_medium.line_height,
              fontWeight: look().type.ui_medium.weight,
            }}
          >
            {t("context.title")}
          </Text>
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
            }}
          >
            {t("context.description")}
          </Text>
        </View>
        <SmeltButton
          variant="quiet"
          size="sm"
          icon="icons/plus.svg"
          label={t("context.folder")}
          tooltip={t("context.folder_tooltip")}
          onPress={() => {
            const id = props.agent()?.id;
            if (id) void pickAgentContextFolders(id);
          }}
        />
      </View>

      <If
        when={rows().length > 0}
        else={
          <View style={{ paddingLeft: 20, paddingRight: 20, paddingTop: 12, paddingBottom: 12 }}>
            <Text
              style={{
                color: look().palette.faint,
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
              }}
            >
              {t("context.empty")}
            </Text>
          </View>
        }
      >
        <View
          style={{
            paddingLeft: 8,
            paddingRight: 8,
            paddingBottom: 4,
            display: "flex",
            flexDirection: "column",
          }}
        >
          <Each of={rows()} key={(r: any) => r.key}>
            {(row: () => { value: string; isFolder: boolean; kindLabel: string }) => (
              <View
                style={{
                  paddingLeft: 12,
                  paddingRight: 12,
                  paddingTop: 6,
                  paddingBottom: 6,
                  display: "flex",
                  flexDirection: "row",
                  alignItems: "center",
                  gap: 12,
                }}
              >
                <Text
                  style={{
                    flexShrink: 0,
                    color: look().palette.faint,
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                  }}
                >
                  {row().kindLabel}
                </Text>
                <Text
                  style={{
                    flex: 1,
                    minWidth: 0,
                    color: look().palette.text,
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                    numberOfLines: 1,
                  }}
                >
                  {row().value}
                </Text>
                <SmeltButton
                  variant="quiet"
                  size="sm"
                  icon="icons/trash.svg"
                  tooltip={
                    row().isFolder ? t("context.unbind_folder") : t("context.remove_link")
                  }
                  onPress={() => {
                    const id = props.agent()?.id;
                    if (!id) return;
                    if (row().isFolder) {
                      removeAgentContextFolder(id, row().value);
                    } else {
                      removeAgentContextLink(id, row().value);
                    }
                  }}
                />
              </View>
            )}
          </Each>
        </View>
      </If>

      {/* 内联链接输入框 + 添加按钮（空时禁用，回车也可添加） */}
      <View
        style={{
          paddingLeft: 20,
          paddingRight: 20,
          paddingTop: 4,
          paddingBottom: 8,
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          gap: 8,
        }}
      >
        <View style={{ flex: 1, minWidth: 0 }}>
          <SmeltInput
            value={contextLinkInput()}
            placeholder={t("context.link_placeholder")}
            accessibilityLabel={t("context.link_label")}
            onChange={({ value }) => contextLinkInput.set(value)}
            onSubmit={() => {
              const id = props.agent()?.id;
              if (id) addAgentContextLink(id);
            }}
          />
        </View>
        <SmeltButton
          size="sm"
          label={t("common.add")}
          disabled={contextLinkInput().trim().length === 0}
          onPress={() => {
            const id = props.agent()?.id;
            if (id) addAgentContextLink(id);
          }}
        />
      </View>

      <View style={{ paddingLeft: 20, paddingRight: 20, paddingBottom: 12 }}>
        <Text
          style={{
            color: look().palette.faint,
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
          }}
        >
          {workspaceText()}
        </Text>
      </View>
    </SurfaceCard>
  );
}

// ---- 配置页记忆摘要行 ----

function MemorySummaryCard(props: { agent: () => any | null }) {
  const look = currentTheme;
  const mem = () => memoriesState();
  const pending = () => memoryPendingCount(mem());
  const summary = () => {
    if (!props.agent()?.memory_enabled) return t("memory.off_summary");
    return memorySummaryText(mem().agent.entries.length, mem().user.entries.length, t);
  };

  return (
    <SurfaceCard
      style={{
        paddingLeft: 20,
        paddingRight: 20,
        paddingTop: 12,
        paddingBottom: 12,
        flexDirection: "row",
        alignItems: "center",
        gap: 12,
      }}
    >
      <Text
        style={{
          flexShrink: 0,
          color: look().palette.text,
          fontSize: look().type.ui_medium.size,
          lineHeight: look().type.ui_medium.line_height,
          fontWeight: look().type.ui_medium.weight,
        }}
      >
        {t("memory.title")}
      </Text>
      <ToggleSwitch
        checked={Boolean(props.agent()?.memory_enabled)}
        accessibilityLabel={t("memory.title")}
        tooltip={
          props.agent()?.memory_enabled
            ? t("memory.switch_off_tooltip")
            : t("memory.switch_on_tooltip")
        }
        onChange={(enabled) => {
          const id = props.agent()?.id;
          if (id) setAgentMemoryEnabled(id, enabled);
        }}
      />
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
        <Text
          style={{
            minWidth: 0,
            color: look().palette.muted,
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            numberOfLines: 1,
          }}
        >
          {summary()}
        </Text>
        <If when={pending() > 0}>
          <Text
            style={{
              flexShrink: 0,
              color: look().palette.bad,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
            }}
          >
            {t("memory.pending_count", { count: pending() })}
          </Text>
        </If>
      </View>
      <SmeltButton
        variant="quiet"
        size="sm"
        label={t("common.manage")}
        icon="icons/chevron-right.svg"
        tooltip={t("memory.manage_tooltip")}
        onPress={() => selectEditorTab({ kind: "memory" })}
      />
    </SurfaceCard>
  );
}

// ---- 能力（技能 / 扩展 / MCP）勾选卡片 ----

function CapabilitiesCard(props: { agent: () => any | null }) {
  const look = currentTheme;
  const selectedCount = () =>
    (props.agent()?.plugins?.length ?? 0) + (props.agent()?.mcp_servers?.length ?? 0);

  const allRows = computed(() => {
    locale();
    const a = props.agent();
    if (!a) return [];
    const snap = piSnapshot();
    return buildCapabilitiesList(a, snap.config, snap.config != null, t);
  });

  const filteredRows = computed(() => filterCapabilities(allRows(), capabilityQuery()));

  const pendingText = computed(() => {
    const snap = piSnapshot();
    if (snap.config != null) return null;
    return snap.error ?? t("pi.loading");
  });

  const hasCapabilitiesView = computed(() => {
    views();
    return capabilitiesViewRegistered();
  });

  return (
    <SurfaceCard>
      <View
        style={{
          paddingLeft: 20,
          paddingRight: 20,
          paddingTop: 16,
          paddingBottom: 8,
          display: "flex",
          flexDirection: "row",
          alignItems: "flex-start",
          gap: 8,
        }}
      >
        <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 4 }}>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui_medium.size,
              lineHeight: look().type.ui_medium.line_height,
              fontWeight: look().type.ui_medium.weight,
            }}
          >
            {t("capabilities.title")}
          </Text>
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
            }}
          >
            {t("capabilities.selected", { count: selectedCount() })}
          </Text>
        </View>
        <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 4 }}>
          <SmeltButton
            variant="quiet"
            size="sm"
            icon="icons/refresh.svg"
            tooltip={t("capabilities.rescan")}
            onPress={refreshAgentPiConfig}
          />
          <If when={hasCapabilitiesView()}>
            <SmeltButton
              variant="quiet"
              size="sm"
              label={t("common.manage")}
              icon="icons/chevron-right.svg"
              tooltip={t("capabilities.manage_tooltip")}
              onPress={() => openCapabilities("skills")}
            />
          </If>
        </View>
      </View>

      {/* 能力条目超过 8 项时出现筛选输入框 */}
      <If when={allRows().length > 8}>
        <View
          style={{
            paddingLeft: 20,
            paddingRight: 20,
            paddingBottom: 8,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 6,
          }}
        >
          <View style={{ flex: 1, minWidth: 0 }}>
            <SmeltInput
              value={capabilityQuery()}
              placeholder={t("capabilities.filter_placeholder")}
              accessibilityLabel={t("capabilities.filter_placeholder")}
              onChange={({ value }) => capabilityQuery.set(value)}
            />
          </View>
          <If when={capabilityQuery().length > 0}>
            <SmeltButton
              variant="dismiss"
              size="xs"
              icon="icons/close.svg"
              accessibilityLabel={t("common.cancel")}
              onPress={() => capabilityQuery.set("")}
            />
          </If>
        </View>
      </If>

      <If
        when={allRows().length === 0}
        else={
          <If
            when={filteredRows().length === 0}
            else={
              <View
                style={{
                  paddingLeft: 8,
                  paddingRight: 8,
                  paddingBottom: 8,
                  display: "flex",
                  flexDirection: "column",
                }}
              >
                <Each of={filteredRows()} key={(r: any) => r.key}>
                  {(row: () => any) => {
                    const usable = () => row().broken == null;
                    const subtitle = () =>
                      row().broken ?? (row().description || row().origin || "");
                    return (
                      <View
                        style={{
                          paddingLeft: 12,
                          paddingRight: 12,
                          paddingTop: 8,
                          paddingBottom: 8,
                          display: "flex",
                          flexDirection: "row",
                          alignItems: "center",
                          gap: 12,
                        }}
                      >
                        <ToggleSwitch
                          checked={Boolean(row().checked)}
                          disabled={!usable() && !row().checked}
                          accessibilityLabel={row().name}
                          onChange={() => {
                            const id = props.agent()?.id;
                            if (!id) return;
                            if (row().pick === "plugin") {
                              toggleAgentPlugin(id, row().id);
                            } else {
                              toggleAgentMcpServer(id, row().id);
                            }
                          }}
                        />
                        <View
                          style={{
                            flex: 1,
                            minWidth: 0,
                            display: "flex",
                            flexDirection: "column",
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
                                color: look().palette.text,
                                fontSize: look().type.text_xsmall.size,
                                lineHeight: look().type.text_xsmall.line_height,
                                numberOfLines: 1,
                              }}
                            >
                              {row().name}
                            </Text>
                            <QuietBadge label={row().kindLabel} onBg={true} />
                          </View>
                          <If when={Boolean(subtitle())}>
                            <Text
                              style={{
                                color: usable() ? look().palette.faint : look().palette.bad,
                                fontSize: look().type.text_xsmall.size,
                                lineHeight: look().type.text_xsmall.line_height,
                                numberOfLines: 1,
                              }}
                            >
                              {subtitle()}
                            </Text>
                          </If>
                        </View>
                      </View>
                    );
                  }}
                </Each>
              </View>
            }
          >
            <View style={{ paddingLeft: 20, paddingRight: 20, paddingTop: 12, paddingBottom: 16 }}>
              <Text
                style={{
                  color: look().palette.faint,
                  fontSize: look().type.text_xsmall.size,
                  lineHeight: look().type.text_xsmall.line_height,
                }}
              >
                {t("capabilities.no_match")}
              </Text>
            </View>
          </If>
        }
      >
        <View style={{ paddingLeft: 20, paddingRight: 20, paddingTop: 12, paddingBottom: 16 }}>
          <Text
            style={{
              color: look().palette.faint,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
            }}
          >
            {pendingText() ?? t("capabilities.empty")}
          </Text>
        </View>
      </If>
    </SurfaceCard>
  );
}

// ---- 删除行 ----

function DeleteAgentRow(props: { ref: () => AgentRef | null }) {
  const look = currentTheme;
  const autos = computed(() => {
    locale();
    const r = props.ref();
    if (!r) return [];
    return automationNamesFor(r.machine, r.id);
  });
  const blocked = () => autos().length > 0;
  const description = () => {
    const names = autos();
    if (names.length === 0) return t("delete.tooltip");
    const joined = names
      .map((n) => t("delete.automation_name", { name: n }))
      .join(t("delete.name_separator"));
    return t("delete.blocked", { count: names.length, names: joined });
  };

  return (
    <SurfaceCard
      style={{
        paddingLeft: 20,
        paddingRight: 20,
        paddingTop: 12,
        paddingBottom: 12,
        flexDirection: "row",
        alignItems: "center",
        gap: 12,
      }}
    >
      <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
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
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
          }}
        >
          {description()}
        </Text>
      </View>
      <SmeltButton
        variant="danger"
        size="sm"
        label={t("common.delete")}
        disabled={blocked()}
        onPress={() => {
          const r = props.ref();
          if (r) requestDeleteAgent(r);
        }}
      />
    </SurfaceCard>
  );
}

// ---- 记忆页签 ----

function MemorySection(props: {
  ref: () => AgentRef | null;
  layer: MemoryLayer;
  title: string;
  emptyHint: string;
  listing: () => { entries: any[]; core_over_budget: boolean };
  query: () => string;
  filter: () => MemoryFilter;
  narrowed: () => boolean;
}) {
  const look = currentTheme;
  const isLocal = () => props.ref()?.machine == null;
  const rows = computed(() =>
    visibleMemories(props.listing().entries, props.query(), props.filter()),
  );
  const headerTitle = () =>
    memorySectionTitle(
      props.title,
      props.listing().entries,
      props.narrowed() ? rows().length : null,
    );

  return (
    <View
      style={{
        paddingLeft: 8,
        paddingRight: 8,
        paddingBottom: 12,
        display: "flex",
        flexDirection: "column",
      }}
    >
      <View
        style={{
          paddingLeft: 12,
          paddingRight: 12,
          paddingTop: 4,
          paddingBottom: 4,
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          gap: 8,
        }}
      >
        <Text
          style={{
            flex: 1,
            minWidth: 0,
            color: look().palette.muted,
            fontSize: look().type.text_xsmall_medium.size,
            lineHeight: look().type.text_xsmall_medium.line_height,
            fontWeight: look().type.text_xsmall_medium.weight,
          }}
        >
          {headerTitle()}
        </Text>
        {/* 远程机器隐藏本机专用的「打开记忆文件夹」按钮 */}
        <If when={isLocal()}>
          <SmeltButton
            variant="quiet"
            size="sm"
            icon="icons/folder-open.svg"
            tooltip={t("memory.open_folder")}
            onPress={() => {
              const r = props.ref();
              if (r) openAgentMemoryFolder(r.id, props.layer);
            }}
          />
        </If>
      </View>

      <If when={props.listing().core_over_budget}>
        <View style={{ paddingLeft: 12, paddingRight: 12, paddingTop: 4, paddingBottom: 4 }}>
          <Text
            style={{
              color: look().palette.bad,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
            }}
          >
            {t("memory.over_budget", { kb: Math.floor(CORE_MEMORY_MAX_BYTES / 1000) })}
          </Text>
        </View>
      </If>

      <If
        when={rows().length > 0}
        else={
          <View style={{ paddingLeft: 12, paddingRight: 12, paddingTop: 6, paddingBottom: 6 }}>
            <Text
              style={{
                color: look().palette.faint,
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
              }}
            >
              {props.narrowed() && props.listing().entries.length > 0
                ? t("memory.no_match")
                : props.emptyHint}
            </Text>
          </View>
        }
      >
        <View style={{ display: "flex", flexDirection: "column" }}>
          <Each of={rows()} key={(e: any) => `${props.layer}:${e.file_name}`}>
            {(entry: () => any) => {
              const isExpanded = () => {
                const exp = expandedMemory();
                return Boolean(
                  exp && exp.layer === props.layer && exp.file_name === entry().file_name,
                );
              };
              const subtitle = () => {
                const reason = entry().pending_reason
                  ? pendingReasonText(entry().pending_reason, t)
                  : "";
                if (reason) {
                  return t("memory.pending_subtitle", {
                    reason,
                    description: entry().description ?? "",
                  });
                }
                return entry().description ? entry().description : entry().file_name;
              };
              const usage = () => memoryUsageInfo(entry());
              const bodyText = () => String(entry().body ?? "");

              return (
                <SmeltDisclosure
                  open={isExpanded()}
                  content={
                    <View style={{ paddingHorizontal: 12, paddingBottom: 8 }}>
                      <View
                        style={{
                          borderRadius: look().radius.md,
                          backgroundColor: look().palette.side,
                          paddingHorizontal: 12,
                          paddingVertical: 8,
                        }}
                      >
                        <Scroll style={{ maxHeight: 224 }}>
                          <Text
                            style={{
                              color: bodyText() ? look().palette.muted : look().palette.faint,
                              fontSize: look().type.text_xsmall.size,
                              lineHeight: look().type.text_xsmall.line_height,
                            }}
                          >
                            {bodyText() || t("memory.empty_body")}
                          </Text>
                        </Scroll>
                      </View>
                    </View>
                  }
                >
                  <View
                    style={{
                      paddingLeft: 12,
                      paddingRight: 12,
                      paddingTop: 6,
                      paddingBottom: 6,
                      display: "flex",
                      flexDirection: "row",
                      alignItems: "center",
                      gap: 12,
                    }}
                  >
                    <Text
                      style={{
                        width: 48,
                        flexShrink: 0,
                        color: look().palette.faint,
                        fontSize: look().type.text_xsmall.size,
                        lineHeight: look().type.text_xsmall.line_height,
                        numberOfLines: 1,
                      }}
                    >
                      {memoryKindLabel(entry().kind)}
                    </Text>

                    <SmeltRow
                      accessibilityLabel={entry().name}
                      onPress={() => toggleMemoryExpanded(props.layer, entry().file_name)}
                      class="flex-1 min-w-0"
                      style={{ paddingLeft: 4, paddingRight: 4, paddingTop: 2, paddingBottom: 2 }}
                    >
                      <View
                        style={{
                          flex: 1,
                          minWidth: 0,
                          display: "flex",
                          flexDirection: "column",
                          gap: 2,
                        }}
                      >
                        <View
                          style={{
                            display: "flex",
                            flexDirection: "row",
                            alignItems: "center",
                            gap: 4,
                          }}
                        >
                          <SmeltDisclosureChevron
                            open={isExpanded()}
                            size={12}
                            color={look().palette.faint}
                          />
                          <Text
                            style={{
                              flex: 1,
                              minWidth: 0,
                              color: look().palette.text,
                              fontSize: look().type.text_xsmall.size,
                              lineHeight: look().type.text_xsmall.line_height,
                              numberOfLines: 1,
                            }}
                          >
                            {entry().name}
                          </Text>
                        </View>
                        <Text
                          style={{
                            color: look().palette.faint,
                            fontSize: look().type.text_xsmall.size,
                            lineHeight: look().type.text_xsmall.line_height,
                            numberOfLines: 2,
                          }}
                        >
                          {subtitle()}
                        </Text>
                      </View>
                    </SmeltRow>

                    <If when={usage() != null}>
                      <Text
                        style={{
                          flexShrink: 0,
                          color: usage()?.alert ? look().palette.bad : look().palette.faint,
                          fontSize: look().type.text_xsmall.size,
                          lineHeight: look().type.text_xsmall.line_height,
                        }}
                      >
                        {usage()?.label ?? ""}
                      </Text>
                    </If>

                    {/* 远程机器隐藏本机专用的「用文本编辑器打开」按钮 */}
                    <If when={isLocal() && typeof entry().path === "string" && entry().path.length > 0}>
                      <SmeltButton
                        variant="quiet"
                        size="sm"
                        icon="icons/external-link.svg"
                        tooltip={t("memory.open_in_editor")}
                        onPress={() => openAgentMemoryFile(entry().path)}
                      />
                    </If>

                    <If when={entry().pending_reason != null}>
                      <SmeltButton
                        variant="quiet"
                        size="sm"
                        icon="icons/check.svg"
                        accessibilityLabel={t("memory.accept")}
                        tooltip={t("memory.accept_tooltip")}
                        onPress={() => {
                          const r = props.ref();
                          if (r) approveAgentMemory(r.id, props.layer, entry().file_name);
                        }}
                      />
                    </If>

                    <SmeltButton
                      variant="quiet"
                      size="sm"
                      icon="icons/close.svg"
                      tooltip={t("memory.delete_tooltip")}
                      onPress={() => {
                        const r = props.ref();
                        if (!r) return;
                        requestDeleteMemory({
                          agent_id: r.id,
                          layer: props.layer,
                          file_name: entry().file_name,
                          name: entry().name,
                        });
                      }}
                    />
                  </View>
                </SmeltDisclosure>
              );
            }}
          </Each>
        </View>
      </If>
    </View>
  );
}

function MemoryTabPanel(props: { ref: () => AgentRef | null; agent: () => any | null }) {
  const look = currentTheme;
  const query = () => memoryQuery();
  const filter = () => memoryFilter();
  const narrowed = () => query().trim().length > 0 || filter() !== "all";

  const filterOptions = computed<Array<{ value: MemoryFilter; label: string; tooltip: string }>>(() => {
    locale();
    return [
      { value: "all", label: t("memory.filter.all"), tooltip: t("memory.filter.all_tooltip") },
      {
        value: "pending",
        label: t("memory.filter.pending"),
        tooltip: t("memory.filter.pending_tooltip"),
      },
      { value: "rules", label: t("memory.filter.rules"), tooltip: t("memory.filter.rules_tooltip") },
      { value: "facts", label: t("memory.filter.facts"), tooltip: t("memory.filter.facts_tooltip") },
    ];
  });

  return (
    <SurfaceCard>
      <View
        style={{
          paddingLeft: 20,
          paddingRight: 20,
          paddingTop: 16,
          paddingBottom: 8,
          display: "flex",
          flexDirection: "row",
          alignItems: "flex-start",
          gap: 8,
        }}
      >
        <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 4 }}>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui_medium.size,
              lineHeight: look().type.ui_medium.line_height,
              fontWeight: look().type.ui_medium.weight,
            }}
          >
            {t("memory.title")}
          </Text>
          <Text
            style={{
              color: look().palette.muted,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
            }}
          >
            {props.agent()?.memory_enabled ? t("memory.description") : t("memory.description_off")}
          </Text>
        </View>
        <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
          <SmeltButton
            variant="quiet"
            size="sm"
            icon="icons/refresh.svg"
            tooltip={t("memory.reload")}
            onPress={refreshAgentMemories}
          />
          <ToggleSwitch
            checked={Boolean(props.agent()?.memory_enabled)}
            accessibilityLabel={t("memory.title")}
            tooltip={
              props.agent()?.memory_enabled
                ? t("memory.switch_off_tooltip")
                : t("memory.switch_on_tooltip")
            }
            onChange={(enabled) => {
              const id = props.agent()?.id;
              if (id) setAgentMemoryEnabled(id, enabled);
            }}
          />
        </View>
      </View>

      <View
        style={{
          paddingLeft: 20,
          paddingRight: 20,
          paddingBottom: 8,
          display: "flex",
          flexDirection: "row",
          flexWrap: "wrap",
          alignItems: "center",
          gap: 8,
        }}
      >
        <View
          style={{
            flex: 1,
            minWidth: 200,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 4,
          }}
        >
          <View style={{ flex: 1, minWidth: 0 }}>
            <SmeltInput
              value={query()}
              placeholder={t("memory.search_placeholder")}
              accessibilityLabel={t("memory.search_placeholder")}
              onChange={({ value }) => memoryQuery.set(value)}
            />
          </View>
          <If when={query().length > 0}>
            <SmeltButton
              variant="dismiss"
              size="xs"
              icon="icons/close.svg"
              accessibilityLabel={t("common.cancel")}
              onPress={() => memoryQuery.set("")}
            />
          </If>
        </View>
        <SmeltSegmented
          options={filterOptions()}
          value={filter()}
          onChange={(value) => memoryFilter.set(value)}
        />
      </View>

      <MemorySection
        ref={props.ref}
        layer="agent"
        title={t("memory.agent_title")}
        emptyHint={t("memory.agent_empty")}
        listing={() => memoriesState().agent}
        query={query}
        filter={filter}
        narrowed={narrowed}
      />
      <MemorySection
        ref={props.ref}
        layer="user"
        title={t("memory.user_title")}
        emptyHint={t("memory.user_empty")}
        listing={() => memoriesState().user}
        query={query}
        filter={filter}
        narrowed={narrowed}
      />
    </SurfaceCard>
  );
}

// ---- 编辑页主视图 ----

export function AgentEditorView() {
  const look = currentTheme;
  cleanup(() => {
    void flushEditorText();
  });

  const ref = computed(() => editingAgent());
  const agent = computed(() => {
    locale();
    agentHostsSignal();
    const r = ref();
    if (!r) return null;
    return getAgentDefinition(r.machine, r.id);
  });

  type TabKey = "config" | "automations" | "memory";
  const tabOptions = computed(() => {
    locale();
    views();
    const pending = memoryPendingCount(memoriesState());
    const list: Array<{ value: TabKey; label: string }> = [
      { value: "config", label: t("tab.config") },
    ];
    if (automationsViewRegistered()) {
      list.push({ value: "automations", label: t("tab.automations") });
    }
    list.push({
      value: "memory",
      label:
        pending > 0 ? t("tab.memory_pending", { count: pending }) : t("memory.title"),
    });
    return list;
  });

  const selectedTabKey = computed(() => editorTab().kind);

  const handleTabSelect = (key: TabKey) => {
    if (key === "config") {
      selectEditorTab({ kind: "config" });
    } else if (key === "automations") {
      selectEditorTab({ kind: "automations" });
    } else if (key === "memory") {
      selectEditorTab({ kind: "memory" });
    }
  };

  const errorBanner = computed(() => {
    if (pageError()) return pageError();
    const r = ref();
    return r ? hostUnavailable(r.machine) : null;
  });

  const usageText = computed(() => {
    locale();
    const r = ref();
    if (!r) return "";
    const convs = agentConversationCount(workspaceDirectory(r.machine), r.id);
    const autos = automationCountFor(r.machine, r.id);
    return agentUsageSummary(convs, autos, t);
  });

  return (
    <If when={ref() != null && agent() != null}>
      <View
        style={{
          flex: 1,
          width: "100%",
          height: "100%",
          minWidth: 0,
          minHeight: 0,
          display: "flex",
          flexDirection: "column",
        }}
      >
        <PageToolbar>
          <SmeltButton
            variant="quiet"
            icon="icons/chevron-left.svg"
            tooltip={editingConversationDraft() ? t("editor.cancel") : t("editor.back")}
            onPress={closeAgentSettings}
          />
          <AgentMonogram name={editorName()} size={20} onCard={false} />
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
            <Text
              style={{
                minWidth: 0,
                color: look().palette.text,
                fontSize: look().type.ui_medium.size,
                lineHeight: look().type.ui_medium.line_height,
                fontWeight: look().type.ui_medium.weight,
                numberOfLines: 1,
              }}
            >
              {agentDisplayName(editorName(), t)}
            </Text>
            <If when={editingConversationDraft()}>
              <Text
                style={{
                  minWidth: 0,
                  color: look().palette.muted,
                  fontSize: look().type.text_xsmall.size,
                  lineHeight: look().type.text_xsmall.line_height,
                  numberOfLines: 1,
                }}
              >
                {t("editor.draft_note")}
              </Text>
            </If>
          </View>

          <SmeltSegmented
            options={tabOptions()}
            value={selectedTabKey()}
            onChange={handleTabSelect}
          />

          <If
            when={editingConversationDraft()}
            else={
              <SmeltButton
                variant="primary"
                label={t("new_conversation.title")}
                tooltip={t("new_conversation.tooltip")}
                onPress={() => {
                  const r = ref();
                  if (r) startAgentConversation(r);
                }}
              />
            }
          >
            <SmeltButton
              variant="primary"
              label={t("editor.start")}
              onPress={() => {
                void commitConversationDraft();
              }}
            />
          </If>
        </PageToolbar>

        {/* 配置页签 */}
        <If when={editorTab().kind === "config"}>
          <Scroll style={{ flex: 1, width: "100%", minWidth: 0, minHeight: 0 }}>
            <View
              style={{
                width: "100%",
                maxWidth: 720,
                marginLeft: "auto",
                marginRight: "auto",
                paddingLeft: 32,
                paddingRight: 32,
                paddingTop: 20,
                paddingBottom: 24,
                display: "flex",
                flexDirection: "column",
                gap: 16,
              }}
            >
              <If when={errorBanner() != null}>
                <ErrorAlert message={errorBanner() ?? ""} />
              </If>

              {/* 身份卡片：名称框占满整行，不重复引擎图标；引擎图标仅在下方芯片/下拉中出现 */}
              <SurfaceCard
                style={{
                  paddingLeft: 20,
                  paddingRight: 20,
                  paddingTop: 16,
                  paddingBottom: 16,
                  gap: 10,
                }}
              >
                <SmeltInput
                  value={editorName()}
                  placeholder={t("editor.name_placeholder")}
                  accessibilityLabel={t("agent.name_label")}
                  onChange={({ value }: { value: string }) => setEditorNameInput(value)}
                  onBlur={() => {
                    void flushEditorText();
                  }}
                  onSubmit={() => {
                    void flushEditorText();
                  }}
                />
                <View
                  style={{
                    display: "flex",
                    flexDirection: "row",
                    flexWrap: "wrap",
                    alignItems: "center",
                    gap: 8,
                  }}
                >
                  <EngineControl
                    agentId={() => agent()?.id ?? ""}
                    engineId={() => agentEngineId(agent())}
                  />
                  <ModelControl
                    agentId={() => agent()?.id ?? ""}
                    modelProvider={() => agent()?.model_provider ?? ""}
                    modelId={() => agent()?.model_id ?? ""}
                  />
                  <Text
                    style={{
                      color: look().palette.faint,
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                    }}
                  >
                    {usageText()}
                  </Text>
                </View>
              </SurfaceCard>

              {/* 工作方式（系统提示词）卡片 */}
              <SurfaceCard>
                <View
                  style={{
                    paddingLeft: 20,
                    paddingRight: 20,
                    paddingTop: 16,
                    paddingBottom: 8,
                    display: "flex",
                    flexDirection: "column",
                    gap: 4,
                  }}
                >
                  <Text
                    style={{
                      color: look().palette.text,
                      fontSize: look().type.ui_medium.size,
                      lineHeight: look().type.ui_medium.line_height,
                      fontWeight: look().type.ui_medium.weight,
                    }}
                  >
                    {t("instructions.title")}
                  </Text>
                  <Text
                    style={{
                      color: look().palette.muted,
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                    }}
                  >
                    {t("instructions.description")}
                  </Text>
                </View>
                <View style={{ paddingLeft: 20, paddingRight: 20, paddingBottom: 12 }}>
                  <SmeltInput
                    multiline={true}
                    value={editorPrompt()}
                    placeholder={t("editor.instructions_placeholder")}
                    accessibilityLabel={t("instructions.title")}
                    onChange={({ value }: { value: string }) => setEditorPromptInput(value)}
                    onBlur={() => {
                      void flushEditorText();
                    }}
                    class="min-h-40 max-h-90"
                  />
                </View>
                <View style={{ paddingLeft: 20, paddingRight: 20, paddingTop: 2, paddingBottom: 12 }}>
                  <Text
                    style={{
                      color: look().palette.faint,
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                    }}
                  >
                    {agentInstructionsFooter(editorPrompt(), t)}
                  </Text>
                </View>
              </SurfaceCard>

              <ContextCard ref={() => ref()} agent={() => agent()} />
              <MemorySummaryCard agent={() => agent()} />
              <CapabilitiesCard agent={() => agent()} />
              <DeleteAgentRow ref={() => ref()} />
            </View>
          </Scroll>
        </If>

        {/* 自动化页签（直接读取已订阅的 automations.changed 投影，无跨 VM 插槽） */}
        <If when={editorTab().kind === "automations"}>
          <AgentAutomationsTab />
        </If>

        {/* 记忆页签 */}
        <If when={editorTab().kind === "memory"}>
          <Scroll style={{ flex: 1, width: "100%", minWidth: 0, minHeight: 0 }}>
            <View
              style={{
                width: "100%",
                maxWidth: 720,
                marginLeft: "auto",
                marginRight: "auto",
                paddingLeft: 32,
                paddingRight: 32,
                paddingTop: 20,
                paddingBottom: 24,
                display: "flex",
                flexDirection: "column",
                gap: 16,
              }}
            >
              <If when={errorBanner() != null}>
                <ErrorAlert message={errorBanner() ?? ""} />
              </If>
              <MemoryTabPanel ref={() => ref()} agent={() => agent()} />
            </View>
          </Scroll>
        </If>
      </View>
    </If>
  );
}
