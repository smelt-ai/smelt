// 跨线类型。真源是 crates/smelt-plugin-api/src/lib.rs 的 serde 表示，这里只是
// 它在 TS 侧的投影——**改这个文件前先去看 Rust 那边**。
//
// 只声明真正跨线的形状。宿主内部类型、GUI 类型一律不进来：SDK 是协议适配层，
// 不是 Smelt 的类型镜像。
//
// 防漂移不靠这个文件，靠 crates/smelt-plugin-host/tests/bun_runtime.rs 里那组
// 拿真实宿主跑的往返用例。类型声明写错了编译器不会知道，行为对不上测试会红。

/// 帧大小上限，与 `PLUGIN_WIRE_MAX_LINE_BYTES` 对齐。
///
/// 是 32MB 不是 1MB：invocation 的 payload 会带 base64 图片，Rust 侧有一条
/// 专门发 10MB 图片的用例。按 1MB 截会在正常使用中偶发炸掉。
export const PLUGIN_WIRE_MAX_LINE_BYTES = 32 * 1024 * 1024;

export const PLUGIN_API_VERSION = 1;

// Rust 侧这些都是 `#[serde(transparent)]` 的 newtype，线上就是字符串。
export type PluginId = string;
export type Capability = string;
export type ContributionId = string;
export type InvocationId = string;
export type InvocationOperation = string;
export type PluginResourceId = string;
export type PluginResourceType = string;

export interface InvocationRequest {
  invocation_id: InvocationId;
  contribution_id: ContributionId;
  operation: InvocationOperation;
  payload: unknown;
  /// Unix 毫秒。宿主也在计时，过期后它不再等这次调用的结果。
  deadline_ms: number;
}

export type InvocationErrorCode =
  | "invalid_request"
  | "rejected"
  | "conflict"
  | "internal";

export type InvocationResponse =
  | { status: "success"; invocation_id: InvocationId; result: unknown }
  | {
      status: "error";
      invocation_id: InvocationId;
      code: InvocationErrorCode;
      message: string;
      retryable: boolean;
      details?: unknown;
    };

export type SettingsActionStyle = "default" | "primary" | "danger";
export type SettingsTone = "neutral" | "positive" | "warning" | "negative";

export interface SettingsActionView {
  id: ContributionId;
  label?: string;
  command: ContributionId;
  payload?: Record<string, unknown>;
  style?: SettingsActionStyle;
  disabled?: boolean;
}

export interface SettingsAvatarRef {
  command: ContributionId;
  revision: string;
}

export interface SettingsAvatarData {
  mime: "image/png" | "image/jpeg" | "image/webp" | "image/gif";
  data_base64: string;
}

export type SettingsItemView =
  | { type: "header"; id: ContributionId; title: string }
  | {
      type: "account";
      id: ContributionId;
      title: string;
      signed_in?: boolean;
      display_name?: string;
      detail?: string;
      message?: string;
      tone?: SettingsTone;
      avatar?: SettingsAvatarRef;
      actions?: SettingsActionView[];
    }
  | {
      type: "status";
      id: ContributionId;
      title: string;
      text: string;
      tone?: SettingsTone;
      description?: string;
      actions?: SettingsActionView[];
    }
  | {
      type: "text";
      id: ContributionId;
      title: string;
      value: string;
      description?: string;
      copyable?: boolean;
    }
  | {
      type: "toggle";
      id: ContributionId;
      title: string;
      value: boolean;
      description?: string;
      action: SettingsActionView;
      disabled?: boolean;
    }
  | {
      type: "actions";
      id: ContributionId;
      title?: string;
      description?: string;
      actions: SettingsActionView[];
    };

export interface SettingsSectionView {
  revision?: number;
  items?: SettingsItemView[];
}

export interface ResourceCreatorTargetView {
  id: string;
  label: string;
}

export interface ResourceCreatorView {
  revision?: number;
  available?: boolean;
  unavailable_message?: string;
  targets?: ResourceCreatorTargetView[];
  default_target_id?: string;
}

export interface ResourceCreateParams {
  submission_id: string;
  target_id: string;
  title: string;
  description?: string;
}

