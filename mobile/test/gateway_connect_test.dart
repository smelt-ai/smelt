import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/services/gateway_service.dart';

void main() {
  test('an unreachable gateway gives up instead of hanging on connecting', () async {
    // 192.0.2.0/24 is TEST-NET-1: routable-looking but black-holed, which is
    // exactly the "typo'd IP" case where the WebSocket handshake never
    // resolves either way.
    final service = GatewayService(
      connectTimeout: const Duration(milliseconds: 300),
    );
    final states = <WsState>[];
    final errors = <String>[];
    final stateSub = service.stateStream.listen(states.add);
    final errorSub = service.errorStream.listen(errors.add);

    await service.connect('ws://192.0.2.1:9877', 'irrelevant-token');
    await _waitFor(() => service.state == WsState.disconnected);

    expect(states, contains(WsState.connecting));
    expect(service.state, WsState.disconnected);
    expect(states, isNot(contains(WsState.reconnecting)));
    expect(errors, isNotEmpty);

    await stateSub.cancel();
    await errorSub.cancel();
  });

  test('cancelling while connecting returns to disconnected', () async {
    final service = GatewayService(connectTimeout: const Duration(seconds: 30));
    unawaited(service.connect('ws://192.0.2.1:9877', 'irrelevant-token'));
    await _waitFor(() => service.state == WsState.connecting);

    service.disconnect();

    expect(service.state, WsState.disconnected);
  });
}

Future<void> _waitFor(bool Function() predicate) async {
  final deadline = DateTime.now().add(const Duration(seconds: 15));
  while (DateTime.now().isBefore(deadline)) {
    if (predicate()) return;
    await Future<void>.delayed(const Duration(milliseconds: 50));
  }
  fail('condition was not met within the timeout');
}
