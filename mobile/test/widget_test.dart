import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/main.dart';
import 'package:smelt_mobile/models/acp_snapshot.dart';
import 'package:smelt_mobile/models/pairing_config.dart';
import 'package:smelt_mobile/models/saved_desktop.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/pages/console_page.dart';
import 'package:smelt_mobile/services/pairing_storage.dart';
import 'package:smelt_mobile/services/pending_actions_controller.dart';
import 'package:smelt_mobile/widgets/approval_card.dart';
import 'package:smelt_mobile/widgets/session_row.dart';

class MemoryPairingStorage implements PairingStorage {
  SavedDesktopCollection value = const SavedDesktopCollection();

  @override
  Future<SavedDesktopCollection> load() async => value;

  @override
  Future<SavedDesktopCollection> save(PairingConfig pairing) async {
    final desktop = SavedDesktop.create(pairing);
    value = SavedDesktopCollection(
      desktops: [
        desktop,
        ...value.desktops.where((item) => item.id != desktop.id),
      ],
      activeDesktopId: desktop.id,
    );
    return value;
  }

  @override
  Future<SavedDesktopCollection> setActive(String desktopId) async {
    value = SavedDesktopCollection(
      desktops: value.desktops,
      activeDesktopId: desktopId,
    );
    return value;
  }

  @override
  Future<SavedDesktopCollection> rename(String desktopId, String name) async {
    value = SavedDesktopCollection(
      desktops: value.desktops
          .map(
            (item) => item.id == desktopId ? item.copyWith(name: name) : item,
          )
          .toList(),
      activeDesktopId: value.activeDesktopId,
    );
    return value;
  }

  @override
  Future<SavedDesktopCollection> remove(String desktopId) async {
    final remaining = value.desktops
        .where((item) => item.id != desktopId)
        .toList();
    value = SavedDesktopCollection(
      desktops: remaining,
      activeDesktopId: value.activeDesktopId == desktopId
          ? remaining.firstOrNull?.id
          : value.activeDesktopId,
    );
    return value;
  }
}

