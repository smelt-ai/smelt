// 归档对外的约定：视图 id，布局里按它摆。改名就是丢掉用户的布局，别改。

/** 插件 id，也是下面各 id 的前缀。 */
export const OWNER = "smelt.archive";

/** 视图名：侧栏底下的归档按钮，点开是列出全部归档的弹出卡片。不带参数。 */
export const BUTTON_NAME = "button";
export const BUTTON = `${OWNER}/${BUTTON_NAME}`;
