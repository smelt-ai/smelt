import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/main.dart';
import 'package:smelt_mobile/models/saved_runtime.dart';
import 'package:smelt_mobile/pages/console_page.dart';
import 'package:smelt_mobile/pages/settings_page.dart';
import 'package:smelt_mobile/services/pairing_storage.dart';
import 'package:smelt_mobile/services/pending_actions_controller.dart';
import 'package:smelt_mobile/services/runtime_service.dart';
import 'package:smelt_mobile/widgets/approval_card.dart';
import 'package:smelt_mobile/widgets/session_row.dart';

import 'support/runtime_fixtures.dart';

class MemoryPairingStorage implements PairingStorage {
  MemoryPairingStorage([this.value = const SavedRuntimeCollection()]);

  SavedRuntimeCollection value;

  @override
  Future<SavedRuntimeCollection> load() async => value;

  @override
  Future<SavedRuntimeCollection> save(RuntimeInfo info) async {
    final runtime = SavedRuntime.fromInfo(info);
    value = SavedRuntimeCollection(
      runtimes: [
        runtime,
        ...value.runtimes.where((item) => item.runtimeId != runtime.runtimeId),
      ],
      activeRuntimeId: runtime.runtimeId,
    );
    return value;
  }

  @override
  Future<SavedRuntimeCollection> setActive(String runtimeId) async {
    value = SavedRuntimeCollection(
      runtimes: value.runtimes,
      activeRuntimeId: runtimeId,
    );
    return value;
  }

  @override
  Future<SavedRuntimeCollection> rename(String runtimeId, String name) async {
    value = SavedRuntimeCollection(
      runtimes: value.runtimes
          .map(
            (item) =>
                item.runtimeId == runtimeId ? item.copyWith(name: name) : item,
          )
          .toList(),
      activeRuntimeId: value.activeRuntimeId,
    );
    return value;
  }

  @override
  Future<SavedRuntimeCollection> remove(String runtimeId) async {
    final remaining = value.runtimes
        .where((item) => item.runtimeId != runtimeId)
        .toList();
    value = SavedRuntimeCollection(
      runtimes: remaining,
      activeRuntimeId: value.activeRuntimeId == runtimeId
          ? remaining.firstOrNull?.runtimeId
          : value.activeRuntimeId,
    );
    return value;
  }
}

