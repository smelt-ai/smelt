// 智能体目录页：页头工具行、默认智能体选择器、新建菜单、选默认提示条、按机器分组列表与空状态。

import { computed, Scroll, Text, View } from "nsh";
import { ContextMenu } from "nsh/base";
import { CONTEXT_MENU_MOTION, locale, SmeltButton, SmeltIcon, t } from "smelt/sdk";

import {
  agentCardPrompt,
  agentDisplayName,
  agentEngineId,
  agentRefKey,
  isAgentConversationReady,
  machineKey,
} from "../model.js";
import {
  agentEngineIds,
  agentHostsSignal,
  AgentRef,
  agentsOnMachine,
  armConversationOnCreate,
  attachedMachines,
  automationCountFor,
  catalog,
  chooseDefaultConversationAgent,
  createAgent,
  creationMachines,
  currentTheme,
  findEngineMeta,
  getNewConversationPlan,
  hostSettings,
  hostUnavailable,
  isDefaultAgent,
  machineDisplayName,
  machineUnavailableReason,
  openAgent,
  pageError,
  pickingForConversation,
  requestDeleteAgent,
  startAgentConversation,
} from "./state.ts";
import {
  AgentMonogram,
  DropdownButton,
  EngineChip,
  EngineMark,
  ErrorAlert,
  MenuDivider,
  MenuEntry,
  MenuSectionTitle,
  PageToolbar,
  POPUP_CLASS,
  QuietBadge,
  SurfaceCard,
} from "./ui.tsx";

interface MachineGroup {
  key: string;
  machine: string | null;
  name: string;
  agents: any[];
  status: string | null;
}

interface FlatMenuEntry {
  key: string;
  kind: "separator" | "section" | "item";
  label?: string;
  iconPath?: string | null;
  checked?: boolean;
  onSelect?: () => void;
}

function buildCreateMenuEntries(): FlatMenuEntry[] {
  const ms = creationMachines();
  const engines = catalog().agent_engines;
  const grouped = ms.length > 1;
  const items: FlatMenuEntry[] = [];
  ms.forEach((machine, ix) => {
    const mKey = machineKey(machine);
    if (grouped) {
      if (ix > 0) items.push({ key: `sep:${mKey}`, kind: "separator" });
      items.push({ key: `sec:${mKey}`, kind: "section", label: machineDisplayName(machine) });
    }
    for (const eng of engines) {
      items.push({
        key: `add:${mKey}:${eng.id}`,
        kind: "item",
        label: eng.label,
        iconPath: eng.icon ?? "icons/bot.svg",
        onSelect: () => {
          void createAgent(machine, eng.id);
        },
      });
    }
  });
  return items;
}

function CreateAgentButton(props: { small?: boolean }) {
  const engines = () => catalog().agent_engines;
  const ms = computed(() => {
    locale();
    agentHostsSignal();
    return creationMachines();
  });
  const direct = computed(() => ms().length === 1 && engines().length === 1);
  const menuEntries = computed(() => {
    locale();
    agentHostsSignal();
    return buildCreateMenuEntries();
  });

  return (
    <If
      when={direct()}
      else={
        <DropdownButton
          label={t("agent.new")}
          iconPath="icons/plus.svg"
          small={props.small}
          align="end"
          minWidth={184}
        >
          <Each of={menuEntries()} key={(e: FlatMenuEntry) => e.key}>
            {(entry: () => FlatMenuEntry) =>
              entry().kind === "separator" ? (
                <MenuDivider />
              ) : entry().kind === "section" ? (
                <MenuSectionTitle text={entry().label ?? ""} />
              ) : (
                <MenuEntry
                  label={entry().label ?? ""}
                  iconPath={entry().iconPath}
                  onSelect={() => entry().onSelect?.()}
                />
              )
            }
          </Each>
        </DropdownButton>
      }
    >
      <SmeltButton
        small={props.small}
        icon="icons/plus.svg"
        label={t("agent.new")}
        onPress={() => {
          const m = ms()[0] ?? null;
          const eng = engines()[0]?.id ?? "pi";
          void createAgent(m, eng);
        }}
      />
    </If>
  );
}

