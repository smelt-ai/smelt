import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/acp_snapshot.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/services/pending_actions_controller.dart';

SessionSummary _waiting(String id, {String title = 'session'}) =>
    SessionSummary(
      id: id,
      title: title,
      phase: 'idle',
      status: 'waiting_approval',
      agent: 'codex',
    );

AcpSnapshot _withPermission(String question) => AcpSnapshot(
  entries: const [],
  phase: const AcpPhaseAwaitingApproval(),
  pendingPermissions: [
    PendingPermission(
      toolCallId: 'tool-1',
      question: question,
      options: const [
        PermissionOption(
          optionId: 'allow',
          name: 'Allow',
          kind: 'AllowOnce',
        ),
      ],
    ),
  ],
);

void main() {
  group('PendingActionsController', () {
    late StreamController<List<SessionSummary>> sessions;
    late StreamController<String> snapshots;
    late Map<String, AcpSnapshot> cache;
    late List<String> requests;
    late bool connected;

    setUp(() {
      sessions = StreamController<List<SessionSummary>>.broadcast();
      snapshots = StreamController<String>.broadcast();
      cache = {};
      requests = [];
      connected = true;
    });

    tearDown(() async {
      await sessions.close();
      await snapshots.close();
    });

    PendingActionsController build() => PendingActionsController(
      sessions: sessions.stream,
      initialSessions: const [],
      snapshotUpdates: snapshots.stream,
      lookupSnapshot: (id) => cache[id],
      requestDetails: (id) {
        requests.add(id);
        return connected;
      },
    );

    test('surfaces every waiting session without entering any of them', () async {
      final controller = build();
      addTearDown(controller.dispose);

      sessions.add([_waiting('a'), _waiting('b')]);
      await pumpEventQueue();

      expect(
        controller.items.map((item) => item.sessionId),
        ['a', 'b'],
        reason: 'the console must show sibling sessions, not just the open one',
      );
      expect(requests, ['a', 'b']);
      expect(controller.items.every((item) => item.isLoadingDetails), isTrue);
    });

    test('fills in the approval once its details arrive', () async {
      final controller = build();
      addTearDown(controller.dispose);

      sessions.add([_waiting('a')]);
      await pumpEventQueue();

      cache['a'] = _withPermission('run cargo test?');
      snapshots.add('a');
      await pumpEventQueue();

      final item = controller.items.single;
      expect(item.isLoadingDetails, isFalse);
      expect(item.question, 'run cargo test?');
      expect(item.permission?.toolCallId, 'tool-1');
    });

    test('asks for details once per session, not on every rebuild', () async {
      final controller = build();
      addTearDown(controller.dispose);

      sessions.add([_waiting('a')]);
      await pumpEventQueue();
      // An unrelated session's snapshot must not re-trigger the fetch: some
      // attentions never carry an approval, and refetching them would spin.
      snapshots.add('other');
      snapshots.add('other');
      await pumpEventQueue();

      expect(requests, ['a']);
    });

    test('retries details after a send failed while offline', () async {
      connected = false;
      final controller = build();
      addTearDown(controller.dispose);

      sessions.add([_waiting('a')]);
      await pumpEventQueue();
      expect(requests, ['a']);

      connected = true;
      snapshots.add('a');
      await pumpEventQueue();

      expect(
        requests,
        ['a', 'a'],
        reason: 'a request that never left the device must not be remembered',
      );
    });

    test('drops a card as soon as the session stops waiting', () async {
      final controller = build();
      addTearDown(controller.dispose);

      sessions.add([_waiting('a')]);
      await pumpEventQueue();
      expect(controller.items, hasLength(1));

      sessions.add([
        SessionSummary(id: 'a', title: 'session', phase: 'idle', agent: 'codex'),
      ]);
      await pumpEventQueue();

      expect(controller.items, isEmpty);
    });
  });
}
