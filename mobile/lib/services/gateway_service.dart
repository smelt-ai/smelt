// WebSocket client for the gateway /acp/ws endpoint.

import 'dart:async';
import 'dart:collection';
import 'dart:convert';
import 'dart:math';
import 'dart:io';
import 'package:flutter/foundation.dart';
import 'package:web_socket_channel/web_socket_channel.dart';
import '../models/acp_snapshot.dart';
import '../models/pairing_config.dart';
import 'session_cache_store.dart';

Map<String, dynamic> _decodeGatewayJson(String data) =>
    jsonDecode(data) as Map<String, dynamic>;

class LifecycleAttention {
  final String sessionId;
  final String title;
  final String message;
  final String kind;

  const LifecycleAttention({
    required this.sessionId,
    required this.title,
    required this.message,
    required this.kind,
  });

  bool get requiresAction =>
      kind == 'approval' || kind == 'input' || kind == 'failure';

  factory LifecycleAttention.fromJson(Map<String, dynamic> json) {
    return LifecycleAttention(
      sessionId: json['sessionId'] as String? ?? '',
      title: json['title'] as String? ?? '',
      message: json['message'] as String? ?? '',
      kind: json['kind'] as String? ?? 'notice',
    );
  }

  Map<String, dynamic> toJson() => {
    'sessionId': sessionId,
    'title': title,
    'message': message,
    'kind': kind,
  };
}

enum SessionKind { acp, terminal }

/// 这条会话不是用户开的，而是某条自动化的一次运行现场。
///
/// 桌面上 Run 挂在自动化页的运行历史下面，会话在 daemon 目录里是 hidden 的；
/// 手机把它们提到指挥台，因为「自动化半夜停下来等审批」这一刻只有手机在场。
/// 用户此时没有任何上下文，光有一句「Pi 想执行 git push」不足以判断该不该放行，
/// 所以来源、触发方式和这次运行固化的输入必须跟着卡片一起到。
class AutomationSource {
  final String automationId;
  final String automationName;
  final String runId;

  /// 与桌面 `AgentRunStatus` 同名：starting / queued / dispatching / running /
  /// awaiting_approval / waiting_for_user / completed / failed / cancelled / skipped。
  final String runStatus;

  /// manual / scheduled / webhook / event。
  final String runSource;

  /// 这次运行**固化**的输入，不是自动化当前的定义值——定义改过之后回看旧 Run，
  /// 显示当前值会直接把排查带偏。
  final String? prompt;
  final int? startedAt;

  const AutomationSource({
    required this.automationId,
    required this.automationName,
    required this.runId,
    required this.runStatus,
    required this.runSource,
    this.prompt,
    this.startedAt,
  });

  factory AutomationSource.fromJson(Map<String, dynamic> json) =>
      AutomationSource(
        automationId: json['automation_id'] as String? ?? '',
        automationName: json['automation_name'] as String? ?? '',
        runId: json['run_id'] as String? ?? '',
        runStatus: json['run_status'] as String? ?? 'running',
        runSource: json['run_source'] as String? ?? 'manual',
        prompt: json['prompt'] as String?,
        startedAt: json['started_at'] as int?,
      );

  Map<String, dynamic> toJson() => {
    'automation_id': automationId,
    'automation_name': automationName,
    'run_id': runId,
    'run_status': runStatus,
    'run_source': runSource,
    if (prompt != null) 'prompt': prompt,
    if (startedAt != null) 'started_at': startedAt,
  };
}

/// 自动化的一条调度规则。字段与桌面 `AgentSchedule` 同构，文案在移动端拼。
class AutomationSchedule {
  /// daily / every_minutes / every_hours / weekly。
  final String type;
  final int hour;
  final int minute;
  final int minutes;
  final int hours;

  /// 周几的位掩码，bit0 = 周一。仅 weekly 有意义。
  final int days;

  const AutomationSchedule({
    required this.type,
    this.hour = 0,
    this.minute = 0,
    this.minutes = 0,
    this.hours = 0,
    this.days = 0,
  });

  factory AutomationSchedule.fromJson(Map<String, dynamic> json) =>
      AutomationSchedule(
        type: json['type'] as String? ?? 'daily',
        hour: json['hour'] as int? ?? 0,
        minute: json['minute'] as int? ?? 0,
        minutes: json['minutes'] as int? ?? 0,
        hours: json['hours'] as int? ?? 0,
        days: json['days'] as int? ?? 0,
      );
}

/// 自动化目录里的「上次结果」。
class AutomationRunSummary {
  final String runId;
  final String status;
  final String source;
  final int? startedAt;
  final int? finishedAt;

  /// 那次运行的会话，有值就能从目录直接跳进执行现场。
  final String? sessionId;
  final String? error;

  const AutomationRunSummary({
    required this.runId,
    required this.status,
    required this.source,
    this.startedAt,
    this.finishedAt,
    this.sessionId,
    this.error,
  });

  factory AutomationRunSummary.fromJson(Map<String, dynamic> json) =>
      AutomationRunSummary(
        runId: json['run_id'] as String? ?? '',
        status: json['status'] as String? ?? 'running',
        source: json['source'] as String? ?? 'manual',
        startedAt: json['started_at'] as int?,
        finishedAt: json['finished_at'] as int?,
        sessionId: json['session_id'] as String?,
        error: json['error'] as String?,
      );
}

/// 自动化目录的一行：它叫什么、什么时候由谁做、上次结果如何。
///
/// 网关做过脱敏，webhook 的 endpoint 和 secret 不会到这里——手机上没有任何
/// 需要它们的操作，多带一份凭据只是白白扩大泄露面。
class AutomationSummary {
  final String id;
  final String name;
  final bool enabled;

  /// 三种时机可以混排在同一条自动化上，所以这里是三份并列的时机，不是一个单选
  /// 的类型：只显示其中一种，会让「既定时又能被外部打」的自动化在手机上少掉
  /// 一半，而用户上手机就是来核对「它到底什么时候跑」的。
  final List<AutomationSchedule> schedules;
  final List<String> eventTopics;
  final bool webhook;

  /// agent / shell。
  final String actionKind;

  /// 执行者展示名。null 表示这条自动化绑的智能体定义已经不在了——那本身就是
  /// 用户要看见的排查线索。
  final String? agentName;
  final int? nextRunAt;
  final AutomationRunSummary? lastRun;

  const AutomationSummary({
    required this.id,
    required this.name,
    required this.enabled,
    this.schedules = const [],
    this.eventTopics = const [],
    this.webhook = false,
    required this.actionKind,
    this.agentName,
    this.nextRunAt,
    this.lastRun,
  });

  factory AutomationSummary.fromJson(Map<String, dynamic> json) =>
      AutomationSummary(
        id: json['id'] as String? ?? '',
        name: json['name'] as String? ?? '',
        enabled: json['enabled'] as bool? ?? false,
        schedules: (json['schedules'] as List<dynamic>? ?? const [])
            .whereType<Map<String, dynamic>>()
            .map(AutomationSchedule.fromJson)
            .toList(),
        eventTopics: (json['event_topics'] as List<dynamic>? ?? const [])
            .whereType<String>()
            .toList(),
        webhook: json['webhook'] as bool? ?? false,
        actionKind: json['action_kind'] as String? ?? 'agent',
        agentName: json['agent_name'] as String?,
        nextRunAt: json['next_run_at'] as int?,
        lastRun: json['last_run'] is Map<String, dynamic>
            ? AutomationRunSummary.fromJson(
                json['last_run'] as Map<String, dynamic>,
              )
            : null,
      );
}

/// 智能体定义的只读投影。手机不提供编辑入口，这里只回答「它为什么这么干」。
class AgentDefinitionSummary {
  final String id;
  final String name;
  final String description;

  /// 执行引擎机器码（当前只有 pi 能被自动化调用）。
  final String agentId;

  /// 长期工作方式，会写进引擎 system prompt。
  final String prompt;
  final List<String> plugins;
  final List<String> contextFolders;
  final List<String> contextLinks;

  const AgentDefinitionSummary({
    required this.id,
    required this.name,
    this.description = '',
    required this.agentId,
    this.prompt = '',
    this.plugins = const [],
    this.contextFolders = const [],
    this.contextLinks = const [],
  });

  static List<String> _strings(dynamic value) =>
      (value as List<dynamic>? ?? const []).whereType<String>().toList();

