import 'package:flutter/material.dart';
import 'package:xterm/xterm.dart';

/// 终端配色**由连接的那台 PC 下发**，移动端不写死。
///
/// 原因有两条：
/// 1. PC 端主题可配置（深/浅色模式 + 用户自选终端底色），配色不是常量；
/// 2. 一部手机可以连多台设备，每台的主题可能都不一样。
///
/// 而且这事不只是好看：TUI（Claude Code、bat、delta…）会用 OSC 11 查终端背景色来
/// 决定自己用哪档灰，应答由 PC 端终端给出（`smelt::terminal::EventProxy::resolve_color`）。
/// 手机渲染的是同一份 PTY 字节流，底色跟应答对不上就是对比度问题。
///
/// 网关在 `terminalReady` 里带上 `theme` 字段（见
/// `crates/smelt-core/src/terminal_theme.rs`），这里负责解析成 xterm 的
/// [TerminalTheme]。老网关不发 `theme` 时回退 [SmeltTerminalTheme.fallbackDark]，
/// 它与 PC 出厂深色主题一致。
class SmeltTerminalTheme {
  const SmeltTerminalTheme._();

  /// 网关没下发配色时的兜底（= PC 出厂深色主题，`smelt-core` 的
  /// `TerminalThemeSnapshot::default()`）。
  static const TerminalTheme fallbackDark = TerminalTheme(
    cursor: Color(0xffd8d8d8),
    selection: Color(0xff334a6a),
    foreground: Color(0xffd8d8d8),
    background: Color(0xff313338),
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

  /// 解析 `terminalReady.theme`。缺字段、色值写坏、色板不足 16 色都逐项回退到
  /// [fallbackDark] 的对应色位——一个坏字段不该让整屏没颜色。
  static TerminalTheme fromWire(Map<String, dynamic> json) {
    const base = fallbackDark;
    Color color(String key, Color fallback) => _parseHex(json[key]) ?? fallback;

    final rawPalette = json['palette'];
    final palette = <Color?>[
      if (rawPalette is List)
        for (final entry in rawPalette.take(16)) _parseHex(entry),
    ];
    Color ansi(int index, Color fallback) =>
        index < palette.length ? (palette[index] ?? fallback) : fallback;

    final foreground = color('foreground', base.foreground);
    return TerminalTheme(
      cursor: color('cursor', foreground),
      selection: color('selection', base.selection),
      foreground: foreground,
      background: color('background', base.background),
      black: ansi(0, base.black),
      red: ansi(1, base.red),
      green: ansi(2, base.green),
      yellow: ansi(3, base.yellow),
      blue: ansi(4, base.blue),
      magenta: ansi(5, base.magenta),
      cyan: ansi(6, base.cyan),
      white: ansi(7, base.white),
      brightBlack: ansi(8, base.brightBlack),
      brightRed: ansi(9, base.brightRed),
      brightGreen: ansi(10, base.brightGreen),
      brightYellow: ansi(11, base.brightYellow),
      brightBlue: ansi(12, base.brightBlue),
      brightMagenta: ansi(13, base.brightMagenta),
      brightCyan: ansi(14, base.brightCyan),
      brightWhite: ansi(15, base.brightWhite),
      searchHitBackground: color('searchHit', base.searchHitBackground),
      searchHitBackgroundCurrent: color(
        'searchHitCurrent',
        base.searchHitBackgroundCurrent,
      ),
      searchHitForeground: foreground,
    );
  }

  /// PC 是不是深色主题：手机侧的终端周边 chrome（页面底色等）跟着走，
  /// 免得浅色终端嵌在纯黑页面里出现刺眼的边框对比。
  static bool isDark(Map<String, dynamic> json) =>
      json['dark'] as bool? ?? true;

  /// `#rrggbb` / `#aarrggbb`（也接受不带 `#` 的）→ Color；解析不了返回 null。
  static Color? _parseHex(Object? raw) {
    if (raw is! String) return null;
    final text = raw.startsWith('#') ? raw.substring(1) : raw;
    if (text.length != 6 && text.length != 8) return null;
    final value = int.tryParse(text, radix: 16);
    if (value == null) return null;
    return Color(text.length == 6 ? 0xff000000 | value : value);
  }
}
