import 'dart:async';
import 'dart:typed_data';

import '../src/rust/api/error.dart';
import '../src/rust/api/terminal.dart' as rust;
import '../src/rust/api/terminal.dart' show TerminalGeometry;

export '../src/rust/api/terminal.dart' show TerminalGeometry;

/// 首次 attach 要多少行 scrollback。
///
/// 不切备用屏的 TUI（antigravity 这类完全靠终端滚动的）history 能堆到上万行，
/// 序列化后是几十万字节；手机走 iroh，一次全推过去就是进页面先干等。先只要最新的
/// 一小段，用户往上滚再按需要更多。
///
/// 按视口算而不是拍个常量：同一个行数在 60 行的小字体屏上是十几屏、在 20 行的大字体
/// 屏上是三十屏，本来就不该是同一个数。六屏的余量够随手往上翻几下；再往上才
/// 触发补历史（整帧重取，很贵，所以不能让它太容易发生）。底线 120 行是给极矮视口
/// （键盘弹起、分屏）兜的。
int initialTerminalScrollbackLines(int rows) {
  final budget = rows > 0 ? rows * 6 : 120;
  return budget.clamp(120, kMaxTerminalScrollbackLines);
}

/// scrollback 的硬上限，同时也是 xterm 缓冲区的行数。
///
/// 两者必须是同一个数：运行时发多了，超出的部分解码完就被 xterm 丢掉，纯浪费。
const int kMaxTerminalScrollbackLines = 5000;

/// 终端流的状态，全部由 Rust 内核推来的事件推导：
///
/// - [connecting]：已经（或即将）发起附着，还没收到 `Ready`。首次进页面、切回前台、
///   补历史重取都会经过它。
/// - [connected]：收到 `Ready`，或之后流上还有字节在来。
/// - [reconnecting]：收到非 fatal 错误，内核在后台按退避重新附着，成功后再来 `Ready`。
/// - [ended]：会话结束（`Closed`）或 fatal 错误。之后不再附着。
enum TerminalStreamState { connecting, connected, reconnecting, ended }

sealed class TerminalStreamEvent {
  const TerminalStreamEvent();
}

/// 附着成功（含内核自动重新附着）。每次都是一整帧新画面，页面要按 [cols]×[rows]
/// 重建终端再喂关键帧。
class TerminalReadyEvent extends TerminalStreamEvent {
  const TerminalReadyEvent({
    required this.cols,
    required this.rows,
    required this.replayBytes,
    this.scrollbackLines = 0,
    this.historyLines = 0,
  });

  final int cols;
  final int rows;
  final int replayBytes;

  /// 这一帧实际要到的行数预算。
  final int scrollbackLines;

  /// 会话侧总共还有多少行可取；大于 [scrollbackLines] 就说明上面还有更老的内容。
  final int historyLines;
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

/// Rust 内核的终端入口。单独抽出来是为了单测能注入假的，不加载动态库。
///
/// 语义以 `crates/smelt-mobile/src/api/terminal.rs` 的注释为准：全是同步调用，
/// 在 UI 线程上按调用顺序执行；同一会话再次 [attach] 会替换旧附着（旧流随之结束）。
abstract interface class TerminalBackend {
  Stream<rust.TerminalEvent> attach({
    required String sessionId,
    required TerminalGeometry geometry,
    int? maxScrollbackLines,
  });

  void resize({required String sessionId, required TerminalGeometry geometry});

  void input({required String sessionId, required String data});

  void detach({required String sessionId});
}

class RustTerminalBackend implements TerminalBackend {
  const RustTerminalBackend();

  @override
  Stream<rust.TerminalEvent> attach({
    required String sessionId,
    required TerminalGeometry geometry,
    int? maxScrollbackLines,
  }) => rust.terminalAttach(
    sessionId: sessionId,
    geometry: geometry,
    maxScrollbackLines: maxScrollbackLines,
  );

  @override
  void resize({
    required String sessionId,
    required TerminalGeometry geometry,
  }) => rust.terminalResize(sessionId: sessionId, geometry: geometry);

  @override
  void input({required String sessionId, required String data}) =>
      rust.terminalInput(sessionId: sessionId, data: data);

  @override
  void detach({required String sessionId}) =>
      rust.terminalDetach(sessionId: sessionId);
}

/// 终端流：Rust 内核附着之上的一层薄封装。
///
/// 连接、断线重连、运行时切换都在内核里做；这里只管「什么时候附着、要多少历史、
/// 尺寸怎么防抖」，并把内核事件翻成页面用的 [TerminalStreamEvent]。
class TerminalStreamService implements TerminalStreamClient {
  TerminalStreamService({
    required this.sessionId,
    this.backend = const RustTerminalBackend(),
    this.resizeDebounce = const Duration(milliseconds: 120),
    this.attachSettleDelay = const Duration(milliseconds: 150),
  });

  final String sessionId;
  final TerminalBackend backend;
  final Duration resizeDebounce;

