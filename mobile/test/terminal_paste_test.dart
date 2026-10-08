import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/utils/terminal_paste.dart';

void main() {
  test('plain paste turns newlines into CR so a shell submits each line', () {
    expect(
      encodeTerminalPaste('one\r\ntwo\nthree', bracketed: false),
      'one\rtwo\rthree',
    );
  });

  test('bracketed paste wraps the text and strips escapes', () {
    expect(
      encodeTerminalPaste('rm\x1b[31m file', bracketed: true),
      '\x1b[200~rm[31m file\x1b[201~',
    );
  });
}
