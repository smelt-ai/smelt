// 额度对外的约定：视图 id，布局里按它摆。改名就是丢掉用户的布局，别改。

/** 插件 id，也是下面各 id 的前缀。 */
export const OWNER = "smelt.quota";

/** 视图名：账户额度那一条，每家一枚额度环，点开看全部、点哪家查哪家。不带参数，摆在哪归布局。 */
export const BAR_NAME = "bar";
export const BAR = `${OWNER}/${BAR_NAME}`;

/** 视图名：设置里的「账户额度」页，每家查不查。 */
export const SETTINGS_NAME = "settings";
export const SETTINGS_VIEW = `${OWNER}/${SETTINGS_NAME}`;
