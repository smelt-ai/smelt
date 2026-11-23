// 通知插件（来源 `smelt.notifications`）：
// - 订阅各台机器的 `attention.changed` 话题，镜像会话提醒；
// - 通过宿主通用能力投递/撤回系统通知、更新 macOS 菜单栏与 Dock 角标，并在人看着会话时自动 `attention.mark_read`；
// - 持久化用户提醒开关与已投递通知清单到 `~/.smelt/plugin-data/smelt.notifications/store.sqlite3`；
// - 宿主常驻 `MainShell` 保证主窗口关闭后本插件无头根仍在后台运行，点通知/菜单栏即可重开主窗口并跳到对应会话。

import { computed, Scroll, signal, Text, View } from "nsh";
import { Switch } from "nsh/base";
import {
  activeContext,
  agentCatalog,
  agentIdFromLaunch,
  attentionTexts,
  bootTask,
  call,
  command,
  daemonStateStatus,
  initHost,
  machines,
  notificationPermission,
  onEvent,
  registerTopics,
  registerViews,
  run,
  setStatusMenu,
  SmeltButton,
  stableMemo,
  state as hostState,
  store,
  t,
  text,
  windowActive,
} from "smelt/sdk";

import {
  ACTIVATE_SESSION,
  METHOD_MARK_READ,
  NOTIFY,
  OPEN_NOTIFICATION_SETTINGS,
  REQUEST_NOTIFICATION_PERMISSION,
  SET_DOCK_BADGE,
  TOPIC_ATTENTION,
  VIEW_NAME,
  WITHDRAW_NOTIFICATION,
} from "./contract.js";
import {
  AttentionMirror,
  buildMenu,
  conversationRow,
  DEFAULT_PREFS,
  deliveredKey,
  machineColumn,
  machineFromColumn,
  notificationId,
  permissionLine,
  prefsAllows,
} from "./model.js";

const SETTINGS_KEYWORDS = [
  "Agent",
  "提醒",
  "notification",
  "系统通知",
  "等待批准",
  "等待输入",
  "任务完成",
  "任务失败",
  "审批",
  "批准",
  "响铃",
  "bell",
  "approval",
  "alert",
];

const MIGRATIONS = [
  `CREATE TABLE prefs (
      id INTEGER PRIMARY KEY CHECK (id = 1),
      approval INTEGER NOT NULL,
      input INTEGER NOT NULL,
      success INTEGER NOT NULL,
      failure INTEGER NOT NULL,
      bell INTEGER NOT NULL
  );
  CREATE TABLE delivered (
      machine TEXT NOT NULL,
      session_id TEXT NOT NULL,
      PRIMARY KEY (machine, session_id)
  );`,
];

interface Prefs {
  approval: boolean;
  input: boolean;
  success: boolean;
  failure: boolean;
  bell: boolean;
}

const prefsSignal = signal<Prefs>({ ...DEFAULT_PREFS });
let storeReady = false;

const mirror = new AttentionMirror();
// deliveredKey -> { machine, sessionId }
const delivered = new Map<string, { machine: string | null; sessionId: string }>();
const marking = new Set<string>();

let lastMenuJson = "";
let lastBadgeSent: string | null | undefined = undefined;
let knownAgentIds: Set<string> | null = null;

function agentIds(): Set<string> {
  if (!knownAgentIds) {
    const cat = agentCatalog();
    const ids = new Set<string>();
    for (const a of cat.conversation_agents ?? []) ids.add(a.id);
    for (const a of cat.agent_engines ?? []) ids.add(a.id);
    for (const id of cat.terminal_agent_ids ?? []) ids.add(id);
    knownAgentIds = ids;
  }
  return knownAgentIds;
}

function viewedSession(): { machine: string | null; sessionId: string } | null {
  if (!windowActive()) return null;
  const session = activeContext()?.session;
  if (!session || !session.at_latest || typeof session.id !== "string") return null;
  return { machine: session.machine ?? null, sessionId: session.id };
}

function machineRows(entry: any): any[] {
  const dir = entry?.directory;
  if (!dir || !Array.isArray(dir.conversations)) return [];
  const statesMap = new Map<string, any>();
  for (const st of entry.states ?? []) {
    if (st && typeof st.id === "string") {
      statesMap.set(st.id, st);
    }
  }
  const machine = entry.machine ?? null;
  const remoteName = machine !== null ? String(entry.name ?? machine) : null;
  const deps = {
    t,
    statusOf: (st: any) => daemonStateStatus(st),
    agentFromLaunch: (cmd: string) => agentIdFromLaunch(cmd),
  };
  const rows: any[] = [];
  for (const conv of dir.conversations) {
    if (conv?.archived_at != null) continue;
    const row = conversationRow(machine, conv, statesMap, remoteName, deps);
    if (row) rows.push(row);
  }
  return rows;
}

