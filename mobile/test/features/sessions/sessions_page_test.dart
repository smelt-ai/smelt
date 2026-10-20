import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/features/sessions/history_page.dart';
import 'package:smelt_mobile/features/sessions/sessions_page.dart';

import 'harness.dart';

SessionSummary _extra(
  String id,
  String title, {
  String? definition,
  AutomationSource? automation,
}) => SessionSummary(
  id: id,
  kind: SessionKind.acp,
  title: title,
  phase: SessionPhase.succeeded,
  status: SessionStatus.idle,
  agent: 'pi',
  cwd: '/Users/demo/spaces/$id',
  projectOrder: 0xFFFFFFFF,
  updatedAt: 0,
  agentDefinitionId: definition,
  automation: automation,
);

TestBackend _backend() => TestBackend(
  extra: {
    'rt-mbp': [
      _extra('s-agent', '本周周报', definition: 'def-assistant'),
      _extra(
        's-auto',
        '汇总昨日 PR',
        automation: const AutomationSource(
          automationName: '每日站会摘要',
          runStatus: AutomationRunStatus.completed,
          runSource: AutomationRunSource.scheduled,
        ),
      ),
    ],
  },
);

Finder _inDialog(String text) =>
    find.descendant(of: find.byType(AlertDialog), matching: find.text(text));

