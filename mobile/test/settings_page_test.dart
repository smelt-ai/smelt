import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/runtime_models.dart';
import 'package:smelt_mobile/models/saved_runtime.dart';
import 'package:smelt_mobile/pages/settings_page.dart';
import 'package:smelt_mobile/services/appearance_prefs_store.dart';
import 'package:smelt_mobile/services/terminal_prefs_store.dart';
import 'package:smelt_mobile/theme/smelt_theme.dart';

SavedRuntimeCollection _runtimes(int count) {
  final list = [
    for (var i = 0; i < count; i++)
      SavedRuntime(
        runtimeId: 'r$i',
        name: 'MacBook $i',
        endpointId: 'endpoint-$i',
        lastUsedAt: DateTime(2026, 1, 1),
      ),
  ];
  return SavedRuntimeCollection(
    runtimes: list,
    activeRuntimeId: list.isEmpty ? null : list.first.runtimeId,
  );
}

Widget _host({
  int runtimeCount = 1,
  RuntimeConnectionState state = RuntimeConnectionState.connected,
  LinkMetrics? link,
  ThemeMode themeMode = ThemeMode.system,
  double fontSize = 13,
  ValueChanged<ThemeMode>? onThemeModeChanged,
  ValueChanged<double>? onFontSizeChanged,
  VoidCallback? onDisconnect,
  Widget? connectionBar,
}) {
  return MaterialApp(
    theme: smeltTheme(Brightness.dark),
    home: Scaffold(
      body: SettingsPage(
        runtimes: _runtimes(runtimeCount),
        connectionState: state,
        link: link,
        connectionBar: connectionBar ?? const SizedBox.shrink(),
        themeMode: themeMode,
        onThemeModeChanged: onThemeModeChanged ?? (_) {},
        terminalFontSize: fontSize,
        onTerminalFontSizeChanged: onFontSizeChanged ?? (_) {},
        onSwitchRuntime: () {},
        onPair: () {},
        onDisconnect: onDisconnect ?? () {},
      ),
    ),
  );
}

