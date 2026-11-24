// 自动化下钻头、左侧运行列表与任务表单编辑器。
import { computed, Scroll, Text, View } from "nsh";
import { Menu } from "nsh/base";
import { isManagedAutomationWorkspace, pathStyle, SmeltButton, SmeltIcon, t } from "smelt/sdk";
import {
  actionKindLabel,
  automationAccessNote,
  automationPromptHint,
  automationSaveHint,
  BUILTIN_AUTOMATION_ACTIONS,
  BUILTIN_AUTOMATION_TRIGGER_PRESETS,
  compactInstructions,
  DEFAULT_WEBHOOK_BASE_URL,
  findAutomation,
  formatRelativeUntil,
  formatUnixLocal,
  localTimezoneLabel,
  NOTIFY_CHOICES,
  notifyHint,
  notifyLabel,
  runErrorText,
  runSourceLabel,
  runStatusLabel,
  scheduleFromRule,
  stateForAutomation,
  triggerEntryHint,
  triggerEntryTitle,
  triggerPresetHint,
  triggerPresetIconPath,
  triggerPresetIsAdvanced,
  triggerPresetLabel,
  webhookCurlExample,
  webhookUrl,
  weekdayLabel,
} from "../model.js";
import { AGENTS_VIEW } from "../contract.js";
import {
  addEditorTriggerPreset,
  agentsVersion,
  closeEditor,
  copyWithFlash,
  createAutomationFromEditor,
  currentTheme,
  editor,
  editorMissingFields,
  type EditorState,
  isViewRegistered,
  machineAutomationsOn,
  openAgentsView,
  pageError,
  pickEditorWorkspace,
  readyAgentsOn,
  regenerateWebhookSecret,
  removeEditorTriggerEntry,
  route,
  runOnceFromEditor,
  type ScheduleRuleDraft,
  selectAutomationRun,
  selectedRunId,
  setAutomationEnabled,
  setEditorActionKind,
  setEditorAgent,
  setEditorCommand,
  setEditorName,
  setEditorNotification,
  setEditorPrompt,
  setEditorWorkspace,
  setTriggerInterval,
  setTriggerIntervalDayScope,
  setTriggerIntervalUnit,
  setTriggerScheduleKind,
  setTriggerTime,
  setTriggerWindowEnd,
  setTriggerWindowOpen,
  setTriggerWindowStart,
  showAutomationTask,
  startDeleteAutomation,
  storeVersion,
  toggleTriggerWeekday,
  type TriggerEntryDraft,
} from "./state.ts";
import {
  AutomationSwitch,
  ContentCard,
  ErrorAlert,
  FieldHint,
  MenuDivider,
  MenuRow,
  MultiSegmentedControl,
  NeutralIconMark,
  PageToolbar,
  POPUP_CLASS,
  RowItem,
  RunLines,
  SectionHeading,
  SegmentedControl,
  StatusDot,
  StyledInput,
  runStatusColor,
} from "./ui.tsx";

const EDITOR_MAX_WIDTH = 640;

