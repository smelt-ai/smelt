// 本地智能体群组插件（`smelt.groups`）对外约定。

export const OWNER = "smelt.groups";

export const MAIN_VIEW_NAME = "main";
export const MAIN_VIEW = `${OWNER}/${MAIN_VIEW_NAME}`;

export const CMD_CREATE_FOR_PROJECT = "create-for-project";
export const CMD_ADD_CONVERSATION = "add-conversation";
export const CMD_OPEN_GROUP = "open-group";

export const FULL_CMD_CREATE_FOR_PROJECT = `${OWNER}/${CMD_CREATE_FOR_PROJECT}`;
export const FULL_CMD_ADD_CONVERSATION = `${OWNER}/${CMD_ADD_CONVERSATION}`;
export const FULL_CMD_OPEN_GROUP = `${OWNER}/${CMD_OPEN_GROUP}`;

// 来自 `smelt.session-list` 的右键菜单约定（plugins/session-list/contract.js）。
export const PROJECT_MENU = "smelt.session-list/project";
export const SESSION_MENU = "smelt.session-list/conversation";

// 来自宿主的打开视图命令。
export const OPEN_VIEW_CMD = "smelt.app/open-view";

// 运行时事件话题。
export const TOPIC_GROUPS_CHANGED = "group.changed";
export const TOPIC_GROUP_MESSAGE_APPENDED = "group.message_appended";
