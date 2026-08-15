import 'dart:typed_data';

/// Removes private xterm control sequences that xterm.dart 4.0 misreads as SGR.
///
/// 顺带在同一趟扫描里记录对端有没有开 kitty keyboard protocol。这里做是因为每个
/// PTY 输出字节本来就要过这个过滤器，而 xterm.dart 完全不认这套协议——它既不会
/// 记状态，也就没法告诉我们该用哪种编码发按键。放在这里不用改网关协议，老网关
/// 一样能用。
class XtermInputFilter {
  static const _escape = 0x1b;
  static const _csi = 0x5b;
  static const _privateGreaterThan = 0x3e;
  static const _privateLessThan = 0x3c;
  static const _privateEquals = 0x3d;
  static const _sgrFinal = 0x6d;
  static const _kittyFinal = 0x75; // 'u'
  static const _maxSequenceLength = 1024;

  final _pending = <int>[];

  /// kitty keyboard protocol 的模式栈。应用用 `CSI > flags u` 压栈、`CSI < n u`
  /// 出栈，所以要按栈记——只记一个布尔的话，嵌套使用的程序退出内层就会把外层也
  /// 关掉。
  final _kittyStack = <int>[];

  /// 对端是否开了 DISAMBIGUATE（flag 1）。开着时带修饰的按键必须走 CSI u：
  /// kitty 规范规定这种模式下歧义键的 legacy 序列被抑制，Shift+Tab 再发 `ESC[Z`
  /// 应用收不到。Claude Code v2.1 起启动就会开。
  bool get kittyKeyboardEnabled =>
      _kittyStack.isNotEmpty && (_kittyStack.last & 1) != 0;

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
        _trackKittyKeyboard(byte);
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

  /// `_pending` 此刻是一条完整的 CSI（含 ESC [ 开头）。只认 kitty 那三条：
  /// `CSI > flags u` 压栈、`CSI < n u` 出栈、`CSI = flags ; mode u` 改栈顶。
  /// `CSI ? u` 是查询，回包由终端发、不经过这里，忽略。
  void _trackKittyKeyboard(int finalByte) {
    if (finalByte != _kittyFinal || _pending.length < 3) return;
    final prefix = _pending[2];
    final body = String.fromCharCodes(_pending.sublist(3, _pending.length - 1));

    switch (prefix) {
      case _privateGreaterThan:
        // 参数缺省时按 flags=1，跟 kitty 一致。
        _kittyStack.add(body.isEmpty ? 1 : (int.tryParse(body) ?? 1));
        // 栈深度设上限，坏流不至于把内存吃穿。
        if (_kittyStack.length > 32) _kittyStack.removeAt(0);
      case _privateLessThan:
        final count = body.isEmpty ? 1 : (int.tryParse(body) ?? 1);
        for (var i = 0; i < count && _kittyStack.isNotEmpty; i++) {
          _kittyStack.removeLast();
        }
      case _privateEquals:
        final flags = int.tryParse(body.split(';').first);
        if (flags == null) return;
        // mode 3 = 只清不设；1 = 全设；2 = 置位。这里只关心 flag 1 在不在，
        // 前两种都可以直接当成「栈顶变成 flags」。
        final mode = body.contains(';')
            ? int.tryParse(body.split(';')[1]) ?? 1
            : 1;
        if (_kittyStack.isEmpty) {
          if (mode != 3) _kittyStack.add(flags);
          return;
        }
        _kittyStack[_kittyStack.length - 1] = switch (mode) {
          2 => _kittyStack.last | flags,
          3 => _kittyStack.last & ~flags,
          _ => flags,
        };
    }
  }

  bool _isCsiBody(int byte) => byte >= 0x20 && byte <= 0x3f;

  bool _isCsiFinal(int byte) => byte >= 0x40 && byte <= 0x7e;
}
