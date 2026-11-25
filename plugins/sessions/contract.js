// 会话插件（smelt.sessions）对外约定（权威定义）。
export const OWNER = "smelt.sessions";
export const SESSION_VIEW = "smelt.sessions/session";
export const SESSION_VIEW_NAME = "session";

// 事件
export const SELECTION_CHANGED = "smelt.sessions/selection-changed";
export const CONVERSATION_CREATED = "smelt.sessions/conversation-created";

// 命令
export const ACTIVATE = "smelt.sessions/activate";
export const START_CONVERSATION = "smelt.sessions/start-conversation";
export const FORK = "smelt.sessions/fork";
export const RESUME = "smelt.sessions/resume";
export const RESUME_IN_TERMINAL = "smelt.sessions/resume-in-terminal";
export const HANDOFF = "smelt.sessions/handoff";
export const SEND_TEXT = "smelt.sessions/send-text";
export const SELECTION = "smelt.sessions/selection";
export const CREATE_CONVERSATION = "smelt.sessions/create-conversation";
export const SELECT_PROJECT = "smelt.sessions/select-project";
export const PREV_PANE = "smelt.sessions/prev-pane";
export const NEXT_PANE = "smelt.sessions/next-pane";
export const SPLIT_RIGHT = "smelt.sessions/split-right";
export const SPLIT_DOWN = "smelt.sessions/split-down";
export const CLOSE_PANE = "smelt.sessions/close-pane";
export const COPY_SESSION_ID = "smelt.sessions/copy-session-id";
