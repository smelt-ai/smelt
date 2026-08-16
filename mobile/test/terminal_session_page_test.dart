import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/pages/terminal_session_page.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/services/terminal_stream_service.dart';
import 'package:smelt_mobile/theme/terminal_theme_wire.dart';
import 'package:xterm/xterm.dart';

class _FakeTerminalStream implements TerminalStreamClient {
  final _events = StreamController<TerminalStreamEvent>.broadcast(sync: true);
  final _states = StreamController<TerminalStreamState>.broadcast(sync: true);

  TerminalStreamState _state = TerminalStreamState.waitingForGateway;
  final geometries = <TerminalGeometry>[];

  void emit(TerminalStreamEvent event) => _events.add(event);

  void connect() {
    _state = TerminalStreamState.connected;
    _states.add(_state);
  }

  @override
  Stream<TerminalStreamEvent> get events => _events.stream;

  @override
  Stream<TerminalStreamState> get stateStream => _states.stream;

  @override
  TerminalStreamState get state => _state;

  @override
  bool get writeEnabled => true;

  @override
  void resume() {}

  final sentInput = <String>[];

  @override
  void sendInput(String data) => sentInput.add(data);

  @override
  void start(TerminalGeometry geometry) {}

  @override
  void suspend() {}

  @override
  void updateGeometry(TerminalGeometry geometry) => geometries.add(geometry);

  @override
  Future<void> dispose() async {
    await _events.close();
    await _states.close();
  }
}

