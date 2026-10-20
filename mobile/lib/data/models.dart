/// 内核类型的统一出口，加几处纯展示用的扩展。
///
/// 这里只做「同一份数据怎么说给人听」：状态记号、链路说法、标题取舍。哪些会话算在等你、
/// 怎么排序，都在 Rust 的 Pass 投影里，这里不重复判断。
library;

import '../l10n/gen/l10n.dart';
import '../src/rust/api/catalog.dart';
import '../src/rust/api/conversation.dart';
import '../src/rust/api/error.dart';
import '../src/rust/api/fleet.dart';
import '../ui/agent_glyph.dart';
import '../ui/status.dart';

export '../src/rust/api/catalog.dart';
export '../src/rust/api/conversation.dart';
export '../src/rust/api/draft.dart';
export '../src/rust/api/error.dart';
export '../src/rust/api/fleet.dart';
export '../src/rust/api/identity.dart';
export '../src/rust/api/pass.dart';
export '../src/rust/api/terminal.dart';

/// 空的工作区目录：拿到运行时的目录之前占位。
const emptyWorkspaceCatalog = WorkspaceCatalog(
  projects: [],
  agents: [],
  sections: LaunchSections(common: [], terminal: [], conversation: []),
);

/// 内核报错给人看的一句话。`SmeltError` / `SendMessageError` 的 message 本来就是给人看的。
String describeError(Object error) => switch (error) {
  SmeltError(:final message) => message,
  SendMessageError(:final message) => message,
  _ => error.toString(),
};

extension SessionSummaryView on SessionSummary {
  /// 这条会话在界面上的状态记号。
  SessionMark get mark => switch (phase) {
    SessionPhase.awaitingApproval ||
    SessionPhase.waitingForUser => SessionMark.needsYou,
    SessionPhase.failed => SessionMark.failed,
    SessionPhase.succeeded => SessionMark.done,
    SessionPhase.dead => SessionMark.ended,
    SessionPhase.connecting ||
    SessionPhase.thinking ||
    SessionPhase.executingTool => SessionMark.running,
    SessionPhase.idle =>
      status == SessionStatus.running
          ? SessionMark.running
          : status == SessionStatus.needsYou
          ? SessionMark.needsYou
          : SessionMark.idle,
  };

  bool get unread => attention != null;

  bool get isTerminal => kind == SessionKind.terminal;

  /// 行副标题里的项目名：没有项目（智能体对话、自动化运行）就用自动化名或 cwd 末段。
  String? get placeLabel {
    final project = projectTitle?.trim();
    if (project != null && project.isNotEmpty) return project;
    final automationName = automation?.automationName.trim();
    if (automationName != null && automationName.isNotEmpty) {
      return automationName;
    }
    final dir = cwd?.trim();
    if (dir == null || dir.isEmpty) return null;
    final parts = dir.split('/').where((p) => p.isNotEmpty);
    return parts.isEmpty ? dir : parts.last;
  }
}

/// 行副标题里「谁在干活」：认得出 agent 用 agent 名；认不出的终端就是「终端」，
/// 不能说成「Agent」。
String sessionAgentLabel(L10n l10n, SessionSummary session) {
  final agent = session.agent.trim().toLowerCase();
  if (session.isTerminal &&
      (agent.isEmpty || agent == 'other') &&
      agentIconAsset(agent) == null) {
    return l10n.kindTerminal;
  }
  return agentDisplayName(session.agent);
}

String phaseLabel(L10n l10n, SessionPhase phase) => switch (phase) {
  SessionPhase.connecting => l10n.phaseConnecting,
  SessionPhase.thinking => l10n.phaseThinking,
  SessionPhase.executingTool => l10n.phaseExecutingTool,
  SessionPhase.awaitingApproval => l10n.phaseAwaitingApproval,
  SessionPhase.waitingForUser => l10n.phaseWaitingForUser,
  SessionPhase.succeeded => l10n.phaseSucceeded,
  SessionPhase.failed => l10n.phaseFailed,
  SessionPhase.idle => l10n.phaseIdle,
  SessionPhase.dead => l10n.phaseDead,
};

extension FleetRuntimeView on FleetRuntime {
  String get id => record.runtimeId;
  String get name => record.name;
  bool get online => status.state == ConnectionState.connected;

  /// 停下来且要用户处理（被撤销 / 要升级）。
  bool get needsAttention =>
      status.state == ConnectionState.disconnected &&
      (status.error?.kind == SmeltErrorKind.unpaired ||
          status.error?.kind == SmeltErrorKind.incompatible);
}

/// 链路的一句话：「局域网 · 12 ms」「中继 · 84 ms」「重连中…」。
String linkLabel(L10n l10n, RuntimeStatus status) {
  switch (status.state) {
    case ConnectionState.connected:
      final link = status.link;
      final ms = link?.rttMs;
      if (link == null || ms == null) return l10n.machineOnline;
      return switch (link.path) {
        LinkPath.lan => l10n.machineOnlineLan(ms),
        LinkPath.peerToPeer => l10n.machineOnlineDirect(ms),
        LinkPath.relay => l10n.machineOnlineRelay(ms),
        LinkPath.unknown => l10n.machineOnline,
      };
    case ConnectionState.connecting:
      return l10n.machineConnecting;
    case ConnectionState.reconnecting:
      return l10n.machineReconnecting;
    case ConnectionState.disconnected:
      return switch (status.error?.kind) {
        SmeltErrorKind.unpaired => l10n.machineRevoked,
        SmeltErrorKind.incompatible => l10n.machineIncompatible,
        _ => l10n.machineOffline,
      };
  }
}

/// 显示的是缓存时，前半句说为什么：机器连着就是还在同步，没连上就说它的连接状态。
/// 后半句（数据是几点的）由调用方拼。
String cachedStatusLabel(L10n l10n, RuntimeStatus? status) {
  if (status == null) return l10n.machineOffline;
  if (status.state == ConnectionState.connected) return l10n.machineSyncing;
  return linkLabel(l10n, status);
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
  /// 必填项都填好了没有。界面上还没提交的文字要用 [localTextValues] 再算一遍。
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

extension HistorySessionView on HistorySessionSummary {
  String get displayTitle {
    final custom = customTitle?.trim();
    return custom == null || custom.isEmpty ? title : custom;
  }
}
