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