export function DrillHeader() {
  const look = currentTheme;
  const ed = computed(() => editor());
  const curRoute = computed(() => route());

  const headerModel = computed(() => {
    storeVersion();
    agentsVersion();
    const r = curRoute();
    const current = ed();
    const same = Boolean(current && r && current.id === r.id && current.machine === r.machine);
    const activeEd = same ? current : null;

    let title: string;
    if (activeEd) {
      title = activeEd.isNew
        ? t("catalog.new")
        : activeEd.name.trim() || t("automation.unnamed");
    } else if (r) {
      const stored = findAutomation(machineAutomationsOn(r.machine), r.id);
      title = stored?.name?.trim() ? stored.name : t("automation.unnamed");
    } else {
      title = t("automation.unnamed");
    }

    const missing = activeEd ? editorMissingFields(activeEd) : [];
    const subtitle = activeEd ? automationSaveHint(missing, activeEd.isNew, t) : null;
    const stored = activeEd
      ? findAutomation(machineAutomationsOn(activeEd.machine), activeEd.id)
      : null;
    const enabled = stored ? stored.enabled !== false : true;

    return {
      activeEd,
      title,
      missing,
      subtitle,
      enabled,
      backTooltip: activeEd?.isNew ? t("editor.cancel") : t("editor.back"),
    };
  });

  return (
    <PageToolbar justify="start">
      <SmeltButton
        testID="automation-drill-back"
        quiet={true}
        icon="icons/chevron-left.svg"
        tooltip={headerModel().backTooltip}
        accessibilityLabel={headerModel().backTooltip}
        onPress={closeEditor}
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
            fontSize: look().type.ui_medium.size,
            lineHeight: look().type.ui_medium.line_height,
            fontWeight: look().type.ui_medium.weight,
            color: look().palette.text,
            numberOfLines: 1,
          }}
        >
          {headerModel().title}
        </Text>
        <If when={headerModel().subtitle != null}>
          <Text
            style={{
              flex: 1,
              minWidth: 0,
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: headerModel().activeEd?.isNew ? look().palette.faint : look().palette.bad,
              numberOfLines: 1,
            }}
          >
            {headerModel().subtitle ?? ""}
          </Text>
        </If>
      </View>

      <If when={headerModel().activeEd != null}>
        <If
          when={Boolean(headerModel().activeEd?.isNew)}
          else={
            <View
              style={{
                flexShrink: 0,
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                gap: 8,
              }}
            >
              <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
                <AutomationSwitch
                  testID="automation-editor-enabled"
                  checked={headerModel().enabled}
                  accessibilityLabel={t("editor.enabled")}
                  onChange={(on) => {
                    const active = headerModel().activeEd;
                    if (active) {
                      setAutomationEnabled({ machine: active.machine, id: active.id }, on);
                    }
                  }}
                />
                <Text
                  style={{
                    fontSize: look().type.text_xsmall.size,
                    lineHeight: look().type.text_xsmall.line_height,
                    color: look().palette.muted,
                  }}
                >
                  {headerModel().enabled ? t("editor.enabled") : t("editor.paused")}
                </Text>
              </View>

              {/* Bug Fix #1 & #7: loading 仅由 runningOnce 驱动；点击前先冲刷未保存草稿 */}
              <SmeltButton
                testID="automation-editor-run"
                small={true}
                icon="icons/play.svg"
                label={t("menu.run_once")}
                loading={Boolean(headerModel().activeEd?.runningOnce)}
                onPress={() => void runOnceFromEditor()}
              />

              <Menu
                placement="bottom"
                align="end"
                popupClass={POPUP_CLASS}
                popupStyle={{ minWidth: 168 }}
                trigger={
                  <View
                    testID="automation-editor-more"
                    accessibilityLabel={t("catalog.more")}
                    class="hover:bg-accent"
                    style={{
                      width: 24,
                      height: 24,
                      borderRadius: look().radius.md,
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                      cursor: "default",
                    }}
                  >
                    <SmeltIcon path="icons/ellipsis.svg" size={16} color={look().palette.muted} />
                  </View>
                }
              >
                <MenuRow
                  label={t("menu.delete")}
                  icon="icons/trash.svg"
                  danger={true}
                  onSelect={() => {
                    const active = headerModel().activeEd;
                    if (active) {
                      startDeleteAutomation({ machine: active.machine, id: active.id });
                    }
                  }}
                />
              </Menu>
            </View>
          }
        >
          <SmeltButton
            testID="automation-create"
            primary={true}
            label={t("editor.create")}
            loading={headerModel().activeEd?.savingAction === "create"}
            disabled={headerModel().missing.length > 0}
            onPress={createAutomationFromEditor}
          />
        </If>
      </If>
    </PageToolbar>
  );
}

