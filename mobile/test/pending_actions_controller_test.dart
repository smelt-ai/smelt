import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/services/pending_actions_controller.dart';
import 'package:smelt_mobile/services/runtime_service.dart';

import 'support/runtime_fixtures.dart';

SessionSummary _waiting(String id, {int updatedAt = 0, String? detail}) =>
    session(
      id: id,
      title: 'session',
      phase: SessionPhase.awaitingApproval,
      status: SessionStatus.needsYou,
      updatedAt: updatedAt,
      detail: detail,
    );

PendingActions _withPermission(String question) => PendingActions(
  permissions: [
    PermissionRequest(
      toolCallId: 'tool-1',
      question: question,
      options: const [
        PermissionOption(
          optionId: 'allow',
          name: 'Allow',
          kind: PermissionOptionKind.allowOnce,
        ),
      ],
      details: const ApprovalDetails(kind: ApprovalKind.generic),
    ),
  ],
);

void main() {
  group('PendingActionsController', () {
    late StreamController<List<SessionSummary>> sessions;
    late Map<String, Completer<PendingActions>> pending;
    late List<String> requests;

    setUp(() {
      sessions = StreamController<List<SessionSummary>>.broadcast();
      pending = {};
      requests = [];
    });

    tearDown(() async {
      await sessions.close();
    });

    PendingActionsController build() => PendingActionsController(
      sessions: sessions.stream,
      initialSessions: const [],
      fetchDetails: (id) {
        requests.add(id);
        return (pending[id] = Completer<PendingActions>()).future;
      },
    );

    test(
      'surfaces every waiting session without entering any of them',
      () async {
        final controller = build();
        addTearDown(controller.dispose);

        sessions.add([_waiting('a'), _waiting('b')]);
        await pumpEventQueue();

        expect(
          controller.items.map((item) => item.sessionId),
          ['a', 'b'],
          reason:
              'the console must show sibling sessions, not just the open one',
        );
        expect(requests, ['a', 'b']);
        expect(controller.items.every((item) => item.isLoadingDetails), isTrue);
      },
    );

    test('fills in the approval once its details arrive', () async {
      final controller = build();
      addTearDown(controller.dispose);

      sessions.add([_waiting('a')]);
      await pumpEventQueue();

      pending['a']!.complete(_withPermission('run cargo test?'));
      await pumpEventQueue();

      final item = controller.items.single;
      expect(item.isLoadingDetails, isFalse);
      expect(item.question, 'run cargo test?');
      expect(item.permission?.toolCallId, 'tool-1');
    });

    test(
      'an attention without an approval stops loading once fetched',
      () async {
        final controller = build();
        addTearDown(controller.dispose);

        sessions.add([_waiting('a')]);
        await pumpEventQueue();
        pending['a']!.complete(const PendingActions(permissions: []));
        await pumpEventQueue();

        final item = controller.items.single;
        expect(item.permission, isNull);
        expect(item.isLoadingDetails, isFalse);
      },
    );

    test(
      'asks for details once per session state, not on every push',
      () async {
        final controller = build();
        addTearDown(controller.dispose);

        sessions.add([_waiting('a')]);
        await pumpEventQueue();
        pending['a']!.complete(const PendingActions(permissions: []));
        await pumpEventQueue();
        // 同一状态再推一遍不能重取：有些提醒本来就没有审批卡，反复取会空转。
        sessions.add([_waiting('a')]);
        sessions.add([_waiting('a'), session(id: 'idle')]);
        await pumpEventQueue();

        expect(requests, ['a']);
      },
    );

    test('refetches when the waiting session changes', () async {
      final controller = build();
      addTearDown(controller.dispose);

      sessions.add([_waiting('a', updatedAt: 1, detail: 'git push?')]);
      await pumpEventQueue();
      pending['a']!.complete(_withPermission('git push?'));
      await pumpEventQueue();

      sessions.add([_waiting('a', updatedAt: 2, detail: 'rm -rf build?')]);
      await pumpEventQueue();
      expect(requests, ['a', 'a']);
      // 新详情到之前先留着旧卡，不闪回加载态。
      expect(controller.items.single.question, 'git push?');

      pending['a']!.complete(_withPermission('rm -rf build?'));
      await pumpEventQueue();
      expect(controller.items.single.question, 'rm -rf build?');
    });

    test('retries details after a fetch failed while offline', () async {
      final controller = build();
      addTearDown(controller.dispose);

      sessions.add([_waiting('a')]);
      await pumpEventQueue();
      pending['a']!.completeError(
        const SmeltError(kind: SmeltErrorKind.offline, message: 'offline'),
      );
      await pumpEventQueue();
      expect(controller.items.single.isLoadingDetails, isTrue);

      sessions.add([_waiting('a')]);
      await pumpEventQueue();

      expect(requests, [
        'a',
        'a',
      ], reason: 'a fetch that failed must not be remembered as done');
    });

    test('drops a card as soon as the session stops waiting', () async {
      final controller = build();
      addTearDown(controller.dispose);

      sessions.add([_waiting('a')]);
      await pumpEventQueue();
      expect(controller.items, hasLength(1));

      sessions.add([session(id: 'a', title: 'session')]);
      await pumpEventQueue();

      expect(controller.items, isEmpty);
    });
  });
}
