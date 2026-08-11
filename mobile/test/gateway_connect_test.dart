import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/services/session_cache_store.dart';

void main() {
  test('a message attempted while disconnected fails immediately', () async {
    final service = GatewayService();
    final result = service.messageSendStream.first;

    final requestId = service.sendMessage('session-1', 'hello');

    final failure = await result;
    expect(failure.requestId, requestId);
    expect(failure.ok, isFalse);
    expect(failure.error, contains('not connected'));
  });

  test('correlates a sent message with the desktop acknowledgement', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final received = Completer<Map<String, dynamic>>();
    server.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
      socket.listen((data) {
        final message = jsonDecode(data as String) as Map<String, dynamic>;
        if (message['method'] != 'sendMessage') return;
        received.complete(message);
        final params = message['params'] as Map<String, dynamic>;
        socket.add(
          jsonEncode({
            'type': 'messageSent',
            'ok': true,
            'requestId': params['requestId'],
          }),
        );
      });
    });
    final service = GatewayService();
    final result = service.messageSendStream.first;

    await service.connect('http://127.0.0.1:${server.port}', 'token');
    await _waitFor(() => service.state == WsState.connected);
    final requestId = service.sendMessage('session-1', 'hello');

    final request = await received.future;
    expect(request['params'], containsPair('requestId', requestId));
    expect((await result).requestId, requestId);

    service.disconnect();
    await server.close(force: true);
  });

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

  test('restores saved sessions before an offline desktop times out', () async {
    final directory = await Directory.systemTemp.createTemp(
      'smelt-gateway-cache-',
    );
    addTearDown(() => directory.delete(recursive: true));
    final store = FileSessionCacheStore(
      directoryProvider: () async => directory,
    );
    const endpoint = 'ws://192.0.2.1:9877';
    const token = 'cached-token';
    final namespace = store.namespaceFor(endpoint, token);
    await store.saveSessions(namespace, const [
      SessionSummary(
        id: 'cached-session',
        title: 'Offline task',
        phase: 'idle',
        agent: 'codex',
      ),
    ]);
    final service = GatewayService(
      connectTimeout: const Duration(milliseconds: 200),
      cacheStore: store,
    );
    final restored = service.sessionsStream.firstWhere(
      (sessions) => sessions.isNotEmpty,
    );

    await service.connect(endpoint, token);

    expect((await restored).single.id, 'cached-session');
    expect(service.lastSessions.single.id, 'cached-session');
    expect(service.sessionsAreCached, isTrue);
    expect(service.state, WsState.disconnected);
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
      irohTunnelOpener: (endpointId, relayUrl) async {
        requestedEndpoints.add(endpointId);
        requestedRelays.add(relayUrl);
        return server.port;
      },
    );

    await service.connect(
      'smelt+iroh://k7d3ffb1c9a24e5f?relay=https%3A%2F%2Frelay.test&relay_token=secret',
      'tok',
    );
    await _waitFor(() => requestedPaths.isNotEmpty);

    expect(requestedEndpoints, ['k7d3ffb1c9a24e5f']);
    expect(requestedRelays, ['https://relay.test']);
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
        irohTunnelOpener: (_, _) => Completer<int>().future,
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
    final firstSubscribed = Completer<void>();
    final secondSubscribed = Completer<void>();

    firstServer.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      firstSocketReady.complete(socket);
      socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
      socket.listen((data) {
        final message = jsonDecode(data as String) as Map<String, dynamic>;
        if (message['method'] == 'subscribe' && !firstSubscribed.isCompleted) {
          firstSubscribed.complete();
        }
      });
    });
    secondServer.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      secondSocketReady.complete(socket);
      socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
      socket.listen((data) {
        final message = jsonDecode(data as String) as Map<String, dynamic>;
        if (message['method'] == 'subscribe' && !secondSubscribed.isCompleted) {
          secondSubscribed.complete();
        }
      });
    });

    var tunnelPort = firstServer.port;
    var stopCount = 0;
    final openedPorts = <int>[];
    final service = GatewayService(
      connectTimeout: const Duration(seconds: 2),
      reconnectDelay: const Duration(milliseconds: 20),
      irohTunnelOpener: (_, _) async {
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
    service.subscribe('session-1');
    await firstSubscribed.future;

    await firstSocket.close();
    await secondSocketReady.future;
    await _waitFor(() => service.state == WsState.connected);
    await secondSubscribed.future;

    expect(stopCount, 1);
    expect(openedPorts, [firstServer.port, secondServer.port]);

    service.disconnect();
    await firstServer.close(force: true);
    await secondServer.close(force: true);
  });

  test('reconnect errors are coalesced and cancel stops retry loop', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final socketReady = Completer<WebSocket>();
    server.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      socketReady.complete(socket);
      socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
    });

    var openCount = 0;
    final errors = <String>[];
    final service = GatewayService(
      connectTimeout: const Duration(milliseconds: 200),
      reconnectDelay: const Duration(milliseconds: 10),
      irohTunnelOpener: (_, _) async {
        openCount++;
        if (openCount == 1) return server.port;
        throw StateError('desktop is offline');
      },
    );
    final errorSub = service.errorStream.listen(errors.add);

    await service.connect(
      'smelt+iroh://peer?relay=https%3A%2F%2Frelay.test',
      'tok',
    );
    final socket = await socketReady.future;
    await _waitFor(() => service.state == WsState.connected);
    await socket.close();
    await _waitFor(() => openCount >= 4);

    expect(errors, hasLength(1));
    expect(errors.single, contains('正在自动重连'));

    service.disconnect();
    final attemptsAfterCancel = openCount;
    await Future<void>.delayed(const Duration(milliseconds: 150));
    expect(service.state, WsState.disconnected);
    expect(openCount, attemptsAfterCancel);
    expect(errors, hasLength(1));

    await errorSub.cancel();
    await server.close(force: true);
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
      irohTunnelOpener: (_, _) async => server.port,
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
      irohTunnelOpener: (_, _) async => server.port,
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

  test('a half-open connection is detected and reconnected', () async {
    // 手机切网/被系统冻结后，TCP 可能写得出去读不回来，且不触发 onDone。
    // 心跳必须自己把这种连接判死，否则界面会永远停在掉线那一刻的旧会话。
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    var openCount = 0;
    var respondToPing = true;
    var pingCount = 0;
    server.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      openCount++;
      socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
      socket.listen((data) {
        final message = jsonDecode(data as String) as Map<String, dynamic>;
        if (message['method'] != 'ping') return;
        pingCount++;
        if (!respondToPing) return;
        final params = message['params'] as Map<String, dynamic>;
        socket.add(
          jsonEncode({'type': 'pong', 'sentAtMs': params['sentAtMs']}),
        );
      });
    });

    final states = <WsState>[];
    final service = GatewayService(
      connectTimeout: const Duration(seconds: 2),
      reconnectDelay: const Duration(milliseconds: 20),
      metricsInterval: const Duration(milliseconds: 30),
      pongTimeout: const Duration(milliseconds: 150),
    );
    final stateSub = service.stateStream.listen(states.add);

    await service.connect('http://127.0.0.1:${server.port}', 'tok');
    await _waitFor(() => service.state == WsState.connected && pingCount > 0);

    respondToPing = false;
    await _waitFor(() => states.contains(WsState.reconnecting));

    respondToPing = true;
    await _waitFor(() => service.state == WsState.connected && openCount >= 2);

    service.disconnect();
    await stateSub.cancel();
    await server.close(force: true);
  });

  test('a stalled ping never wedges the heartbeat', () async {
    // 回归点：`_pendingPingSentAt` 只在收到 pong 时清空。少了超时判死，它会
    // 永久非空，后续每个周期都跳过发送——心跳就此停摆且无人察觉。
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    var pingCount = 0;
    server.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
      socket.listen((data) {
        final message = jsonDecode(data as String) as Map<String, dynamic>;
        if (message['method'] == 'ping') pingCount++;
      });
    });

    final service = GatewayService(
      connectTimeout: const Duration(seconds: 2),
      reconnectDelay: const Duration(milliseconds: 20),
      metricsInterval: const Duration(milliseconds: 30),
      pongTimeout: const Duration(milliseconds: 100),
    );
    await service.connect('http://127.0.0.1:${server.port}', 'tok');
    await _waitFor(() => pingCount >= 3);

    service.disconnect();
    await server.close(force: true);
  });

  test('an unsupported ping does not trip the liveness timeout', () async {
    // 老桌面端不认识 ping，回 `invalid request`。那不是掉线，不该被判死。
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    var openCount = 0;
    server.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      openCount++;
      socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
      socket.listen((data) {
        final message = jsonDecode(data as String) as Map<String, dynamic>;
        if (message['method'] == 'ping') {
          socket.add(jsonEncode({'type': 'error', 'error': 'invalid request'}));
        }
      });
    });

    final service = GatewayService(
      connectTimeout: const Duration(seconds: 2),
      reconnectDelay: const Duration(milliseconds: 20),
      metricsInterval: const Duration(milliseconds: 30),
      pongTimeout: const Duration(milliseconds: 100),
    );
    await service.connect('http://127.0.0.1:${server.port}', 'tok');
    await _waitFor(() => service.state == WsState.connected);
    await Future<void>.delayed(const Duration(milliseconds: 400));

    expect(service.state, WsState.connected);
    expect(openCount, 1);

    service.disconnect();
    await server.close(force: true);
  });

  test('returning to the foreground re-pulls sessions', () async {
    // 后台期间定时器是冻结的，回前台必须主动拉一次，否则先看到的是旧数据。
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    var listCount = 0;
    server.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
      socket.listen((data) {
        final message = jsonDecode(data as String) as Map<String, dynamic>;
        if (message['method'] == 'listSessions') listCount++;
      });
    });

    final service = GatewayService(
      connectTimeout: const Duration(seconds: 2),
      metricsInterval: const Duration(seconds: 30),
    );
    await service.connect('http://127.0.0.1:${server.port}', 'tok');
    await _waitFor(() => service.state == WsState.connected && listCount >= 1);
    final afterConnect = listCount;

    service.verifyConnection();
    await _waitFor(() => listCount > afterConnect);

    service.disconnect();
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