export function RunHistoryColumn(props: {
  machine: string | null;
  runs: any[];
  showTask: boolean;
}) {
  const look = currentTheme;
  const selRunId = computed(() => selectedRunId());

  return (
    <View
      style={{
        width: 280,
        flexShrink: 0,
        height: "100%",
        minHeight: 0,
        borderRightWidth: 1,
        borderRightColor: look().palette.hair,
        display: "flex",
        flexDirection: "column",
      }}
    >
      <Scroll style={{ flex: 1, minHeight: 0, width: "100%" }}>
        <View style={{ padding: 8, display: "flex", flexDirection: "column", gap: 4 }}>
          <If when={props.showTask}>
            <RowItem
              testID="automation-run-column-task"
              selected={selRunId() == null}
              onPress={showAutomationTask}
              style={{
                paddingLeft: 8,
                paddingRight: 8,
                paddingTop: 8,
                paddingBottom: 8,
                borderRadius: look().radius.row,
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
                {t("editor.settings")}
              </Text>
            </RowItem>
          </If>

          <View style={{ paddingLeft: 8, paddingRight: 8, paddingTop: 8, paddingBottom: 4 }}>
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: look().palette.faint,
              }}
            >
              {t("editor.runs")}
            </Text>
          </View>

          <Each of={props.runs} key={(r: any) => r.id}>
            {(runItem: () => any) => {
              const selected = () => selRunId() === runItem().id;
              const info = () => {
                const r = runItem();
                const at = r.finished_at ?? r.created_at;
                const title = `${runSourceLabel(r.source, t)} · ${runStatusLabel(r.status, t)}`;
                const detail = formatUnixLocal(at);
                const err =
                  r.status === "failed"
                    ? (() => {
                        const raw = runErrorText(r, t);
                        return raw ? compactInstructions(raw) : null;
                      })()
                    : null;
                return { title, detail, err, status: r.status };
              };
              return (
                <RowItem
                  testID={`automation-editor-run-${runItem().id}`}
                  selected={selected()}
                  onPress={() => selectAutomationRun(props.machine, runItem().id)}
                  style={{
                    paddingLeft: 8,
                    paddingRight: 8,
                    paddingTop: 8,
                    paddingBottom: 8,
                    borderRadius: look().radius.row,
                    alignItems: "flex-start",
                    gap: 8,
                  }}
                >
                  <StatusDot
                    size={7}
                    color={runStatusColor(info().status, look().palette)}
                    style={{ marginTop: 6 }}
                  />
                  <RunLines
                    title={info().title}
                    detail={info().detail}
                    error={info().err}
                    selected={selected()}
                  />
                </RowItem>
              );
            }}
          </Each>
        </View>
      </Scroll>
    </View>
  );
}

// ---- 触发器卡片（日历/间隔 与 Webhook 分离，无重复 ID，无死 can_remove） ----

