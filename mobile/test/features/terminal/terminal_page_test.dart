import 'dart:async';
import 'dart:convert';

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/preferences.dart';
import 'package:smelt_mobile/data/stores.dart';
import 'package:smelt_mobile/features/terminal/terminal_page.dart';
import 'package:smelt_mobile/features/terminal/terminal_shortcut_bar.dart';
import 'package:smelt_mobile/features/terminal/terminal_stream.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';
import 'package:smelt_mobile/theme/primitives.dart';
import 'package:smelt_mobile/theme/terminal_theme.dart';
import 'package:smelt_mobile/theme/theme.dart';
import 'package:xterm/xterm.dart';

/// 演示后端里那条终端会话。
const _runtimeId = 'rt-mbp';
const _sessionId = 's-zsh';

/// 附着前要等的布局落定时间（见 [TerminalStream.attachSettleDelay]），多给一点。
const _settle = Duration(milliseconds: 200);

/// 一次附着：事件由测试手动推。
class _Attach {
  _Attach(this.geometry, this.maxScrollbackLines);

  final TerminalGeometry geometry;
  final int? maxScrollbackLines;
  final controller = StreamController<TerminalEvent>(sync: true);

  void ready({
    required int cols,
    required int rows,
    int historyLines = 0,
    int scrollbackLines = 120,
  }) => controller.add(
    TerminalEvent(
      kind: TerminalEventKind.ready,
      ready: TerminalReady(
        cols: cols,
        rows: rows,
        replayBytes: 0,
        historyLines: historyLines,
        scrollbackLines: scrollbackLines,
      ),
      fatal: false,
    ),
  );

  void data(String text) => controller.add(
    TerminalEvent(
      kind: TerminalEventKind.data,
      data: Uint8List.fromList(utf8.encode(text)),
      fatal: false,
    ),
  );

  void bytes(List<int> bytes) => controller.add(
    TerminalEvent(
      kind: TerminalEventKind.data,
      data: Uint8List.fromList(bytes),
      fatal: false,
    ),
  );

  void replayComplete() => controller.add(
    const TerminalEvent(kind: TerminalEventKind.replayComplete, fatal: false),
  );

  void closed() => controller.add(
    const TerminalEvent(kind: TerminalEventKind.closed, fatal: false),
  );

  void error(String message, {bool fatal = false}) => controller.add(
    TerminalEvent(
      kind: TerminalEventKind.error,
      message: message,
      fatal: fatal,
    ),
  );
}

/// 演示后端，终端部分换成测试手推的流，并记下发给内核的输入与 resize。
class _ScriptedBackend extends DemoBackend {
  final attaches = <_Attach>[];
  final resizes = <TerminalGeometry>[];
  final sentInput = <String>[];

  _Attach get current => attaches.last;

  @override
  Stream<TerminalEvent> terminalAttach(
    String runtimeId,
    String sessionId, {
    required TerminalGeometry geometry,
    int? maxScrollbackLines,
  }) {
    _endCurrent();
    final attach = _Attach(geometry, maxScrollbackLines);
    attaches.add(attach);
    return attach.controller.stream;
  }

  @override
  void terminalResize(
    String runtimeId,
    String sessionId,
    TerminalGeometry geometry,
  ) => resizes.add(geometry);

  @override
  void terminalInput(String runtimeId, String sessionId, String data) =>
      sentInput.add(data);

  @override
  void terminalDetach(String runtimeId, String sessionId) => _endCurrent();

  void _endCurrent() {
    if (attaches.isEmpty) return;
    final controller = attaches.last.controller;
    if (!controller.isClosed) unawaited(controller.close());
  }
}

/// 只记下输入、其余照演示后端走（附着后推合成输出、回显）。
class _RecordingDemoBackend extends DemoBackend {
  final sentInput = <String>[];

  @override
  void terminalInput(String runtimeId, String sessionId, String data) {
    sentInput.add(data);
    super.terminalInput(runtimeId, sessionId, data);
  }
}

Widget _app(
  AppModel model, {
  Brightness brightness = Brightness.light,
  Widget? home,
}) => AppScope(
  model: model,
  child: MaterialApp(
    theme: buildGalleyTheme(brightness),
    localizationsDelegates: L10n.localizationsDelegates,
    supportedLocales: L10n.supportedLocales,
    home:
        home ??
        const TerminalPage(runtimeId: _runtimeId, sessionId: _sessionId),
  ),
);

