import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:smelt_mobile/pages/agents_page.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/util/relative_time.dart';
import 'package:smelt_mobile/widgets/automation_source.dart';

AutomationSummary _automation({
  String id = 'automation-1',
  String name = 'Morning digest',
  bool enabled = true,
  List<AutomationSchedule> schedules = const [
    AutomationSchedule(type: 'daily', hour: 9, minute: 0),
  ],
  List<String> eventTopics = const [],
  bool webhook = false,
  String actionKind = 'agent',
  String? agentName = 'Digest agent',
  int? nextRunAt,
  AutomationRunSummary? lastRun,
}) => AutomationSummary(
  id: id,
  name: name,
  enabled: enabled,
  schedules: schedules,
  eventTopics: eventTopics,
  webhook: webhook,
  actionKind: actionKind,
  agentName: agentName,
  nextRunAt: nextRunAt,
  lastRun: lastRun,
);

Widget _host(Widget child) =>
    MaterialApp(home: Scaffold(body: SizedBox(height: 800, child: child)));

Widget _view({
  AutomationCatalog? catalog,
  void Function(String, bool)? onSetEnabled,
  void Function(String)? onRunOnce,
  void Function(String)? onOpenSession,
  Set<String> startableAgentIds = const {},
  void Function(String)? onStartConversation,
}) => _host(
  AgentsView(
    catalog: catalog,
    onRefresh: () {},
    onSetEnabled: onSetEnabled ?? (_, _) {},
    onRunOnce: onRunOnce ?? (_) {},
    onOpenSession: onOpenSession ?? (_) {},
    startableAgentIds: startableAgentIds,
    onStartConversation: onStartConversation,
  ),
);

