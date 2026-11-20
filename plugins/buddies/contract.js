// 用到的别的插件的约定，照抄自它们的 contract.js，权威定义以那边为准。

/**
 * 会话列表（smelt.session-list）的 SESSION_AVATAR：每行行首的头像插槽，每行一份实例，参数
 * `{ machine, conversation }`（conversation 是工作区目录里那段会话的 id），第一次打开给，之后不换
 * 会话。画在 36pt 见方的格子里，零件可以伸出去一点；右下角的小圆标归会话列表画。
 */
export const SESSION_AVATAR = "smelt.session-list/session-avatar";

/** 本插件的头像视图（填 SESSION_AVATAR）。 */
export const AVATAR_NAME = "avatar";