/// 进页面，等布局落定、附着发出。
Future<AppModel> _open(
  WidgetTester tester,
  DemoBackend backend, {
  Brightness brightness = Brightness.light,
}) async {
  final model = AppModel(backend, PreferencesStore.memory())..start();
  await tester.pumpWidget(_app(model, brightness: brightness));
  await tester.pump();
  await tester.pump(_settle);
  return model;
}

/// 收尾：先卸页面（取消订阅、detach），再停数据根。
Future<void> _close(WidgetTester tester, AppModel model) async {
  await tester.pumpWidget(const SizedBox.shrink());
  model.dispose();
}

Future<void> _ready(WidgetTester tester, _ScriptedBackend backend) async {
  backend.current
    ..ready(cols: 40, rows: 20)
    ..replayComplete();
  await tester.pump();
  await tester.pump();
}

TerminalView _view(WidgetTester tester) =>
    tester.widget<TerminalView>(find.byType(TerminalView));

Finder _iconButton(IconData icon) =>
    find.ancestor(of: find.byIcon(icon), matching: find.byType(IconButton));

Future<Offset> _cellGlobal(WidgetTester tester, CellOffset cell) async {
  final state = tester.state<TerminalViewState>(find.byType(TerminalView));
  final render = state.renderTerminal;
  final size = render.cellSize;
  return render.localToGlobal(
    render.getOffset(cell) + Offset(size.width / 2, size.height / 2),
  );
}

Future<void> _tapCell(WidgetTester tester, CellOffset cell) async {
  await tester.tapAt(await _cellGlobal(tester, cell));
  await tester.pump();
}

/// 拦下 url_launcher 的平台调用，记下要打开的链接。
List<Uri> _captureLaunches(WidgetTester tester) {
  final opened = <Uri>[];
  const channel = MethodChannel('plugins.flutter.io/url_launcher');
  tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(channel, (
    call,
  ) async {
    if (call.method == 'launch') {
      opened.add(Uri.parse((call.arguments as Map)['url'] as String));
      return true;
    }
    return true;
  });
  addTearDown(
    () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      channel,
      null,
    ),
  );
  return opened;
}

