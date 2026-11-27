import type { JSX, StyleProp, ViewStyle } from "../nsh/types";

export interface SmeltConversationStart {
  fork?: {
    provider_session_id: string;
    cut?: any;
  } | null;
  prefill?: string | null;
}

export interface SmeltConversationSpec {
  agent: string;
  cwd: string;
  binding?: any;
}

/** `<SmeltConversation>` 的命令式接口（`conversationRef` 给出）。 */
export interface SmeltConversationApi {
  /** 往输入框里追加文本（如 `smelt.sessions/send-text`）：接在草稿后面，光标放到末尾并聚焦输入框。 */
  insertText(text: string): void;
}

export interface SmeltConversationProps {
  machine: string | null;
  sessionId: string;
  directoryAcp?: any | null;
  spec?: SmeltConversationSpec | null;
  start?: SmeltConversationStart | null;
  initialConfig?: Array<[string, string]>;
  placeholder?: string | null;
  /** 嵌入模式（如自动化运行页）：隐藏浮动标题胶囊。 */
  embedded?: boolean;
  /** 是否可见（在 `display: none` 隐藏会话或非活动页时传 false，暂停所有旋转动画）。 */
  active?: boolean;
  title?: string | null;
  safeArea?: {
    title_bar?: boolean;
    top_left?: number;
    top_right?: number;
  } | null;
  reuseKey?: string;
  focusSeq?: number;
  /** 挂上时给出命令式接口，卸下时给 `null`。 */
  conversationRef?: (api: SmeltConversationApi | null) => void;
  refreshModelsSeq?: number;
  readOnly?: boolean;
  style?: StyleProp<ViewStyle>;
  onAtLatestChange?: (atLatest: boolean) => void;
  onFocusIn?: () => void;
  onFork?: (req: { provider_session_id: string; cut: any; prompt: string }) => void;
  onNewSession?: () => void;
  onOpenConversation?: (sid: string) => void;
  onManageCapabilities?: (byAgent: boolean) => void;
  onOpenAgentMemory?: (agentId: string) => void;
  onConfigSelected?: (agentId: string, configId: string, valueId: string) => void;
  onRevealInList?: () => void;
}

export declare function SmeltConversation(props: SmeltConversationProps): JSX.Element;
