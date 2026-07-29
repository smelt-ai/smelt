import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/main.dart';
import 'package:smelt_mobile/models/pairing_config.dart';
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
    expect(find.text('Connect'), findsOneWidget);
    expect(find.text('Scan QR Code to Pair'), findsOneWidget);

    await tester.pumpWidget(const SizedBox.shrink());
  });
}