  factory AgentDefinitionSummary.fromJson(Map<String, dynamic> json) =>
      AgentDefinitionSummary(
        id: json['id'] as String? ?? '',
        name: json['name'] as String? ?? '',
        description: json['description'] as String? ?? '',
        agentId: json['agent_id'] as String? ?? '',
        prompt: json['prompt'] as String? ?? '',
        plugins: _strings(json['plugins']),
        contextFolders: _strings(json['context_folders']),
        contextLinks: _strings(json['context_links']),
      );
}

/// 「智能体」这一栏的整份数据。自动化和定义一起下发，目录行才能显示执行者名字。
class AutomationCatalog {
  final List<AutomationSummary> automations;
  final List<AgentDefinitionSummary> agents;

  const AutomationCatalog({
    this.automations = const [],
    this.agents = const [],
  });
}

/// 会话摘要（列表用）
class SessionSummary {
  static const unknownOrder = 0xffffffff;

  final String id;
  final SessionKind kind;
  final String title;
  final String phase;
  final String status;
  final String agent;
  final String? cwd;
  final String? projectRoot;
  final String? projectTitle;
  final int projectOrder;
  final int sessionOrder;
  final int leafOrder;
  final int updatedAt;
  final String? detail;
  final bool unread;
  final LifecycleAttention? attention;

  /// 只有自动化 Run 的会话才有。null = 用户自己开的对话/终端。
  final AutomationSource? automation;

  /// 这是一条智能体对话时，它属于哪个智能体定义。
  ///
  /// 网关从 cwd 反推（智能体 space 的目录名就是定义 id），所以只有 id 没有名字
  /// ——名字要读存档，而会话摘要是每次事件都重算的热路径。展示名由
  /// [AutomationCatalog] 配。
  final String? agentDefinitionId;

  /// 有归属就是智能体对话。自动化 Run 不算：它有自己的来源标注，两者在界面上
  /// 是不同的东西（一个是我和智能体聊，一个是它自己半夜跑）。
  bool get isAgentConversation =>
      automation == null && agentDefinitionId != null;

  const SessionSummary({
    required this.id,
    this.kind = SessionKind.acp,
    required this.title,
    required this.phase,
    this.status = 'idle',
    required this.agent,
    this.cwd,
    this.projectRoot,
    this.projectTitle,
    this.projectOrder = unknownOrder,
    this.sessionOrder = unknownOrder,
    this.leafOrder = unknownOrder,
    this.updatedAt = 0,
    this.detail,
    this.unread = false,
    this.attention,
    this.automation,
    this.agentDefinitionId,
  });

  factory SessionSummary.fromJson(Map<String, dynamic> json) {
    return SessionSummary(
      id: json['id'] as String? ?? '',
      kind: switch (json['kind']) {
        'terminal' => SessionKind.terminal,
        _ => SessionKind.acp,
      },
      title: json['title'] as String? ?? '',
      phase: json['phase'] as String? ?? 'idle',
      status: json['status'] as String? ?? 'idle',
      agent: json['agent'] as String? ?? 'other',
      cwd: json['cwd'] as String?,
      projectRoot: json['project_root'] as String?,
      projectTitle: json['project_title'] as String?,
      projectOrder: json['project_order'] as int? ?? unknownOrder,
      sessionOrder: json['session_order'] as int? ?? unknownOrder,
      leafOrder: json['leaf_order'] as int? ?? unknownOrder,
      updatedAt: json['updated_at'] as int? ?? 0,
      detail: json['detail'] as String?,
      unread: json['unread'] as bool? ?? false,
      attention: json['attention'] is Map<String, dynamic>
          ? LifecycleAttention.fromJson(
              json['attention'] as Map<String, dynamic>,
            )
          : null,
      automation: json['automation'] is Map<String, dynamic>
          ? AutomationSource.fromJson(
              json['automation'] as Map<String, dynamic>,
            )
          : null,
      agentDefinitionId: json['agent_definition_id'] as String?,
    );
  }

  Map<String, dynamic> toJson() => {
    'id': id,
    'kind': kind.name,
    'title': title,
    'phase': phase,
    'status': status,
    'agent': agent,
    if (cwd != null) 'cwd': cwd,
    if (projectRoot != null) 'project_root': projectRoot,
    if (projectTitle != null) 'project_title': projectTitle,
    'project_order': projectOrder,
    'session_order': sessionOrder,
    'leaf_order': leafOrder,
    'updated_at': updatedAt,
    if (detail != null) 'detail': detail,
    'unread': unread,
    if (attention != null) 'attention': attention!.toJson(),
    if (automation != null) 'automation': automation!.toJson(),
    if (agentDefinitionId != null) 'agent_definition_id': agentDefinitionId,
  };
}

class WorkspaceProject {
  final String root;
  final String title;
  final int order;

  const WorkspaceProject({
    required this.root,
    required this.title,
    required this.order,
  });

  factory WorkspaceProject.fromJson(Map<String, dynamic> json) =>
      WorkspaceProject(
        root: json['root'] as String? ?? '',
        title: json['title'] as String? ?? '',
        order: json['order'] as int? ?? SessionSummary.unknownOrder,
      );
}

class AcpAgentOption {
  final String id;
  final String kind;
  final String label;
  final bool profile;

  /// Set when this option is a product agent rather than a bare engine or a
  /// workspace profile. Such a conversation can start without a project: the
  /// gateway puts it in the agent's own space, same as the desktop.
  final String? agentDefinitionId;

  const AcpAgentOption({
    required this.id,
    required this.kind,
    required this.label,
    required this.profile,
    this.agentDefinitionId,
  });

  bool get isAgentDefinition => agentDefinitionId != null;

  factory AcpAgentOption.fromJson(Map<String, dynamic> json) => AcpAgentOption(
    id: json['id'] as String? ?? '',
    kind: json['kind'] as String? ?? '',
    label: json['label'] as String? ?? '',
    profile: json['profile'] as bool? ?? false,
    agentDefinitionId: json['agentDefinitionId'] as String?,
  );
}

/// 新建会话选择器里的一行，跟桌面「+」弹层是同一份目录（`smelt_core::new_session`）。
///
/// 「常用 / 终端 / 对话」的分组、顺序、Pin 全在电脑那边算好；手机只负责画，并把
/// [key] 原样回传。命令行永远不在手机上拼——配对设备不该决定电脑上跑什么进程。
enum LaunchSection { common, terminal, conversation }

enum LaunchTarget { conversation, terminal, blankTerminal }

class LaunchAction {
  final String key;
  final String label;
  final LaunchSection section;
  final LaunchTarget target;

  /// 动作是「对话」还是「终端」。跟 [section] 无关：常用组里两种都有。
  final bool isConversation;

  /// 画图标用的 agent 标识；认不出就是空串。
  final String agent;

  const LaunchAction({
    required this.key,
    required this.label,
    required this.section,
    required this.target,
    required this.isConversation,
    required this.agent,
  });

  /// 列表里显示成「名称 · 对话/终端」，跟桌面同一种写法。
  String get kindLabel => isConversation ? 'Conversation' : 'Terminal';

  static LaunchAction? fromJson(Map<String, dynamic> json) {
    final key = json['key'] as String? ?? '';
    if (key.isEmpty) return null;
    final target = switch (json['target'] as String?) {
      'conversation' => LaunchTarget.conversation,
      'terminal' => LaunchTarget.terminal,
      'blankTerminal' => LaunchTarget.blankTerminal,
      _ => null,
    };
    if (target == null) return null;
    final section = switch (json['section'] as String?) {
      'common' => LaunchSection.common,
      'terminal' => LaunchSection.terminal,
      'conversation' => LaunchSection.conversation,
      _ => null,
    };
    if (section == null) return null;
    return LaunchAction(
      key: key,
      label: json['label'] as String? ?? key,
      section: section,
      target: target,
      isConversation: (json['kind'] as String?) == 'conversation',
      agent:
          (json['agentKind'] as String?) ??
          (json['provider'] as String?) ??
          '',
    );
  }
}

/// 一条已加载的技能。名字 + 一句话描述，和桌面输入栏的技能弹层同一份数据。
class SessionSkill {
  final String name;
  final String description;

  const SessionSkill({required this.name, this.description = ''});

  factory SessionSkill.fromJson(Map<String, dynamic> json) => SessionSkill(
    name: json['name'] as String? ?? '',
    description: json['description'] as String? ?? '',
  );
}

/// `listSessionSkills` 的应答。[supported] = 这场对话的引擎根本有「技能」这个概念
/// （目前只有 Pi），或者电脑端版本太旧。不支持时界面不画入口，而不是画一个永远
/// 空的列表。
class SessionSkills {
  final String sessionId;
  final bool supported;
  final List<SessionSkill> skills;