function ScheduleRuleBody(props: { uid: string; rule: ScheduleRuleDraft }) {
  const look = currentTheme;
  const r = () => props.rule;

  const validationError = computed(() => {
    const res = scheduleFromRule(r(), t);
    return res.ok ? null : res.error;
  });

  const clockKinds = computed(() => {
    return [
      { value: "daily" as const, label: t("schedule_kind.daily") },
      { value: "weekdays" as const, label: t("schedule_kind.weekdays") },
      { value: "weekly" as const, label: t("schedule_kind.weekly") },
    ];
  });

  const intervalUnits = computed(() => {
    return [
      { value: "minutes" as const, label: t("interval_unit.minutes") },
      { value: "hours" as const, label: t("interval_unit.hours") },
    ];
  });

  const dayScopes = computed(() => {
    return [
      { value: "everyday" as const, label: t("schedule_kind.daily") },
      { value: "weekdays" as const, label: t("schedule_kind.weekdays") },
      { value: "weekly" as const, label: t("schedule_kind.weekly") },
    ];
  });

  const weekdayItems = computed(() => {
    const mask = r().weekdayMask;
    return [0, 1, 2, 3, 4, 5, 6].map((idx) => ({
      key: String(idx),
      label: weekdayLabel(idx, t),
      active: (mask & (1 << idx)) !== 0,
    }));
  });

  const tz = computed(() => {
    return localTimezoneLabel(t);
  });

  return (
    <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <If when={r().kind !== "interval"}>
        <View style={{ display: "flex", flexDirection: "row" }}>
          <SegmentedControl
            testID={`automation-trigger-frequency-${props.uid}`}
            options={clockKinds()}
            selected={r().kind === "weekdays" || r().kind === "weekly" ? r().kind : "daily"}
            onSelect={(kind) => setTriggerScheduleKind(props.uid, kind)}
          />
        </View>
      </If>

      <If when={r().kind === "weekly"}>
        <View style={{ display: "flex", flexDirection: "row" }}>
          <MultiSegmentedControl
            testID={`automation-weekday-${props.uid}`}
            items={weekdayItems()}
            onToggle={(idxStr) => toggleTriggerWeekday(props.uid, 1 << Number(idxStr))}
          />
        </View>
      </If>

      <If when={r().kind !== "interval"}>
        <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
          <View style={{ width: 108 }}>
            <StyledInput
              testID={`automation-time-${props.uid}`}
              value={r().time}
              placeholder="09:00"
              accessibilityLabel={
                r().kind === "daily"
                  ? t("schedule.daily_time")
                  : r().kind === "weekdays"
                    ? t("schedule.weekdays_time")
                    : t("schedule.run_time")
              }
              invalid={validationError() != null}
              onChange={(val) => setTriggerTime(props.uid, val)}
            />
          </View>
          <Text
            style={{
              fontSize: look().type.text_xsmall.size,
              lineHeight: look().type.text_xsmall.line_height,
              color: look().palette.muted,
            }}
          >
            {tz()}
          </Text>
        </View>
      </If>

      <If when={r().kind === "interval"}>
        <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
            <View style={{ width: 120 }}>
              <StyledInput
                testID={`automation-interval-${props.uid}`}
                value={r().interval}
                placeholder="1"
                accessibilityLabel={t("trigger_preset.interval")}
                invalid={validationError() != null}
                onChange={(val) => setTriggerInterval(props.uid, val)}
              />
            </View>
            <SegmentedControl
              testID={`automation-interval-unit-${props.uid}`}
              options={intervalUnits()}
              selected={r().intervalUnit}
              onSelect={(unit) => setTriggerIntervalUnit(props.uid, unit)}
            />
          </View>

          <View style={{ display: "flex", flexDirection: "row" }}>
            <SegmentedControl
              testID={`automation-interval-days-${props.uid}`}
              options={dayScopes()}
              selected={r().intervalDayScope}
              onSelect={(scope) => setTriggerIntervalDayScope(props.uid, scope)}
            />
          </View>

          <If when={r().intervalDayScope === "weekly"}>
            <View style={{ display: "flex", flexDirection: "row" }}>
              <MultiSegmentedControl
                testID={`automation-interval-weekday-${props.uid}`}
                items={weekdayItems()}
                onToggle={(idxStr) => toggleTriggerWeekday(props.uid, 1 << Number(idxStr))}
              />
            </View>
          </If>

          <If
            when={r().windowOpen}
            else={
              <View style={{ display: "flex", flexDirection: "row" }}>
                <SmeltButton
                  testID={`automation-interval-window-open-${props.uid}`}
                  quiet={true}
                  small={true}
                  label={t("schedule.limit_hours")}
                  onPress={() => setTriggerWindowOpen(props.uid, true)}
                />
              </View>
            }
          >
            <View
              style={{
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                flexWrap: "wrap",
                gap: 8,
              }}
            >
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {t("schedule.from")}
              </Text>
              <View style={{ width: 108 }}>
                <StyledInput
                  testID={`automation-window-start-${props.uid}`}
                  value={r().windowStart}
                  placeholder={t("schedule.window_start")}
                  accessibilityLabel={t("schedule.start_time")}
                  invalid={validationError() != null}
                  onChange={(val) => setTriggerWindowStart(props.uid, val)}
                />
              </View>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {t("schedule.to")}
              </Text>
              <View style={{ width: 108 }}>
                <StyledInput
                  testID={`automation-window-end-${props.uid}`}
                  value={r().windowEnd}
                  placeholder={t("schedule.window_end")}
                  accessibilityLabel={t("schedule.end_time")}
                  invalid={validationError() != null}
                  onChange={(val) => setTriggerWindowEnd(props.uid, val)}
                />
              </View>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                {tz()}
              </Text>
              <SmeltButton
                testID={`automation-interval-window-clear-${props.uid}`}
                quiet={true}
                small={true}
                label={t("schedule.all_day")}
                onPress={() => setTriggerWindowOpen(props.uid, false)}
              />
            </View>
          </If>
        </View>
      </If>

      <If when={validationError() != null}>
        <Text
          style={{
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            color: look().palette.bad,
          }}
        >
          {validationError() ?? ""}
        </Text>
      </If>
    </View>
  );
}

