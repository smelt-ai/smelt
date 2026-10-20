import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/features/launch/launch_flow.dart';
import 'package:smelt_mobile/features/sessions/sessions_page.dart';

import 'harness.dart';

Finder _inSheet(String text) =>
    find.descendant(of: find.byType(BottomSheet), matching: find.text(text));

void main() {
  testWidgets('新建：选机器 → 项目 → 启动方式，带 requestId 发创建，出现在目录后打开', (tester) async {
    final backend = TestBackend();
    final filter = ValueNotifier<String?>(null);
    addTearDown(filter.dispose);
    final harness = await pumpHarness(
      tester,
      backend,
      SessionsPage(filter: filter),
    );

    await tester.tap(find.byTooltip('New session'));
    await settle(tester);

    // 两台在线，要选机器；重连中的那台不可选并说明原因。
    expect(_inSheet('Choose a machine'), findsOneWidget);
    expect(
      _inSheet('Reconnecting… · available once it reconnects'),
      findsOneWidget,
    );
    await tester.tap(_inSheet('build-01'));
    await settle(tester);
    expect(_inSheet('Choose a machine'), findsOneWidget);

    await tester.tap(_inSheet('MacBook Pro'));
    await settle(tester);
    expect(_inSheet('Choose a project'), findsOneWidget);

    await tester.tap(_inSheet('smelt'));
    await settle(tester);
    expect(_inSheet('Pinned'), findsOneWidget);
    expect(_inSheet('Agents'), findsOneWidget);

    await tester.tap(_inSheet('Codex'));
    // 演示后端模拟了创建延迟，多等一会儿让会话出现在目录里。
    await tester.pump(const Duration(milliseconds: 400));
    await settle(tester);

    final call = backend.created.single;
    expect(call.runtimeId, 'rt-mbp');
    expect(call.projectRoot, '/Users/demo/code/smelt');
    expect(call.launchKey, 'codex');
    expect(call.requestId, isNotNull);
    // 新会话出现在目录里：弹层收起，会话被打开。
    expect(find.byType(BottomSheet), findsNothing);
    expect(harness.opened, ['rt-mbp/demo-new-1']);
  });

  testWidgets('创建失败可重试，重试沿用同一个 requestId', (tester) async {
    final backend = TestBackend()
      ..failCreates = 1
      ..createdNeverAppears = true;
    final filter = ValueNotifier<String?>('rt-mini');
    addTearDown(filter.dispose);
    final harness = await pumpHarness(
      tester,
      backend,
      SessionsPage(filter: filter),
    );

    // 筛到某台在线机器时，直接从选项目开始。
    await tester.tap(find.byTooltip('New session'));
    await settle(tester);
    expect(_inSheet('Choose a machine'), findsNothing);
    expect(_inSheet('Choose a project'), findsOneWidget);

    await tester.tap(_inSheet('velum'));
    await settle(tester);
    await tester.tap(_inSheet('Pi'));
    await settle(tester);

    expect(_inSheet("Couldn't start Pi"), findsOneWidget);
    expect(_inSheet('连接断了'), findsOneWidget);
    await tester.tap(_inSheet('Retry'));
    await settle(tester);

    expect(backend.created, hasLength(2));
    expect(backend.created[1].requestId, backend.created[0].requestId);
    expect(backend.created[1].launchKey, 'pi');

    // 会话一直没出现在目录里：等到超时，弹层收起并说明，不打开。
    await tester.pump(launchListTimeout);
    await settle(tester);
    expect(find.byType(BottomSheet), findsNothing);
    expect(find.textContaining("It started on home-mini"), findsOneWidget);
    expect(harness.opened, isEmpty);
  });

  testWidgets('不选项目：只列产品智能体，按 agentOptionId 开在智能体自己的空间', (tester) async {
    final backend = TestBackend();
    final filter = ValueNotifier<String?>('rt-mbp');
    addTearDown(filter.dispose);
    await pumpHarness(tester, backend, SessionsPage(filter: filter));

    await tester.tap(find.byTooltip('New session'));
    await settle(tester);
    await tester.tap(_inSheet('No project'));
    await settle(tester);
    expect(_inSheet('Pinned'), findsNothing);
    await tester.tap(_inSheet('工作助手'));
    await settle(tester);

    final call = backend.created.single;
    expect(call.projectRoot, '');
    expect(call.agentOptionId, 'assistant');
    expect(call.launchKey, isNull);
  });
}
