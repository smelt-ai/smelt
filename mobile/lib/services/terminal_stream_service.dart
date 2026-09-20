import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:web_socket_channel/web_socket_channel.dart';
import 'package:xterm/xterm.dart';

import '../theme/terminal_theme_wire.dart';
import 'gateway_service.dart';

typedef TerminalChannelFactory = WebSocketChannel Function(Uri uri);

/// 首次 attach 要多少行 scrollback。
///
/// 不切备用屏的 TUI（antigravity 这类完全靠终端滚动的）history 能堆到上万行，
/// 序列化后是几十万字节；手机走隧道，一次全推过去就是进页面先干等。先只要最新的
/// 一小段，用户往上滚再按需要更多。
///
/// 按视口算而不是拍个常量：同一个行数在 60 行的小字体屏上是十几屏、在 20 行的大字体
/// 屏上是三十屏，本来就不该是同一个数。六屏的余量够随手往上翻几下；再往上才
/// 触发补历史（整帧重取，很贵，所以不能让它太容易发生）。底线 120 行是给极矮视口
/// （键盘弹起、分屏）兑的。
int initialTerminalScrollbackLines(int rows) {
  final budget = rows > 0 ? rows * 6 : 120;
  return budget.clamp(120, kMaxTerminalScrollbackLines);
}

/// scrollback 的硬上限，同时也是 xterm 缓冲区的行数。
///
/// 两者必须是同一个数：daemon 发多了，超出的部分解码完就被 xterm 丢掉，纯浪费。
const int kMaxTerminalScrollbackLines = 5000;

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

/// 通道已接通、还没 attach。先把 `writeEnabled` 告诉页面，让快捷键栏这类会改变
/// 终端视口高度的 chrome 先上屏；等布局落定再拿最终行列去 attach。
///
/// 不这么做的后果很贵：先按「没有快捷键栏」的高度 attach，回放完成后栏子又把视口
/// 压矮一次，于是每次进页面都把 PTY resize 两次。不切备用屏的 CLI（整段对话都在
/// scrollback 里的那类）每收一次 SIGWINCH 就把整段对话重印一遍——实测一次改尺寸
/// 就是 560KB / 360 个分片 / 8 秒的输出，手机上就是「一进来对话又滚了很久」。
class TerminalConnectedEvent extends TerminalStreamEvent {
  const TerminalConnectedEvent({required this.writeEnabled});

  final bool writeEnabled;
}

class TerminalReadyEvent extends TerminalStreamEvent {
  const TerminalReadyEvent({
    required this.cols,
    required this.rows,
    required this.replayBytes,
    required this.writeEnabled,
    this.scrollbackLines = 0,
    this.historyLines = 0,
    this.theme,
    this.themeIsDark = true,
  });

  final int cols;
  final int rows;
  final int replayBytes;
  final bool writeEnabled;

  /// 这一帧实际要到的行数预算。
  final int scrollbackLines;

  /// 会话侧总共还有多少行可取；大于 [scrollbackLines] 就说明上面还有更老的内容。
  final int historyLines;

  /// 这台设备（PC）当前的终端配色。老网关不下发时为 null，客户端沿用兜底深色。
  final TerminalTheme? theme;

  /// 配色是不是深色主题——手机侧周边 chrome 跟着走。
  final bool themeIsDark;
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

  bool get canLoadMoreScrollback;

  void start(TerminalGeometry geometry);
  void updateGeometry(TerminalGeometry geometry);
  void sendInput(String data);