function WebhookEntryBody(props: { uid: string; secret: string; editor: EditorState }) {
  const look = currentTheme;
  const copyId = () => `automation-copy-webhook-${props.uid}`;
  const curlId = () => `automation-copy-webhook-curl-${props.uid}`;
  const regenId = () => `automation-regen-webhook-${props.uid}`;

  const buttonLabel = (id: string, idle: string) => {
    const fl = props.editor.flash;
    if (fl && fl.id === id) {
      return fl.kind === "copied" ? t("flash.copied") : t("flash.regenerated");
    }
    return idle;
  };

  const info = computed(() => {
    storeVersion();
    const proj = machineAutomationsOn(props.editor.machine);
    const base = proj?.webhook_base_url || DEFAULT_WEBHOOK_BASE_URL;
    const stored = findAutomation(proj, props.editor.id);
    const paused = Boolean(stored && stored.enabled === false);
    const liveUrl =
      !props.editor.isNew && props.secret.length > 0 ? webhookUrl(base, props.secret) : null;
    return { paused, liveUrl };
  });

  return (
    <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <Text
        style={{
          fontSize: look().type.text_xsmall.size,
          lineHeight: look().type.text_xsmall.line_height,
          color: look().palette.muted,
        }}
      >
        {info().liveUrl ? t("webhook.post_hint") : t("webhook.save_hint")}
      </Text>

      <If when={info().paused}>
        <Text
          style={{
            fontSize: look().type.text_xsmall.size,
            lineHeight: look().type.text_xsmall.line_height,
            color: look().palette.muted,
          }}
        >
          {t("webhook.paused")}
        </Text>
      </If>

      <If when={info().liveUrl != null}>
        <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
          <View
            style={{
              flex: 1,
              minWidth: 0,
              paddingLeft: 12,
              paddingRight: 12,
              paddingTop: 8,
              paddingBottom: 8,
              borderRadius: 8,
              borderWidth: 1,
              borderColor: look().palette.hair_strong,
              backgroundColor: look().palette.side,
            }}
          >
            <Text
              style={{
                fontFamily: look().fonts?.mono,
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: look().palette.text,
              }}
            >
              {info().liveUrl ?? ""}
            </Text>
          </View>
          <SmeltButton
            testID={copyId()}
            quiet={true}
            small={true}
            label={buttonLabel(copyId(), t("webhook.copy"))}
            onPress={() => {
              const url = info().liveUrl;
              if (url) copyWithFlash(copyId(), url);
            }}
          />
        </View>
      </If>

      <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
        <If when={info().liveUrl != null}>
          <SmeltButton
            testID={curlId()}
            quiet={true}
            small={true}
            label={buttonLabel(curlId(), t("webhook.copy_example"))}
            onPress={() => {
              const url = info().liveUrl;
              if (url) copyWithFlash(curlId(), webhookCurlExample(url, t));
            }}
          />
        </If>
        <If when={!props.editor.isNew}>
          {/* Bug Fix #1: 仅该条 webhook 重新生成中时显示 spinner */}
          <SmeltButton
            testID={regenId()}
            quiet={true}
            small={true}
            loading={props.editor.regeneratingUid === props.uid}
            label={buttonLabel(regenId(), t("webhook.regenerate"))}
            onPress={() => regenerateWebhookSecret(props.uid)}
          />
        </If>
      </View>
    </View>
  );
}

