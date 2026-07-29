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
    expect(summary.attention?.sessionId, 'session-1');
    expect(summary.attention?.requiresAction, isFalse);
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
