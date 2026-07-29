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
  });

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
}
