// 智能体插件（来源 `smelt.agents`）对外约定（权威定义）：
// 智能体页与 Agent 设置页的视图 id、新对话命令、各台机器的智能体配置读/订/改契约。
// 插件之间不互相依赖，调用方照抄用到的 id 与 JSON 形状并注明出处。

export const PLUGIN_ID = "smelt.agents";

/**
 * 智能体页视图 id。
 * 打开参数（均可选）：
 * `{ machine?: string | null, agent?: string, tab?: "config" | "automations" | "memory", new_conversation?: boolean }`
 */
export const VIEW = "smelt.agents/main";

/** 设置里的「Agent」页视图 id（不带参数）。 */
export const SETTINGS_VIEW = "smelt.agents/settings";

/**
 * 跟默认智能体开一段新对话；没有默认就打开智能体页选，一个都没有就去建。
 * 不带参数，标题随默认智能体名称动态更新（⌘N）。
 */
export const NEW_CONVERSATION = "smelt.agents/new-conversation";

/**
 * 读取各台机器眼下的智能体配置（仅含已收到推送的机器，不含 `absent`）。
 * 返回：`Array<{ machine: string | null, state: "ready", settings: AgentHostSettings } | { machine: string | null, state: "unavailable", reason: string }>`
 */
export const AGENT_SETTINGS = "smelt.agents/agent-settings";

/**
 * 一台机器的智能体配置发生变化（整份替换、不可用或机器移除）。
 * 载荷：`{ machine: string | null, state: "ready" | "unavailable" | "absent", settings?, reason? }`
 */
export const AGENT_SETTINGS_CHANGED = "smelt.agents/agent-settings-changed";

/**
 * 修改一台机器的智能体配置，交由该机器运行时合并后返回最新整份配置。
 * 参数：`{ machine: string | null, before: AgentHostSettings, after: AgentHostSettings }`
 */
export const EDIT_AGENT_SETTINGS = "smelt.agents/edit-agent-settings";
