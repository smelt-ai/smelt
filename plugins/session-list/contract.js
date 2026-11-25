// 会话列表对外的约定：视图、插槽、菜单、命令的 id 和参数。别的插件照抄这里的 id 和形状，
// 改名就是丢掉用户的快捷键和布局，别改。
//
// 机器在线上一律是 null（本机）或远程机器的 runtime id。

/** 插件 id，也是下面各 id 的前缀。 */
export const OWNER = "smelt.session-list";

// ---- 视图 ----

/** 视图名：会话列表，单例，摆在布局里。打开参数 `{ focus?: boolean }`：focus 把键盘焦点交给列表。 */
export const LIST_NAME = "main";
export const LIST = `${OWNER}/${LIST_NAME}`;
/** 设置页（「会话列表」：行首显示头像还是图标）。 */
export const SETTINGS_NAME = "settings";

// ---- 右键菜单 ----

/** 项目行的右键菜单。上下文 `{ machine, path }`（path 是项目根目录）。 */
export const PROJECT_MENU = `${OWNER}/project`;
/** 机器行的右键菜单。上下文 `{ machine }`。 */
export const MACHINE_MENU = `${OWNER}/machine`;
/**
 * 对话行的右键菜单，别的插件登记的项排在自带的「置顶」「改名」后面。上下文
 * `{ machine, conversation, session_id }`（session_id 是领头那一格的守护会话 id，还没起过是 null）。
 */
export const SESSION_MENU = `${OWNER}/conversation`;

// ---- 带参数的命令（给别的插件调） ----

/**
 * 在会话列表里找到这段会话：侧栏收着就打开，展开它所在的组，滚过去并闪一下。不换选中、不动
 * 焦点。参数 `{ machine, session_id }`（守护会话 id）。会话不在列表里是失败。
 */
export const REVEAL_IN_LIST = `${OWNER}/reveal-in-list`;

// ---- 不带参数的命令（快捷键、命令面板） ----

/** 新建会话（打开新建会话的菜单）。 */
export const NEW_SESSION = `${OWNER}/new-session`;
/** 打开项目（选一个目录）。 */
export const OPEN_PROJECT = `${OWNER}/open-project`;
/** 焦点移到会话列表。 */
export const FOCUS_LIST = `${OWNER}/focus-list`;
/** 选中列表里的上一段、下一段会话。 */
export const PREV_SESSION = `${OWNER}/prev`;
export const NEXT_SESSION = `${OWNER}/next`;