void main() {
  test('automation summary parses the sanitized gateway payload', () {
    final summary = AutomationSummary.fromJson({
      'id': 'automation-1',
      'name': '每日晨报',
      'enabled': true,
      'schedules': [
        {'type': 'weekly', 'days': 0x1f, 'hour': 9, 'minute': 30},
      ],
      'event_topics': ['deploy'],
      'webhook': true,
      'action_kind': 'agent',
      'agent_name': '晨报助手',
      'next_run_at': 1000,
      'last_run': {
        'run_id': 'run-1',
        'status': 'awaiting_approval',
        'source': 'scheduled',
        'started_at': 900,
        'session_id': 'acp-automation-1',
      },
    });

    expect(summary.schedules.single.days, 0x1f);
    // 混排的时机全部列出来，不折叠——手机上要能一眼核对「它什么时候跑」。
    expect(
      automationTriggerSummary(summary),
      'Weekdays 09:30 · On deploy · External trigger',
    );
    expect(summary.lastRun!.sessionId, 'acp-automation-1');
  });

  test('schedule labels cover every desktop schedule shape', () {
    expect(
      automationScheduleLabel(
        const AutomationSchedule(type: 'daily', hour: 7, minute: 5),
      ),
      'Daily 07:05',
    );
    expect(
      automationScheduleLabel(
        const AutomationSchedule(type: 'every_minutes', minutes: 15),
      ),
      'Every 15 min',
    );
    expect(
      automationScheduleLabel(
        const AutomationSchedule(type: 'every_hours', hours: 2),
      ),
      'Every 2 h',
    );
    // bit0 = 周一，与桌面 SCHEDULE_DAY_* 对齐；全选说成 Daily 而不是罗列七天。
    expect(
      automationScheduleLabel(
        const AutomationSchedule(
          type: 'weekly',
          days: 0x7f,
          hour: 9,
          minute: 0,
        ),
      ),
      'Daily 09:00',
    );
    expect(
      automationScheduleLabel(
        const AutomationSchedule(
          type: 'weekly',
          days: 0x05,
          hour: 9,
          minute: 0,
        ),
      ),
      'Mon Wed 09:00',
    );
  });

  test('upcoming timestamps never collapse into "just now"', () {
    final now = DateTime.fromMillisecondsSinceEpoch(1_000_000_000 * 1000);
    expect(formatUpcomingEpochSeconds(1_000_003_600, now: now), 'in 1h');
    expect(formatUpcomingEpochSeconds(1_000_000_600, now: now), 'in 10m');
    // 调度器还没醒时说 due，而不是谎报一个未来时间。
    expect(formatUpcomingEpochSeconds(999_999_000, now: now), 'due');
    expect(formatUpcomingEpochSeconds(0, now: now), isNull);
  });

  testWidgets('a missing catalog is not shown as an empty catalog', (
    tester,
  ) async {
    await tester.pumpWidget(_view(catalog: null));

    expect(find.text('Syncing with your desktop'), findsOneWidget);
    expect(find.text('No automations yet'), findsNothing);
  });

  testWidgets('the automation row states when, who and the last result', (
    tester,
  ) async {
    await tester.pumpWidget(
      _view(
        catalog: AutomationCatalog(
          automations: [
            _automation(
              lastRun: const AutomationRunSummary(
                runId: 'run-1',
                status: 'awaiting_approval',
                source: 'scheduled',
                startedAt: 900,
                sessionId: 'acp-automation-1',
              ),
            ),
          ],
        ),
      ),
    );

    expect(find.text('Morning digest'), findsOneWidget);
    expect(find.text('Daily 09:00 → Digest agent'), findsOneWidget);
    expect(find.textContaining('Awaiting approval'), findsOneWidget);
    expect(find.byType(Switch), findsOneWidget);
  });

  testWidgets('the toggle sets an explicit value instead of flipping', (
    tester,
  ) async {
    final calls = <(String, bool)>[];
    await tester.pumpWidget(
      _view(
        catalog: AutomationCatalog(automations: [_automation()]),
        onSetEnabled: (id, enabled) => calls.add((id, enabled)),
      ),
    );

    await tester.tap(find.byType(Switch));
    await tester.pump();

    expect(calls, [('automation-1', false)]);
  });

  testWidgets('a deleted agent definition keeps the automation visible', (
    tester,
  ) async {
    await tester.pumpWidget(
      _view(
        catalog: AutomationCatalog(
          automations: [_automation(agentName: null)],
        ),
      ),
    );

    expect(find.text('Daily 09:00 → Missing agent'), findsOneWidget);
  });

  testWidgets('details offer run now and jumping into the last run', (
    tester,
  ) async {
    final runs = <String>[];
    final opened = <String>[];
    await tester.pumpWidget(
      _view(
        catalog: AutomationCatalog(
          automations: [
            _automation(
              lastRun: const AutomationRunSummary(
                runId: 'run-1',
                status: 'failed',
                source: 'scheduled',
                finishedAt: 900,
                sessionId: 'acp-automation-1',
                error: 'agent exited with status 1',
              ),
            ),
          ],
        ),
        onRunOnce: runs.add,
        onOpenSession: opened.add,
      ),
    );

    await tester.tap(find.text('Morning digest'));
    await tester.pumpAndSettle();
    expect(find.text('agent exited with status 1'), findsOneWidget);

    await tester.tap(find.text('Open last run'));
    await tester.pumpAndSettle();
    expect(opened, ['acp-automation-1']);

    await tester.tap(find.text('Morning digest'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Run now'));
    await tester.pumpAndSettle();
    expect(runs, ['automation-1']);
  });

  testWidgets('the agents segment opens a read-only definition page', (
    tester,
  ) async {
    await tester.pumpWidget(
      _view(
        catalog: const AutomationCatalog(
          agents: [
            AgentDefinitionSummary(
              id: 'agent-1',
              name: 'Digest agent',
              agentId: 'pi',
              prompt: 'Summarise yesterday and flag blockers.',
              plugins: ['skill:research'],
              contextFolders: ['/home/me/notes'],
            ),
          ],
        ),
      ),
    );

    await tester.tap(find.text('Agents'));
    await tester.pumpAndSettle();
    // 单数不能说成 "1 plugins"。
    expect(find.text('pi · 1 plugin · 1 folder'), findsOneWidget);

    await tester.tap(find.text('Digest agent'));
    await tester.pumpAndSettle();

    expect(
      find.text('Summarise yesterday and flag blockers.'),
      findsOneWidget,
    );
    expect(find.text('/home/me/notes'), findsOneWidget);
    // 手机是展示层：定义页不能出现任何写入口。
    expect(find.byType(TextField), findsNothing);
    expect(
      find.text('Read-only on mobile. Edit this agent on the desktop.'),
      findsOneWidget,
    );
  });

  group('starting a conversation from the Agents tab', () {
    final catalog = AutomationCatalog(
      automations: const [],
      agents: const [
        AgentDefinitionSummary(
          id: 'agent-1',
          name: 'Digest agent',
          agentId: 'pi',
          prompt: '',
        ),
        AgentDefinitionSummary(
          id: 'agent-2',
          name: 'Claude agent',
          agentId: 'claude',
          prompt: '',
        ),
      ],
    );

    Future<void> openAgentsSegment(WidgetTester tester) async {
      await tester.tap(find.text('Agents'));
      await tester.pumpAndSettle();
    }

    testWidgets('only agents the workspace can start get the entry', (
      tester,
    ) async {
      await tester.pumpWidget(
        _view(
          catalog: catalog,
          startableAgentIds: const {'agent-1'},
          onStartConversation: (_) {},
        ),
      );
      await openAgentsSegment(tester);

      // 引擎没注册成产品级引擎的智能体在桌面也开不了对话，手机上不能给它一个
      // 按下去只会报错的按钮。
      expect(find.byIcon(Icons.chat_bubble_outline), findsOneWidget);
    });

    testWidgets('the entry is hidden while the workspace catalog is missing', (
      tester,
    ) async {
      await tester.pumpWidget(
        _view(catalog: catalog, onStartConversation: (_) {}),
      );
      await openAgentsSegment(tester);

      expect(find.byIcon(Icons.chat_bubble_outline), findsNothing);
    });

    testWidgets('tapping the entry starts that agent, not the row it sits on', (
      tester,
    ) async {
      final started = <String>[];
      await tester.pumpWidget(
        _view(
          catalog: catalog,
          startableAgentIds: const {'agent-1', 'agent-2'},
          onStartConversation: started.add,
        ),
      );
      await openAgentsSegment(tester);

      await tester.tap(find.byIcon(Icons.chat_bubble_outline).last);
      await tester.pumpAndSettle();

      expect(started, ['agent-2']);
      // 开对话不能顺带把配置页推上来。
      expect(find.text('Working style'), findsNothing);
    });

    testWidgets('the row itself still opens the read-only config', (
      tester,
    ) async {
      final started = <String>[];
      await tester.pumpWidget(
        _view(
          catalog: catalog,
          startableAgentIds: const {'agent-1'},
          onStartConversation: started.add,
        ),
      );
      await openAgentsSegment(tester);

      await tester.tap(find.text('Digest agent'));
      await tester.pumpAndSettle();

      expect(started, isEmpty);
      expect(find.text('Start conversation'), findsOneWidget);

      await tester.tap(find.text('Start conversation'));
      await tester.pumpAndSettle();

      // 配置页开完对话要退回去：新会话会接管整个 home，返回键不该把用户丢回
      // 一个跟当前对话无关的配置页。
      expect(started, ['agent-1']);
      expect(find.text('Digest agent'), findsOneWidget);
      expect(find.text('Start conversation'), findsNothing);
    });
  });
}
