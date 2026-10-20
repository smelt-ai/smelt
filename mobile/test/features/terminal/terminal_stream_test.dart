import 'dart:async';
import 'dart:typed_data';

import 'package:fake_async/fake_async.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/models.dart' as rust;
import 'package:smelt_mobile/data/models.dart'
    show SmeltError, SmeltErrorKind, TerminalGeometry;
import 'package:smelt_mobile/features/terminal/terminal_stream.dart';

/// 一次 [_FakeBackend.terminalAttach]。
class _Attach {
  _Attach(
    this.runtimeId,
    this.sessionId,
    this.geometry,
    this.maxScrollbackLines,
  );

  final String runtimeId;
  final String sessionId;
  final TerminalGeometry geometry;
  final int? maxScrollbackLines;
  final controller = StreamController<rust.TerminalEvent>();

  void add(rust.TerminalEvent event) => controller.add(event);
}

/// 假的 Rust 内核：记下终端调用，事件由测试手动推。同一会话再次附着、detach 都会
/// 结束旧流，与内核一致。其余接口沿用演示后端（这里用不到）。
class _FakeBackend extends DemoBackend {
  final attaches = <_Attach>[];
  final resizes = <TerminalGeometry>[];
  final inputs = <String>[];
  final detaches = <String>[];
  Object? attachError;
  Object? inputError;

  _Attach get current => attaches.last;

  @override
  Stream<rust.TerminalEvent> terminalAttach(
    String runtimeId,
    String sessionId, {
    required TerminalGeometry geometry,
    int? maxScrollbackLines,
  }) {
    final error = attachError;
    if (error != null) throw error;
    _endCurrent();
    final attach = _Attach(runtimeId, sessionId, geometry, maxScrollbackLines);
    attaches.add(attach);
    return attach.controller.stream;
  }

  @override
  void terminalResize(
    String runtimeId,
    String sessionId,
    TerminalGeometry geometry,
  ) {
    resizes.add(geometry);
  }

  @override
  void terminalInput(String runtimeId, String sessionId, String data) {
    final error = inputError;
    if (error != null) throw error;
    inputs.add(data);
  }

  @override
  void terminalDetach(String runtimeId, String sessionId) {
    detaches.add(sessionId);
    _endCurrent();
  }

  void _endCurrent() {
    if (attaches.isEmpty) return;
    final controller = attaches.last.controller;
    if (!controller.isClosed) unawaited(controller.close());
  }
}

const _g40x20 = TerminalGeometry(
  cols: 40,
  rows: 20,
  cellWidth: 8,
  cellHeight: 16,
);
const _g40x32 = TerminalGeometry(
  cols: 40,
  rows: 32,
  cellWidth: 8,
  cellHeight: 16,
);
const _g50x24 = TerminalGeometry(
  cols: 50,
  rows: 24,
  cellWidth: 8,
  cellHeight: 16,
);

rust.TerminalEvent _ready({
  int cols = 40,
  int rows = 20,
  int replayBytes = 0,
  int historyLines = 0,
  int scrollbackLines = 120,
}) => rust.TerminalEvent(
  kind: rust.TerminalEventKind.ready,
  ready: rust.TerminalReady(
    cols: cols,
    rows: rows,
    replayBytes: replayBytes,
    historyLines: historyLines,
    scrollbackLines: scrollbackLines,
  ),
  fatal: false,
);

rust.TerminalEvent _data(List<int> bytes) => rust.TerminalEvent(
  kind: rust.TerminalEventKind.data,
  data: Uint8List.fromList(bytes),
  fatal: false,
);

const _replayComplete = rust.TerminalEvent(
  kind: rust.TerminalEventKind.replayComplete,
  fatal: false,
);

const _closed = rust.TerminalEvent(
  kind: rust.TerminalEventKind.closed,
  fatal: false,
);

rust.TerminalEvent _error(String message, {bool fatal = false}) =>
    rust.TerminalEvent(
      kind: rust.TerminalEventKind.error,
      message: message,
      fatal: fatal,
    );

const _settle = Duration(milliseconds: 150);
const _debounce = Duration(milliseconds: 120);

class _Harness {
  _Harness() {
    _subscriptions
      ..add(service.events.listen(events.add))
      ..add(service.stateStream.listen(states.add));
  }

  final backend = _FakeBackend();
  late final service = TerminalStream(
    runtimeId: 'rt-1',
    sessionId: 'terminal-1',
    backend: backend,
    resizeDebounce: _debounce,
    attachSettleDelay: _settle,
  );
  final events = <TerminalStreamEvent>[];
  final states = <TerminalStreamState>[];
  final _subscriptions = <StreamSubscription<Object>>[];

