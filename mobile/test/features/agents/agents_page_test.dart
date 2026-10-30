import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/preferences.dart';
import 'package:smelt_mobile/data/stores.dart';
import 'package:smelt_mobile/features/agents/agent_edit_page.dart';
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

  /// 能力库读不到：模型、插件、MCP 都是 null，带原因。
  bool choicesUnreadable = false;
  bool memoriesFail = false;

  /// 记忆列表晚回来：等它放行。
  Completer<void>? memoriesGate;

  static const _unread = AgentChoices(error: 'Pi 没装');

  @override
  Future<AgentChoices> agentChoices(String runtimeId) async =>
      choicesUnreadable ? _unread : super.agentChoices(runtimeId);

  @override
  Future<AgentEditor> agentEditor(String runtimeId, {String? agentId}) async {
    final editor = await super.agentEditor(runtimeId, agentId: agentId);
    if (!choicesUnreadable) return editor;
    return AgentEditor(draft: editor.draft, choices: _unread);
  }

  @override
  Future<AgentMemories> agentMemories(String runtimeId, String agentId) async {
    final memories = await super.agentMemories(runtimeId, agentId);
    await memoriesGate?.future;
    if (memoriesFail) {
      throw const SmeltError(kind: SmeltErrorKind.offline, message: '断了');
    }
    return memories;
  }

  final createdAgents = <(String, AgentDraft)>[];
  final updatedAgents = <(String, String, AgentDraft, AgentDraft)>[];
  final deletedAgents = <(String, String)>[];
  Object? deleteAgentError;

  @override
  Future<AgentDefinitionSummary> createAgent(
    String runtimeId,
    AgentDraft draft,
  ) {
    createdAgents.add((runtimeId, draft));
    return super.createAgent(runtimeId, draft);
  }

  @override
  Future<AgentDefinitionSummary> updateAgent(
    String runtimeId,
    String agentId, {
    required AgentDraft original,
    required AgentDraft edited,
  }) {
    updatedAgents.add((runtimeId, agentId, original, edited));
    return super.updateAgent(
      runtimeId,
      agentId,
      original: original,
      edited: edited,
    );
  }

  @override
  Future<void> deleteAgent(String runtimeId, String agentId) async {
    deletedAgents.add((runtimeId, agentId));
    if (deleteAgentError case final error?) throw error;
    await super.deleteAgent(runtimeId, agentId);
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
    // 插件、MCP 和编辑页一样显示名字和说明，不是 id 原文。
    expect(find.text('lark-cli'), findsOneWidget);
    expect(find.text('飞书文档、消息、任务'), findsOneWidget);
    expect(find.text('skill:lark-cli'), findsNothing);
    expect(find.text('notion'), findsOneWidget);
    expect(find.text('笔记'), findsOneWidget);
    expect(find.text('/Users/demo/notes'), findsOneWidget);
    expect(find.text('跟随这台机器的默认模型'), findsOneWidget);
    expect(find.text('1 条待确认'), findsOneWidget);

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

  testWidgets('新建智能体：先选机器，填名字、勾插件，存完出现在列表里', (tester) async {
    final backend = _Backend();
    await _pump(tester, backend);

    await tester.tap(find.byTooltip('新建智能体'));
    await tester.pumpAndSettle();
    // 两台在线：先选建在哪台。
    expect(find.text('建在哪台机器上？'), findsOneWidget);
    await tester.tap(find.text('MacBook Pro').last);
    await tester.pumpAndSettle();

    final save = find.widgetWithText(TextButton, '保存');
    expect(tester.widget<TextButton>(save).onPressed, isNull, reason: '名字空不能存');
    await tester.enterText(find.byType(TextField).first, '调研助手');
    await tester.enterText(find.byType(TextField).at(1), '先查资料再下结论');
    await tester.tap(find.text('lark-cli'));
    await tester.pump();
    // 坏了的插件不能新勾。
    await tester.tap(find.text('notify'));
    await tester.pump();
    await tester.tap(save);
    await tester.pumpAndSettle();

    final (runtimeId, draft) = backend.createdAgents.single;
    expect(runtimeId, 'rt-mbp');
    expect(draft.name, '调研助手');
    expect(draft.prompt, '先查资料再下结论');
    expect(draft.plugins, ['skill:lark-cli']);
    expect(draft.memoryEnabled, isTrue);
    expect(find.text('调研助手'), findsWidgets);
  });

  testWidgets('编辑智能体：改指令后保存，带上打开时的那份；没存就退要确认', (tester) async {
    final backend = _Backend();
    await _pump(tester, backend);
    await tester.tap(find.text('工作助手').first);
    await tester.pumpAndSettle();

    await tester.tap(find.byTooltip('编辑'));
    await tester.pumpAndSettle();
    expect(find.text('编辑智能体'), findsOneWidget);
    await tester.enterText(find.byType(TextField).at(1), '只做开发相关的事。');
    await tester.pump();

    // 有改动时返回先问。
    await tester.binding.handlePopRoute();
    await tester.pumpAndSettle();
    expect(find.text('放弃修改？'), findsOneWidget);
    await tester.tap(find.text('取消'));
    await tester.pumpAndSettle();

    await tester.tap(find.widgetWithText(TextButton, '保存'));
    await tester.pumpAndSettle();
    final (runtimeId, agentId, original, edited) = backend.updatedAgents.single;
    expect((runtimeId, agentId), ('rt-mbp', 'def-assistant'));
    expect(original.prompt, '你是工作助手，负责开发和日常工作处理。');
    expect(edited.prompt, '只做开发相关的事。');
    expect(edited.plugins, original.plugins);
    // 回到详情，看到的是新指令。
    expect(find.text('只做开发相关的事。'), findsOneWidget);
  });

  testWidgets('删除智能体：被拒绝时说原因；删掉后详情一起退', (tester) async {
    final backend = _Backend()
      ..deleteAgentError = const SmeltError(
        kind: SmeltErrorKind.other,
        message: '请先删除该智能体关联的 1 条自动化',
      );
    await _pump(tester, backend);
    await tester.tap(find.text('工作助手').first);
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('编辑'));
    await tester.pumpAndSettle();

    Future<void> deleteIt() async {
      await tester.scrollUntilVisible(
        find.text('删除智能体'),
        300,
        scrollable: find
            .descendant(
              of: find.byType(AgentEditPage),
              matching: find.byType(Scrollable),
            )
            .first,
      );
      await tester.tap(find.text('删除智能体'));
      await tester.pumpAndSettle();
      expect(find.text('删除「工作助手」？'), findsOneWidget);
      await tester.tap(find.widgetWithText(FilledButton, '删除'));
      await tester.pumpAndSettle();
    }

    await deleteIt();
    expect(find.text('没能删除：请先删除该智能体关联的 1 条自动化'), findsOneWidget);
    expect(find.text('编辑智能体'), findsOneWidget, reason: '留在编辑页');

    tester
        .state<ScaffoldMessengerState>(find.byType(ScaffoldMessenger))
        .hideCurrentSnackBar();
    await tester.pumpAndSettle();
    backend.deleteAgentError = null;
    await deleteIt();
    expect(backend.deletedAgents, hasLength(2));
    expect(find.text('编辑智能体'), findsNothing);
    expect(find.text('长期指令'), findsNothing, reason: '详情页也退了');
  });

  testWidgets('记忆：待确认的排最前，采纳后详情里的条数跟着变', (tester) async {
    await _pump(tester, _Backend());
    await tester.tap(find.text('工作助手').first);
    await tester.pumpAndSettle();
    expect(find.text('1 条待确认'), findsOneWidget);

    await tester.tap(find.text('1 条待确认'));
    await tester.pumpAndSettle();
    expect(find.text('工作助手 · 记忆'), findsOneWidget);
    final pending = tester.getTopLeft(find.text('发版窗口')).dy;
    expect(pending, lessThan(tester.getTopLeft(find.text('回复风格')).dy));
    expect(find.text('待确认 · 读过外部网页的那一轮写的'), findsOneWidget);

    await tester.tap(find.text('发版窗口'));
    await tester.pumpAndSettle();
    expect(find.text('周四晚上有例行维护，发版挪到周五上午。'), findsOneWidget);
    await tester.tap(find.text('采纳'));
    await tester.pumpAndSettle();
    expect(find.textContaining('待确认'), findsNothing);

    await tester.binding.handlePopRoute();
    await tester.pumpAndSettle();
    expect(find.text('2 条'), findsOneWidget);
  });

  testWidgets('编辑页：点输入框外面、拖动列表都收起键盘', (tester) async {
    await _pump(tester, _Backend());
    await tester.tap(find.byTooltip('新建智能体'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('MacBook Pro').last);
    await tester.pumpAndSettle();

    await tester.showKeyboard(find.byType(TextField).first);
    expect(tester.testTextInput.isVisible, isTrue);
    await tester.tap(find.text('长期指令'));
    await tester.pumpAndSettle();
    expect(tester.testTextInput.isVisible, isFalse, reason: '点外面收起');

    await tester.showKeyboard(find.byType(TextField).at(1));
    expect(tester.testTextInput.isVisible, isTrue);
    await tester.drag(find.text('模型'), const Offset(0, -200));
    await tester.pumpAndSettle();
    expect(tester.testTextInput.isVisible, isFalse, reason: '拖动列表收起');
  });

  testWidgets('能力库读不到：选了的照原样显示，不说成「已经没有了」', (tester) async {
    await _pump(tester, _Backend()..choicesUnreadable = true);
    await tester.tap(find.text('工作助手').first);
    await tester.pumpAndSettle();

    expect(find.text('skill:lark-cli'), findsOneWidget);
    expect(find.text('notion'), findsOneWidget);
    expect(find.text('这台机器上已经没有了'), findsNothing);

    await tester.tap(find.byTooltip('编辑'));
    await tester.pumpAndSettle();
    expect(find.textContaining('Pi 没装'), findsOneWidget, reason: '编辑页说明原因');
    expect(find.text('这台机器上已经没有了'), findsNothing);
    expect(find.text('skill:lark-cli'), findsOneWidget);
  });

  testWidgets('勾上再取消不算改动：保存不亮、返回不问', (tester) async {
    await _pump(tester, _Backend());
    await tester.tap(find.text('工作助手').first);
    await tester.pumpAndSettle();
    await tester.tap(find.byTooltip('编辑'));
    await tester.pumpAndSettle();

    final save = find.widgetWithText(TextButton, '保存');
    await tester.tap(find.text('lark-cli'));
    await tester.pump();
    expect(tester.widget<TextButton>(save).onPressed, isNotNull);
    await tester.tap(find.text('lark-cli'));
    await tester.pump();
    expect(tester.widget<TextButton>(save).onPressed, isNull);

    await tester.binding.handlePopRoute();
    await tester.pumpAndSettle();
    expect(find.text('放弃修改？'), findsNothing);
    expect(find.text('编辑智能体'), findsNothing);
  });

  testWidgets('记忆没取到时详情里说没取到，不一直转圈', (tester) async {
    await _pump(tester, _Backend()..memoriesFail = true);
    await tester.tap(find.text('工作助手').first);
    await tester.pumpAndSettle();
    expect(find.text('记忆没取到'), findsOneWidget);
  });

  testWidgets('记忆页：晚回来的旧列表不盖掉采纳后的新列表', (tester) async {
    final backend = _Backend();
    await _pump(tester, backend);
    await tester.tap(find.text('工作助手').first);
    await tester.pumpAndSettle();

    // 进记忆页时的那次取列表卡住，先显示详情页带过去的那份。
    backend.memoriesGate = Completer<void>();
    await tester.tap(find.text('1 条待确认'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('发版窗口'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('采纳'));
    await tester.pumpAndSettle();
    expect(find.textContaining('待确认'), findsNothing);

    // 旧的那次取列表现在才回来（演示后端取的是采纳前的快照）。
    backend.memoriesGate!.complete();
    await tester.pumpAndSettle();
    expect(find.textContaining('待确认'), findsNothing);
  });
}