  /// 提高 scrollback 预算并重新取一次快照。返回 false 表示已经到头/取不动了。
  bool loadMoreScrollback();
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
    this.attachSettleDelay = const Duration(milliseconds: 150),
  }) : _channelFactory = channelFactory ?? WebSocketChannel.connect {
    _gatewaySubscription = gateway.stateStream.listen(_handleGatewayState);
  }

  final GatewayService gateway;
  final String sessionId;
  final TerminalChannelFactory _channelFactory;
  final Duration connectTimeout;
  final Duration resizeDebounce;

  /// 收到 `terminalConnected` 后等多久再 attach。留给页面一次布局：快捷键栏上屏会
  /// 把终端视口压矮，行数得在 attach 之前就落定，否则紧接着就是一次多余的 PTY resize。
  final Duration attachSettleDelay;

  final _eventsController = StreamController<TerminalStreamEvent>.broadcast();
  final _stateController = StreamController<TerminalStreamState>.broadcast();

  late final StreamSubscription<WsState> _gatewaySubscription;
  StreamSubscription<dynamic>? _channelSubscription;
  WebSocketChannel? _channel;
  Timer? _reconnectTimer;
  Timer? _resizeTimer;
  Timer? _attachTimer;
  TerminalGeometry? _geometry;
  TerminalGeometry? _lastSentGeometry;
  int _replayBytesRemaining = 0;
  bool _replayComplete = false;

  /// 下一次 attach 要声明的预算。null = 按视口算首屏预算（见
  /// [initialTerminalScrollbackLines]）；只有「补历史」那一次会把它提到上限。
  ///
  /// 补历史是「这一次」的动作，不该变成之后每次重连的固定成本——切前后台会频繁重连，
  /// 而重连本来就要重建终端、回到最新输出，上一次补的历史留不住。
  int? _nextScrollbackLines;

  /// 当前这一帧实际拿到的预算。「还能不能往上拉」按它判断。
  int _scrollbackLines = 0;
  int _historyLines = 0;
  bool _loadingMoreScrollback = false;
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
  bool get canLoadMoreScrollback =>
      !_disposed &&
      !_ended &&
      _scrollbackLines < kMaxTerminalScrollbackLines &&
      _historyLines > _scrollbackLines;

  @override
  bool loadMoreScrollback() {
    if (_loadingMoreScrollback || !canLoadMoreScrollback) return false;
    if (_state != TerminalStreamState.connected) return false;
    // 一次要到顶，不做翻倍爬坡：快照是整帧的，每次提预算都是把**已经拿过的**
    // 那段历史连同更老的一起重传。600→1200→2400→4800→5000 这条梯子累计要传
    // 14000 行，比改造前一次性 10000 行还多，而且每一级都要整帧重解码、把用户
    // 从正在看的位置挪走。终点本来就只有一个——xterm 的环形缓冲只存
    // [kMaxTerminalScrollbackLines] 行，再多的历史客户端也留不住。
    _scrollbackLines = kMaxTerminalScrollbackLines;
    _nextScrollbackLines = kMaxTerminalScrollbackLines;
    _loadingMoreScrollback = true;
    // 快照是整帧的，加不进去只能重取。重连路径已经会重建终端并按新预算 attach，
    // 所以这里只需要断开——不另起一套「增量追加」通路。
    _reconnectDelayMs = 500;
    _closeChannel();
    _setState(TerminalStreamState.waitingForGateway);
    unawaited(_open());
    return true;
  }

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
      // attach 不在这里发：等 `terminalConnected` 把 writeEnabled 带回来，页面把
      // chrome 摆完、行列落定了再 attach（见 [TerminalConnectedEvent]）。
      // 网关的 attach 窗口有 15 秒，这点等待绰绰有余。
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
          _eventsController.add(
            TerminalConnectedEvent(writeEnabled: _writeEnabled),
          );
          _attachTimer?.cancel();
          _attachTimer = Timer(
            attachSettleDelay,
            () => _sendAttach(generation),
          );
        case 'terminalReady':
          final cols = message['cols'] as int? ?? 80;
          final rows = message['rows'] as int? ?? 24;
          final geometry = _geometry;
          if (geometry != null &&
              (cols != geometry.cols || rows != geometry.rows)) {
            _reattachForGeometry(generation);
            return;
          }
          _writeEnabled = message['writeEnabled'] as bool? ?? false;
          _replayBytesRemaining = message['replayBytes'] as int? ?? 0;
          _replayComplete = false;
          _loadingMoreScrollback = false;
          // 老网关不下发 historyLines：保持 0，canLoadMoreScrollback 就永远是 false，
          // 手机不会去拉一个对端根本不支持的东西。
          _historyLines = message['historyLines'] as int? ?? 0;
          _reconnectDelayMs = 500;
          _setState(TerminalStreamState.connected);
          // 配色跟着每条连接来：同一部手机连不同设备，各自的主题（深浅色、
          // 用户自选底色）不一样，客户端不能有全局固定色板。
          final rawTheme = message['theme'];
          _eventsController.add(
            TerminalReadyEvent(
              cols: cols,
              rows: rows,
              replayBytes: message['replayBytes'] as int? ?? 0,
              writeEnabled: _writeEnabled,
              scrollbackLines:
                  message['scrollbackLines'] as int? ?? _scrollbackLines,
              historyLines: _historyLines,
              theme: rawTheme is Map<String, dynamic>
                  ? SmeltTerminalTheme.fromWire(rawTheme)
                  : null,
              themeIsDark: rawTheme is Map<String, dynamic>
                  ? SmeltTerminalTheme.isDark(rawTheme)
                  : true,
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

  /// 按落定后的行列 attach。一条连接只发一次。
  void _sendAttach(int generation) {
    if (_disposed || _ended || generation != _generation) return;
    final geometry = _geometry;
    if (geometry == null || _channel == null) return;
    _lastSentGeometry = geometry;
    _scrollbackLines =
        _nextScrollbackLines ?? initialTerminalScrollbackLines(geometry.rows);
    _nextScrollbackLines = null;
    _send({
      'method': 'attach',
      'params': {
        ...geometry.toJson(),
        'maxScrollbackLines': _scrollbackLines,
      },
    });
  }

  void _reattachForGeometry(int generation) {
    if (_disposed || generation != _generation) return;
    // Legacy gateways attach at the desktop geometry, resize the daemon, then
    // deliver that stale snapshot. Replaying and reflowing a large populated
    // xterm buffer corrupts its indexed scrollback. Drop the entire generation;
    // the gateway has already completed the requested resize before Ready, so
    // the next attachment receives an authoritative mobile-sized snapshot.
    _closeChannel();
    _setState(TerminalStreamState.waitingForGateway);
    _scheduleReconnect();
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
    _attachTimer?.cancel();
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
    _attachTimer?.cancel();
    // 这里**不**通知对端归还 PTY 尺寸租约：退出再进来是手机上最频繁的动作，中间
    // 还一次、进来再抢一次，等于让不切备用屏的 CLI 把整段对话重印两遍。尺寸由
    // 桌面侧的真实操作（敲键盘）或守护的宽限期回收。
    _closeChannel();
    await _gatewaySubscription.cancel();
    await _eventsController.close();
    await _stateController.close();
  }
}
