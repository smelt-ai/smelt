/// 界面用到的运行时视图模型。
///
/// 模型本身都是 Rust 内核（`crates/smelt-mobile/src/api`）经 flutter_rust_bridge
/// 生成的类，这里只做两件事：
///
///   1. 按名字转出界面要的类型，**不转出 FRB 的顶层函数**——界面调运行时一律走
///      `RuntimeService`，否则单测里换不掉 Rust 动态库。
///   2. 放几条纯展示用的派生判断（扩展方法）。协议解析、合并、排序都在 Rust，
///      这里不再出现。
library;

import '../src/rust/api/catalog.dart';
import '../src/rust/api/connection.dart' as connection;
import '../src/rust/api/conversation.dart';
import '../src/rust/api/error.dart';

export '../src/rust/api/catalog.dart'
    show
        AcpAgentOption,
        AgentDefinitionSummary,
        AttentionItem,
        AttentionKind,
        AutomationActionKind,
        AutomationCatalog,
        AutomationRunSource,
        AutomationRunStatus,
        AutomationRunSummary,
        AutomationSchedule,
        AutomationSource,
        AutomationSummary,
        CatalogEvent,
        CatalogEventKind,
        HistoryRenameResult,
        HistorySessionSummary,
        LaunchAction,
        LaunchSections,
        LaunchTarget,
        ScheduleKind,
        SessionKind,
        SessionList,
        SessionPhase,
        SessionStatus,
        SessionSummary,
        WorkspaceCatalog,
        WorkspaceProject;
// Flutter 的 widgets 库也有一个 `ConnectionState`（AsyncSnapshot 用），同名会在
// 每个界面文件里撞车，所以运行时的那个改叫 [RuntimeConnectionState]。
export '../src/rust/api/connection.dart'
    show ConnectionEvent, LinkMetrics, LinkPath, RuntimeInfo;
export '../src/rust/api/conversation.dart'
    show
        ApprovalDetails,
        ApprovalKind,
        ChoiceOption,
        ConfigOption,
        ContextUsage,
        ConversationEntry,
        ConversationEvent,
        ConversationEventKind,
        ConversationImage,
        ConversationPhase,
        ConversationView,
        DiffView,
        Elicitation,
        ElicitationField,
        ElicitationFieldKind,
        ElicitationTextInput,
        EntryKind,
        ImageData,
        ImageInput,
        ImageRef,
        ModelProvider,
        ModelSelector,
        PendingActions,
        PermissionOption,
        PermissionOptionKind,
        PermissionRequest,
        PlanStep,
        PlanStepStatus,
        SendFailureKind,
        SendMessageError,
        SessionSkill,
        SessionSkills,
        SlashCommand,
        TaskNoteKind,
        TaskNoteView,
        ToolCallView,
        ToolKind,
        ToolOutput,
        ToolOutputKind,
        ToolStatus;
export '../src/rust/api/error.dart' show SmeltError, SmeltErrorKind;
export '../src/rust/api/identity.dart' show DeviceIdentity, PairingTarget;

/// 运行时连接状态：connecting / connected / reconnecting / disconnected。
typedef RuntimeConnectionState = connection.ConnectionState;

/// 空的工作区目录：连上运行时、拿到目录之前界面用它占位。
const emptyWorkspaceCatalog = WorkspaceCatalog(
  projects: [],
  agents: [],
  sections: LaunchSections(common: [], terminal: [], conversation: []),
);

/// 运行时调用失败时给人看的一句话。`SmeltError` / `SendMessageError` 的 message
/// 本来就是给人看的；其它异常（Rust panic 之类）只能原样转字符串。
String describeRuntimeError(Object error) => switch (error) {
  SmeltError(:final message) => message,
  SendMessageError(:final message) => message,
  _ => error.toString(),
};

extension AttentionItemView on AttentionItem {
  /// 审批、输入、失败是要人处理的事；完成、响铃、通知只是告知。
  bool get requiresAction =>
      kind == AttentionKind.approval ||
      kind == AttentionKind.input ||
      kind == AttentionKind.failure;
}

extension SessionSummaryView on SessionSummary {
  /// 有归属就是智能体对话。自动化 Run 不算：它有自己的来源标注，两者在界面上
  /// 是不同的东西（一个是我和智能体聊，一个是它自己半夜跑）。
  bool get isAgentConversation =>
      automation == null && agentDefinitionId != null;

  /// 这台手机上还没读的提醒。
  bool get unread => attention != null;
}

extension HistorySessionView on HistorySessionSummary {
  /// 列表上显示的名字：用户改的名字优先，否则用 agent 自己的标题。
  String get displayTitle {
    final custom = customTitle?.trim();
    return custom == null || custom.isEmpty ? title : custom;
  }

  bool get hasCustomTitle => displayTitle != title;

  DateTime? get lastActiveAt => lastActiveAtMs == null
      ? null
      : DateTime.fromMillisecondsSinceEpoch(lastActiveAtMs!);
}

extension LaunchActionView on LaunchAction {
  bool get isConversation => target == LaunchTarget.conversation;

  /// 列表里显示成「名称 · 对话/终端」，跟桌面同一种写法。
  String get kindLabel => isConversation ? 'Conversation' : 'Terminal';
}

extension LaunchSectionsView on LaunchSections {
  bool get isEmpty =>
      common.isEmpty && terminal.isEmpty && conversation.isEmpty;
}

extension PermissionOptionView on PermissionOption {
  bool get isAllow =>
      kind == PermissionOptionKind.allowOnce ||
      kind == PermissionOptionKind.allowAlways;
  bool get isReject =>
      kind == PermissionOptionKind.rejectOnce ||
      kind == PermissionOptionKind.rejectAlways;
  bool get isAlways =>
      kind == PermissionOptionKind.allowAlways ||
      kind == PermissionOptionKind.rejectAlways;
}

extension ConversationPhaseView on ConversationPhase {
  /// 这个阶段能不能接新消息：等审批、等选择时要先把那件事处理掉。
  bool get acceptsPrompt => switch (this) {
    ConversationPhase.idle ||
    ConversationPhase.thinking ||
    ConversationPhase.executingTool ||
    ConversationPhase.succeeded ||
    ConversationPhase.failed => true,
    _ => false,
  };

  bool get isWorking =>
      this == ConversationPhase.thinking ||
      this == ConversationPhase.executingTool;
}

extension ElicitationView on Elicitation {
  /// 必填项都填好了没有。运行时的 [Elicitation.isReady] 只算它已经记下的值，
  /// 界面上还没提交的文字要用 [localTextValues] 再算一遍。
  bool isReadyWith(Map<int, String> localTextValues) {
    for (final (index, field) in fields.indexed) {
      if (!field.required_) continue;
      final ready = switch (field.kind) {
        ElicitationFieldKind.select ||
        ElicitationFieldKind.multiSelect => field.chosen.isNotEmpty,
        ElicitationFieldKind.text =>
          (localTextValues[index] ?? field.textValue)?.trim().isNotEmpty ==
              true,
        ElicitationFieldKind.externalUrl => true,
      };
      if (!ready) return false;
    }
    return true;
  }
}
