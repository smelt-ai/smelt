/// WebSocket 服务
/// 
/// 连接 gateway 的 /acp/ws 端点，处理双向通信。

import 'dart:async';
import 'dart:convert';
import 'package:web_socket_channel/web_socket_channel.dart';
import '../models/acp_snapshot.dart';

/// 会话摘要（列表用）
class SessionSummary {
  final String id;
  final String title;
  final String phase;
  final String agent;
  final String? cwd;
  final int updatedAt;

  const SessionSummary({
    required this.id,
    required this.title,
    required this.phase,
    required this.agent,
    this.cwd,
    this.updatedAt = 0,
  });

  factory SessionSummary.fromJson(Map<String, dynamic> json) {
    return SessionSummary(
      id: json['id'] as String? ?? '',
      title: json['title'] as String? ?? '',
      phase: json['phase'] as String? ?? 'idle',
      agent: json['agent'] as String? ?? 'other',
      cwd: json['cwd'] as String?,
      updatedAt: json['updated_at'] as int? ?? 0,
    );
  }
}

/// WebSocket 连接状态
enum WsState {
  disconnected,
  connecting,
  connected,
  reconnecting,
}

/// Gateway WebSocket 服务
class GatewayService {
  WebSocketChannel? _channel;
  WsState _state = WsState.disconnected;
  String? _endpoint;
  String? _token;
  
  final _stateController = StreamController<WsState>.broadcast();
  final _sessionsController = StreamController<List<SessionSummary>>.broadcast();
  final _snapshotController = StreamController<AcpSnapshot>.broadcast();
  final _errorController = StreamController<String>.broadcast();
  
  String? _subscribedSessionId;

  /// 连接状态流
  Stream<WsState> get stateStream => _stateController.stream;
  
  /// 会话列表流
  Stream<List<SessionSummary>> get sessionsStream => _sessionsController.stream;
  
  /// 当前订阅会话的快照流
  Stream<AcpSnapshot> get snapshotStream => _snapshotController.stream;
  
  /// 错误流
  Stream<String> get errorStream => _errorController.stream;
  
  /// 当前状态
  WsState get state => _state;
  
  /// 当前订阅的会话 ID
  String? get subscribedSessionId => _subscribedSessionId;

  /// 连接到 gateway
  Future<void> connect(String endpoint, String token) async {
    if (_state == WsState.connected || _state == WsState.connecting) {
      return;
    }
    
    _endpoint = endpoint;
    _token = token;
    _setState(WsState.connecting);
    
    try {
      final uri = Uri.parse('$endpoint/acp/ws?token=$token');
      _channel = WebSocketChannel.connect(uri);
      
      _channel!.stream.listen(
        _onMessage,
        onError: _onError,
        onDone: _onDone,
      );
      
      // 等待连接确认
      // 服务端会发送 {"type": "connected", ...}
    } catch (e) {
      _setState(WsState.disconnected);
      _errorController.add('连接失败: $e');
    }
  }

  /// 断开连接
  void disconnect() {
    _subscribedSessionId = null;
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
      'params': {
        'sessionId': sessionId,
        'content': content,
      },
    });
  }

  /// 响应权限请求
  void respondApproval(String sessionId, String optionKey, {String? customText}) {
    _send({
      'method': 'respondApproval',
      'params': {
        'sessionId': sessionId,
        'optionKey': optionKey,
        if (customText != null) 'customText': customText,
      },
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
          _setState(WsState.connected);
          // 自动请求会话列表
          listSessions();
          
        case 'sessions':
          final sessions = (json['sessions'] as List<dynamic>?)
              ?.map((s) => SessionSummary.fromJson(s as Map<String, dynamic>))
              .toList() ?? [];
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
    _reconnect();
  }

  void _onDone() {
    if (_state == WsState.connected) {
      _reconnect();
    }
  }

  void _reconnect() {
    if (_endpoint != null && _token != null) {
      _setState(WsState.reconnecting);
      Future.delayed(const Duration(seconds: 2), () {
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
    _errorController.close();
  }
}

/// 全局单例
final gatewayService = GatewayService();
