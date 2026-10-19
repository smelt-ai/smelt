/// 运行时内核的调用面。
///
/// 界面和 `RuntimeService` 只认 [RuntimeBackend]；生产实现 [FrbRuntimeBackend]
/// 直接转给 flutter_rust_bridge 生成的函数。单测注入假的实现，不加载 Rust 动态库。
library;

import '../models/runtime_models.dart';
import '../src/rust/api/catalog.dart' as catalog;
import '../src/rust/api/connection.dart' as connection;
import '../src/rust/api/conversation.dart' as conversation;
import '../src/rust/api/identity.dart' as identity;

abstract interface class RuntimeBackend {
  // 身份与配对
  PairingTarget parsePairing(String uri);
  Future<RuntimeInfo> pair(String uri, String deviceName);

  // 连接
  Future<RuntimeInfo> connect(String endpointId, String? relayUrl);
  Future<void> disconnect();
  Future<void> verifyConnection();
  Stream<ConnectionEvent> connectionEvents();

  // 会话目录
  Stream<CatalogEvent> catalogEvents();
  void markRead(String sessionId);
  Future<WorkspaceCatalog> workspaceCatalog();
  Future<String> createSession({
    required String projectRoot,
    String? agentOptionId,
    String? launchKey,
    String? resumeId,
    SessionKind? kind,
    String? requestId,
  });
  Future<void> deleteSession(String sessionId);
  Future<AutomationCatalog> automationCatalog();
  Future<AutomationCatalog> setAutomationEnabled(
    String automationId,
    bool enabled,
  );
  Future<AutomationCatalog> runAutomationOnce(String automationId);
  Future<List<HistorySessionSummary>> listHistory(
    String projectRoot,
    String agentOptionId,
  );
  Future<HistoryRenameResult> renameHistory(
    String projectRoot,
    String agentOptionId,
    String resumeId,
    String? title,
  );

  // 对话
  Stream<ConversationEvent> watchConversation(String sessionId);
  Future<void> unwatchConversation();
  Future<bool> loadOlder(String sessionId);
  Future<PendingActions> pendingActions(String sessionId);
  String newRequestId();
  Future<void> sendMessage(
    String sessionId,
    String requestId,
    String text,
    List<ImageInput> images,
  );
  Future<void> cancelTurn(String sessionId);
  Future<void> setConfigOption(
    String sessionId,
    String configId,
    String valueId, {
    bool? boolean,
  });
  Future<void> respondApproval(
    String sessionId,
    String toolCallId,
    String optionId,
  );
  Future<void> chooseElicitation(
    String sessionId,
    int fieldIndex,
    int optionIndex,
  );
  Future<void> submitElicitation(
    String sessionId,
    List<ElicitationTextInput> textValues,
  );
  Future<void> dismissElicitation(String sessionId);
  Future<SessionSkills> listSkills(String sessionId);
  Future<ImageData> loadImage(String sessionId, ImageRef image, {int? maxEdge});
}

/// 生产实现：逐个转给 FRB。必须在 `RustLib.init()` 之后才能调用任何方法。
class FrbRuntimeBackend implements RuntimeBackend {
  const FrbRuntimeBackend();

  @override
  PairingTarget parsePairing(String uri) => identity.parsePairing(uri: uri);

  @override
  Future<RuntimeInfo> pair(String uri, String deviceName) =>
      identity.pair(uri: uri, deviceName: deviceName);

  @override
  Future<RuntimeInfo> connect(String endpointId, String? relayUrl) =>
      connection.connect(endpointId: endpointId, relayUrl: relayUrl);

  @override
  Future<void> disconnect() => connection.disconnect();

  @override
  Future<void> verifyConnection() => connection.verifyConnection();

  @override
  Stream<ConnectionEvent> connectionEvents() => connection.connectionEvents();

  @override
  Stream<CatalogEvent> catalogEvents() => catalog.catalogEvents();

  @override
  void markRead(String sessionId) => catalog.markRead(sessionId: sessionId);