  const SessionSkills({
    required this.sessionId,
    required this.supported,
    this.skills = const [],
  });
}

class WorkspaceCatalog {
  final List<WorkspaceProject> projects;
  final List<AcpAgentOption> agents;

  /// 新建会话的启动动作，已按「常用 / 终端 / 对话」排好序。旧网关不下发这个字段，
  /// 那时是空列表——选择器会直接让用户去升级电脑端，而不是拼一个半残的目录。
  final List<LaunchAction> launchActions;

  const WorkspaceCatalog({
    required this.projects,
    required this.agents,
    this.launchActions = const [],
  });

  List<LaunchAction> actionsIn(LaunchSection section) =>
      launchActions.where((action) => action.section == section).toList();
}

class HistorySessionSummary {
  final String resumeId;

  /// Title generated by the agent itself — always the raw value, kept so a
  /// renamed session can still show where it came from (same as the desktop).
  final String title;

  /// Name the user typed in Smelt, on either device. Null when untouched.
  final String? customTitle;
  final DateTime? startedAt;
  final DateTime? lastActiveAt;
  final int messageCount;

  const HistorySessionSummary({
    required this.resumeId,
    required this.title,
    required this.customTitle,
    required this.startedAt,
    required this.lastActiveAt,
    required this.messageCount,
  });

  /// What the list renders: the user's name wins, otherwise the agent's.
  String get displayTitle {
    final custom = customTitle?.trim();
    return custom == null || custom.isEmpty ? title : custom;
  }

  bool get hasCustomTitle => displayTitle != title;

  factory HistorySessionSummary.fromJson(Map<String, dynamic> json) =>
      HistorySessionSummary(
        resumeId: json['resumeId'] as String? ?? '',
        title: json['title'] as String? ?? '',
        customTitle: json['customTitle'] as String?,
        startedAt: DateTime.tryParse(json['startedAt'] as String? ?? ''),
        lastActiveAt: DateTime.tryParse(json['lastActiveAt'] as String? ?? ''),
        messageCount: json['messageCount'] as int? ?? 0,
      );

  HistorySessionSummary withCustomTitle(String? customTitle) =>
      HistorySessionSummary(
        resumeId: resumeId,
        title: title,
        customTitle: customTitle,
        startedAt: startedAt,
        lastActiveAt: lastActiveAt,
        messageCount: messageCount,
      );
}

class SessionHistoryResult {
  final String projectRoot;
  final String agentOptionId;
  final List<HistorySessionSummary> sessions;

  const SessionHistoryResult({
    required this.projectRoot,
    required this.agentOptionId,
    required this.sessions,
  });
}

/// Ack for `renameSessionHistory`. [customTitle] is null once the user resets
/// the name, and [title] then carries the agent's own title back — the phone
/// never has to guess what the default was.
class SessionHistoryRenameResult {
  final String projectRoot;
  final String agentOptionId;
  final String resumeId;
  final String title;
  final String? customTitle;

  const SessionHistoryRenameResult({
    required this.projectRoot,
    required this.agentOptionId,
    required this.resumeId,
    required this.title,
    required this.customTitle,
  });
}

int compareSessionMenuOrder(SessionSummary a, SessionSummary b) {
  var compared = a.projectOrder.compareTo(b.projectOrder);
  if (compared != 0) return compared;
  compared = a.sessionOrder.compareTo(b.sessionOrder);
  if (compared != 0) return compared;
  compared = a.leafOrder.compareTo(b.leafOrder);
  if (compared != 0) return compared;
  compared = a.title.compareTo(b.title);
  if (compared != 0) return compared;
  return a.id.compareTo(b.id);
}

/// WebSocket 连接状态
enum WsState { disconnected, connecting, connected, reconnecting }

enum ConnectionPathKind { lan, p2p, relay, direct, unknown }

class IrohPathSample {
  final ConnectionPathKind kind;
  final int rttMs;

  const IrohPathSample({required this.kind, required this.rttMs});
}

class ConnectionMetrics {
  final ConnectionPathKind kind;
  final int? latencyMs;

  const ConnectionMetrics({
    this.kind = ConnectionPathKind.unknown,
    this.latencyMs,
  });
}

class MessageSendResult {
  final String requestId;
  final bool ok;
  final String? error;

  const MessageSendResult({
    required this.requestId,
    required this.ok,
    this.error,
  });
}

/// 启动 iroh 隧道并返回手机本地入口端口。
///
/// 做成可注入的函数而不是直接调 FFI，是为了让 `GatewayService` 的测试
/// 不必依赖编译好的 Rust 动态库。
typedef IrohTunnelOpener =
    Future<int> Function(String endpointId, String relayUrl);
typedef IrohTunnelStopper = Future<void> Function();
typedef IrohPathProbe = Future<IrohPathSample?> Function();

/// Gateway WebSocket 服务
class GatewayService {
  GatewayService({
    this.connectTimeout = const Duration(seconds: 10),
    this.reconnectDelay = const Duration(seconds: 2),
    this.metricsInterval = const Duration(seconds: 3),
    this.messageAckTimeout = const Duration(seconds: 20),
    this.pongTimeout = const Duration(seconds: 15),
    this.cacheStore,
    IrohTunnelOpener? irohTunnelOpener,
    IrohTunnelStopper? irohTunnelStopper,
    IrohPathProbe? irohPathProbe,
  }) : irohTunnelOpener = irohTunnelOpener ?? _irohUnavailable,
       irohTunnelStopper = irohTunnelStopper ?? _noopIrohStop,
       irohPathProbe = irohPathProbe ?? _noIrohPath;

  /// 从发起连接到收到服务端 `connected` 的整体上限。
  final Duration connectTimeout;
  final Duration reconnectDelay;
  final Duration metricsInterval;
  final Duration messageAckTimeout;

  /// ping 发出后等 pong 的上限，超时即判定连接已死。
  ///
  /// 手机切换 WiFi/蜂窝、或被系统冻结后恢复，TCP 常常处于半开：写得出去、
  /// 读不回来，且不触发 `onDone`/`onError`。没有这个上限的话状态会永远停在
  /// `connected`，而推送早已收不到——界面于是一直显示掉线那一刻的旧会话。
  final Duration pongTimeout;
  final SessionCacheStore? cacheStore;

  /// 启动 iroh 隧道的方式。默认会明确报错 —— 真正的实现由组装根（`main()`）
  /// 在 RustLib 初始化之后注入，这样本文件保持纯 Dart，单测不必依赖动态库。
  IrohTunnelOpener irohTunnelOpener;
  IrohTunnelStopper irohTunnelStopper;
  IrohPathProbe irohPathProbe;

  static Future<int> _irohUnavailable(String _, String _) =>
      Future.error(StateError('本版本未编入 iroh 隧道支持'));
  static Future<void> _noopIrohStop() async {}
  static Future<IrohPathSample?> _noIrohPath() async => null;

  WebSocketChannel? _channel;
  Uri? _activeGatewayWsUri;
  StreamSubscription<dynamic>? _channelSubscription;
  Timer? _reconnectTimer;
  Timer? _connectWatchdog;
  Timer? _metricsTimer;
  WsState _state = WsState.disconnected;
  String? _endpoint;
  String? _token;
  bool _manuallyDisconnected = true;

  /// 当前目标是否曾经握手成功过。没成功过的地址（打错、主机不存在）失败后直接
  /// 回到断开态让用户改，而不是无限自动重连。
  bool _everConnected = false;
  int _connectionGeneration = 0;
  int _reconnectAttempts = 0;
  bool _outageErrorReported = false;
  bool _writeEnabled = false;
  ConnectionMetrics _metrics = const ConnectionMetrics();
  bool _pingSupported = true;
  bool _hasPongLatency = false;
  int? _pendingPingSentAt;

  /// 旧桌面不认识 `listSessionSkills`，只会回一句 `invalid request`。碰一次就不再问，
  /// 也不把它当作会话错误弹给用户——该升级的是电脑端，不是这场对话出了问题。
  bool _sessionSkillsSupported = true;
  int _pendingSkillsRequests = 0;
  Future<void> _messageQueue = Future.value();

  static const int _maxCachedSessions = 5;
  static const int _maxCacheBytes = 32 * 1024 * 1024;
  static const int _initialTailLimit = 100;

