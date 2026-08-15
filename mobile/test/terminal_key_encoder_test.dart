import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/utils/terminal_key_encoder.dart';
import 'package:smelt_mobile/utils/xterm_input_filter.dart';

void main() {
  group('kitty keyboard protocol 探测', () {
    test('CSI > 1 u 开启后 kittyKeyboardEnabled 为真', () {
      final filter = XtermInputFilter();
      expect(filter.kittyKeyboardEnabled, isFalse);
      filter.add(utf8.encode('\x1b[>1u'));
      expect(filter.kittyKeyboardEnabled, isTrue);
    });

    test('CSI < u 出栈后恢复', () {
      final filter = XtermInputFilter();
      filter.add(utf8.encode('\x1b[>1u'));
      filter.add(utf8.encode('\x1b[<u'));
      expect(filter.kittyKeyboardEnabled, isFalse);
    });

    test('嵌套开启时退出内层不会误关外层', () {
      final filter = XtermInputFilter();
      filter.add(utf8.encode('\x1b[>1u'));
      filter.add(utf8.encode('\x1b[>1u'));
      filter.add(utf8.encode('\x1b[<u'));
      expect(filter.kittyKeyboardEnabled, isTrue);
    });

    test('flags 不含 bit1 时不算开启', () {
      final filter = XtermInputFilter();
      filter.add(utf8.encode('\x1b[>4u'));
      expect(filter.kittyKeyboardEnabled, isFalse);
    });

    test('序列被拆成多个 chunk 也能正确识别', () {
      final filter = XtermInputFilter();
      filter.add(utf8.encode('\x1b[>'));
      filter.add(utf8.encode('1'));
      filter.add(utf8.encode('u'));
      expect(filter.kittyKeyboardEnabled, isTrue);
    });

    test('探测不吞掉正常输出', () {
      final filter = XtermInputFilter();
      final out = filter.add(utf8.encode('hi\x1b[>1uthere'));
      expect(utf8.decode(out), contains('hi'));
      expect(utf8.decode(out), contains('there'));
    });

    test('CSI = flags ; 3 u 只清不设', () {
      final filter = XtermInputFilter();
      filter.add(utf8.encode('\x1b[>1u'));
      filter.add(utf8.encode('\x1b[=1;3u'));
      expect(filter.kittyKeyboardEnabled, isFalse);
    });
  });

  group('按键编码', () {
    test('Shift+Tab：kitty 关时是 backtab', () {
      expect(
        encodeTermKey(TermKey.tab, shift: true),
        '\x1b[Z',
      );
    });

    test('Shift+Tab：kitty 开时必须走 CSI u，否则 Claude Code 收不到', () {
      expect(
        encodeTermKey(TermKey.tab, shift: true, kitty: true),
        '\x1b[9;2u',
      );
    });

    test('Shift+Enter：kitty 开时 CSI u，关时退回 LF', () {
      expect(
        encodeTermKey(TermKey.enter, shift: true, kitty: true),
        '\x1b[13;2u',
      );
      expect(encodeTermKey(TermKey.enter, shift: true), '\n');
      expect(encodeTermKey(TermKey.enter), '\r');
    });

    test('方向键跟随 application cursor 模式切 SS3', () {
      expect(encodeTermKey(TermKey.up), '\x1b[A');
      expect(encodeTermKey(TermKey.up, appCursor: true), '\x1bOA');
      expect(encodeTermKey(TermKey.left, appCursor: true), '\x1bOD');
    });

    test('裸键与原有 bar 的字面量保持一致，不改变既有行为', () {
      expect(encodeTermKey(TermKey.escape), '\x1b');
      expect(encodeTermKey(TermKey.tab), '\t');
      expect(encodeTermKey(TermKey.pageUp), '\x1b[5~');
      expect(encodeTermKey(TermKey.pageDown), '\x1b[6~');
    });

    test('带修饰的方向键走 xterm 修饰序列', () {
      expect(encodeTermKey(TermKey.right, ctrl: true), '\x1b[1;5C');
      expect(encodeTermKey(TermKey.right, ctrl: true, kitty: true),
          '\x1b[4;5u');
    });

    test('Ctrl+字母映射到 C0', () {
      expect(encodeCtrlLetter('c'), '\x03');
      expect(encodeCtrlLetter('C'), '\x03');
      expect(encodeCtrlLetter('r'), '\x12');
      expect(encodeCtrlLetter('1'), isNull);
    });

    test('修饰位编码与 CSI u 规范一致', () {
      expect(csiUModifiers(), 1);
      expect(csiUModifiers(shift: true), 2);
      expect(csiUModifiers(alt: true), 3);
      expect(csiUModifiers(ctrl: true), 5);
      expect(csiUModifiers(shift: true, ctrl: true), 6);
    });
  });
}
