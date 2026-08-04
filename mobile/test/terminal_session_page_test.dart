import 'dart:async';
import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/pages/terminal_session_page.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/services/terminal_stream_service.dart';
import 'package:xterm/xterm.dart';

class _FakeTerminalStream implements TerminalStreamClient {
  final _events = StreamController<TerminalStreamEvent>.broadcast(sync: true);
  final _states = StreamController<TerminalStreamState>.broadcast(sync: true);

  TerminalStreamState _state = TerminalStreamState.waitingForGateway;

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
  void forceGeometry() {}

  @override
  void resume() {}

  @override
  void sendInput(String data) {}

  @override
  void start(TerminalGeometry geometry) {}

  @override
  void suspend() {}

  @override
  void updateGeometry(TerminalGeometry geometry) {}

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

  testWidgets('terminal shortcut bar emits PTY control sequences', (
    tester,
  ) async {
    final inputs = <String>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(body: TerminalShortcutBar(onInput: inputs.add)),
      ),
    );

    await tester.tap(find.text('Esc'));
    await tester.tap(find.text('^C'));
    await tester.tap(find.byIcon(Icons.keyboard_arrow_up));

    expect(inputs, ['\x1b', '\x03', '\x1b[A']);
    expect(tester.takeException(), isNull);
  });
}
