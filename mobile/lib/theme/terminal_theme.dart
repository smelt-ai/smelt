import 'package:flutter/material.dart';
import 'package:xterm/xterm.dart';

/// 终端配色由手机自己定：跟 App 当前的明暗主题走，用内置的两套。
///
/// 运行时不再下发配色。两套色值抄自桌面出厂主题（`crates/smelt/src/terminal.rs` 的
/// `PALETTE_DARK` / `PALETTE_LIGHT` 与默认前景色、`terminal_view.rs` 的选区与搜索命中色），
/// 同一段 TUI 输出在两端看起来是同一套颜色。深色底色取手机舞台底，终端和页面之间不露界缝。
class SmeltTerminalTheme {
  const SmeltTerminalTheme._();

  static const TerminalTheme dark = TerminalTheme(
    cursor: Color(0xffd8d8d8),
    selection: Color(0xff0c3d7a),
    foreground: Color(0xffd8d8d8),
    background: Color(0xff070707),
    black: Color(0xff15161e),
    red: Color(0xfff7768e),
    green: Color(0xff9ece6a),
    yellow: Color(0xffe0af68),
    blue: Color(0xff7aa2f7),
    magenta: Color(0xffbb9af7),
    cyan: Color(0xff7dcfff),
    white: Color(0xffc7c7c7),
    brightBlack: Color(0xff2c3149),
    brightRed: Color(0xfff7768e),
    brightGreen: Color(0xff9ece6a),
    brightYellow: Color(0xffe0af68),
    brightBlue: Color(0xff7aa2f7),
    brightMagenta: Color(0xffbb9af7),
    brightCyan: Color(0xff7dcfff),
    brightWhite: Color(0xffffffff),
    searchHitBackground: Color(0xff7a5c20),
    searchHitBackgroundCurrent: Color(0xffd4a017),
    searchHitForeground: Color(0xffd8d8d8),
  );

  /// 浅色色板是深色同色相压深/加饱和的版本，浅底上仍有足够对比度。
  static const TerminalTheme light = TerminalTheme(
    // 光标是实心块，块本身就是前景色（与桌面一致）。
    cursor: Color(0xff24292e),
    selection: Color(0xffd6eaff),
    foreground: Color(0xff24292e),
    background: Color(0xffffffff),
    // 显式 ANSI black 常被 TUI 用来画分隔线；浅底上用深灰，避免比正文还抢眼。
    black: Color(0xff3f4654),
    red: Color(0xffc0324a),
    green: Color(0xff4e8a2f),
    yellow: Color(0xffa1690f),
    blue: Color(0xff3760bf),
    magenta: Color(0xff7847bd),
    cyan: Color(0xff0f7b9e),
    white: Color(0xff4a4a4a),
    brightBlack: Color(0xff6b7089),
    brightRed: Color(0xffd7495f),
    brightGreen: Color(0xff5fae3f),
    brightYellow: Color(0xffc48511),
    brightBlue: Color(0xff2e6fe0),
    brightMagenta: Color(0xff9161d9),
    brightCyan: Color(0xff1093c2),
    brightWhite: Color(0xff1a1b26),
    searchHitBackground: Color(0xffffe9a8),
    searchHitBackgroundCurrent: Color(0xffffc107),
    searchHitForeground: Color(0xff24292e),
  );

  /// App 当前明暗对应的那一套。
  static TerminalTheme of(Brightness brightness) =>
      brightness == Brightness.light ? light : dark;

  /// 交给 `TerminalView` 画的那一份（见 [forDisplay]）。缓存起来，页面每次重建拿到的是
  /// 同一个实例，不会让终端白白重绘。
  static TerminalTheme displayOf(Brightness brightness) =>
      brightness == Brightness.light ? _lightDisplay : _darkDisplay;

  static final TerminalTheme _darkDisplay = forDisplay(dark);
  static final TerminalTheme _lightDisplay = forDisplay(light);

  /// xterm 把选区矩形画在字形上面，而且用的是不透明色，浅色主题的选区色会把字整个
  /// 盖住。渲染前把 alpha 压下来，字才能透出来。
  static const double selectionOverlayAlpha = 0.32;

  static TerminalTheme forDisplay(TerminalTheme theme) {
    return TerminalTheme(
      cursor: theme.cursor,
      selection: theme.selection.withValues(alpha: selectionOverlayAlpha),
      foreground: theme.foreground,
      background: theme.background,
      black: theme.black,
      red: theme.red,
      green: theme.green,
      yellow: theme.yellow,
      blue: theme.blue,
      magenta: theme.magenta,
      cyan: theme.cyan,
      white: theme.white,
      brightBlack: theme.brightBlack,
      brightRed: theme.brightRed,
      brightGreen: theme.brightGreen,
      brightYellow: theme.brightYellow,
      brightBlue: theme.brightBlue,
      brightMagenta: theme.brightMagenta,
      brightCyan: theme.brightCyan,
      brightWhite: theme.brightWhite,
      searchHitBackground: theme.searchHitBackground,
      searchHitBackgroundCurrent: theme.searchHitBackgroundCurrent,
      searchHitForeground: theme.searchHitForeground,
    );
  }
}
