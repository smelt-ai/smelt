import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/theme/terminal_theme_wire.dart';
import 'package:xterm/xterm.dart';

void main() {
  // 配色由 PC 下发（`crates/smelt-core/src/terminal_theme.rs` 的
  // TerminalThemeSnapshot::to_wire），移动端不写死：PC 主题可配置，而且一部手机
  // 可以连多台设备，每台的主题都可能不同。
  test('applies the theme sent by the connected device', () {
    final theme = SmeltTerminalTheme.fromWire({
      'version': 1,
      'dark': false,
      'background': '#ffffff',
      'foreground': '#24292e',
      'cursor': '#0969da',
      'selection': '#add6ff',
      'palette': [
        '#3f4654',
        '#c0324a',
        '#4e8a2f',
        '#a1690f',
        '#3760bf',
        '#7847bd',
        '#0f7b9e',
        '#4a4a4a',
        '#6b7089',
        '#d7495f',
        '#5fae3f',
        '#c48511',
        '#2e6fe0',
        '#9161d9',
        '#1093c2',
        '#1a1b26',
      ],
      'searchHit': '#ffe9a8',
      'searchHitCurrent': '#ffc107',
    });

    expect(theme.background, const Color(0xffffffff));
    expect(theme.foreground, const Color(0xff24292e));
    expect(theme.cursor, const Color(0xff0969da));
    expect(theme.selection, const Color(0xffadd6ff));
    expect(theme.black, const Color(0xff3f4654));
    expect(theme.brightWhite, const Color(0xff1a1b26));
    expect(theme.searchHitBackground, const Color(0xffffe9a8));
    expect(theme.searchHitBackgroundCurrent, const Color(0xffffc107));
    expect(SmeltTerminalTheme.isDark({'dark': false}), isFalse);
  });

  test('falls back per field when the payload is partial or malformed', () {
    const base = SmeltTerminalTheme.fallbackDark;
    final theme = SmeltTerminalTheme.fromWire({
      'background': '#101112',
      'foreground': 'not-a-color',
      'palette': ['#010203'],
    });

    expect(theme.background, const Color(0xff101112));
    expect(theme.foreground, base.foreground, reason: '坏色值逐项回退，不整屏失色');
    expect(theme.black, const Color(0xff010203));
    expect(theme.red, base.red, reason: '色板不足 16 色时用兜底补齐');
    expect(theme.cursor, base.foreground, reason: '没给光标色时跟随前景色');
    expect(SmeltTerminalTheme.isDark(const {}), isTrue);
  });

  // 老网关不下发 theme 时的兜底必须等于 PC 出厂深色主题
  // （smelt-core 的 TerminalThemeSnapshot::default()），否则 TUI 靠 OSC 11
  // 查到的底色跟手机上渲染的对不上，就是对比度问题。
  test('fallback matches the desktop default dark snapshot', () {
    const TerminalTheme fallback = SmeltTerminalTheme.fallbackDark;
    expect(fallback.background, const Color(0xff313338));
    expect(fallback.foreground, const Color(0xffd8d8d8));
    expect(fallback.selection, const Color(0xff334a6a));
    expect(fallback.black, const Color(0xff15161e));
    expect(fallback.brightWhite, const Color(0xffffffff));
    expect(fallback.searchHitBackground, const Color(0xff7a5c20));
    expect(fallback.searchHitBackgroundCurrent, const Color(0xffd4a017));
  });
}
