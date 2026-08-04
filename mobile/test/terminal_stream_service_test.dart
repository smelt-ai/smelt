import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/services/terminal_stream_service.dart';

Future<void> _waitFor(bool Function() condition) async {
  final deadline = DateTime.now().add(const Duration(seconds: 5));
  while (!condition()) {
    if (DateTime.now().isAfter(deadline)) {
      throw TimeoutException('condition was not met');
    }
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}

void main() {
  test(
    'terminal stream attaches, forwards bytes, input, resize, and fatal close',
    () async {
      final attach = Completer<Map<String, dynamic>>();
      final input = Completer<Map<String, dynamic>>();
      final resizes = List.generate(
        4,
        (_) => Completer<Map<String, dynamic>>(),
      );
      var resizeCount = 0;
      var terminalConnections = 0;

      final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
      addTearDown(() => server.close(force: true));
      server.listen((request) async {
        if (request.uri.path == '/acp/ws') {
          final socket = await WebSocketTransformer.upgrade(request);
          socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
          await for (final _ in socket) {}
          return;
        }
        if (request.uri.path == '/terminal/terminal-1/ws') {
          terminalConnections++;
          final socket = await WebSocketTransformer.upgrade(request);
          socket.add(
            jsonEncode({
              'type': 'terminalConnected',
              'sessionId': 'terminal-1',
              'writeEnabled': true,
            }),
          );
          await for (final raw in socket) {
            if (raw is! String) continue;
            final message = jsonDecode(raw) as Map<String, dynamic>;
            switch (message['method']) {
              case 'attach':
                if (!attach.isCompleted) attach.complete(message);
                socket.add(
                  jsonEncode({
                    'type': 'terminalReady',
                    'sessionId': 'terminal-1',
                    'cols': 40,
                    'rows': 20,
                    'replayBytes': 3,
                    'writeEnabled': true,
                  }),
                );
                socket.add([0xe4, 0xb8, 0xad]);
              case 'input':
                if (!input.isCompleted) input.complete(message);
              case 'resize':
                if (resizeCount < resizes.length) {
                  resizes[resizeCount].complete(message);
                  resizeCount++;
                }
                if (resizeCount == resizes.length) {
                  socket.add(
                    jsonEncode({
                      'type': 'terminalError',
                      'error': 'terminal session not found',
                      'fatal': true,
                    }),
                  );
                }
            }
          }
          return;
        }
        request.response.statusCode = HttpStatus.notFound;
        await request.response.close();
      });

      final gateway = GatewayService();
      addTearDown(gateway.dispose);
      await gateway.connect('http://127.0.0.1:${server.port}', 'tok');
      await _waitFor(() => gateway.state == WsState.connected);
      expect(
        gateway.terminalWebSocketUri('terminal-1')?.path,
        '/terminal/terminal-1/ws',
      );

      final service = TerminalStreamService(
        gateway: gateway,
        sessionId: 'terminal-1',
        resizeDebounce: Duration.zero,
        attachRefreshDelay: Duration.zero,
      );
      addTearDown(service.dispose);
      final events = <TerminalStreamEvent>[];
      final eventSubscription = service.events.listen(events.add);
      addTearDown(eventSubscription.cancel);

      service.start(
        const TerminalGeometry(
          cols: 40,
          rows: 20,
          cellWidth: 8,
          cellHeight: 16,
        ),
      );
      final attachMessage = await attach.future.timeout(
        const Duration(seconds: 5),
      );
      expect(attachMessage['params']['cols'], 40);
      await _waitFor(() => events.any((event) => event is TerminalDataEvent));
      final data = events.whereType<TerminalDataEvent>().single;
      expect(data.bytes, [0xe4, 0xb8, 0xad]);

      final attachNudge = await resizes[0].future.timeout(
        const Duration(seconds: 5),
      );
      expect(attachNudge['params']['cols'], 40);
      expect(attachNudge['params']['rows'], 21);
      final attachRestore = await resizes[1].future.timeout(
        const Duration(seconds: 5),
      );
      expect(attachRestore['params']['cols'], 40);
      expect(attachRestore['params']['rows'], 20);

      service.sendInput('\x03');
      final inputMessage = await input.future.timeout(
        const Duration(seconds: 5),
      );
      expect(inputMessage['params']['data'], '\x03');

      service.forceGeometry();
      final forcedResizeMessage = await resizes[2].future.timeout(
        const Duration(seconds: 5),
      );
      expect(forcedResizeMessage['params']['cols'], 40);
      expect(forcedResizeMessage['params']['rows'], 20);

      service.updateGeometry(
        const TerminalGeometry(
          cols: 50,
          rows: 24,
          cellWidth: 8,
          cellHeight: 16,
        ),
      );
      final resizeMessage = await resizes[3].future.timeout(
        const Duration(seconds: 5),
      );
      expect(resizeMessage['params']['cols'], 50);
      expect(resizeMessage['params']['rows'], 24);
      await _waitFor(() => service.state == TerminalStreamState.ended);
      await Future<void>.delayed(const Duration(milliseconds: 700));
      expect(terminalConnections, 1);
    },
  );
}
