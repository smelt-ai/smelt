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

  test('quoteTerminalPath keeps safe paths plain and quotes special chars', () {
    expect(quoteTerminalPath('/tmp/smelt-1/img-a.png'), '/tmp/smelt-1/img-a.png');
    expect(quoteTerminalPath("/tmp/my 'pic' 1.png"), "'/tmp/my '\\''pic'\\'' 1.png'");
    expect(quoteTerminalPath(''), "''");

    expect(
      quoteTerminalPath(r'C:\Temp\img-1.png', windows: true),
      r'C:\Temp\img-1.png',
    );
    expect(
      quoteTerminalPath(r'C:\My Pics\a"b.png', windows: true),
      r'"C:\My Pics\a\"b.png"',
    );
  });

  test('encodeTerminalImageSubmission separates each image, text, and CR', () {
    expect(
      encodeTerminalImageSubmission(
        paths: ['/tmp/a.png', '/tmp/my pic.jpg'],
        text: '',
        bracketed: true,
      ),
      [
        '\x1b[200~/tmp/a.png \x1b[201~',
        "\x1b[200~'/tmp/my pic.jpg' \x1b[201~",
        '\r',
      ],
    );

    expect(
      encodeTerminalImageSubmission(
        paths: ['/tmp/a.png'],
        text: 'look\nat this',
        bracketed: true,
      ),
      [
        '\x1b[200~/tmp/a.png \x1b[201~',
        '\x1b[200~look\nat this\x1b[201~',
        '\r',
      ],
    );

    expect(
      encodeTerminalImageSubmission(
        paths: [r'C:\Temp\a b.png'],
        text: 'check\nplease',
        bracketed: false,
        windows: true,
      ),
      [
        r'"C:\Temp\a b.png" ',
        'check\rplease',
        '\r',
      ],
    );
  });
}
