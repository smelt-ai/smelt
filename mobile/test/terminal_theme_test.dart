import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/theme/terminal_theme.dart';
import 'package:xterm/xterm.dart';

void main() {
  // 终端配色由手机按 App 明暗自己定，两套都抄自桌面出厂主题：同一段 TUI 输出
  // 在两端看起来是同一套颜色。
  test('dark palette mirrors the desktop default dark terminal', () {
    const TerminalTheme theme = SmeltTerminalTheme.dark;
    expect(theme.background, const Color(0xff070707), reason: '与手机深色舞台底一致');
    expect(theme.foreground, const Color(0xffd8d8d8));
    expect(theme.cursor, theme.foreground);
    expect(theme.selection, const Color(0xff0c3d7a));
    expect(theme.black, const Color(0xff15161e));
    expect(theme.red, const Color(0xfff7768e));
    expect(theme.white, const Color(0xffc7c7c7));
    expect(theme.brightWhite, const Color(0xffffffff));
    expect(theme.searchHitBackground, const Color(0xff7a5c20));
    expect(theme.searchHitBackgroundCurrent, const Color(0xffd4a017));
  });

  test('light palette mirrors the desktop default light terminal', () {
    const TerminalTheme theme = SmeltTerminalTheme.light;
    expect(theme.background, const Color(0xffffffff));
    expect(theme.foreground, const Color(0xff24292e));
    expect(theme.cursor, theme.foreground);
    expect(theme.selection, const Color(0xffd6eaff));
    expect(theme.black, const Color(0xff3f4654));
    expect(theme.red, const Color(0xffc0324a));
    expect(theme.brightWhite, const Color(0xff1a1b26));
    expect(theme.searchHitBackground, const Color(0xffffe9a8));
    expect(theme.searchHitBackgroundCurrent, const Color(0xffffc107));
  });

  test('picks the palette by brightness', () {
    expect(
      SmeltTerminalTheme.of(Brightness.dark),
      same(SmeltTerminalTheme.dark),
    );
    expect(
      SmeltTerminalTheme.of(Brightness.light),
      same(SmeltTerminalTheme.light),
    );
    // 每次重建拿到同一个实例，终端不会因为换了个等值对象白白重绘。
    expect(
      SmeltTerminalTheme.displayOf(Brightness.light),
      same(SmeltTerminalTheme.displayOf(Brightness.light)),
    );
  });

  test('display copy only lowers the selection alpha', () {
    for (final brightness in Brightness.values) {
      final base = SmeltTerminalTheme.of(brightness);
      final displayed = SmeltTerminalTheme.displayOf(brightness);
      expect(
        displayed.selection,
        base.selection.withValues(
          alpha: SmeltTerminalTheme.selectionOverlayAlpha,
        ),
        reason: 'xterm 把不透明选区画在字形上面，会把字整个盖住',
      );
      expect(displayed.background, base.background);
      expect(displayed.foreground, base.foreground);
      expect(displayed.blue, base.blue);
      expect(displayed.searchHitForeground, base.searchHitForeground);
    }
  });
}
