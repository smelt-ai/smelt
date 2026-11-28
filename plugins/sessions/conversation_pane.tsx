// 会话内对话分屏格：直接嵌入通用 SDK 能力 `<SmeltConversation>`（零跨 VM 插槽、零 Rust UI 视图），
// 负责查找工作区目录里的 `WorkspaceAcp`、读写 `smelt.agents` 记住的配置、响应 `pi_config.changed`
// 模型刷新、以及承载 `smelt.sessions/send-text` 直投。
import { cleanup, computed, effect, signal, Text, untracked, View } from "nsh";
import {
  machineMetaEntry,
  rememberAcpConfigValue,
  rememberedAcpConfig,
  run,
  sessionStates,
  stableMemo,
  currentTheme,
  t,
  workspaceDirectory,
} from "smelt/sdk";
import { SmeltConversation, type SmeltConversationApi } from "smelt/conversation";
import { machineKey, terminalPaneKey } from "./model.js";

const OPEN_VIEW_CMD = "smelt.app/open-view";
const AGENT_SETTINGS_CMD = "smelt.agents/agent-settings";
const EDIT_AGENT_SETTINGS_CMD = "smelt.agents/edit-agent-settings";
const AGENTS_VIEW = "smelt.agents/main";
const CAPABILITIES_VIEW = "smelt.capabilities/main";
const REVEAL_IN_LIST_CMD = "smelt.session-list/reveal-in-list";


// ---- 外部直投文本注册表（对齐 `terminal_pane.tsx`） ----

const openConversations = new Map<string, Set<(text: string) => void>>();

function registerOpenConversation(key: string, insert: (text: string) => void): () => void {
  let set = openConversations.get(key);
  if (!set) {
    set = new Set();
    openConversations.set(key, set);
  }
  set.add(insert);
  return () => {
    const cur = openConversations.get(key);
    if (!cur) return;
    cur.delete(insert);
    if (cur.size === 0) openConversations.delete(key);
  };
}

export function sendTextToConversation(machine: string | null, sessionId: string, text: string) {
  const set = openConversations.get(terminalPaneKey(machine, sessionId));
  if (!set || set.size === 0) {
    throw new Error(t("error.not_open"));
  }
  for (const insert of set) {
    insert(text);
    return;
  }
}

// ---- `smelt.agents` 智能体配置只读缓存 & Pi 模型目录刷新信号 ----

const agentSettingsByMachine = new Map<string, any>();
const agentSettingsRev = signal(0);
const piConfigSeq = signal(0);
let settingsFetchState: "idle" | "loading" | "done" = "idle";

export function ensureAgentSettingsLoaded() {
  if (settingsFetchState !== "idle") return;
  settingsFetchState = "loading";
  run(AGENT_SETTINGS_CMD, null).then(
    (list: any) => {
      if (Array.isArray(list)) {
        for (const item of list) {
          const mk = machineKey(item?.machine);
          if (item?.state === "ready" && item?.settings && !agentSettingsByMachine.has(mk)) {
            agentSettingsByMachine.set(mk, item.settings);
          }
        }
      }
      settingsFetchState = "done";
      agentSettingsRev.set(agentSettingsRev() + 1);
    },
    () => {
      settingsFetchState = "idle";
    },
  );
}

export function applyAgentSettingsChanged(payload: any) {
  if (!payload || typeof payload !== "object") return;
  const mk = machineKey(payload.machine);
  if (payload.state === "ready" && payload.settings) {
    agentSettingsByMachine.set(mk, payload.settings);
  } else {
    agentSettingsByMachine.delete(mk);
  }
  agentSettingsRev.set(agentSettingsRev() + 1);
}

export function notifyPiConfigChanged() {
  piConfigSeq.set(piConfigSeq() + 1);
}

function initialConfigFor(machine: string | null, agentId: string): Array<[string, string]> {
  void agentSettingsRev();
  // 运行时给的设置读不懂（两边版本不一致）只是铺不上记住的选项，对话照开。
  try {
    return rememberedAcpConfig(agentSettingsByMachine.get(machineKey(machine)), agentId);
  } catch (err) {
    console.error("[sessions] 读不懂智能体设置，不铺记住的选项：", err);
    return [];
  }
}

function rememberConfigValue(
  machine: string | null,
  agentId: string,
  configId: string,
  valueId: string,
) {
  const mk = machineKey(machine);
  const before = agentSettingsByMachine.get(mk);
  if (!before) return;
  // 记不记、怎么去重按运行时同一份规则（Pi 的默认模型与推理档位归引擎配置，不记）。
  let after: unknown;
  try {
    after = rememberAcpConfigValue(before, agentId, configId, valueId);
  } catch (err) {
    console.error("[sessions] 读不懂智能体设置，这次选择没记住：", err);
    return;
  }
  if (!after) return;
  void run(EDIT_AGENT_SETTINGS_CMD, {
    machine: machine ?? null,
    before,
    after,
  })
    .then((updated: any) => {
      if (updated && typeof updated === "object") {
        agentSettingsByMachine.set(mk, updated);
        agentSettingsRev.set(agentSettingsRev() + 1);
      }
    })
    .catch((err) => console.error("[sessions] 记住会话选项失败：", err));
}

function isBackgroundAcpId(sessionId: string): boolean {
  return sessionId.startsWith("acp-automation-");
}

