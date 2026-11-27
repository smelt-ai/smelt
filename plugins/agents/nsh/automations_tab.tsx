// 智能体详情页的「自动化」页签：直接消费本插件已订阅的 `automations.changed` 投影，
// 零跨 VM 插槽；点选条目或「新建自动化」经 `smelt.app/open-view` 跳到 `smelt.automations/main`。

import { computed, Scroll, Text, View } from "nsh";
import { locale, SmeltButton, SmeltRow, t } from "smelt/sdk";

import {
  agentDisplayName,
  automationRunErrorText,
  automationRunStatusLabel,
  formatAutomationRelativeUntil,
  formatAutomationTriggerSummary,
  formatAutomationUnixLocal,
} from "../model.js";
import {
  agentAutomationNextRunAt,
  agentAutomationRunsFor,
  agentAutomationsFor,
  agentHostsSignal,
  automationsTabError,
  automationsVersion,
  clearAutomationsTabError,
  currentTheme,
  editingAgent,
  getAgentDefinition,
  openAutomationsView,
  setAgentAutomationEnabled,
} from "./state.ts";
import { ErrorAlert, SurfaceCard, ToggleSwitch } from "./ui.tsx";

function runStatusColor(status: string, palette: any): string {
  switch (status) {
    case "starting":
    case "queued":
    case "dispatching":
    case "running":
      return palette.muted;
    case "awaiting_approval":
    case "waiting_for_user":
      return palette.need;
    case "completed":
      return palette.ok;
    case "failed":
      return palette.bad;
    case "cancelled":
    case "skipped":
      return palette.faint;
    default:
      return palette.faint;
  }
}

function SectionHeader(props: { title: string; action?: any }) {
  const look = currentTheme;
  return (
    <View
      style={{
        paddingLeft: 4,
        paddingRight: 4,
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
          fontSize: look().type.ui_medium.size,
          lineHeight: look().type.ui_medium.line_height,
          fontWeight: look().type.ui_medium.weight,
          color: look().palette.muted,
          numberOfLines: 1,
        }}
      >
        {props.title}
      </Text>
      {props.action}
    </View>
  );
}

