/// 手机与运行时之间的薄服务层。
///
/// 只做两件事：把 Rust 内核的事件流和调用转给界面，并留住界面需要的最近一份状态
/// （连接状态与链路、会话列表、工作区目录、自动化目录）。协议解析、对话合并、离线
/// 缓存、attention 投影全在 Rust（`crates/smelt-mobile`），这里一行都不做。
library;

import 'dart:async';

import '../models/runtime_models.dart';
import 'runtime_backend.dart';

export '../models/runtime_models.dart';
export 'runtime_backend.dart' show RuntimeBackend, FrbRuntimeBackend;

class RuntimeService {
  RuntimeService(this._backend);

  final RuntimeBackend _backend;

  StreamSubscription<ConnectionEvent>? _connectionSubscription;
  StreamSubscription<CatalogEvent>? _catalogSubscription;

  ConnectionEvent _connection = const ConnectionEvent(
    state: RuntimeConnectionState.disconnected,
  );

  /// 最近一次要连的运行时。断线后「重试」连的就是它。
  ({String endpointId, String? relayUrl})? _target;

  SessionList _sessionList = const SessionList(sessions: [], cached: true);
  WorkspaceCatalog _workspace = emptyWorkspaceCatalog;
  AutomationCatalog? _automationCatalog;

  /// 对话页的订阅栈。Rust 一次只订阅一个会话；从会话 A 点进会话 B 再返回时，
  /// 要把 A 重新订阅回来，否则 A 停在离开那一刻。
  final List<ConversationWatch> _watches = [];

  final _connectionController = StreamController<ConnectionEvent>.broadcast();
  final _sessionListController = StreamController<SessionList>.broadcast();
  final _workspaceController = StreamController<WorkspaceCatalog>.broadcast();
  final _automationCatalogController =
      StreamController<AutomationCatalog>.broadcast();
  final _attentionController = StreamController<AttentionItem>.broadcast();
  final _attentionResolvedController = StreamController<String>.broadcast();
  final _errorController = StreamController<String>.broadcast();

  /// 开始接收 Rust 的连接与会话目录事件。`RustLib.init()` 之后调一次；重复调用无效。
  void start() {
    if (_connectionSubscription != null) return;
    _connectionSubscription = _backend.connectionEvents().listen(
      _onConnection,
      onError: _reportError,
    );
    _catalogSubscription = _backend.catalogEvents().listen(
      _onCatalog,
      onError: _reportError,
    );
  }

  // ---------------------------------------------------------------------------
  // 状态
  // ---------------------------------------------------------------------------

  /// 连接状态、链路或错误变化。
  Stream<ConnectionEvent> get connectionStream => _connectionController.stream;

  /// 整份会话列表（含是否来自离线缓存）。
  Stream<SessionList> get sessionListStream => _sessionListController.stream;

  Stream<List<SessionSummary>> get sessionsStream =>
      _sessionListController.stream.map((list) => list.sessions);

  Stream<WorkspaceCatalog> get workspaceStream => _workspaceController.stream;

  /// 自动化目录。开关、立即运行的结果也从这里出来——运行时回的是权威目录。
  Stream<AutomationCatalog> get automationCatalogStream =>
      _automationCatalogController.stream;

  /// 新的提醒。
  Stream<AttentionItem> get attentionStream => _attentionController.stream;

  /// 这条会话要处理的事已经过去了（任一客户端处理掉的）。
  Stream<String> get attentionResolvedStream =>
      _attentionResolvedController.stream;

  /// 只给人看的错误（会话目录订阅出错、连接调用异常）。
  Stream<String> get errorStream => _errorController.stream;

  ConnectionEvent get connection => _connection;
  RuntimeConnectionState get state => _connection.state;
  LinkMetrics? get link => _connection.link;

  SessionList get sessionList => _sessionList;
  List<SessionSummary> get sessions => _sessionList.sessions;
  WorkspaceCatalog get workspace => _workspace;

  /// 最近一次的自动化目录。null = 还没拉到。
  AutomationCatalog? get automationCatalog => _automationCatalog;

  /// 智能体展示名。配不上就返回 null，让调用方退回通用标签而不是印一个 id。
  String? agentDefinitionName(String? definitionId) {
    if (definitionId == null) return null;
    for (final agent in _automationCatalog?.agents ?? const []) {
      if (agent.id == definitionId) {
        final name = agent.name.trim();
        return name.isEmpty ? null : name;
      }
    }
    return null;
  }

  void _onConnection(ConnectionEvent event) {
    final wasConnected = _connection.state == RuntimeConnectionState.connected;
    _connection = event;
    _connectionController.add(event);
    // 工作区和自动化目录是一问一答，连上时拉一次：会话行要拿自动化目录里的
    // 智能体名，新建选择器要工作区目录。
    if (event.state == RuntimeConnectionState.connected && !wasConnected) {
      unawaited(refreshWorkspace().then((_) {}, onError: (_) {}));
      unawaited(refreshAutomations().then((_) {}, onError: (_) {}));
    }
  }

