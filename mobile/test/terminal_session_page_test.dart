import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/pages/terminal_session_page.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:xterm/xterm.dart';

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
    expect(terminalView.focusNode?.hasFocus, isFalse);
    expect(find.byIcon(Icons.keyboard_outlined), findsOneWidget);
    expect(find.byType(TerminalShortcutBar), findsNothing);

    await tester.pumpWidget(const SizedBox.shrink());
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