  /// 指挥台按需取详情时的窗口。卡片只用 pending_permissions，配几条最近 entry
  /// 作上下文即可，不必把整段历史拉到手机上。
  static const int _pendingActionsTailLimit = 8;

  /// "从末尾取"的哨兵下标：smeltd 侧会 clamp 到实际长度。
  static const int _tailProbeOffset = 0x7fffffff;
  final LinkedHashMap<String, AcpSnapshot> _snapshotCache = LinkedHashMap();
  final Set<String> _historyLoads = {};
  final Set<String> _cachedSnapshotIds = {};
  final Map<String, Timer> _snapshotCacheTimers = {};
  String? _cacheNamespace;
  int _cacheLoadGeneration = 0;
  List<SessionSummary> _lastSessions = const [];
  AutomationCatalog? _lastAutomationCatalog;
  DateTime? _cachedAt;
  bool _sessionsAreCached = false;

  final _stateController = StreamController<WsState>.broadcast();
  final _sessionsController =
      StreamController<List<SessionSummary>>.broadcast();
  final _workspaceController = StreamController<WorkspaceCatalog>.broadcast();
  final _automationCatalogController =
      StreamController<AutomationCatalog>.broadcast();
  final _sessionHistoryController =
      StreamController<SessionHistoryResult>.broadcast();
  final _sessionSkillsController = StreamController<SessionSkills>.broadcast();
  final _sessionHistoryRenameController =
      StreamController<SessionHistoryRenameResult>.broadcast();
  final _sessionCreatedController = StreamController<String>.broadcast();
  final _sessionDeletedController = StreamController<String>.broadcast();
  final _snapshotController = StreamController<AcpSnapshot>.broadcast();
  final _snapshotCacheController = StreamController<String>.broadcast();
  final _attentionController = StreamController<LifecycleAttention>.broadcast();
  final _attentionResolvedController = StreamController<String>.broadcast();
  final _errorController = StreamController<String>.broadcast();
  final _metricsController = StreamController<ConnectionMetrics>.broadcast();
  final _messageSendController =
      StreamController<MessageSendResult>.broadcast();
  final LinkedHashSet<String> _pendingMessageRequests = LinkedHashSet();
  final Map<String, Timer> _messageAckTimers = {};

  String? _subscribedSessionId;

  /// 连接状态流
  Stream<WsState> get stateStream => _stateController.stream;

  /// 会话列表流
  Stream<List<SessionSummary>> get sessionsStream => _sessionsController.stream;

  Stream<WorkspaceCatalog> get workspaceStream => _workspaceController.stream;

  /// 「智能体」栏的数据流。写命令的结果也从这里回来——daemon 是自动化的所有者，
  /// 它回一份权威快照，手机就不会出现「开关拨过去了、实际没生效」。
  Stream<AutomationCatalog> get automationCatalogStream =>
      _automationCatalogController.stream;

  Stream<SessionHistoryResult> get sessionHistoryStream =>
      _sessionHistoryController.stream;

  /// 会话已加载技能的应答。按需发问，不跟快照推：算它要在电脑那边扫磁盘。
  Stream<SessionSkills> get sessionSkillsStream =>
      _sessionSkillsController.stream;

  Stream<SessionHistoryRenameResult> get sessionHistoryRenameStream =>
      _sessionHistoryRenameController.stream;

  Stream<String> get sessionCreatedStream => _sessionCreatedController.stream;

  Stream<String> get sessionDeletedStream => _sessionDeletedController.stream;

  /// 当前订阅会话的快照流
  Stream<AcpSnapshot> get snapshotStream => _snapshotController.stream;

  /// 任一会话的缓存快照被刷新时发出它的 id——包括当前没在看的那些。
  ///
  /// `snapshotStream` 只发当前订阅的会话，因为会话页只关心自己。指挥台要同时盯住
  /// 一批会话，需要的是"谁的详情变了"这个更底层的信号，据此再去 `cachedSnapshot`
  /// 取内容。把它单独暴露出来，指挥台就不必为了拿详情去抢订阅槽。
  Stream<String> get snapshotCacheStream => _snapshotCacheController.stream;

  /// smeltd 统一生命周期产生的关注事件。
  Stream<LifecycleAttention> get attentionStream => _attentionController.stream;

  /// 任一客户端处理完行动项后，由同一 AttentionStore 发出的解决事件。
  Stream<String> get attentionResolvedStream =>
      _attentionResolvedController.stream;

  /// 错误流
  Stream<String> get errorStream => _errorController.stream;

  Stream<ConnectionMetrics> get metricsStream => _metricsController.stream;

  Stream<MessageSendResult> get messageSendStream =>
      _messageSendController.stream;

  /// 当前状态
  WsState get state => _state;

  ConnectionMetrics get metrics => _metrics;

  /// Whether the desktop gateway allows prompts and approval responses.
  bool get writeEnabled => _writeEnabled;

  List<SessionSummary> get lastSessions => _lastSessions;

  /// 最近一次的自动化目录。会话列表要拿它把 `agent_definition_id` 配成展示名，
  /// 为一个副标题再开一条订阅不值得。null = 还没拉到。
  AutomationCatalog? get lastAutomationCatalog => _lastAutomationCatalog;

  /// 智能体展示名。配不上就返回 null，让调用方退回通用标签而不是印一个 uuid。
  String? agentDefinitionName(String? definitionId) {
    if (definitionId == null) return null;
    for (final agent in _lastAutomationCatalog?.agents ?? const []) {
      if (agent.id == definitionId) {
        return agent.name.trim().isEmpty ? null : agent.name.trim();
      }
    }
    return null;
  }

  DateTime? get cachedAt => _cachedAt;

  bool get sessionsAreCached => _sessionsAreCached;

  bool snapshotIsCached(String sessionId) =>
      _cachedSnapshotIds.contains(sessionId);

  /// 当前订阅的会话 ID
  String? get subscribedSessionId => _subscribedSessionId;

  /// Returns and promotes a cached session snapshot in the LRU.
  AcpSnapshot? cachedSnapshot(String sessionId) {
    final snapshot = _snapshotCache.remove(sessionId);
    if (snapshot != null) _snapshotCache[sessionId] = snapshot;
    return snapshot;
  }

  /// 连接到 gateway
  Future<void> connect(String endpoint, String token) async {
    final target = endpoint.trim();
    final sameTarget = matchesTarget(target, token);
    final restartIroh =
        _state == WsState.reconnecting &&
        Uri.tryParse(target)?.scheme == PairingConfig.irohScheme;
    // 同一目标已连/在连 → 幂等返回；换了目标 → 拆掉旧连接改连新的，否则扫码切换
    // 桌面会被静默忽略（UI 显示新地址、实际连着旧的）。
    if (sameTarget) {
      if (_state == WsState.connected || _state == WsState.connecting) return;
    } else {
      _teardownSocket();
      _everConnected = false;
      _reconnectAttempts = 0;
      _outageErrorReported = false;
      _clearSnapshotCache();
      _lastSessions = const [];
      _sessionsController.add(_lastSessions);
    }

    _endpoint = target;
    _token = token;
    _manuallyDisconnected = false;
    _reconnectTimer?.cancel();
    _setState(WsState.connecting);
    if (!sameTarget) {
      await _restoreTargetCache(target, token);
      if (!matchesTarget(target, token) || _manuallyDisconnected) return;
    }
    // 不可达但可路由的地址（打错 IP）不会立刻报错，`ready` 会一直挂着；握手成功
    // 但服务端不发 `connected` 同样会卡住。用一个看门狗兜住整段握手。
    _connectWatchdog?.cancel();
    _connectWatchdog = Timer(connectTimeout, () {
      if (_state != WsState.connecting) return;
      _reportConnectionFailure('连接超时：$target 没有响应');
      _failConnection();
    });

    final generation = ++_connectionGeneration;
    try {
      final wsUri = await _resolveWsUri(
        _endpoint!,
        token,
        restartIroh: restartIroh,
      );
      if (generation != _connectionGeneration) return;
      _activeGatewayWsUri = wsUri;
      final channel = WebSocketChannel.connect(wsUri);
      _channel = channel;
      _channelSubscription = channel.stream.listen(
        (data) {
          if (generation == _connectionGeneration) {
            _enqueueMessage(data, generation);
          }
        },
        onError: (error) {
          if (generation == _connectionGeneration) _onError(error);
        },
        onDone: () {
          if (generation == _connectionGeneration) _onDone();
        },
      );
      // 看门狗只负责改状态；这里同样要超时，否则 `connect()` 返回的 Future
      // 永远不完成，调用方无法 await。
      await channel.ready.timeout(connectTimeout);
    } catch (e) {
      if (generation != _connectionGeneration) return;
      _reportConnectionFailure('连接失败: $e');
      _failConnection();
    }
  }