void main() {
  testWidgets('terminal page starts in browse mode', (tester) async {
    const session = SessionSummary(
      id: 'terminal-1',
      kind: SessionKind.terminal,
      title: 'Shell',
      phase: 'running',
      agent: 'terminal',
    );

    await tester.pumpWidget(
      const MaterialApp(home: TerminalSessionPage(session: session)),
    );

    final terminalView = tester.widget<TerminalView>(find.byType(TerminalView));
    expect(terminalView.autofocus, isFalse);
    expect(terminalView.hardwareKeyboardOnly, isTrue);
    expect(terminalView.onTapUp, isNotNull);
    expect(terminalView.scrollController, isNotNull);
    expect(terminalView.focusNode?.hasFocus, isFalse);
    expect(find.byIcon(Icons.keyboard_outlined), findsOneWidget);
    expect(find.byType(TerminalShortcutBar), findsNothing);

    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets(
    'terminal history can scroll back after landing on latest output',
    (tester) async {
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
    },
  );

  testWidgets('terminal page lands on the live tail after replay', (
    tester,
  ) async {
    final stream = _FakeTerminalStream();
    const session = SessionSummary(
      id: 'terminal-1',
      kind: SessionKind.terminal,
      title: 'Shell',
      phase: 'running',
      agent: 'terminal',
    );
    await tester.pumpWidget(
      MaterialApp(
        home: TerminalSessionPage(session: session, stream: stream),
      ),
    );
    stream.connect();
    final replay = Uint8List.fromList(
      utf8.encode(
        '\x1b[?1049l\x1b[H\x1b[2J\x1b[3J'
        '${List.generate(100, (index) => 'replay-$index\r\n').join()}'
        'LATEST',
      ),
    );
    stream.emit(
      TerminalReadyEvent(
        cols: 40,
        rows: 20,
        replayBytes: replay.length,
        writeEnabled: true,
      ),
    );
    stream.emit(TerminalDataEvent(replay));
    stream.emit(const TerminalReplayCompleteEvent());
    await tester.pump();
    await tester.pump();
    await tester.pump();

    final view = tester.widget<TerminalView>(find.byType(TerminalView));
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
  });

  testWidgets('snapshot replays at the negotiated mobile geometry', (
    tester,
  ) async {
      final stream = _FakeTerminalStream();
      const session = SessionSummary(
        id: 'terminal-1',
        kind: SessionKind.terminal,
        title: 'Shell',
        phase: 'running',
        agent: 'terminal',
      );
      await tester.pumpWidget(
        MaterialApp(
          home: TerminalSessionPage(session: session, stream: stream),
        ),
      );
      expect(stream.geometries, isNotEmpty);
      final mobileGeometry = stream.geometries.last;

      stream.connect();
      final replay = Uint8List.fromList(
        utf8.encode(
          '\x1b[?1049l\x1b[H\x1b[2J\x1b[3J\x1b[?7l'
          '${List.generate(90, (index) => 'desktop-$index\x1b[K\r\n').join()}'
          'LATEST',
        ),
      );
      stream.emit(
        TerminalReadyEvent(
          cols: mobileGeometry.cols,
          rows: mobileGeometry.rows,
          replayBytes: replay.length,
          writeEnabled: true,
        ),
      );
      stream.emit(TerminalDataEvent(replay));
      await tester.pump();

      var view = tester.widget<TerminalView>(find.byType(TerminalView));
      expect(view.autoResize, isFalse);
      expect(
        (view.terminal.viewWidth, view.terminal.viewHeight),
        (mobileGeometry.cols, mobileGeometry.rows),
      );
      expect(view.terminal.buffer.getText(), contains('LATEST'));

      stream.emit(const TerminalReplayCompleteEvent());
      await tester.pump();
      await tester.pump();

      view = tester.widget<TerminalView>(find.byType(TerminalView));
      expect(view.autoResize, isTrue);
      expect(
        (view.terminal.viewWidth, view.terminal.viewHeight),
        (mobileGeometry.cols, mobileGeometry.rows),
      );
      expect(view.terminal.buffer.getText(), contains('LATEST'));
      expect(view.scrollController!.position.maxScrollExtent, greaterThan(0));

      final live = utf8.encode('\r\nLIVE-\u7aef');
      stream.emit(
        TerminalDataEvent(Uint8List.fromList(live.sublist(0, live.length - 1))),
      );
      stream.emit(
        TerminalDataEvent(Uint8List.fromList(live.sublist(live.length - 1))),
      );
      await tester.pump();
      expect(view.terminal.buffer.getText(), contains('LIVE-\u7aef'));
    },
  );

  testWidgets(
    'software keyboard preserves PTY geometry and terminal scrolling',
    (tester) async {
      addTearDown(() => tester.view.viewInsets = FakeViewPadding.zero);
      final stream = _FakeTerminalStream();
      const session = SessionSummary(
        id: 'terminal-1',
        kind: SessionKind.terminal,
        title: 'Shell',
        phase: 'running',
        agent: 'terminal',
      );
      await tester.pumpWidget(
        MaterialApp(
          home: TerminalSessionPage(session: session, stream: stream),
        ),
      );
      stream.connect();
      final replay = Uint8List.fromList(
        utf8.encode(
          '${List.generate(100, (index) => 'history-$index\r\n').join()}'
          'LATEST',
        ),
      );
      stream.emit(
        TerminalReadyEvent(
          cols: 40,
          rows: 20,
          replayBytes: replay.length,
          writeEnabled: true,
        ),
      );
      stream.emit(TerminalDataEvent(replay));
      stream.emit(const TerminalReplayCompleteEvent());
      await tester.pump();
      await tester.pump();
      await tester.pump();

      final viewBefore = tester.widget<TerminalView>(find.byType(TerminalView));
      final stateBefore = tester.state<TerminalViewState>(
        find.byType(TerminalView),
      );
      final scrollController = viewBefore.scrollController!;
      expect(stream.geometries, isNotEmpty);
      final initialGeometry = stream.geometries.last;
      stream.geometries.clear();

      await tester.tap(find.byIcon(Icons.keyboard_outlined));
      await tester.pump();
      await tester.pump();

      final keyboardView = tester.widget<TerminalView>(
        find.byType(TerminalView),
      );
      expect(keyboardView.autoResize, isFalse);
      expect(find.byType(TerminalShortcutBar), findsOneWidget);
      expect(
        scrollController.offset,
        closeTo(scrollController.position.maxScrollExtent, 0.5),
      );

      tester.view.viewInsets = const FakeViewPadding(bottom: 300);
      await tester.pump();
      await tester.pump();
      stream.emit(
        TerminalDataEvent(
          Uint8List.fromList(
            utf8.encode(
              List.generate(30, (index) => 'keyboard-live-$index\r\n').join(),
            ),
          ),
        ),
      );
      await tester.pump();
      await tester.pump();
      expect(
        scrollController.offset,
        closeTo(scrollController.position.maxScrollExtent, 0.5),
        reason: 'live output must keep advancing while the keyboard is open',
      );

      tester.testTextInput.updateEditingValue(
        const TextEditingValue(
          text: '  da',
          selection: TextSelection.collapsed(offset: 4),
          composing: TextRange(start: 2, end: 4),
        ),
      );
      await tester.pump();

      await tester.tap(find.byIcon(Icons.keyboard_hide_outlined));
      await tester.pump();
      tester.view.viewInsets = FakeViewPadding.zero;
      await tester.pump();
      await tester.pump();

      final viewAfter = tester.widget<TerminalView>(find.byType(TerminalView));
      final stateAfter = tester.state<TerminalViewState>(
        find.byType(TerminalView),
      );
      expect(viewAfter.autoResize, isTrue);
      expect(stateAfter, isNot(same(stateBefore)));
      expect(find.byType(TerminalShortcutBar), findsNothing);
      expect(
        stream.geometries.every(
          (geometry) =>
              geometry.cols == initialGeometry.cols &&
              geometry.rows == initialGeometry.rows,
        ),
        isTrue,
      );
      expect(
        scrollController.offset,
        closeTo(scrollController.position.maxScrollExtent, 0.5),
      );

      stream.emit(
        TerminalDataEvent(
          Uint8List.fromList(
            utf8.encode(
              List.generate(
                30,
                (index) => 'post-keyboard-live-$index\r\n',
              ).join(),
            ),
          ),
        ),
      );
      await tester.pump();
      await tester.pump();
      expect(
        scrollController.offset,
        closeTo(scrollController.position.maxScrollExtent, 0.5),
        reason: 'live output must keep advancing after the keyboard closes',
      );

      final latestOffset = scrollController.offset;
      await tester.drag(find.byType(TerminalView), const Offset(0, 120));
      await tester.pumpAndSettle();
      expect(scrollController.offset, lessThan(latestOffset));
    },
  );

  testWidgets('a repaint that clears the scrollback survives the next reflow', (
    tester,
  ) async {
    final stream = _FakeTerminalStream();
    const session = SessionSummary(
      id: 'terminal-1',
      kind: SessionKind.terminal,
      title: 'Shell',
      phase: 'running',
      agent: 'terminal',
    );
    await tester.pumpWidget(
      MaterialApp(home: TerminalSessionPage(session: session, stream: stream)),
    );
    expect(stream.geometries, isNotEmpty);
    final mobileGeometry = stream.geometries.last;

    stream.connect();
    // Replaying at a narrower grid than the viewport makes the resize that
    // follows the replay a width change, which is what triggers a reflow.
    stream.emit(
      TerminalReadyEvent(
        cols: mobileGeometry.cols - 1,
        rows: mobileGeometry.rows,
        replayBytes: 0,
        writeEnabled: true,
      ),
    );
    await tester.pump();

    // History, then a TUI full repaint that clears the scrollback, then the
    // content the user is supposed to end up looking at.
    stream.emit(
      TerminalDataEvent(
        Uint8List.fromList(
          utf8.encode(
            List.generate(600, (index) => 'OLD-$index\r\n').join(),
          ),
        ),
      ),
    );
    stream.emit(
      TerminalDataEvent(Uint8List.fromList(utf8.encode('\x1b[H\x1b[2J\x1b[3J'))),
    );
    stream.emit(
      TerminalDataEvent(
        Uint8List.fromList(
          utf8.encode(List.generate(80, (index) => 'NEW-$index\r\n').join()),
        ),
      ),
    );
    stream.emit(const TerminalReplayCompleteEvent());
    await tester.pump();
    await tester.pump();

    final view = tester.widget<TerminalView>(find.byType(TerminalView));
    expect(view.autoResize, isTrue);
    expect(view.terminal.viewWidth, mobileGeometry.cols);

    final text = view.terminal.buffer.getText();
    expect(text, contains('NEW-79'));
    expect(
      text,
      isNot(contains('OLD-')),
      reason: 'lines dropped by the scrollback clear must not come back',
    );
    expect(view.scrollController!.position.maxScrollExtent, greaterThan(0));

    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('terminal shortcut bar emits PTY control sequences', (
    tester,
  ) async {
    final keys = <String>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: TerminalShortcutBar(
            onKey: (key, {shift = false, alt = false, ctrl = false}) =>
                keys.add('$key shift=$shift alt=$alt ctrl=$ctrl'),
          ),
        ),
      ),
    );

    await tester.tap(find.text('Esc'));
    await tester.tap(find.text('^C'));
    await tester.tap(find.byIcon(Icons.keyboard_arrow_up));

    // 每个键都只报按了什么，编码由终端按其当前模式决定。
    expect(keys, [
      'TerminalKey.escape shift=false alt=false ctrl=false',
      'TerminalKey.keyC shift=false alt=false ctrl=true',
      'TerminalKey.arrowUp shift=false alt=false ctrl=false',
    ]);
    expect(tester.takeException(), isNull);
  });

  testWidgets('shortcut bar exposes Shift+Tab and Shift+Enter', (
    tester,
  ) async {
    final keys = <String>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: TerminalShortcutBar(
            onKey: (key, {shift = false, alt = false, ctrl = false}) =>
                keys.add('$key shift=$shift'),
          ),
        ),
      ),
    );

    // 这两个组合软键盘上按不出来，只能靠快捷键栏。
    await tester.tap(find.text('⇧Tab'));
    await tester.tap(find.text('⇧↵'));

    expect(keys, [
      'TerminalKey.tab shift=true',
      'TerminalKey.enter shift=true',
    ]);
    expect(tester.takeException(), isNull);
  });