  void _onCatalog(CatalogEvent event) {
    switch (event.kind) {
      case CatalogEventKind.sessions:
        final list = event.sessions;
        if (list == null) return;
        _sessionList = list;
        _sessionListController.add(list);
      case CatalogEventKind.attention:
        final item = event.attention;
        if (item != null) _attentionController.add(item);
      case CatalogEventKind.attentionResolved:
        final sessionId = event.sessionId;
        if (sessionId != null) _attentionResolvedController.add(sessionId);
      case CatalogEventKind.error:
        final message = event.message;
        if (message != null) _errorController.add(message);
    }
  }

  void _reportError(Object error) =>
      _errorController.add(describeRuntimeError(error));

  // ---------------------------------------------------------------------------
  // 配对与连接
  // ---------------------------------------------------------------------------

  /// 只解析、校验配对串，不联网。格式不对抛 [SmeltError]（invalidPairing）。
  PairingTarget parsePairing(String uri) => _backend.parsePairing(uri.trim());

  /// 用配对串配对。成功后它就是当前运行时（已连接）；失败抛 [SmeltError]。
  Future<RuntimeInfo> pair(String uri, String deviceName) async {
    final info = await _backend.pair(uri.trim(), deviceName);
    _target = (endpointId: info.endpointId, relayUrl: info.relayUrl);
    return info;
  }

  /// 切换到某个运行时。结果从 [connectionStream] 推出：离线时进入重连，未配对 /
  /// 被撤销时停在断开并带上错误，所以这里不抛 [SmeltError]。
  Future<void> connect(String endpointId, String? relayUrl) async {
    _target = (endpointId: endpointId, relayUrl: relayUrl);
    try {
      await _backend.connect(endpointId, relayUrl);
    } on SmeltError {
      // 连接状态流里已经带着这次的结果。
    } catch (error) {
      _reportError(error);
    }
  }

  /// 断开当前运行时。[forget] 为真时连「重试」的目标也忘掉（运行时被删掉了）。
  Future<void> disconnect({bool forget = false}) async {
    if (forget) _target = null;
    try {
      await _backend.disconnect();
    } catch (error) {
      _reportError(error);
    }
  }

  /// 断开时重连上一个运行时；重连中则立刻再试一次。
  Future<void> retry() async {
    final target = _target;
    if (target == null) return;
    switch (state) {
      case RuntimeConnectionState.disconnected:
        await connect(target.endpointId, target.relayUrl);
      case RuntimeConnectionState.reconnecting:
        verifyConnection();
      case RuntimeConnectionState.connecting:
      case RuntimeConnectionState.connected:
        break;
    }
  }

  /// App 回到前台时调：立刻探活，不等下一个心跳。
  void verifyConnection() {
    unawaited(_backend.verifyConnection().catchError(_reportError));
  }

  // ---------------------------------------------------------------------------
  // 会话目录
  // ---------------------------------------------------------------------------

  void markRead(String sessionId) => _backend.markRead(sessionId);

  Future<WorkspaceCatalog> refreshWorkspace() async {
    final workspace = await _backend.workspaceCatalog();
    _workspace = workspace;
    _workspaceController.add(workspace);
    return workspace;
  }

  Future<AutomationCatalog> refreshAutomations() =>
      _publishAutomations(_backend.automationCatalog());

  /// 幂等设值而不是 toggle：电脑上可能刚改过同一条，toggle 会翻反。
  Future<AutomationCatalog> setAutomationEnabled(
    String automationId,
    bool enabled,
  ) =>
      _publishAutomations(_backend.setAutomationEnabled(automationId, enabled));

  Future<AutomationCatalog> runAutomationOnce(String automationId) =>
      _publishAutomations(_backend.runAutomationOnce(automationId));

  Future<AutomationCatalog> _publishAutomations(
    Future<AutomationCatalog> request,
  ) async {
    final catalog = await request;
    _automationCatalog = catalog;
    _automationCatalogController.add(catalog);
    return catalog;
  }

  /// 新建会话，返回会话 id。参数语义见 Rust `create_session`。
  Future<String> createSession({
    required String projectRoot,
    String? agentOptionId,
    String? launchKey,
    String? resumeId,
  }) => _backend.createSession(
    projectRoot: projectRoot,
    agentOptionId: agentOptionId,
    launchKey: launchKey,
    resumeId: resumeId,
  );

  Future<void> deleteSession(String sessionId) =>
      _backend.deleteSession(sessionId);

  Future<List<HistorySessionSummary>> listHistory(
    String projectRoot,
    String agentOptionId,
  ) => _backend.listHistory(projectRoot, agentOptionId);

  /// [title] 为空表示恢复 agent 原始标题。
  Future<HistoryRenameResult> renameHistory(
    String projectRoot,
    String agentOptionId,
    String resumeId, {
    String? title,
  }) => _backend.renameHistory(projectRoot, agentOptionId, resumeId, title);

  // ---------------------------------------------------------------------------
  // 对话
  // ---------------------------------------------------------------------------

