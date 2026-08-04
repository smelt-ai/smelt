import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:web_socket_channel/web_socket_channel.dart';

import 'gateway_service.dart';

typedef TerminalChannelFactory = WebSocketChannel Function(Uri uri);

enum TerminalStreamState { waitingForGateway, connecting, connected, ended }

class TerminalGeometry {
  const TerminalGeometry({
    required this.cols,
    required this.rows,
    required this.cellWidth,
    required this.cellHeight,
  });

  final int cols;
  final int rows;
  final int cellWidth;
  final int cellHeight;

  Map<String, int> toJson() => {
    'cols': cols,
    'rows': rows,
    'cellWidth': cellWidth,
    'cellHeight': cellHeight,
  };

  @override
  bool operator ==(Object other) =>
      other is TerminalGeometry &&
      cols == other.cols &&
      rows == other.rows &&
      cellWidth == other.cellWidth &&
      cellHeight == other.cellHeight;

  @override
  int get hashCode => Object.hash(cols, rows, cellWidth, cellHeight);
}

sealed class TerminalStreamEvent {
  const TerminalStreamEvent();
}

class TerminalReadyEvent extends TerminalStreamEvent {
  const TerminalReadyEvent({
    required this.cols,
    required this.rows,
    required this.replayBytes,
    required this.writeEnabled,
  });

  final int cols;
  final int rows;
  final int replayBytes;
  final bool writeEnabled;
}

class TerminalDataEvent extends TerminalStreamEvent {
  const TerminalDataEvent(this.bytes);

  final Uint8List bytes;
}

class TerminalReplayCompleteEvent extends TerminalStreamEvent {
  const TerminalReplayCompleteEvent();
}

class TerminalErrorEvent extends TerminalStreamEvent {
  const TerminalErrorEvent(this.message, {this.fatal = false});

  final String message;
  final bool fatal;
}

class TerminalClosedEvent extends TerminalStreamEvent {
  const TerminalClosedEvent();
}

abstract interface class TerminalStreamClient {
  Stream<TerminalStreamEvent> get events;
  Stream<TerminalStreamState> get stateStream;
  TerminalStreamState get state;
  bool get writeEnabled;

  void start(TerminalGeometry geometry);
  void updateGeometry(TerminalGeometry geometry);
  void sendInput(String data);
  void suspend();
  void resume();
  Future<void> dispose();
}

class TerminalStreamService implements TerminalStreamClient {
  TerminalStreamService({
    required this.gateway,
    required this.sessionId,
    TerminalChannelFactory? channelFactory,
    this.connectTimeout = const Duration(seconds: 15),
    this.resizeDebounce = const Duration(milliseconds: 120),
  }) : _channelFactory = channelFactory ?? WebSocketChannel.connect {
    _gatewaySubscription = gateway.stateStream.listen(_handleGatewayState);
  }

  final GatewayService gateway;
  final String sessionId;
  final TerminalChannelFactory _channelFactory;
  final Duration connectTimeout;
  final Duration resizeDebounce;

  final _eventsController = StreamController<TerminalStreamEvent>.broadcast();
  final _stateController = StreamController<TerminalStreamState>.broadcast();

  late final StreamSubscription<WsState> _gatewaySubscription;
  StreamSubscription<dynamic>? _channelSubscription;
  WebSocketChannel? _channel;
  Timer? _reconnectTimer;
  Timer? _resizeTimer;
  TerminalGeometry? _geometry;
  TerminalGeometry? _lastSentGeometry;
  int _replayBytesRemaining = 0;
  bool _replayComplete = false;
  TerminalStreamState _state = TerminalStreamState.waitingForGateway;
  int _generation = 0;
  int _reconnectDelayMs = 500;
  bool _started = false;
  bool _suspended = false;
  bool _disposed = false;
  bool _ended = false;
  bool _writeEnabled = false;

  @override
  Stream<TerminalStreamEvent> get events => _eventsController.stream;
  @override
  Stream<TerminalStreamState> get stateStream => _stateController.stream;
  @override
  TerminalStreamState get state => _state;
  @override
  bool get writeEnabled => _writeEnabled;

  @override
  void start(TerminalGeometry geometry) {
    if (_disposed || _ended) return;
    _started = true;
    _geometry = geometry;
    if (gateway.state == WsState.connected) unawaited(_open());
  }

  @override
  void updateGeometry(TerminalGeometry geometry) {
    if (_disposed || _ended) return;
    _geometry = geometry;
    if (!_started) {
      start(geometry);
      return;
    }
    if (_state != TerminalStreamState.connected ||
        geometry == _lastSentGeometry) {
      return;
    }
    _resizeTimer?.cancel();
    _resizeTimer = Timer(resizeDebounce, () {
      if (_disposed ||
          _suspended ||
          _state != TerminalStreamState.connected ||
          _geometry == _lastSentGeometry) {
        return;
      }
      final latest = _geometry;
      if (latest == null) return;
      _send({'method': 'resize', 'params': latest.toJson()});
      _lastSentGeometry = latest;
    });
  }

  @override
  void sendInput(String data) {
    if (data.isEmpty ||
        !_writeEnabled ||
        _state != TerminalStreamState.connected) {
      return;
    }
    _send({
      'method': 'input',
      'params': {'data': data},
    });
  }

  @override
  void suspend() {
    if (_disposed || _suspended) return;
    _suspended = true;
    _reconnectTimer?.cancel();
    _closeChannel();
    _setState(TerminalStreamState.waitingForGateway);
  }

  @override
  void resume() {
    if (_disposed || !_suspended || _ended) return;
    _suspended = false;
    if (_started && gateway.state == WsState.connected) unawaited(_open());
  }