function RunLines(props: { title: string; detail: string; error?: string | null }) {
  const look = currentTheme;
  return (
    <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
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
      <If when={props.detail.length > 0}>
        <Text
          style={{
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            color: look().palette.muted,
            numberOfLines: 1,
          }}
        >
          {props.detail}
        </Text>
      </If>
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

export function AgentAutomationsTab() {
  const look = currentTheme;
  const target = computed(() => editingAgent());

  const data = computed(() => {
    automationsVersion();
    agentHostsSignal();
    locale();
    const tgt = target();
    if (!tgt) {
      return {
        displayName: "",
        mine: [] as Array<{ id: string; name: string; detail: string; enabled: boolean }>,
        runs: [] as Array<{
          id: string;
          automationId: string;
          name: string;
          detail: string;
          error: string | null;
          status: string;
        }>,
      };
    }

    const def = getAgentDefinition(tgt.machine, tgt.id);
    const displayName = def ? agentDisplayName(def.name, t) : "";
    const now = Math.floor(Date.now() / 1000);

    const mine = agentAutomationsFor(tgt.machine, tgt.id).map((auto: any) => {
      const name = auto.name?.trim() ? auto.name : t("automations.unnamed");
      let when = "";
      if (auto.enabled !== false) {
        const nextAt = agentAutomationNextRunAt(tgt.machine, auto.id);
        if (nextAt != null) {
          when = t("automations.next", { when: formatAutomationRelativeUntil(nextAt, now, t) });
        }
      } else {
        when = t("automations.paused");
      }
      const schedule = formatAutomationTriggerSummary(auto.trigger, t);
      const detail = [schedule, when].filter((part) => part.length > 0).join(" · ");
      return {
        id: String(auto.id),
        name,
        detail,
        enabled: auto.enabled !== false,
      };
    });

    const runs = agentAutomationRunsFor(tgt.machine, tgt.id).map((r: any) => {
      const name = r.context?.automation_name?.trim()
        ? r.context.automation_name
        : t("automations.unnamed");
      const at = r.finished_at ?? r.created_at;
      const error = r.status === "failed" ? automationRunErrorText(r, t) : null;
      return {
        id: String(r.id),
        automationId: String(r.automation_id),
        name,
        detail: `${automationRunStatusLabel(r.status, t)} · ${formatAutomationUnixLocal(at)}`,
        error,
        status: String(r.status),
      };
    });

    return { displayName, mine, runs };
  });

  return (
    <If when={target() != null}>
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
            paddingBottom: 20,
            display: "flex",
            flexDirection: "column",
            gap: 24,
          }}
        >
          <If when={automationsTabError() != null}>
            <SmeltRow
              accessibilityLabel={automationsTabError() ?? ""}
              onPress={clearAutomationsTabError}
              class="w-full"
            >
              <View style={{ flex: 1, minWidth: 0 }}>
                <ErrorAlert message={automationsTabError() ?? ""} />
              </View>
            </SmeltRow>
          </If>

          <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            <SectionHeader
              title={t("automations.title", { name: data().displayName })}
              action={
                <SmeltButton
                  testID="agent-tab-new-automation"
                  size="sm"
                  icon="icons/plus.svg"
                  label={t("automations.new")}
                  onPress={() => {
                    const tgt = target();
                    if (!tgt) return;
                    openAutomationsView({
                      machine: tgt.machine,
                      create_for_agent: tgt.id,
                    });
                  }}
                />
              }
            />

            <SurfaceCard style={{ overflow: "hidden" }}>
              <If
                when={data().mine.length > 0}
                else={
                  <View style={{ padding: 16 }}>
                    <Text
                      style={{
                        fontSize: look().type.text_xsmall.size,
                        lineHeight: look().type.text_xsmall.line_height,
                        color: look().palette.muted,
                      }}
                    >
                      {t("automations.empty")}
                    </Text>
                  </View>
                }
              >
                <Each of={data().mine} key={(item: any) => item.id}>
                  {(item: () => { id: string; name: string; detail: string; enabled: boolean }) => (
                    <View
                      testID={`agent-tab-automation-${item().id}`}
                      style={{
                        width: "100%",
                        paddingRight: 16,
                        display: "flex",
                        flexDirection: "row",
                        alignItems: "center",
                      }}
                    >
                      <SmeltRow
                        rounded={false}
                        accessibilityLabel={item().name}
                        onPress={() => {
                          const tgt = target();
                          if (!tgt) return;
                          openAutomationsView({
                            machine: tgt.machine,
                            automation: item().id,
                          });
                        }}
                        class="flex-1 min-w-0 self-stretch"
                        style={{ paddingLeft: 16, paddingRight: 12, paddingTop: 12, paddingBottom: 12 }}
                      >
                        <RunLines title={item().name} detail={item().detail} />
                      </SmeltRow>
                      <ToggleSwitch
                        checked={item().enabled}
                        accessibilityLabel={t("automations.enabled")}
                        onChange={(checked) => {
                          const tgt = target();
                          if (!tgt) return;
                          setAgentAutomationEnabled(tgt.machine, item().id, checked);
                        }}
                      />
                    </View>
                  )}
                </Each>
              </If>
            </SurfaceCard>
          </View>

          <If when={data().runs.length > 0}>
            <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              <SectionHeader title={t("automations.recent")} />
              <SurfaceCard style={{ overflow: "hidden" }}>
                <Each of={data().runs} key={(r: any) => r.id}>
                  {(r: () => {
                    id: string;
                    automationId: string;
                    name: string;
                    detail: string;
                    error: string | null;
                    status: string;
                  }) => (
                    <SmeltRow
                      testID={`agent-tab-run-${r().id}`}
                      rounded={false}
                      accessibilityLabel={r().name}
                      onPress={() => {
                        const tgt = target();
                        if (!tgt) return;
                        openAutomationsView({
                          machine: tgt.machine,
                          automation: r().automationId,
                          run: r().id,
                        });
                      }}
                      class="w-full gap-3"
                      style={{ paddingLeft: 16, paddingRight: 16, paddingTop: 12, paddingBottom: 12 }}
                    >
                      <View
                        class="rounded-full"
                        style={{
                          width: 7,
                          height: 7,
                          backgroundColor: runStatusColor(r().status, look().palette),
                          flexShrink: 0,
                        }}
                      />
                      <RunLines title={r().name} detail={r().detail} error={r().error} />
                    </SmeltRow>
                  )}
                </Each>
              </SurfaceCard>
            </View>
          </If>
        </View>
      </Scroll>
    </If>
  );
}
