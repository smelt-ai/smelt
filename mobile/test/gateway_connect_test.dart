import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/services/gateway_service.dart';

void main() {
  test(
    'an unreachable gateway gives up instead of hanging on connecting',
    () async {
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
    },
  );

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
    final requestedRelays = <String>[];
    unawaited(
      server.first.then((request) {
        requestedPaths.add(request.uri.toString());
        request.response.statusCode = 404;
        request.response.close();
      }),
    );

    final service = GatewayService(
      connectTimeout: const Duration(seconds: 5),
      irohTunnelOpener: (endpointId, relayUrl, relayToken) async {
        requestedEndpoints.add(endpointId);
        requestedRelays.add('$relayUrl|$relayToken');
        return server.port;
      },
    );

    await service.connect(
      'smelt+iroh://k7d3ffb1c9a24e5f?relay=https%3A%2F%2Frelay.test&relay_token=secret',
      'tok',
    );
    await _waitFor(() => requestedPaths.isNotEmpty);

    expect(requestedEndpoints, ['k7d3ffb1c9a24e5f']);
    expect(requestedRelays, ['https://relay.test|secret']);
    expect(requestedPaths.single, '/acp/ws?token=tok');

    service.disconnect();
    await server.close(force: true);
  });

  test(
    'a tunnel that never opens does not hang the UI on connecting',
    () async {
      // 打错的 EndpointId 表现为拨号一直不返回，界面必须能自己退出加载态。
      final service = GatewayService(
        connectTimeout: const Duration(milliseconds: 300),
        irohTunnelOpener: (_, _, _) => Completer<int>().future,
      );
      final errors = <String>[];
      final errorSub = service.errorStream.listen(errors.add);

      await service.connect(
        'smelt+iroh://k7d3ffb1c9a24e5f?relay=https%3A%2F%2Frelay.test',
        'tok',
      );
      await _waitFor(() => service.state == WsState.disconnected);

      expect(errors, isNotEmpty);
      await errorSub.cancel();
    },
  );

  test('without a tunnel opener an iroh pairing fails loudly', () async {
    final service = GatewayService(
      connectTimeout: const Duration(milliseconds: 300),
    );
    final errors = <String>[];
    final errorSub = service.errorStream.listen(errors.add);

    await service.connect(
      'smelt+iroh://k7d3ffb1c9a24e5f?relay=https%3A%2F%2Frelay.test',
      'tok',
    );
    await _waitFor(() => service.state == WsState.disconnected);

    expect(errors, isNotEmpty);
    await errorSub.cancel();
  });

  test('iroh reconnect replaces a stale local tunnel', () async {
    final firstServer = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final secondServer = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final firstSocketReady = Completer<WebSocket>();
    final secondSocketReady = Completer<WebSocket>();

    firstServer.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      firstSocketReady.complete(socket);
      socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
    });
    secondServer.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      secondSocketReady.complete(socket);
      socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
    });

    var tunnelPort = firstServer.port;
    var stopCount = 0;
    final openedPorts = <int>[];
    final service = GatewayService(
      connectTimeout: const Duration(seconds: 2),
      reconnectDelay: const Duration(milliseconds: 20),
      irohTunnelOpener: (_, _, _) async {
        openedPorts.add(tunnelPort);
        return tunnelPort;
      },
      irohTunnelStopper: () async {
        stopCount++;
        tunnelPort = secondServer.port;
      },
    );

    await service.connect(
      'smelt+iroh://peer?relay=https%3A%2F%2Frelay.test',
      'tok',
    );
    final firstSocket = await firstSocketReady.future;
    await _waitFor(() => service.state == WsState.connected);

    await firstSocket.close();
    await secondSocketReady.future;
    await _waitFor(() => service.state == WsState.connected);

    expect(stopCount, 1);
    expect(openedPorts, [firstServer.port, secondServer.port]);

    service.disconnect();
    await firstServer.close(force: true);
    await secondServer.close(force: true);
  });

  test('reports the selected iroh path and end-to-end latency', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final socketReady = Completer<WebSocket>();
    final listenerReady = Completer<StreamSubscription<dynamic>>();
    var pingCount = 0;
    server.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      socketReady.complete(socket);
      socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
      listenerReady.complete(
        socket.listen((data) {
          final message = jsonDecode(data as String) as Map<String, dynamic>;
          if (message['method'] != 'ping') return;
          pingCount++;
          final params = message['params'] as Map<String, dynamic>;
          socket.add(
            jsonEncode({'type': 'pong', 'sentAtMs': params['sentAtMs']}),
          );
        }),
      );
    });

    final service = GatewayService(
      connectTimeout: const Duration(seconds: 2),
      metricsInterval: const Duration(milliseconds: 50),
      irohTunnelOpener: (_, _, _) async => server.port,
      irohPathProbe: () async =>
          const IrohPathSample(kind: ConnectionPathKind.p2p, rttMs: 17),
    );

    await service.connect(
      'smelt+iroh://peer?relay=https%3A%2F%2Frelay.test',
      'tok',
    );
    final socket = await socketReady.future;
    await _waitFor(
      () =>
          service.metrics.kind == ConnectionPathKind.p2p &&
          service.metrics.latencyMs != null &&
          pingCount > 0,
    );

    expect(service.metrics.kind, ConnectionPathKind.p2p);
    expect(service.metrics.latencyMs, greaterThanOrEqualTo(0));

    service.disconnect();
    await (await listenerReady.future).cancel();
    await socket.close();
    await server.close(force: true);
  });

  test('old gateways fall back to QUIC RTT without showing an error', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final socketReady = Completer<WebSocket>();
    final listenerReady = Completer<StreamSubscription<dynamic>>();
    server.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      socketReady.complete(socket);
      socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
      listenerReady.complete(
        socket.listen((data) {
          final message = jsonDecode(data as String) as Map<String, dynamic>;
          if (message['method'] == 'ping') {
            socket.add(
              jsonEncode({'type': 'error', 'error': 'invalid request'}),
            );
          }
        }),
      );
    });

    final errors = <String>[];
    final service = GatewayService(
      connectTimeout: const Duration(seconds: 2),
      metricsInterval: const Duration(milliseconds: 50),
      irohTunnelOpener: (_, _, _) async => server.port,
      irohPathProbe: () async =>
          const IrohPathSample(kind: ConnectionPathKind.lan, rttMs: 17),
    );
    final errorSub = service.errorStream.listen(errors.add);

    await service.connect(
      'smelt+iroh://peer?relay=https%3A%2F%2Frelay.test',
      'tok',
    );
    final socket = await socketReady.future;
    await _waitFor(
      () =>
          service.metrics.kind == ConnectionPathKind.lan &&
          service.metrics.latencyMs == 17,
    );
    await Future<void>.delayed(const Duration(milliseconds: 100));

    expect(errors, isEmpty);
    expect(service.metrics.latencyMs, 17);

    service.disconnect();
    await errorSub.cancel();
    await (await listenerReady.future).cancel();
    await socket.close();
    await server.close(force: true);
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
