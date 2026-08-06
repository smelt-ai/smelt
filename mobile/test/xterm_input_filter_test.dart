import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/utils/xterm_input_filter.dart';

void main() {
  test('drops xterm private modifier CSI without changing adjacent bytes', () {
    final filter = XtermInputFilter();
    final output = filter.add(utf8.encode('before\x1b[>4;2mafter'));

    expect(utf8.decode(output), 'beforeafter');
  });

  test('drops a private modifier CSI split across every input boundary', () {
    final bytes = utf8.encode('中\x1b[>4;2m文');

    for (var split = 0; split <= bytes.length; split++) {
      final filter = XtermInputFilter();
      final output = <int>[
        ...filter.add(bytes.sublist(0, split)),
        ...filter.add(bytes.sublist(split)),
        ...filter.flush(),
      ];
      expect(utf8.decode(output), '中文', reason: 'split at byte $split');
    }
  });

  test('preserves real SGR and unrelated private CSI sequences', () {
    final filter = XtermInputFilter();
    const input = '\x1b[4;2mtext\x1b[0m\x1b[>1u';

    expect(utf8.decode(filter.add(utf8.encode(input))), input);
  });

  test('flush preserves an incomplete non-filtered sequence', () {
    final filter = XtermInputFilter();
    final first = filter.add(utf8.encode('text\x1b[31'));

    expect(utf8.decode([...first, ...filter.flush()]), 'text\x1b[31');
  });
}
