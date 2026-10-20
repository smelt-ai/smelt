import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart' hide ConnectionState;
import 'package:flutter_test/flutter_test.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/stores.dart';
import 'package:smelt_mobile/features/machines/machine_detail_page.dart';
import 'package:smelt_mobile/features/settings/settings_page.dart';

import 'harness.dart';

/// 从设置页点进某台机器的详情。
Future<AppModel> _openDetail(
  WidgetTester tester,
  SettingsBackend backend,
  String name,
) async {
  final model = await pumpSettingsHarness(
    tester,
    backend,
    const SettingsPage(),
  );
  await tester.tap(find.text(name));
  await tester.pumpAndSettle();
  expect(find.byType(MachineDetailPage), findsOneWidget);
  return model;
}

Finder _swatch(String label) => find.byWidgetPredicate(
  (w) => w is Semantics && w.properties.label == label,
);

void main() {
  testWidgets('链路详情写出状态、路径、延迟、endpoint 与版本', (tester) async {
    await _openDetail(tester, SettingsBackend(), 'home-mini');

    expect(find.text('Online'), findsOneWidget);
    expect(find.text('Relay'), findsOneWidget);
    expect(find.text('84 ms'), findsOneWidget);
    expect(find.text('demo-rt-mini'), findsOneWidget);
    expect(find.text('Default public relays'), findsOneWidget);
    expect(find.text('0.10.0'), findsOneWidget);
    expect(find.text('macOS · home-mini'), findsOneWidget);
  });

  testWidgets('重连中的机器不画残留的路径和延迟，写出最近一次失败', (tester) async {
    await _openDetail(tester, SettingsBackend(), 'build-01');

    expect(find.text('Reconnecting…'), findsOneWidget);
    expect(find.text('Path'), findsNothing);
    expect(find.text('Round trip'), findsNothing);
    expect(find.text('Last error'), findsOneWidget);
  });

  testWidgets('被撤销时说明原因，给出重新配对', (tester) async {
    await _openDetail(
      tester,
      SettingsBackend(
        statuses: {
          'rt-mini': const RuntimeStatus(
            state: ConnectionState.disconnected,
            error: SmeltError(kind: SmeltErrorKind.unpaired, message: 'x'),
          ),
        },
      ),
      'home-mini',
    );

    expect(find.textContaining('no longer accepts this phone'), findsOneWidget);
    expect(find.text('Pair again'), findsOneWidget);
    expect(
      find.text('This phone was removed from this machine'),
      findsOneWidget,
    );
  });

  testWidgets('中继设置混用时，iOS 上自建中继的机器写一行说明', (tester) async {
    debugDefaultTargetPlatformOverride = TargetPlatform.iOS;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    final backend = SettingsBackend(
      relays: {'rt-mini': 'https://relay.example'},
    );

    await _openDetail(tester, backend, 'home-mini');
    expect(find.text('https://relay.example'), findsOneWidget);
    expect(find.textContaining('different relay settings'), findsOneWidget);

    // 没指定中继的机器走默认预设，不受影响，不写。
    await tester.pageBack();
    await tester.pumpAndSettle();
    await tester.tap(find.text('MacBook Pro'));
    await tester.pumpAndSettle();
    expect(find.textContaining('different relay settings'), findsNothing);
    debugDefaultTargetPlatformOverride = null;
  });

  testWidgets('改名：保存后机队里和列表里都是新名字', (tester) async {
    final model = await _openDetail(tester, SettingsBackend(), 'MacBook Pro');

    await tester.tap(find.byIcon(LucideIcons.pencil));
    await tester.pumpAndSettle();
    expect(find.text('Rename machine'), findsOneWidget);
    await tester.enterText(find.byType(TextField), '  Work Mac ');
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();

    expect(model.fleet.runtime('rt-mbp')!.record.name, 'Work Mac');
    expect(find.text('Work Mac'), findsWidgets);

    await tester.pageBack();
    await tester.pumpAndSettle();
    expect(find.text('Work Mac'), findsOneWidget);
    expect(find.text('MacBook Pro'), findsNothing);
  });

  testWidgets('改名时清空就用回主机名', (tester) async {
    final backend = SettingsBackend();
    await backend.renameRuntime('rt-mbp', 'Work Mac');
    final model = await _openDetail(tester, backend, 'Work Mac');

    await tester.tap(find.byIcon(LucideIcons.pencil));
    await tester.pumpAndSettle();
    expect(
      find.text('Leave empty to use the host name (MacBook Pro)'),
      findsOneWidget,
    );
    await tester.enterText(find.byType(TextField), '');
    await tester.pump();
    await tester.tap(find.text('Save'));
    await tester.pumpAndSettle();

    expect(model.fleet.runtime('rt-mbp')!.record.name, 'MacBook Pro');
  });

  testWidgets('换标签色：选中的色块打勾，写回机队', (tester) async {
    final model = await _openDetail(tester, SettingsBackend(), 'MacBook Pro');

    // 演示数据里 MacBook Pro 是 0 号色（蓝）。
    expect(
      find.descendant(
        of: _swatch('Blue'),
        matching: find.byIcon(LucideIcons.check),
      ),
      findsOneWidget,
    );

    await tester.tap(_swatch('Pink'));
    await tester.pumpAndSettle();

    expect(model.fleet.runtime('rt-mbp')!.record.accent, 3);
    expect(
      find.descendant(
        of: _swatch('Pink'),
        matching: find.byIcon(LucideIcons.check),
      ),
      findsOneWidget,
    );
    expect(
      find.descendant(
        of: _swatch('Blue'),
        matching: find.byIcon(LucideIcons.check),
      ),
      findsNothing,
    );
    expect(_swatchSelected(tester, 'Pink'), isTrue);
    expect(_swatchSelected(tester, 'Blue'), isFalse);
  });

  testWidgets('忘记机器先确认，说明只删本机记录；取消不动，确认才删', (tester) async {
    final backend = SettingsBackend();
    final model = await _openDetail(tester, backend, 'home-mini');

    await tester.tap(find.text('Forget this machine'));
    await tester.pumpAndSettle();
    expect(find.text('Forget home-mini?'), findsOneWidget);
    expect(find.textContaining('managed on home-mini itself'), findsOneWidget);

    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(backend.forgotten, isEmpty);
    expect(model.fleet.runtime('rt-mini'), isNotNull);

    await tester.tap(find.text('Forget this machine'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Forget'));
    await tester.pumpAndSettle();

    expect(backend.forgotten, ['rt-mini']);
    expect(model.fleet.runtime('rt-mini'), isNull);
    // 退回设置页，列表里已经没有它。
    expect(find.byType(MachineDetailPage), findsNothing);
    expect(find.text('home-mini'), findsNothing);
    expect(find.text('Forgot home-mini'), findsOneWidget);
  });
}

bool _swatchSelected(WidgetTester tester, String label) =>
    (tester.widget(_swatch(label)) as Semantics).properties.selected == true;
