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

export interface SmeltConversationProps {
  machine: string | null;
  sessionId: string;
  directoryAcp?: any | null;
  spec?: SmeltConversationSpec | null;
  start?: SmeltConversationStart | null;
  initialConfig?: Array<[string, string]>;
  placeholder?: string | null;
  embedded?: boolean;
  title?: string | null;
  safeArea?: {
    title_bar?: boolean;
    top_left?: number;
    top_right?: number;
  } | null;
  reuseKey?: string;
  focusSeq?: number;
  insertText?: { seq: number; text: string } | null;
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