function sessionTitle(machine: string | null, sessionId: string): string | null {
  const entry = machines().find((m: any) => (m?.machine ?? null) === machine);
  if (!entry) return null;
  const row = machineRows(entry).find((r) => r.ids.includes(sessionId));
  return row ? row.title : null;
}

function withdraw(machine: string | null, sessionId: string) {
  const key = deliveredKey(machine, sessionId);
  if (!delivered.has(key)) return;
  delivered.delete(key);
  if (storeReady) {
    void store
      .execute("DELETE FROM delivered WHERE machine = ?1 AND session_id = ?2", [
        machineColumn(machine),
        sessionId,
      ])
      .catch((error) => console.error("[smelt.notifications] remove delivered failed:", error));
  }
  void run(WITHDRAW_NOTIFICATION, { id: notificationId(machine, sessionId) }).catch((error) => {
    console.error("[smelt.notifications] withdraw failed:", error);
  });
}

function withdrawRead() {
  for (const { machine, sessionId } of [...delivered.values()]) {
    if (mirror.primed(machine) && !mirror.hasUnread(machine, sessionId)) {
      withdraw(machine, sessionId);
    }
  }
}

function markViewedRead() {
  const viewed = viewedSession();
  if (!viewed) return;
  if (!mirror.hasUnread(viewed.machine, viewed.sessionId)) return;
  const key = deliveredKey(viewed.machine, viewed.sessionId);
  if (marking.has(key)) return;
  marking.add(key);
  void call(viewed.machine, METHOD_MARK_READ, { session_id: viewed.sessionId })
    .catch((error) => console.error("[smelt.notifications] mark_read failed:", error))
    .finally(() => {
      marking.delete(key);
    });
}

function deliver(machine: string | null, session: any, alert: any) {
  const sessionId = String(session.session_id);
  const viewed = viewedSession();
  const isViewing =
    viewed !== null && viewed.machine === machine && viewed.sessionId === sessionId;
  if (!prefsAllows(prefsSignal(), alert?.kind) || isViewing) {
    withdraw(machine, sessionId);
    return;
  }

  let titleText = String(alert?.title ?? "");
  let bodyText = String(alert?.message ?? "");
  try {
    const resolved = attentionTexts(alert);
    titleText = resolved.title;
    bodyText = resolved.message;
  } catch (error) {
    console.error("[smelt.notifications] attentionTexts failed:", error);
  }

  const convTitle = sessionTitle(machine, sessionId);
  const subtitle = convTitle ? `${titleText} · ${convTitle}` : titleText;

  if (session.unread == null) {
    void run(NOTIFY, {
      id: null,
      subtitle,
      body: bodyText,
      on_click: null,
    }).catch((error) => console.error("[smelt.notifications] notify failed:", error));
    return;
  }

  const id = notificationId(machine, sessionId);
  void run(NOTIFY, {
    id,
    subtitle,
    body: bodyText,
    on_click: {
      command: ACTIVATE_SESSION,
      args: { machine, session_id: sessionId },
    },
  }).catch((error) => console.error("[smelt.notifications] notify failed:", error));

  const key = deliveredKey(machine, sessionId);
  if (!delivered.has(key)) {
    delivered.set(key, { machine, sessionId });
    if (storeReady) {
      void store
        .execute("INSERT OR IGNORE INTO delivered (machine, session_id) VALUES (?1, ?2)", [
          machineColumn(machine),
          sessionId,
        ])
        .catch((error) => console.error("[smelt.notifications] add delivered failed:", error));
    }
  }
}

function refreshMenubar() {
  const attention = mirror.bySession();
  const rows: any[] = [];
  for (const entry of machines()) {
    rows.push(...machineRows(entry));
  }
  const palette = hostState().theme.palette;
  const built = buildMenu(rows, attention, palette, agentIds(), t);
  const menuJson = JSON.stringify(built.statusMenu);
  if (menuJson !== lastMenuJson) {
    lastMenuJson = menuJson;
    void setStatusMenu(built.statusMenu).catch((error) => {
      console.error("[smelt.notifications] setStatusMenu failed:", error);
      lastMenuJson = "";
    });
  }

  if (!command(SET_DOCK_BADGE)) return;
  const badgeLabel = attention.size > 0 ? String(attention.size) : null;
  if (lastBadgeSent !== badgeLabel) {
    lastBadgeSent = badgeLabel;
    void run(SET_DOCK_BADGE, { label: badgeLabel }).catch((error) => {
      console.error("[smelt.notifications] set-dock-badge failed:", error);
      lastBadgeSent = undefined;
    });
  }
}

