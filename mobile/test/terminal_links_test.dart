import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/utils/terminal_links.dart';
import 'package:xterm/xterm.dart';

void main() {
  test(
    'visible http(s) links are hittable and trailing punctuation is not',
    () {
      final terminal = _terminal();
      terminal.write('see https://example.com/notes. and http://a.b/x,');

      expect(
        _index(terminal).targetAt(terminal, const CellOffset(4, 0)),
        'https://example.com/notes',
      );
      expect(
        _index(terminal).targetAt(terminal, const CellOffset(0, 0)),
        isNull,
      );
      expect(
        _index(terminal).targetAt(
          terminal,
          const CellOffset('see https://example.com/notes. and '.length, 0),
        ),
        'http://a.b/x',
      );
    },
  );

  test('a link split by soft wrap is still the full url', () {
    final terminal = _terminal(cols: 10);
    terminal.write('https://example.com/a');
    final index = _index(terminal);

    expect(
      index.targetAt(terminal, const CellOffset(0, 0)),
      'https://example.com/a',
    );
    expect(
      index.targetAt(terminal, const CellOffset(0, 1)),
      'https://example.com/a',
    );
    expect(
      terminalClipboardText(terminal, null),
      contains('https://example.com/a'),
    );
  });

  test('wide characters before a url do not shift the hit', () {
    final terminal = _terminal();
    terminal.write('中文 https://example.com/a');
    final index = _index(terminal);

    expect(index.targetAt(terminal, const CellOffset(0, 0)), isNull);
    final line = terminal.buffer.lines[0];
    var col = 0;
    while (col < terminal.viewWidth &&
        line.getCodePoint(col) != 'h'.codeUnitAt(0)) {
      var span = line.getWidth(col);
      if (span <= 0) span = 1;
      col += span;
    }
    expect(col, greaterThan(2));
    expect(
      index.targetAt(terminal, CellOffset(col, 0)),
      'https://example.com/a',
    );
  });

  test('osc 8 opens the hidden uri, not the visible label', () {
    final index = TerminalLinkIndex();
    final terminal = _terminal(index: index);
    terminal.write('\x1b]8;;https://example.com/docs\x07docs\x1b]8;;\x07 tail');

    expect(
      index.targetAt(terminal, const CellOffset(0, 0)),
      'https://example.com/docs',
    );
    expect(
      index.targetAt(terminal, const CellOffset(3, 0)),
      'https://example.com/docs',
    );
    expect(index.targetAt(terminal, const CellOffset(5, 0)), isNull);
  });

  test('osc 8 that is not http(s) is not opened', () {
    final index = TerminalLinkIndex();
    final terminal = _terminal(index: index);
    terminal.write('\x1b]8;;file:///etc/hosts\x07hosts\x1b]8;;\x07');

    expect(index.targetAt(terminal, const CellOffset(0, 0)), isNull);
  });

  test('a new snapshot drops links from the previous buffer', () {
    final index = TerminalLinkIndex();
    final first = _terminal(index: index);
    first.write('\x1b]8;;https://example.com/old\x07old\x1b]8;;\x07');
    expect(index.targetAt(first, const CellOffset(0, 0)), isNotNull);

    index.clear();
    final second = _terminal(index: index);
    second.write('plain');
    expect(index.targetAt(second, const CellOffset(0, 0)), isNull);
  });

  test('links scrolled out of the scrollback are released on new output', () {
    final index = TerminalLinkIndex();
    late final Terminal terminal;
    terminal = Terminal(
      maxLines: 30,
      onPrivateOSC: (code, args) => index.onOsc(terminal, code, args),
    );
    terminal.resize(40, 5);
    terminal.write('\x1b]8;;https://example.com/old\x07old\x1b]8;;\x07\r\n');
    expect(index.trackedLinkCount, 1);

    terminal.write(List.generate(80, (i) => 'line-$i\r\n').join());
    terminal.write('\x1b]8;;https://example.com/new\x07new\x1b]8;;\x07');
    expect(index.trackedLinkCount, 1);
  });

  test('long press word boundary keeps a url intact', () {
    final terminal = Terminal(wordSeparators: cliWordSeparators);
    terminal.resize(80, 5);
    terminal.write('https://example.com/a');

    final boundary = terminal.buffer.getWordBoundary(const CellOffset(8, 0));
    expect(boundary, isNotNull);
    expect(terminal.buffer.getText(boundary), 'https://example.com/a');
  });

  test('copy without a selection trims trailing blank lines', () {
    final terminal = _terminal();
    terminal.write('hello\r\n');

    final text = terminalClipboardText(terminal, null);
    expect(text, 'hello');
    expect(text, isNot(endsWith('\n')));
  });
}

Terminal _terminal({int cols = 80, TerminalLinkIndex? index}) {
  late final Terminal terminal;
  terminal = Terminal(
    onPrivateOSC: index == null
        ? null
        : (code, args) => index.onOsc(terminal, code, args),
  );
  terminal.resize(cols, 8);
  return terminal;
}

TerminalLinkIndex _index(Terminal terminal) {
  final index = TerminalLinkIndex();
  // 已经写进缓冲的文本没有 OSC 事件可回放；可见链接不依赖它。
  return index;
}
