// 自动化插件（来源 `smelt.automations`）对外约定（权威定义）以及所调用的对端插件约定。
// 插件之间不互相依赖，跨插件调用均以字符串 ID 与公开 JSON 形状约定沟通。

export const PLUGIN_ID = "smelt.automations";

export const VIEW_NAME = "main";

/**
 * 自动化主视图 id。
 * 打开参数（均可选，不带参数时停留在当前页面状态）：
 * - `{ machine?: string | null, automation: string }`: 打开该条自动化的编辑器
 * - `{ machine?: string | null, automation: string, run: string }`: 打开该条自动化并选中该次运行
 * - `{ machine?: string | null, create_for_agent: string }`: 在指定机器上为该智能体新建自动化
 */
export const VIEW = "smelt.automations/main";

/** 订阅的运行时话题（权威定义在 `smelt-model/src/daemon_state.rs`）。 */
export const TOPIC_AUTOMATIONS_CHANGED = "automations.changed";

// ---- 对端插件约定（照抄自提供方权威定义） ----

/** 约定：`smelt.agents` 的智能体主视图（`plugins/agents/contract.js`）。 */
export const AGENTS_VIEW = "smelt.agents/main";

/** 约定：`smelt.agents` 读取各台机器智能体配置的命令（`plugins/agents/contract.js`）。 */
export const AGENT_SETTINGS_CMD = "smelt.agents/agent-settings";

/** 约定：`smelt.agents` 智能体配置变化事件（`plugins/agents/contract.js`）。 */
export const AGENT_SETTINGS_CHANGED_EVENT = "smelt.agents/agent-settings-changed";

// ---- 宿主通用命令 ----

export const OPEN_VIEW_CMD = "smelt.app/open-view";
export const NOTIFY_CMD = "smelt.app/notify";
export const WITHDRAW_NOTIFICATION_CMD = "smelt.app/withdraw-notification";
export const SET_VIEW_BADGE_CMD = "smelt.app/set-view-badge";
export const PICK_PATHS_CMD = "smelt.app/pick-paths";
