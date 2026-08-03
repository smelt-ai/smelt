import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/pages/terminal_session_page.dart';

void main() {
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
