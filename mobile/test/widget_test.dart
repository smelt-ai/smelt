import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/main.dart';
import 'package:smelt_mobile/models/pairing_config.dart';
import 'package:smelt_mobile/models/saved_desktop.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/services/pairing_storage.dart';

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
      filterSessions(sessions, SessionListFilter.attention).map((s) => s.id),
      ['approval', 'failed'],
    );
    expect(
      filterSessions(sessions, SessionListFilter.running).map((s) => s.id),
      ['running'],
    );
    expect(filterSessions(sessions, SessionListFilter.all), sessions);
  });

  testWidgets('session filter bar fits a compact phone width', (tester) async {
    await tester.binding.setSurfaceSize(const Size(320, 160));
    addTearDown(() => tester.binding.setSurfaceSize(null));

    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: SessionFilterBar(
            selected: SessionListFilter.attention,
            attentionCount: 123,
            runningCount: 45,
            allCount: 234,
            onChanged: (_) {},
          ),
        ),
      ),
    );

    expect(find.text('Action 99+'), findsOneWidget);
    expect(find.text('Running 45'), findsOneWidget);
    expect(find.text('All 99+'), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('session filter labels stay on one line with large text', (
    tester,
  ) async {
    await tester.binding.setSurfaceSize(const Size(390, 160));
    addTearDown(() => tester.binding.setSurfaceSize(null));

    await tester.pumpWidget(
      MaterialApp(
        home: MediaQuery(
          data: const MediaQueryData(textScaler: TextScaler.linear(1.4)),
          child: Scaffold(
            body: SessionFilterBar(
              selected: SessionListFilter.running,
              attentionCount: 99,
              runningCount: 99,
              allCount: 99,
              onChanged: (_) {},
            ),
          ),
        ),
      ),
    );

    for (final label in ['Action 99', 'Running 99', 'All 99']) {
      final text = tester.widget<Text>(find.text(label));
      expect(text.maxLines, 1);
      expect(text.softWrap, isFalse);
    }
    expect(find.byIcon(Icons.priority_high), findsNothing);
    expect(find.byIcon(Icons.autorenew), findsNothing);
    expect(find.byIcon(Icons.forum_outlined), findsNothing);
    expect(tester.takeException(), isNull);
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
