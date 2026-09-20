import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/session_filters.dart';
import 'package:smelt_mobile/pages/console_page.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/services/pending_actions_controller.dart';
import 'package:smelt_mobile/theme/smelt_theme.dart';
import 'package:smelt_mobile/widgets/session_row.dart';

SessionSummary _session(
  String id, {
  String phase = 'idle',
  String status = 'idle',
  int updatedAt = 1000,
  String? title,
  String? projectTitle,
  String? agentDefinitionId,
  bool unread = false,
}) => SessionSummary(
  id: id,
  title: title ?? id,
  phase: phase,
  status: status,
  agent: 'pi',
  updatedAt: updatedAt,
  unread: unread,
  projectTitle: projectTitle,
  projectRoot: projectTitle == null ? null : '/tmp/$projectTitle',
  agentDefinitionId: agentDefinitionId,
);

PendingActionsController _controller(List<SessionSummary> sessions) =>
    PendingActionsController(
      sessions: const Stream.empty(),
      initialSessions: sessions,
      snapshotUpdates: const Stream.empty(),
      lookupSnapshot: (_) => null,
      requestDetails: (_) => true,
    );

Widget _host(PendingActionsController controller) => MaterialApp(
  theme: smeltTheme(Brightness.dark),
  home: Scaffold(
    body: ConsolePage(onOpenSession: (_) {}, controller: controller),
  ),
);

void main() {
  group('指挥台展示全部对话', () {
    test('闲置会话不再从指挥台上消失', () {
      // 从没跑过的对话：四段谓词一个都不认，以前等于不存在。
      final fresh = _session('a');
      expect(sessionNeedsAction(fresh), isFalse);
      expect(sessionIsRunning(fresh), isFalse);
      expect(sessionRecentlyDone(fresh), isFalse);
      expect(sessionRecentlyRan(fresh), isFalse);
      expect(sessionIsIdle(fresh), isTrue);
    });

    test('有事的会话不会同时落进闲置段', () {
      expect(sessionIsIdle(_session('a', phase: 'running')), isFalse);
      expect(sessionIsIdle(_session('b', status: 'needs_you')), isFalse);
      expect(sessionIsIdle(_session('c', phase: 'succeeded')), isFalse);
      expect(
        sessionIsIdle(_session('d', phase: 'succeeded', unread: true)),
        isFalse,
      );
    });

    test('从没跑过的会话照样进闲置段', () {
      // 名册以菜单为准之后，「没有活动时间」的常态是「桌面开着但这次还没说过
      // 话」。挡掉它们，刚重启桌面的用户打开指挥台会看到一整块空白。
      expect(sessionIsIdle(_session('term', updatedAt: 0)), isTrue);
      expect(sessionIsIdle(_session('acp', updatedAt: 0)), isTrue);
      expect(sessionIsIdle(_session('used', updatedAt: 1)), isTrue);
    });

    test('一个都没跑过时指挥台仍然列出全部会话', () {
      final controller = _controller([
        _session('a', updatedAt: 0),
        _session('b', updatedAt: 0),
      ]);
      addTearDown(controller.dispose);

      expect(controller.idle.map((s) => s.id), ['a', 'b']);
    });

    test('同为零时间的会话回落到名册序，而不是随机顺序', () {
      final controller = _controller([
        _session('never-b', updatedAt: 0, title: 'b'),
        _session('ran', updatedAt: 5),
        _session('never-a', updatedAt: 0, title: 'a'),
      ]);
      addTearDown(controller.dispose);

      // 跑过的排在最前（时间倒序），没跑过的沉底并按名册序稳定排列。
      expect(controller.idle.map((s) => s.id), [
        'ran',
        'never-a',
        'never-b',
      ]);
    });

    test('每一段都按最后活动时间倒序', () {
      final controller = _controller([
        _session('old', phase: 'running', updatedAt: 100),
        _session('newest', phase: 'running', updatedAt: 900),
        _session('mid', phase: 'running', updatedAt: 500),
        _session('idle-old', updatedAt: 10),
        _session('idle-new', updatedAt: 800),
      ]);
      addTearDown(controller.dispose);

      // 原来 running 段沿用网关的项目序——那是给项目树用的，在跨项目的分诊
      // 列表里没有意义。
      expect(controller.running.map((s) => s.id), [
        'newest',
        'mid',
        'old',
      ]);
      expect(controller.idle.map((s) => s.id), ['idle-new', 'idle-old']);
    });

    testWidgets('闲置的智能体对话出现在指挥台上', (tester) async {
      final controller = _controller([
        _session('chat', title: '周报怎么写', agentDefinitionId: 'agent-1'),
      ]);
      addTearDown(controller.dispose);
      await tester.pumpWidget(_host(controller));

      expect(find.text('IDLE'), findsOneWidget);
      expect(find.text('周报怎么写'), findsOneWidget);
    });

    testWidgets('项目会话和智能体对话同屏时各自带得出归属', (tester) async {
      final controller = _controller([
        _session('proj', title: '修登录', projectTitle: 'smelt', updatedAt: 20),
        _session(
          'chat',
          title: '周报怎么写',
          agentDefinitionId: 'agent-1',
          updatedAt: 10,
        ),
      ]);
      addTearDown(controller.dispose);
      await tester.pumpWidget(_host(controller));

      expect(find.text('smelt'), findsOneWidget);
      // 目录还没拉到时退回通用标签，绝不把 definition id 印到界面上。
      expect(find.text('Agent'), findsOneWidget);
      expect(find.textContaining('agent-1'), findsNothing);
    });
  });

  group('会话行的智能体归属', () {
    Widget row(SessionSummary session, {String? agentName}) => MaterialApp(
      theme: smeltTheme(Brightness.dark),
      home: Scaffold(
        body: SessionRow(
          session: session,
          onTap: () {},
          showProject: true,
          agentName: agentName,
        ),
      ),
    );

    testWidgets('配得上名字时显示智能体名', (tester) async {
      await tester.pumpWidget(
        row(
          _session('chat', title: '周报怎么写', agentDefinitionId: 'agent-1'),
          agentName: '工作助手',
        ),
      );
      expect(find.text('工作助手'), findsOneWidget);
    });

    testWidgets('普通项目会话不受影响', (tester) async {
      await tester.pumpWidget(
        row(
          _session('proj', title: '修登录', projectTitle: 'smelt'),
          agentName: '工作助手',
        ),
      );
      expect(find.text('smelt'), findsOneWidget);
      expect(find.text('工作助手'), findsNothing);
    });
  });

  test('会话摘要解析网关下发的智能体归属', () {
    final session = SessionSummary.fromJson({
      'id': 'acp-1',
      'title': '周报怎么写',
      'phase': 'idle',
      'agent': 'pi',
      'agent_definition_id': 'agent-1',
    });
    expect(session.agentDefinitionId, 'agent-1');
    expect(session.isAgentConversation, isTrue);

    // 自动化 Run 有自己的来源标注，不算智能体对话。
    final run = SessionSummary.fromJson({
      'id': 'acp-automation-1',
      'title': '汇总昨日 PR',
      'phase': 'running',
      'agent': 'pi',
      'agent_definition_id': 'agent-1',
      'automation': {
        'automation_id': 'automation-1',
        'automation_name': '每日晨报',
        'run_id': 'run-1',
        'run_status': 'running',
        'run_source': 'scheduled',
      },
    });
    expect(run.isAgentConversation, isFalse);
  });
}
