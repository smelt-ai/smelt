// 通知插件（来源 `smelt.notifications`）与外部命令约定。

export const OWNER = "smelt.notifications";
export const VIEW_NAME = "main";
export const VIEW = `${OWNER}/${VIEW_NAME}`;

export const METHOD_MARK_READ = "attention.mark_read";

// 约定：smelt.sessions 的 ACTIVATE（权威定义在 plugins/sessions/contract.js）。
export const ACTIVATE_SESSION = "smelt.sessions/activate";

// 宿主的能力命令（smelt.app/*）。
export const SHOW_MAIN_WINDOW = "smelt.app/show-main-window";
export const SET_DOCK_BADGE = "smelt.app/set-dock-badge";
export const NOTIFY = "smelt.app/notify";
export const WITHDRAW_NOTIFICATION = "smelt.app/withdraw-notification";
export const REQUEST_NOTIFICATION_PERMISSION = "smelt.app/request-notification-permission";
export const OPEN_NOTIFICATION_SETTINGS = "smelt.app/open-notification-settings";
