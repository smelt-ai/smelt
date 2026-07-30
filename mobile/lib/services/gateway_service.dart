// WebSocket client for the gateway /acp/ws endpoint.

import 'dart:async';
import 'dart:convert';
import 'package:web_socket_channel/web_socket_channel.dart';
import '../models/acp_snapshot.dart';
import '../models/pairing_config.dart';

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
}

/// 会话摘要（列表用）
class SessionSummary {
  static const unknownOrder = 0xffffffff;

  final String id;
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

/// 启动 iroh 隧道并返回手机本地入口端口。
///
/// 做成可注入的函数而不是直接调 FFI，是为了让 `GatewayService` 的测试
/// 不必依赖编译好的 Rust 动态库。
typedef IrohTunnelOpener =
    Future<int> Function(String endpointId, String relayUrl, String relayToken);
typedef IrohTunnelStopper = Future<void> Function();

/// Gateway WebSocket 服务
class GatewayService {
  GatewayService({
    this.connectTimeout = const Duration(seconds: 10),
    this.reconnectDelay = const Duration(seconds: 2),
    IrohTunnelOpener? irohTunnelOpener,
    IrohTunnelStopper? irohTunnelStopper,
  }) : irohTunnelOpener = irohTunnelOpener ?? _irohUnavailable,
       irohTunnelStopper = irohTunnelStopper ?? _noopIrohStop;

  /// 从发起连接到收到服务端 `connected` 的整体上限。
  final Duration connectTimeout;
  final Duration reconnectDelay;

  /// 启动 iroh 隧道的方式。默认会明确报错 —— 真正的实现由组装根（`main()`）
  /// 在 RustLib 初始化之后注入，这样本文件保持纯 Dart，单测不必依赖动态库。
  IrohTunnelOpener irohTunnelOpener;
  IrohTunnelStopper irohTunnelStopper;

  static Future<int> _irohUnavailable(String _, String _, String _) =>
      Future.error(StateError('本版本未编入 iroh 隧道支持'));
  static Future<void> _noopIrohStop() async {}

  WebSocketChannel? _channel;
  StreamSubscription<dynamic>? _channelSubscription;
  Timer? _reconnectTimer;
  Timer? _connectWatchdog;
  WsState _state = WsState.disconnected;
  String? _endpoint;
  String? _token;
  bool _manuallyDisconnected = true;

  /// 当前目标是否曾经握手成功过。没成功过的地址（打错、主机不存在）失败后直接
  /// 回到断开态让用户改，而不是无限自动重连。
  bool _everConnected = false;
  int _connectionGeneration = 0;
  bool _writeEnabled = false;

  final _stateController = StreamController<WsState>.broadcast();
  final _sessionsController =
      StreamController<List<SessionSummary>>.broadcast();
  final _snapshotController = StreamController<AcpSnapshot>.broadcast();
  final _attentionController = StreamController<LifecycleAttention>.broadcast();
  final _attentionResolvedController = StreamController<String>.broadcast();
  final _errorController = StreamController<String>.broadcast();

  String? _subscribedSessionId;

  /// 连接状态流
  Stream<WsState> get stateStream => _stateController.stream;

  /// 会话列表流
  Stream<List<SessionSummary>> get sessionsStream => _sessionsController.stream;

  /// 当前订阅会话的快照流
  Stream<AcpSnapshot> get snapshotStream => _snapshotController.stream;

  /// smeltd 统一生命周期产生的关注事件。
  Stream<LifecycleAttention> get attentionStream => _attentionController.stream;

  /// 任一客户端处理完行动项后，由同一 AttentionStore 发出的解决事件。
  Stream<String> get attentionResolvedStream =>
      _attentionResolvedController.stream;

  /// 错误流
  Stream<String> get errorStream => _errorController.stream;

  /// 当前状态
  WsState get state => _state;

  /// Whether the desktop gateway allows prompts and approval responses.
  bool get writeEnabled => _writeEnabled;

  /// 当前订阅的会话 ID
  String? get subscribedSessionId => _subscribedSessionId;

