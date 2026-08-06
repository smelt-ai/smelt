import 'dart:typed_data';

/// Removes private xterm control sequences that xterm.dart 4.0 misreads as SGR.
class XtermInputFilter {
  static const _escape = 0x1b;
  static const _csi = 0x5b;
  static const _privateGreaterThan = 0x3e;
  static const _sgrFinal = 0x6d;
  static const _maxSequenceLength = 1024;

  final _pending = <int>[];

  Uint8List add(List<int> bytes) {
    final output = <int>[];
    for (final byte in bytes) {
      if (_pending.isEmpty) {
        if (byte == _escape) {
          _pending.add(byte);
        } else {
          output.add(byte);
        }
        continue;
      }

      if (_pending.length == 1) {
        if (byte == _csi) {
          _pending.add(byte);
        } else {
          output.add(_escape);
          if (byte == _escape) {
            _pending[0] = byte;
          } else {
            output.add(byte);
            _pending.clear();
          }
        }
        continue;
      }

      _pending.add(byte);
      if (_isCsiFinal(byte)) {
        if (!_isUnsupportedPrivateModifier(byte)) {
          output.addAll(_pending);
        }
        _pending.clear();
      } else if (!_isCsiBody(byte) || _pending.length >= _maxSequenceLength) {
        output.addAll(_pending);
        _pending.clear();
      }
    }
    return Uint8List.fromList(output);
  }

  Uint8List flush() {
    final output = Uint8List.fromList(_pending);
    _pending.clear();
    return output;
  }

  bool _isUnsupportedPrivateModifier(int finalByte) =>
      finalByte == _sgrFinal &&
      _pending.length > 2 &&
      _pending[2] == _privateGreaterThan;

  bool _isCsiBody(int byte) => byte >= 0x20 && byte <= 0x3f;

  bool _isCsiFinal(int byte) => byte >= 0x40 && byte <= 0x7e;
}