  Future<void> _restoreTargetCache(String endpoint, String token) async {
    final store = cacheStore;
    if (store == null) return;
    final generation = ++_cacheLoadGeneration;
    final namespace = store.namespaceFor(endpoint, token);
    try {
      final cached = await store.load(namespace);
      if (generation != _cacheLoadGeneration ||
          !matchesTarget(endpoint, token)) {
        return;
      }
      _cacheNamespace = namespace;
      _lastSessions = cached.sessions;
      _cachedAt = cached.updatedAt;
      _sessionsAreCached = cached.sessions.isNotEmpty;
      for (final entry in cached.snapshots.entries) {
        _snapshotCache[entry.key] = entry.value;
        _cachedSnapshotIds.add(entry.key);
      }
      _trimSnapshotCache();
      _sessionsController.add(_lastSessions);
    } catch (_) {
      // Cache is an optimization. Connection setup must remain independent.
      if (generation == _cacheLoadGeneration) _cacheNamespace = namespace;
    }
  }

  /// 把存下来的 endpoint 变成这次真正要连的 WebSocket 地址。
  ///
  /// iroh 配对存的是 `smelt+iroh://<endpoint_id>`，本身不可拨号：得先把隧道
  /// 拉起来拿到手机本地端口，再按普通 ws 连过去。隧道口只在回环上，明文
  /// 不出手机；离开手机那一段由 QUIC 加密。
  Future<Uri> _resolveWsUri(
    String endpoint,
    String token, {
    required bool restartIroh,
  }) async {
    final parsed = Uri.parse(endpoint);
    if (parsed.scheme != PairingConfig.irohScheme) {
      return _gatewayUri(endpoint, token);
    }
    // 打洞/中继协商可能很久，必须有上限：否则打错的 EndpointId 会让界面
    // 永远停在「连接中」，这正是之前踩过的坑。
    final relayUrl = parsed.queryParameters['relay'] ?? '';
    if (relayUrl.isEmpty) {
      throw const FormatException(
        'The iroh pairing is missing its relay address',
      );
    }
    if (restartIroh) {
      await irohTunnelStopper().timeout(connectTimeout);
    }
    final port = await irohTunnelOpener(
      parsed.host,
      relayUrl,
    ).timeout(connectTimeout);
    return _gatewayUri('http://127.0.0.1:$port', token);
  }

  Uri _gatewayUri(String endpoint, String token) {
    final parsed = Uri.parse(endpoint);
    final scheme = switch (parsed.scheme) {
      'http' => 'ws',
      'https' => 'wss',
      _ => parsed.scheme,
    };
    final basePath = parsed.path.replaceFirst(RegExp(r'/+$'), '');
    final path = basePath.endsWith('/acp/ws') ? basePath : '$basePath/acp/ws';
    return parsed.replace(
      scheme: scheme,
      path: path,
      queryParameters: {...parsed.queryParameters, 'token': token},
    );
  }

  /// 当前主连接实际使用的网关地址。iroh 每次重连都可能换本地端口，终端流
  /// 必须按这份最新地址建立，不能从持久化的 smelt+iroh endpoint 自己猜。
  Uri? terminalWebSocketUri(String sessionId) {
    final active = _activeGatewayWsUri;
    if (active == null || _state != WsState.connected || sessionId.isEmpty) {
      return null;
    }
    final segments = active.pathSegments.toList();
    if (segments.length >= 2 &&
        segments[segments.length - 2] == 'acp' &&
        segments.last == 'ws') {
      segments.removeRange(segments.length - 2, segments.length);
    }
    return active.replace(
      pathSegments: [...segments, 'terminal', sessionId, 'ws'],
    );
  }

  /// 断开连接
  void disconnect() {
    _manuallyDisconnected = true;
    _reconnectAttempts = 0;
    _outageErrorReported = false;
    _teardownSocket();
    _setState(WsState.disconnected);
  }

  /// 当前连接（或正在重连）的目标是否就是这组 endpoint/token。
  bool matchesTarget(String endpoint, String token) =>
      _endpoint == endpoint.trim() && _token == token;

  Future<void> retryCurrentConnection() async {
    final endpoint = _endpoint;
    final token = _token;
    if (endpoint == null || token == null || _state != WsState.disconnected) {
      return;
    }
    await connect(endpoint, token);
  }

  /// App 回到前台时调用：立刻确认这条连接还活着，别等下一个心跳周期。
  ///
  /// 系统在后台会冻结 Dart 的 Timer，心跳期间是停摆的；这段时间里网络多半
  /// 已经换过（WiFi↔蜂窝）或者连接被运营商回收，而 TCP 半开不会触发
  /// `onDone`。所以回前台既要重新催一次心跳，也要主动拉一次全量会话——
  /// 连接若还活着，用户立刻看到最新状态，而不是先盯着一屏旧数据。
  void verifyConnection() {
    switch (_state) {
      case WsState.connected:
        _sampleMetrics();
        listSessions();
        listWorkspace();
        final sessionId = _subscribedSessionId;
        if (sessionId != null) subscribe(sessionId);
      case WsState.disconnected:
        unawaited(retryCurrentConnection());
      case WsState.connecting:
      case WsState.reconnecting:
        break;
    }
  }

  /// 关闭底层通道并作废旧连接的回调，不改变对外状态。
  void _teardownSocket({bool preserveSubscription = false}) {
    _connectionGeneration++;
    _reconnectTimer?.cancel();
    _reconnectTimer = null;
    _connectWatchdog?.cancel();
    _connectWatchdog = null;
    _metricsTimer?.cancel();
    _metricsTimer = null;
    _updateMetrics(const ConnectionMetrics());
    _pendingPingSentAt = null;
    _hasPongLatency = false;
    if (!preserveSubscription) _subscribedSessionId = null;
    _writeEnabled = false;
    if (_pendingMessageRequests.isNotEmpty) {
      final pending = _pendingMessageRequests.toList();
      _pendingMessageRequests.clear();
      for (final requestId in pending) {
        _messageSendController.add(
          MessageSendResult(
            requestId: requestId,
            ok: false,
            error: 'Connection closed before delivery was confirmed',
          ),
        );
      }
    }
    for (final timer in _messageAckTimers.values) {
      timer.cancel();
    }
    _messageAckTimers.clear();
    _channelSubscription?.cancel();
    _channelSubscription = null;
    _channel?.sink.close();
    _channel = null;
    _activeGatewayWsUri = null;
  }

  /// 请求会话列表
  void listSessions() {
    _send({'method': 'listSessions'});
  }

  void listWorkspace() {
    _send({'method': 'listWorkspace'});
  }

  void listAutomations() {
    _send({'method': 'listAutomations'});
  }

  /// 幂等设值而不是 toggle：桌面可能刚改过同一条，toggle 会翻反。
  void setAutomationEnabled(String automationId, bool enabled) {
    _send({
      'method': 'setAutomationEnabled',
      'params': {'automationId': automationId, 'enabled': enabled},
    });
  }

  void runAutomationOnce(String automationId) {
    _send({
      'method': 'runAutomationOnce',
      'params': {'automationId': automationId},
    });
  }

  void listSessionHistory(String projectRoot, String agentOptionId) {
    _send({
      'method': 'listSessionHistory',
      'params': {'projectRoot': projectRoot, 'agentOptionId': agentOptionId},
    });
  }

  /// Rename one history conversation. Pass a null/blank [title] to drop the
  /// custom name and fall back to the agent's own title. The desktop reads the
  /// same store, so both ends agree without an extra sync.
  void renameSessionHistory(
    String projectRoot,
    String agentOptionId,
    String resumeId, {
    String? title,
  }) {
    _send({
      'method': 'renameSessionHistory',
      'params': {
        'projectRoot': projectRoot,
        'agentOptionId': agentOptionId,
        'resumeId': resumeId,
        'title': ?title,
      },
    });
  }

  /// 开一场 ACP 对话。[projectRoot] 传空串表示「不绑项目」——只有产品智能体
  /// 能这么开，网关会把它落在智能体自己的 space。
  void createSession(
    String projectRoot,
    String? agentOptionId, {
    String? resumeId,
  }) {
    _send({
      'method': 'createSession',
      'params': {
        'projectRoot': projectRoot,
        'agentOptionId': ?agentOptionId,
        'resumeId': ?resumeId,
      },
    });
  }