function DefaultAgentPicker() {
  const plan = computed(() => {
    locale();
    agentHostsSignal();
    return getNewConversationPlan();
  });

  const label = computed(() => {
    const p = plan();
    if (p.intent.kind === "start") {
      return t("default.current", { name: agentDisplayName(p.default_name ?? "", t) });
    }
    return t("default.pick");
  });

  const menuEntries = computed<FlatMenuEntry[]>(() => {
    locale();
    const p = plan();
    const startOnPick = p.intent.kind !== "start" && pickingForConversation();
    const currentKey =
      p.intent.kind === "start"
        ? agentRefKey(p.intent.agent.machine, p.intent.agent.id)
        : null;
    const grouped = p.ready.some((pick: any) => pick.agent.machine != null);
    const items: FlatMenuEntry[] = [];
    let lastMachineKey: string | null = null;
    for (const pick of p.ready) {
      const mKey = machineKey(pick.agent.machine);
      if (grouped && lastMachineKey !== mKey) {
        if (lastMachineKey !== null) {
          items.push({ key: `sep:${mKey}`, kind: "separator" });
        }
        items.push({
          key: `sec:${mKey}`,
          kind: "section",
          label: machineDisplayName(pick.agent.machine),
        });
        lastMachineKey = mKey;
      }
      const refKey = agentRefKey(pick.agent.machine, pick.agent.id);
      items.push({
        key: `pick:${refKey}`,
        kind: "item",
        label: agentDisplayName(pick.name, t),
        checked: !startOnPick && currentKey === refKey,
        onSelect: () => {
          chooseDefaultConversationAgent(
            { machine: pick.agent.machine ?? null, id: pick.agent.id },
            startOnPick,
          );
        },
      });
    }
    return items;
  });

  return (
    <If when={plan().ready.length > 0}>
      <DropdownButton
        label={label()}
        tooltip={t("default.tooltip")}
        quiet={true}
        align="end"
        minWidth={196}
      >
        <Each of={menuEntries()} key={(e: FlatMenuEntry) => e.key}>
          {(entry: () => FlatMenuEntry) =>
            entry().kind === "separator" ? (
              <MenuDivider />
            ) : entry().kind === "section" ? (
              <MenuSectionTitle text={entry().label ?? ""} />
            ) : (
              <MenuEntry
                label={entry().label ?? ""}
                checked={entry().checked}
                onSelect={() => entry().onSelect?.()}
              />
            )
          }
        </Each>
      </DropdownButton>
    </If>
  );
}

