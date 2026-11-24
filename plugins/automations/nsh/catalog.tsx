// 自动化目录：任务表、模板画廊、跨机器运行记录列表。
import { computed, Scroll, Text, View } from "nsh";
import { Menu } from "nsh/base";
import { popupMotion, SmeltButton, SmeltIcon, t } from "smelt/sdk";
import {
  actionAgentDefinitionId,
  activeRunForAutomation,
  automationRowStatus,
  automationRowStatusLabel,
  compactInstructions,
  compareRunsDesc,
  filteredTemplates,
  formatLastRun,
  formatNextRunCell,
  formatShellCommand,
  formatUnixLocal,
  isAutomationReady,
  isRunStatusActive,
  isShellAction,
  latestRunForAutomation,
  machineKey,
  runErrorText,
  runSourceLabel,
  runStatusLabel,
  runtimeElementKey,
  scheduleSummary,
  stateForAutomation,
  TEMPLATE_CATEGORIES,
  templateCategoryLabel,
  triggerSummary,
} from "../model.js";
import {
  addAutomation,
  addAutomationFromTemplate,
  agentsOn,
  agentsVersion,
  allMachineAutomationsEntries,
  automationMachineGroups,
  cancelAutomationRun,
  catalogTab,
  createEmptyMachineAutomations,
  creationMachines,
  currentTheme,
  machineAutomationsOn,
  machineName,
  openAutomationEditor,
  openCatalogRun,
  pageError,
  runAutomationOnceFromCatalog,
  setAutomationEnabled,
  startDeleteAutomation,
  storeVersion,
  templateFilter,
} from "./state.ts";
import {
  ContentCard,
  ErrorAlert,
  MenuDivider,
  MenuRow,
  MenuSectionHeader,
  NeutralIconMark,
  PageToolbar,
  POPUP_CLASS,
  RowItem,
  RunLines,
  SegmentedControl,
  StatusDot,
  runStatusColor,
} from "./ui.tsx";

const CATALOG_MAX_WIDTH = 960;
const COL_SCHEDULE = 168;
const COL_NEXT = 96;
const COL_STATUS = 140;
const COL_MENU = 32;

function MachinePickingButton(props: {
  label: string;
  icon?: string;
  quiet?: boolean;
  small?: boolean;
  onPick: (machine: string | null) => void;
  testID?: string;
}) {
  const look = currentTheme;
  const targets = computed(() => {
    storeVersion();
    return creationMachines().map((m) => ({
      key: machineKey(m),
      machine: m,
      name: machineName(m),
    }));
  });

  return (
    <If
      when={targets().length <= 1}
      else={
        <Menu
          placement="bottom"
          align="end"
          popupClass={POPUP_CLASS}
          {...popupMotion("bottom")}
          popupStyle={{ minWidth: 168 }}
          trigger={
            <View
              testID={props.testID}
              class="hover:bg-accent"
              style={{
                height: props.small ? 24 : 28,
                paddingLeft: props.small ? 10 : 12,
                paddingRight: props.small ? 8 : 10,
                borderRadius: props.small ? look().radius.md : look().radius.lg,
                backgroundColor: props.quiet
                  ? "transparent"
                  : (look().palette.button_secondary?.[0] ?? look().palette.soft),
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                gap: 6,
                flexShrink: 0,
                cursor: "default",
              }}
            >
              <If when={Boolean(props.icon)}>
                <SmeltIcon
                  path={props.icon!}
                  size={props.small ? 14 : 15}
                  color={props.quiet ? look().palette.muted : look().palette.text}
                />
              </If>
              <Text
                style={{
                  fontSize: props.small ? look().type.small.size : look().type.ui_medium.size,
                  lineHeight: props.small
                    ? look().type.small.line_height
                    : look().type.ui_medium.line_height,
                  fontWeight: props.quiet
                    ? look().type.small.weight
                    : look().type.ui_medium.weight,
                  color: props.quiet ? look().palette.muted : look().palette.text,
                  numberOfLines: 1,
                }}
              >
                {props.label}
              </Text>
              <SmeltIcon path="icons/chevron-down.svg" size={12} color={look().palette.muted} />
            </View>
          }
        >
          <MenuSectionHeader label={t("catalog.machine_menu")} />
          <Each of={targets()} key={(item: any) => item.key}>
            {(item: () => { key: string; machine: string | null; name: string }) => (
              <MenuRow label={item().name} onSelect={() => props.onPick(item().machine)} />
            )}
          </Each>
        </Menu>
      }
    >
      <SmeltButton
        testID={props.testID}
        quiet={props.quiet}
        small={props.small}
        icon={props.icon}
        label={props.label}
        onPress={() => props.onPick(targets()[0]?.machine ?? null)}
      />
    </If>
  );
}

