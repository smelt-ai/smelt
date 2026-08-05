import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/utils/safe_terminal.dart';
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

int detachedLines(Terminal terminal) {
  var detached = 0;
  final lines = terminal.buffer.lines;
  for (var i = 0; i < lines.length; i++) {
    if (!lines[i].attached) detached++;
  }
  return detached;
}

/// Drives a terminal the way a TUI repaint after a mobile attach does: a scroll
/// region with a non-zero top margin scrolls (which detaches lines that stay in
/// the ring), then a region anchored at the top scrolls (which shifts through
/// `lines.insert` and trips over them).
({int detachedAfterScrollUp, Object? crash}) scrollThroughBothMarginPaths(
  Terminal terminal,
) {
  terminal.resize(49, 47);
  for (var i = 0; i < 200; i++) {
    terminal.write('line $i\r\n');
  }
  terminal.write('\x1b[5;40r\x1b[40;1H');
  for (var i = 0; i < 10; i++) {
    terminal.write('scrolled $i\r\n');
  }
  final detached = detachedLines(terminal);

  terminal.write('\x1b[1;11r\x1b[11;1H');
  Object? crash;
  try {
    for (var i = 0; i < 10; i++) {
      terminal.write('after $i\r\n');
    }
  } catch (error) {
    crash = error;
  }
  return (detachedAfterScrollUp: detached, crash: crash);
}

void main() {
  test('a reflow after a cleared scrollback keeps the current content', () {
    final terminal = SafeTerminal(maxLines: 5000);
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
    // `SafeTerminal` can be deleted.
    final terminal = Terminal(maxLines: 5000);
    replayThenReflow(terminal);

    expect(
      visibleLines(terminal).where((line) => line.startsWith('OLD')),
      isNotEmpty,
    );
  });

  test('a scroll region does not poison later line feeds', () {
    final terminal = SafeTerminal(maxLines: 5000);
    final result = scrollThroughBothMarginPaths(terminal);

    expect(
      result.crash,
      isNull,
      reason: 'writing must not throw out of the terminal',
    );
    expect(result.detachedAfterScrollUp, 0);
    expect(detachedLines(terminal), 0);
    expect(visibleLines(terminal), contains('after 9'));
  });

  test('the plain xterm terminal still detaches lines it keeps', () {
    // Second guard test: xterm's `Buffer.scrollUp` detaches the line it has
    // just shifted down, and the next `lines.insert` throws on it. Delete the
    // workaround once this fails.
    final terminal = Terminal(maxLines: 5000);
    final result = scrollThroughBothMarginPaths(terminal);

    expect(result.detachedAfterScrollUp, greaterThan(0));
    expect(result.crash, isNotNull);
  });

  test('normalising the rotation preserves line order and count', () {
    final terminal = SafeTerminal(maxLines: 200);
    terminal.resize(20, 5);
    for (var i = 0; i < 500; i++) {
      terminal.write('row $i\r\n');
    }
    final before = visibleLines(terminal);

    SafeTerminal.normalizeScrollbackRotation(terminal.buffer);

    expect(visibleLines(terminal), before);
    expect(terminal.buffer.lines.maxLength, 200);
  });
}