  /// 按新建选择器里的一行开会话：对话还是终端、终端先跑什么命令，全由网关按
  /// [launchKey] 解析。手机不传命令，也不需要知道 `kind`。
  void createSessionFromLaunch(String projectRoot, String launchKey) {
    _send({
      'method': 'createSession',
      'params': {'projectRoot': projectRoot, 'launchKey': launchKey},
    });
  }

  void deleteSession(String sessionId) {
    _send({
      'method': 'deleteSession',
      'params': {'sessionId': sessionId},
    });
  }

  /// 订阅会话
  void subscribe(String sessionId) {
    _subscribedSessionId = sessionId;
    final cached = cachedSnapshot(sessionId);
    final historyId = cached?.stableHistoryId;
    final knownEntries =
        cached != null && cached.entriesEnd == cached.entriesTotal
        ? cached.entriesTotal
        : null;
    _send({
      'method': 'subscribe',
      'params': {
        'sessionId': sessionId,
        'historySessionId': ?historyId,
        'knownEntries': ?knownEntries,
        'snapshotRevision': ?cached?.snapshotRevision,
        'tailLimit': _initialTailLimit,
      },
    });
  }

  /// Requests the page immediately preceding the cached window.
  bool loadOlder(String sessionId) {
    final cached = _snapshotCache[sessionId];
    if (_state != WsState.connected ||
        cached == null ||
        !cached.hasMoreBefore ||
        !_historyLoads.add(sessionId)) {
      return false;
    }
    _send({
      'method': 'loadHistory',
      'params': {
        'sessionId': sessionId,
        'beforeOffset': cached.entriesOffset,
        'limit': _initialTailLimit,
      },
    });
    return true;
  }

  /// 取某个会话的待批详情，不占用订阅槽。
  ///
  /// 指挥台要在"不进会话"的前提下渲染审批卡，需要 pending permission 的选项和
  /// tool call id——这些只在快照里有。但这**不需要订阅**：网关的 `loadHistory`
  /// 落到 smeltd 的 `acp_snapshot`，那边自己开一条连接读取，跟订阅互不干扰；
  /// 而 `respondApproval` 本来就带显式 sessionId。所以"看得见 + 批得掉"两件事
  /// 都不依赖当前订阅的是谁。
  ///
  /// 只取尾部一小段：快照无论请求哪个区间都会带上完整的 pending_permissions /
  /// pending_elicitation（见 smelt-core `to_snapshot_range`），卡片要的就是这个，
  /// 没必要把整段历史拉到手机上。
  bool fetchPendingActions(String sessionId) {
    if (_state != WsState.connected || sessionId.isEmpty) return false;
    _send({
      'method': 'loadHistory',
      'params': {
        'sessionId': sessionId,
        // smeltd 会 `before.min(entries.len())`，给一个不可能达到的下标即"从末尾取"。
        'beforeOffset': _tailProbeOffset,
        'limit': _pendingActionsTailLimit,
      },
    });
    return true;
  }

  /// 取消订阅
  void unsubscribe() {
    if (_subscribedSessionId != null) {
      _send({
        'method': 'unsubscribe',
        'params': {'sessionId': _subscribedSessionId},
      });
      _subscribedSessionId = null;
    }
  }

  /// 发送消息
  String sendMessage(
    String sessionId,
    String content, {
    List<AcpImageData> images = const [],
    String? requestId,
  }) {
    requestId ??= createMessageRequestId();
    if (_channel == null || _state != WsState.connected) {
      _messageSendController.add(
        MessageSendResult(
          requestId: requestId,
          ok: false,
          error: 'Desktop is not connected',
        ),
      );
      return requestId;
    }
    _pendingMessageRequests.add(requestId);
    _messageAckTimers[requestId]?.cancel();
    _messageAckTimers[requestId] = Timer(messageAckTimeout, () {
      _messageAckTimers.remove(requestId);
      if (_pendingMessageRequests.remove(requestId)) {
        _messageSendController.add(
          MessageSendResult(
            requestId: requestId!,
            ok: false,
            error: 'Desktop did not confirm delivery in time',
          ),
        );
      }
    });
    _send({
      'method': 'sendMessage',
      'params': {
        'sessionId': sessionId,
        'requestId': requestId,
        'content': content,
        'images': images.map((image) => image.toJson()).toList(),
      },
    });
    return requestId;
  }

  String createMessageRequestId() {
    final random = Random.secure();
    final entropy = List<int>.generate(16, (_) => random.nextInt(256));
    return base64UrlEncode(entropy).replaceAll('=', '');
  }

  void cancelTurn(String sessionId) {
    _send({
      'method': 'cancelTurn',
      'params': {'sessionId': sessionId},
    });
  }

  void setConfigOption(String sessionId, String configId, String valueId) {
    _send({
      'method': 'setConfigOption',
      'params': {
        'sessionId': sessionId,
        'configId': configId,
        'valueId': valueId,
      },
    });
  }

  /// 问一次这场对话加载了哪些技能。旧桌面不支持时直接当作「没有这个能力」，
  /// 由调用方决定不画入口。
  void listSessionSkills(String sessionId) {
    if (!_sessionSkillsSupported) {
      _sessionSkillsController.add(
        SessionSkills(sessionId: sessionId, supported: false),
      );
      return;
    }
    // 没连上就不计数：`_send` 会默默丢掉这条请求，留下的 pending 会把后续别人的
    // `invalid request` 误当成「技能不支持」。重连后由会话页再问一次。
    if (_state != WsState.connected) return;
    _pendingSkillsRequests += 1;
    _send({
      'method': 'listSessionSkills',
      'params': {'sessionId': sessionId},
    });
  }

  /// 响应权限请求
  void respondApproval(
    String sessionId,
    String toolCallId,
    String optionKey, {
    String? customText,
  }) {
    _send({
      'method': 'respondApproval',
      'params': {
        'sessionId': sessionId,
        'toolCallId': toolCallId,
        'optionKey': optionKey,
        'customText': ?customText,
      },
    });
  }

  void chooseElicitation(String sessionId, int fieldIndex, int optionIndex) {
    _send({
      'method': 'chooseElicitation',
      'params': {
        'sessionId': sessionId,
        'fieldIndex': fieldIndex,
        'optionIndex': optionIndex,
      },
    });
  }

  void updateElicitationText(String sessionId, int fieldIndex, String value) {
    _send({
      'method': 'updateElicitationText',
      'params': {
        'sessionId': sessionId,
        'fieldIndex': fieldIndex,
        'value': value,
      },
    });
  }

  void submitElicitation(String sessionId) {
    _send({
      'method': 'submitElicitation',
      'params': {'sessionId': sessionId},
    });
  }

  void dismissElicitation(String sessionId) {
    _send({
      'method': 'dismissElicitation',
      'params': {'sessionId': sessionId},
    });
  }

  void markRead(String sessionId) {
    _send({
      'method': 'markRead',
      'params': {'sessionId': sessionId},
    });
  }

  void _send(Map<String, dynamic> message) {
    if (_channel != null && _state == WsState.connected) {
      _channel!.sink.add(jsonEncode(message));
    }
  }

  void _setState(WsState newState) {
    if (newState == WsState.connected) {
      _connectWatchdog?.cancel();
      _connectWatchdog = null;
      _everConnected = true;
      _reconnectAttempts = 0;
      _outageErrorReported = false;
      _startMetrics();
    } else {
      _metricsTimer?.cancel();
      _metricsTimer = null;
      _updateMetrics(const ConnectionMetrics());
    }
    _state = newState;
    _stateController.add(newState);
  }

  void _startMetrics() {
    _metricsTimer?.cancel();
    _pingSupported = true;
    _hasPongLatency = false;
    _pendingPingSentAt = null;
    // 重连可能接到另一台（或已升级的）电脑，能力判定跟着连接重算。
    _sessionSkillsSupported = true;
    _pendingSkillsRequests = 0;
    _sampleMetrics();
    _metricsTimer = Timer.periodic(metricsInterval, (_) => _sampleMetrics());
  }