export function CatalogHeader() {
  const tabs = computed(() => {
    return [
      { value: "tasks" as const, label: t("title") },
      { value: "runs" as const, label: t("catalog.runs") },
    ];
  });

  return (
    <PageToolbar>
      <SegmentedControl
        testID="automations-tabs"
        options={tabs()}
        selected={catalogTab()}
        onSelect={(tab) => catalogTab.set(tab)}
      />
      <MachinePickingButton
        testID="automations-add"
        icon="icons/plus.svg"
        label={t("catalog.new")}
        onPick={(machine) => addAutomation(machine)}
      />
    </PageToolbar>
  );
}

function AutomationTableHeader() {
  const look = currentTheme;
  const colStyle = () => ({
    fontSize: look().type.text_xsmall.size,
    lineHeight: look().type.text_xsmall.line_height,
    color: look().palette.faint,
    numberOfLines: 1,
  });
  return (
    <View
      style={{
        width: "100%",
        paddingLeft: 16,
        paddingRight: 16,
        paddingTop: 12,
        paddingBottom: 8,
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        gap: 12,
      }}
    >
      <Text style={{ flex: 1, minWidth: 0, ...colStyle() }}>{t("title")}</Text>
      <View style={{ width: COL_SCHEDULE, flexShrink: 0 }}>
        <Text style={colStyle()}>{t("catalog.col_schedule")}</Text>
      </View>
      <View style={{ width: COL_NEXT, flexShrink: 0 }}>
        <Text style={colStyle()}>{t("catalog.col_next_run")}</Text>
      </View>
      <View style={{ width: COL_STATUS, flexShrink: 0 }}>
        <Text style={colStyle()}>{t("catalog.col_status")}</Text>
      </View>
      <View style={{ width: COL_MENU, flexShrink: 0 }} />
    </View>
  );
}

