/// 内核的调用面：界面只认 [Backend]。
///
/// 生产实现 [FrbBackend] 一比一转给 flutter_rust_bridge 生成的函数；测试和截图审查用
/// `DemoBackend`（合成数据，不加载 Rust 动态库）。这一层不做任何业务判断——协议解析、
/// 跨机器汇总、分段排序都在 Rust 内核（`crates/smelt-mobile`）。
library;

import 'dart:async';
import 'dart:typed_data';

import '../src/rust/api/catalog.dart' as catalog;
import '../src/rust/api/conversation.dart' as conversation;
import '../src/rust/api/draft.dart' as draft;
import '../src/rust/api/fleet.dart' as fleet;
import '../src/rust/api/identity.dart' as identity;
import '../src/rust/api/pass.dart' as pass;
import '../src/rust/api/terminal.dart' as terminal;
import 'models.dart';

abstract interface class Backend {
  // 身份与配对 ---------------------------------------------------------------
  PairingTarget parsePairing(String uri);
  Future<RuntimeRecord> pair(String uri, String deviceName);

  // 机队 ---------------------------------------------------------------------
  Stream<FleetView> fleetEvents();
  Future<void> renameRuntime(String runtimeId, String name);
  Future<void> setRuntimeAccent(String runtimeId, int accent);
  Future<void> moveRuntime(String runtimeId, int toIndex);
  Future<void> forgetRuntime(String runtimeId);
  Future<void> reconnectRuntime(String runtimeId);
  Future<void> verifyConnections();

  /// 一次性迁移旧版钥匙串里的机器列表。
  Future<List<RuntimeRecord>> importRuntimes(List<RuntimeImport> runtimes);

  // 指挥台投影 ---------------------------------------------------------------
  Stream<Pass> passEvents();

  // 会话目录 -----------------------------------------------------------------
  Stream<CatalogEvent> catalogEvents();
  void markRead(String runtimeId, String sessionId);
  Future<WorkspaceCatalog> workspaceCatalog(String runtimeId);
  Future<String> createSession(
    String runtimeId, {
    required String projectRoot,
    String? agentOptionId,
    String? launchKey,
    String? resumeId,
    SessionKind? kind,
    String? requestId,
  });
  Future<void> deleteSession(String runtimeId, String sessionId);
  Future<AutomationCatalog> automationCatalog(String runtimeId);
  Future<AutomationCatalog> setAutomationEnabled(
    String runtimeId,
    String automationId,
    bool enabled,
  );
  Future<AutomationCatalog> runAutomationOnce(
    String runtimeId,
    String automationId,
  );
  Future<List<HistorySessionSummary>> listHistory(
    String runtimeId,
    String projectRoot,
    String agentOptionId,
  );
  Future<HistoryRenameResult> renameHistory(
    String runtimeId, {
    required String projectRoot,
    required String agentOptionId,
    required String resumeId,
    String? title,
  });

  // 对话 ---------------------------------------------------------------------
  /// 订阅一个会话。每次调用是一个独立的订阅者：同一会话可以同时有几个（新页先订、旧页
  /// 后退），取消流就退掉自己这一个，不影响别人。
  Stream<ConversationEvent> watchConversation(
    String runtimeId,
    String sessionId,
  );
  Future<bool> loadOlder(String runtimeId, String sessionId);
  Future<PendingActions> pendingActions(String runtimeId, String sessionId);
  String newRequestId();
  Future<void> sendMessage(
    String runtimeId,
    String sessionId, {
    required String requestId,
    required String text,
    required List<ImageInput> images,
  });
  Future<void> cancelTurn(String runtimeId, String sessionId);
  Future<void> setConfigOption(
    String runtimeId,
    String sessionId, {
    required String configId,
    required String valueId,
    bool? boolean,
  });
  Future<void> respondApproval(
    String runtimeId,
    String sessionId, {
    required String toolCallId,
    required String optionId,
  });
  Future<void> chooseElicitation(
    String runtimeId,
    String sessionId, {
    required int fieldIndex,
    required int optionIndex,
  });
  Future<void> updateElicitationText(
    String runtimeId,
    String sessionId, {
    required int fieldIndex,
    required String value,
  });
  Future<void> submitElicitation(
    String runtimeId,
    String sessionId,
    List<ElicitationTextInput> textValues,
  );
  Future<void> dismissElicitation(String runtimeId, String sessionId);
  Future<SessionSkills> listSkills(String runtimeId, String sessionId);
  Future<ImageData> loadImage(
    String runtimeId,
    String sessionId, {
    required int entry,
    required String digest,
    int? maxEdge,
  });

  // 终端 ---------------------------------------------------------------------
  Stream<TerminalEvent> terminalAttach(
    String runtimeId,
    String sessionId, {
    required TerminalGeometry geometry,
    int? maxScrollbackLines,
  });
  void terminalResize(
    String runtimeId,
    String sessionId,
    TerminalGeometry geometry,
  );
  void terminalInput(String runtimeId, String sessionId, String data);
  void terminalDetach(String runtimeId, String sessionId);

