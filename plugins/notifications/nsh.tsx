// 通知插件（来源 `smelt.notifications`）：会话提醒的系统通知、菜单栏、Dock 角标和通知设置页。
//
// 提醒由各台机器的运行时（smeltd）判定，宿主镜像（machines() 每项的 attention），刚来、该响一下
// 的那条宿主另发 attention_alert。这里只决定发不发、写什么：
//
// - 设置里关了这类、或者人正看着这段会话（主窗口在前台、露着它、翻在最新处），不发，这段会话
//   之前那条也撤掉；正看着的顺手告诉运行时看过了（attention.mark_read）。
// - 能标已读的会话发一条带 id 的通知，点了跳到那段会话；运行时说它没有未读了（哪台设备上看过、
//   回过），撤掉。发过哪些记在库里（`~/.smelt/plugin-data/smelt.notifications/store.sqlite3`），
//   重启后也撤得掉。
// - 没处标已读的（自动化运行、命令行发的）发一次性的，不挂着等撤。
//
// 系统通知、菜单栏、角标都是宿主的能力（smelt.app/notify、setStatusMenu、smelt.app/set-dock-badge）。
// 主窗口关着时插件照常在后台跑，点通知、菜单项会重开主窗口跳到那段会话。什么时候做全靠事件。

import { computed, signal, Text, View } from "nsh";
import {
  activeContext,
  bootTask,
  call,
  command,
  currentTheme,
  daemonStateStatus,
  initHost,
  machines,
  notificationPermission,
  onEvent,
  registerViews,
  run,
  setStatusMenu,
  SmeltButton,
  SmeltPageScroll,
  SmeltSection,
  SmeltSectionCell,
  SmeltSettingRow,
  SmeltSwitch,
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
  VIEW_NAME,
  WITHDRAW_NOTIFICATION,
} from "./contract.js";
import {
  attentionIndex,
  buildMenu,
  DEFAULT_PREFS,
  deliveredKey,
  dockBadge,
  machineColumn,
  machineFromColumn,
  machineRows,
  notificationId,
  permissionLine,
  prefsAllows,
  sessionTitle,
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

type Session = { machine: string | null; sessionId: string };

const prefsSignal = signal<Prefs>({ ...DEFAULT_PREFS });
/** 库打开了。打不开（权限、版本比这个 App 新）就只在内存里用，日志里留一句。 */
let storeReady = false;

/** 发出去、还挂在通知中心里的，和库里那份一致。deliveredKey → 会话。 */
const delivered = new Map<string, Session>();
/** 已经告诉运行时看过、还没回话的：同一段不重复发。 */
const marking = new Set<string>();

/** 上次交给宿主的菜单、角标；交失败了清掉，下次事件重交。 */
let lastMenuJson = "";
let lastBadge: string | null | undefined = undefined;

const log = (what: string) => (error: unknown) =>
  console.error(`[smelt.notifications] ${what}失败：`, error);

function rowsOf(entries: any[]): any[] {
  const deps = { t, statusOf: daemonStateStatus };
  return entries.flatMap((entry) => machineRows(entry, deps));
}

/** 人正看着的会话：主窗口在前台、舞台上露着、翻在最新处。 */
function viewedSession(): Session | null {
  if (!windowActive()) return null;
  const session = activeContext()?.session;
  if (!session?.at_latest || typeof session.id !== "string") return null;
  return { machine: session.machine ?? null, sessionId: session.id };
}

function rememberDelivered(session: Session) {
  const key = deliveredKey(session.machine, session.sessionId);
  if (delivered.has(key)) return;
  delivered.set(key, session);
  if (!storeReady) return;
  void store
    .execute("INSERT OR IGNORE INTO delivered (machine, session_id) VALUES (?1, ?2)", [
      machineColumn(session.machine),
      session.sessionId,
    ])
    .catch(log("记下发过的通知"));
}

function withdraw(machine: string | null, sessionId: string) {
  const key = deliveredKey(machine, sessionId);
  if (!delivered.delete(key)) return;
  if (storeReady) {
    void store
      .execute("DELETE FROM delivered WHERE machine = ?1 AND session_id = ?2", [
        machineColumn(machine),
        sessionId,
      ])
      .catch(log("删掉发过的通知"));
  }
  void run(WITHDRAW_NOTIFICATION, { id: notificationId(machine, sessionId) }).catch(log("撤通知"));
}

/** 刚来的一条提醒：发系统通知，或者不发。 */
function deliver(event: any) {
  const machine: string | null = event.machine ?? null;
  const session = event.session;
  const sessionId = String(session.session_id);
  const viewed = viewedSession();
  const viewing = viewed?.machine === machine && viewed?.sessionId === sessionId;
  if (!prefsAllows(prefsSignal(), event.alert?.kind) || viewing) {
    // 这段会话又有了新情况，之前那条已经过时，一起撤掉。
    withdraw(machine, sessionId);
    return;
  }
  const entry = machines().filter((m: any) => (m?.machine ?? null) === machine);
  const title = sessionTitle(rowsOf(entry), sessionId);
  const subtitle = title ? `${event.title} · ${title}` : String(event.title);
  if (session.unread == null) {
    // 运行时没记未读：没处标已读（自动化运行、命令行发的），发一次性的。
    void run(NOTIFY, { id: null, subtitle, body: event.message, on_click: null }).catch(log("发通知"));
    return;
  }
  void run(NOTIFY, {
    id: notificationId(machine, sessionId),
    subtitle,
    body: event.message,
    on_click: { command: ACTIVATE_SESSION, args: { machine, session_id: sessionId } },
  }).catch(log("发通知"));
  rememberDelivered({ machine, sessionId });
}

/**
 * 跟着宿主的提醒镜像走：撤掉运行时说已经没有未读的（别的设备上看过、在终端里打过字、回过；
 * 还没收到快照的机器说不准，不算），撤掉已经移除了的机器的，正看着的告诉运行时看过了，
 * 重算菜单栏和角标。
 */
function sync() {
  const entries = machines();
  const index = attentionIndex(entries);
  const known = new Set(entries.map((entry: any) => machineColumn(entry?.machine ?? null)));
  for (const [key, { machine, sessionId }] of [...delivered]) {
    const column = machineColumn(machine);
    if (!known.has(column) || (index.primed.has(column) && !index.unread.has(key))) {
      withdraw(machine, sessionId);
    }
  }
  markViewedRead(index.unread);
  refreshMenubar(entries, index.bySession);
}

/**
 * 人正看着的那段会话有未读：告诉它所在的运行时看过了。运行时清掉后宿主的镜像跟着变，通知随之
 * 撤掉、角标跟着少。
 */
function markViewedRead(unread: Set<string>) {
  const viewed = viewedSession();
  if (!viewed) return;
  const key = deliveredKey(viewed.machine, viewed.sessionId);
  if (!unread.has(key) || marking.has(key)) return;
  marking.add(key);
  void call(viewed.machine, METHOD_MARK_READ, { session_id: viewed.sessionId })
    .catch(log("标已读"))
    .finally(() => marking.delete(key));
}

/** 重算菜单和角标，变了才交给宿主。主窗口关着也照常更新。 */
function refreshMenubar(entries: any[], bySession: Map<string, boolean>) {
  const menu = buildMenu(rowsOf(entries), bySession, currentTheme().palette, t);
  const menuJson = JSON.stringify(menu);
  if (menuJson !== lastMenuJson) {
    lastMenuJson = menuJson;
    void setStatusMenu(menu).catch((error) => {
      lastMenuJson = "";
      log("交菜单栏菜单")(error);
    });
  }
  // 宿主命令跟着主窗口的工作台登记，启动时还没有；登记好了会发 commands_changed。
  if (!command(SET_DOCK_BADGE)) return;
  const badge = dockBadge(bySession);
  if (badge !== lastBadge) {
    lastBadge = badge;
    void run(SET_DOCK_BADGE, { label: badge }).catch((error) => {
      lastBadge = undefined;
      log("交 Dock 角标")(error);
    });
  }
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
      [next.approval, next.input, next.success, next.failure, next.bell].map((on) => (on ? 1 : 0)),
    )
    .catch(log("存设置"));
}