  @override
  Future<WorkspaceCatalog> workspaceCatalog() => catalog.workspaceCatalog();

  @override
  Future<String> createSession({
    required String projectRoot,
    String? agentOptionId,
    String? launchKey,
    String? resumeId,
    SessionKind? kind,
    String? requestId,
  }) => catalog.createSession(
    projectRoot: projectRoot,
    agentOptionId: agentOptionId,
    launchKey: launchKey,
    resumeId: resumeId,
    kind: kind,
    requestId: requestId,
  );

  @override
  Future<void> deleteSession(String sessionId) =>
      catalog.deleteSession(sessionId: sessionId);

  @override
  Future<AutomationCatalog> automationCatalog() => catalog.automationCatalog();

  @override
  Future<AutomationCatalog> setAutomationEnabled(
    String automationId,
    bool enabled,
  ) => catalog.setAutomationEnabled(
    automationId: automationId,
    enabled: enabled,
  );

  @override
  Future<AutomationCatalog> runAutomationOnce(String automationId) =>
      catalog.runAutomationOnce(automationId: automationId);

  @override
  Future<List<HistorySessionSummary>> listHistory(
    String projectRoot,
    String agentOptionId,
  ) => catalog.listHistory(
    projectRoot: projectRoot,
    agentOptionId: agentOptionId,
  );

  @override
  Future<HistoryRenameResult> renameHistory(
    String projectRoot,
    String agentOptionId,
    String resumeId,
    String? title,
  ) => catalog.renameHistory(
    projectRoot: projectRoot,
    agentOptionId: agentOptionId,
    resumeId: resumeId,
    title: title,
  );

  @override
  Stream<ConversationEvent> watchConversation(String sessionId) =>
      conversation.watchConversation(sessionId: sessionId);

  @override
  Future<void> unwatchConversation() => conversation.unwatchConversation();

  @override
  Future<bool> loadOlder(String sessionId) =>
      conversation.loadOlder(sessionId: sessionId);

  @override
  Future<PendingActions> pendingActions(String sessionId) =>
      conversation.pendingActions(sessionId: sessionId);

  @override
  String newRequestId() => conversation.newRequestId();

  @override
  Future<void> sendMessage(
    String sessionId,
    String requestId,
    String text,
    List<ImageInput> images,
  ) => conversation.sendMessage(
    sessionId: sessionId,
    requestId: requestId,
    text: text,
    images: images,
  );

  @override
  Future<void> cancelTurn(String sessionId) =>
      conversation.cancelTurn(sessionId: sessionId);

  @override
  Future<void> setConfigOption(
    String sessionId,
    String configId,
    String valueId, {
    bool? boolean,
  }) => conversation.setConfigOption(
    sessionId: sessionId,
    configId: configId,
    valueId: valueId,
    boolean: boolean,
  );

  @override
  Future<void> respondApproval(
    String sessionId,
    String toolCallId,
    String optionId,
  ) => conversation.respondApproval(
    sessionId: sessionId,
    toolCallId: toolCallId,
    optionId: optionId,
  );

  @override
  Future<void> chooseElicitation(
    String sessionId,
    int fieldIndex,
    int optionIndex,
  ) => conversation.chooseElicitation(
    sessionId: sessionId,
    fieldIndex: fieldIndex,
    optionIndex: optionIndex,
  );

  @override
  Future<void> submitElicitation(
    String sessionId,
    List<ElicitationTextInput> textValues,
  ) => conversation.submitElicitation(
    sessionId: sessionId,
    textValues: textValues,
  );

  @override
  Future<void> dismissElicitation(String sessionId) =>
      conversation.dismissElicitation(sessionId: sessionId);

  @override
  Future<SessionSkills> listSkills(String sessionId) =>
      conversation.listSkills(sessionId: sessionId);

  @override
  Future<ImageData> loadImage(
    String sessionId,
    ImageRef image, {
    int? maxEdge,
  }) => conversation.loadImage(
    sessionId: sessionId,
    entry: image.entry,
    digest: image.digest,
    maxEdge: maxEdge,
  );
}
