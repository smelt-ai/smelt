import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/preferences.dart';
import 'package:smelt_mobile/data/stores.dart';
import 'package:smelt_mobile/features/agents/agents_page.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';
import 'package:smelt_mobile/theme/theme.dart';
import 'package:smelt_mobile/ui/adaptive.dart';

/// 演示后端上记下调用，并让开关真的改到目录里（演示后端本身原样返回）。
class _Backend extends DemoBackend {
  final toggles = <(String, String, bool)>[];
  final runs = <(String, String)>[];
  final created = <(String, String, String?, String?)>[];
  final _enabled = <String, bool>{};
  Object? toggleError;
  Completer<void>? toggleGate;

  AutomationSummary _withEnabled(AutomationSummary a, bool enabled) =>
      AutomationSummary(
        id: a.id,
        name: a.name,
        enabled: enabled,
        schedules: a.schedules,
        eventTopics: a.eventTopics,
        webhook: a.webhook,
        action: a.action,
        agentName: a.agentName,
        nextRunAt: a.nextRunAt,
        lastRun: a.lastRun,
      );

  @override
  Future<AutomationCatalog> automationCatalog(String runtimeId) async {
    final c = await super.automationCatalog(runtimeId);
    return AutomationCatalog(
      agents: c.agents,
      automations: [
        for (final a in c.automations)
          _withEnabled(a, _enabled[a.id] ?? a.enabled),
      ],
    );
  }

  @override
  Future<AutomationCatalog> setAutomationEnabled(
    String runtimeId,
    String automationId,
    bool enabled,
  ) async {
    toggles.add((runtimeId, automationId, enabled));
    await toggleGate?.future;
    if (toggleError case final error?) throw error;
    _enabled[automationId] = enabled;
    return automationCatalog(runtimeId);
  }

  @override
  Future<AutomationCatalog> runAutomationOnce(
    String runtimeId,
    String automationId,
  ) async {
    runs.add((runtimeId, automationId));
    return automationCatalog(runtimeId);
  }

  @override
  Future<String> createSession(
    String runtimeId, {
    required String projectRoot,
    String? agentOptionId,
    String? launchKey,
    String? resumeId,
    SessionKind? kind,
    String? requestId,
  }) {
    created.add((runtimeId, projectRoot, agentOptionId, requestId));
    return super.createSession(
      runtimeId,
      projectRoot: projectRoot,
      agentOptionId: agentOptionId,
      requestId: requestId,
    );
  }
}

