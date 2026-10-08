import 'package:flutter/foundation.dart';
import 'package:xterm/xterm.dart';

/// 长按选词用。xterm 默认把 `.:/-` 都当成分隔符，`https://example.com/a`
/// 只能选中中间一段，复制出来不是链接。CLI 里这些字符属于同一个 token。
final Set<int> cliWordSeparators = {0, ...' \t"\'`()[]{}<>|*+\\,;'.codeUnits};

/// 复制到剪贴板的纯文本。有选区时原样返回（用户选了什么就复制什么）；
/// 没选区时复制整段回滚，并丢掉末尾空白行。
String terminalClipboardText(Terminal terminal, BufferRange? selection) {
  final text = terminal.buffer.getText(selection);
  return selection == null ? text.trimRight() : text;
}

/// 只允许在手机上直接打开的链接。桌面路径、`file:`、`javascript:` 都不开：
/// 前者是那台电脑的磁盘，后者不该从终端输出里被点开。
String? openableHttpUrl(String raw) {
  final text = raw.trim();
  final lower = text.toLowerCase();
  if (!lower.startsWith('http://') && !lower.startsWith('https://')) {
    return null;
  }
  if (text.contains(RegExp(r'\s'))) return null;
  final uri = Uri.tryParse(text);
  if (uri == null || uri.host.isEmpty) return null;
  final scheme = uri.scheme.toLowerCase();
  if (scheme != 'http' && scheme != 'https') return null;
  return text;
}

class TerminalLinkHit {
  const TerminalLinkHit({
    required this.url,
    required this.start,
    required this.end,
  });

  /// 已经通过 [openableHttpUrl] 的目标。
  final String url;

  /// 含。
  final CellOffset start;

  /// 不含。跟 xterm 选区的 extent 同一约定，可以直接拿去 `setSelection`。
  final CellOffset end;
}

/// 终端里可点的链接。
///
/// 两路来源，和桌面终端一致：先看 OSC 8（可见文字往往只是标题，真正的 URL
/// 在协议里），没有再按可见文本扫 `http(s)://`。软换行截断的长链接会先拼回
/// 一整行再扫，避免点出来的是被切断的半截地址。
class TerminalLinkIndex {
  final List<_OscLink> _links = [];
  _PendingOsc? _pending;

  @visibleForTesting
  int get trackedLinkCount => _links.length;

  void clear() {
    for (final link in _links) {
      link.dispose();
    }
    _links.clear();
    _pending?.dispose();
    _pending = null;
  }

  /// `OSC 8 ; params ; URI ST`。空 URI 结束当前链接；新的非空 URI 先结束上一条。
  void onOsc(Terminal terminal, String code, List<String> args) {
    if (code != '8') return;
    // 只在点击时清理的话，持续输出超链接（`ls --hyperlink`）会让列表一直涨。
    // 滚出回滚上限的行会把锚点解除挂载，这里顺手收掉。
    _pruneDetached();
    final uri = args.length >= 2 ? args.sublist(1).join(';') : '';
    _end(terminal);
    if (uri.isEmpty) return;
    _pending = _PendingOsc(uri, terminal.buffer.createAnchorFromCursor());
  }

  String? targetAt(Terminal terminal, CellOffset cell) =>
      hitAt(terminal, cell)?.url;

  TerminalLinkHit? hitAt(Terminal terminal, CellOffset cell) {
    _pruneDetached();
    for (final link in _links.reversed) {
      if (!_onBuffer(terminal, link.start) || !_onBuffer(terminal, link.end)) {
        continue;
      }
      final start = link.start.offset;
      final end = link.end.offset;
      if (!_contains(cell, start, end)) continue;
      final url = openableHttpUrl(link.uri);
      if (url == null) continue;
      return TerminalLinkHit(url: url, start: start, end: end);
    }

    final pending = _pending;
    if (pending != null && _onBuffer(terminal, pending.start)) {
      final end = CellOffset(
        terminal.buffer.cursorX,
        terminal.buffer.absoluteCursorY,
      );
      if (_contains(cell, pending.start.offset, end)) {
        final url = openableHttpUrl(pending.uri);
        if (url != null) {
          return TerminalLinkHit(
            url: url,
            start: pending.start.offset,
            end: end,
          );
        }
      }
    }

    return _urlAtCell(terminal, cell);
  }

  void _end(Terminal terminal) {
    final pending = _pending;
    _pending = null;
    if (pending == null) return;
    final end = terminal.buffer.createAnchorFromCursor();
    if (!_onBuffer(terminal, pending.start) ||
        !_onBuffer(terminal, end) ||
        !end.offset.isAfter(pending.start.offset)) {
      pending.dispose();
      end.dispose();
      return;
    }
    _links.add(_OscLink(pending.uri, pending.start, end));
  }

  void _pruneDetached() {
    _links.removeWhere((link) {
      final dead = !link.start.attached || !link.end.attached;
      if (dead) link.dispose();
      return dead;
    });
    if (_pending != null && !_pending!.start.attached) {
      _pending!.dispose();
      _pending = null;
    }
  }