async function openStore() {
  await store.migrate(MIGRATIONS);
  storeReady = true;
  const [row] = await store.query("SELECT approval, input, success, failure, bell FROM prefs WHERE id = 1");
  if (row) {
    prefsSignal.set({
      approval: Boolean(row.approval),
      input: Boolean(row.input),
      success: Boolean(row.success),
      failure: Boolean(row.failure),
      bell: Boolean(row.bell),
    });
  }
  for (const row of await store.query("SELECT machine, session_id FROM delivered")) {
    if (typeof row?.session_id !== "string") continue;
    const machine = machineFromColumn(row.machine);
    delivered.set(deliveredKey(machine, row.session_id), { machine, sessionId: row.session_id });
  }
}

onEvent(
  (event) => {
    switch (event?.type) {
      case "attention_alert":
        deliver(event);
        sync();
        return;
      // 提醒、会话状态、机器变了；菜单栏上的字是按当时的语言、主题写好交给系统的。
      case "sessions_changed":
      case "machines_changed":
      case "commands_changed":
      case "locale_changed":
      case "themes_changed":
      case "active_context_changed":
      case "window_activated":
        sync();
        return;
    }
  },
  [
    "attention_alert",
    "sessions_changed",
    "machines_changed",
    "commands_changed",
    "locale_changed",
    "themes_changed",
    "active_context_changed",
    "window_activated",
  ],
);