void main() {
  testWidgets('演示后端：显示会话与机器，推合成输出，快捷键送到 terminalInput', (tester) async {
    final backend = _RecordingDemoBackend();
    final model = await _open(tester, backend);

    expect(find.text('zsh'), findsOneWidget);
    expect(find.text('MacBook Pro'), findsOneWidget);
    expect(find.text('Live'), findsOneWidget);

    // 合成的 cargo build 一行一行推上来。
    await tester.pump(const Duration(seconds: 4));
    expect(_view(tester).terminal.buffer.getText(), contains('Finished'));
    expect(_view(tester).theme.background, Primitives.termBg);

    await tester.tap(find.text('Esc'));
    await tester.tap(find.text('^C'));
    await tester.tap(find.byTooltip('Up'));
    await tester.tap(find.text('/'));
    await tester.pump();
    expect(backend.sentInput, ['\x1b', '\x03', '\x1b[A', '/']);

    await _close(tester, model);
  });

  testWidgets('进页面是浏览态：不弹键盘，没连上不收输入', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);

    final view = _view(tester);
    expect(view.autofocus, isFalse);
    expect(view.hardwareKeyboardOnly, isTrue);
    expect(view.focusNode?.hasFocus, isFalse);
    expect(view.readOnly, isTrue, reason: '还没附着上，不收键盘输入');
    expect(find.text('Connecting…'), findsOneWidget);
    expect(
      tester.widget<IconButton>(_iconButton(LucideIcons.keyboard)).onPressed,
      isNull,
    );
    // 快捷键栏从一开始就在：它得在 attach 之前把终端视口压到最终高度。
    expect(find.byType(TerminalShortcutBar), findsOneWidget);
    expect(backend.attaches, hasLength(1));

    await _close(tester, model);
  });

  testWidgets('终端固定深色，不跟 App 明暗', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend, brightness: Brightness.light);
    await _ready(tester, backend);
    final view = _view(tester);
    expect(view.theme, same(SmeltTerminalTheme.dark));
    expect(view.theme.background, Primitives.termBg);
    expect(view.theme.foreground, Primitives.termFg);
    expect(view.textStyle.fontFamily, 'JetBrainsMonoNL');
    await _close(tester, model);
  });

  testWidgets('字号跟偏好走', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    await _ready(tester, backend);
    expect(
      _view(tester).textStyle.fontSize,
      Preferences.defaultTerminalFontSize,
    );

    await tester.tap(find.byTooltip('Text size'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('18 pt'));
    await tester.pumpAndSettle();
    expect(model.prefs.value.terminalFontSize, 18);
    expect(_view(tester).textStyle.fontSize, 18);

    await _close(tester, model);
  });

  testWidgets('重连期间照常可写，会话结束后收起快捷键栏', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    await _ready(tester, backend);
    expect(find.byType(TerminalShortcutBar), findsOneWidget);
    expect(find.text('Live'), findsOneWidget);

    // 重连期间不收栏子（会改行数），键盘也照样能开。
    backend.current.error('终端连接断开：eof');
    await tester.pump();
    expect(find.byType(TerminalShortcutBar), findsOneWidget);
    expect(_view(tester).readOnly, isFalse);
    expect(find.text('终端连接断开：eof'), findsOneWidget);
    expect(find.text('Reconnecting…'), findsOneWidget);

    backend.current.closed();
    await tester.pump();
    expect(find.byType(TerminalShortcutBar), findsNothing);
    expect(find.text('This terminal session has ended.'), findsOneWidget);
    expect(find.text('Ended'), findsOneWidget);
    expect(_view(tester).readOnly, isTrue);

    await _close(tester, model);
  });

  testWidgets('流被悄悄结束两次：诚实地说连不上了', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    await _ready(tester, backend);

    unawaited(backend.current.controller.close());
    await tester.pump();
    expect(backend.attaches, hasLength(2), reason: '第一次悄悄结束会重新附着');
    unawaited(backend.current.controller.close());
    await tester.pump();
    expect(find.textContaining('Lost this terminal'), findsOneWidget);
    expect(find.text('Ended'), findsOneWidget);

    await _close(tester, model);
  });

  testWidgets('快捷键栏不开软键盘也能用', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    await _ready(tester, backend);

    expect(find.byIcon(LucideIcons.keyboard), findsOneWidget);
    await tester.tap(find.text('^C'));
    await tester.pump();
    expect(backend.sentInput, contains('\u0003'));

    await _close(tester, model);
  });

  testWidgets('快捷键栏只报键，编码交给终端', (tester) async {
    final keys = <String>[];
    final texts = <String>[];
    var ctrl = 0;
    await tester.pumpWidget(
      MaterialApp(
        theme: buildGalleyTheme(Brightness.light),
        localizationsDelegates: L10n.localizationsDelegates,
        supportedLocales: L10n.supportedLocales,
        home: Scaffold(
          body: TerminalShortcutBar(
            onKey: (key, {shift = false, alt = false, ctrl = false}) =>
                keys.add('$key shift=$shift alt=$alt ctrl=$ctrl'),
            onText: texts.add,
            onPaste: () {},
            ctrlLatched: false,
            onToggleCtrl: () => ctrl++,
          ),
        ),
      ),
    );

    await tester.tap(find.text('Esc'));
    await tester.tap(find.text('^C'));
    await tester.tap(find.byTooltip('Up'));
    // 这两个组合软键盘上按不出来，只能靠快捷键栏。
    await tester.tap(find.byTooltip('Shift+Tab'));
    await tester.tap(find.byTooltip('Shift+Enter (new line)'));
    await tester.tap(find.text('Ctrl'));
    await tester.tap(find.text('|'));

    expect(keys, [
      'TerminalKey.escape shift=false alt=false ctrl=false',
      'TerminalKey.keyC shift=false alt=false ctrl=true',
      'TerminalKey.arrowUp shift=false alt=false ctrl=false',
      'TerminalKey.tab shift=true alt=false ctrl=false',
      'TerminalKey.enter shift=true alt=false ctrl=false',
    ]);
    expect(ctrl, 1);
    expect(texts, ['|']);
    expect(tester.takeException(), isNull);
  });

  testWidgets('快捷键栏的键高够触控下限', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    final esc = find.ancestor(
      of: find.text('Esc'),
      matching: find.byType(InkWell),
    );
    expect(tester.getSize(esc.first).height, greaterThanOrEqualTo(48));
    expect(tester.getSize(esc.first).width, greaterThanOrEqualTo(48));
    await _close(tester, model);
  });

  testWidgets('Ctrl 锁存作用于下一个键：软键盘打的字和栏上的键', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    await _ready(tester, backend);
    final terminal = _view(tester).terminal;

    await tester.tap(find.text('Ctrl'));
    await tester.pump();
    // 软键盘打的字经 xterm 的 textInput 出来。
    terminal.textInput('c');
    await tester.pump();
    expect(backend.sentInput.last, '\x03');
    // 只作用一次。
    terminal.textInput('c');
    expect(backend.sentInput.last, 'c');

    await tester.tap(find.text('Ctrl'));
    await tester.pump();
    await tester.tap(find.byTooltip('Left'));
    await tester.pump();
    expect(backend.sentInput.last, '\x1b[1;5D');

    // 按两下等于取消。
    await tester.tap(find.text('Ctrl'));
    await tester.tap(find.text('Ctrl'));
    await tester.pump();
    terminal.textInput('a');
    expect(backend.sentInput.last, 'a');

    await _close(tester, model);
  });

  testWidgets('Shift+Tab 跟随对端的 kitty keyboard 模式切换编码', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    await _ready(tester, backend);

    // 打开软键盘：快捷键栏一直都在，开不开键盘都一样能按。
    await tester.tap(find.byIcon(LucideIcons.keyboard));
    await tester.pump();
    await tester.pump();

    // 对端还没开 kitty：走传统 backtab。
    await tester.tap(find.byTooltip('Shift+Tab'));
    expect(backend.sentInput.last, '\x1b[Z');

    // Claude Code v2.1 起启动时会发这条开启 kitty keyboard protocol。
    backend.current.data('\x1b[>1u');
    await tester.pump();

    // 同一个按钮，编码必须跟着变——否则应用收不到（kitty 下 legacy 被抑制）。
    await tester.tap(find.byTooltip('Shift+Tab'));
    expect(backend.sentInput.last, '\x1b[9;2u');

    await tester.tap(find.byTooltip('Shift+Enter (new line)'));
    expect(backend.sentInput.last, '\x1b[13;2u');

    // ^C 也走同一条路。kitty 开着也不能变成 CSI u——readline 读的是 0x03。
    await tester.tap(find.text('^C'));
    expect(backend.sentInput.last, '\x03');

    expect(tester.takeException(), isNull);
    await _close(tester, model);
  });

  testWidgets('xterm 视图本身能往回滚', (tester) async {
    final terminal = Terminal(maxLines: 5000);
    final scrollController = ScrollController();
    addTearDown(scrollController.dispose);

    await tester.pumpWidget(
      MaterialApp(
        home: SizedBox(
          width: 320,
          height: 160,
          child: TerminalView(
            terminal,
            scrollController: scrollController,
            hardwareKeyboardOnly: true,
          ),
        ),
      ),
    );
    terminal.write(List.generate(80, (index) => 'history-$index\r\n').join());
    await tester.pump();
    await tester.pump();

    expect(scrollController.position.maxScrollExtent, greaterThan(0));
    scrollController.jumpTo(scrollController.position.maxScrollExtent);
    final latestOffset = scrollController.offset;

    await tester.drag(find.byType(TerminalView), const Offset(0, 120));
    await tester.pumpAndSettle();
    expect(scrollController.offset, lessThan(latestOffset));
  });

  testWidgets('回放完落在最新输出上，且还能往回滚', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    backend.current
      ..ready(cols: 40, rows: 20)
      ..data(
        '\x1b[?1049l\x1b[H\x1b[2J\x1b[3J'
        '${List.generate(100, (index) => 'replay-$index\r\n').join()}'
        'LATEST',
      )
      ..replayComplete();
    await tester.pump();
    await tester.pump();
    await tester.pump();

    final view = _view(tester);
    expect(view.terminal.buffer.getText(), contains('LATEST'));
    final scrollController = view.scrollController!;
    expect(scrollController.position.maxScrollExtent, greaterThan(0));
    expect(
      scrollController.offset,
      closeTo(scrollController.position.maxScrollExtent, 0.5),
    );

    final latestOffset = scrollController.offset;
    await tester.drag(find.byType(TerminalView), const Offset(0, 120));
    await tester.pumpAndSettle();
    expect(scrollController.offset, lessThan(latestOffset));
    // 离开了最新输出：给一个回去的出口。
    expect(find.text('Latest'), findsOneWidget);

    await _close(tester, model);
  });

  testWidgets('按附着时的几何回放，回放结束不再改行列', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    // 快捷键栏从第一帧就在，行数在 attach 之前就落定。
    expect(find.byType(TerminalShortcutBar), findsOneWidget);
    final mobileGeometry = backend.current.geometry;

    backend.current
      ..ready(cols: mobileGeometry.cols, rows: mobileGeometry.rows)
      ..data(
        '\x1b[?1049l\x1b[H\x1b[2J\x1b[3J\x1b[?7l'
        '${List.generate(90, (index) => 'desktop-$index\x1b[K\r\n').join()}'
        'LATEST',
      );
    // 内核事件经流转发是异步的：第一次 pump 投递事件，第二次画出来。
    await tester.pump();
    await tester.pump();

    var view = _view(tester);
    expect(view.autoResize, isFalse);
    expect(
      (view.terminal.viewWidth, view.terminal.viewHeight),
      (mobileGeometry.cols, mobileGeometry.rows),
    );
    expect(view.terminal.buffer.getText(), contains('LATEST'));

    backend.current.replayComplete();
    await tester.pump();
    await tester.pump();
    await tester.pump(TerminalStream.defaultResizeDebounce * 2);

    view = _view(tester);
    expect(view.autoResize, isTrue);
    // 行列在 attach 之前就落定了，回放结束不能再改一次。改一次就是一次 PTY
    // resize：不切备用屏的 CLI 收到 SIGWINCH 会把整段对话重印一遍。
    expect(view.terminal.viewWidth, mobileGeometry.cols);
    expect(view.terminal.viewHeight, mobileGeometry.rows);
    expect(backend.resizes, isEmpty);
    expect(view.scrollController!.position.maxScrollExtent, greaterThan(0));

    // 一个多字节字被拆在两个包里：解码器得接得上。
    final live = utf8.encode('\r\nLIVE-\u7aef');
    backend.current
      ..bytes(live.sublist(0, live.length - 1))
      ..bytes(live.sublist(live.length - 1));
    await tester.pump();
    expect(view.terminal.buffer.getText(), contains('LIVE-\u7aef'));

    await _close(tester, model);
  });

  testWidgets('开关软键盘不改 PTY 几何，实时输出照样跟底', (tester) async {
    addTearDown(() => tester.view.viewInsets = FakeViewPadding.zero);
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    backend.current
      ..ready(cols: 40, rows: 20)
      ..data(
        '${List.generate(100, (index) => 'history-$index\r\n').join()}'
        'LATEST',
      )
      ..replayComplete();
    await tester.pump();
    await tester.pump();
    await tester.pump();

    final stateBefore = tester.state<TerminalViewState>(
      find.byType(TerminalView),
    );
    final scrollController = _view(tester).scrollController!;
    backend.resizes.clear();
    final rowsBefore = _view(tester).terminal.viewHeight;

    await tester.tap(find.byIcon(LucideIcons.keyboard));
    await tester.pump();
    await tester.pump();

    expect(_view(tester).autoResize, isFalse);
    expect(find.byType(TerminalShortcutBar), findsOneWidget);
    expect(
      scrollController.offset,
      closeTo(scrollController.position.maxScrollExtent, 0.5),
    );

    tester.view.viewInsets = const FakeViewPadding(bottom: 300);
    await tester.pump();
    await tester.pump();
    backend.current.data(
      List.generate(30, (index) => 'keyboard-live-$index\r\n').join(),
    );
    await tester.pump();
    await tester.pump();
    expect(
      scrollController.offset,
      closeTo(scrollController.position.maxScrollExtent, 0.5),
      reason: '键盘开着时实时输出也要一直跟到底',
    );

    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: '  da',
        selection: TextSelection.collapsed(offset: 4),
        composing: TextRange(start: 2, end: 4),
      ),
    );
    await tester.pump();

    await tester.tap(find.byIcon(LucideIcons.keyboardOff));
    await tester.pump();
    tester.view.viewInsets = FakeViewPadding.zero;
    await tester.pump();
    await tester.pump();
    await tester.pump(TerminalStream.defaultResizeDebounce * 2);

    final stateAfter = tester.state<TerminalViewState>(
      find.byType(TerminalView),
    );
    expect(_view(tester).autoResize, isTrue);
    expect(stateAfter, isNot(same(stateBefore)));
    // 关掉软键盘不收起快捷键栏：^C / Esc 这类打断键在浏览态同样要能按。
    expect(find.byType(TerminalShortcutBar), findsOneWidget);
    expect(_view(tester).terminal.viewHeight, rowsBefore);
    expect(backend.resizes, isEmpty, reason: '键盘一开一关不该变成 PTY resize');

    backend.current.data(
      List.generate(30, (index) => 'post-keyboard-live-$index\r\n').join(),
    );
    await tester.pump();
    await tester.pump();
    expect(
      scrollController.offset,
      closeTo(scrollController.position.maxScrollExtent, 0.5),
      reason: '键盘收起后实时输出也要一直跟到底',
    );

    await _close(tester, model);
  });

  testWidgets('回放期间视口变矮，回放完全屏 TUI 仍然正好铺满视口', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    // 附着时的几何：那时还没有说明条。
    final attached = backend.current.geometry;

    backend.current
      ..ready(cols: attached.cols, rows: attached.rows)
      // 全屏 TUI（alt buffer）：没有回滚，行数恒等于视口行数。
      ..data(
        '\x1b[?1049h\x1b[H\x1b[2J'
        '${List.generate(attached.rows, (i) => 'tui-$i\r\n').join()}',
      )
      // 回放中途弹出说明条（比如一次输入没发出去）：终端视口当场变矮，而终端还锁着尺寸。
      ..error('终端输入发送失败');
    await tester.pump();
    await tester.pump();
    expect(find.text('终端输入发送失败'), findsOneWidget);

    backend.current.replayComplete();
    await tester.pump();
    await tester.pump();
    await tester.pump(TerminalStream.defaultResizeDebounce * 2);

    final view = _view(tester);
    expect(view.autoResize, isTrue);
    // alt buffer 高过视口就会长出滚动量，手势被外层 Scrollable 吃掉，TUI 内部再也
    // 滚不动。
    expect(view.scrollController!.position.maxScrollExtent, 0);
    expect(view.terminal.viewHeight, lessThan(attached.rows));
    expect(backend.resizes.last.rows, view.terminal.viewHeight);

    // 视口没有滚动量，拖拽才会落到外层的 InfiniteScrollView，被翻译成方向键交给
    // TUI——这就是「在 TUI 内部滚动」。
    backend.sentInput.clear();
    await tester.drag(find.byType(TerminalView), const Offset(0, -120));
    await tester.pumpAndSettle();
    expect(backend.sentInput, isNotEmpty);
    expect(backend.sentInput.every((data) => data == '\x1b[B'), isTrue);

    expect(tester.takeException(), isNull);
    await _close(tester, model);
  });

  testWidgets('清掉回滚的重绘在下一次重排后不复活', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    final mobileGeometry = backend.current.geometry;

    // 按比视口窄一列的网格回放：回放后的 resize 是一次改宽，会触发重排。
    backend.current.ready(
      cols: mobileGeometry.cols - 1,
      rows: mobileGeometry.rows,
    );
    await tester.pump();

    // 历史，然后一次清掉回滚的 TUI 全屏重绘，然后用户该看到的内容。
    backend.current
      ..data(List.generate(600, (index) => 'OLD-$index\r\n').join())
      ..data('\x1b[H\x1b[2J\x1b[3J')
      ..data(List.generate(80, (index) => 'NEW-$index\r\n').join())
      ..replayComplete();
    await tester.pump();
    await tester.pump();

    final view = _view(tester);
    expect(view.autoResize, isTrue);
    expect(view.terminal.viewWidth, mobileGeometry.cols);

    final text = view.terminal.buffer.getText();
    expect(text, contains('NEW-79'));
    expect(text, isNot(contains('OLD-')), reason: '被清掉的回滚不能在重排后回来');
    expect(view.scrollController!.position.maxScrollExtent, greaterThan(0));

    await _close(tester, model);
  });

  /// 首屏只带最新的一段；滚到顶就该去补更老的，并且不能把用户弹回底部。
  testWidgets('滚到顶触发补加载历史，补完后停在原来的内容上', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    backend.current.ready(
      cols: 40,
      rows: 20,
      scrollbackLines: 120,
      historyLines: 4000,
    );
    await tester.pump();
    backend.current
      ..data('${List.generate(200, (i) => 'line-$i').join('\r\n')}\r\n')
      ..replayComplete();
    await tester.pumpAndSettle();

    final controller = _view(tester).scrollController!;
    final extentBefore = controller.position.maxScrollExtent;
    expect(extentBefore, greaterThan(0));
    expect(backend.attaches, hasLength(1), reason: '停在底部时不该去拉历史');

    // 程序性地把偏移送到顶部不算数：新建的 ScrollPosition、视口变化后的夹取都会这么
    // 走一遭，那不是用户要看更老的内容。
    controller.jumpTo(0);
    await tester.pump();
    expect(backend.attaches, hasLength(1), reason: '没人碰屏幕时把偏移送到顶部，不该触发整帧重取');

    controller.jumpTo(controller.position.maxScrollExtent);
    await tester.pumpAndSettle();
    for (var i = 0; i < 60 && backend.attaches.length == 1; i++) {
      await tester.drag(find.byType(TerminalView), const Offset(0, 600));
      // 进度条是无限动画，触发补加载之后 pumpAndSettle 不会停。
      await tester.pump(const Duration(milliseconds: 16));
      await tester.pump(const Duration(milliseconds: 16));
    }
    expect(backend.attaches, hasLength(2));
    expect(backend.current.maxScrollbackLines, kMaxTerminalScrollbackLines);
    expect(find.byType(LinearProgressIndicator), findsOneWidget);

    // 补加载回来的是一份更长的快照：历史往上长，底部不动。
    backend.current.ready(
      cols: 40,
      rows: 20,
      scrollbackLines: kMaxTerminalScrollbackLines,
      historyLines: 4000,
    );
    await tester.pump();
    backend.current
      ..data('${List.generate(600, (i) => 'line-$i').join('\r\n')}\r\n')
      ..replayComplete();
    await tester.pumpAndSettle();

    final restored = _view(tester).scrollController!;
    expect(find.byType(LinearProgressIndicator), findsNothing);
    expect(
      restored.position.pixels,
      greaterThan(0),
      reason: '补完历史必须停在用户原来看的内容上，不能弹到底',
    );
    expect(
      restored.position.maxScrollExtent,
      greaterThan(extentBefore),
      reason: '补回来的历史要变成新的可滚动量',
    );
    expect(
      restored.position.maxScrollExtent - restored.position.pixels,
      closeTo(extentBefore, 1),
      reason: '离底距离要跟触发时一致（触发时 pixels=0，距底就是当时的 maxScrollExtent）',
    );

    // 停在几千行之外时，回最新输出不能只剩「一路手动拖」这一条路。
    expect(find.text('Latest'), findsOneWidget);
    await tester.tap(find.text('Latest'));
    await tester.pumpAndSettle();
    expect(
      restored.position.pixels,
      closeTo(restored.position.maxScrollExtent, 0.5),
    );
    expect(find.text('Latest'), findsNothing);

    await _close(tester, model);
  });

  testWidgets('复制输出把终端文字放进剪贴板', (tester) async {
    String? copied;
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (call) async {
        if (call.method == 'Clipboard.setData') {
          copied = (call.arguments as Map)['text'] as String?;
        }
        return null;
      },
    );
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        SystemChannels.platform,
        null,
      ),
    );

    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    await _ready(tester, backend);
    backend.current.data('hello-from-cli\r\n');
    await tester.pump();

    await tester.tap(find.byTooltip('Copy output'));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 500));

    expect(copied, 'hello-from-cli');
    expect(find.text('Copied'), findsOneWidget);

    await _close(tester, model);
  });

  testWidgets('点链接打开它，不弹键盘', (tester) async {
    final opened = _captureLaunches(tester);
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    await _ready(tester, backend);
    backend.current.data('https://example.com/notes\r\nnot-a-link\r\n');
    await tester.pump();

    await _tapCell(tester, const CellOffset(0, 0));
    await tester.pump();
    expect(opened, [Uri.parse('https://example.com/notes')]);
    expect(find.byIcon(LucideIcons.keyboard), findsOneWidget);

    await _tapCell(tester, const CellOffset(0, 1));
    expect(opened, hasLength(1));

    await tester.pump(kDoubleTapTimeout);
    await _close(tester, model);
  });

  testWidgets('长按选词，给出复制 / 全选 / 打开链接', (tester) async {
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    await _ready(tester, backend);
    backend.current.data('https://example.com/notes');
    await tester.pump();

    await tester.longPressAt(await _cellGlobal(tester, const CellOffset(4, 0)));
    await tester.pump();

    expect(find.text('Copy'), findsOneWidget);
    expect(find.text('Select all'), findsOneWidget);
    expect(find.text('Open link'), findsOneWidget);
    expect(find.text('Paste'), findsOneWidget);
    expect(find.byTooltip('Copy selection'), findsOneWidget);

    await _close(tester, model);
  });

  testWidgets('长按拖动时「打开链接」跟着选区走', (tester) async {
    final opened = _captureLaunches(tester);
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    await _ready(tester, backend);
    backend.current.data('see https://example.com/notes');
    await tester.pump();

    final gesture = await tester.startGesture(
      await _cellGlobal(tester, const CellOffset(8, 0)),
    );
    await tester.pump(kLongPressTimeout + const Duration(milliseconds: 50));
    expect(find.text('Open link'), findsOneWidget);

    // 拖到前面的普通词：选区变成 `see https://...`，不再是一个链接。
    await gesture.moveTo(await _cellGlobal(tester, const CellOffset(0, 0)));
    await tester.pump();
    expect(find.text('Copy'), findsOneWidget);
    expect(find.text('Open link'), findsNothing);

    // 再拖回链接上，「打开链接」要回来，点了打开的是链接本身。
    await gesture.moveTo(await _cellGlobal(tester, const CellOffset(10, 0)));
    await tester.pump();
    await gesture.up();
    await tester.pump();
    expect(find.text('Open link'), findsOneWidget);
    await tester.tap(find.text('Open link'));
    await tester.pump();
    expect(opened, [Uri.parse('https://example.com/notes')]);
    expect(tester.takeException(), isNull);

    await _close(tester, model);
  });

  testWidgets('开着鼠标上报的 TUI 里点链接：打开链接，不算一次点击', (tester) async {
    final opened = _captureLaunches(tester);
    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    await _ready(tester, backend);
    backend.current.data('\x1b[?1000h\x1b[?1006hhttps://example.com/notes');
    await tester.pump();
    backend.sentInput.clear();

    await _tapCell(tester, const CellOffset(0, 0));
    await tester.pump();
    // 链接被认领：打开链接，TUI 收不到这次点击，一次点按只做一件事。
    expect(opened, [Uri.parse('https://example.com/notes')]);
    expect(backend.sentInput, isEmpty);

    // 不在链接上的点击照常报给 TUI。xterm 抬手后挂着双击判定计时器，等它过期。
    await tester.pump(kDoubleTapTimeout);
    await _tapCell(tester, const CellOffset(0, 2));
    expect(backend.sentInput, isNotEmpty);
    expect(opened, hasLength(1));
    await tester.pump(kDoubleTapTimeout);

    await _close(tester, model);
  });

  testWidgets('粘贴键把剪贴板发给 PTY，跟随 bracketed paste', (tester) async {
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (call) async {
        if (call.method == 'Clipboard.getData') {
          return {'text': 'hello\nworld'};
        }
        return null;
      },
    );
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        SystemChannels.platform,
        null,
      ),
    );

    final backend = _ScriptedBackend();
    final model = await _open(tester, backend);
    await _ready(tester, backend);

    await tester.tap(find.byTooltip('Paste'));
    await tester.pump();
    expect(backend.sentInput, ['hello\rworld']);

    backend.sentInput.clear();
    backend.current.data('\x1b[?2004h');
    await tester.pump();
    await tester.tap(find.byTooltip('Paste'));
    await tester.pump();
    expect(backend.sentInput, ['\x1b[200~hello\nworld\x1b[201~']);

    await _close(tester, model);
  });
}
