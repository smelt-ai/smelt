import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/preferences.dart';
import 'package:smelt_mobile/features/machines/machine_detail_page.dart';
import 'package:smelt_mobile/features/settings/settings_page.dart';

import 'harness.dart';

void main() {
  testWidgets('每台机器一行：名字和链路一句话，末行是配对新机器', (tester) async {
    await pumpSettingsHarness(tester, SettingsBackend(), const SettingsPage());

    expect(find.text('MacBook Pro'), findsOneWidget);
    expect(find.text('Local · 12 ms'), findsOneWidget);
    expect(find.text('home-mini'), findsOneWidget);
    expect(find.text('Relay · 84 ms'), findsOneWidget);
    expect(find.text('build-01'), findsOneWidget);
    expect(find.text('Reconnecting…'), findsOneWidget);
    expect(find.text('Pair a new machine'), findsOneWidget);
    // 机器按登记顺序排，配对入口在它们后面。
    final y = [
      for (final name in ['MacBook Pro', 'home-mini', 'build-01'])
        tester.getTopLeft(find.text(name)).dy,
    ];
    expect(y, orderedEquals([...y]..sort()));
    expect(
      tester.getTopLeft(find.text('Pair a new machine')).dy,
      greaterThan(y.last),
    );
  });

  testWidgets('点一台机器打开它的详情', (tester) async {
    await pumpSettingsHarness(tester, SettingsBackend(), const SettingsPage());

    await tester.tap(find.text('home-mini'));
    await tester.pumpAndSettle();

    expect(find.byType(MachineDetailPage), findsOneWidget);
    expect(find.text('Forget this machine'), findsOneWidget);
  });

  testWidgets('长按拖动调整机器顺序，按拖完的位置调 moveRuntime', (tester) async {
    final backend = SettingsBackend();
    final model = await pumpSettingsHarness(
      tester,
      backend,
      const SettingsPage(),
    );

    final from = tester.getCenter(find.text('build-01'));
    final to = tester.getCenter(find.text('MacBook Pro'));
    final gesture = await tester.startGesture(from);
    await tester.pump(kLongPressTimeout + const Duration(milliseconds: 100));
    // 分几步拖到第一行上沿之上：列表按替身的位置一格一格让位。
    final dy = to.dy - from.dy - 40;
    for (var i = 0; i < 8; i++) {
      await gesture.moveBy(Offset(0, dy / 8));
      await tester.pump(const Duration(milliseconds: 50));
    }
    await gesture.up();
    await tester.pumpAndSettle();

    expect(backend.moves, [('rt-build', 0)]);
    expect(model.fleet.value.runtimes.map((r) => r.record.runtimeId), [
      'rt-build',
      'rt-mbp',
      'rt-mini',
    ]);
  });

  testWidgets('明暗分段控件写进偏好', (tester) async {
    final prefs = PreferencesStore.memory();
    await pumpSettingsHarness(
      tester,
      SettingsBackend(),
      const SettingsPage(),
      prefs: prefs,
    );
    expect(prefs.value.themeMode, ThemeMode.system);

    await tester.tap(find.text('Dark'));
    await tester.pumpAndSettle();
    expect(prefs.value.themeMode, ThemeMode.dark);

    await tester.tap(find.text('Light'));
    await tester.pumpAndSettle();
    expect(prefs.value.themeMode, ThemeMode.light);

    await tester.tap(find.text('System'));
    await tester.pumpAndSettle();
    expect(prefs.value.themeMode, ThemeMode.system);
  });

  testWidgets('终端字号按档位一步一步走，到头就不能再按', (tester) async {
    final prefs = PreferencesStore.memory(
      const Preferences(terminalFontSize: 18),
    );
    await pumpSettingsHarness(
      tester,
      SettingsBackend(),
      const SettingsPage(),
      prefs: prefs,
    );
    expect(find.text('18 pt'), findsOneWidget);

    await tester.tap(find.byTooltip('Larger text'));
    await tester.pumpAndSettle();
    expect(prefs.value.terminalFontSize, 20);
    expect(find.text('20 pt'), findsOneWidget);

    // 最大一档：再按没反应。
    await tester.tap(find.byTooltip('Larger text'));
    await tester.pumpAndSettle();
    expect(prefs.value.terminalFontSize, 20);

    await tester.tap(find.byTooltip('Smaller text'));
    await tester.pumpAndSettle();
    expect(prefs.value.terminalFontSize, 18);
    expect(
      Preferences.terminalFontSteps,
      contains(prefs.value.terminalFontSize),
    );
  });

  testWidgets('没有机器时只有配对入口和一句说明', (tester) async {
    final backend = SettingsBackend();
    for (final id in ['rt-mbp', 'rt-mini', 'rt-build']) {
      await backend.forgetRuntime(id);
    }
    await pumpSettingsHarness(tester, backend, const SettingsPage());

    expect(find.text('Pair a new machine'), findsOneWidget);
    expect(
      find.text('No machines yet. Pair one and its sessions show up here.'),
      findsOneWidget,
    );
    expect(find.text('MacBook Pro'), findsNothing);
  });
}
