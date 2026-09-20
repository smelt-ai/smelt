import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/main.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/theme/smelt_theme.dart';

Future<void> _pump(
  WidgetTester tester, {
  required WsState state,
  required bool cached,
  VoidCallback? onRetry,
}) {
  return tester.pumpWidget(
    MaterialApp(
      theme: smeltTheme(Brightness.dark),
      home: Scaffold(
        body: buildConnectionBanner(
          state: state,
          cached: cached,
          cachedAt: DateTime.now(),
          onRetry: onRetry,
        ),
      ),
    ),
  );
}

void main() {
  // `LAN · 4 ms` 这类链路遥测的去处是设置页的连接卡。设计稿全篇只有 F 出现过这个
  // chip，指挥台 / 项目 / 会话三屏都不该常驻一条。
  testWidgets('正常联通时整条横幅不占位', (tester) async {
    await _pump(tester, state: WsState.connected, cached: false);
    expect(find.byType(CachedConnectionBar), findsNothing);
    final size = tester.getSize(find.byType(SizedBox).first);
    expect(size.height, 0);
  });

  testWidgets('断线时仍然画，并带重试入口', (tester) async {
    var retried = false;
    await _pump(
      tester,
      state: WsState.disconnected,
      cached: true,
      onRetry: () => retried = true,
    );
    expect(find.byType(CachedConnectionBar), findsOneWidget);
    expect(find.textContaining('Offline'), findsOneWidget);
    await tester.tap(find.byType(TextButton));
    expect(retried, isTrue);
  });

  testWidgets('重连中仍然画', (tester) async {
    await _pump(tester, state: WsState.reconnecting, cached: true);
    expect(find.textContaining('Reconnecting'), findsOneWidget);
  });

  // 联通了但看的是缓存，用户有权知道自己读的不是最新的。
  testWidgets('联通但在用缓存时仍然画', (tester) async {
    await _pump(tester, state: WsState.connected, cached: true);
    expect(find.byType(CachedConnectionBar), findsOneWidget);
  });
}