  bool _onBuffer(Terminal terminal, CellAnchor anchor) {
    if (!anchor.attached) return false;
    final lines = terminal.buffer.lines;
    final y = anchor.y;
    if (y < 0 || y >= lines.length) return false;
    return identical(anchor.line, lines[y]);
  }
}

class _PendingOsc {
  _PendingOsc(this.uri, this.start);

  final String uri;
  final CellAnchor start;

  void dispose() => start.dispose();
}

class _OscLink {
  _OscLink(this.uri, this.start, this.end);

  final String uri;
  final CellAnchor start;
  final CellAnchor end;

  void dispose() {
    start.dispose();
    end.dispose();
  }
}

const _urlTrailingPunctuation = {
  0x2e,
  0x2c,
  0x3b,
  0x3a,
  0x21,
  0x3f,
  0x29,
  0x5d,
  0x7d,
  0x22,
  0x27,
};

TerminalLinkHit? _urlAtCell(Terminal terminal, CellOffset cell) {
  final buffer = terminal.buffer;
  if (cell.y < 0 || cell.y >= buffer.lines.length) return null;

  final first = _wrappedStart(buffer, cell.y);
  final last = _wrappedEnd(buffer, cell.y);
  final glyphs = <_Glyph>[];
  for (var row = first; row <= last; row++) {
    final line = buffer.lines[row];
    final limit = line.length < buffer.viewWidth
        ? line.length
        : buffer.viewWidth;
    var col = 0;
    while (col < limit) {
      final codePoint = line.getCodePoint(col);
      var span = line.getWidth(col);
      if (span <= 0) span = 1;
      if (col + span > limit) span = limit - col;
      if (codePoint != 0) {
        glyphs.add(_Glyph(row, col, col + span, codePoint));
      }
      col += span;
    }
  }

  var i = 0;
  while (i < glyphs.length) {
    if (!_startsHttp(glyphs, i)) {
      i++;
      continue;
    }
    var end = i;
    while (end < glyphs.length && _isUrlChar(glyphs[end].cp)) {
      end++;
    }
    while (end > i && _urlTrailingPunctuation.contains(glyphs[end - 1].cp)) {
      end--;
    }
    final hit = end - i >= 10 ? _hit(glyphs, i, end, cell) : null;
    if (hit != null) return hit;
    i = end > i ? end : i + 1;
  }
  return null;
}

TerminalLinkHit? _hit(
  List<_Glyph> glyphs,
  int start,
  int end,
  CellOffset cell,
) {
  _Glyph? first;
  _Glyph? last;
  var covers = false;
  for (var i = start; i < end; i++) {
    final glyph = glyphs[i];
    first ??= glyph;
    last = glyph;
    if (glyph.row == cell.y &&
        cell.x >= glyph.startCol &&
        cell.x < glyph.endCol) {
      covers = true;
    }
  }
  if (!covers || first == null || last == null) return null;
  final url = openableHttpUrl(
    String.fromCharCodes(glyphs.sublist(start, end).map((glyph) => glyph.cp)),
  );
  if (url == null) return null;
  return TerminalLinkHit(
    url: url,
    start: CellOffset(first.startCol, first.row),
    end: CellOffset(last.endCol, last.row),
  );
}

bool _startsHttp(List<_Glyph> glyphs, int index) {
  return _startsScheme(glyphs, index, 'https://') ||
      _startsScheme(glyphs, index, 'http://');
}

bool _startsScheme(List<_Glyph> glyphs, int index, String scheme) {
  if (index + scheme.length > glyphs.length) return false;
  for (var i = 0; i < scheme.length; i++) {
    final codePoint = glyphs[index + i].cp;
    final folded = codePoint >= 0x41 && codePoint <= 0x5a
        ? codePoint + 0x20
        : codePoint;
    if (folded != scheme.codeUnitAt(i)) return false;
  }
  return true;
}

bool _isUrlChar(int codePoint) {
  if (codePoint <= 32 || codePoint == 0x7f) return false;
  return !_blockedUrlChars.contains(codePoint);
}

const _blockedUrlChars = {0x3c, 0x3e, 0x22, 0x60, 0x7c, 0x5e, 0x7b, 0x7d};

int _wrappedStart(Buffer buffer, int row) {
  var first = row;
  while (first > 0 && buffer.lines[first].isWrapped) {
    first--;
  }
  return first;
}

int _wrappedEnd(Buffer buffer, int row) {
  var last = row;
  while (last + 1 < buffer.lines.length && buffer.lines[last + 1].isWrapped) {
    last++;
  }
  return last;
}

bool _contains(CellOffset cell, CellOffset start, CellOffset end) {
  if (!end.isAfter(start)) return false;
  if (cell.y < start.y || cell.y > end.y) return false;
  if (start.y == end.y) {
    return cell.x >= start.x && cell.x < end.x;
  }
  if (cell.y == start.y) return cell.x >= start.x;
  if (cell.y == end.y) return cell.x < end.x;
  return true;
}

class _Glyph {
  const _Glyph(this.row, this.startCol, this.endCol, this.cp);

  final int row;
  final int startCol;
  final int endCol;
  final int cp;
}
