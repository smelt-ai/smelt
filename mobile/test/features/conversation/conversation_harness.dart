import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/preferences.dart';
import 'package:smelt_mobile/data/stores.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';
import 'package:smelt_mobile/theme/theme.dart';

/// 演示后端加上记账：对话页调了什么、带了什么参数。可以把对话流换成手动推的。
class RecordingBackend extends DemoBackend {
  RecordingBackend({this.manualWatch = false});

  /// 为 true 时 [watchConversation] 返回 [watch]，由测试手动推事件。
  final bool manualWatch;
  final watch = StreamController<ConversationEvent>.broadcast();
  final calls = <String>[];
  final savedDrafts = <MessageDraft>[];

  /// 下一次发送抛出的错误；为空时发送成功。
  Object? sendError;
  int _ids = 0;

  @override
  Stream<ConversationEvent> watchConversation(
    String runtimeId,
    String sessionId,
  ) {
    calls.add('watch $sessionId');
    return _recordCancel(
      sessionId,
      manualWatch
          ? watch.stream
          : super.watchConversation(runtimeId, sessionId),
    );
  }

  /// 监听方取消订阅时记一笔。
  ///
  /// 演示快照是一次性的 [Stream]，自己就会结束；真订阅会一直开着，直到这一页取消。
  /// 上游结束不算退订。取消发生在 `dispose` 里，记账必须同步——widget 测试的假异步
  /// 不会把 `async*` 的 `finally` 跑完。
  Stream<ConversationEvent> _recordCancel(
    String sessionId,
    Stream<ConversationEvent> source,
  ) {
    late final StreamController<ConversationEvent> controller;
    StreamSubscription<ConversationEvent>? subscription;
    controller = StreamController<ConversationEvent>(
      onListen: () {
        subscription = source.listen(
          controller.add,
          onError: controller.addError,
          // 演示快照自己会结束。那不是页面退订，下游继续开着。
          onDone: () {},
        );
      },
      onCancel: () {
        calls.add('unwatch $sessionId');
        return subscription?.cancel();
      },
    );
    return controller.stream;
  }

  @override
  void markRead(String runtimeId, String sessionId) =>
      calls.add('read $sessionId');

  @override
  Future<bool> loadOlder(String runtimeId, String sessionId) async {
    calls.add('older $sessionId');
    return true;
  }

  @override
  String newRequestId() {
    final id = 'req-${_ids++}';
    calls.add('newRequestId $id');
    return id;
  }

  @override
  Future<void> sendMessage(
    String runtimeId,
    String sessionId, {
    required String requestId,
    required String text,
    required List<ImageInput> images,
  }) async {
    calls.add('send $requestId $text');
    final error = sendError;
    if (error != null) throw error;
  }

  @override
  Future<void> cancelTurn(String runtimeId, String sessionId) async {
    calls.add('cancel $sessionId');
  }

  @override
  Future<void> setConfigOption(
    String runtimeId,
    String sessionId, {
    required String configId,
    required String valueId,
    bool? boolean,
  }) async {
    calls.add('config $configId $valueId ${boolean ?? ''}'.trimRight());
  }

  @override
  Future<void> respondApproval(
    String runtimeId,
    String sessionId, {
    required String toolCallId,
    required String optionId,
  }) {
    calls.add('approve $toolCallId $optionId');
    return super.respondApproval(
      runtimeId,
      sessionId,
      toolCallId: toolCallId,
      optionId: optionId,
    );
  }

  @override
  Future<void> submitElicitation(
    String runtimeId,
    String sessionId,
    List<ElicitationTextInput> textValues,
  ) async {
    calls.add(
      'submit ${textValues.map((v) => '${v.fieldIndex}=${v.value}').join(',')}',
    );
  }

  @override
  Future<void> saveDraft(
    String runtimeId,
    String sessionId,
    MessageDraft d,
  ) async {
    savedDrafts.add(d);
    calls.add('saveDraft ${d.requestId ?? '-'}');
    await super.saveDraft(runtimeId, sessionId, d);
  }

  @override
  Future<void> clearDraft(String runtimeId, String sessionId) async {
    calls.add('clearDraft');
    await super.clearDraft(runtimeId, sessionId);
  }
}

/// 手机竖屏尺寸：对话页主要在手机上用，默认的 800×600 测不出窄屏的问题。
void usePhone(WidgetTester tester) {
  tester.view.physicalSize = const Size(390 * 3, 844 * 3);
  tester.view.devicePixelRatio = 3;
  addTearDown(tester.view.reset);
}

/// 测试宿主：主题、文案、数据根。[reduceMotion] 让「在做」的呼吸点停下来，
/// 不然 pumpAndSettle 永远等不到头。
Widget host(AppModel model, Widget child, {bool reduceMotion = true}) =>
    AppScope(
      model: model,
      child: MaterialApp(
        theme: buildGalleyTheme(Brightness.light),
        localizationsDelegates: L10n.localizationsDelegates,
        supportedLocales: L10n.supportedLocales,
        builder: (context, child) => MediaQuery(
          data: MediaQuery.of(
            context,
          ).copyWith(disableAnimations: reduceMotion),
          child: child!,
        ),
        home: child,
      ),
    );

AppModel startModel(RecordingBackend backend) =>
    AppModel(backend, PreferencesStore.memory())..start();

ConversationEntry entry(
  int index,
  EntryKind kind, {
  String text = '',
  bool thought = false,
  ToolCallView? tool,
}) => ConversationEntry(
  index: index,
  kind: kind,
  text: text,
  thought: thought,
  images: const [],
  toolCall: tool,
);

ToolCallView tool(
  String id,
  String title, {
  ToolKind kind = ToolKind.read,
  ToolStatus status = ToolStatus.completed,
  List<ToolOutput> output = const [],
}) => ToolCallView(
  id: id,
  title: title,
  kind: kind,
  status: status,
  output: output,
);

ConversationView view({
  ConversationPhase phase = ConversationPhase.idle,
  List<ConversationEntry>? entries,
  List<PermissionRequest> permissions = const [],
  Elicitation? elicitation,
  int? finalAnswerIndex,
  bool hasMoreBefore = false,
  int entriesOffset = 0,
  String? statusLine,
  bool compacting = false,
  ConversationNotice? notice,
}) => ConversationView(
  sessionId: 's-fleet',
  phase: phase,
  statusLine: statusLine,
  compacting: compacting,
  notice: notice,
  endReason: '',
  plan: const [],
  pendingPermissions: permissions,
  pendingElicitation: elicitation,
  entries:
      entries ??
      [
        entry(0, EntryKind.user, text: 'Fix the login bug'),
        entry(1, EntryKind.toolCall, tool: tool('t1', 'lib/auth.dart')),
        entry(
          2,
          EntryKind.assistant,
          text: 'Done, the session cookie was stale.',
        ),
      ],
  entriesOffset: entriesOffset,
  hasMoreBefore: hasMoreBefore,
  finalAnswerIndex: finalAnswerIndex,
  supportsImage: true,
  availableCommands: const [],
  configOptions: const [],
);

ConversationEvent snapshot(
  ConversationView v, {
  bool cached = false,
  int? savedAtMs,
}) => ConversationEvent(
  kind: ConversationEventKind.snapshot,
  view: v,
  cached: cached,
  savedAtMs: savedAtMs,
);
