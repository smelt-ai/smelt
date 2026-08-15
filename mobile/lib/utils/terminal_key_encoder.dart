/// 把「按了哪个键 + 哪些修饰键」翻译成要写进 PTY 的字节。
///
/// 为什么不直接用 xterm.dart 的 `Terminal.keyInput()`：它的 keytab 把
/// `Return+Shift` 映射成 `\EOM`（小键盘 Enter），Claude Code 不认；而且它完全没有
/// kitty keyboard protocol 的概念。这两点恰好都落在移动端最需要的键上
/// （Shift+Enter 换行、Shift+Tab 切模式），所以这里照搬桌面端
/// `crates/smelt/src/terminal_view.rs` 的 `keystroke_to_bytes` 那张真值表，
/// 两端保持同一套编码。
library;

enum TermKey {
  escape,
  tab,
  enter,
  backspace,
  up,
  down,
  left,
  right,
  home,
  end,
  pageUp,
  pageDown,
  insert,
  delete,
}

/// CSI u 的修饰位：1 + shift + alt<<1 + ctrl<<2。
int csiUModifiers({bool shift = false, bool alt = false, bool ctrl = false}) =>
    1 + (shift ? 1 : 0) + (alt ? 2 : 0) + (ctrl ? 4 : 0);

/// kitty CSI u 的键码表（与桌面端 `kitty_special_key` 一致）。
const _kittyCode = <TermKey, int>{
  TermKey.up: 1,
  TermKey.down: 2,
  TermKey.left: 3,
  TermKey.right: 4,
  TermKey.tab: 9,
  TermKey.home: 11,
  TermKey.end: 12,
  TermKey.pageUp: 13,
  TermKey.pageDown: 14,
  TermKey.insert: 15,
  TermKey.delete: 16,
  TermKey.escape: 27,
  TermKey.backspace: 127,
};

/// 带修饰键时的 xterm 序列（未开 kitty 时用）。
const _xtermModified = <TermKey, String>{
  TermKey.up: '\x1b[1;{m}A',
  TermKey.down: '\x1b[1;{m}B',
  TermKey.right: '\x1b[1;{m}C',
  TermKey.left: '\x1b[1;{m}D',
  TermKey.home: '\x1b[1;{m}H',
  TermKey.end: '\x1b[1;{m}F',
  TermKey.insert: '\x1b[2;{m}~',
  TermKey.delete: '\x1b[3;{m}~',
  TermKey.pageUp: '\x1b[5;{m}~',
  TermKey.pageDown: '\x1b[6;{m}~',
};

/// 光标键在 application cursor 模式（DECCKM，vim 等 TUI 会开）下改用 SS3 前缀。
/// 发错的话应用收不到方向键——这是原来硬编码 `\x1b[A` 的老问题。
const _cursorKeys = <TermKey, String>{
  TermKey.up: 'A',
  TermKey.down: 'B',
  TermKey.right: 'C',
  TermKey.left: 'D',
  TermKey.home: 'H',
  TermKey.end: 'F',
};

const _plain = <TermKey, String>{
  TermKey.escape: '\x1b',
  TermKey.tab: '\t',
  TermKey.backspace: '\x7f',
  TermKey.insert: '\x1b[2~',
  TermKey.delete: '\x1b[3~',
  TermKey.pageUp: '\x1b[5~',
  TermKey.pageDown: '\x1b[6~',
};

/// [kitty] 传对端是否开了 DISAMBIGUATE，[appCursor] 传 DECCKM。
String encodeTermKey(
  TermKey key, {
  bool shift = false,
  bool alt = false,
  bool ctrl = false,
  bool kitty = false,
  bool appCursor = false,
}) {
  final mods = csiUModifiers(shift: shift, alt: alt, ctrl: ctrl);

  // Enter 必须单拎：遗留编码里 Shift/Ctrl+Enter 全塌缩成 `\r`，跟裸 Enter 无从
  // 区分，所以「Shift+Enter 换行、Enter 提交」只有开了 kitty 才真正做得到。
  if (key == TermKey.enter) {
    if (kitty && mods > 1) return '\x1b[13;${mods}u';
    if (alt) return '\x1b\r';
    // 没开 kitty 时跟桌面端一样退回 LF，部分多行 prompt 认这个。
    if (shift) return '\n';
    return '\r';
  }

  // Backspace 的 Alt/Ctrl 变体在两种模式下都用遗留编码，跟桌面端保持一致。
  if (key == TermKey.backspace && !kitty) {
    if (alt) return '\x1b\x7f';
    if (ctrl) return '\x08';
  }

  if (kitty && mods > 1) {
    final code = _kittyCode[key];
    if (code != null) return '\x1b[$code;${mods}u';
  }

  if (mods > 1) {
    final template = _xtermModified[key];
    if (template != null) return template.replaceFirst('{m}', '$mods');
    // Shift+Tab 没有 xterm 修饰形式，用它自己的 backtab 序列。
    if (key == TermKey.tab && shift) return '\x1b[Z';
  }

  final cursor = _cursorKeys[key];
  if (cursor != null) return appCursor ? '\x1bO$cursor' : '\x1b[$cursor';

  return _plain[key] ?? '';
}

/// Ctrl+字母 → C0 控制符（Ctrl+A = 0x01 …… Ctrl+Z = 0x1a）。
String? encodeCtrlLetter(String letter) {
  if (letter.length != 1) return null;
  final upper = letter.toUpperCase().codeUnitAt(0);
  if (upper < 0x41 || upper > 0x5a) return null;
  return String.fromCharCode(upper - 0x40);
}