function TriggerEntryCard(props: { entry: TriggerEntryDraft; editor: EditorState }) {
  const look = currentTheme;
  const e = () => props.entry;
  const icon = () =>
    e().triggerKind === "webhook"
      ? "icons/webhook.svg"
      : e().triggerKind === "interval"
        ? "icons/clock.svg"
        : "icons/calendar.svg";
  const title = () => triggerEntryTitle(e(), t);
  const hint = () => triggerEntryHint(e(), t);
  const showHint = () => title() !== hint();

  return (
    <ContentCard
      testID={`automation-trigger-${e().uid}`}
      style={{ paddingLeft: 16, paddingRight: 16, paddingTop: 12, paddingBottom: 12, gap: 12 }}
    >
      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "flex-start",
          gap: 12,
        }}
      >
        <NeutralIconMark icon={icon()} size={32} />
        <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
          <Text
            style={{
              fontSize: look().type.ui_medium.size,
              lineHeight: look().type.ui_medium.line_height,
              fontWeight: look().type.ui_medium.weight,
              color: look().palette.text,
            }}
          >
            {title()}
          </Text>
          <If when={showHint()}>
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: look().palette.muted,
              }}
            >
              {hint()}
            </Text>
          </If>
        </View>
        <SmeltButton
          testID={`automation-remove-trigger-${e().uid}`}
          quiet={true}
          small={true}
          icon="icons/trash.svg"
          tooltip={t("trigger.delete")}
          accessibilityLabel={t("trigger.delete")}
          onPress={() => removeEditorTriggerEntry(e().uid)}
        />
      </View>

      <If
        when={e().triggerKind === "webhook"}
        else={
          <ScheduleRuleBody
            uid={e().uid}
            rule={(e() as Extract<TriggerEntryDraft, { triggerKind: "calendar" | "interval" }>).schedule}
          />
        }
      >
        <WebhookEntryBody
          uid={e().uid}
          secret={(e() as Extract<TriggerEntryDraft, { triggerKind: "webhook" }>).webhookSecret}
          editor={props.editor}
        />
      </If>
    </ContentCard>
  );
}

function AddTriggerButton() {
  const look = currentTheme;
  const regularPresets = BUILTIN_AUTOMATION_TRIGGER_PRESETS.filter((p) => !triggerPresetIsAdvanced(p));
  const advancedPresets = BUILTIN_AUTOMATION_TRIGGER_PRESETS.filter((p) => triggerPresetIsAdvanced(p));

  return (
    <Menu
      placement="bottom"
      align="start"
      popupClass={POPUP_CLASS}
      popupStyle={{ minWidth: 220 }}
      trigger={
        <View
          testID="automation-add-trigger"
          class="hover:bg-accent"
          style={{
            width: "100%",
            height: 36,
            borderRadius: 18,
            borderWidth: 1,
            borderStyle: "dashed",
            borderColor: look().palette.hair_strong,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            justifyContent: "center",
            gap: 6,
            cursor: "default",
          }}
        >
          <SmeltIcon path="icons/plus.svg" size={14} color={look().palette.muted} />
          <Text
            style={{
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
              color: look().palette.muted,
            }}
          >
            {t("trigger.add")}
          </Text>
        </View>
      }
    >
      <Each of={regularPresets} key={(p: string) => p}>
        {(preset: () => string) => (
          <MenuRow
            icon={triggerPresetIconPath(preset())}
            label={`${triggerPresetLabel(preset(), t)} · ${triggerPresetHint(preset(), t)}`}
            onSelect={() => addEditorTriggerPreset(preset())}
          />
        )}
      </Each>
      <If when={advancedPresets.length > 0}>
        <MenuDivider />
        <Each of={advancedPresets} key={(p: string) => p}>
          {(preset: () => string) => (
            <MenuRow
              icon={triggerPresetIconPath(preset())}
              label={`${triggerPresetLabel(preset(), t)} · ${triggerPresetHint(preset(), t)}`}
              onSelect={() => addEditorTriggerPreset(preset())}
            />
          )}
        </Each>
      </If>
    </Menu>
  );
}

// ---- 完整编辑器表单 ----

