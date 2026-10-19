// 单测共用的运行时数据与假 backend。模型都是 FRB 生成的类，构造它们不需要加载
// Rust 动态库；所有调用都打到 [FakeRuntimeBackend]。

import 'dart:async';

import 'package:smelt_mobile/services/runtime_service.dart';

/// 一条会话摘要，没给的字段取「空闲的 ACP 对话」。
SessionSummary session({
  required String id,
  String title = '',
  SessionKind kind = SessionKind.acp,
  SessionPhase phase = SessionPhase.idle,
  SessionStatus status = SessionStatus.idle,
  String agent = 'codex',
  String? cwd,
  String? projectRoot,
  String? projectTitle,
  int projectOrder = 0xffffffff,
  int updatedAt = 0,
  String? detail,
  AttentionItem? attention,
  AutomationSource? automation,
  String? agentDefinitionId,
}) => SessionSummary(
  id: id,
  kind: kind,
  title: title,
  phase: phase,
  status: status,
  agent: agent,
  cwd: cwd,
  projectRoot: projectRoot,
  projectTitle: projectTitle,
  projectOrder: projectOrder,
  updatedAt: updatedAt,
  detail: detail,
  attention: attention,
  automation: automation,
  agentDefinitionId: agentDefinitionId,
);

AttentionItem attention(
  String sessionId,
  AttentionKind kind, {
  String title = 'Attention',
  String message = '',
}) => AttentionItem(
  sessionId: sessionId,
  title: title,
  message: message,
  kind: kind,
);

const commandApproval = PermissionRequest(
  toolCallId: 'tool-1',
  question: 'Run this command?',
  options: [
    PermissionOption(
      optionId: 'reject',
      name: 'Deny',
      kind: PermissionOptionKind.rejectOnce,
    ),
    PermissionOption(
      optionId: 'allow',
      name: 'Allow',
      kind: PermissionOptionKind.allowOnce,
    ),
    PermissionOption(
      optionId: 'always',
      name: 'Always allow',
      kind: PermissionOptionKind.allowAlways,
    ),
  ],
  details: ApprovalDetails(
    kind: ApprovalKind.command,
    command: 'cargo test -p smelt-ui --no-fail-fast',
    cwd: '~/code/smelt',
  ),
);

const emptyCatalog = AutomationCatalog(automations: [], agents: []);

RuntimeInfo runtimeInfo({
  String runtimeId = 'runtime-a',
  String? hostname = 'office-mac',
  String endpointId = 'endpoint-a',
  String? relayUrl,
}) => RuntimeInfo(
  runtimeId: runtimeId,
  hostname: hostname,
  version: '1.0.0',
  endpointId: endpointId,
  relayUrl: relayUrl,
);

/// 记录调用、由测试推事件的假 backend。
class FakeRuntimeBackend implements RuntimeBackend {
  final connection = StreamController<ConnectionEvent>.broadcast();
  final catalog = StreamController<CatalogEvent>.broadcast();

  /// 每次 [watchConversation] 新开的流，按调用顺序。
  final watches =
      <({String sessionId, StreamController<ConversationEvent> events})>[];
  final calls = <String>[];

  WorkspaceCatalog workspace = emptyWorkspaceCatalog;
  AutomationCatalog automations = emptyCatalog;
  PendingActions Function(String sessionId) pending = (_) =>
      const PendingActions(permissions: []);
  Object? sendError;
  RuntimeInfo? pairResult;
  Object? pairError;
  Object? connectError;

  void _record(String call) => calls.add(call);

  @override
  PairingTarget parsePairing(String uri) {
    if (!uri.startsWith('smelt+iroh://')) {
      throw const SmeltError(
        kind: SmeltErrorKind.invalidPairing,
        message: 'Not a Smelt pairing code',
      );
    }
    return const PairingTarget(endpointId: 'endpoint-a');
  }

  @override
  Future<RuntimeInfo> pair(String uri, String deviceName) async {
    _record('pair $uri');
    if (pairError case final error?) throw error;
    return pairResult ?? runtimeInfo();
  }