function AgentCatalogRow(props: { machine: string | null; agent: () => any }) {
  const look = currentTheme;
  const engines = () => catalog().agent_engines;
  const showEngine = () => engines().length > 1;
  const ref = (): AgentRef => ({ machine: props.machine, id: props.agent().id });
  const name = () => agentDisplayName(props.agent().name, t);
  const engId = () => agentEngineId(props.agent());
  const engLabel = () => findEngineMeta(engId())?.short_label ?? engId();
  const summary = () => agentCardPrompt(props.agent().prompt);
  const isDef = () => isDefaultAgent(props.machine, props.agent().id);
  const ready = () => isAgentConversationReady(props.agent(), agentEngineIds());
  const autoCount = () => automationCountFor(props.machine, props.agent().id);

  const handleRowPress = () => {
    if (pickingForConversation() && ready()) {
      chooseDefaultConversationAgent(ref(), true);
      return;
    }
    openAgent(ref(), { kind: "config" });
  };

  return (
    <ContextMenu
      popupClass={POPUP_CLASS}
      {...CONTEXT_MENU_MOTION}
      popupStyle={{ minWidth: 184 }}
      trigger={
        <View
          role="button"
          onPress={handleRowPress}
          style={{
            width: "100%",
            paddingLeft: 16,
            paddingRight: 16,
            paddingTop: 12,
            paddingBottom: 12,
            borderRadius: 12,
            borderWidth: 1,
            borderColor: look().palette.hair,
            backgroundColor: look().palette.surface,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 12,
            cursor: "default",
          }}
          hoverStyle={{
            backgroundColor: look().palette.soft,
          }}
        >
          <AgentMonogram name={name()} size={36} onCard={true} />
          <View
            style={{
              flex: 1,
              minWidth: 0,
              display: "flex",
              flexDirection: "column",
              gap: 4,
            }}
          >
            <View
              style={{
                width: "100%",
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
                  color: look().palette.text,
                  fontSize: look().type.ui_medium.size,
                  lineHeight: look().type.ui_medium.line_height,
                  fontWeight: look().type.ui_medium.weight,
                  numberOfLines: 1,
                }}
              >
                {name()}
              </Text>
              <If when={isDef()}>
                <QuietBadge label={t("default.badge")} onBg={true} />
              </If>
              <If when={showEngine()}>
                <EngineChip label={engLabel()} engineId={engId()} />
              </If>
            </View>
            <If when={summary() != null}>
              <Text
                style={{
                  color: look().palette.muted,
                  fontSize: look().type.text_xsmall.size,
                  lineHeight: 18,
                  numberOfLines: 2,
                }}
              >
                {summary() ?? ""}
              </Text>
            </If>
          </View>
        </View>
      }
    >

      <MenuEntry
        label={t("new_conversation.title")}
        onSelect={() => startAgentConversation(ref())}
      />
      <MenuEntry
        label={t("default.set")}
        checked={isDef()}
        disabled={isDef() || !ready()}
        onSelect={() => chooseDefaultConversationAgent(ref(), false)}
      />
      <MenuDivider />
      <MenuEntry
        label={t("delete.title")}
        iconPath="icons/trash.svg"
        danger={true}
        disabled={autoCount() !== 0}
        onSelect={() => requestDeleteAgent(ref())}
      />
    </ContextMenu>
  );
}