  /// 拿到第一份几何（或切回前台）后等多久再附着。留给页面一次布局：字号偏好是异步
  /// 读出来的，读到后会改行列；行列得在附着之前就落定，否则紧接着就是一次多余的 PTY
  /// resize——不切备用屏的 CLI 每收一次 SIGWINCH 就把整段对话重印一遍。
  final Duration attachSettleDelay;

  final _eventsController = StreamController<TerminalStreamEvent>.broadcast();
  final _stateController = StreamController<TerminalStreamState>.broadcast();

  StreamSubscription<rust.TerminalEvent>? _subscription;
  Timer? _resizeTimer;
  Timer? _attachTimer;
  TerminalGeometry? _geometry;
  TerminalGeometry? _lastSentGeometry;

  /// 下一次附着要声明的预算。null = 按视口算首屏预算（见
  /// [initialTerminalScrollbackLines]）；只有「补历史」那一次会把它提到上限。
  ///
  /// 补历史是「这一次」的动作，不该变成之后每次切回前台的固定成本：重新附着本来就要
  /// 重建终端、回到最新输出，上一次补的历史留不住。
  int? _nextScrollbackLines;

  /// 当前这一帧实际拿到的预算。「还能不能往上拉」按它判断。
  int _scrollbackLines = 0;
  int _historyLines = 0;

  TerminalStreamState _state = TerminalStreamState.connecting;

  /// 每次附着加一；旧附着的事件和结束通知按它丢掉。
  int _generation = 0;

  /// 内核里有我们的附着（附着过、还没 detach、也没结束）。
  bool _attached = false;

  /// 没收到任何事件就被结束的连续次数，见 [_handleDone]。
  int _silentEnds = 0;
  bool _started = false;
  bool _suspended = false;
  bool _disposed = false;
  bool _ended = false;

  @override
  Stream<TerminalStreamEvent> get events => _eventsController.stream;
  @override
  Stream<TerminalStreamState> get stateStream => _stateController.stream;
  @override
  TerminalStreamState get state => _state;

  @override
  bool get canLoadMoreScrollback =>
      !_disposed &&
      !_ended &&
      _scrollbackLines < kMaxTerminalScrollbackLines &&
      _historyLines > _scrollbackLines;

  @override
  bool loadMoreScrollback() {
    if (!canLoadMoreScrollback || _state != TerminalStreamState.connected) {
      return false;
    }
    // 一次要到顶，不做翻倍爬坡：快照是整帧的，每次提预算都是把**已经拿过的**
    // 那段历史连同更老的一起重传。600→1200→2400→4800→5000 这条梯子累计要传
    // 14000 行，而且每一级都要整帧重解码、把用户从正在看的位置挪走。终点本来就只有
    // 一个——xterm 的环形缓冲只存 [kMaxTerminalScrollbackLines] 行。
    _scrollbackLines = kMaxTerminalScrollbackLines;
    _nextScrollbackLines = kMaxTerminalScrollbackLines;
    // 快照加不进去只能重取：同一会话再附着一次，内核替换旧附着、按新预算出关键帧。
    // 几何没变，不用等布局。
    _attach();
    return true;
  }

  @override
  void start(TerminalGeometry geometry) {
    if (_disposed || _ended) return;
    _geometry = geometry;
    if (_started) {
      updateGeometry(geometry);
      return;
    }
    _started = true;
    _scheduleAttach();
  }

  @override
  void updateGeometry(TerminalGeometry geometry) {
    if (_disposed || _ended) return;
    if (!_started) {
      start(geometry);
      return;
    }
    // 还在等布局落定：只记下来，附着时用最新的。
    _geometry = geometry;
    if (!_attached || geometry == _lastSentGeometry) return;
    _resizeTimer?.cancel();
    _resizeTimer = Timer(resizeDebounce, _flushResize);
  }

  void _flushResize() {
    final latest = _geometry;
    if (_disposed || !_attached || latest == null) return;
    if (latest == _lastSentGeometry) return;
    _lastSentGeometry = latest;
    try {
      backend.resize(sessionId: sessionId, geometry: latest);
    } catch (_) {
      // 只有附着已经不在内核里才会报错；那种情况流上会有 Closed / fatal 错误说明原因。
    }
  }

  @override
  void sendInput(String data) {
    if (data.isEmpty || !_attached) return;
    try {
      backend.input(sessionId: sessionId, data: data);
    } catch (error) {
      _emit(TerminalErrorEvent(_describe(error)));
    }
  }

  @override
  void suspend() {
    if (_disposed || _suspended) return;
    _suspended = true;
    _attachTimer?.cancel();
    _detach();
    if (!_ended) _setState(TerminalStreamState.connecting);
  }

  @override
  void resume() {
    if (_disposed || !_suspended) return;
    _suspended = false;
    if (_started && !_ended) _scheduleAttach();
  }

  void _scheduleAttach() {
    _attachTimer?.cancel();
    _attachTimer = Timer(attachSettleDelay, _attach);
  }

