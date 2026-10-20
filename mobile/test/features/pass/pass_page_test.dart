import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/preferences.dart';
import 'package:smelt_mobile/data/stores.dart';
import 'package:smelt_mobile/features/pass/pass_page.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';
import 'package:smelt_mobile/theme/galley.dart';
import 'package:smelt_mobile/theme/theme.dart';
import 'package:smelt_mobile/ui/adaptive.dart';

Future<AppModel> _pump(WidgetTester tester, {DemoBackend? backend}) async {
  final model = AppModel(backend ?? DemoBackend(), PreferencesStore.memory())
    ..start();
  addTearDown(model.dispose);
  tester.view.physicalSize = const Size(1170, 2532);
  tester.view.devicePixelRatio = 3;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(
    AppScope(
      model: model,
      child: MaterialApp(
        theme: buildGalleyTheme(Brightness.light),
        locale: const Locale('zh'),
        localizationsDelegates: L10n.localizationsDelegates,
        supportedLocales: L10n.supportedLocales,
        home: Builder(
          builder: (context) => DetailHost(
            open: (key, builder) => Navigator.of(
              context,
            ).push(MaterialPageRoute<void>(builder: builder)),
            selectedKey: null,
            child: const Scaffold(body: PassPage()),
          ),
        ),
      ),
    ),
  );
  await tester.pump();
  await tester.pump();
  return model;
}

void main() {
  testWidgets('跨机器的单据挂在轨上，灶台按机器分组', (tester) async {
    await _pump(tester);
    expect(find.text('修复 markdown 链接解析'), findsWidgets);
    expect(find.text('接入 sing-box 路由'), findsWidgets);
    // 单据头写着机器名；灶台标题也是机器名。
    expect(find.text('MacBook Pro'), findsWidgets);
    expect(find.text('home-mini'), findsWidgets);
    // 离线的机器诚实地写出数据时间（在第一屏下面，滚过去看）。
    await tester.scrollUntilVisible(
      find.textContaining('重连中… · 显示的是'),
      300,
      scrollable: find.byType(Scrollable).first,
    );
    expect(find.textContaining('重连中… · 显示的是'), findsOneWidget);
  });

  testWidgets('在单据上点允许：调内核、盖章、单据离轨', (tester) async {
    final model = await _pump(tester);
    expect(model.pass.value.tickets, hasLength(3));

    await tester.tap(find.text('允许').first);
    await tester.pump(const Duration(milliseconds: 300));
    expect(model.pass.value.tickets, hasLength(2));
    expect(find.textContaining('已允许'), findsOneWidget);

    await tester.pump(
      Motion.stamp + Motion.ticket + const Duration(milliseconds: 100),
    );
    expect(find.textContaining('已允许'), findsNothing);
  });

  testWidgets('点单据进逐张处理，处理完自动到下一张', (tester) async {
    final model = await _pump(tester);
    await tester.tap(find.text('修复 markdown 链接解析').first);
    await tester.pumpAndSettle();
    expect(find.text('第 1 / 3 张'), findsOneWidget);
    // 完整版列出全部选项。
    expect(find.text('总是允许'), findsOneWidget);
    // 长单据上印着会话最近几条：你说了什么、它刚做了什么，批之前看得到来龙去脉。
    expect(find.textContaining('链接地址里带括号时解析错了'), findsOneWidget);
    expect(find.textContaining('nested_parens_in_url'), findsOneWidget);

    await tester.tap(find.text('允许一次'));
    await tester.pump(const Duration(milliseconds: 300));
    await tester.pump(Motion.stamp);
    await tester.pumpAndSettle();
    expect(model.pass.value.tickets, hasLength(2));
    expect(find.text('第 2 / 3 张'), findsOneWidget);
  });

  testWidgets('没有机器时只谈怎么配对', (tester) async {
    final backend = DemoBackend();
    for (final id in ['rt-mbp', 'rt-mini', 'rt-build']) {
      await backend.forgetRuntime(id);
    }
    await _pump(tester, backend: backend);
    expect(find.text('配对第一台机器'), findsOneWidget);
    expect(find.text('扫描配对码'), findsOneWidget);
  });
}
