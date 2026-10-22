import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/features/conversation/conversation_page.dart';
import 'package:smelt_mobile/features/home_shell.dart';
import 'package:smelt_mobile/features/machines/machine_detail_page.dart';
import 'package:smelt_mobile/features/sessions/sessions_page.dart';

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
}