  void _attach() {
    final geometry = _geometry;
    if (_disposed || _suspended || _ended || !_started || geometry == null) {
      return;
    }
    _attachTimer?.cancel();
    _resizeTimer?.cancel();
    _cancelSubscription();
    final generation = ++_generation;
    final budget =
        _nextScrollbackLines ?? initialTerminalScrollbackLines(geometry.rows);
    _nextScrollbackLines = null;
    _setState(TerminalStreamState.connecting);
    final Stream<rust.TerminalEvent> stream;
    try {
      stream = backend.attach(
        sessionId: sessionId,
        geometry: geometry,
        maxScrollbackLines: budget,
      );
    } catch (error) {
      // 同步报错只有调用错误（行列为 0、会话 id 为空），重试也没用。
      _attached = false;
      _conclude(TerminalErrorEvent(_describe(error), fatal: true));
      return;
    }
    _attached = true;
    _lastSentGeometry = geometry;
    _subscription = stream.listen(
      (event) => _handleEvent(event, generation),
      onError: (Object error) {
        if (generation != _generation) return;
        _emit(TerminalErrorEvent(_describe(error)));
      },
      onDone: () => _handleDone(generation),
    );
  }

  void _handleEvent(rust.TerminalEvent event, int generation) {
    if (_disposed || generation != _generation) return;
    _silentEnds = 0;
    switch (event.kind) {
      case rust.TerminalEventKind.ready:
        final ready = event.ready;
        if (ready == null) return;
        _scrollbackLines = ready.scrollbackLines;
        _historyLines = ready.historyLines;
        _setState(TerminalStreamState.connected);
        _emit(
          TerminalReadyEvent(
            cols: ready.cols,
            rows: ready.rows,
            replayBytes: ready.replayBytes,
            scrollbackLines: ready.scrollbackLines,
            historyLines: ready.historyLines,
          ),
        );
      case rust.TerminalEventKind.data:
        final bytes = event.data;
        if (bytes == null || bytes.isEmpty) return;
        // 非 fatal 错误也可能只是一次输入没发出去，附着本身还活着：流上还有字节来，
        // 就说明没在重连。
        _setState(TerminalStreamState.connected);
        _emit(TerminalDataEvent(bytes));
      case rust.TerminalEventKind.replayComplete:
        _setState(TerminalStreamState.connected);
        _emit(const TerminalReplayCompleteEvent());
      case rust.TerminalEventKind.closed:
        _attached = false;
        _conclude(const TerminalClosedEvent());
      case rust.TerminalEventKind.error:
        final message = event.message ?? 'Terminal request failed';
        if (event.fatal) {
          _attached = false;
          _conclude(TerminalErrorEvent(message, fatal: true));
          return;
        }
        _setState(TerminalStreamState.reconnecting);
        _emit(TerminalErrorEvent(message));
    }
  }

  /// 流结束了，却不是我们 detach / 替换的，也没有 Closed / fatal 说明原因：附着被别处
  /// 摘掉了（`terminalDetachAll`，或刚退出的同会话页面在 dispose 里 detach 了我们
  /// 新建的附着——内核按会话 id 认附着）。重新附着一次；连着两次一个事件都没收到就
  /// 结束，免得空转。
  void _handleDone(int generation) {
    if (_disposed || generation != _generation) return;
    _subscription = null;
    _attached = false;
    if (_ended || _suspended) return;
    _silentEnds++;
    if (_silentEnds > 1) {
      _conclude(const TerminalErrorEvent('Terminal stream ended', fatal: true));
      return;
    }
    _attach();
  }

  void _conclude(TerminalStreamEvent event) {
    _ended = true;
    _attachTimer?.cancel();
    _resizeTimer?.cancel();
    _cancelSubscription();
    _setState(TerminalStreamState.ended);
    _emit(event);
  }

  /// 断开附着：内核关掉连接（运行时据此进入几何租约的宽限期），不再重连。
  void _detach() {
    _resizeTimer?.cancel();
    _cancelSubscription();
    if (!_attached) return;
    _attached = false;
    _lastSentGeometry = null;
    try {
      backend.detach(sessionId: sessionId);
    } catch (_) {}
  }

  void _cancelSubscription() {
    // 先作废代号：cancel 之后内核那边可能还有在途事件或结束通知。
    _generation++;
    final subscription = _subscription;
    _subscription = null;
    if (subscription != null) unawaited(subscription.cancel());
  }

  void _emit(TerminalStreamEvent event) {
    if (_eventsController.isClosed) return;
    _eventsController.add(event);
  }

  void _setState(TerminalStreamState next) {
    if (_state == next || _stateController.isClosed) return;
    _state = next;
    _stateController.add(next);
  }

  static String _describe(Object error) =>
      error is SmeltError ? error.message : error.toString();

  @override
  Future<void> dispose() async {
    if (_disposed) return;
    _disposed = true;
    _attachTimer?.cancel();
    _detach();
    // 不等 close 完成：done 要投递给监听者才算完，监听者已经取消时在测试的
    // fakeAsync 里会一直等下去；dispose 只负责释放，不必等对方收到结束。
    unawaited(_eventsController.close());
    unawaited(_stateController.close());
  }
}