void main() {
  group('连接卡的链路文案', () {
    test('链路和延迟都有时拼成一个 chip', () {
      expect(
        connectionChipLabel(
          RuntimeConnectionState.connected,
          const LinkMetrics(path: LinkPath.peerToPeer, rttMs: 42),
        ),
        'P2P · 42ms',
      );
    });

    // 认不出链路时不要编一个「未知」——那对用户没有信息量。
    test('链路认不出但有延迟时只画延迟', () {
      expect(
        connectionChipLabel(
          RuntimeConnectionState.connected,
          const LinkMetrics(path: LinkPath.unknown, rttMs: 42),
        ),
        '42ms',
      );
      expect(
        connectionPathDetail(
          RuntimeConnectionState.connected,
          const LinkMetrics(path: LinkPath.unknown, rttMs: 42),
        ),
        isNull,
      );
    });

    test('什么都没有时整个 chip 不画', () {
      expect(
        connectionChipLabel(
          RuntimeConnectionState.connected,
          const LinkMetrics(path: LinkPath.unknown),
        ),
        isNull,
      );
      expect(
        connectionChipLabel(RuntimeConnectionState.connected, null),
        isNull,
      );
    });

    // 没连上的时候延迟是上一次的残值，画出来等于骗人。
    test('没连上时不画链路信息', () {
      const link = LinkMetrics(path: LinkPath.peerToPeer, rttMs: 42);
      expect(
        connectionChipLabel(RuntimeConnectionState.disconnected, link),
        isNull,
      );
      expect(
        connectionPathDetail(RuntimeConnectionState.reconnecting, link),
        isNull,
      );
    });

    test('走中继时说清楚是中继', () {
      expect(
        connectionPathDetail(
          RuntimeConnectionState.connected,
          const LinkMetrics(path: LinkPath.relay),
        ),
        'Forwarded by iroh relay',
      );
    });

    test('局域网直连标成 LAN', () {
      const link = LinkMetrics(path: LinkPath.lan, rttMs: 3);
      expect(
        connectionChipLabel(RuntimeConnectionState.connected, link),
        'LAN · 3ms',
      );
      expect(
        connectionPathDetail(RuntimeConnectionState.connected, link),
        'Direct over LAN',
      );
    });
  });

  group('SettingsPage', () {
    testWidgets('画出连接和外观两组，不画尚未落地的通知和语言', (tester) async {
      await tester.pumpWidget(_host());
      expect(find.text('Connection'), findsOneWidget);
      expect(find.text('Appearance'), findsOneWidget);
      // 分片 2 / 分片 6 落地前，这两组不该出现——点不动的开关比没有更糟。
      expect(find.text('Notifications'), findsNothing);
      expect(find.text('Language'), findsNothing);
    });

    // 卡片本身就是连接状态，正常联通时再叠一条状态条是同一信息画两遍。
    testWidgets('正常联通时不叠加连接状态条，异常时才画', (tester) async {
      const bar = Text('BAR-SENTINEL');
      await tester.pumpWidget(_host(connectionBar: bar));
      expect(find.text('BAR-SENTINEL'), findsNothing);
      await tester.pumpWidget(
        _host(state: RuntimeConnectionState.reconnecting, connectionBar: bar),
      );
      expect(find.text('BAR-SENTINEL'), findsOneWidget);
    });

    testWidgets('只有一个运行时时不给「切换运行时」', (tester) async {
      await tester.pumpWidget(_host());
      expect(find.text('Switch runtime'), findsNothing);
      await tester.pumpWidget(_host(runtimeCount: 2));
      expect(find.text('Switch runtime'), findsOneWidget);
    });

    testWidgets('连接卡画出运行时名和链路', (tester) async {
      await tester.pumpWidget(
        _host(link: const LinkMetrics(path: LinkPath.relay, rttMs: 80)),
      );
      expect(find.text('MacBook 0'), findsOneWidget);
      expect(find.text('Relay · 80ms'), findsOneWidget);
      expect(find.text('Forwarded by iroh relay'), findsOneWidget);
    });

    testWidgets('配对入口说清楚去哪儿拿配对码', (tester) async {
      await tester.pumpWidget(_host());
      expect(find.text('Pair with runtime'), findsOneWidget);
      expect(find.text(pairingGuidance), findsOneWidget);
    });

    testWidgets('已断开时不给「断开连接」', (tester) async {
      await tester.pumpWidget(
        _host(state: RuntimeConnectionState.disconnected),
      );
      expect(find.text('Disconnect'), findsNothing);
      await tester.pumpWidget(_host());
      expect(find.text('Disconnect'), findsOneWidget);
    });

    testWidgets('主题分段控件反映当前值并能切换', (tester) async {
      ThemeMode? picked;
      await tester.pumpWidget(
        _host(themeMode: ThemeMode.dark, onThemeModeChanged: (m) => picked = m),
      );
      final segmented = tester.widget<SegmentedButton<ThemeMode>>(
        find.byType(SegmentedButton<ThemeMode>),
      );
      expect(segmented.selected, {ThemeMode.dark});
      await tester.tap(find.text('Light'));
      await tester.pumpAndSettle();
      expect(picked, ThemeMode.light);
    });

    testWidgets('字号标题显示当前档位', (tester) async {
      await tester.pumpWidget(_host(fontSize: 16));
      expect(find.text('Terminal font size · 16pt'), findsOneWidget);
    });

    // 连续滑杆会对着远端狂发 resize，所以只能落在 TerminalPrefs.steps 上。
    testWidgets('字号滑杆只吐得出合法档位', (tester) async {
      final seen = <double>[];
      await tester.pumpWidget(_host(onFontSizeChanged: seen.add));
      final slider = find.byType(Slider);
      await tester.drag(slider, const Offset(200, 0));
      await tester.pump();
      await tester.drag(slider, const Offset(-400, 0));
      await tester.pump();
      expect(seen, isNotEmpty);
      for (final size in seen) {
        expect(TerminalPrefs.steps, contains(size));
      }
    });
  });

  group('AppearancePrefs', () {
    test('认不出的主题名退回跟随系统', () {
      expect(
        AppearancePrefs.fromJson({'themeMode': 'neon'}).themeMode,
        ThemeMode.system,
      );
      expect(AppearancePrefs.fromJson({}).themeMode, ThemeMode.system);
      expect(
        AppearancePrefs.fromJson({'themeMode': 'dark'}).themeMode,
        ThemeMode.dark,
      );
    });

    test('存下去的主题读得回来', () async {
      final dir = await Directory.systemTemp.createTemp('appearance-prefs');
      addTearDown(() => dir.delete(recursive: true));
      final store = FileAppearancePrefsStore(
        directoryProvider: () async => dir,
      );
      expect((await store.load()).themeMode, ThemeMode.system);
      await store.save(const AppearancePrefs(themeMode: ThemeMode.light));
      expect((await store.load()).themeMode, ThemeMode.light);
    });

    test('读不出来时不炸，退回默认', () async {
      final store = FileAppearancePrefsStore(
        directoryProvider: () async => throw const FileSystemException('nope'),
      );
      expect((await store.load()).themeMode, ThemeMode.system);
    });
  });
}
