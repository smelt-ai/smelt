// 会话列表用到的别的插件的约定：本插件用到的 id 和参数形状，照抄自权威定义，形状必须和那边一致。
// 插件之间不互相 import，只经命令、事件、视图 id 打交道。

// ---- 会话（smelt.sessions，crates/plugins/sessions/src/contract.rs） ----
// 会话视图、选中哪段会话或哪个项目、把新会话写进目录都归那边，列表只画、只发请求。

/**
 * 会话视图：选中的那段会话。open-view 带 { machine, conversation_id }（或 session_id，可带 after：
 * 刚写进目录时那次调用回的目录版本）是选中那段、焦点给它；不带参数只是露出来。
 */
export const SESSION = "smelt.sessions/session";
/**
 * 眼下选中的是什么：{ kind: "none" } | { kind: "conversation", machine, conversation_id } |
 * { kind: "project", machine, root }。列表起来时问一次，之后跟着 SELECTION_CHANGED。
 */
export const SELECTION = "smelt.sessions/selection";
/** 选中变了（载荷同 SELECTION）。又选了一次同一段也发：它可能折在列表里，列表据此露出它。 */
export const SELECTION_CHANGED = "smelt.sessions/selection-changed";
/** 一段会话刚写进目录、还没推回来：{ machine, conversation_id }。列表等它出现时播一次落进来的动画。 */
export const CONVERSATION_CREATED = "smelt.sessions/conversation-created";
/** 把一段新会话写进那台机器的目录再打开过去：{ machine, conversation }（目录里那一条，id 先分好）。 */
export const CREATE_CONVERSATION = "smelt.sessions/create-conversation";
/** 选中一个项目：{ machine, root, after? }。目录里没有是失败；刚打开的带上打开那次回的目录版本。 */
export const SELECT_PROJECT = "smelt.sessions/select-project";

// ---- 智能体（smelt.agents，crates/plugins/agents/src/contract.rs） ----

/**
 * 各台机器眼下的智能体配置，只含收到过推送的机器：[{ machine, state: "ready", settings } |
 * { machine, state: "unavailable", reason } | { machine, state: "absent" }]。
 */
export const AGENT_SETTINGS = "smelt.agents/agent-settings";
/** 一台机器的智能体配置换了（整份，形状同 AGENT_SETTINGS 的一项），或那台机器给不了、移除了。 */
export const AGENT_SETTINGS_CHANGED = "smelt.agents/agent-settings-changed";

// ---- 变更（smelt.git，crates/plugins/git/src/contract.rs） ----

/** 在浏览器里打开那个仓库当前分支的新建 PR 页：{ machine, root }。开不了时命令失败，原因给人看。 */
export const OPEN_PULL_REQUEST = "smelt.git/open-pull-request";