export function AgentCatalogView() {
  const look = currentTheme;

  const groups = computed<MachineGroup[]>(() => {
    locale();
    agentHostsSignal();
    const attached = attachedMachines();
    const keys: Array<string | null> =
      attached.length > 0
        ? attached.map((m: any) =>
            typeof m.machine === "string" && m.machine.length > 0 ? m.machine : null,
          )
        : [null];
    return keys.map((machine) => {
      const loaded = hostSettings(machine) != null;
      const status =
        machineUnavailableReason(machine) ??
        hostUnavailable(machine) ??
        (!loaded ? t("common.loading") : null);
      return {
        key: machineKey(machine) || "local",
        machine,
        name: machineDisplayName(machine),
        agents: agentsOnMachine(machine),
        status,
      };
    });
  });

  const totalCount = computed(() =>
    groups().reduce((sum, group) => sum + group.agents.length, 0),
  );

  const countLabel = computed(() => {
    const n = totalCount();
    return n === 1 ? t("page.count.one") : t("page.count.other", { count: n });
  });

  const grouped = computed(() => groups().length > 1);

  const topError = computed(() => {
    if (pageError()) return pageError();
    if (!grouped()) {
      const first = groups()[0];
      if (first) return hostUnavailable(first.machine);
    }
    return null;
  });

  const isEmptySingleMachine = computed(() => totalCount() === 0 && !grouped());

  const defaultEngineId = () => catalog().agent_engines[0]?.id ?? "pi";

  return (
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
      <PageToolbar style={{ justifyContent: "space-between" }}>
        <Text
          style={{
            color: look().palette.muted,
            fontSize: look().type.ui.size,
            lineHeight: look().type.ui.line_height,
          }}
        >
          {countLabel()}
        </Text>
        <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
          <DefaultAgentPicker />
          <CreateAgentButton />
        </View>
      </PageToolbar>

      <If
        when={isEmptySingleMachine()}
        else={
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
                gap: 12,
              }}
            >
              <If when={topError() != null}>
                <ErrorAlert message={topError() ?? ""} />
              </If>

              <If when={pickingForConversation() && totalCount() > 0}>
                <View
                  style={{
                    paddingLeft: 14,
                    paddingRight: 14,
                    paddingTop: 10,
                    paddingBottom: 10,
                    borderRadius: look().radius.md,
                    borderWidth: 1,
                    borderColor: look().palette.hair_strong,
                    backgroundColor: look().palette.soft,
                    display: "flex",
                    flexDirection: "row",
                    alignItems: "center",
                    gap: 8,
                  }}
                >
                  <SmeltIcon path="icons/bot.svg" size={14} color={look().palette.text} />
                  <Text
                    style={{
                      flex: 1,
                      minWidth: 0,
                      color: look().palette.text,
                      fontSize: look().type.text_xsmall.size,
                      lineHeight: look().type.text_xsmall.line_height,
                    }}
                  >
                    {t("default.pick_banner")}
                  </Text>
                </View>
              </If>

              <Each of={groups()} key={(g: MachineGroup) => g.key}>
                {(group: () => MachineGroup) => (
                  <View style={{ display: "flex", flexDirection: "column", gap: 12 }}>
                    <If when={grouped()}>
                      <View
                        style={{
                          paddingTop: 8,
                          display: "flex",
                          flexDirection: "row",
                          alignItems: "center",
                          gap: 8,
                        }}
                      >
                        <Text
                          style={{
                            color: look().palette.muted,
                            fontSize: look().type.text_xsmall_medium.size,
                            lineHeight: look().type.text_xsmall_medium.line_height,
                            fontWeight: look().type.text_xsmall_medium.weight,
                          }}
                        >
                          {group().name}
                        </Text>
                        <If when={group().status != null}>
                          <Text
                            style={{
                              color: look().palette.faint,
                              fontSize: look().type.text_xsmall.size,
                              lineHeight: look().type.text_xsmall.line_height,
                            }}
                          >
                            {group().status ?? ""}
                          </Text>
                        </If>
                        <If when={group().status == null && group().agents.length === 0}>
                          <Text
                            style={{
                              color: look().palette.faint,
                              fontSize: look().type.text_xsmall.size,
                              lineHeight: look().type.text_xsmall.line_height,
                            }}
                          >
                            {t("empty.title")}
                          </Text>
                        </If>
                      </View>
                    </If>

                    <Each
                      of={group().agents}
                      key={(a: any) => agentRefKey(group().machine, a.id)}
                    >
                      {(agent: () => any) => (
                        <AgentCatalogRow machine={group().machine} agent={agent} />
                      )}
                    </Each>
                  </View>
                )}
              </Each>
            </View>
          </Scroll>
        }
      >
        <View
          style={{
            flex: 1,
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            paddingLeft: 32,
            paddingRight: 32,
            gap: 12,
          }}
        >
          <If when={topError() != null}>
            <View style={{ width: "100%", maxWidth: 420 }}>
              <ErrorAlert message={topError() ?? ""} />
            </View>
          </If>
          <SurfaceCard
            style={{
              width: "100%",
              maxWidth: 420,
              paddingLeft: 32,
              paddingRight: 32,
              paddingTop: 40,
              paddingBottom: 40,
              alignItems: "center",
              gap: 16,
            }}
          >
            <EngineMark engineId={defaultEngineId()} size={28} />
            <View
              style={{
                display: "flex",
                flexDirection: "column",
                alignItems: "center",
                gap: 8,
              }}
            >
              <Text
                style={{
                  color: look().palette.text,
                  fontSize: 18,
                  fontWeight: 600,
                }}
              >
                {t("empty.title")}
              </Text>
              <Text
                style={{
                  color: look().palette.muted,
                  fontSize: look().type.ui.size,
                  lineHeight: look().type.ui.line_height,
                  textAlign: "center",
                }}
              >
                {armConversationOnCreate() ? t("empty.description_arm") : t("empty.description")}
              </Text>
            </View>
            <CreateAgentButton small={true} />
          </SurfaceCard>
        </View>
      </If>
    </View>
  );
}