void main() {
  test(
    'session list uses ACP conversation text instead of agent CLI label',
    () {
      const session = SessionSummary(
        id: 'acp-1',
        title: '修复移动端项目列表',
        phase: 'idle',
        agent: 'codex',
        detail: '  正在检查列表数据  ',
      );

      expect(sessionListTitle(session), '修复移动端项目列表');
      expect(sessionListSubtitle(session), '正在检查列表数据');
    },
  );

  test('session list omits empty detail and has an ACP fallback title', () {
    const session = SessionSummary(
      id: 'acp-2',
      title: '  ',
      phase: 'idle',
      agent: 'other',
      detail: ' ',
    );

    expect(sessionListTitle(session), 'ACP conversation');
    expect(sessionListSubtitle(session), isNull);
  });

  test('terminal sessions use a terminal fallback title', () {
    const session = SessionSummary(
      id: 'terminal-1',
      kind: SessionKind.terminal,
      title: ' ',
      phase: 'idle',
      agent: 'codex',
    );

    expect(sessionListTitle(session), 'Terminal');
  });

  test('session filters separate actionable and running conversations', () {
    const approval = SessionSummary(
      id: 'approval',
      title: 'Approve command',
      phase: 'awaiting_approval',
      status: 'waiting_approval',
      agent: 'codex',
    );
    const running = SessionSummary(
      id: 'running',
      title: 'Implement feature',
      phase: 'running',
      status: 'running',
      agent: 'codex',
    );
    const idle = SessionSummary(
      id: 'idle',
      title: 'Finished task',
      phase: 'idle',
      agent: 'codex',
    );
    const failed = SessionSummary(
      id: 'failed',
      title: 'Failed task',
      phase: 'failed',
      status: 'done',
      agent: 'codex',
      attention: LifecycleAttention(
        sessionId: 'failed',
        title: 'Failed',
        message: 'Agent stopped',
        kind: 'failure',
      ),
    );
    const sessions = [approval, running, idle, failed];

    expect(
      sessions.where(sessionNeedsAction).map((s) => s.id),
      ['approval', 'failed'],
    );
    expect(sessions.where(sessionIsRunning).map((s) => s.id), ['running']);
  });

  test('attention notifications stay hidden for the active session', () {
    const completed = LifecycleAttention(
      sessionId: 'current',
      title: 'Completed',
      message: 'Task completed',
      kind: 'success',
    );
    const input = LifecycleAttention(
      sessionId: 'current',
      title: 'Waiting for you',
      message: 'Agent needs input',
      kind: 'input',
    );

    for (final attention in [completed, input]) {
      expect(
        shouldShowAttentionNotification(
          attention: attention,
          activeSessionId: 'current',
          subscribedSessionId: null,
        ),
        isFalse,
      );
    }
  });

  test('attention notifications still surface for other sessions', () {
    const completed = LifecycleAttention(
      sessionId: 'background',
      title: 'Completed',
      message: 'Task completed',
      kind: 'success',
    );
    const input = LifecycleAttention(
      sessionId: 'background',
      title: 'Waiting for you',
      message: 'Agent needs input',
      kind: 'input',
    );

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
    final snapshots = StreamController<String>.broadcast();
    addTearDown(sessions.close);
    addTearDown(snapshots.close);

    final cache = <String, AcpSnapshot>{
      'acp-b': AcpSnapshot(
        entries: const [],
        phase: const AcpPhaseAwaitingApproval(),
        pendingPermissions: const [
          PendingPermission(
            toolCallId: 'tool-b',
            question: 'Run this command?',
            options: [
              PermissionOption(
                optionId: 'reject',
                name: 'Deny',
                kind: 'RejectOnce',
              ),
              PermissionOption(
                optionId: 'allow',
                name: 'Allow',
                kind: 'AllowOnce',
              ),
              PermissionOption(
                optionId: 'always',
                name: 'Always allow',
                kind: 'AllowAlways',
              ),
            ],
            details: ApprovalDetailsCommand(
              command: 'cargo test -p smelt-ui --no-fail-fast',
              cwd: '~/code/smelt',
            ),
          ),
        ],
      ),
    };

    final controller = PendingActionsController(
      sessions: sessions.stream,
      initialSessions: const [],
      snapshotUpdates: snapshots.stream,
      lookupSnapshot: (id) => cache[id],
      requestDetails: (_) => true,
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
      const SessionSummary(
        id: 'acp-b',
        title: 'refactor gateway',
        phase: 'idle',
        status: 'waiting_approval',
        agent: 'codex',
      ),
    ]);
    snapshots.add('acp-b');
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
    expect(
      responded,
      ['acp-b/tool-b/allow'],
      reason: 'deciding must not require opening the session',
    );
  });

  testWidgets('always-allow is demoted out of the primary button row', (
    tester,
  ) async {
    final responded = <String>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: ApprovalCard(
            permission: const PendingPermission(
              toolCallId: 'tool-1',
              question: 'Run this command?',
              options: [
                PermissionOption(
                  optionId: 'reject',
                  name: 'Deny',
                  kind: 'RejectOnce',
                ),
                PermissionOption(
                  optionId: 'allow',
                  name: 'Allow',
                  kind: 'AllowOnce',
                ),
                PermissionOption(
                  optionId: 'always',
                  name: 'Always allow',
                  kind: 'AllowAlways',
                ),
              ],
            ),
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
            state: WsState.reconnecting,
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
    const waiting = SessionSummary(
      id: 'approval',
      title: 'Needs approval',
      phase: 'running',
      status: 'waiting_approval',
      agent: 'codex',
    );
    const idle = SessionSummary(
      id: 'idle',
      title: 'Finished task',
      phase: 'idle',
      agent: 'codex',
    );
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
                initialSessions: const [idle],
              ),
            ],
          ),
        ),
      ),
    );

    // 没人等我的时候不占地方。
    expect(find.byType(IconButton), findsNothing);

    sessions.add(const [waiting, idle]);
    await tester.pump();
    expect(find.text('1'), findsOneWidget);

    await tester.tap(find.byIcon(Icons.notification_important_outlined));
    expect(taps, 1);

    sessions.add(const [idle]);
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
            state: WsState.disconnected,
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

  testWidgets('desktop rename dialog can be saved repeatedly', (tester) async {
    final renamed = <String>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Builder(
          builder: (context) => Scaffold(
            body: FilledButton(
              onPressed: () async {
                final name = await showDesktopRenameDialog(context, 'Desktop');
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

  testWidgets('no bottom navigation until there is something to navigate to', (
    tester,
  ) async {
    await tester.pumpWidget(SmeltApp(pairingStorage: MemoryPairingStorage()));
    await tester.pumpAndSettle();

    // The shell must not offer tabs it cannot render: while disconnected with
    // no cached sessions every tab collapses to the same connection screen.
    expect(find.byType(NavigationBar), findsNothing);
    expect(find.text('Not connected'), findsOneWidget);

    await tester.pumpWidget(const SizedBox.shrink());
  });

  testWidgets('shows pairing controls while disconnected', (tester) async {
    await tester.pumpWidget(SmeltApp(pairingStorage: MemoryPairingStorage()));
    await tester.pumpAndSettle();

    expect(find.text('Not connected'), findsOneWidget);
    expect(find.text('Pairing Code'), findsOneWidget);
    expect(find.text('Gateway Endpoint'), findsNothing);
    expect(find.text('Token'), findsNothing);
    expect(find.text('Connect'), findsOneWidget);
    expect(find.text('Scan QR Code to Pair'), findsOneWidget);

    await tester.pumpWidget(const SizedBox.shrink());
  });
}