void main() {
  testWidgets('按机器 → 项目分组；离线机器写明是几点的数据；芯片和外部筛选都能筛', (tester) async {
    final filter = ValueNotifier<String?>(null);
    addTearDown(filter.dispose);
    await pumpHarness(tester, _backend(), SessionsPage(filter: filter));

    // 三台机器的会话都在。
    expect(find.text('多运行时连接状态机'), findsOneWidget); // MacBook Pro
    expect(find.text('整理 0.10 发布说明'), findsOneWidget); // home-mini
    expect(find.text('cargo build --release'), findsOneWidget); // build-01
    // 智能体对话、自动化运行各自成组，智能体名从工作区目录配上。
    expect(find.text('Agent conversations'), findsOneWidget);
    expect(find.text('Automation runs'), findsOneWidget);
    expect(find.textContaining('工作助手'), findsOneWidget);
    expect(find.textContaining('每日站会摘要'), findsOneWidget);
    // 项目组末尾有「历史会话」；工作区里还没开着会话的项目收进一组，直达历史。
    expect(find.text('History'), findsWidgets);
    expect(find.text('Projects without open sessions'), findsWidgets);
    // 重连中的机器：列表来自离线缓存，写明时间（演示数据存于 9 分钟前）。
    expect(
      find.text('Reconnecting… · showing data from 13:51'),
      findsOneWidget,
    );

    // 点芯片筛到 home-mini。
    await tester.tap(find.widgetWithText(ChoiceChip, 'home-mini'));
    await settle(tester);
    expect(filter.value, 'rt-mini');
    expect(find.text('多运行时连接状态机'), findsNothing);
    expect(find.text('整理 0.10 发布说明'), findsOneWidget);

    // 指挥台点「全部会话」从外面设筛选。
    filter.value = 'rt-build';
    await settle(tester);
    expect(find.text('整理 0.10 发布说明'), findsNothing);
    expect(find.text('cargo build --release'), findsOneWidget);

    await tester.tap(find.widgetWithText(ChoiceChip, 'All'));
    await settle(tester);
    expect(filter.value, isNull);
    expect(find.text('多运行时连接状态机'), findsOneWidget);
  });

  testWidgets('搜索标题、项目、agent；没有匹配时说清楚', (tester) async {
    final filter = ValueNotifier<String?>(null);
    addTearDown(filter.dispose);
    await pumpHarness(tester, _backend(), SessionsPage(filter: filter));

    await tester.enterText(find.byType(TextField), 'velum');
    await settle(tester);
    expect(find.text('接入 sing-box 路由'), findsOneWidget);
    expect(find.text('多运行时连接状态机'), findsNothing);
    // 搜索时不出「历史会话」行。
    expect(find.text('History'), findsNothing);

    await tester.enterText(find.byType(TextField), 'copilot');
    await settle(tester);
    expect(find.text('整理 0.10 发布说明'), findsOneWidget);
    expect(find.text('接入 sing-box 路由'), findsNothing);

    await tester.enterText(find.byType(TextField), 'zzz');
    await settle(tester);
    expect(find.text('Nothing matches “zzz”'), findsOneWidget);

    await tester.tap(find.byTooltip('Clear search'));
    await settle(tester);
    expect(find.text('多运行时连接状态机'), findsOneWidget);
  });

  testWidgets('长按删除要先确认；取消不删，确认才删', (tester) async {
    final backend = _backend();
    final filter = ValueNotifier<String?>(null);
    addTearDown(filter.dispose);
    await pumpHarness(tester, backend, SessionsPage(filter: filter));

    Future<void> askToDelete() async {
      await tester.longPress(find.text('整理 workspace 文档'));
      await settle(tester);
      await tester.tap(find.text('Delete'));
      await settle(tester);
      expect(find.text('Delete this conversation?'), findsOneWidget);
    }

    await askToDelete();
    await tester.tap(_inDialog('Cancel'));
    await settle(tester);
    expect(backend.deleted, isEmpty);

    await askToDelete();
    await tester.tap(_inDialog('Delete'));
    await settle(tester);
    expect(backend.deleted, [('rt-mbp', 's-docs')]);
  });

  testWidgets('左滑出删除；终端的确认文案说明会结束进程', (tester) async {
    final backend = _backend();
    final filter = ValueNotifier<String?>(null);
    addTearDown(filter.dispose);
    await pumpHarness(tester, backend, SessionsPage(filter: filter));

    await tester.drag(find.text('zsh'), const Offset(-400, 0));
    await settle(tester);
    await tester.tap(find.text('Delete'));
    await settle(tester);
    expect(find.text('Delete this terminal?'), findsOneWidget);
    await tester.tap(_inDialog('Delete'));
    await settle(tester);
    expect(backend.deleted, [('rt-mbp', 's-zsh')]);
  });

  testWidgets('运行时拒绝删除（自动化运行）时提示原因', (tester) async {
    final backend = _backend()
      ..deleteError = const SmeltError(
        kind: SmeltErrorKind.other,
        message: '自动化运行的会话不能删除',
      );
    final filter = ValueNotifier<String?>(null);
    addTearDown(filter.dispose);
    await pumpHarness(tester, backend, SessionsPage(filter: filter));

    await tester.longPress(find.text('汇总昨日 PR'));
    await settle(tester);
    await tester.tap(find.text('Delete'));
    await settle(tester);
    await tester.tap(_inDialog('Delete'));
    await settle(tester);
    expect(backend.deleted, [('rt-mbp', 's-auto')]);
    expect(find.text("Couldn't delete: 自动化运行的会话不能删除"), findsOneWidget);
    // 行还在。
    expect(find.text('汇总昨日 PR'), findsOneWidget);
  });

  testWidgets('点会话打开它；点「历史会话」打开该项目的历史页', (tester) async {
    final filter = ValueNotifier<String?>('rt-mbp');
    addTearDown(filter.dispose);
    final harness = await pumpHarness(
      tester,
      _backend(),
      SessionsPage(filter: filter),
    );

    await tester.tap(find.text('多运行时连接状态机'));
    await settle(tester);
    expect(harness.opened, ['rt-mbp/s-fleet']);

    await tester.tap(find.text('History').first);
    await settle(tester);
    expect(harness.opened.last, 'history/rt-mbp//Users/demo/code/smelt');
  });

  testWidgets('历史页：选 agent 列出历史，改名后就地更新', (tester) async {
    await pumpHarness(
      tester,
      _backend(),
      const HistoryPage(
        runtimeId: 'rt-mbp',
        projectRoot: '/Users/demo/code/smelt',
        projectTitle: 'smelt',
      ),
    );
    expect(find.text('重构 catalog 投影'), findsOneWidget);
    // 改过名的显示新名，副标题补原名。
    expect(find.text('iroh 重连（已修）'), findsOneWidget);
    expect(find.textContaining('Originally “排查 iroh 重连”'), findsOneWidget);

    await tester.longPress(find.text('重构 catalog 投影'));
    await settle(tester);
    await tester.enterText(
      find.descendant(
        of: find.byType(AlertDialog),
        matching: find.byType(TextField),
      ),
      '新名字',
    );
    await tester.tap(_inDialog('Save'));
    await settle(tester);
    expect(find.text('新名字'), findsOneWidget);
    expect(find.textContaining('Originally “重构 catalog 投影”'), findsOneWidget);
  });
}