  Future<void> _open() async {
    if (_disposed ||
        _suspended ||
        _ended ||
        !_started ||
        _channel != null ||
        gateway.state != WsState.connected) {
      return;
    }
    final geometry = _geometry;
    final uri = gateway.terminalWebSocketUri(sessionId);
    if (geometry == null || uri == null) return;

    _reconnectTimer?.cancel();
    _setState(TerminalStreamState.connecting);
    final generation = ++_generation;
    final channel = _channelFactory(uri);
    _channel = channel;
    _channelSubscription = channel.stream.listen(
      (data) => _handleMessage(data, generation),
      onError: (_) => _handleChannelDone(generation),
      onDone: () => _handleChannelDone(generation),
    );
    try {
      await channel.ready.timeout(connectTimeout);
      if (_disposed || generation != _generation || _channel != channel) return;
      _lastSentGeometry = geometry;
      _send({'method': 'attach', 'params': geometry.toJson()});
    } catch (error) {
      if (generation != _generation) return;
      _eventsController.add(
        TerminalErrorEvent('Terminal connection failed: $error'),
      );
      _handleChannelDone(generation);
    }
  }

  void _handleMessage(dynamic data, int generation) {
    if (_disposed || generation != _generation) return;
    if (data is List<int>) {
      final bytes = Uint8List.fromList(data);
      _eventsController.add(TerminalDataEvent(bytes));
      if (_replayBytesRemaining > 0) {
        _replayBytesRemaining = (_replayBytesRemaining - bytes.length).clamp(
          0,
          1 << 30,
        );
        if (_replayBytesRemaining == 0) _completeReplay();
      }
      return;
    }
    if (data is! String) return;
    try {
      final message = jsonDecode(data) as Map<String, dynamic>;
      switch (message['type']) {
        case 'terminalConnected':
          _writeEnabled = message['writeEnabled'] as bool? ?? false;
        case 'terminalReady':
          _writeEnabled = message['writeEnabled'] as bool? ?? false;
          _replayBytesRemaining = message['replayBytes'] as int? ?? 0;
          _replayComplete = false;
          _reconnectDelayMs = 500;
          _setState(TerminalStreamState.connected);
          _eventsController.add(
            TerminalReadyEvent(
              cols: message['cols'] as int? ?? 80,
              rows: message['rows'] as int? ?? 24,
              replayBytes: message['replayBytes'] as int? ?? 0,
              writeEnabled: _writeEnabled,
            ),
          );
          final latest = _geometry;
          if (latest != null && latest != _lastSentGeometry) {
            updateGeometry(latest);
          }
          if (_replayBytesRemaining == 0) _completeReplay();
        case 'terminalError':
          final fatal = message['fatal'] as bool? ?? false;
          _eventsController.add(
            TerminalErrorEvent(
              message['error'] as String? ?? 'Terminal request failed',
              fatal: fatal,
            ),
          );
          if (fatal) {
            _ended = true;
            _setState(TerminalStreamState.ended);
            _closeChannel();
          }
        case 'terminalClosed':
          _ended = true;
          _setState(TerminalStreamState.ended);
          _eventsController.add(const TerminalClosedEvent());
          _closeChannel();
      }
    } catch (error) {
      _eventsController.add(
        TerminalErrorEvent('Invalid terminal message: $error'),
      );
    }
  }

  void _completeReplay() {
    if (_replayComplete) return;
    _replayComplete = true;
    _eventsController.add(const TerminalReplayCompleteEvent());
  }

  void _handleChannelDone(int generation) {
    if (_disposed || generation != _generation) return;
    _generation++;
    _channelSubscription?.cancel();
    _channel?.sink.close();
    _channel = null;
    _channelSubscription = null;
    _writeEnabled = false;
    if (_ended) {
      _setState(TerminalStreamState.ended);
      return;
    }
    _setState(TerminalStreamState.waitingForGateway);
    _scheduleReconnect();
  }

  void _handleGatewayState(WsState state) {
    if (_disposed || _ended) return;
    if (state == WsState.connected) {
      if (_started && !_suspended) unawaited(_open());
      return;
    }
    _reconnectTimer?.cancel();
    _closeChannel();
    _setState(TerminalStreamState.waitingForGateway);
  }

  void _scheduleReconnect() {
    if (_disposed ||
        _suspended ||
        _ended ||
        gateway.state != WsState.connected) {
      return;
    }
    _reconnectTimer?.cancel();
    final delay = Duration(milliseconds: _reconnectDelayMs);
    _reconnectDelayMs = (_reconnectDelayMs * 2).clamp(500, 8000);
    _reconnectTimer = Timer(delay, () => unawaited(_open()));
  }

  void _send(Map<String, dynamic> message) {
    try {
      _channel?.sink.add(jsonEncode(message));
    } catch (_) {
      _handleChannelDone(_generation);
    }
  }

  void _closeChannel() {
    _generation++;
    _replayBytesRemaining = 0;
    _replayComplete = false;
    _resizeTimer?.cancel();
    _channelSubscription?.cancel();
    _channelSubscription = null;
    _channel?.sink.close();
    _channel = null;
    _lastSentGeometry = null;
    _writeEnabled = false;
  }

  void _setState(TerminalStreamState next) {
    if (_state == next) return;
    _state = next;
    _stateController.add(next);
  }

  @override
  Future<void> dispose() async {
    if (_disposed) return;
    _disposed = true;
    _reconnectTimer?.cancel();
    _resizeTimer?.cancel();
    _closeChannel();
    await _gatewaySubscription.cancel();
    await _eventsController.close();
    await _stateController.close();
  }
}
