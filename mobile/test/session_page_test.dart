import 'dart:async';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/main.dart';
import 'package:smelt_mobile/services/message_draft_store.dart';
import 'package:smelt_mobile/services/runtime_service.dart';

import 'support/runtime_fixtures.dart';

class _MemoryDraftStore implements MessageDraftStore {
  final drafts = <String, MessageDraft>{};

  @override
  Future<MessageDraft?> load(String sessionId) async => drafts[sessionId];

  @override
  Future<void> save(String sessionId, MessageDraft draft) async {
    if (draft.isEmpty) {
      drafts.remove(sessionId);
    } else {
      drafts[sessionId] = draft;
    }
  }

  @override
  Future<void> delete(String sessionId) async => drafts.remove(sessionId);

  @override
  Future<bool> resolveRequest(
    String sessionId,
    String requestId, {
    required bool succeeded,
  }) async {
    final draft = drafts[sessionId];
    if (draft?.requestId != requestId) return false;
    if (succeeded) {
      drafts.remove(sessionId);
    } else {
      drafts[sessionId] = draft!.copyWith(clearRequestId: true);
    }
    return true;
  }
}

ConversationEntry _entry(int index, EntryKind kind, String text) =>
    ConversationEntry(
      index: index,
      kind: kind,
      text: text,
      thought: false,
      images: const [],
    );

ConversationView _view({
  ConversationPhase phase = ConversationPhase.idle,
  List<ConversationEntry>? entries,
  Elicitation? elicitation,
  int? finalAnswerIndex,
}) => ConversationView(
  sessionId: 'acp-1',
  phase: phase,
  endReason: '',
  plan: const [],
  pendingPermissions: const [],
  pendingElicitation: elicitation,
  entries:
      entries ??
      [
        _entry(0, EntryKind.user, 'Fix the login bug'),
        _entry(1, EntryKind.assistant, 'Done, the session cookie was stale.'),
      ],
  entriesOffset: 0,
  hasMoreBefore: false,
  finalAnswerIndex: finalAnswerIndex,
  supportsImage: true,
  availableCommands: const [],
  configOptions: const [],
);

ConversationEvent _snapshot(
  ConversationView view, {
  bool cached = false,
  int? savedAtMs,
}) => ConversationEvent(
  kind: ConversationEventKind.snapshot,
  view: view,
  cached: cached,
  savedAtMs: savedAtMs,
);