  void _sampleMetrics() {
    if (_state != WsState.connected) return;
    // 上一个 ping 迟迟没有回应 = 连接已经死了，只是 TCP 还没告诉我们。
    // 这里必须主动把它判死并重连，否则 `_pendingPingSentAt` 永远非空，
    // 下面的分支再也不会发出新的 ping，心跳就此停摆。
    final pendingSince = _pendingPingSentAt;
    if (pendingSince != null &&
        DateTime.now().millisecondsSinceEpoch - pendingSince >
            pongTimeout.inMilliseconds) {
      _reportConnectionFailure('与桌面端失去响应');
      _scheduleReconnect();
      return;
    }
    if (_pingSupported && _pendingPingSentAt == null) {
      final sentAt = DateTime.now().millisecondsSinceEpoch;
      _pendingPingSentAt = sentAt;
      _send({
        'method': 'ping',
        'params': {'sentAtMs': sentAt},
      });
    }

    final endpoint = _endpoint;
    if (endpoint == null) return;
    final uri = Uri.tryParse(endpoint);
    if (uri?.scheme == PairingConfig.irohScheme) {
      final generation = _connectionGeneration;
      unawaited(_sampleIrohPath(generation));
      return;
    }
    _updateMetrics(
      ConnectionMetrics(
        kind: _isLanHost(uri?.host)
            ? ConnectionPathKind.lan
            : ConnectionPathKind.direct,
        latencyMs: _metrics.latencyMs,
      ),
    );
  }

  Future<void> _sampleIrohPath(int generation) async {
    try {
      final sample = await irohPathProbe();
      if (sample == null ||
          generation != _connectionGeneration ||
          _state != WsState.connected) {
        return;
      }
      _updateMetrics(
        ConnectionMetrics(
          kind: sample.kind,
          latencyMs: _hasPongLatency ? _metrics.latencyMs : sample.rttMs,
        ),
      );
    } catch (_) {
      // Path observation is diagnostic only and must never affect the session.
    }
  }

  void _updateMetrics(ConnectionMetrics next) {
    if (_metrics.kind == next.kind && _metrics.latencyMs == next.latencyMs) {
      return;
    }
    _metrics = next;
    _metricsController.add(next);
  }

  /// 一次连接尝试失败后的收尾：从没连通过的地址直接回断开态（让用户改地址），
  /// 只有掉线重连才值得自动重试。
  void _failConnection() {
    if (_everConnected) {
      _scheduleReconnect();
      return;
    }
    _teardownSocket();
    _setState(WsState.disconnected);
  }

  void _enqueueMessage(dynamic data, int generation) {
    if (data is! String) return;
    _messageQueue = _messageQueue.then((_) => _onMessage(data, generation));
  }

  Future<void> _onMessage(String data, int generation) async {
    try {
      final json = data.length >= 32 * 1024
          ? await compute(_decodeGatewayJson, data)
          : _decodeGatewayJson(data);
      if (generation != _connectionGeneration) return;
      final type = json['type'] as String?;

      switch (type) {
        case 'connected':
          _writeEnabled = json['writeEnabled'] as bool? ?? false;
          _setState(WsState.connected);
          listSessions();
          listWorkspace();
          // 会话行要拿目录里的定义把 `agent_definition_id` 配成智能体名，所以
          // 这一份跟工作区一起在连上时就拉，不等用户切到「智能体」栏。
          listAutomations();
          final sessionId = _subscribedSessionId;
          if (sessionId != null) subscribe(sessionId);

        case 'pong':
          final sentAt = json['sentAtMs'] as int?;
          if (sentAt != null) {
            if (_pendingPingSentAt == sentAt) _pendingPingSentAt = null;
            final latency = DateTime.now().millisecondsSinceEpoch - sentAt;
            if (latency >= 0 && latency < 60000) {
              _hasPongLatency = true;
              _updateMetrics(
                ConnectionMetrics(kind: _metrics.kind, latencyMs: latency),
              );
            }
          }

        case 'sessions':
          final sessions =
              (json['sessions'] as List<dynamic>?)
                  ?.map(
                    (s) => SessionSummary.fromJson(s as Map<String, dynamic>),
                  )
                  .toList() ??
              [];
          _lastSessions = sessions;
          _cachedAt = DateTime.now();
          _sessionsAreCached = false;
          _sessionsController.add(sessions);
          final namespace = _cacheNamespace;
          if (namespace != null) {
            _ignoreCacheFailure(cacheStore?.saveSessions(namespace, sessions));
          }

        case 'workspace':
          final projects =
              (json['projects'] as List<dynamic>? ?? const [])
                  .whereType<Map<String, dynamic>>()
                  .map(WorkspaceProject.fromJson)
                  .toList()
                ..sort((a, b) => a.order.compareTo(b.order));
          final agents = (json['agents'] as List<dynamic>? ?? const [])
              .whereType<Map<String, dynamic>>()
              .map(AcpAgentOption.fromJson)
              .toList();
          final launchActions =
              (json['launchActions'] as List<dynamic>? ?? const [])
                  .whereType<Map<String, dynamic>>()
                  .map(LaunchAction.fromJson)
                  .nonNulls
                  .toList();
          _workspaceController.add(
            WorkspaceCatalog(
              projects: projects,
              agents: agents,
              launchActions: launchActions,
            ),
          );

        case 'automations':
          _lastAutomationCatalog = AutomationCatalog(
            automations: (json['automations'] as List<dynamic>? ?? const [])
                .whereType<Map<String, dynamic>>()
                .map(AutomationSummary.fromJson)
                .toList(),
            agents: (json['agents'] as List<dynamic>? ?? const [])
                .whereType<Map<String, dynamic>>()
                .map(AgentDefinitionSummary.fromJson)
                .toList(),
          );
          _automationCatalogController.add(_lastAutomationCatalog!);

        case 'sessionSkills':
          if (_pendingSkillsRequests > 0) _pendingSkillsRequests -= 1;
          _sessionSkillsController.add(
            SessionSkills(
              sessionId: json['sessionId'] as String? ?? '',
              supported: json['supported'] as bool? ?? false,
              skills: (json['skills'] as List<dynamic>? ?? const [])
                  .whereType<Map<String, dynamic>>()
                  .map(SessionSkill.fromJson)
                  .toList(),
            ),
          );

        case 'sessionHistory':
          _sessionHistoryController.add(
            SessionHistoryResult(
              projectRoot: json['projectRoot'] as String? ?? '',
              agentOptionId: json['agentOptionId'] as String? ?? '',
              sessions: (json['sessions'] as List<dynamic>? ?? const [])
                  .whereType<Map<String, dynamic>>()
                  .map(HistorySessionSummary.fromJson)
                  .toList(),
            ),
          );

        case 'sessionHistoryRenamed':
          final resumeId = json['resumeId'] as String? ?? '';
          if (resumeId.isNotEmpty) {
            _sessionHistoryRenameController.add(
              SessionHistoryRenameResult(
                projectRoot: json['projectRoot'] as String? ?? '',
                agentOptionId: json['agentOptionId'] as String? ?? '',
                resumeId: resumeId,
                title: json['title'] as String? ?? '',
                customTitle: json['customTitle'] as String?,
              ),
            );
            // 会话列表里可能挂着从这条历史续接出来的会话，标题跟着一起变。
            listSessions();
          }

        case 'sessionCreated':
          final sessionId = json['sessionId'] as String?;
          if (sessionId != null && sessionId.isNotEmpty) {
            _sessionCreatedController.add(sessionId);
            listSessions();
          }

        case 'sessionDeleted':
          final sessionId = json['sessionId'] as String?;
          if (sessionId != null && sessionId.isNotEmpty) {
            _snapshotCache.remove(sessionId);
            _cachedSnapshotIds.remove(sessionId);
            _snapshotCacheTimers.remove(sessionId)?.cancel();
            final namespace = _cacheNamespace;
            if (namespace != null) {
              _ignoreCacheFailure(
                cacheStore?.deleteSnapshot(namespace, sessionId),
              );
            }
            _sessionDeletedController.add(sessionId);
            listSessions();
          }

        case 'messageSent':
          final requestId = _takeMessageRequest(
            json['requestId'] as String?,
            allowSingleFallback: true,
          );
          if (requestId != null) {
            _messageSendController.add(
              MessageSendResult(requestId: requestId, ok: true),
            );
          }

        case 'subscribed':
          // 订阅确认
          break;

        case 'unsubscribed':
          // Local state is cleared when sending unsubscribe. A delayed ack for
          // session A must not erase a newer subscription to session B.
          break;

        case 'snapshot':
          // 旧格式兼容
          _publishSnapshot(
            AcpSnapshot.fromJson(json),
            sessionId: json['sessionId'] as String?,
          );

        case 'attention':
          final item = json['item'];
          if (item is Map<String, dynamic>) {
            _attentionController.add(LifecycleAttention.fromJson(item));
          }

        case 'attentionResolved':
          final sessionId = json['sessionId'] as String?;
          if (sessionId != null && sessionId.isNotEmpty) {
            _attentionResolvedController.add(sessionId);
          }

        case 'error':
          final error = json['error'] as String? ?? 'Gateway 请求失败';
          final requestId = _takeMessageRequest(json['requestId'] as String?);
          if (requestId != null) {
            _messageSendController.add(
              MessageSendResult(requestId: requestId, ok: false, error: error),
            );
            break;
          }
          if (error == 'invalid request' && _pendingPingSentAt != null) {
            // Older desktop builds do not know the diagnostic ping method.
            // Keep the iroh QUIC RTT and do not surface a protocol-version
            // mismatch as a user-facing session error.
            _pingSupported = false;
            _pendingPingSentAt = null;
            _hasPongLatency = false;
            break;
          }
          if (error == 'invalid request' && _pendingSkillsRequests > 0) {
            // 同理：旧桌面还没有 `listSessionSkills`。告诉界面「没这能力」，它会把
            // 技能入口收起来，而不是报一个用户无法处理的错误。
            _sessionSkillsSupported = false;
            _pendingSkillsRequests = 0;
            _sessionSkillsController.add(
              const SessionSkills(sessionId: '', supported: false),
            );
            break;
          }
          final sessionId = _subscribedSessionId;
          if (sessionId != null) _historyLoads.remove(sessionId);
          _errorController.add(error);

        default:
          // 可能是原始 smeltd 格式: {"snapshot": {...}}
          if (json.containsKey('snapshot')) {
            _publishSnapshot(
              AcpSnapshot.fromJson(json),
              sessionId: json['sessionId'] as String?,
            );
          }
      }
    } catch (e) {
      _errorController.add('解析消息失败: $e');
    }
  }