  // 草稿 ---------------------------------------------------------------------
  Future<MessageDraft?> loadDraft(String runtimeId, String sessionId);
  Future<void> saveDraft(String runtimeId, String sessionId, MessageDraft d);
  Future<void> clearDraft(String runtimeId, String sessionId);

  /// 一次发送有了结论：草稿还属于这次发送（requestId 相同）才处理——成功删掉，失败清掉
  /// requestId 留着内容。发送途中用户又改了草稿就不动它，返回 false。
  Future<bool> resolveDraft(
    String runtimeId,
    String sessionId, {
    required String requestId,
    required bool succeeded,
  });
}

class FrbBackend implements Backend {
  const FrbBackend();

  @override
  PairingTarget parsePairing(String uri) => identity.parsePairing(uri: uri);

  @override
  Future<RuntimeRecord> pair(String uri, String deviceName) =>
      identity.pair(uri: uri, deviceName: deviceName);

  @override
  Stream<FleetView> fleetEvents() => fleet.fleetEvents();

  @override
  Future<void> renameRuntime(String runtimeId, String name) =>
      fleet.renameRuntime(runtimeId: runtimeId, name: name);

  @override
  Future<void> setRuntimeAccent(String runtimeId, int accent) =>
      fleet.setRuntimeAccent(runtimeId: runtimeId, accent: accent);

  @override
  Future<void> moveRuntime(String runtimeId, int toIndex) =>
      fleet.moveRuntime(runtimeId: runtimeId, toIndex: toIndex);

  @override
  Future<void> forgetRuntime(String runtimeId) =>
      fleet.forgetRuntime(runtimeId: runtimeId);

  @override
  Future<void> reconnectRuntime(String runtimeId) =>
      fleet.reconnectRuntime(runtimeId: runtimeId);

  @override
  Future<void> verifyConnections() => fleet.verifyConnections();

  @override
  Future<List<RuntimeRecord>> importRuntimes(List<RuntimeImport> runtimes) =>
      fleet.importRuntimes(runtimes: runtimes);

  @override
  Stream<Pass> passEvents() => pass.passEvents();

  @override
  Stream<CatalogEvent> catalogEvents() => catalog.catalogEvents();

  @override
  void markRead(String runtimeId, String sessionId) =>
      catalog.markRead(runtimeId: runtimeId, sessionId: sessionId);

  @override
  Future<WorkspaceCatalog> workspaceCatalog(String runtimeId) =>
      catalog.workspaceCatalog(runtimeId: runtimeId);

  @override
  Future<String> createSession(
    String runtimeId, {
    required String projectRoot,
    String? agentOptionId,
    String? launchKey,
    String? resumeId,
    SessionKind? kind,
    String? requestId,
  }) => catalog.createSession(
    runtimeId: runtimeId,
    projectRoot: projectRoot,
    agentOptionId: agentOptionId,
    launchKey: launchKey,
    resumeId: resumeId,
    kind: kind,
    requestId: requestId,
  );

  @override
  Future<void> deleteSession(String runtimeId, String sessionId) =>
      catalog.deleteSession(runtimeId: runtimeId, sessionId: sessionId);

  @override
  Future<AutomationCatalog> automationCatalog(String runtimeId) =>
      catalog.automationCatalog(runtimeId: runtimeId);

  @override
  Future<AutomationCatalog> setAutomationEnabled(
    String runtimeId,
    String automationId,
    bool enabled,
  ) => catalog.setAutomationEnabled(
    runtimeId: runtimeId,
    automationId: automationId,
    enabled: enabled,
  );

  @override
  Future<AutomationCatalog> runAutomationOnce(
    String runtimeId,
    String automationId,
  ) => catalog.runAutomationOnce(
    runtimeId: runtimeId,
    automationId: automationId,
  );

  @override
  Future<List<HistorySessionSummary>> listHistory(
    String runtimeId,
    String projectRoot,
    String agentOptionId,
  ) => catalog.listHistory(
    runtimeId: runtimeId,
    projectRoot: projectRoot,
    agentOptionId: agentOptionId,
  );

  @override
  Future<HistoryRenameResult> renameHistory(
    String runtimeId, {
    required String projectRoot,
    required String agentOptionId,
    required String resumeId,
    String? title,
  }) => catalog.renameHistory(
    runtimeId: runtimeId,
    projectRoot: projectRoot,
    agentOptionId: agentOptionId,
    resumeId: resumeId,
    title: title,
  );

  /// 订阅者编号，进程内不重复。
  static int _nextWatchId = 0;

  @override
  Stream<ConversationEvent> watchConversation(
    String runtimeId,
    String sessionId,
  ) async* {
    final watchId = _nextWatchId++;
    try {
      yield* conversation.watchConversation(
        runtimeId: runtimeId,
        sessionId: sessionId,
        watchId: watchId,
      );
    } finally {
      // 流被取消（或内核那头结束）：退掉自己这个订阅者。
      unawaited(
        conversation
            .unwatchConversation(
              runtimeId: runtimeId,
              sessionId: sessionId,
              watchId: watchId,
            )
            .catchError((Object _) {}),
      );
    }
  }

