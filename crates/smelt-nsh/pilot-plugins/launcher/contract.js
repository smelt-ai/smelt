// 启动栏对外的约定：两个视图的 id，布局里按它摆。改名就是丢掉用户的布局，别改。

/** 插件 id，也是下面各 id 的前缀。 */
export const OWNER = "smelt.launcher";

/** 视图名：新对话按钮和入口列表。不带参数，高度跟着内容，布局给它一格按内容定高的位置。 */
export const NAV_NAME = "nav";
export const NAV = `${OWNER}/${NAV_NAME}`;
/** 视图名：「设置」按钮，有设置页挂着角标时带红点。不带参数。 */
export const SETTINGS_NAME = "settings";
export const SETTINGS_VIEW = `${OWNER}/${SETTINGS_NAME}`;
