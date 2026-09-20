import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/acp_snapshot.dart';
import 'package:smelt_mobile/models/session_filters.dart';
import 'package:smelt_mobile/pages/console_page.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/services/pending_actions_controller.dart';
import 'package:smelt_mobile/theme/smelt_theme.dart';
import 'package:smelt_mobile/widgets/session_row.dart';

const _source = AutomationSource(
  automationId: 'automation-1',
  automationName: 'Daily digest',
  runId: 'run-1',
  runStatus: 'awaiting_approval',
  runSource: 'scheduled',
  prompt: 'Summarise yesterday\'s pull requests',
  startedAt: 1,
);

SessionSummary _runSession({AutomationSource? automation = _source}) =>
    SessionSummary(
      id: 'acp-automation-1',
      title: 'Daily digest · Morning agent',
      phase: 'awaiting_approval',
      status: 'waiting_approval',
      agent: 'pi',
      cwd: '/home/me/.smelt/workspaces/automations/automation-1',
      updatedAt: 1,
      automation: automation,
    );

const _managedWorkspace =
    '/home/me/.smelt/workspaces/automations/497d023dc33a0a893b4cb7a28208e598';

AcpSnapshot _pushApproval() => AcpSnapshot(
  entries: const [],
  phase: const AcpPhaseAwaitingApproval(),
  pendingPermissions: const [
    PendingPermission(
      toolCallId: 'tool-1',
      question: 'Run git push?',
      details: ApprovalDetailsCommand(
        command: 'git push origin main',
        cwd: _managedWorkspace,
      ),
      options: [
        PermissionOption(optionId: 'allow', name: 'Allow', kind: 'AllowOnce'),
        PermissionOption(optionId: 'deny', name: 'Deny', kind: 'RejectOnce'),
      ],
    ),
  ],
);

Widget _host(Widget child) =>
    MaterialApp(theme: smeltTheme(Brightness.dark), home: Scaffold(body: child));

void main() {
  group('automation projection', () {
    test('会话摘要带上来源、触发方式和这次固化的输入', () {
      final summary = SessionSummary.fromJson({
        'id': 'acp-automation-1',
        'kind': 'acp',
        'title': 'Daily digest · Morning agent',
        'phase': 'awaiting_approval',
        'status': 'waiting_approval',
        'agent': 'pi',
        'automation': {
          'automation_id': 'automation-1',
          'automation_name': 'Daily digest',
          'run_id': 'run-1',
          'run_status': 'awaiting_approval',
          'run_source': 'scheduled',
          'prompt': 'Summarise yesterday\'s pull requests',
          'started_at': 1,
        },
      });

      final automation = summary.automation;
      expect(automation, isNotNull);
      expect(automation!.automationName, 'Daily digest');
      expect(automation.runSource, 'scheduled');
      expect(automation.prompt, 'Summarise yesterday\'s pull requests');
    });

    test('普通会话没有来源字段，不被误判成自动化', () {
      final summary = SessionSummary.fromJson({
        'id': 'acp-1',
        'title': 'fix login',
        'phase': 'idle',
        'agent': 'codex',
      });
      expect(summary.automation, isNull);
    });

    // Run 的工作区是 daemon 分配的目录。放进项目树会按 cwd 兜底出一个用户从没
    // 打开过的「项目」——那正是这条谓词存在的唯一理由。
    test('自动化 Run 不进项目树，普通会话进', () {
      expect(sessionBelongsToProjectTree(_runSession()), isFalse);
      expect(
        sessionBelongsToProjectTree(_runSession(automation: null)),
        isTrue,
      );
    });
  });

  group('console', () {
    testWidgets('审批卡说明是哪条自动化、怎么触发的、这次输入是什么', (tester) async {
      final sessions = StreamController<List<SessionSummary>>.broadcast();
      addTearDown(sessions.close);
      final snapshots = StreamController<String>.broadcast();
      addTearDown(snapshots.close);
      final controller = PendingActionsController(
        sessions: sessions.stream,
        initialSessions: [_runSession()],
        snapshotUpdates: snapshots.stream,
        lookupSnapshot: (_) => _pushApproval(),
        requestDetails: (_) => true,
      );
      addTearDown(controller.dispose);

      await tester.pumpWidget(
        _host(
          ConsolePage(
            controller: controller,
            onOpenSession: (_) {},
            onRespond: (_, _, _) {},
          ),
        ),
      );
      await tester.pump();

      expect(find.text('Run git push?'), findsOneWidget);
      expect(
        find.text('Daily digest'),
        findsOneWidget,
        reason: '无人值守时用户不知道这条会话是谁起的，来源必须在卡片上',
      );
      expect(find.text('Scheduled'), findsOneWidget);
      expect(
        find.textContaining('Summarise yesterday', findRichText: true),
        findsOneWidget,
        reason: '只给「它想执行 X」等于让用户盲批，这次运行的输入要一起给到',
      );
      // 托管工作区是 daemon 按 id 哈希出来的目录，用户没打开过、也判断不了危险，
      // 却要在窄屏上占三行把「允许 / 拒绝」推出首屏。
      expect(
        find.textContaining(_managedWorkspace),
        findsNothing,
        reason: '自动化 Run 的托管工作区路径不该占掉卡片正文',
      );
    });

    // 标题是 agent 自己起的，单看认不出这条是不是我开的。
    testWidgets('自动化 Run 的行标出是哪条自动化起的', (tester) async {
      await tester.pumpWidget(
        _host(
          SessionRow(
            session: _runSession(),
            onTap: () {},
            showProject: true,
          ),
        ),
      );

      expect(find.textContaining('Daily digest'), findsWidgets);
      expect(find.byIcon(Icons.settings_suggest_outlined), findsOneWidget);
      expect(
        find.textContaining('workspaces'),
        findsNothing,
        reason: 'daemon 工作区路径对用户没有意义，不该出现在行上',
      );
    });
  });
}