function AutomationTableRow(props: { machine: string | null; automation: any }) {
  const look = currentTheme;

  const model = computed(() => {
    storeVersion();
    agentsVersion();
    const auto = props.automation;
    const proj = machineAutomationsOn(props.machine) ?? createEmptyMachineAutomations();
    const agents = agentsOn(props.machine);
    const agentDefId = actionAgentDefinitionId(auto.action);
    const agent = agentDefId ? agents.find((a) => a.id === agentDefId) ?? null : null;
    const activeRun = activeRunForAutomation(proj, auto.id);
    const isShell = isShellAction(auto.action);
    const actionReady = isShell ? true : agent != null;
    const canRun = isAutomationReady(auto) && actionReady && activeRun == null;
    const cancelRunId = activeRun && isRunStatusActive(activeRun.status) ? activeRun.id : null;
    const running = cancelRunId != null;
    const agentMissing = !isShell && agent == null;
    const rowStatus = automationRowStatus(
      auto.enabled !== false,
      proj.store_error != null,
      agentMissing,
      running,
    );
    const now = Math.floor(Date.now() / 1000);
    const nextRun = formatNextRunCell(
      stateForAutomation(proj, auto.id)?.next_run_at ?? null,
      auto.enabled !== false,
      now,
      t,
    );
    const latestRun = latestRunForAutomation(proj, auto.id);
    const lastRunText = latestRun
      ? formatLastRun(latestRun.status, latestRun.finished_at ?? latestRun.created_at, now, t)
      : null;
    const lastRunFailed = latestRun?.status === "failed";
    let actionSummary: string;
    if (isShell) {
      const cmdLine = compactInstructions(formatShellCommand(auto.action));
      actionSummary = cmdLine ? `Shell · ${cmdLine}` : "Shell";
    } else {
      actionSummary = agent?.name ? agent.name : t("row_status.agent_missing");
    }
    const displayName = auto.name?.trim() ? auto.name : t("automation.unnamed");
    const schedule = triggerSummary(auto.trigger, t);
    const dotColor =
      rowStatus === "running"
        ? look().palette.run
        : rowStatus === "agent_missing" || rowStatus === "unavailable"
          ? look().palette.bad
          : rowStatus === "paused"
            ? look().palette.faint
            : null;
    return {
      auto,
      enabled: auto.enabled !== false,
      canRun,
      cancelRunId,
      latestRunId: latestRun?.id ?? null,
      rowStatus,
      rowStatusLabel: automationRowStatusLabel(rowStatus, t),
      dotColor,
      nextRun,
      lastRunText,
      lastRunFailed,
      actionSummary,
      displayName,
      schedule,
    };
  });

  const target = () => ({ machine: props.machine, id: props.automation.id });
  const rowKey = () => `${runtimeElementKey(props.machine)}-${props.automation.id}`;

  return (
    <View
      testID={`automation-row-${rowKey()}`}
      class="hover:bg-accent/50"
      style={{
        width: "100%",
        paddingLeft: 16,
        paddingRight: 16,
        paddingTop: 10,
        paddingBottom: 10,
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        gap: 12,
      }}
    >
      {/* 左侧主区域整块点击进入编辑器，与右侧 ⋯ 菜单分离，避免嵌套按钮误触发 */}
      <View
        onPress={() => openAutomationEditor(target())}
        style={{
          flex: 1,
          minWidth: 0,
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          gap: 12,
          cursor: "default",
        }}
      >
        <View
          style={{
            flex: 1,
            minWidth: 0,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 12,
          }}
        >
          <NeutralIconMark icon="icons/workflow.svg" size={28} />
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
              {model().displayName}
            </Text>
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: look().palette.faint,
                numberOfLines: 1,
              }}
            >
              {model().actionSummary}
            </Text>
          </View>
        </View>

        <View style={{ width: COL_SCHEDULE, flexShrink: 0 }}>
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: look().palette.muted,
              numberOfLines: 1,
            }}
          >
            {model().schedule}
          </Text>
        </View>

        <View style={{ width: COL_NEXT, flexShrink: 0 }}>
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: look().palette.muted,
              numberOfLines: 1,
            }}
          >
            {model().nextRun}
          </Text>
        </View>

        <View
          style={{
            width: COL_STATUS,
            flexShrink: 0,
            display: "flex",
            flexDirection: "column",
            gap: 2,
          }}
        >
          <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 6 }}>
            <If when={model().dotColor != null}>
              <StatusDot size={6} color={model().dotColor ?? look().palette.muted} />
            </If>
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: look().palette.muted,
                numberOfLines: 1,
              }}
            >
              {model().rowStatusLabel}
            </Text>
          </View>
          <If when={model().lastRunText != null}>
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: model().lastRunFailed ? look().palette.bad : look().palette.faint,
                numberOfLines: 1,
              }}
            >
              {model().lastRunText ?? ""}
            </Text>
          </If>
        </View>
      </View>

      <View style={{ width: COL_MENU, flexShrink: 0, display: "flex", alignItems: "flex-end" }}>
        <Menu
          placement="bottom"
          align="end"
          popupClass={POPUP_CLASS}
          {...popupMotion("bottom")}
          popupStyle={{ minWidth: 168 }}
          trigger={
            <View
              testID={`automation-row-more-${rowKey()}`}
              accessibilityLabel={t("catalog.more")}
              class="hover:bg-accent"
              style={{
                width: 24,
                height: 24,
                borderRadius: look().radius.sm,
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                cursor: "default",
              }}
            >
              <SmeltIcon path="icons/ellipsis.svg" size={15} color={look().palette.muted} />
            </View>
          }
        >
          <If
            when={model().cancelRunId != null}
            else={
              <MenuRow
                label={t("menu.run_once")}
                icon="icons/play.svg"
                disabled={!model().canRun}
                onSelect={() => runAutomationOnceFromCatalog(target())}
              />
            }
          >
            <MenuRow
              label={t("menu.stop")}
              icon="icons/close.svg"
              danger={true}
              onSelect={() => {
                const rid = model().cancelRunId;
                if (rid) cancelAutomationRun(props.machine, rid);
              }}
            />
          </If>
          <If when={model().latestRunId != null}>
            <MenuRow
              label={t("menu.view_runs")}
              icon="icons/eye.svg"
              onSelect={() => {
                const rid = model().latestRunId;
                if (rid) openCatalogRun(props.machine, rid);
              }}
            />
          </If>
          <MenuRow
            label={model().enabled ? t("menu.pause") : t("menu.enable")}
            icon={model().enabled ? "icons/pause.svg" : "icons/circle-check.svg"}
            onSelect={() => setAutomationEnabled(target(), !model().enabled)}
          />
          <MenuDivider />
          <MenuRow
            label={t("menu.delete")}
            icon="icons/trash.svg"
            danger={true}
            onSelect={() => startDeleteAutomation(target())}
          />
        </Menu>
      </View>
    </View>
  );
}

