import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/stores.dart';
import 'package:smelt_mobile/features/conversation/conversation_page.dart';
import 'package:smelt_mobile/features/home_shell.dart';
import 'package:smelt_mobile/features/machines/machine_detail_page.dart';
import 'package:smelt_mobile/features/sessions/sessions_page.dart';
import 'package:smelt_mobile/ui/glass_nav_bar.dart';
import 'package:smelt_mobile/ui/page.dart';

import 'conversation/conversation_harness.dart';

/// 平板横屏：列表栏 + 详情栏。
void _useTablet(WidgetTester tester) {
  tester.view.physicalSize = const Size(1194 * 2, 834 * 2);
  tester.view.devicePixelRatio = 2;
  addTearDown(tester.view.reset);
}

/// 演示数据里在线机器上最近动过的会话（MacBook Pro 上，42 秒前）。
const _latestTitle = '修复 markdown 链接解析';

Future<void> _settle(WidgetTester tester) async {
  for (var i = 0; i < 5; i++) {
    await tester.pump(const Duration(milliseconds: 200));
  }
}

Future<void> _tapNav(WidgetTester tester, String label) async {
  await tester.tap(
    find.descendant(
      of: find.byType(NavigationRail),
      matching: find.text(label),
    ),
  );
  await _settle(tester);
}

Future<void> _tapRow(WidgetTester tester, String title) async {
  await tester.tap(
    find.descendant(of: find.byType(SessionsPage), matching: find.text(title)),
  );
  await _settle(tester);
}

int _count(RecordingBackend backend, String call) =>
    backend.calls.where((c) => c == call).length;

/// 推一条「首页文案改版已完成」的提醒。正文是真实数据没标题时的兜底。
Future<void> _attention(WidgetTester tester, AppModel model) async {
  model.catalog.apply(
    const CatalogEvent(
      runtimeId: 'rt-mbp',
      kind: CatalogEventKind.attention,
      attention: AttentionItem(
        sessionId: 's-copy',
        title: '已完成',
        message: '会话 s-copy',
        kind: AttentionKind.success,
      ),
    ),
  );
  await _settle(tester);
}

void main() {
  testWidgets('平板：点默认详情里正显示的会话、再点已选中的会话，页面原样留着不重订', (tester) async {
    _useTablet(tester);
    final backend = RecordingBackend();
    await tester.pumpWidget(host(startModel(backend), const HomeShell()));
    await _settle(tester);

    await _tapNav(tester, 'Sessions');
    expect(find.byType(ConversationPage), findsOneWidget);
    expect(_count(backend, 'watch s-md'), 1, reason: '默认详情显示最近的会话');

    await _tapRow(tester, _latestTitle);
    expect(_count(backend, 'watch s-md'), 1, reason: '默认详情就是它：不重建');
    expect(_count(backend, 'unwatch s-md'), 0);

    await _tapRow(tester, _latestTitle);
    expect(_count(backend, 'watch s-md'), 1, reason: '已选中的再点一次：不重建');
    expect(_count(backend, 'unwatch s-md'), 0);

    await _tapRow(tester, '首页文案改版');
    expect(_count(backend, 'watch s-copy'), 1);
    expect(_count(backend, 'unwatch s-md'), 1, reason: '换了会话，旧页退订');

    await tester.pumpWidget(const SizedBox.shrink());
    await _settle(tester);
  });

  testWidgets('平板：打开过详情再切 tab，详情栏回到那个 tab 的默认内容', (tester) async {
    _useTablet(tester);
    final backend = RecordingBackend();
    await tester.pumpWidget(host(startModel(backend), const HomeShell()));
    await _settle(tester);

    await _tapNav(tester, 'Sessions');
    await _tapRow(tester, '首页文案改版');
    expect(_count(backend, 'watch s-copy'), 1);

    await _tapNav(tester, 'Settings');
    expect(
      find.byType(ConversationPage),
      findsNothing,
      reason: '上一个 tab 的详情不留着',
    );
    expect(find.byType(MachineDetailPage), findsOneWidget);
    expect(_count(backend, 'unwatch s-copy'), 1);

    await _tapNav(tester, 'Sessions');
    expect(find.byType(ConversationPage), findsOneWidget);
    expect(_count(backend, 'watch s-md'), 2, reason: '默认内容重新放上来');

    await tester.pumpWidget(const SizedBox.shrink());
    await _settle(tester);
  });

  testWidgets('手机：提醒提示条显示会话标题，点「打开」压进对话页', (tester) async {
    usePhone(tester);
    final backend = RecordingBackend();
    final model = startModel(backend);
    await tester.pumpWidget(host(model, const HomeShell()));
    await _settle(tester);

    await tester.tap(
      find.descendant(
        of: find.byType(GlassNavBar),
        matching: find.text('Sessions'),
      ),
    );
    await _settle(tester);
    await _attention(tester, model);

    expect(find.text('MacBook Pro · 首页文案改版 · 已完成'), findsOneWidget);
    expect(find.byType(ConversationPage), findsNothing);

    await tester.tap(find.widgetWithText(SnackBarAction, 'Open'));
    await _settle(tester);
    expect(find.byType(ConversationPage), findsOneWidget);
    expect(_count(backend, 'watch s-copy'), 1);

    await tester.pumpWidget(const SizedBox.shrink());
    await _settle(tester);
  });

  testWidgets('平板：提醒提示条点「打开」放进详情栏', (tester) async {
    _useTablet(tester);
    final backend = RecordingBackend();
    final model = startModel(backend);
    await tester.pumpWidget(host(model, const HomeShell()));
    await _settle(tester);

    await _tapNav(tester, 'Sessions');
    await _attention(tester, model);
    await tester.tap(find.widgetWithText(SnackBarAction, 'Open'));
    await _settle(tester);

    expect(_count(backend, 'watch s-copy'), 1);
    expect(_count(backend, 'unwatch s-md'), 1, reason: '换掉默认详情');
    expect(find.byType(ConversationPage), findsOneWidget);

    await tester.pumpWidget(const SizedBox.shrink());
    await _settle(tester);
  });

  testWidgets('手机：内容伸到玻璃底栏下面，页面底部留白让开它', (tester) async {
    usePhone(tester);
    tester.view.padding = const FakeViewPadding(bottom: 34 * 3);
    tester.view.viewPadding = const FakeViewPadding(bottom: 34 * 3);
    final model = startModel(RecordingBackend());
    await tester.pumpWidget(host(model, const HomeShell()));
    await _settle(tester);

    final screen = tester.getSize(find.byType(HomeShell));
    final bar = tester.getRect(find.byType(GlassNavBar));
    final page = find.byType(GalleyPage).first;
    expect(tester.getRect(page).bottom, screen.height, reason: '页面铺到屏幕底，滚到胶囊下面');
    expect(
      MediaQuery.paddingOf(tester.element(page)).bottom,
      greaterThanOrEqualTo(screen.height - bar.top),
      reason: '最后一行能滚出胶囊',
    );

    await tester.pumpWidget(const SizedBox.shrink());
    await _settle(tester);
  });
}
