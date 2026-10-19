import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/services/runtime_service.dart';

import 'support/runtime_fixtures.dart';

ConversationEvent _error(String message) => ConversationEvent(
  kind: ConversationEventKind.error,
  cached: false,
  message: message,
);

void main() {
  late FakeRuntimeBackend backend;
  late RuntimeService service;

  setUp(() {
    backend = FakeRuntimeBackend();
    service = RuntimeService(backend)..start();
  });

  tearDown(() async {
    await service.dispose();
    await backend.close();
  });

  group('连接', () {
    test('转发连接事件并留住最近的状态和链路', () async {
      final seen = <RuntimeConnectionState>[];
      service.connectionStream.listen((event) => seen.add(event.state));

      backend.connection.add(
        const ConnectionEvent(
          state: RuntimeConnectionState.connected,
          endpointId: 'endpoint-a',
          link: LinkMetrics(path: LinkPath.lan, rttMs: 4),
        ),
      );
      await pumpEventQueue();

      expect(seen, [RuntimeConnectionState.connected]);
      expect(service.state, RuntimeConnectionState.connected);
      expect(service.link, const LinkMetrics(path: LinkPath.lan, rttMs: 4));
    });

    test('连上时拉一次工作区和自动化目录，链路变化不重复拉', () async {
      backend.workspace = const WorkspaceCatalog(
        projects: [
          WorkspaceProject(root: '/code/smelt', title: 'smelt', order: 0),
        ],
        agents: [],
        sections: LaunchSections(common: [], terminal: [], conversation: []),
      );
      backend.automations = AutomationCatalog(
        automations: const [],
        agents: const [
          AgentDefinitionSummary(
            id: 'agent-1',
            name: ' 工作助手 ',
            description: '',
            engineKindId: 'pi',
            prompt: '',
            plugins: [],
            contextFolders: [],
            contextLinks: [],
          ),
        ],
      );
      final workspaces = <WorkspaceCatalog>[];
      service.workspaceStream.listen(workspaces.add);

      backend.connection.add(
        const ConnectionEvent(state: RuntimeConnectionState.connected),
      );
      backend.connection.add(
        const ConnectionEvent(
          state: RuntimeConnectionState.connected,
          link: LinkMetrics(path: LinkPath.relay),
        ),
      );
      await pumpEventQueue();

      expect(backend.calls.where((call) => call == 'workspace'), hasLength(1));
      expect(
        backend.calls.where((call) => call == 'automations'),
        hasLength(1),
      );
      expect(workspaces.single.projects.single.title, 'smelt');
      expect(service.workspace.projects.single.root, '/code/smelt');
      expect(service.agentDefinitionName('agent-1'), '工作助手');
      expect(service.agentDefinitionName('missing'), isNull);
    });

    test('connect 的失败交给状态流表达，不往外抛', () async {
      backend.connectError = const SmeltError(
        kind: SmeltErrorKind.offline,
        message: 'offline',
      );
      final errors = <String>[];
      service.errorStream.listen(errors.add);

      await service.connect('endpoint-a', null);
      await pumpEventQueue();

      expect(backend.calls, ['connect endpoint-a']);
      expect(errors, isEmpty);
    });

    test('断开后「重试」连回上一个运行时，忘掉之后就不连了', () async {
      await service.connect('endpoint-a', 'https://relay.example');
      await service.disconnect();
      await service.retry();
      expect(backend.calls, [
        'connect endpoint-a',
        'disconnect',
        'connect endpoint-a',
      ]);

      backend.calls.clear();
      await service.disconnect(forget: true);
      await service.retry();
      expect(backend.calls, ['disconnect']);
    });

    test('重连中「重试」只催一次探活', () async {
      await service.connect('endpoint-a', null);
      backend.connection.add(
        const ConnectionEvent(state: RuntimeConnectionState.reconnecting),
      );
      await pumpEventQueue();
      backend.calls.clear();

      await service.retry();
      await pumpEventQueue();

      expect(backend.calls, ['verify']);
    });

    test('配对成功后它就是「重试」的目标', () async {
      backend.pairResult = runtimeInfo(endpointId: 'endpoint-p');
      final info = await service.pair(
        '  smelt+iroh://endpoint-p/?code=1 ',
        'me',
      );
      expect(info.endpointId, 'endpoint-p');
      expect(backend.calls, ['pair smelt+iroh://endpoint-p/?code=1']);

      await service.retry();
      expect(backend.calls.last, 'connect endpoint-p');
    });
  });

  group('会话目录', () {
    test('会话列表、提醒、已处理、错误各走各的流', () async {
      final lists = <SessionList>[];
      final attentions = <AttentionItem>[];
      final resolved = <String>[];
      final errors = <String>[];
      service.sessionListStream.listen(lists.add);
      service.attentionStream.listen(attentions.add);
      service.attentionResolvedStream.listen(resolved.add);
      service.errorStream.listen(errors.add);

      final list = SessionList(
        sessions: [
          session(id: 'a'),
          session(id: 'b'),
        ],
        cached: true,
        savedAtMs: 42,
      );
      backend.catalog
        ..add(CatalogEvent(kind: CatalogEventKind.sessions, sessions: list))
        ..add(
          CatalogEvent(
            kind: CatalogEventKind.attention,
            attention: attention('a', AttentionKind.approval),
          ),
        )
        ..add(
          const CatalogEvent(
            kind: CatalogEventKind.attentionResolved,
            sessionId: 'a',
          ),
        )
        ..add(
          const CatalogEvent(
            kind: CatalogEventKind.error,
            message: 'subscription dropped',
          ),
        );
      await pumpEventQueue();

      expect(lists, [list]);
      expect(service.sessionList.cached, isTrue);
      expect(service.sessionList.savedAtMs, 42);
      expect(service.sessions.map((s) => s.id), ['a', 'b']);
      expect(attentions.single.sessionId, 'a');
      expect(resolved, ['a']);
      expect(errors, ['subscription dropped']);
    });

    test('开关自动化后留住运行时回的权威目录', () async {
      final catalogs = <AutomationCatalog>[];
      service.automationCatalogStream.listen(catalogs.add);

      await service.setAutomationEnabled('automation-1', false);
      await service.runAutomationOnce('automation-1');
      await pumpEventQueue();

      expect(backend.calls, ['enable automation-1 false', 'run automation-1']);
      expect(catalogs, hasLength(2));
      expect(service.automationCatalog, same(backend.automations));
    });
  });

  group('对话订阅', () {
    test('进 B 再退回 A 时把 A 重新订阅回来', () async {
      final a = service.watchConversation('a');
      final aEvents = <ConversationEvent>[];
      a.events.listen(aEvents.add);
      expect(service.watchedSessionId, 'a');

      final b = service.watchConversation('b');
      final bEvents = <ConversationEvent>[];
      b.events.listen(bEvents.add);
      expect(service.watchedSessionId, 'b');

      backend.watches[1].events.add(_error('b event'));
      // 被顶下去的 A 不再收事件。
      backend.watches[0].events.add(_error('stale a event'));
      await pumpEventQueue();
      expect(bEvents.single.message, 'b event');
      expect(aEvents, isEmpty);

      b.close();
      expect(service.watchedSessionId, 'a');
      expect(backend.calls, ['watch a', 'watch b', 'watch a']);

      backend.watches[2].events.add(_error('a again'));
      await pumpEventQueue();
      expect(aEvents.single.message, 'a again');

      a.close();
      await pumpEventQueue();
      expect(service.watchedSessionId, isNull);
      expect(backend.calls.last, 'unwatch');
    });

    test('重试只对栈顶的订阅生效', () async {
      final a = service.watchConversation('a');
      final b = service.watchConversation('b');

      a.restart();
      expect(backend.calls, ['watch a', 'watch b']);

      b.restart();
      expect(backend.calls, ['watch a', 'watch b', 'watch b']);
    });
  });
}
