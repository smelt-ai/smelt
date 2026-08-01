import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/main.dart';
import 'package:smelt_mobile/models/pairing_config.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/services/pairing_storage.dart';

class MemoryPairingStorage implements PairingStorage {
  PairingConfig? value;

  @override
  Future<void> clear() async => value = null;

  @override
  Future<PairingConfig?> load() async => value;

  @override
  Future<void> save(PairingConfig pairing) async => value = pairing;
}

void main() {
  test(
    'session list uses ACP conversation text instead of agent CLI label',
    () {
      const session = SessionSummary(
        id: 'acp-1',
        title: '修复移动端项目列表',
        phase: 'idle',
        agent: 'codex',
        detail: '  正在检查列表数据  ',
      );

      expect(sessionListTitle(session), '修复移动端项目列表');
      expect(sessionListSubtitle(session), '正在检查列表数据');
    },
  );

  test('session list omits empty detail and has an ACP fallback title', () {
    const session = SessionSummary(
      id: 'acp-2',
      title: '  ',
      phase: 'idle',
      agent: 'other',
      detail: ' ',
    );

    expect(sessionListTitle(session), 'ACP conversation');
    expect(sessionListSubtitle(session), isNull);
  });

  test('session filters separate actionable and running conversations', () {
    const approval = SessionSummary(
      id: 'approval',
      title: 'Approve command',
      phase: 'awaiting_approval',
      status: 'waiting_approval',
      agent: 'codex',
    );
    const running = SessionSummary(
      id: 'running',
      title: 'Implement feature',
      phase: 'running',
      status: 'running',
      agent: 'codex',
    );
    const idle = SessionSummary(
      id: 'idle',
      title: 'Finished task',
      phase: 'idle',
      agent: 'codex',
    );
    const failed = SessionSummary(
      id: 'failed',
      title: 'Failed task',
      phase: 'failed',
      status: 'done',
      agent: 'codex',
      attention: LifecycleAttention(
        sessionId: 'failed',
        title: 'Failed',
        message: 'Agent stopped',
        kind: 'failure',
      ),
    );
    const sessions = [approval, running, idle, failed];

    expect(
      filterSessions(sessions, SessionListFilter.attention).map((s) => s.id),
      ['approval', 'failed'],
    );
    expect(
      filterSessions(sessions, SessionListFilter.running).map((s) => s.id),
      ['running'],
    );
    expect(filterSessions(sessions, SessionListFilter.all), sessions);
  });

  testWidgets('session filter bar fits a compact phone width', (tester) async {
    await tester.binding.setSurfaceSize(const Size(320, 160));
    addTearDown(() => tester.binding.setSurfaceSize(null));

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SessionFilterBar(
            selected: SessionListFilter.attention,
            attentionCount: 123,
            runningCount: 45,
            allCount: 234,
            onChanged: (_) {},
          ),
        ),
      ),
    );

    expect(find.text('Action 99+'), findsOneWidget);
    expect(find.text('Running 45'), findsOneWidget);
    expect(find.text('All 99+'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  test('message auto-follow only continues at the bottom', () {
    expect(isNearMessageBottom(0, 0), isTrue);
    expect(isNearMessageBottom(48, 0), isTrue);
    expect(isNearMessageBottom(49, 0), isFalse);
    expect(
      shouldAutoFollowSnapshot(initialLoad: true, wasAtBottom: false),
      isTrue,
    );
    expect(
      shouldAutoFollowSnapshot(initialLoad: false, wasAtBottom: true),
      isTrue,
    );
    expect(
      shouldAutoFollowSnapshot(initialLoad: false, wasAtBottom: false),
      isFalse,
    );
  });

  testWidgets('shows pairing controls while disconnected', (tester) async {
    await tester.pumpWidget(SmeltApp(pairingStorage: MemoryPairingStorage()));
    await tester.pumpAndSettle();

    expect(find.text('Not connected'), findsOneWidget);
    expect(find.text('Pairing Code'), findsOneWidget);
    expect(find.text('Gateway Endpoint'), findsNothing);
    expect(find.text('Token'), findsNothing);
    expect(find.text('Connect'), findsOneWidget);
    expect(find.text('Scan QR Code to Pair'), findsOneWidget);

    await tester.pumpWidget(const SizedBox.shrink());
  });
}