export function ConversationPane(props: {
  args: {
    machine?: string | null;
    session_id?: string;
    placeholder?: string | null;
    start?: { fork?: any; prefill?: string | null } | null;
  } | null;
  many: boolean;
  visible?: boolean;
  safeArea: any;
  reuseKey?: string;
  focusSeq?: number;
  onFocusIn?: () => void;
  onAtLatestChange: (machine: string | null, sessionId: string, atLatest: boolean) => void;
  onFork: (
    machine: string | null,
    sessionId: string,
    req: { provider_session_id: string; cut: any; prompt: string },
  ) => void;
  onNewSession: (machine: string | null, acp: any) => void;
  onOpenConversation: (machine: string | null, sid: string) => void;
}) {
  const look = currentTheme;
  const conversation = signal<SmeltConversationApi | null>(null);
  /** 对话还没挂上（目录里还没有这段会话）时来的文本：挂上后补进输入框，只留最后一段。 */
  let pendingInsert: string | null = null;

  const machine = () => props.args?.machine ?? null;
  const sessionId = () => (typeof props.args?.session_id === "string" ? props.args.session_id : "");

  effect(() => {
    ensureAgentSettingsLoaded();
  });

  effect(() => {
    const sid = sessionId();
    if (!sid) return;
    const key = terminalPaneKey(machine(), sid);
    const unreg = registerOpenConversation(key, (chunk) => {
      const api = untracked(conversation);
      if (api) api.insertText(chunk);
      else pendingInsert = chunk;
    });
    cleanup(() => {
      unreg();
      // 等着的那段是给这段会话的，换了会话不带过去。
      pendingInsert = null;
    });
  });

  // 渲染完再补：这时输入框已在窗口里，聚焦才生效。
  effect(() => {
    const api = conversation();
    if (!api || pendingInsert == null) return;
    const text = pendingInsert;
    pendingInsert = null;
    untracked(() => api.insertText(text));
  });

  const directory = computed(() => workspaceDirectory(machine()));
  const directoryAcp = computed(() => {
    const sid = sessionId();
    if (!sid) return null;
    const convs = directory()?.conversations ?? [];
    for (const c of convs) {
      if (c?.acp?.sid === sid) return c.acp;
    }
    return null;
  });

  const embedded = computed(() => {
    const acp = directoryAcp();
    const sid = sessionId();
    return Boolean(props.many || acp?.automation_id || isBackgroundAcpId(sid));
  });

  const floatingTitle = computed(() => {
    if (embedded()) return null;
    const sid = sessionId();
    const m = machine();
    const st = sessionStates(m).find((s: any) => s?.id === sid);
    const baseName =
      typeof st?.conversation_name === "string" && st.conversation_name.trim()
        ? st.conversation_name.trim()
        : t("kind.conversation");
    if (m == null) return baseName;
    const entry = machineMetaEntry(m);
    const mName = entry?.name ? String(entry.name) : m;
    return `${baseName} · ${mName}`;
  });

  const initialConfig = computed(() => {
    const acp = directoryAcp();
    if (!acp) return [];
    return initialConfigFor(machine(), String(acp.agent ?? "claude"));
  });

  return (
    <View style={{ position: "relative", flex: 1, overflow: "hidden" }}>
      <If
        when={directoryAcp() != null}
        else={
          <View
            style={{
              flex: 1,
              width: "100%",
              height: "100%",
              alignItems: "center",
              justifyContent: "center",
              padding: 20,
            }}
          >
            <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
              {directory() != null && props.args?.start == null
                ? t("error.session_gone")
                : (props.args?.placeholder ?? t("kind.conversation"))}
            </Text>
          </View>
        }
      >
        <SmeltConversation
          machine={machine()}
          sessionId={sessionId()}
          active={props.visible !== false}
          directoryAcp={directoryAcp()}
          start={props.args?.start ?? null}
          initialConfig={initialConfig()}
          placeholder={props.args?.placeholder ?? null}
          embedded={embedded()}
          title={floatingTitle()}
          safeArea={props.safeArea}
          reuseKey={props.reuseKey}
          focusSeq={props.focusSeq ?? 0}
          conversationRef={(api) => conversation.set(api)}
          refreshModelsSeq={piConfigSeq()}
          onAtLatestChange={(atLatest) => props.onAtLatestChange(machine(), sessionId(), atLatest)}
          onFocusIn={props.onFocusIn}
          onFork={(req) => props.onFork(machine(), sessionId(), req)}
          onNewSession={() => {
            const acp = directoryAcp();
            if (acp) props.onNewSession(machine(), acp);
          }}
          onOpenConversation={(sid) => props.onOpenConversation(machine(), sid)}
          onManageCapabilities={(byAgent) => {
            const acp = directoryAcp();
            if (byAgent) {
              const args: Record<string, unknown> = { machine: machine() };
              if (acp?.agent_definition_id) args.agent = acp.agent_definition_id;
              void run(OPEN_VIEW_CMD, { view: AGENTS_VIEW, args }).catch(() => {});
            } else {
              void run(OPEN_VIEW_CMD, {
                view: CAPABILITIES_VIEW,
                args: { tab: "skills", machine: machine() },
              }).catch(() => {});
            }
          }}
          onOpenAgentMemory={(agentId) => {
            void run(OPEN_VIEW_CMD, {
              view: AGENTS_VIEW,
              args: { machine: machine(), agent: agentId, tab: "memory" },
            }).catch(() => {});
          }}
          onConfigSelected={(agentId, configId, valueId) =>
            rememberConfigValue(machine(), agentId, configId, valueId)
          }
          onRevealInList={() => {
            void run(REVEAL_IN_LIST_CMD, {
              machine: machine(),
              session_id: sessionId(),
            }).catch(() => {});
          }}
        />
      </If>
    </View>
  );
}