export function EditorForm(props: { editor: EditorState }) {
  const look = currentTheme;
  const ed = () => props.editor;

  const readyAgents = computed(() => {
    agentsVersion();
    return readyAgentsOn(ed().machine);
  });

  const currentAgentName = computed(() => {
    const found = readyAgents().find((a) => a.id === ed().agentId);
    return found?.name ? String(found.name) : t("editor.choose_agent");
  });

  const actionOptions = computed(() => {
    return BUILTIN_AUTOMATION_ACTIONS.map((kind) => ({
      value: kind as "agent" | "shell",
      label: actionKindLabel(kind, t),
    }));
  });

  const notifyOptions = computed(() => {
    return NOTIFY_CHOICES.map((choice) => ({
      value: choice as "never" | "failures_only" | "always",
      label: notifyLabel(choice, t),
    }));
  });

  const whenHint = computed(() => {
    storeVersion();
    const cur = ed();
    if (!cur.isNew) {
      const proj = machineAutomationsOn(cur.machine);
      const stored = findAutomation(proj, cur.id);
      if (stored && stored.enabled !== false) {
        const nextAt = stateForAutomation(proj, cur.id)?.next_run_at;
        if (nextAt != null) {
          return t("editor.next_run", {
            when: formatRelativeUntil(nextAt, Math.floor(Date.now() / 1000), t),
          });
        }
      }
    }
    return t("editor.trigger_hint");
  });

  const isAgent = () => ed().actionKind === "agent";
  const isWebhook = () => ed().entries.some((e) => e.triggerKind === "webhook");
  /** 用户自选的工作目录；没选或是运行时分配的托管目录时为 `null`。 */
  const customDir = () => {
    const { machine, workspaceDir, id } = ed();
    if (typeof workspaceDir !== "string" || workspaceDir.trim().length === 0) return null;
    return isManagedAutomationWorkspace(pathStyle(machine), workspaceDir, id) ? null : workspaceDir;
  };
  const canOpenAgents = computed(() => {
    return isViewRegistered(AGENTS_VIEW);
  });

  return (
    <Scroll style={{ flex: 1, minWidth: 0, height: "100%" }}>
      <View
        style={{
          width: "100%",
          maxWidth: EDITOR_MAX_WIDTH,
          marginLeft: "auto",
          marginRight: "auto",
          paddingLeft: 24,
          paddingRight: 24,
          paddingTop: 24,
          paddingBottom: 40,
          display: "flex",
          flexDirection: "column",
          gap: 20,
        }}
      >
        {/* Bug Fix #2: 在编辑器里同时可见呈现 pageError 与 editor.error */}
        <If when={pageError() != null}>
          <ErrorAlert
            testID="automation-page-error"
            message={pageError() ?? ""}
            onDismiss={() => pageError.set(null)}
          />
        </If>
        <If when={ed().error != null}>
          <ErrorAlert
            testID="automation-editor-error"
            message={ed().error ?? ""}
          />
        </If>

        {/* 名称 */}
        <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <SectionHeading label={t("missing.name")} />
          <StyledInput
            testID="automation-name"
            autoFocus={ed().autoFocusName}
            value={ed().name}
            placeholder={t("editor.name_placeholder")}
            accessibilityLabel={t("missing.name")}
            onChange={(val) => setEditorName(val)}
          />
        </View>

        {/* 谁来做 */}
        <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <SectionHeading label={t("editor.who")} />
          <View
            style={{
              display: "flex",
              flexDirection: "row",
              alignItems: "center",
              justifyContent: "space-between",
              gap: 12,
            }}
          >
            <SegmentedControl
              testID="automation-action-kind"
              options={actionOptions()}
              selected={ed().actionKind}
              onSelect={(kind) => setEditorActionKind(kind)}
            />

            <If when={isAgent()}>
              <If
                when={readyAgents().length > 0}
                else={
                  <If when={canOpenAgents()}>
                    <SmeltButton
                      testID="automation-open-agents"
                      quiet={true}
                      small={true}
                      icon="icons/bot.svg"
                      label={t("editor.create_agent")}
                      onPress={() => openAgentsView(ed().machine)}
                    />
                  </If>
                }
              >
                <Menu
                  placement="bottom"
                  align="end"
                  popupClass={POPUP_CLASS}
                  popupStyle={{ minWidth: 168 }}
                  trigger={
                    <View
                      testID="automation-agent"
                      class="hover:bg-accent"
                      style={{
                        height: 24,
                        paddingLeft: 10,
                        paddingRight: 8,
                        borderRadius: look().radius.md,
                        display: "flex",
                        flexDirection: "row",
                        alignItems: "center",
                        gap: 6,
                        cursor: "default",
                      }}
                    >
                      <SmeltIcon path="icons/bot.svg" size={14} color={look().palette.muted} />
                      <Text
                        style={{
                          fontSize: look().type.small.size,
                          lineHeight: look().type.small.line_height,
                          color: look().palette.text,
                          numberOfLines: 1,
                        }}
                      >
                        {currentAgentName()}
                      </Text>
                      <SmeltIcon path="icons/chevron-down.svg" size={12} color={look().palette.muted} />
                    </View>
                  }
                >
                  <Each of={readyAgents()} key={(a: any) => a.id}>
                    {(agent: () => any) => (
                      <MenuRow
                        icon="icons/bot.svg"
                        label={agent().name || t("agent.unnamed")}
                        onSelect={() => setEditorAgent(agent().id)}
                      />
                    )}
                  </Each>
                </Menu>
              </If>
            </If>
          </View>
        </View>

        {/* 什么时候 */}
        <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <SectionHeading label={t("editor.when")} hint={whenHint()} />
          <View style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            <Each of={ed().entries} key={(entry: TriggerEntryDraft) => entry.uid}>
              {(entry: () => TriggerEntryDraft) => (
                <TriggerEntryCard entry={entry()} editor={ed()} />
              )}
            </Each>
            <AddTriggerButton />
          </View>
        </View>

        {/* 做什么 */}
        <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <SectionHeading label={t("editor.what")} />
          <ContentCard style={{ padding: 12 }}>
            <If
              when={isAgent()}
              else={
                <StyledInput
                  testID="automation-command"
                  multiline={true}
                  bare={true}
                  value={ed().command}
                  placeholder={t("editor.command_placeholder")}
                  accessibilityLabel={t("missing.command")}
                  onChange={(val) => setEditorCommand(val)}
                  style={{ fontFamily: look().fonts?.mono }}
                />
              }
            >
              <StyledInput
                testID="automation-prompt"
                multiline={true}
                bare={true}
                value={ed().prompt}
                placeholder={t("editor.prompt_placeholder")}
                accessibilityLabel={t("missing.prompt")}
                onChange={(val) => setEditorPrompt(val)}
              />
            </If>
          </ContentCard>
          <If when={isAgent()}>
            <FieldHint text={automationPromptHint(isWebhook(), t)} />
          </If>
          <View style={{ display: "flex", flexDirection: "column", gap: 2 }}>
            <Text
              style={{
                fontSize: look().type.text_xsmall_medium.size,
                lineHeight: look().type.text_xsmall_medium.line_height,
                fontWeight: look().type.text_xsmall_medium.weight,
                color: look().palette.muted,
              }}
            >
              {t("access.title")}
            </Text>
            <Text
              style={{
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
                color: look().palette.muted,
              }}
            >
              {automationAccessNote(isAgent(), t)}
            </Text>
          </View>
        </View>

        {/* 在哪做 */}
        <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <SectionHeading label={t("editor.where")} />
          <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
            <Text
              style={{
                flex: 1,
                minWidth: 0,
                fontSize: look().type.small.size,
                lineHeight: look().type.small.line_height,
                color: customDir() != null ? look().palette.text : look().palette.muted,
                numberOfLines: 1,
              }}
            >
              {customDir() ?? t("workspace.default")}
            </Text>
            <SmeltButton
              testID="automation-workspace-choose"
              small={true}
              label={t("workspace.choose")}
              onPress={pickEditorWorkspace}
            />
            <If when={customDir() != null}>
              <SmeltButton
                testID="automation-workspace-reset"
                small={true}
                quiet={true}
                label={t("workspace.reset")}
                onPress={() => setEditorWorkspace(null)}
              />
            </If>
          </View>
          <If when={customDir() == null}>
            <FieldHint
              text={isAgent() ? t("workspace.default_agent") : t("workspace.default_shell")}
            />
          </If>
        </View>

        {/* 通知 */}
        <View style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <SectionHeading label={t("notification.title")} />
          <View style={{ display: "flex", flexDirection: "row" }}>
            <SegmentedControl
              testID="automation-notification"
              options={notifyOptions()}
              selected={ed().notification}
              onSelect={(notify) => setEditorNotification(notify)}
            />
          </View>
          <FieldHint text={notifyHint(ed().notification, t)} />
        </View>
      </View>
    </Scroll>
  );
}