function onAttention(machine: string | null, message: any) {
  const applied = mirror.apply(machine, message);
  if (applied.kind === "nothing") return;
  if (applied.kind === "changed" && applied.alert != null) {
    deliver(machine, applied.session, applied.alert);
  }
  withdrawRead();
  markViewedRead();
  refreshMenubar();
}

function savePrefs(next: Prefs) {
  prefsSignal.set(next);
  if (!storeReady) return;
  void store
    .execute(
      `INSERT INTO prefs (id, approval, input, success, failure, bell)
       VALUES (1, ?1, ?2, ?3, ?4, ?5)
       ON CONFLICT (id) DO UPDATE SET
         approval = ?1, input = ?2, success = ?3, failure = ?4, bell = ?5`,
      [
        next.approval ? 1 : 0,
        next.input ? 1 : 0,
        next.success ? 1 : 0,
        next.failure ? 1 : 0,
        next.bell ? 1 : 0,
      ],
    )
    .catch((error) => console.error("[smelt.notifications] save prefs failed:", error));
}

onEvent((event) => {
  if (!event || typeof event !== "object") return;
  switch (event.type) {
    case "runtime_topic":
      if (event.topic === TOPIC_ATTENTION) {
        onAttention(event.machine ?? null, event.message);
      }
      break;
    case "machines_changed": {
      const known = new Set<string | null>(machines().map((m: any) => m?.machine ?? null));
      mirror.retainMachines((m) => known.has(m));
      for (const { machine, sessionId } of [...delivered.values()]) {
        if (!known.has(machine)) {
          withdraw(machine, sessionId);
        }
      }
      refreshMenubar();
      break;
    }
    case "sessions_changed":
    case "commands_changed":
    case "locale_changed":
    case "themes_changed":
      refreshMenubar();
      break;
    case "active_context_changed":
    case "window_activated":
      markViewedRead();
      break;
    default:
      break;
  }
});

bootTask(async () => {
  await initHost();
  try {
    await store.migrate(MIGRATIONS);
    storeReady = true;
    const prefRows = await store.query(
      "SELECT approval, input, success, failure, bell FROM prefs WHERE id = 1",
    );
    if (Array.isArray(prefRows) && prefRows.length > 0) {
      const row = prefRows[0];
      prefsSignal.set({
        approval: Boolean(row.approval),
        input: Boolean(row.input),
        success: Boolean(row.success),
        failure: Boolean(row.failure),
        bell: Boolean(row.bell),
      });
    }
    const deliveredRows = await store.query("SELECT machine, session_id FROM delivered");
    if (Array.isArray(deliveredRows)) {
      for (const row of deliveredRows) {
        if (typeof row?.session_id === "string") {
          const machine = machineFromColumn(row.machine);
          delivered.set(deliveredKey(machine, row.session_id), {
            machine,
            sessionId: row.session_id,
          });
        }
      }
    }
  } catch (error) {
    console.error("[smelt.notifications] open store failed:", error);
  }

  await registerTopics([TOPIC_ATTENTION]);
  await registerViews([
    {
      name: VIEW_NAME,
      export: VIEW_NAME,
      title: text("page.title"),
      icon: "icons/bell.svg",
      draws_top_bar: false,
      instances: false,
      settings: {
        group: "general",
        keywords: SETTINGS_KEYWORDS,
      },
    },
  ]);

  refreshMenubar();
});

const currentTheme = stableMemo(() => hostState().theme);

function SectionCard(props: { title: string; children?: any }) {
  const look = currentTheme;
  return (
    <View style={{ width: "100%", display: "flex", flexDirection: "column", gap: 8 }}>
      <Text
        style={{
          fontSize: look().type.small_medium.size,
          lineHeight: look().type.small_medium.line_height,
          fontWeight: look().type.small_medium.weight,
          color: look().palette.muted,
        }}
      >
        {props.title}
      </Text>
      <View
        style={{
          width: "100%",
          borderRadius: look().radius.lg ?? 8,
          backgroundColor: look().palette.surface,
          borderWidth: 1,
          borderColor: look().palette.hair,
          overflow: "hidden",
          display: "flex",
          flexDirection: "column",
        }}
      >
        {props.children}
      </View>
    </View>
  );
}