  /// 当前真正在订阅的会话（栈顶）。
  String? get watchedSessionId => _watches.lastOrNull?.sessionId;

  /// 订阅一个会话的视图。顶掉当前订阅；返回的 [ConversationWatch] 关掉时，下面
  /// 那一个会被重新订阅回来。
  ConversationWatch watchConversation(String sessionId) {
    final watch = ConversationWatch._(this, sessionId);
    final previous = _watches.lastOrNull;
    _watches.add(watch);
    _attach(watch);
    previous?._detach();
    return watch;
  }

  void _attach(ConversationWatch watch) {
    final previous = watch._source;
    watch._source = _backend
        .watchConversation(watch.sessionId)
        .listen(
          watch._controller.add,
          onError: (Object error) => watch._controller.add(
            ConversationEvent(
              kind: ConversationEventKind.error,
              cached: false,
              message: describeRuntimeError(error),
            ),
          ),
        );
    unawaited(previous?.cancel());
  }

  void _restart(ConversationWatch watch) {
    if (identical(_watches.lastOrNull, watch)) _attach(watch);
  }

  void _close(ConversationWatch watch) {
    final wasTop = identical(_watches.lastOrNull, watch);
    _watches.remove(watch);
    watch._detach();
    unawaited(watch._controller.close());
    if (!wasTop) return;
    final next = _watches.lastOrNull;
    if (next != null) {
      _attach(next);
    } else {
      unawaited(_backend.unwatchConversation().catchError(_reportError));
    }
  }

  /// 往前翻一页。false = 没翻（不是当前订阅、前面没有了、已在翻）。
  Future<bool> loadOlder(String sessionId) => _backend.loadOlder(sessionId);

  /// 不订阅、只取一个会话当前在等的审批和询问（指挥台用）。
  Future<PendingActions> pendingActions(String sessionId) =>
      _backend.pendingActions(sessionId);

  /// 新的发送请求 id。没等到确认的草稿重发时要用同一个。
  String newRequestId() => _backend.newRequestId();

  /// 失败抛 [SendMessageError]：rejected 可以放心重发，unknown 可能已送达。
  Future<void> sendMessage(
    String sessionId,
    String requestId,
    String text,
    List<ImageInput> images,
  ) => _backend.sendMessage(sessionId, requestId, text, images);

  Future<void> cancelTurn(String sessionId) => _backend.cancelTurn(sessionId);

  Future<void> setConfigOption(
    String sessionId,
    String configId,
    String valueId, {
    bool? boolean,
  }) =>
      _backend.setConfigOption(sessionId, configId, valueId, boolean: boolean);

  Future<void> respondApproval(
    String sessionId,
    String toolCallId,
    String optionId,
  ) => _backend.respondApproval(sessionId, toolCallId, optionId);

  Future<void> chooseElicitation(
    String sessionId,
    int fieldIndex,
    int optionIndex,
  ) => _backend.chooseElicitation(sessionId, fieldIndex, optionIndex);

  Future<void> submitElicitation(
    String sessionId,
    List<ElicitationTextInput> textValues,
  ) => _backend.submitElicitation(sessionId, textValues);

  Future<void> dismissElicitation(String sessionId) =>
      _backend.dismissElicitation(sessionId);

  Future<SessionSkills> listSkills(String sessionId) =>
      _backend.listSkills(sessionId);

  /// 取一张大图：给 [maxEdge] 出缩略图，不给出原图。
  Future<ImageData> loadImage(
    String sessionId,
    ImageRef image, {
    int? maxEdge,
  }) => _backend.loadImage(sessionId, image, maxEdge: maxEdge);

  Future<void> dispose() async {
    await _connectionSubscription?.cancel();
    await _catalogSubscription?.cancel();
    for (final watch in List.of(_watches)) {
      watch._detach();
      await watch._controller.close();
    }
    _watches.clear();
    await _connectionController.close();
    await _sessionListController.close();
    await _workspaceController.close();
    await _automationCatalogController.close();
    await _attentionController.close();
    await _attentionResolvedController.close();
    await _errorController.close();
  }
}

/// 一个对话页对某个会话的订阅。
class ConversationWatch {
  ConversationWatch._(this._service, this.sessionId);

  final RuntimeService _service;
  final String sessionId;
  final _controller = StreamController<ConversationEvent>.broadcast();
  StreamSubscription<ConversationEvent>? _source;

  /// 这个会话的视图事件。被别的会话顶下去期间没有事件。
  Stream<ConversationEvent> get events => _controller.stream;

  /// 重新订阅（「重试」、运行时重新连上之后）。被顶下去时什么也不做。
  void restart() => _service._restart(this);

  /// 离开对话页时调。
  void close() => _service._close(this);

  void _detach() {
    unawaited(_source?.cancel());
    _source = null;
  }
}

/// 全局实例。`main()` 在 `RustLib.init()` 之后调 [RuntimeService.start]；单测换成
/// 注入假 backend 的实例，不加载 Rust 动态库。
RuntimeService runtimeService = RuntimeService(const FrbRuntimeBackend());