/// 打开过的详情键。智能体 / 自动化详情真的压栈；会话只记下键（对话页不在本测试范围）。
Future<List<String>> _pump(WidgetTester tester, _Backend backend) async {
  tester.view.physicalSize = const Size(1200, 3000);
  tester.view.devicePixelRatio = 3;
  addTearDown(tester.view.reset);
  final model = AppModel(backend, PreferencesStore.memory())..start();
  final navigator = GlobalKey<NavigatorState>();
  final opened = <String>[];
  void open(String key, WidgetBuilder builder) {
    opened.add(key);
    if (!key.startsWith('agent:') && !key.startsWith('automation:')) return;
    navigator.currentState!.push(
      MaterialPageRoute<void>(
        builder: (_) => DetailHost(
          open: open,
          selectedKey: null,
          child: Builder(builder: builder),
        ),
      ),
    );
  }

  await tester.pumpWidget(
    AppScope(
      model: model,
      child: MaterialApp(
        navigatorKey: navigator,
        theme: buildGalleyTheme(Brightness.light),
        locale: const Locale('zh'),
        localizationsDelegates: L10n.localizationsDelegates,
        supportedLocales: L10n.supportedLocales,
        home: DetailHost(
          open: open,
          selectedKey: null,
          child: const Scaffold(body: AgentsPage()),
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
  return opened;
}

Switch _switchAt(WidgetTester tester, int index) =>
    tester.widget<Switch>(find.byType(Switch).at(index));

void main() {
  testWidgets('按机器分组；分段在智能体与自动化之间切换；离线机器不取', (tester) async {
    await _pump(tester, _Backend());

    // 两台在线机器各一组，离线那台只说离线。
    expect(find.text('MacBook Pro'), findsOneWidget);
    expect(find.text('home-mini'), findsOneWidget);
    expect(find.text('build-01'), findsOneWidget);
    expect(find.text('重连中…'), findsOneWidget);
    expect(find.text('这台机器连上后再显示。'), findsOneWidget);

    expect(find.text('工作助手'), findsNWidgets(2));
    expect(find.text('开发和日常工作处理'), findsNWidgets(2));
    expect(find.text('每日站会摘要'), findsNothing);

    await tester.tap(find.text('自动化'));
    await tester.pumpAndSettle();

    expect(find.text('工作助手'), findsNothing);
    expect(find.text('每日站会摘要'), findsNWidgets(2));
    expect(find.text('夜间备份检查'), findsNWidgets(2));
    // 暂停的写「已暂停」和触发方式，上次失败写出状态。
    expect(find.text('已暂停 · 外部调用（webhook）'), findsNWidgets(2));
    expect(find.textContaining('失败 · '), findsNWidgets(2));
    expect(find.byType(Switch), findsNWidgets(4));
  });

  testWidgets('开关先拨过去，再调 setAutomationEnabled 写明目标值', (tester) async {
    final backend = _Backend()..toggleGate = Completer<void>();
    await _pump(tester, backend);
    await tester.tap(find.text('自动化'));
    await tester.pumpAndSettle();

    expect(_switchAt(tester, 0).value, isTrue);
    await tester.tap(find.byType(Switch).first);
    await tester.pump();

    expect(backend.toggles, [('rt-mbp', 'auto-standup', false)]);
    // 运行时还没回话，开关已经在新位置。
    expect(_switchAt(tester, 0).value, isFalse);

    backend.toggleGate!.complete();
    await tester.pumpAndSettle();
    expect(_switchAt(tester, 0).value, isFalse);
    expect(find.text('已暂停 · 每天 09:30'), findsOneWidget);
  });

  testWidgets('开关失败时回到原位并提示原因', (tester) async {
    final backend = _Backend()
      ..toggleGate = Completer<void>()
      ..toggleError = const SmeltError(
        kind: SmeltErrorKind.offline,
        message: '连不上运行时',
      );
    await _pump(tester, backend);
    await tester.tap(find.text('自动化'));
    await tester.pumpAndSettle();

    await tester.tap(find.byType(Switch).first);
    await tester.pump();
    expect(_switchAt(tester, 0).value, isFalse);

    backend.toggleGate!.complete();
    await tester.pumpAndSettle();
    expect(_switchAt(tester, 0).value, isTrue);
    expect(find.text('没能暂停「每日站会摘要」：连不上运行时'), findsOneWidget);
  });

  testWidgets('自动化详情把日程说成人话，立即运行调 runAutomationOnce', (tester) async {
    final backend = _Backend();
    final opened = await _pump(tester, backend);
    await tester.tap(find.text('自动化'));
    await tester.pumpAndSettle();

    await tester.tap(find.text('每日站会摘要').first);
    await tester.pumpAndSettle();
    expect(opened, ['automation:rt-mbp/auto-standup']);

    expect(find.text('每天 09:30'), findsOneWidget);
    expect(find.text('由智能体「工作助手」执行'), findsOneWidget);
    expect(find.text('最近一次运行'), findsOneWidget);
    expect(find.text('定时'), findsOneWidget);

    await tester.tap(find.text('立即运行一次'));
    await tester.pumpAndSettle();
    expect(backend.runs, [('rt-mbp', 'auto-standup')]);
    expect(find.text('已开始运行「每日站会摘要」'), findsOneWidget);
  });

  testWidgets('自动化详情写出上次运行的错误', (tester) async {
    await _pump(tester, _Backend());
    await tester.tap(find.text('自动化'));
    await tester.pumpAndSettle();

    await tester.tap(find.text('夜间备份检查').first);
    await tester.pumpAndSettle();
    expect(find.text('外部调用（webhook）'), findsOneWidget);
    expect(find.text('运行一条 shell 命令'), findsOneWidget);
    expect(find.text('磁盘空间不足'), findsOneWidget);
    expect(find.text('外部调用'), findsOneWidget);
  });

  testWidgets('智能体详情：开始对话在这台机器上开不绑项目的对话并打开它', (tester) async {
    final backend = _Backend();
    final opened = await _pump(tester, backend);

    await tester.tap(find.text('工作助手').first);
    await tester.pumpAndSettle();
    expect(opened, ['agent:rt-mbp/def-assistant']);
    expect(find.text('长期指令'), findsOneWidget);
    expect(find.text('你是工作助手，负责开发和日常工作处理。'), findsOneWidget);
    expect(find.text('lark-cli'), findsOneWidget);
    expect(find.text('~/notes'), findsOneWidget);

    await tester.tap(find.text('开始对话'));
    await tester.pumpAndSettle();

    expect(backend.created, hasLength(1));
    final (runtimeId, projectRoot, optionId, requestId) =
        backend.created.single;
    expect(runtimeId, 'rt-mbp');
    expect(projectRoot, '');
    // 工作区目录里 agentDefinitionId 对上的那个选项。
    expect(optionId, 'assistant');
    expect(requestId, isNotNull);
    expect(opened.last, 'rt-mbp/demo-new-1');
  });
}