function TemplateCard(props: { template: any }) {
  const look = currentTheme;
  const tpl = () => props.template;
  return (
    <ContentCard
      testID={`automation-template-${tpl().id}`}
      style={{
        flex: 1,
        minWidth: 0,
        padding: 16,
        minHeight: 168,
        gap: 12,
        justifyContent: "space-between",
      }}
    >
      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "flex-start",
          justifyContent: "space-between",
          gap: 8,
        }}
      >
        <NeutralIconMark icon={tpl().icon} size={32} />
        <MachinePickingButton
          testID={`automation-template-add-${tpl().id}`}
          quiet={true}
          small={true}
          label={t("template.add")}
          onPick={(machine) => addAutomationFromTemplate(machine, tpl().id)}
        />
      </View>

      <View style={{ flex: 1, display: "flex", flexDirection: "column", gap: 4 }}>
        <Text
          style={{
            fontSize: look().type.ui_medium.size,
            lineHeight: look().type.ui_medium.line_height,
            fontWeight: look().type.ui_medium.weight,
            color: look().palette.text,
          }}
        >
          {t(tpl().nameKey)}
        </Text>
        <Text
          style={{
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            color: look().palette.muted,
          }}
        >
          {t(tpl().descriptionKey)}
        </Text>
      </View>

      <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 4 }}>
        <SmeltIcon path="icons/calendar.svg" size={12} color={look().palette.faint} />
        <Text
          style={{
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            color: look().palette.faint,
          }}
        >
          {scheduleSummary(tpl().schedule, t)}
        </Text>
      </View>
    </ContentCard>
  );
}

