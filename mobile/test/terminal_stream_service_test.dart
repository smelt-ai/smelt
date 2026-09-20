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
        1,
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
      await _waitFor(
        () => events.any((event) => event is TerminalReplayCompleteEvent),
      );
      expect(events.whereType<TerminalReplayCompleteEvent>(), hasLength(1));
      expect(
        events.indexWhere((event) => event is TerminalReplayCompleteEvent),
        greaterThan(events.indexWhere((event) => event is TerminalDataEvent)),
      );
      await Future<void>.delayed(const Duration(milliseconds: 150));
      expect(resizeCount, 0, reason: 'replay completion must not redraw PTY');

      service.sendInput('\x03');
      final inputMessage = await input.future.timeout(
        const Duration(seconds: 5),
      );
      expect(inputMessage['params']['data'], '\x03');

      service.updateGeometry(
        const TerminalGeometry(
          cols: 50,
          rows: 24,
          cellWidth: 8,
          cellHeight: 16,
        ),
      );
      final resizeMessage = await resizes[0].future.timeout(
        const Duration(seconds: 5),
      );
      expect(resizeMessage['params']['cols'], 50);
      expect(resizeMessage['params']['rows'], 24);
      await _waitFor(() => service.state == TerminalStreamState.ended);
      await Future<void>.delayed(const Duration(milliseconds: 700));
      expect(terminalConnections, 1);
    },
  );

  test(
    'terminal stream discards a mismatched snapshot and reattaches',
    () async {
      var terminalConnections = 0;
      final attachGeometries = <Map<String, dynamic>>[];

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
          final connection = ++terminalConnections;
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
            if (message['method'] != 'attach') continue;
            attachGeometries.add(
              Map<String, dynamic>.from(
                message['params'] as Map<String, dynamic>,
              ),
            );
            if (connection == 1) {
              socket.add(
                jsonEncode({
                  'type': 'terminalReady',
                  'sessionId': 'terminal-1',
                  'cols': 181,
                  'rows': 59,
                  'replayBytes': 3,
                  'writeEnabled': true,
                }),
              );
              socket.add(utf8.encode('OLD'));
            } else {
              socket.add(
                jsonEncode({
                  'type': 'terminalReady',
                  'sessionId': 'terminal-1',
                  'cols': 49,
                  'rows': 47,
                  'replayBytes': 3,
                  'writeEnabled': true,
                }),
              );
              socket.add(utf8.encode('NEW'));
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

      final service = TerminalStreamService(
        gateway: gateway,
        sessionId: 'terminal-1',
      );
      addTearDown(service.dispose);
      final events = <TerminalStreamEvent>[];
      final eventSubscription = service.events.listen(events.add);
      addTearDown(eventSubscription.cancel);

      service.start(
        const TerminalGeometry(
          cols: 49,
          rows: 47,
          cellWidth: 8,
          cellHeight: 16,
        ),
      );

      await _waitFor(() => terminalConnections == 2);
      await _waitFor(
        () => events.any((event) => event is TerminalReplayCompleteEvent),
      );

      expect(attachGeometries, hasLength(2));
      expect(attachGeometries.every((geometry) => geometry['cols'] == 49), true);
      final ready = events.whereType<TerminalReadyEvent>().single;
      expect((ready.cols, ready.rows), (49, 47));
      expect(
        events
            .whereType<TerminalDataEvent>()
            .expand((event) => event.bytes)
            .toList(),
        utf8.encode('NEW'),
      );
      expect(events.whereType<TerminalReplayCompleteEvent>(), hasLength(1));
    },
  );

  /// 首屏只要最新的一段，用户往上滚才去要更老的——手机走隧道，不切备用屏的 TUI
  /// history 全推过去就是进页面先干等。
  test('terminal stream takes the whole scrollback budget at once', () async {
    final attachParams = <Map<String, dynamic>>[];
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
      if (request.uri.path != '/terminal/terminal-1/ws') {
        request.response.statusCode = HttpStatus.notFound;
        await request.response.close();
        return;
      }
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
        if (message['method'] != 'attach') continue;
        final params = Map<String, dynamic>.from(
          message['params'] as Map<String, dynamic>,
        );
        attachParams.add(params);
        socket.add(
          jsonEncode({
            'type': 'terminalReady',
            'sessionId': 'terminal-1',
            'cols': 40,
            'rows': 20,
            'replayBytes': 0,
            'writeEnabled': true,
            'scrollbackLines': params['maxScrollbackLines'],
            // 会话侧还有 4000 行，比任何一次预算都多。
            'historyLines': 4000,
          }),
        );
      }
    });

    final gateway = GatewayService();
    addTearDown(gateway.dispose);
    await gateway.connect('http://127.0.0.1:${server.port}', 'tok');
    await _waitFor(() => gateway.state == WsState.connected);

    final service = TerminalStreamService(
      gateway: gateway,
      sessionId: 'terminal-1',
      resizeDebounce: Duration.zero,
    );
    addTearDown(service.dispose);
    final events = <TerminalStreamEvent>[];
    final subscription = service.events.listen(events.add);
    addTearDown(subscription.cancel);

    service.start(
      const TerminalGeometry(cols: 40, rows: 20, cellWidth: 8, cellHeight: 16),
    );
    await _waitFor(() => events.whereType<TerminalReadyEvent>().length == 1);
    expect(
      attachParams.single['maxScrollbackLines'],
      initialTerminalScrollbackLines(20),
      reason: '首屏预算按视口算：20 行的终端不该跟 60 行的要同样多历史',
    );
    expect(service.canLoadMoreScrollback, isTrue);

    expect(service.loadMoreScrollback(), isTrue);
    await _waitFor(() => events.whereType<TerminalReadyEvent>().length == 2);
    expect(
      attachParams.last['maxScrollbackLines'],
      kMaxTerminalScrollbackLines,
      reason: '快照是整帧的，翻倍爬坡等于把同一段历史重传好几遍',
    );
    expect(terminalConnections, 2, reason: '提高预算必须重取一次快照');
    expect(
      service.canLoadMoreScrollback,
      isFalse,
      reason: '已经要到客户端能留住的全部，不该再有第二次重取',
    );
    expect(
      events.whereType<TerminalReadyEvent>().last.historyLines,
      4000,
      reason: '客户端要知道上面还有多少，才能决定还能不能再拉',
    );

    // 切后台再切回来：重连不能把「补历史」的大预算一起带上。重连本来就要重建终端、
    // 回到最新输出，上一次补的历史留不住；把它变成每次切回前台的固定成本，就是每次
    // 都要在隧道上重传几千行。
    service.suspend();
    service.resume();
    await _waitFor(() => events.whereType<TerminalReadyEvent>().length == 3);
    expect(
      attachParams.last['maxScrollbackLines'],
      initialTerminalScrollbackLines(20),
      reason: '补历史是「这一次」的动作，不该粘在之后每次重连上',
    );
  });

  /// attach 必须等页面把 chrome 摆完、行列落定之后再发。先 attach 再改尺寸 = 一次多余的
  /// PTY resize，而不切备用屏的 CLI 每收一次 SIGWINCH 就把整段对话重印一遍。
  test('attach waits for the settled geometry', () async {
    final attachParams = <Map<String, dynamic>>[];
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    addTearDown(() => server.close(force: true));
    server.listen((request) async {
      if (request.uri.path == '/acp/ws') {
        final socket = await WebSocketTransformer.upgrade(request);
        socket.add(jsonEncode({'type': 'connected', 'writeEnabled': true}));
        await for (final _ in socket) {}
        return;
      }
      if (request.uri.path != '/terminal/terminal-1/ws') {
        request.response.statusCode = HttpStatus.notFound;
        await request.response.close();
        return;
      }
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
        if (message['method'] != 'attach') continue;
        attachParams.add(
          Map<String, dynamic>.from(message['params'] as Map<String, dynamic>),
        );
        socket.add(
          jsonEncode({
            'type': 'terminalReady',
            'sessionId': 'terminal-1',
            'cols': message['params']['cols'],
            'rows': message['params']['rows'],
            'replayBytes': 0,
            'writeEnabled': true,
          }),
        );
      }
    });

    final gateway = GatewayService();
    addTearDown(gateway.dispose);
    await gateway.connect('http://127.0.0.1:${server.port}', 'tok');
    await _waitFor(() => gateway.state == WsState.connected);

    final service = TerminalStreamService(
      gateway: gateway,
      sessionId: 'terminal-1',
      resizeDebounce: Duration.zero,
      attachSettleDelay: const Duration(milliseconds: 80),
    );
    addTearDown(service.dispose);
    final events = <TerminalStreamEvent>[];
    final subscription = service.events.listen(events.add);
    addTearDown(subscription.cancel);

    // 页面先按「没有快捷键栏」的高度开流。
    service.start(
      const TerminalGeometry(cols: 40, rows: 35, cellWidth: 8, cellHeight: 16),
    );
    // 通道一接通，页面就把栏子摆上去，行数落到 32。
    await _waitFor(() => events.any((event) => event is TerminalConnectedEvent));
    expect(attachParams, isEmpty, reason: '行列未落定之前不能 attach');
    service.updateGeometry(
      const TerminalGeometry(cols: 40, rows: 32, cellWidth: 8, cellHeight: 16),
    );

    await _waitFor(() => events.whereType<TerminalReadyEvent>().isNotEmpty);
    expect(attachParams, hasLength(1));
    expect(attachParams.single['rows'], 32);

    // 落定后再量到的同一份几何不应该变成 resize 帧。
    service.updateGeometry(
      const TerminalGeometry(cols: 40, rows: 32, cellWidth: 8, cellHeight: 16),
    );
    await Future<void>.delayed(const Duration(milliseconds: 120));
    expect(events.whereType<TerminalReadyEvent>(), hasLength(1));
  });


  /// 对端不下发 historyLines（老网关）时，手机不能去拉一个它根本不支持的东西。
  test('a gateway that reports no history never triggers a refetch', () async {
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
      if (request.uri.path != '/terminal/terminal-1/ws') {
        request.response.statusCode = HttpStatus.notFound;
        await request.response.close();
        return;
      }
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
        if (message['method'] != 'attach') continue;
        socket.add(
          jsonEncode({
            'type': 'terminalReady',
            'sessionId': 'terminal-1',
            'cols': 40,
            'rows': 20,
            'replayBytes': 0,
            'writeEnabled': true,
          }),
        );
      }
    });

    final gateway = GatewayService();
    addTearDown(gateway.dispose);
    await gateway.connect('http://127.0.0.1:${server.port}', 'tok');
    await _waitFor(() => gateway.state == WsState.connected);

    final service = TerminalStreamService(
      gateway: gateway,
      sessionId: 'terminal-1',
      resizeDebounce: Duration.zero,
    );
    addTearDown(service.dispose);
    final events = <TerminalStreamEvent>[];
    final subscription = service.events.listen(events.add);
    addTearDown(subscription.cancel);

    service.start(
      const TerminalGeometry(cols: 40, rows: 20, cellWidth: 8, cellHeight: 16),
    );
    await _waitFor(() => events.whereType<TerminalReadyEvent>().isNotEmpty);
    expect(service.canLoadMoreScrollback, isFalse);
    expect(service.loadMoreScrollback(), isFalse);
    await Future<void>.delayed(const Duration(milliseconds: 150));
    expect(terminalConnections, 1);
  });
}