testWidgets(
    'Shift+Tab 跟随对端的 kitty keyboard 模式切换编码',
    (tester) async {
      final stream = _FakeTerminalStream();
      const session = SessionSummary(
        id: 'terminal-kitty',
        kind: SessionKind.terminal,
        title: 'Shell',
        phase: 'running',
        agent: 'terminal',
      );
      await tester.pumpWidget(
        MaterialApp(home: TerminalSessionPage(session: session, stream: stream)),
      );
      stream.connect();
      stream.emit(
        const TerminalReadyEvent(
          cols: 40,
          rows: 20,
          replayBytes: 0,
          writeEnabled: true,
        ),
      );
      stream.emit(const TerminalReplayCompleteEvent());
      await tester.pump();
      await tester.pump();

      // 打开软键盘，快捷键栏才会出现。
      await tester.tap(find.byIcon(Icons.keyboard_outlined));
      await tester.pump();
      await tester.pump();

      // 对端还没开 kitty：走传统 backtab。
      await tester.tap(find.text('⇧Tab'));
      expect(stream.sentInput.last, '\x1b[Z');

      // Claude Code v2.1 起启动时会发这条开启 kitty keyboard protocol。
      stream.emit(TerminalDataEvent(Uint8List.fromList(utf8.encode('\x1b[>1u'))));
      await tester.pump();

      // 同一个按钮，编码必须跟着变——否则应用收不到（kitty 下 legacy 被抑制）。
      await tester.tap(find.text('⇧Tab'));
      expect(stream.sentInput.last, '\x1b[9;2u');

      await tester.tap(find.text('⇧↵'));
      expect(stream.sentInput.last, '\x1b[13;2u');

      // ^C 现在也走同一条路。kitty 开着也不能变成 CSI u——readline 读的是 0x03。
      await tester.tap(find.text('^C'));
      expect(stream.sentInput.last, '\x03');

      expect(tester.takeException(), isNull);
      await tester.pumpWidget(const SizedBox.shrink());
    },
  );

  testWidgets('terminal adopts the theme sent by the connected device', (
    tester,
  ) async {
    final stream = _FakeTerminalStream();
    const session = SessionSummary(
      id: 'terminal-1',
      kind: SessionKind.terminal,
      title: 'Shell',
      phase: 'running',
      agent: 'terminal',
    );
    await tester.pumpWidget(
      MaterialApp(home: TerminalSessionPage(session: session, stream: stream)),
    );

    // 连上之前只能用兜底深色（= PC 出厂主题）。
    expect(
      tester.widget<TerminalView>(find.byType(TerminalView)).theme.background,
      SmeltTerminalTheme.fallbackDark.background,
    );

    stream.connect();
    // PC 端切了浅色主题：手机必须跟着换。否则 TUI 按 OSC 11 查到的浅底挑灰度，
    // 手机上却渲染在深底上，直接是对比度问题。
    stream.emit(
      TerminalReadyEvent(
        cols: 40,
        rows: 20,
        replayBytes: 0,
        writeEnabled: true,
        theme: SmeltTerminalTheme.fromWire(const {
          'dark': false,
          'background': '#ffffff',
          'foreground': '#24292e',
        }),
        themeIsDark: false,
      ),
    );
    await tester.pump();
    await tester.pump();

    final view = tester.widget<TerminalView>(find.byType(TerminalView));
    expect(view.theme.background, const Color(0xffffffff));
    expect(view.theme.foreground, const Color(0xff24292e));
    expect(
      tester.widget<Scaffold>(find.byType(Scaffold)).backgroundColor,
      const Color(0xffffffff),
      reason: '浅色终端不该嵌在纯黑页面里',
    );

    await tester.pumpWidget(const SizedBox.shrink());
  });

}
