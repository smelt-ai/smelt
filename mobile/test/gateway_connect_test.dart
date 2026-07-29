import 'dart:async';
import 'dart:io';

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

  test('an iroh pairing dials through the injected tunnel', () async {
    // 真的起一个本地 WebSocket 服务器冒充隧道出口，验证 GatewayService
    // 确实按拿到的端口去连，而不是把 smelt+iroh:// 直接丢给 WebSocket。
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final requestedPaths = <String>[];
    final requestedEndpoints = <String>[];
    unawaited(
      server.first.then((request) {
        requestedPaths.add(request.uri.toString());
        request.response.statusCode = 404;
        request.response.close();
      }),
    );

    final service = GatewayService(
      connectTimeout: const Duration(seconds: 5),
      irohTunnelOpener: (endpointId) async {
        requestedEndpoints.add(endpointId);
        return server.port;
      },
    );

    await service.connect('smelt+iroh://k7d3ffb1c9a24e5f', 'tok');
    await _waitFor(() => requestedPaths.isNotEmpty);

    expect(requestedEndpoints, ['k7d3ffb1c9a24e5f']);
    expect(requestedPaths.single, '/acp/ws?token=tok');

    service.disconnect();
    await server.close(force: true);
  });

  test('a tunnel that never opens does not hang the UI on connecting', () async {
    // 打错的 EndpointId 表现为拨号一直不返回，界面必须能自己退出加载态。
    final service = GatewayService(
      connectTimeout: const Duration(milliseconds: 300),
      irohTunnelOpener: (_) => Completer<int>().future,
    );
    final errors = <String>[];
    final errorSub = service.errorStream.listen(errors.add);

    await service.connect('smelt+iroh://k7d3ffb1c9a24e5f', 'tok');
    await _waitFor(() => service.state == WsState.disconnected);

    expect(errors, isNotEmpty);
    await errorSub.cancel();
  });

  test('without a tunnel opener an iroh pairing fails loudly', () async {
    final service = GatewayService(
      connectTimeout: const Duration(milliseconds: 300),
    );
    final errors = <String>[];
    final errorSub = service.errorStream.listen(errors.add);

    await service.connect('smelt+iroh://k7d3ffb1c9a24e5f', 'tok');
    await _waitFor(() => service.state == WsState.disconnected);

    expect(errors, isNotEmpty);
    await errorSub.cancel();
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