bootTask(async () => {
  await initHost([
    "machines_meta",
    "sessions_data",
    "active_context",
    "window_active",
    "notification_permission",
    "commands",
    "theme",
  ]);
  try {
    await openStore();
  } catch (error) {
    log("打开库")(error);
  }
  await registerViews([
    {
      name: VIEW_NAME,
      export: VIEW_NAME,
      title: text("page.title"),
      icon: "icons/bell.svg",
      draws_top_bar: false,
      instances: false,
      settings: { group: "general", keywords: SETTINGS_KEYWORDS },
    },
  ]);
  sync();
});

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
    <SmeltSectionCell>
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
            flexDirection: "row",
            alignItems: "center",
            gap: 8,
          }}
        >
          <View
            class="rounded-full"
            style={{
              width: 6,
              height: 6,
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
              size="sm"
              label={
                line()?.fix === "request" ? t("permission.request") : t("permission.open_settings")
              }
              onPress={runFix}
            />
          </If>
        </View>
      </If>
    </SmeltSectionCell>
  );
}

export function main() {
  const prefs = () => prefsSignal();
  const update = (patch: Partial<Prefs>) => {
    savePrefs({ ...prefsSignal(), ...patch });
  };

  return (
    <SmeltPageScroll>
      <SmeltSection title={t("section.system")}>
        <PermissionCardContent />
      </SmeltSection>

      <SmeltSection title={t("section.agent")}>
        <SmeltSectionCell>
          <SmeltSettingRow title={t("prefs.approval")}>
            <SmeltSwitch
              checked={prefs().approval}
              accessibilityLabel={t("prefs.approval")}
              onChange={(approval: boolean) => update({ approval: Boolean(approval) })}
            />
          </SmeltSettingRow>
        </SmeltSectionCell>
        <SmeltSectionCell divider>
          <SmeltSettingRow title={t("prefs.input")} description={t("prefs.input_description")}>
            <SmeltSwitch
              checked={prefs().input}
              accessibilityLabel={t("prefs.input")}
              onChange={(input: boolean) => update({ input: Boolean(input) })}
            />
          </SmeltSettingRow>
        </SmeltSectionCell>
        <SmeltSectionCell divider>
          <SmeltSettingRow title={t("prefs.success")}>
            <SmeltSwitch
              checked={prefs().success}
              accessibilityLabel={t("prefs.success")}
              onChange={(success: boolean) => update({ success: Boolean(success) })}
            />
          </SmeltSettingRow>
        </SmeltSectionCell>
        <SmeltSectionCell divider>
          <SmeltSettingRow title={t("prefs.failure")}>
            <SmeltSwitch
              checked={prefs().failure}
              accessibilityLabel={t("prefs.failure")}
              onChange={(failure: boolean) => update({ failure: Boolean(failure) })}
            />
          </SmeltSettingRow>
        </SmeltSectionCell>
      </SmeltSection>

      <SmeltSection title={t("section.terminal")}>
        <SmeltSectionCell>
          <SmeltSettingRow title={t("prefs.bell")} description={t("prefs.bell_description")}>
            <SmeltSwitch
              checked={prefs().bell}
              accessibilityLabel={t("prefs.bell")}
              onChange={(bell: boolean) => update({ bell: Boolean(bell) })}
            />
          </SmeltSettingRow>
        </SmeltSectionCell>
      </SmeltSection>
    </SmeltPageScroll>
  );
}

export default main;
