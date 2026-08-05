import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/utils/reflow_safe_terminal.dart';
import 'package:xterm/xterm.dart';

/// Drives a terminal the way a mobile attach does: a large history, a TUI full
/// repaint that clears the scrollback, fresh content, then the viewport-driven
/// resize that reflows the buffer.
void replayThenReflow(Terminal terminal) {
  terminal.resize(49, 47);
  for (var i = 0; i < 1500; i++) {
    terminal.write('OLD $i\r\n');
  }
  terminal.write('\x1b[H\x1b[2J\x1b[3J');
  for (var i = 0; i < 100; i++) {
    terminal.write('NEW $i\r\n');
  }
  terminal.resize(50, 47);
}

List<String> visibleLines(Terminal terminal) => List.generate(
  terminal.buffer.lines.length,
  (i) => terminal.buffer.lines[i].toString().trim(),
);

void main() {
  test('a reflow after a cleared scrollback keeps the current content', () {
    final terminal = ReflowSafeTerminal(maxLines: 5000);
    replayThenReflow(terminal);

    final lines = visibleLines(terminal);
    expect(
      lines.where((line) => line.startsWith('OLD')),
      isEmpty,
      reason: 'lines dropped by the scrollback clear must stay dropped',
    );
    expect(lines.first, 'NEW 0');
    expect(lines.where((line) => line.startsWith('NEW')), hasLength(100));
  });

  test('the plain xterm terminal still shows the bug this class works around', () {
    // Guards the workaround against silently becoming dead code: once xterm
    // fixes `IndexAwareCircularBuffer.replaceWith`, this test fails and
    // `ReflowSafeTerminal` can be deleted.
    final terminal = Terminal(maxLines: 5000);
    replayThenReflow(terminal);

    expect(
      visibleLines(terminal).where((line) => line.startsWith('OLD')),
      isNotEmpty,
    );
  });

  test('normalising the rotation preserves line order and count', () {
    final terminal = ReflowSafeTerminal(maxLines: 200);
    terminal.resize(20, 5);
    for (var i = 0; i < 500; i++) {
      terminal.write('row $i\r\n');
    }
    final before = visibleLines(terminal);

    ReflowSafeTerminal.normalizeScrollbackRotation(terminal.buffer);

    expect(visibleLines(terminal), before);
    expect(terminal.buffer.lines.maxLength, 200);
  });
}
