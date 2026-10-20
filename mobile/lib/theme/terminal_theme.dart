/// 终端的配色和字。
///
/// 终端固定深色，不跟 App 的明暗走：终端输出本来就是按深底调出来的（TUI 挑灰度、
/// 语法高亮都默认深底），跟着浅色模式翻成白底，同一段输出会和桌面上看到的两样。
/// 底色与前景取 [Primitives.termBg] / [Primitives.termFg]；ANSI 16 色抄桌面出厂的
/// `PALETTE_DARK`（`crates/smelt/src/terminal.rs`），同一段 TUI 输出两端是同一套颜色。
library;

import 'package:flutter/painting.dart';
import 'package:xterm/xterm.dart';

import 'galley.dart';
import 'primitives.dart';

abstract final class SmeltTerminalTheme {
  // ANSI 16 色：桌面 Tokyo Night 一套（白 / 亮白改成灰白 / 纯白）。只有终端用，
  // 所以留在这里而不进 Primitives。
  static const _black = Color(0xFF15161E);
  static const _red = Color(0xFFF7768E);
  static const _green = Color(0xFF9ECE6A);
  static const _yellow = Color(0xFFE0AF68);
  static const _blue = Color(0xFF7AA2F7);
  static const _magenta = Color(0xFFBB9AF7);
  static const _cyan = Color(0xFF7DCFFF);
  static const _white = Color(0xFFC7C7C7);
  static const _brightBlack = Color(0xFF2C3149);
  static const _brightWhite = Color(0xFFFFFFFF);

  // 搜索命中色（桌面 `terminal_view.rs` 同值）。手机上暂时没有搜索，xterm 要求给。
  static const _searchHit = Color(0xFF7A5C20);
  static const _searchHitCurrent = Color(0xFFD4A017);

  /// xterm 把选区矩形画在字形**上面**，不透明色会把字整个盖住；压低 alpha 字才透得出来。
  static const double _selectionAlpha = 0.32;

  /// 交给 `TerminalView` 的那一份。`static final` 是为了每次重建拿到同一个实例：
  /// 换一个（哪怕值一样）终端就会白白整屏重绘。
  ///
  /// 选区用琥珀半透明：深底上最醒目、又不和任何 ANSI 色撞；光标是实心块，用前景色。
  static final TerminalTheme dark = TerminalTheme(
    cursor: Primitives.termFg,
    selection: Primitives.heat70.withValues(alpha: _selectionAlpha),
    foreground: Primitives.termFg,
    background: Primitives.termBg,
    black: _black,
    red: _red,
    green: _green,
    yellow: _yellow,
    blue: _blue,
    magenta: _magenta,
    cyan: _cyan,
    white: _white,
    brightBlack: _brightBlack,
    brightRed: _red,
    brightGreen: _green,
    brightYellow: _yellow,
    brightBlue: _blue,
    brightMagenta: _magenta,
    brightCyan: _cyan,
    brightWhite: _brightWhite,
    searchHitBackground: _searchHit,
    searchHitBackgroundCurrent: _searchHitCurrent,
    searchHitForeground: Primitives.termFg,
  );

  /// 行高：等宽网格里留一点行距，CJK 字形不顶格；再大一屏就少几行。
  static const double lineHeight = 1.15;

  /// 终端网格的字。字号只取 `Preferences.terminalFontSteps` 里的档位。
  static TerminalStyle style(double fontSize) => TerminalStyle(
    fontSize: fontSize,
    height: lineHeight,
    fontFamily: GalleyText.monoFamily,
  );

  /// 终端视口四周的留白：左右各留一点，字不贴屏幕边。
  static const EdgeInsets padding = EdgeInsets.symmetric(
    horizontal: Space.xs + 2,
    vertical: Space.xs,
  );
}
