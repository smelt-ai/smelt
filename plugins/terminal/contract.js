// 终端设置与主题配色同步插件（来源 `smelt.terminal`）。
// 终端画布本身由原子能力 `<SmeltNativeTerminal>`（`crates/smelt-terminal`）直接提供给会话视图使用。

export const OWNER = "smelt.terminal";
export const SETTINGS_VIEW_NAME = "settings";
export const SETTINGS_VIEW = `${OWNER}/${SETTINGS_VIEW_NAME}`;
export const MENU = `${OWNER}/menu`;

export const DEFAULT_FONT_PX = 13;
export const MIN_FONT_PX = 9;
export const MAX_FONT_PX = 22;