void main() {
  test(
    'session list uses ACP conversation text instead of agent CLI label',
    () {
      final summary = session(
        id: 'acp-1',
        title: '修复移动端项目列表',
        detail: '  正在检查列表数据  ',
      );

      expect(sessionListTitle(summary), '修复移动端项目列表');
      expect(sessionListSubtitle(summary), '正在检查列表数据');
    },
  );

  test('session list omits empty detail and has an ACP fallback title', () {
    final summary = session(
      id: 'acp-2',
      title: '  ',
      agent: 'other',
      detail: ' ',
    );

    expect(sessionListTitle(summary), 'ACP conversation');
    expect(sessionListSubtitle(summary), isNull);
  });

  test('terminal sessions use a terminal fallback title', () {
    final summary = session(
      id: 'terminal-1',
      kind: SessionKind.terminal,
      title: ' ',
    );

    expect(sessionListTitle(summary), 'Terminal');
  });

  test('session filters separate actionable and running conversations', () {
    final approval = session(
      id: 'approval',
      phase: SessionPhase.awaitingApproval,
      status: SessionStatus.needsYou,
    );
    final running = session(
      id: 'running',
      phase: SessionPhase.thinking,
      status: SessionStatus.running,
    );
    final idle = session(id: 'idle');
    // 状态已经回到空闲，但这台手机上还挂着没处理的失败提醒。
    final failed = session(
      id: 'failed',
      phase: SessionPhase.failed,
      attention: attention(
        'failed',
        AttentionKind.failure,
        message: 'Agent stopped',
      ),
    );
    final done = session(
      id: 'done',
      phase: SessionPhase.succeeded,
      attention: attention('done', AttentionKind.success),
    );
    final sessions = [approval, running, idle, failed, done];

    expect(sessions.where(sessionNeedsAction).map((s) => s.id), [
      'approval',
      'failed',
    ]);
    expect(sessions.where(sessionIsRunning).map((s) => s.id), ['running']);
    expect(sessions.where(sessionRecentlyDone).map((s) => s.id), ['done']);
  });

  test('attention notifications stay hidden for the active session', () {
    final completed = attention('current', AttentionKind.success);
    final input = attention('current', AttentionKind.input);

    for (final item in [completed, input]) {
      expect(
        shouldShowAttentionNotification(
          attention: item,
          activeSessionId: 'current',
          subscribedSessionId: null,
        ),
        isFalse,
      );
    }
  });

  test('attention notifications still surface for other sessions', () {
    final completed = attention('background', AttentionKind.success);
    final input = attention('background', AttentionKind.input);

    expect(
      shouldShowAttentionNotification(
        attention: completed,
        activeSessionId: 'current',
        subscribedSessionId: null,
      ),
      isTrue,
    );
    expect(
      shouldShowAttentionNotification(
        attention: completed,
        activeSessionId: null,
        subscribedSessionId: 'background',
      ),
      isFalse,
    );
    expect(
      shouldShowAttentionNotification(
        attention: input,
        activeSessionId: null,
        subscribedSessionId: 'background',
      ),
      isTrue,
    );
  });

  testWidgets('console shows approvals from sessions you are not inside', (
    tester,
  ) async {
    final sessions = StreamController<List<SessionSummary>>.broadcast();
    addTearDown(sessions.close);

    final controller = PendingActionsController(
      sessions: sessions.stream,
      initialSessions: const [],
      fetchDetails: (id) async => id == 'acp-b'
          ? const PendingActions(permissions: [commandApproval])
          : const PendingActions(permissions: []),
    );
    addTearDown(controller.dispose);

    final responded = <String>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ConsolePage(
            controller: controller,
            onOpenSession: (_) {},
            onRespond: (session, tool, option) =>
                responded.add('$session/$tool/$option'),
          ),
        ),
      ),
    );

    sessions.add([
      session(
        id: 'acp-b',
        title: 'refactor gateway',
        status: SessionStatus.needsYou,
      ),
    ]);
    await tester.pumpAndSettle();

    expect(find.text('Run this command?'), findsOneWidget);
    expect(
      find.text('cargo test -p smelt-ui --no-fail-fast'),
      findsOneWidget,
      reason: 'the console must show what it is asking to approve',
    );
    expect(find.text('Working directory: ~/code/smelt'), findsOneWidget);

    await tester.tap(find.text('Allow'));
    await tester.pump();
    expect(responded, [
      'acp-b/tool-1/allow',
    ], reason: 'deciding must not require opening the session');
  });

  testWidgets('always-allow is demoted out of the primary button row', (
    tester,
  ) async {
    final responded = <String>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ApprovalCard(
            permission: commandApproval,
            onRespond: responded.add,
          ),
        ),
      ),
    );

    // Allow must not be a filled button sitting in the thumb zone, and the
    // irreversible option must not sit shoulder to shoulder with it.
    expect(find.widgetWithText(FilledButton, 'Allow'), findsNothing);
    expect(find.widgetWithText(OutlinedButton, 'Allow'), findsOneWidget);
    expect(find.widgetWithText(OutlinedButton, 'Always allow'), findsNothing);
    expect(find.widgetWithText(TextButton, 'Always allow'), findsOneWidget);
  });

  testWidgets('cached connection bar identifies stale content', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: CachedConnectionBar(
            state: RuntimeConnectionState.reconnecting,
            cachedAt: DateTime.now().subtract(const Duration(minutes: 3)),
          ),
        ),
      ),
    );

    expect(find.text('Reconnecting · Saved 3m ago'), findsOneWidget);
    expect(find.byType(CircularProgressIndicator), findsOneWidget);
  });

  testWidgets('pending action badge tracks sessions that need a decision', (
    tester,
  ) async {
    final waiting = session(
      id: 'approval',
      title: 'Needs approval',
      status: SessionStatus.needsYou,
    );
    final idle = session(id: 'idle', title: 'Finished task');
    final sessions = StreamController<List<SessionSummary>>.broadcast();
    addTearDown(sessions.close);
    var taps = 0;

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          appBar: AppBar(
            actions: [
              PendingActionBadge(
                onPressed: () => taps++,
                sessions: sessions.stream,
                initialSessions: [idle],
              ),
            ],
          ),
        ),
      ),
    );

    // 没人等我的时候不占地方。
    expect(find.byType(IconButton), findsNothing);

    sessions.add([waiting, idle]);
    await tester.pump();
    expect(find.text('1'), findsOneWidget);

    await tester.tap(find.byIcon(Icons.notification_important_outlined));
    expect(taps, 1);

    sessions.add([idle]);
    await tester.pump();
    expect(find.byType(IconButton), findsNothing);
  });

  testWidgets('offline connection bar offers a retry instead of a spinner', (
    tester,
  ) async {
    var retries = 0;
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: CachedConnectionBar(
            state: RuntimeConnectionState.disconnected,
            cachedAt: DateTime.now().subtract(const Duration(hours: 2)),
            onRetry: () => retries++,
          ),
        ),
      ),
    );

    expect(find.text('Offline · Saved 2h ago'), findsOneWidget);
    // 没有重连在跑的时候转菊花是在骗人。
    expect(find.byType(CircularProgressIndicator), findsNothing);
    expect(find.byIcon(Icons.cloud_off_outlined), findsOneWidget);

    await tester.tap(find.text('Retry'));
    expect(retries, 1);
  });

  testWidgets('runtime rename dialog can be saved repeatedly', (tester) async {
    final renamed = <String>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => Scaffold(
            body: FilledButton(
              onPressed: () async {
                final name = await showRuntimeRenameDialog(context, 'Runtime');
                if (name != null) renamed.add(name);
              },
              child: const Text('Rename'),
            ),
          ),
        ),
      ),
    );

    for (final name in ['Office Mac', 'Home Mac']) {
      await tester.tap(find.text('Rename'));
      await tester.pumpAndSettle();
      await tester.enterText(find.byType(TextFormField), name);
      await tester.tap(find.text('Save'));
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull);
    }

    expect(renamed, ['Office Mac', 'Home Mac']);
  });

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

  group('home', () {
    late FakeRuntimeBackend backend;

    setUp(() {
      FlutterSecureStorage.setMockInitialValues({});
      backend = FakeRuntimeBackend();
    });

    tearDown(() async {
      await runtimeService.dispose();
      await backend.close();
    });

    Future<void> pumpHome(
      WidgetTester tester, [
      MemoryPairingStorage? storage,
    ]) async {
      // 在测试的 zone 里订阅，事件才会随 pump 送达。
      runtimeService = RuntimeService(backend)..start();
      await tester.pumpWidget(
        SmeltApp(pairingStorage: storage ?? MemoryPairingStorage()),
      );
      await tester.pumpAndSettle();
    }

    testWidgets('no bottom navigation until there is something to navigate to', (
      tester,
    ) async {
      await pumpHome(tester);

      // The shell must not offer tabs it cannot render: while disconnected with
      // no cached sessions every tab collapses to the same connection screen.
      expect(find.byType(NavigationBar), findsNothing);
      expect(find.text('Not connected'), findsOneWidget);

      await tester.pumpWidget(const SizedBox.shrink());
    });

    testWidgets('shows pairing controls while disconnected', (tester) async {
      await pumpHome(tester);

      expect(find.text('Not connected'), findsOneWidget);
      expect(find.text('Pairing Code'), findsOneWidget);
      expect(find.text('Token'), findsNothing);
      expect(find.text(pairingGuidance), findsOneWidget);
      expect(find.text('Connect'), findsOneWidget);
      expect(find.text('Scan QR Code to Pair'), findsOneWidget);

      await tester.pumpWidget(const SizedBox.shrink());
    });

    testWidgets('a pasted pairing link pairs and saves the runtime', (
      tester,
    ) async {
      final storage = MemoryPairingStorage();
      backend.pairResult = runtimeInfo(
        runtimeId: 'runtime-a',
        hostname: 'office-mac',
        endpointId: 'endpoint-a',
        relayUrl: 'https://relay.example',
      );
      await pumpHome(tester, storage);

      const uri = 'smelt+iroh://endpoint-a/?code=123456';
      await tester.enterText(find.byType(TextField), uri);
      await tester.tap(find.text('Connect'));
      await tester.pumpAndSettle();

      expect(backend.calls, contains('pair $uri'));
      final saved = storage.value.activeRuntime;
      expect(saved?.runtimeId, 'runtime-a');
      expect(saved?.name, 'office-mac');
      expect(saved?.endpointId, 'endpoint-a');
      expect(saved?.relayUrl, 'https://relay.example');

      await tester.pumpWidget(const SizedBox.shrink());
    });

    testWidgets('a malformed pairing code is rejected before dialing', (
      tester,
    ) async {
      await pumpHome(tester);

      await tester.enterText(
        find.byType(TextField),
        'https://mac.local/?token=legacy',
      );
      await tester.tap(find.text('Connect'));
      await tester.pumpAndSettle();

      expect(find.text('Not a Smelt pairing code'), findsOneWidget);
      expect(backend.calls.where((call) => call.startsWith('pair')), isEmpty);

      await tester.pumpWidget(const SizedBox.shrink());
    });

    testWidgets('a failed pairing reconnects the previous runtime', (
      tester,
    ) async {
      final storage = MemoryPairingStorage(
        SavedRuntimeCollection(
          runtimes: [
            SavedRuntime.fromInfo(runtimeInfo(endpointId: 'endpoint-old')),
          ],
          activeRuntimeId: 'runtime-a',
        ),
      );
      backend.pairError = const SmeltError(
        kind: SmeltErrorKind.invalidPairing,
        message: 'The pairing code has expired',
      );
      await pumpHome(tester, storage);
      backend.calls.clear();

      await tester.enterText(
        find.byType(TextField),
        'smelt+iroh://endpoint-b/?code=1',
      );
      await tester.tap(find.text('Connect'));
      await tester.pumpAndSettle();

      expect(
        find.text('Pairing failed: The pairing code has expired'),
        findsOneWidget,
      );
      expect(backend.calls.last, 'connect endpoint-old');

      await tester.pumpWidget(const SizedBox.shrink());
    });

    testWidgets('launching reconnects the last runtime', (tester) async {
      final storage = MemoryPairingStorage(
        SavedRuntimeCollection(
          runtimes: [
            SavedRuntime.fromInfo(runtimeInfo(endpointId: 'endpoint-a')),
          ],
          activeRuntimeId: 'runtime-a',
        ),
      );
      await pumpHome(tester, storage);

      expect(backend.calls, contains('connect endpoint-a'));

      await tester.pumpWidget(const SizedBox.shrink());
    });

    testWidgets('a revoked device is told to scan a new pairing code', (
      tester,
    ) async {
      final storage = MemoryPairingStorage(
        SavedRuntimeCollection(
          runtimes: [SavedRuntime.fromInfo(runtimeInfo())],
          activeRuntimeId: 'runtime-a',
        ),
      );
      await pumpHome(tester, storage);

      backend.connection.add(
        const ConnectionEvent(
          state: RuntimeConnectionState.disconnected,
          endpointId: 'endpoint-a',
          error: SmeltError(
            kind: SmeltErrorKind.unpaired,
            message: 'device is not paired',
          ),
        ),
      );
      await tester.pumpAndSettle();

      const message =
          'This device was revoked or is not paired with office-mac. '
          'Scan a new pairing code to reconnect.';
      // 断开页和提示条各说一次：提示条带「扫码」按钮。
      expect(find.text(message), findsNWidgets(2));
      expect(find.widgetWithText(SnackBarAction, 'Scan'), findsOneWidget);

      await tester.pumpWidget(const SizedBox.shrink());
    });

    testWidgets(
      'cached sessions show the project tree with an offline banner',
      (tester) async {
        await pumpHome(tester);

        final savedAt = DateTime.now().subtract(const Duration(minutes: 5));
        backend.catalog.add(
          CatalogEvent(
            kind: CatalogEventKind.sessions,
            sessions: SessionList(
              sessions: [
                session(
                  id: 'acp-1',
                  title: 'Fix login',
                  projectRoot: '/code/smelt',
                  projectTitle: 'smelt',
                ),
              ],
              cached: true,
              savedAtMs: savedAt.millisecondsSinceEpoch,
            ),
          ),
        );
        await tester.pumpAndSettle();

        expect(find.byType(NavigationBar), findsOneWidget);
        await tester.tap(find.text('Projects'));
        await tester.pumpAndSettle();
        expect(find.text('smelt'), findsOneWidget);
        expect(find.text('Offline · Saved 5m ago'), findsOneWidget);

        await tester.pumpWidget(const SizedBox.shrink());
      },
    );
  });
}