function PrefSwitchRow(props: {
  label: string;
  description?: string;
  checked: boolean;
  last?: boolean;
  onChange: (checked: boolean) => void;
}) {
  const look = currentTheme;
  return (
    <View
      style={{
        width: "100%",
        paddingLeft: 16,
        paddingRight: 16,
        paddingTop: 12,
        paddingBottom: 12,
        borderBottomWidth: props.last ? 0 : 1,
        borderBottomColor: look().palette.hair,
        display: "flex",
        flexDirection: "row",
        alignItems: "center",
        justifyContent: "space-between",
        gap: 16,
      }}
    >
      <View style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: 2 }}>
        <Text
          style={{
            fontSize: look().type.ui.size,
            lineHeight: look().type.ui.line_height,
            color: look().palette.text,
          }}
        >
          {props.label}
        </Text>
        <If when={Boolean(props.description)}>
          <Text
            style={{
              fontSize: look().type.small.size,
              lineHeight: look().type.small.line_height,
              color: look().palette.muted,
            }}
          >
            {props.description ?? ""}
          </Text>
        </If>
      </View>
      <Switch
        checked={props.checked}
        accessibilityLabel={props.label}
        onChange={({ checked }: any) => props.onChange(Boolean(checked))}
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
    </View>
  );
}

function PermissionCardContent() {
  const look = currentTheme;
  const perm = computed(() => notificationPermission());
  const line = computed(() => permissionLine(perm(), t));

  const severityColor = computed(() => {
    const p = look().palette;
    switch (line()?.severity) {
      case "ok":
        return p.ok;
      case "error":
        return p.bad;
      case "warning":
        return p.need;
      default:
        return p.muted;
    }
  });

  const runFix = () => {
    const fix = line()?.fix;
    if (fix === "request") {
      void run(REQUEST_NOTIFICATION_PERMISSION, null).catch((error) => {
        console.error("[smelt.notifications] request-notification-permission failed:", error);
      });
    } else if (fix === "open_settings") {
      void run(OPEN_NOTIFICATION_SETTINGS, null).catch((error) => {
        console.error("[smelt.notifications] open-notification-settings failed:", error);
      });
    }
  };

  return (
    <View
      style={{
        width: "100%",
        paddingLeft: 16,
        paddingRight: 16,
        paddingTop: 12,
        paddingBottom: 12,
      }}
    >
      <If
        when={line() !== null}
        else={
          <Text
            style={{
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
              color: look().palette.muted,
            }}
          >
            {perm().kind === "unsupported" ? t("permission.unsupported") : t("permission.checking")}
          </Text>
        }
      >
        <View
          style={{
            width: "100%",
            minHeight: 28,
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            gap: 8,
          }}
        >
          <View
            style={{
              width: 6,
              height: 6,
              borderRadius: 3,
              backgroundColor: severityColor(),
              flexShrink: 0,
            }}
          />
          <Text
            style={{
              flex: 1,
              minWidth: 0,
              fontSize: look().type.ui.size,
              lineHeight: look().type.ui.line_height,
              color: severityColor(),
            }}
          >
            {line()?.text ?? ""}
          </Text>
          <If when={line()?.fix != null}>
            <SmeltButton
              small={true}
              label={
                line()?.fix === "request" ? t("permission.request") : t("permission.open_settings")
              }
              onPress={runFix}
            />
          </If>
        </View>
      </If>
    </View>
  );
}

export function main() {
  const prefs = () => prefsSignal();
  const update = (patch: Partial<Prefs>) => {
    savePrefs({ ...prefsSignal(), ...patch });
  };

  return (
    <Scroll style={{ flex: 1, width: "100%", height: "100%" }}>
      <View
        style={{
          width: "100%",
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
        }}
      >
        <View
          style={{
            maxWidth: 720,
            width: "100%",
            paddingLeft: 24,
            paddingRight: 24,
            paddingTop: 16,
            paddingBottom: 32,
            display: "flex",
            flexDirection: "column",
            gap: 24,
          }}
        >
          <SectionCard title={t("section.system")}>
            <PermissionCardContent />
          </SectionCard>

          <SectionCard title={t("section.agent")}>
            <PrefSwitchRow
              label={t("prefs.approval")}
              checked={prefs().approval}
              onChange={(approval) => update({ approval })}
            />
            <PrefSwitchRow
              label={t("prefs.input")}
              description={t("prefs.input_description")}
              checked={prefs().input}
              onChange={(input) => update({ input })}
            />
            <PrefSwitchRow
              label={t("prefs.success")}
              checked={prefs().success}
              onChange={(success) => update({ success })}
            />
            <PrefSwitchRow
              label={t("prefs.failure")}
              checked={prefs().failure}
              last={true}
              onChange={(failure) => update({ failure })}
            />
          </SectionCard>

          <SectionCard title={t("section.terminal")}>
            <PrefSwitchRow
              label={t("prefs.bell")}
              description={t("prefs.bell_description")}
              checked={prefs().bell}
              last={true}
              onChange={(bell) => update({ bell })}
            />
          </SectionCard>
        </View>
      </View>
    </Scroll>
  );
}

export default main;