void main() {
  late FakeRuntimeBackend backend;
  late _MemoryDraftStore drafts;

  setUp(() {
    backend = FakeRuntimeBackend();
    drafts = _MemoryDraftStore();
  });

  tearDown(() async {
    await runtimeService.dispose();
    await backend.close();
  });

  Future<void> pumpPage(WidgetTester tester) async {
    // 在测试的 zone 里订阅，事件才会随 pump 送达。
    runtimeService = RuntimeService(backend)..start();
    backend.connection.add(
      const ConnectionEvent(state: RuntimeConnectionState.connected),
    );
    await tester.pump();
    await tester.pumpWidget(
      MaterialApp(
        home: SessionPage(
          session: session(id: 'acp-1', title: 'Login bug'),
          messageDraftStore: drafts,
        ),
      ),
    );
    await tester.pump();
  }

  StreamController<ConversationEvent> watch() => backend.watches.last.events;

  testWidgets('renders the view pushed by the runtime', (tester) async {
    await pumpPage(tester);
    expect(backend.calls, contains('watch acp-1'));

    watch().add(_snapshot(_view(finalAnswerIndex: 1)));
    await tester.pumpAndSettle();

    expect(find.text('Fix the login bug'), findsOneWidget);
    expect(find.text('Done, the session cookie was stale.'), findsOneWidget);
    // 最终回答带复制按钮。
    expect(find.byTooltip('Copy answer'), findsOneWidget);
    expect(find.byType(CachedConnectionBar), findsNothing);
  });

  testWidgets('a cached view says it is showing saved data', (tester) async {
    await pumpPage(tester);
    final savedAt = DateTime.now().subtract(const Duration(minutes: 2));

    watch().add(
      _snapshot(
        _view(),
        cached: true,
        savedAtMs: savedAt.millisecondsSinceEpoch,
      ),
    );
    // 横幅上的加载圈一直在转，不能 pumpAndSettle。
    await tester.pump();
    await tester.pump();
    expect(find.text('Refreshing · Saved 2m ago'), findsOneWidget);

    watch().add(_snapshot(_view()));
    await tester.pumpAndSettle();
    expect(find.byType(CachedConnectionBar), findsNothing);
  });

  testWidgets('an unavailable conversation stops spinning and can retry', (
    tester,
  ) async {
    await pumpPage(tester);

    watch().add(
      const ConversationEvent(
        kind: ConversationEventKind.unavailable,
        cached: false,
        message: 'Not running on the computer',
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('Not running on the computer'), findsOneWidget);

    await tester.tap(find.text('Try again'));
    await tester.pump();
    expect(backend.calls.where((call) => call == 'watch acp-1'), hasLength(2));
  });

  testWidgets('a delivery that may have landed is resent only after asking', (
    tester,
  ) async {
    await pumpPage(tester);
    watch().add(_snapshot(_view()));
    await tester.pumpAndSettle();

    backend.sendError = const SendMessageError(
      kind: SendFailureKind.unknown,
      message: 'timed out',
    );
    await tester.enterText(find.byType(TextField), 'ship it');
    await tester.pump();
    await tester.tap(find.byTooltip('Send message'));
    await tester.pumpAndSettle();

    final firstSend = backend.calls.lastWhere((c) => c.startsWith('send'));
    final requestId = firstSend.split(' ')[2];
    expect(
      find.text('Delivery was not confirmed. Review and retry.'),
      findsOneWidget,
    );
    // 草稿连同请求 id 留着：App 被杀了下次也知道这条没确认。
    expect(drafts.drafts['acp-1']?.content, 'ship it');
    expect(drafts.drafts['acp-1']?.requestId, requestId);

    backend.sendError = null;
    await tester.tap(find.byTooltip('Send message'));
    await tester.pumpAndSettle();
    expect(find.text('Send again?'), findsOneWidget);
    await tester.tap(find.text('Send again'));
    await tester.pumpAndSettle();

    final resend = backend.calls.lastWhere((c) => c.startsWith('send'));
    expect(resend, 'send acp-1 $requestId ship it', reason: '同一个 id 让运行时去重');
    expect(drafts.drafts['acp-1'], isNull);
    expect(find.text('ship it'), findsNothing);
  });

  testWidgets('a rejected message keeps the draft and resends freely', (
    tester,
  ) async {
    await pumpPage(tester);
    watch().add(_snapshot(_view()));
    await tester.pumpAndSettle();

    backend.sendError = const SendMessageError(
      kind: SendFailureKind.rejected,
      message: 'agent is busy',
    );
    await tester.enterText(find.byType(TextField), 'ship it');
    await tester.pump();
    await tester.tap(find.byTooltip('Send message'));
    await tester.pumpAndSettle();

    expect(find.text('Message was not sent: agent is busy'), findsOneWidget);
    expect(drafts.drafts['acp-1']?.content, 'ship it');
    expect(drafts.drafts['acp-1']?.requestId, isNull);

    backend.sendError = null;
    await tester.tap(find.byTooltip('Send message'));
    await tester.pumpAndSettle();
    expect(find.text('Send again?'), findsNothing);
    expect(
      backend.calls.where((call) => call.startsWith('send')),
      hasLength(2),
    );
  });

  testWidgets('submitting a question sends the text typed on the phone', (
    tester,
  ) async {
    await pumpPage(tester);
    watch().add(
      _snapshot(
        _view(
          phase: ConversationPhase.waitingForUser,
          elicitation: Elicitation(
            message: 'Which branch?',
            fields: [
              ElicitationField(
                key: 'branch',
                title: 'Branch',
                required_: true,
                allowCustomInput: false,
                kind: ElicitationFieldKind.text,
                options: [],
                secret: false,
                chosen: Uint32List(0),
              ),
            ],
            isReady: false,
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();

    await tester.enterText(find.byType(TextFormField), 'main');
    await tester.pump();
    await tester.tap(find.text('Submit'));
    await tester.pump();

    expect(backend.calls, contains('submit 1'));
  });
}