function TemplatesSection() {
  const look = currentTheme;
  const filterOptions = computed(() => {
    return [
      { value: "__all__", label: t("template.all") },
      ...TEMPLATE_CATEGORIES.map((cat) => ({
        value: cat,
        label: templateCategoryLabel(cat, t),
      })),
    ];
  });

  const rows = computed(() => {
    const items = filteredTemplates(templateFilter());
    const chunked: Array<{ key: string; items: any[]; padCount: number }> = [];
    for (let i = 0; i < items.length; i += 3) {
      const slice = items.slice(i, i + 3);
      chunked.push({
        key: slice.map((s) => s.id).join(","),
        items: slice,
        padCount: 3 - slice.length,
      });
    }
    return chunked;
  });

  return (
    <View style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 12,
        }}
      >
        <Text
          style={{
            fontSize: look().type.ui_medium.size,
            lineHeight: look().type.ui_medium.line_height,
            fontWeight: look().type.ui_medium.weight,
            color: look().palette.text,
          }}
        >
          {t("catalog.templates")}
        </Text>
        <SegmentedControl
          testID="automation-template-filters"
          options={filterOptions()}
          selected={templateFilter() ?? "__all__"}
          onSelect={(val) => templateFilter.set(val === "__all__" ? null : val)}
        />
      </View>

      <View style={{ display: "flex", flexDirection: "column", gap: 12 }}>
        <Each of={rows()} key={(r: any) => r.key}>
          {(row: () => { key: string; items: any[]; padCount: number }) => (
            <View
              style={{
                width: "100%",
                display: "flex",
                flexDirection: "row",
                gap: 12,
              }}
            >
              <Each of={row().items} key={(tpl: any) => tpl.id}>
                {(tpl: () => any) => <TemplateCard template={tpl()} />}
              </Each>
              <If when={row().padCount >= 1}>
                <View style={{ flex: 1, minWidth: 0 }} />
              </If>
              <If when={row().padCount >= 2}>
                <View style={{ flex: 1, minWidth: 0 }} />
              </If>
            </View>
          )}
        </Each>
      </View>
    </View>
  );
}

function TasksCatalogBody() {
  const look = currentTheme;
  const groups = computed(() => {
    storeVersion();
    return automationMachineGroups();
  });
  const grouped = computed(() => groups().length > 1);
  const allEmpty = computed(() => groups().every((g) => g.automations.length === 0));
  const bannerError = computed(() => {
    storeVersion();
    return pageError() ?? machineAutomationsOn(null)?.store_error ?? null;
  });

  return (
    <View
      style={{
        width: "100%",
        maxWidth: CATALOG_MAX_WIDTH,
        marginLeft: "auto",
        marginRight: "auto",
        paddingLeft: 24,
        paddingRight: 24,
        paddingTop: 20,
        paddingBottom: 32,
        display: "flex",
        flexDirection: "column",
        gap: 24,
      }}
    >
      <If when={bannerError() != null}>
        <ErrorAlert
          message={bannerError() ?? ""}
          onDismiss={pageError() != null ? () => pageError.set(null) : undefined}
        />
      </If>

      <Each of={groups()} key={(g: any) => machineKey(g.machine)}>
        {(group: () => { machine: string | null; automations: any[]; status: string | null }) => (
          <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            <If when={grouped()}>
              <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
                <Text
                  style={{
                    fontSize: look().type.text_xsmall_medium.size,
                    lineHeight: look().type.text_xsmall_medium.line_height,
                    fontWeight: look().type.text_xsmall_medium.weight,
                    color: look().palette.muted,
                  }}
                >
                  {machineName(group().machine)}
                </Text>
                <If when={group().status != null}>
                  <Text
                    style={{
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                      color: look().palette.faint,
                    }}
                  >
                    {group().status ?? ""}
                  </Text>
                </If>
                <If when={group().status == null && group().automations.length === 0}>
                  <Text
                    style={{
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                      color: look().palette.faint,
                    }}
                  >
                    {t("catalog.empty")}
                  </Text>
                </If>
              </View>
            </If>

            <If when={group().automations.length > 0}>
              <ContentCard style={{ overflow: "hidden" }}>
                <AutomationTableHeader />
                <Each of={group().automations} key={(a: any) => a.id}>
                  {(auto: () => any) => (
                    <AutomationTableRow machine={group().machine} automation={auto()} />
                  )}
                </Each>
              </ContentCard>
            </If>
          </View>
        )}
      </Each>

      <If when={allEmpty()}>
        <Text
          style={{
            fontSize: look().type.ui.size,
            lineHeight: look().type.ui.line_height,
            color: look().palette.muted,
          }}
        >
          {t("catalog.empty_hint")}
        </Text>
      </If>

      <TemplatesSection />
    </View>
  );
}