  /// 开流并附着上，收到 Ready + 回放结束。
  void connect(
    FakeAsync async, {
    TerminalGeometry geometry = _g40x20,
    int historyLines = 0,
    int scrollbackLines = 120,
  }) {
    service.start(geometry);
    async.elapse(_settle);
    backend.current
      ..add(
        _ready(
          cols: geometry.cols,
          rows: geometry.rows,
          historyLines: historyLines,
          scrollbackLines: scrollbackLines,
        ),
      )
      ..add(_replayComplete);
    async.flushMicrotasks();
  }

  void dispose() {
    for (final subscription in _subscriptions) {
      unawaited(subscription.cancel());
    }
    unawaited(service.dispose());
  }
}

/// 终端服务不碰 widget，用纯 Dart 的 fakeAsync 推时间：debounce、落定延迟都靠它。
/// 不用 testWidgets——它收尾时的不变量检查会和这里的流卡在一起。
void testFake(String description, void Function(FakeAsync async) body) {
  test(description, () => fakeAsync(body));
}

void main() {
  /// attach 必须等页面把 chrome 摆完、行列落定之后再发。先 attach 再改尺寸 = 一次多余的
  /// PTY resize，而不切备用屏的 CLI 每收一次 SIGWINCH 就把整段对话重印一遍。
  testFake('attach waits for the settled geometry', (async) {
    final h = _Harness();
    expect(h.service.state, TerminalStreamState.connecting);

    // 页面第一次量到的几何，随后字号偏好读出来又改了一次。
    h.service.start(_g40x20);
    async.elapse(const Duration(milliseconds: 100));
    expect(h.backend.attaches, isEmpty, reason: '行列未落定之前不能 attach');
    h.service.updateGeometry(_g40x32);
    async.elapse(_settle);

    expect(h.backend.attaches, hasLength(1));
    final attach = h.backend.current;
    expect((attach.runtimeId, attach.sessionId), ('rt-1', 'terminal-1'));
    expect(attach.geometry, _g40x32);
    expect(
      attach.maxScrollbackLines,
      initialTerminalScrollbackLines(32),
      reason: '首屏预算按视口算：矮终端不该跟高终端要同样多历史',
    );
    expect(h.backend.resizes, isEmpty, reason: '落定前的几何变化不该变成 resize');

    // 落定后再量到的同一份几何不应该变成 resize。
    h.service.updateGeometry(_g40x32);
    async.elapse(_debounce * 2);
    expect(h.backend.resizes, isEmpty);
    h.dispose();
  });

  testFake('forwards kernel events and derives the stream state', (async) {
    final h = _Harness();
    h.service.start(_g40x20);
    async.elapse(_settle);
    expect(h.service.state, TerminalStreamState.connecting);

    h.backend.current
      ..add(_ready(replayBytes: 3, historyLines: 500, scrollbackLines: 120))
      ..add(_data([0xe4, 0xb8, 0xad]))
      ..add(_replayComplete);
    async.flushMicrotasks();
    expect(h.service.state, TerminalStreamState.connected);
    final ready = h.events.first as TerminalReadyEvent;
    expect((ready.cols, ready.rows, ready.replayBytes), (40, 20, 3));
    expect((ready.scrollbackLines, ready.historyLines), (120, 500));
    expect((h.events[1] as TerminalDataEvent).bytes, [0xe4, 0xb8, 0xad]);
    expect(h.events[2], isA<TerminalReplayCompleteEvent>());

    // 非 fatal：内核在后台重连。
    h.backend.current.add(_error('终端连接断开：eof'));
    async.flushMicrotasks();
    expect(h.service.state, TerminalStreamState.reconnecting);
    final error = h.events.last as TerminalErrorEvent;
    expect((error.message, error.fatal), ('终端连接断开：eof', false));

    // 非 fatal 也可能只是一次输入没发出去：流上还有字节来，就没在重连。
    h.backend.current.add(_data([0x41]));
    async.flushMicrotasks();
    expect(h.service.state, TerminalStreamState.connected);

    // 重新附着成功：内核再来一次 Ready。
    h.backend.current.add(_error('终端连接断开：eof'));
    async.flushMicrotasks();
    h.backend.current.add(_ready());
    async.flushMicrotasks();
    expect(h.service.state, TerminalStreamState.connected);
    expect(h.events.whereType<TerminalReadyEvent>(), hasLength(2));
    expect(h.backend.attaches, hasLength(1), reason: '重连由内核做，Dart 不再附着');

    h.backend.current.add(_error('终端会话不存在或已结束', fatal: true));
    async.flushMicrotasks();
    expect(h.service.state, TerminalStreamState.ended);
    expect((h.events.last as TerminalErrorEvent).fatal, isTrue);
    expect(h.states, [
      TerminalStreamState.connected,
      TerminalStreamState.reconnecting,
      TerminalStreamState.connected,
      TerminalStreamState.reconnecting,
      TerminalStreamState.connected,
      TerminalStreamState.ended,
    ]);

    // 结束后不再附着、不再 resize，也不再 detach（内核已经把它摘了）。
    h.service.suspend();
    h.service.resume();
    h.service.updateGeometry(_g50x24);
    async.elapse(_settle + _debounce);
    expect(h.backend.attaches, hasLength(1));
    expect(h.backend.resizes, isEmpty);
    h.dispose();
    expect(h.backend.detaches, isEmpty);
  });

  testFake('a closed session ends the stream', (async) {
    final h = _Harness();
    h.connect(async);
    h.backend.current.add(_closed);
    async.flushMicrotasks();
    expect(h.service.state, TerminalStreamState.ended);
    expect(h.events.last, isA<TerminalClosedEvent>());
    expect(h.service.canLoadMoreScrollback, isFalse);
    h.dispose();
    expect(h.backend.detaches, isEmpty);
  });

  testFake('a rejected attach call is fatal', (async) {
    final h = _Harness();
    h.backend.attachError = const SmeltError(
      kind: SmeltErrorKind.other,
      message: '终端行列必须大于 0',
    );
    h.service.start(_g40x20);
    async.elapse(_settle);
    expect(h.service.state, TerminalStreamState.ended);
    final error = h.events.single as TerminalErrorEvent;
    expect((error.message, error.fatal), ('终端行列必须大于 0', true));
    h.dispose();
  });

  testFake('resize is debounced and deduplicated', (async) {
    final h = _Harness();
    h.connect(async);

    // 连续几次测量只落最后一次。
    h.service.updateGeometry(_g40x32);
    async.elapse(const Duration(milliseconds: 60));
    h.service.updateGeometry(_g50x24);
    async.elapse(const Duration(milliseconds: 60));
    expect(h.backend.resizes, isEmpty);
    async.elapse(_debounce);
    expect(h.backend.resizes, [_g50x24]);

    // 跟上次发出的一样：不发。
    h.service.updateGeometry(_g50x24);
    async.elapse(_debounce * 2);
    expect(h.backend.resizes, [_g50x24]);

    // 防抖窗口里改了又改回来：也不发。
    h.service.updateGeometry(_g40x20);
    async.elapse(const Duration(milliseconds: 60));
    h.service.updateGeometry(_g50x24);
    async.elapse(_debounce * 2);
    expect(h.backend.resizes, [_g50x24]);

    // 重连期间照样交给内核：它记下来，下次附着按它来。
    h.backend.current.add(_error('终端连接断开：eof'));
    async.flushMicrotasks();
    h.service.updateGeometry(_g40x20);
    async.elapse(_debounce);
    expect(h.backend.resizes, [_g50x24, _g40x20]);
    h.dispose();
  });

  testFake('input goes to the kernel only while attached', (async) {
    final h = _Harness();
    h.service.sendInput('early');
    h.service.start(_g40x20);
    h.service.sendInput('before-attach');
    async.elapse(_settle);
    h.service.sendInput('');
    h.service.sendInput('\x03');
    expect(h.backend.inputs, ['\x03']);

    h.backend.inputError = const SmeltError(
      kind: SmeltErrorKind.other,
      message: '没有附着这个终端',
    );
    h.service.sendInput('x');
    async.flushMicrotasks();
    final error = h.events.last as TerminalErrorEvent;
    expect((error.message, error.fatal), ('没有附着这个终端', false));
    expect(h.service.state, TerminalStreamState.connecting);
    h.dispose();
  });

  /// 首屏只要最新的一段，用户往上滚才去要更老的——手机走 iroh，不切备用屏的 TUI
  /// history 全推过去就是进页面先干等。
  testFake('loading more history re-attaches once with the full budget', (
    async,
  ) {
    final h = _Harness();
    h.connect(async, historyLines: 4000, scrollbackLines: 120);
    expect(h.service.canLoadMoreScrollback, isTrue);

    expect(h.service.loadMoreScrollback(), isTrue);
    expect(h.backend.attaches, hasLength(2), reason: '提高预算必须重取一次快照，且不用等布局');
    expect(
      h.backend.current.maxScrollbackLines,
      kMaxTerminalScrollbackLines,
      reason: '快照是整帧的，翻倍爬坡等于把同一段历史重传好几遍',
    );
    expect(h.backend.current.geometry, _g40x20);
    expect(h.service.state, TerminalStreamState.connecting);
    expect(h.service.canLoadMoreScrollback, isFalse, reason: '在途时不能再触发一次');
    expect(h.service.loadMoreScrollback(), isFalse);

    // 旧流被内核结束：那是我们自己换掉的，不能当成意外断开再附着一次。
    async.flushMicrotasks();
    expect(h.backend.attaches, hasLength(2));

    h.backend.current.add(
      _ready(historyLines: 4000, scrollbackLines: kMaxTerminalScrollbackLines),
    );
    async.flushMicrotasks();
    expect(h.service.state, TerminalStreamState.connected);
    expect(
      h.service.canLoadMoreScrollback,
      isFalse,
      reason: '已经要到客户端能留住的全部，不该再有第二次重取',
    );
    expect(h.events.whereType<TerminalReadyEvent>().last.historyLines, 4000);

    // 切后台再切回来：重新附着不能把「补历史」的大预算一起带上。
    h.service.suspend();
    h.service.resume();
    async.elapse(_settle);
    expect(h.backend.attaches, hasLength(3));
    expect(
      h.backend.current.maxScrollbackLines,
      initialTerminalScrollbackLines(20),
      reason: '补历史是「这一次」的动作，不该粘在之后每次附着上',
    );
    h.dispose();
  });

  testFake('no history beyond the budget never triggers a refetch', (async) {
    final h = _Harness();
    expect(h.service.loadMoreScrollback(), isFalse);
    h.connect(async, historyLines: 80, scrollbackLines: 120);
    expect(h.service.canLoadMoreScrollback, isFalse);
    expect(h.service.loadMoreScrollback(), isFalse);
    expect(h.backend.attaches, hasLength(1));

    // 只在连着的时候补：重连期间去重取只会打断内核的重连。
    final more = _Harness();
    more.connect(async, historyLines: 4000);
    more.backend.current.add(_error('终端连接断开：eof'));
    async.flushMicrotasks();
    expect(more.service.loadMoreScrollback(), isFalse);
    expect(more.backend.attaches, hasLength(1));

    h.dispose();
    more.dispose();
  });

  testFake('suspend detaches and resume re-attaches at the latest size', (
    async,
  ) {
    final h = _Harness();
    h.connect(async);
    final before = h.backend.current;

    h.service.suspend();
    expect(h.backend.detaches, ['terminal-1']);
    expect(h.service.state, TerminalStreamState.connecting);
    // 后台期间：几何只记下，不 resize；输入丢掉；旧流的事件不再转发。
    h.service.updateGeometry(_g50x24);
    h.service.sendInput('x');
    async.elapse(_debounce * 2);
    expect(h.backend.resizes, isEmpty);
    expect(h.backend.inputs, isEmpty);
    expect(before.controller.isClosed, isTrue);
    final eventsBefore = h.events.length;

    h.service.resume();
    expect(h.backend.attaches, hasLength(1), reason: '切回前台也等布局落定');
    async.elapse(_settle);
    expect(h.backend.attaches, hasLength(2));
    expect(h.backend.current.geometry, _g50x24);
    expect(h.events, hasLength(eventsBefore));

    h.backend.current.add(_ready(cols: 50, rows: 24));
    async.flushMicrotasks();
    expect(h.service.state, TerminalStreamState.connected);

    // suspend 在附着前：不 detach，resume 照样附着。
    final early = _Harness();
    early.service.start(_g40x20);
    early.service.suspend();
    async.elapse(_settle);
    expect(early.backend.attaches, isEmpty);
    expect(early.backend.detaches, isEmpty);
    early.service.resume();
    async.elapse(_settle);
    expect(early.backend.attaches, hasLength(1));

    h.dispose();
    early.dispose();
  });

  testFake('a stream ended elsewhere is re-attached, but not forever', (async) {
    final h = _Harness();
    h.connect(async);

    // 别处 detach 了这个会话（如 terminalDetachAll）：流悄悄结束。
    h.backend.current.controller.close();
    async.flushMicrotasks();
    async.flushMicrotasks();
    expect(h.backend.attaches, hasLength(2));
    expect(h.service.state, TerminalStreamState.connecting);

    // 新附着一个事件都没收到又被结束：再试只会空转。
    h.backend.current.controller.close();
    async.flushMicrotasks();
    async.flushMicrotasks();
    expect(h.backend.attaches, hasLength(2));
    expect(h.service.state, TerminalStreamState.ended);
    expect((h.events.last as TerminalErrorEvent).fatal, isTrue);
    h.dispose();
  });

  testFake('dispose detaches a live attachment', (async) {
    final h = _Harness();
    h.connect(async);
    h.dispose();
    expect(h.backend.detaches, ['terminal-1']);

    // 还没附着就退出：没什么可 detach，也不会在之后附着。
    final idle = _Harness();
    idle.service.start(_g40x20);
    idle.dispose();
    async.elapse(_settle);
    expect(idle.backend.attaches, isEmpty);
    expect(idle.backend.detaches, isEmpty);
  });
}