  @override
  Future<RuntimeInfo> connect(String endpointId, String? relayUrl) async {
    _record('connect $endpointId');
    if (connectError case final error?) throw error;
    return runtimeInfo(endpointId: endpointId, relayUrl: relayUrl);
  }

  @override
  Future<void> disconnect() async => _record('disconnect');

  @override
  Future<void> verifyConnection() async => _record('verify');

  @override
  Stream<ConnectionEvent> connectionEvents() => connection.stream;

  @override
  Stream<CatalogEvent> catalogEvents() => catalog.stream;

  @override
  void markRead(String sessionId) => _record('markRead $sessionId');

  @override
  Future<WorkspaceCatalog> workspaceCatalog() async {
    _record('workspace');
    return workspace;
  }

  @override
  Future<String> createSession({
    required String projectRoot,
    String? agentOptionId,
    String? launchKey,
    String? resumeId,
    SessionKind? kind,
    String? requestId,
  }) async {
    _record('create $projectRoot ${launchKey ?? agentOptionId}');
    return 'created';
  }

  @override
  Future<void> deleteSession(String sessionId) async =>
      _record('delete $sessionId');

  @override
  Future<AutomationCatalog> automationCatalog() async {
    _record('automations');
    return automations;
  }

  @override
  Future<AutomationCatalog> setAutomationEnabled(
    String automationId,
    bool enabled,
  ) async {
    _record('enable $automationId $enabled');
    return automations;
  }

  @override
  Future<AutomationCatalog> runAutomationOnce(String automationId) async {
    _record('run $automationId');
    return automations;
  }

  @override
  Future<List<HistorySessionSummary>> listHistory(
    String projectRoot,
    String agentOptionId,
  ) async => const [];

  @override
  Future<HistoryRenameResult> renameHistory(
    String projectRoot,
    String agentOptionId,
    String resumeId,
    String? title,
  ) async => HistoryRenameResult(title: title ?? '', customTitle: title);

  @override
  Stream<ConversationEvent> watchConversation(String sessionId) {
    _record('watch $sessionId');
    final events = StreamController<ConversationEvent>.broadcast();
    watches.add((sessionId: sessionId, events: events));
    return events.stream;
  }

  @override
  Future<void> unwatchConversation() async => _record('unwatch');

  @override
  Future<bool> loadOlder(String sessionId) async => false;

  @override
  Future<PendingActions> pendingActions(String sessionId) async {
    _record('pending $sessionId');
    return pending(sessionId);
  }

  @override
  String newRequestId() => 'request-${calls.length}';

  @override
  Future<void> sendMessage(
    String sessionId,
    String requestId,
    String text,
    List<ImageInput> images,
  ) async {
    _record('send $sessionId $requestId $text');
    if (sendError case final error?) throw error;
  }

  @override
  Future<void> cancelTurn(String sessionId) async => _record('cancel');

  @override
  Future<void> setConfigOption(
    String sessionId,
    String configId,
    String valueId, {
    bool? boolean,
  }) async => _record('config $configId $valueId $boolean');

  @override
  Future<void> respondApproval(
    String sessionId,
    String toolCallId,
    String optionId,
  ) async => _record('respond $sessionId $toolCallId $optionId');

  @override
  Future<void> chooseElicitation(
    String sessionId,
    int fieldIndex,
    int optionIndex,
  ) async => _record('choose $fieldIndex $optionIndex');

  @override
  Future<void> submitElicitation(
    String sessionId,
    List<ElicitationTextInput> textValues,
  ) async => _record('submit ${textValues.length}');

  @override
  Future<void> dismissElicitation(String sessionId) async => _record('dismiss');

  @override
  Future<SessionSkills> listSkills(String sessionId) async =>
      const SessionSkills(supported: false, skills: []);

  @override
  Future<ImageData> loadImage(
    String sessionId,
    ImageRef image, {
    int? maxEdge,
  }) async =>
      throw const SmeltError(kind: SmeltErrorKind.offline, message: 'offline');

  Future<void> close() async {
    await connection.close();
    await catalog.close();
    for (final watch in watches) {
      await watch.events.close();
    }
  }
}