  /// 连接到 gateway
  Future<void> connect(String endpoint, String token) async {
    final target = endpoint.trim();
    final restartIroh =
        _state == WsState.reconnecting &&
        Uri.tryParse(target)?.scheme == PairingConfig.irohScheme;
    // 同一目标已连/在连 → 幂等返回；换了目标 → 拆掉旧连接改连新的，否则扫码切换
    // 桌面会被静默忽略（UI 显示新地址、实际连着旧的）。
    if (matchesTarget(target, token)) {
      if (_state == WsState.connected || _state == WsState.connecting) return;
    } else {
      _teardownSocket();
      _everConnected = false;
    }

    _endpoint = target;
    _token = token;
    _manuallyDisconnected = false;
    _reconnectTimer?.cancel();
    _setState(WsState.connecting);
    // 不可达但可路由的地址（打错 IP）不会立刻报错，`ready` 会一直挂着；握手成功
    // 但服务端不发 `connected` 同样会卡住。用一个看门狗兜住整段握手。
    _connectWatchdog?.cancel();
    _connectWatchdog = Timer(connectTimeout, () {
      if (_state != WsState.connecting) return;
      _errorController.add('连接超时：$target 没有响应');
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
      final channel = WebSocketChannel.connect(wsUri);
      _channel = channel;
      _channelSubscription = channel.stream.listen(
        (data) {
          if (generation == _connectionGeneration) _onMessage(data);
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
      _errorController.add('连接失败: $e');
      _failConnection();
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
    final relayToken = parsed.queryParameters['relay_token'] ?? '';
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
      relayToken,
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

  /// 断开连接
  void disconnect() {
    _manuallyDisconnected = true;
    _teardownSocket();
    _setState(WsState.disconnected);
  }

  /// 当前连接（或正在重连）的目标是否就是这组 endpoint/token。
  bool matchesTarget(String endpoint, String token) =>
      _endpoint == endpoint.trim() && _token == token;

  /// 关闭底层通道并作废旧连接的回调，不改变对外状态。
  void _teardownSocket() {
    _connectionGeneration++;
    _reconnectTimer?.cancel();
    _reconnectTimer = null;
    _connectWatchdog?.cancel();
    _connectWatchdog = null;
    _subscribedSessionId = null;
    _writeEnabled = false;
    _channelSubscription?.cancel();
    _channelSubscription = null;
    _channel?.sink.close();
    _channel = null;
  }

  /// 请求会话列表
  void listSessions() {
    _send({'method': 'listSessions'});
  }

  /// 订阅会话
  void subscribe(String sessionId) {
    _subscribedSessionId = sessionId;
    _send({
      'method': 'subscribe',
      'params': {'sessionId': sessionId},
    });
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
  void sendMessage(
    String sessionId,
    String content, {
    List<AcpImageData> images = const [],
  }) {
    _send({
      'method': 'sendMessage',
      'params': {
        'sessionId': sessionId,
        'content': content,
        'images': images.map((image) => image.toJson()).toList(),
      },
    });
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
    }
    _state = newState;
    _stateController.add(newState);
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

  void _onMessage(dynamic data) {
    try {
      final json = jsonDecode(data as String) as Map<String, dynamic>;
      final type = json['type'] as String?;

      switch (type) {
        case 'connected':
          _writeEnabled = json['writeEnabled'] as bool? ?? false;
          _setState(WsState.connected);
          listSessions();
          final sessionId = _subscribedSessionId;
          if (sessionId != null) subscribe(sessionId);

        case 'sessions':
          final sessions =
              (json['sessions'] as List<dynamic>?)
                  ?.map(
                    (s) => SessionSummary.fromJson(s as Map<String, dynamic>),
                  )
                  .toList() ??
              [];
          _sessionsController.add(sessions);

        case 'subscribed':
          // 订阅确认
          break;

        case 'unsubscribed':
          _subscribedSessionId = null;

        case 'snapshot':
          // 旧格式兼容
          final snapshot = AcpSnapshot.fromJson(json);
          _snapshotController.add(snapshot);

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
          _errorController.add(json['error'] as String? ?? 'Gateway 请求失败');

        default:
          // 可能是原始 smeltd 格式: {"snapshot": {...}}
          if (json.containsKey('snapshot')) {
            final snapshot = AcpSnapshot.fromJson(json);
            _snapshotController.add(snapshot);
          }
      }
    } catch (e) {
      _errorController.add('解析消息失败: $e');
    }
  }

  void _onError(dynamic error) {
    _errorController.add('WebSocket 错误: $error');
    _failConnection();
  }

  void _onDone() {
    _failConnection();
  }

  void _scheduleReconnect() {
    if (!_manuallyDisconnected && _endpoint != null && _token != null) {
      _channelSubscription?.cancel();
      _channelSubscription = null;
      _channel = null;
      _setState(WsState.reconnecting);
      _reconnectTimer?.cancel();
      _reconnectTimer = Timer(reconnectDelay, () {
        if (_state == WsState.reconnecting) {
          connect(_endpoint!, _token!);
        }
      });
    } else {
      _setState(WsState.disconnected);
    }
  }

  void dispose() {
    disconnect();
    _stateController.close();
    _sessionsController.close();
    _snapshotController.close();
    _attentionController.close();
    _attentionResolvedController.close();
    _errorController.close();
  }
}

/// 全局单例
final gatewayService = GatewayService();