function RunsCatalogBody() {
  const look = currentTheme;
  const bannerError = computed(() => {
    storeVersion();
    return pageError() ?? machineAutomationsOn(null)?.store_error ?? null;
  });

  const allRuns = computed(() => {
    storeVersion();
    const entries = allMachineAutomationsEntries();
    const showMachine = entries.length > 1;
    const flat: Array<{ key: string; machine: string | null; run: any; showMachine: boolean }> = [];
    for (const [machine, proj] of entries) {
      for (const r of proj.runs ?? []) {
        flat.push({
          key: `${runtimeElementKey(machine)}-${r.id}`,
          machine,
          run: r,
          showMachine,
        });
      }
    }
    flat.sort((a, b) => compareRunsDesc(a.run, b.run));
    return flat;
  });

  return (
    <View
      style={{
        width: "100%",
        maxWidth: CATALOG_MAX_WIDTH,
        marginLeft: "auto",
        marginRight: "auto",
        paddingLeft: 24,
        paddingRight: 24,
        paddingTop: 20,
        paddingBottom: 32,
        display: "flex",
        flexDirection: "column",
        gap: 12,
      }}
    >
      <If when={bannerError() != null}>
        <ErrorAlert
          message={bannerError() ?? ""}
          onDismiss={pageError() != null ? () => pageError.set(null) : undefined}
        />
      </If>

      <If
        when={allRuns().length > 0}
        else={
          <View
            style={{
              paddingTop: 64,
              paddingBottom: 64,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
            }}
          >
            <Text
              style={{
                fontSize: look().type.ui.size,
                lineHeight: look().type.ui.line_height,
                color: look().palette.muted,
              }}
            >
              {t("catalog.no_runs")}
            </Text>
          </View>
        }
      >
        <ContentCard style={{ overflow: "hidden" }}>
          <Each of={allRuns()} key={(item: any) => item.key}>
            {(item: () => { key: string; machine: string | null; run: any; showMachine: boolean }) => {
              const info = () => {
                const r = item().run;
                const at = r.finished_at ?? r.created_at;
                const name = r.context?.automation_name?.trim()
                  ? r.context.automation_name
                  : t("automation.unnamed");
                const machinePrefix = item().showMachine
                  ? `${machineName(item().machine)} · `
                  : "";
                const detail = `${machinePrefix}${runSourceLabel(r.source, t)} · ${runStatusLabel(r.status, t)} · ${formatUnixLocal(at)}`;
                const err =
                  r.status === "failed"
                    ? (() => {
                        const raw = runErrorText(r, t);
                        return raw ? compactInstructions(raw) : null;
                      })()
                    : null;
                return { name, detail, err, status: r.status };
              };
              return (
                <RowItem
                  testID={`automation-catalog-run-${item().key}`}
                  onPress={() => openCatalogRun(item().machine, item().run.id)}
                  style={{
                    paddingLeft: 16,
                    paddingRight: 16,
                    paddingTop: 12,
                    paddingBottom: 12,
                    gap: 12,
                  }}
                >
                  <StatusDot size={7} color={runStatusColor(info().status, look().palette)} />
                  <RunLines title={info().name} detail={info().detail} error={info().err} />
                </RowItem>
              );
            }}
          </Each>
        </ContentCard>
      </If>
    </View>
  );
}

export function CatalogView() {
  return (
    <View style={{ flex: 1, minWidth: 0, minHeight: 0, display: "flex", flexDirection: "column" }}>
      <CatalogHeader />
      <Scroll style={{ flex: 1, minHeight: 0, width: "100%" }}>
        <If when={catalogTab() === "tasks"} else={<RunsCatalogBody />}>
          <TasksCatalogBody />
        </If>
      </Scroll>
    </View>
  );
}
