import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/theme/smelt_theme.dart';

void main() {
  test('status colors mirror the desktop three-state model', () {
    const dark = SmeltColors.dark;
    expect(dark.forStatus('needs_you'), dark.waitingApproval);
    expect(dark.forStatus('waiting_approval'), dark.waitingApproval);
    expect(dark.forStatus('needs_attention'), dark.waitingApproval);
    expect(dark.forStatus('running'), dark.running);
    expect(dark.forStatus('done'), dark.idle);
    expect(dark.forStatus('idle'), dark.idle);
  });

  test('an unknown status degrades to idle rather than an alarming colour', () {
    // 网关将来新增状态时，手机不该把它画成「等审批」那种最高优先级的红。
    expect(SmeltColors.dark.forStatus('teleporting'), SmeltColors.dark.idle);
    expect(SmeltColors.light.forStatus(''), SmeltColors.light.idle);
  });

  test('status colours are case-insensitive', () {
    expect(
      SmeltColors.dark.forStatus('WAITING_APPROVAL'),
      SmeltColors.dark.waitingApproval,
    );
  });

  test('running and done are not the same teal', () {
    // 15px 图标上看的是通道谁压过谁，不是 RGB 欧氏距离。
    int ch(Color c, int shift) => (c.toARGB32() >> shift) & 0xff;
    final run = SmeltColors.dark.running;
    final done = SmeltColors.dark.done;
    expect(ch(run, 0) - ch(run, 8), greaterThan(110));
    expect(ch(done, 8) - ch(done, 0), greaterThan(100));
  });

  test('light and dark carry different values for every status', () {
    // 浅色底上必须换一组压深的值，否则黄和绿在白底上读不出来。
    expect(
      SmeltColors.light.waitingApproval,
      isNot(SmeltColors.dark.waitingApproval),
    );
    expect(
      SmeltColors.light.needsAttention,
      isNot(SmeltColors.dark.needsAttention),
    );
    expect(SmeltColors.light.running, isNot(SmeltColors.dark.running));
    expect(SmeltColors.light.done, isNot(SmeltColors.dark.done));
  });

  testWidgets('both themes register the extension', (tester) async {
    for (final brightness in Brightness.values) {
      late SmeltColors seen;
      await tester.pumpWidget(
        MaterialApp(
          theme: smeltTheme(brightness),
          home: Builder(
            builder: (context) {
              seen = context.smeltColors;
              return const SizedBox.shrink();
            },
          ),
        ),
      );
      // MaterialApp 内置 AnimatedTheme，换主题时会插值；不 settle 的话读到的是
      // 过渡中间态。
      await tester.pumpAndSettle();
      expect(
        seen,
        brightness == Brightness.light ? SmeltColors.light : SmeltColors.dark,
      );
    }
  });

  testWidgets('a bare MaterialApp falls back instead of crashing', (
    tester,
  ) async {
    // 测试里常见的裸 MaterialApp 没有注册扩展；渲染不该因此崩。
    late SmeltColors seen;
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) {
            seen = context.smeltColors;
            return const SizedBox.shrink();
          },
        ),
      ),
    );
    expect(seen, SmeltColors.light);
  });

  test('sunken 面在深浅两色下都比卡片面更暗', () {
    double lum(Color c) => c.computeLuminance();

    // 深色：sunken(rail) 必须暗于 card；曾经错用 surfaceContainerHighest,
    // 让代码块变成整张卡里最亮的块，方向正好反了。
    final darkScheme = smeltTheme(Brightness.dark).colorScheme;
    expect(
      lum(SmeltColors.dark.sunken),
      lessThan(lum(darkScheme.surfaceContainerHigh)),
    );

    // 浅色同理：代码块要比卡片略暗，不能比卡片亮（否则看着像浮起来）。
    final lightScheme = smeltTheme(Brightness.light).colorScheme;
    expect(
      lum(SmeltColors.light.sunken),
      lessThan(lum(lightScheme.surfaceContainerHigh)),
    );
  });

  test('surfaceContainer 梯子在两套主题里都严格单调', () {
    for (final brightness in Brightness.values) {
      final s = smeltTheme(brightness).colorScheme;
      final ladder = [
        s.surfaceContainerLowest,
        s.surfaceContainerLow,
        s.surfaceContainer,
        s.surfaceContainerHigh,
        s.surfaceContainerHighest,
      ].map((c) => c.computeLuminance()).toList();
      for (var i = 1; i < ladder.length; i++) {
        expect(
          ladder[i],
          isNot(equals(ladder[i - 1])),
          reason: '$brightness 的梯子第 $i 级和上一级撞色了',
        );
      }
      // 深色越往上越亮，浅色越往上越暗。
      final ascending = brightness == Brightness.dark;
      for (var i = 1; i < ladder.length; i++) {
        expect(
          ascending ? ladder[i] > ladder[i - 1] : ladder[i] < ladder[i - 1],
          isTrue,
          reason: '$brightness 的梯子在第 $i 级方向反了',
        );
      }
    }
  });
}