export interface ResourceCreateResult {
  resource_id: string;
  message: string;
}

export type EntityDecorationTone =
  | "neutral"
  | "accent"
  | "positive"
  | "warning"
  | "negative";

export interface EntityDecorationItemView {
  resource_id: PluginResourceId;
  badge?: string;
  tone?: EntityDecorationTone;
  tooltip?: string;
  external_url?: string;
}

export interface EntityDecorationView {
  revision?: number;
  items?: EntityDecorationItemView[];
}

export type PluginUiContribution =
  | {
      type: "tool_panel";
      id: ContributionId;
      title: string;
      entry: string;
    }
  | {
      type: "settings_section";
      id: ContributionId;
      title: string;
      description?: string;
      snapshot: ContributionId;
    }
  | {
      type: "sidebar_account";
      id: ContributionId;
      settings_section: ContributionId;
      account_item: ContributionId;
    }
  | {
      type: "resource_creator";
      id: ContributionId;
      title: string;
      description?: string;
      target_label: string;
      target_placeholder: string;
      submit_label: string;
      snapshot: ContributionId;
      submit: ContributionId;
    }
  | {
      type: "entity_decoration";
      id: ContributionId;
      resource_type: string;
      icon?: string;
      snapshot: ContributionId;
    };

export interface PluginUiManifest {
  contributions?: PluginUiContribution[];
}

export type PluginInputContribution = {
  type: "input_route";
  id: ContributionId;
  operation: InvocationOperation;
};

export interface PluginInputManifest {
  contributions?: PluginInputContribution[];
}

export type SessionActionLocation = "session_menu" | "project_menu";
export type SessionActionIcon = "check" | "external_link";
export type SessionActionResult = "ignore" | "open_external";

export type PluginAgentContribution =
  | {
      type: "agent";
      id: ContributionId;
      name: string;
      icon?: string;
      controller: ContributionId;
    }
  | {
      type: "session_controller";
      id: ContributionId;
      input_route: ContributionId;
    }
  | {
      type: "session_action";
      id: ContributionId;
      title: string;
      operation: InvocationOperation;
      controller: ContributionId;
      locations: SessionActionLocation[];
      icon?: SessionActionIcon;
      result?: SessionActionResult;
    };

export interface PluginAgentManifest {
  contributions?: PluginAgentContribution[];
}

export interface PluginResourceRef {
  plugin_id: PluginId;
  resource_type: PluginResourceType;
  resource_id: PluginResourceId;
}

export interface PluginContributionRef {
  plugin_id: PluginId;
  contribution_id: ContributionId;
}

export interface AgentSessionBinding {
  agent: PluginContributionRef;
  controller: PluginContributionRef;
  instance: PluginResourceRef;
}

export interface PluginInputRouteBinding {
  contribution_id: ContributionId;
  context?: unknown;
}

export interface ConversationInputImage {
  mime: string;
  data_base64: string;
}

export interface InputRouteInvocationPayload {
  submission_id?: string;
  context: unknown;
  agent_preset?: string;
  text: string;
  images?: ConversationInputImage[];
}

export interface SessionActionInvocationPayload {
  agent_session: AgentSessionBinding;
  context?: unknown;
}

export interface PluginAgentSessionSpec {
  agent_id: ContributionId;
  instance: PluginResourceRef;
  context?: unknown;
}

/**
 * The deliberately narrow context exposed to a trusted module loaded by the shared Bun Host.
 * `dataDir` is the host-derived directory reserved for this package. It carries no daemon
 * credential, socket, or capability handle; shared Bun is a process-consolidation mechanism,
 * not a sandbox.
 */
export interface SharedPluginContext {
  pluginId: PluginId;
  dataDir: string;
}

/** The default export contract for a plugin package. */
export interface SharedPlugin {
  invoke(request: InvocationRequest, context: SharedPluginContext): unknown | Promise<unknown>;
}

/** Lets a shared module return a structured invocation failure. */
export class InvocationFailure extends Error {
  constructor(
    readonly code: InvocationErrorCode,
    message: string,
    readonly retryable = false,
    readonly details?: unknown,
  ) {
    super(message);
    this.name = "InvocationFailure";
  }
}
