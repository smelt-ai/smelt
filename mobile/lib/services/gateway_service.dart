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

  const AcpAgentOption({
    required this.id,
    required this.kind,
    required this.label,
    required this.profile,
  });

  factory AcpAgentOption.fromJson(Map<String, dynamic> json) => AcpAgentOption(
    id: json['id'] as String? ?? '',
    kind: json['kind'] as String? ?? '',
    label: json['label'] as String? ?? '',
    profile: json['profile'] as bool? ?? false,
  );
}

class WorkspaceCatalog {
  final List<WorkspaceProject> projects;
  final List<AcpAgentOption> agents;

  const WorkspaceCatalog({required this.projects, required this.agents});
}

class HistorySessionSummary {
  final String resumeId;
  final String title;
  final DateTime? startedAt;
  final DateTime? lastActiveAt;
  final int messageCount;

  const HistorySessionSummary({
    required this.resumeId,
    required this.title,
    required this.startedAt,
    required this.lastActiveAt,
    required this.messageCount,
  });

  factory HistorySessionSummary.fromJson(Map<String, dynamic> json) =>
      HistorySessionSummary(
        resumeId: json['resumeId'] as String? ?? '',
        title: json['title'] as String? ?? '',
        startedAt: DateTime.tryParse(json['startedAt'] as String? ?? ''),
        lastActiveAt: DateTime.tryParse(json['lastActiveAt'] as String? ?? ''),
        messageCount: json['messageCount'] as int? ?? 0,
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
  Future<void> _messageQueue = Future.value();

  static const int _maxCachedSessions = 5;
  static const int _maxCacheBytes = 32 * 1024 * 1024;
  static const int _initialTailLimit = 100;
  final LinkedHashMap<String, AcpSnapshot> _snapshotCache = LinkedHashMap();
  final Set<String> _historyLoads = {};
  final Set<String> _cachedSnapshotIds = {};
  final Map<String, Timer> _snapshotCacheTimers = {};
  String? _cacheNamespace;
  int _cacheLoadGeneration = 0;
  List<SessionSummary> _lastSessions = const [];
  DateTime? _cachedAt;
  bool _sessionsAreCached = false;

  final _stateController = StreamController<WsState>.broadcast();
  final _sessionsController =
      StreamController<List<SessionSummary>>.broadcast();
  final _workspaceController = StreamController<WorkspaceCatalog>.broadcast();
  final _sessionHistoryController =
      StreamController<SessionHistoryResult>.broadcast();
  final _sessionCreatedController = StreamController<String>.broadcast();
  final _sessionDeletedController = StreamController<String>.broadcast();
  final _snapshotController = StreamController<AcpSnapshot>.broadcast();
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

  Stream<SessionHistoryResult> get sessionHistoryStream =>
      _sessionHistoryController.stream;

  Stream<String> get sessionCreatedStream => _sessionCreatedController.stream;

  Stream<String> get sessionDeletedStream => _sessionDeletedController.stream;

  /// 当前订阅会话的快照流
  Stream<AcpSnapshot> get snapshotStream => _snapshotController.stream;

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

  void listSessionHistory(String projectRoot, String agentOptionId) {
    _send({
      'method': 'listSessionHistory',
      'params': {'projectRoot': projectRoot, 'agentOptionId': agentOptionId},
    });
  }

  void createSession(
    String projectRoot,
    String agentOptionId, {
    String? resumeId,
  }) {
    _send({
      'method': 'createSession',
      'params': {
        'projectRoot': projectRoot,
        'agentOptionId': agentOptionId,
        'resumeId': ?resumeId,
      },
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
    _sampleMetrics();
    _metricsTimer = Timer.periodic(metricsInterval, (_) => _sampleMetrics());
  }

  void _sampleMetrics() {
    if (_state != WsState.connected) return;
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
          _workspaceController.add(
            WorkspaceCatalog(projects: projects, agents: agents),
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
    _sessionHistoryController.close();
    _sessionCreatedController.close();
    _sessionDeletedController.close();
    _snapshotController.close();
    _attentionController.close();
    _attentionResolvedController.close();
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