  String? _takeMessageRequest(
    String? requestId, {
    bool allowSingleFallback = false,
  }) {
    if (requestId != null && requestId.isNotEmpty) {
      if (!_pendingMessageRequests.remove(requestId)) return null;
      _messageAckTimers.remove(requestId)?.cancel();
      return requestId;
    }
    if (allowSingleFallback && _pendingMessageRequests.length == 1) {
      final only = _pendingMessageRequests.first;
      _pendingMessageRequests.remove(only);
      _messageAckTimers.remove(only)?.cancel();
      return only;
    }
    return null;
  }

  void _publishSnapshot(AcpSnapshot incoming, {String? sessionId}) {
    sessionId ??= _subscribedSessionId;
    if (sessionId == null) return;
    final previous = _snapshotCache.remove(sessionId);
    final merged = previous?.merge(incoming) ?? incoming;
    _snapshotCache[sessionId] = merged;
    _cachedSnapshotIds.remove(sessionId);
    if (incoming.entriesOffset <
        (previous?.entriesOffset ?? incoming.entriesOffset + 1)) {
      _historyLoads.remove(sessionId);
    }
    _trimSnapshotCache();
    _scheduleSnapshotPersistence(sessionId, merged);
    if (_subscribedSessionId == sessionId) {
      _snapshotController.add(merged);
    }
    // 后台会话的详情也要通知出去，指挥台靠这个刷新审批卡。
    _snapshotCacheController.add(sessionId);
  }

  void _scheduleSnapshotPersistence(String sessionId, AcpSnapshot snapshot) {
    final namespace = _cacheNamespace;
    if (namespace == null || cacheStore == null) return;
    _snapshotCacheTimers.remove(sessionId)?.cancel();
    _snapshotCacheTimers[sessionId] = Timer(
      const Duration(milliseconds: 500),
      () {
        _snapshotCacheTimers.remove(sessionId);
        _ignoreCacheFailure(
          cacheStore!.saveSnapshot(namespace, sessionId, snapshot),
        );
      },
    );
  }

  void _ignoreCacheFailure(Future<void>? operation) {
    if (operation == null) return;
    unawaited(operation.catchError((_) {}));
  }

  void _trimSnapshotCache() {
    var bytes = _snapshotCache.values.fold<int>(
      0,
      (total, snapshot) => total + _estimateSnapshotBytes(snapshot),
    );
    while (_snapshotCache.length > _maxCachedSessions ||
        (bytes > _maxCacheBytes && _snapshotCache.length > 1)) {
      final oldest = _snapshotCache.keys.first;
      final removed = _snapshotCache.remove(oldest)!;
      _historyLoads.remove(oldest);
      bytes -= _estimateSnapshotBytes(removed);
    }
  }

  void _clearSnapshotCache() {
    for (final timer in _snapshotCacheTimers.values) {
      timer.cancel();
    }
    _snapshotCacheTimers.clear();
    _snapshotCache.clear();
    _cachedSnapshotIds.clear();
    _historyLoads.clear();
  }

  void _onError(dynamic error) {
    _reportConnectionFailure('WebSocket 错误: $error');
    _failConnection();
  }

  void _onDone() {
    _failConnection();
  }

  void _scheduleReconnect() {
    if (!_manuallyDisconnected && _endpoint != null && _token != null) {
      _teardownSocket(preserveSubscription: true);
      _setState(WsState.reconnecting);
      final delay = _nextReconnectDelay();
      _reconnectAttempts++;
      _reconnectTimer = Timer(delay, () {
        if (_state == WsState.reconnecting) {
          connect(_endpoint!, _token!);
        }
      });
    } else {
      _setState(WsState.disconnected);
    }
  }

  Duration _nextReconnectDelay() {
    final shift = _reconnectAttempts.clamp(0, 4);
    final milliseconds = reconnectDelay.inMilliseconds * (1 << shift);
    return Duration(milliseconds: milliseconds.clamp(0, 30000));
  }

  void _reportConnectionFailure(String message) {
    if (_manuallyDisconnected) return;
    if (_everConnected) {
      if (_outageErrorReported) return;
      _outageErrorReported = true;
      _errorController.add('$message；正在自动重连');
      return;
    }
    _errorController.add(message);
  }

  void dispose() {
    disconnect();
    _stateController.close();
    _sessionsController.close();
    _workspaceController.close();
    _automationCatalogController.close();
    _sessionHistoryController.close();
    _sessionSkillsController.close();
    _sessionHistoryRenameController.close();
    _sessionCreatedController.close();
    _sessionDeletedController.close();
    _snapshotController.close();
    _attentionController.close();
    _attentionResolvedController.close();
    _snapshotCacheController.close();
    _errorController.close();
    _metricsController.close();
    _messageSendController.close();
  }
}

bool _isLanHost(String? host) {
  if (host == null || host.isEmpty) return false;
  if (host == 'localhost') return true;
  final address = InternetAddress.tryParse(host);
  if (address == null) return false;
  if (address.isLoopback || address.isLinkLocal) return true;
  final bytes = address.rawAddress;
  if (address.type == InternetAddressType.IPv4) {
    return bytes[0] == 10 ||
        (bytes[0] == 172 && bytes[1] >= 16 && bytes[1] <= 31) ||
        (bytes[0] == 192 && bytes[1] == 168);
  }
  return (bytes[0] & 0xfe) == 0xfc;
}

int _estimateSnapshotBytes(AcpSnapshot snapshot) {
  var bytes = 2048;
  for (final entry in snapshot.entries) {
    bytes += switch (entry) {
      AcpEntryUser(text: final text) => text.length * 2 + 64,
      AcpEntryUserWithImages(text: final text, images: final images) =>
        text.length * 2 +
            images.fold<int>(0, (sum, image) => sum + image.base64.length) +
            128,
      AcpEntryAssistant(text: final text) => text.length * 2 + 64,
      AcpEntryToolCall(title: final title, output: final output) =>
        title.length * 2 +
            output.fold<int>(
              0,
              (sum, part) => sum + _estimateOutputBytes(part),
            ) +
            192,
      AcpEntryDivider(label: final label) => label.length * 2 + 32,
      AcpEntryUnknown() => 16,
    };
  }
  return bytes;
}

int _estimateOutputBytes(ToolOutputPart part) => switch (part) {
  ToolOutputText(text: final text) => text.length * 2 + 32,
  ToolOutputDiff(
    path: final path,
    oldText: final oldText,
    newText: final newText,
  ) =>
    (path.length + (oldText?.length ?? 0) + newText.length) * 2 + 64,
  ToolOutputImage(base64: final base64) => base64.length + 64,
};

/// 全局单例
final gatewayService = GatewayService(cacheStore: FileSessionCacheStore());
