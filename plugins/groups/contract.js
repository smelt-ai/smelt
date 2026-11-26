// 群组会话插件（来源 `smelt.groups`）对外约定与跨插件菜单 ID。
// 插件之间不互相依赖，全部通过字符串 ID 与公开 JSON 形状沟通。

export const PLUGIN_ID = "smelt.groups";

export const VIEW_NAME = "main";
export const VIEW = "smelt.groups/main";

export const SIDEBAR_NAME = "sidebar";
export const SIDEBAR_VIEW = "smelt.groups/sidebar";

export const TOPIC_GROUPS_CHANGED = "group.changed";
export const TOPIC_GROUP_MESSAGE_APPENDED = "group.message_appended";

export const NEW_GROUP_CMD = "smelt.groups/new";
export const ADD_CONVERSATION_CMD = "smelt.groups/add-conversation";

// ---- 对端插件约定（照抄自 `plugins/session-list/contract.js`） ----
export const SESSION_LIST_PROJECT_MENU = "smelt.session-list/project";
export const SESSION_LIST_CONVERSATION_MENU = "smelt.session-list/conversation";

// ---- 宿主通用命令 ----
export const OPEN_VIEW_CMD = "smelt.app/open-view";
export const NOTIFY_CMD = "smelt.app/notify";
export const SET_VIEW_BADGE_CMD = "smelt.app/set-view-badge";