  @override
  Future<bool> loadOlder(String runtimeId, String sessionId) =>
      conversation.loadOlder(runtimeId: runtimeId, sessionId: sessionId);

  @override
  Future<PendingActions> pendingActions(String runtimeId, String sessionId) =>
      conversation.pendingActions(runtimeId: runtimeId, sessionId: sessionId);

  @override
  String newRequestId() => conversation.newRequestId();

  @override
  Future<void> sendMessage(
    String runtimeId,
    String sessionId, {
    required String requestId,
    required String text,
    required List<ImageInput> images,
  }) => conversation.sendMessage(
    runtimeId: runtimeId,
    sessionId: sessionId,
    requestId: requestId,
    text: text,
    images: images,
  );

  @override
  Future<void> cancelTurn(String runtimeId, String sessionId) =>
      conversation.cancelTurn(runtimeId: runtimeId, sessionId: sessionId);

  @override
  Future<void> setConfigOption(
    String runtimeId,
    String sessionId, {
    required String configId,
    required String valueId,
    bool? boolean,
  }) => conversation.setConfigOption(
    runtimeId: runtimeId,
    sessionId: sessionId,
    configId: configId,
    valueId: valueId,
    boolean: boolean,
  );

  @override
  Future<void> respondApproval(
    String runtimeId,
    String sessionId, {
    required String toolCallId,
    required String optionId,
  }) => conversation.respondApproval(
    runtimeId: runtimeId,
    sessionId: sessionId,
    toolCallId: toolCallId,
    optionId: optionId,
  );

  @override
  Future<void> chooseElicitation(
    String runtimeId,
    String sessionId, {
    required int fieldIndex,
    required int optionIndex,
  }) => conversation.chooseElicitation(
    runtimeId: runtimeId,
    sessionId: sessionId,
    fieldIndex: fieldIndex,
    optionIndex: optionIndex,
  );

  @override
  Future<void> updateElicitationText(
    String runtimeId,
    String sessionId, {
    required int fieldIndex,
    required String value,
  }) => conversation.updateElicitationText(
    runtimeId: runtimeId,
    sessionId: sessionId,
    fieldIndex: fieldIndex,
    value: value,
  );

  @override
  Future<void> submitElicitation(
    String runtimeId,
    String sessionId,
    List<ElicitationTextInput> textValues,
  ) => conversation.submitElicitation(
    runtimeId: runtimeId,
    sessionId: sessionId,
    textValues: textValues,
  );

  @override
  Future<void> dismissElicitation(String runtimeId, String sessionId) =>
      conversation.dismissElicitation(
        runtimeId: runtimeId,
        sessionId: sessionId,
      );

  @override
  Future<SessionSkills> listSkills(String runtimeId, String sessionId) =>
      conversation.listSkills(runtimeId: runtimeId, sessionId: sessionId);

  @override
  Future<ImageData> loadImage(
    String runtimeId,
    String sessionId, {
    required int entry,
    required String digest,
    int? maxEdge,
  }) => conversation.loadImage(
    runtimeId: runtimeId,
    sessionId: sessionId,
    entry: entry,
    digest: digest,
    maxEdge: maxEdge,
  );

  @override
  Stream<TerminalEvent> terminalAttach(
    String runtimeId,
    String sessionId, {
    required TerminalGeometry geometry,
    int? maxScrollbackLines,
  }) => terminal.terminalAttach(
    runtimeId: runtimeId,
    sessionId: sessionId,
    geometry: geometry,
    maxScrollbackLines: maxScrollbackLines,
  );

  @override
  void terminalResize(
    String runtimeId,
    String sessionId,
    TerminalGeometry geometry,
  ) => terminal.terminalResize(
    runtimeId: runtimeId,
    sessionId: sessionId,
    geometry: geometry,
  );

  @override
  void terminalInput(String runtimeId, String sessionId, String data) =>
      terminal.terminalInput(
        runtimeId: runtimeId,
        sessionId: sessionId,
        data: data,
      );

  @override
  void terminalDetach(String runtimeId, String sessionId) =>
      terminal.terminalDetach(runtimeId: runtimeId, sessionId: sessionId);

  @override
  Future<MessageDraft?> loadDraft(String runtimeId, String sessionId) =>
      draft.loadDraft(runtimeId: runtimeId, sessionId: sessionId);

  @override
  Future<void> saveDraft(String runtimeId, String sessionId, MessageDraft d) =>
      draft.saveDraft(runtimeId: runtimeId, sessionId: sessionId, draft: d);

  @override
  Future<void> clearDraft(String runtimeId, String sessionId) =>
      draft.clearDraft(runtimeId: runtimeId, sessionId: sessionId);

  @override
  Future<bool> resolveDraft(
    String runtimeId,
    String sessionId, {
    required String requestId,
    required bool succeeded,
  }) => draft.resolveDraft(
    runtimeId: runtimeId,
    sessionId: sessionId,
    requestId: requestId,
    succeeded: succeeded,
  );
}

/// 图片字节的别名，方便调用方不直接依赖 typed_data。
typedef Bytes = Uint8List;
