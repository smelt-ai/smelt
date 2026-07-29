// WebSocket client for the gateway /acp/ws endpoint.

import 'dart:async';
import 'dart:convert';
import 'package:web_socket_channel/web_socket_channel.dart';
import '../models/acp_snapshot.dart';

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
  final String id;
  final String title;
  final String phase;
  final String status;
  final String agent;
  final String? cwd;
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

/// WebSocket 连接状态
enum WsState { disconnected, connecting, connected, reconnecting }

/// Gateway WebSocket 服务
class GatewayService {
  WebSocketChannel? _channel;
  StreamSubscription<dynamic>? _channelSubscription;
  Timer? _reconnectTimer;
  WsState _state = WsState.disconnected;
  String? _endpoint;
  String? _token;
  bool _manuallyDisconnected = true;
  int _connectionGeneration = 0;
  bool _writeEnabled = false;

  final _stateController = StreamController<WsState>.broadcast();
  final _sessionsController =
      StreamController<List<SessionSummary>>.broadcast();
  final _snapshotController = StreamController<AcpSnapshot>.broadcast();
  final _attentionController = StreamController<LifecycleAttention>.broadcast();
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
    if (_state == WsState.connected || _state == WsState.connecting) {
      return;
    }

    _endpoint = endpoint.trim();
    _token = token;
    _manuallyDisconnected = false;
    _reconnectTimer?.cancel();
    _setState(WsState.connecting);

    final generation = ++_connectionGeneration;
    try {
      final channel = WebSocketChannel.connect(_gatewayUri(_endpoint!, token));
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
      await channel.ready;
    } catch (e) {
      if (generation != _connectionGeneration) return;
      _errorController.add('连接失败: $e');
      _scheduleReconnect();
    }
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
    _connectionGeneration++;
    _reconnectTimer?.cancel();
    _reconnectTimer = null;
    _subscribedSessionId = null;
    _writeEnabled = false;
    _channelSubscription?.cancel();
    _channelSubscription = null;
    _channel?.sink.close();
    _channel = null;
    _setState(WsState.disconnected);
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
  void sendMessage(String sessionId, String content) {
    _send({
      'method': 'sendMessage',
      'params': {'sessionId': sessionId, 'content': content},
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
    _state = newState;
    _stateController.add(newState);
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
    _scheduleReconnect();
  }

  void _onDone() {
    _scheduleReconnect();
  }

  void _scheduleReconnect() {
    if (!_manuallyDisconnected && _endpoint != null && _token != null) {
      _channelSubscription?.cancel();
      _channelSubscription = null;
      _channel = null;
      _setState(WsState.reconnecting);
      _reconnectTimer?.cancel();
      _reconnectTimer = Timer(const Duration(seconds: 2), () {
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
    _errorController.close();
  }
}

/// 全局单例
final gatewayService = GatewayService();
