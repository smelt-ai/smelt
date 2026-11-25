// 远程机器（来源 `smelt.machines`）对外的约定：设置「远程」页的视图 id，和去添加一台远程
// 机器的命令。
//
// 这里是权威定义。插件之间不互相依赖，调用方各自照抄用到的 id 并注明出处。

export const OWNER = "smelt.machines";

/** 设置里的「远程」页：连出去的机器（添加、看状态、移除），和让别的设备连进来。不带参数。 */
export const VIEW_NAME = "remote";
export const VIEW = `${OWNER}/${VIEW_NAME}`;

/** 去添加一台远程机器：打开「远程」页，配对串在那里粘。不带参数，命令面板里也有。 */
export const ADD_MACHINE_NAME = "add-machine";
export const ADD_MACHINE = `${OWNER}/${ADD_MACHINE_NAME}`;

// 约定：smelt.session-list 的 MACHINE_MENU，权威定义在 plugins/session-list/contract.js。
export const MACHINE_MENU = "smelt.session-list/machine";

export const RECONNECT_NAME = "reconnect";
export const UPGRADE_NAME = "upgrade";
export const RENAME_NAME = "rename";
export const EDIT_RELAY_NAME = "edit-relay";
export const REMOVE_NAME = "remove";
/** 翻转本机智能体能不能经本机运行时使用这台机器（`smelt --machine`）。 */
export const AGENT_ACCESS_NAME = "agent-access";
/** 翻转这台机器能不能连进来操作这台电脑。 */
export const INBOUND_NAME = "inbound";

export const RECONNECT = `${OWNER}/${RECONNECT_NAME}`;
export const UPGRADE = `${OWNER}/${UPGRADE_NAME}`;
export const RENAME = `${OWNER}/${RENAME_NAME}`;
export const EDIT_RELAY = `${OWNER}/${EDIT_RELAY_NAME}`;
export const REMOVE = `${OWNER}/${REMOVE_NAME}`;
export const AGENT_ACCESS = `${OWNER}/${AGENT_ACCESS_NAME}`;
export const INBOUND = `${OWNER}/${INBOUND_NAME}`;
