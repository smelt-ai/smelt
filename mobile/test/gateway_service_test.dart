import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/services/gateway_service.dart';

void main() {
  test('SessionSummary parses canonical lifecycle status and attention', () {
    final summary = SessionSummary.fromJson({
      'id': 'session-1',
      'title': 'Codex',
      'phase': 'succeeded',
      'status': 'done',
      'agent': 'codex',
      'cwd': '/tmp/smelt',
      'project_root': '/tmp/smelt',
      'project_title': 'smelt',
      'project_order': 2,
      'session_order': 4,
      'leaf_order': 1,
      'updated_at': 42,
      'detail': 'Task completed',
      'unread': true,
      'attention': {
        'sessionId': 'session-1',
        'title': 'Completed',
        'message': 'Task completed',
        'kind': 'success',
      },
    });

    expect(summary.phase, 'succeeded');
    expect(summary.status, 'done');
    expect(summary.unread, isTrue);
    expect(summary.projectRoot, '/tmp/smelt');
    expect(summary.projectOrder, 2);
    expect(summary.attention?.sessionId, 'session-1');
    expect(summary.attention?.requiresAction, isFalse);
    expect(summary.kind, SessionKind.acp);
  });

  test(
    'SessionSummary preserves terminal kind through the disk cache shape',
    () {
      final summary = SessionSummary.fromJson({
        'id': 'terminal-1',
        'kind': 'terminal',
        'title': 'Codex CLI',
        'phase': 'thinking',
        'agent': 'codex',
      });

      expect(summary.kind, SessionKind.terminal);
      expect(summary.toJson()['kind'], 'terminal');
    },
  );

  test('session menu order follows project then PC session order', () {
    SessionSummary session(String id, int project, int session) {
      return SessionSummary(
        id: id,
        title: id,
        phase: 'idle',
        agent: 'codex',
        projectOrder: project,
        sessionOrder: session,
      );
    }

    final sessions = [
      session('project-two', 1, 0),
      session('project-one-second', 0, 2),
      session('project-one-first', 0, 1),
    ]..sort(compareSessionMenuOrder);

    expect(sessions.map((item) => item.id), [
      'project-one-first',
      'project-one-second',
      'project-two',
    ]);
  });

  test('approval attention requires action', () {
    final attention = LifecycleAttention.fromJson({
      'sessionId': 'session-2',
      'title': 'Approval required',
      'message': 'Run command?',
      'kind': 'approval',
    });

    expect(attention.requiresAction, isTrue);
  });

  test('workspace and history models parse server-owned session metadata', () {
    final project = WorkspaceProject.fromJson({
      'root': '/repo/smelt',
      'title': 'smelt',
      'order': 2,
    });
    final agent = AcpAgentOption.fromJson({
      'id': 'profile:quant',
      'kind': 'claude',
      'label': 'Claude Quant',
      'profile': true,
    });
    final history = HistorySessionSummary.fromJson({
      'resumeId': 'history-1',
      'title': 'Fix mobile history',
      'lastActiveAt': '2026-07-31T12:00:00Z',
      'messageCount': 8,
    });

    expect(project.root, '/repo/smelt');
    expect(agent.profile, isTrue);
    expect(agent.kind, 'claude');
    expect(history.resumeId, 'history-1');
    expect(history.lastActiveAt?.toUtc().hour, 12);
    expect(history.messageCount, 8);
  });

  group('LaunchAction', () {
    Map<String, dynamic> row(String key, String section, String target) => {
      'key': key,
      'label': key,
      'section': section,
      'target': target,
      'kind': target == 'conversation' ? 'conversation' : 'terminal',
      'pinned': section == 'common',
    };

    test('分组和 target 原样来自电脑端，手机不重新归类', () {
      final actions = [
        row('terminal:blank', 'common', 'blankTerminal'),
        row('terminal:claude:claude', 'terminal', 'terminal'),
        row('conversation:codex', 'conversation', 'conversation'),
      ].map(LaunchAction.fromJson).nonNulls.toList();

      final catalog = WorkspaceCatalog(
        projects: const [],
        agents: const [],
        launchActions: actions,
      );

      expect(
        catalog.actionsIn(LaunchSection.common).single.target,
        LaunchTarget.blankTerminal,
      );
      expect(
        catalog.actionsIn(LaunchSection.terminal).single.key,
        'terminal:claude:claude',
      );
      final conversation = catalog.actionsIn(LaunchSection.conversation).single;
      expect(conversation.isConversation, isTrue);
      expect(conversation.kindLabel, 'Conversation');
    });

    test('认不出的分组或 target 直接丢掉，不猜', () {
      expect(LaunchAction.fromJson(row('x', 'favourites', 'terminal')), isNull);
      expect(LaunchAction.fromJson(row('x', 'common', 'wormhole')), isNull);
      expect(LaunchAction.fromJson(row('', 'common', 'terminal')), isNull);
    });

    test('画图标用的 agent 认 agentKind，也认终端的 provider', () {
      final conversation = LaunchAction.fromJson({
        ...row('conversation:codex', 'conversation', 'conversation'),
        'agentKind': 'codex',
      })!;
      final terminal = LaunchAction.fromJson({
        ...row('terminal:claude:claude', 'terminal', 'terminal'),
        'provider': 'claude',
      })!;
      expect(conversation.agent, 'codex');
      expect(terminal.agent, 'claude');
      expect(
        LaunchAction.fromJson(row('terminal:blank', 'common', 'blankTerminal'))!
            .agent,
        isEmpty,
      );
    });
  });
}
