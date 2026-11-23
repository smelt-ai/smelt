// 能力库插件（来源 `smelt.capabilities`）对外约定（权威定义）：
// 能力库页（技能、扩展、MCP）、模型设置页、记忆设置页的视图 id，以及机器菜单打开命令。
// 插件之间不互相依赖，调用方照抄用到的 id 与 JSON 形状并注明出处。

export const PLUGIN_ID = "smelt.capabilities";

export const VIEW_NAME = "main";
/**
 * 能力库页视图 id：给 Pi 装的东西（技能、扩展、MCP）。
 * 打开参数（均可选）：
 * `{ machine?: string | null, tab?: "skills" | "extensions" | "mcp" }`
 * `machine` 不给或已移除时停在本机。
 */
export const VIEW = "smelt.capabilities/main";

export const MODELS_VIEW_NAME = "models";
/**
 * 设置 → 模型视图 id：Provider 凭据、默认模型、思考强度。
 * 打开参数（可选）：`{ machine?: string | null }`
 */
export const MODELS_VIEW = "smelt.capabilities/models";

export const MEMORY_VIEW_NAME = "memory";
/**
 * 设置 → 记忆视图 id：记忆检索的重排模型与向量服务。
 * 打开参数（可选）：`{ machine?: string | null }`
 */
export const MEMORY_VIEW = "smelt.capabilities/memory";

export const OPEN_ON_MACHINE_NAME = "open-on-machine";
/**
 * 在指定机器上打开能力库页。
 * 参数：`{ machine: string | null }`
 */
export const OPEN_ON_MACHINE = "smelt.capabilities/open-on-machine";

/** 订阅的运行时话题：某台机器的 Pi 配置发生变化。 */
export const TOPIC_PI_CONFIG_CHANGED = "pi_config.changed";

// ---- 外部插件约定（照抄，不依赖提供方源码） ----

/** 约定：smelt.session-list 的机器右键菜单，权威定义在 `plugins/session-list/contract.js`。 */
export const MACHINE_MENU = "smelt.session-list/machine";

/** 约定：smelt.agents 的配置读/订/改契约，权威定义在 `plugins/agents/contract.js`。 */
export const AGENT_SETTINGS = "smelt.agents/agent-settings";
export const AGENT_SETTINGS_CHANGED = "smelt.agents/agent-settings-changed";
export const EDIT_AGENT_SETTINGS = "smelt.agents/edit-agent-settings";
