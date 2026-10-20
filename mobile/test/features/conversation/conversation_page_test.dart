import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/features/conversation/conversation_page.dart';
import 'package:smelt_mobile/theme/galley.dart';
import 'package:smelt_mobile/ui/ticket.dart';
import 'package:smelt_mobile/ui/time.dart';

import 'conversation_harness.dart';

const _page = ConversationPage(runtimeId: 'rt-mbp', sessionId: 's-fleet');

Finder get _composer => find.byType(TextField).last;

void main() {
  group('快照渲染', () {
    testWidgets('用户消息、助手正文、工具调用都画出来，最终回答带复制', (tester) async {
      usePhone(tester);
      final backend = RecordingBackend(manualWatch: true);
      await tester.pumpWidget(host(startModel(backend), _page));
      await tester.pump();
      expect(backend.calls, contains('watch s-fleet'));
      expect(backend.calls, contains('read s-fleet'), reason: '打开即标记已读');
      expect(find.text('Opening conversation…'), findsOneWidget);

      backend.watch.add(snapshot(view(finalAnswerIndex: 2)));
      await tester.pumpAndSettle();

      expect(find.text('Fix the login bug'), findsOneWidget);
      expect(find.text('Done, the session cookie was stale.'), findsOneWidget);
      expect(find.text('Read'), findsOneWidget);
      expect(find.text('lib/auth.dart'), findsOneWidget);
      expect(find.byTooltip('Copy answer'), findsOneWidget);
      // 实时快照、机器在线：不出离线横幅。
      expect(find.textContaining('Offline'), findsNothing);
    });

    testWidgets('演示对话：连续的工具调用收成一组，失败的单独标出', (tester) async {
      usePhone(tester);
      final backend = RecordingBackend();
      await tester.pumpWidget(host(startModel(backend), _page));
      await tester.pumpAndSettle();

      expect(find.textContaining('多运行时连接状态机'), findsWidgets);
      expect(find.text('Plan · 2/4'), findsOneWidget);
      // 模型与配置项是输入框上方的芯片。
      expect(find.text('Claude Sonnet 4.5'), findsOneWidget);
      expect(find.text('Context 42% · 84k of 200k tokens'), findsOneWidget);

      // 编辑 + 失败的命令收成一组，往上翻才看得到。
      await tester.dragUntilVisible(
        find.text('1 failed'),
        find.byType(ListView),
        const Offset(0, 200),
      );
      await tester.pumpAndSettle();
      expect(find.text('2 tool calls'), findsWidgets);

      // 点开那一组才看得到失败命令的输出。
      expect(find.text('Exit code 101'), findsNothing);
      await tester.tap(find.text('1 failed'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('cargo test -p smelt-mobile fleet::').last);
      await tester.pumpAndSettle();
      expect(find.text('Exit code 101'), findsOneWidget);
    });

    testWidgets('空对话、接不上的对话各有说法，接不上可以重试', (tester) async {
      usePhone(tester);
      final backend = RecordingBackend(manualWatch: true);
      await tester.pumpWidget(host(startModel(backend), _page));
      await tester.pump();

      backend.watch.add(
        const ConversationEvent(
          kind: ConversationEventKind.unavailable,
          cached: false,
          message: 'Not running on the computer',
        ),
      );
      await tester.pumpAndSettle();
      expect(find.text("Can't open this conversation"), findsOneWidget);
      expect(find.text('Not running on the computer'), findsOneWidget);

      await tester.tap(find.text('Retry'));
      await tester.pump();
      expect(backend.calls.where((c) => c == 'watch s-fleet'), hasLength(2));

      backend.watch.add(snapshot(view(entries: const [])));
      await tester.pumpAndSettle();
      expect(find.text('No messages yet'), findsOneWidget);
    });

    testWidgets('离线缓存的快照诚实标出时间，实时数据到了就收起', (tester) async {
      usePhone(tester);
      final backend = RecordingBackend(manualWatch: true);
      await tester.pumpWidget(host(startModel(backend), _page));
      await tester.pump();

      final saved = DateTime(2026, 3, 4, 9, 7);
      backend.watch.add(
        snapshot(view(), cached: true, savedAtMs: saved.millisecondsSinceEpoch),
      );
      await tester.pumpAndSettle();
      expect(
        find.text('Syncing… · showing data from ${formatClock(saved)}'),
        findsOneWidget,
      );

      backend.watch.add(snapshot(view()));
      await tester.pumpAndSettle();
      expect(find.textContaining('Offline'), findsNothing);
    });
  });

  group('停靠单据', () {
    testWidgets('待批审批停靠在输入框上方，点允许会调 respondApproval，盖章后收起', (tester) async {
      usePhone(tester);
      final backend = RecordingBackend();
      await tester.pumpWidget(
        host(
          startModel(backend),
          const ConversationPage(runtimeId: 'rt-mbp', sessionId: 's-md'),
          // 要看盖章：不减少动效。这段对话里没有「在做」的呼吸点。
          reduceMotion: false,
        ),
      );
      await tester.pump();
      await tester.pump(const Duration(milliseconds: 100));

      final allow = find.text('Allow once');
      expect(allow, findsOneWidget);
      expect(find.byType(TicketPaper), findsOneWidget);
      expect(find.text('cargo test -p smelt-acp-view'), findsWidgets);
      // 单据在输入框上方。
      expect(
        tester.getRect(find.byType(TicketPaper)).bottom,
        lessThanOrEqualTo(tester.getRect(_composer).top),
      );
      expect(find.text('Handle the ticket above first'), findsOneWidget);

      await tester.tap(allow);
      await tester.pump();
      expect(backend.calls, contains('approve tc-1 allow'));

      // 内核收下后先盖章（停靠单据上一枚，时间线那条工具调用下面一枚）……
      await tester.pump(const Duration(milliseconds: 300));
      expect(find.textContaining('Allowed ·'), findsWidgets);
      // ……章落下再收起。
      await tester.pump(Motion.stamp + Motion.ticket);
      await tester.pump(Motion.ticket);
      expect(find.byType(TicketPaper), findsNothing);
      expect(find.text('Allow once'), findsNothing);
      // 单据收起了，决定不消失：时间线里那条工具调用下面还留着章。
      expect(find.textContaining('Allowed ·'), findsOneWidget);
    });

    testWidgets('单据可以收成一条单据头，再点展开', (tester) async {
      usePhone(tester);
      final backend = RecordingBackend();
      await tester.pumpWidget(
        host(
          startModel(backend),
          const ConversationPage(runtimeId: 'rt-mbp', sessionId: 's-md'),
        ),
      );
      await tester.pumpAndSettle();
      expect(find.text('Allow once'), findsOneWidget);

      await tester.tap(find.text('Claude wants to run'));
      await tester.pumpAndSettle();
      expect(find.text('Allow once'), findsNothing);
      expect(find.byType(TicketPaper), findsOneWidget);

      await tester.tap(find.text('Claude wants to run'));
      await tester.pumpAndSettle();
      expect(find.text('Allow once'), findsOneWidget);
    });

    testWidgets('离线缓存里的审批不停靠：拿旧数据去批是错的', (tester) async {
      usePhone(tester);
      final backend = RecordingBackend(manualWatch: true);
      await tester.pumpWidget(host(startModel(backend), _page));
      await tester.pump();
      backend.watch.add(
        snapshot(
          view(
            phase: ConversationPhase.awaitingApproval,
            permissions: const [
              PermissionRequest(
                toolCallId: 'tc-9',
                question: 'Run the migration',
                options: [
                  PermissionOption(
                    optionId: 'ok',
                    name: '',
                    kind: PermissionOptionKind.allowOnce,
                  ),
                ],
                details: ApprovalDetails(kind: ApprovalKind.generic),
              ),
            ],
          ),
          cached: true,
        ),
      );
      await tester.pumpAndSettle();
      expect(find.byType(TicketPaper), findsNothing);
    });

    testWidgets('询问里在手机上打的字随提交一起交给运行时', (tester) async {
      usePhone(tester);
      final backend = RecordingBackend(manualWatch: true);
      await tester.pumpWidget(host(startModel(backend), _page));
      await tester.pump();
      backend.watch.add(
        snapshot(
          view(
            phase: ConversationPhase.waitingForUser,
            elicitation: Elicitation(
              message: 'Which branch?',
              isReady: false,
              fields: [
                ElicitationField(
                  key: 'branch',
                  title: 'Branch',
                  required_: true,
                  allowCustomInput: false,
                  kind: ElicitationFieldKind.text,
                  options: const [],
                  secret: false,
                  chosen: Uint32List(0),
                ),
              ],
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();
      expect(find.text('Which branch?'), findsOneWidget);

      await tester.enterText(find.byType(TextFormField), 'main');
      await tester.pump();
      await tester.tap(find.text('Send answer'));
      await tester.pumpAndSettle();
      expect(backend.calls, contains('submit 0=main'));
      await tester.pump(const Duration(seconds: 9));
    });
  });

  group('发送', () {
    Future<RecordingBackend> open(WidgetTester tester) async {
      usePhone(tester);
      final backend = RecordingBackend(manualWatch: true);
      await tester.pumpWidget(host(startModel(backend), _page));
      await tester.pump();
      backend.watch.add(snapshot(view()));
      await tester.pumpAndSettle();
      return backend;
    }

    testWidgets('发送时生成 requestId，先连同 id 存草稿，送到后清掉草稿', (tester) async {
      usePhone(tester);
      final backend = await open(tester);

      await tester.enterText(_composer, 'ship it');
      await tester.pump();
      await tester.tap(find.byTooltip('Send'));
      await tester.pumpAndSettle();

      final sends = backend.calls.where((c) => c.startsWith('send ')).toList();
      expect(sends, ['send req-0 ship it']);
      final order = backend.calls;
      expect(
        order.indexOf('newRequestId req-0'),
        lessThan(order.indexOf('saveDraft req-0')),
      );
      expect(
        order.indexOf('saveDraft req-0'),
        lessThan(order.indexOf('send req-0 ship it')),
        reason: '等确认时被杀掉，下次打开还知道这条没确认',
      );
      expect(order.last, 'clearDraft');
      expect(await backend.loadDraft('rt-mbp', 's-fleet'), isNull);
      expect(tester.widget<TextField>(_composer).controller!.text, isEmpty);
    });

    testWidgets('没等到确认：草稿连同 id 留着，重发先问，沿用同一个 id', (tester) async {
      usePhone(tester);
      final backend = await open(tester);
      backend.sendError = const SendMessageError(
        kind: SendFailureKind.unknown,
        message: 'timed out',
      );

      await tester.enterText(_composer, 'ship it');
      await tester.pump();
      await tester.tap(find.byTooltip('Send'));
      await tester.pumpAndSettle();

      expect(find.textContaining("Delivery wasn't confirmed"), findsOneWidget);
      final draft = await backend.loadDraft('rt-mbp', 's-fleet');
      expect(draft?.content, 'ship it');
      expect(draft?.requestId, 'req-0');

      backend.sendError = null;
      await tester.tap(find.byTooltip('Send'));
      await tester.pumpAndSettle();
      expect(find.text('Send again?'), findsOneWidget);
      await tester.tap(find.text('Send again'));
      await tester.pumpAndSettle();

      final sends = backend.calls.where((c) => c.startsWith('send ')).toList();
      expect(sends, ['send req-0 ship it', 'send req-0 ship it']);
      expect(
        backend.calls.where((c) => c.startsWith('newRequestId')),
        hasLength(1),
        reason: '同一个 id 让运行时去重',
      );
      expect(await backend.loadDraft('rt-mbp', 's-fleet'), isNull);
    });

    testWidgets('运行时明确没收下：草稿留着、去掉 id，重发不用问', (tester) async {
      usePhone(tester);
      final backend = await open(tester);
      backend.sendError = const SendMessageError(
        kind: SendFailureKind.rejected,
        message: 'agent is busy',
      );

      await tester.enterText(_composer, 'ship it');
      await tester.pump();
      await tester.tap(find.byTooltip('Send'));
      await tester.pumpAndSettle();

      expect(find.text('Not sent: agent is busy'), findsOneWidget);
      final draft = await backend.loadDraft('rt-mbp', 's-fleet');
      expect(draft?.content, 'ship it');
      expect(draft?.requestId, isNull);

      backend.sendError = null;
      await tester.tap(find.byTooltip('Send'));
      await tester.pumpAndSettle();
      expect(find.text('Send again?'), findsNothing);
      expect(backend.calls.where((c) => c.startsWith('send ')).toList(), [
        'send req-0 ship it',
        'send req-1 ship it',
      ]);
    });

    testWidgets('改了内容就是新的提交：没确认的 id 作废', (tester) async {
      usePhone(tester);
      final backend = await open(tester);
      backend.sendError = const SendMessageError(
        kind: SendFailureKind.unknown,
        message: 'timed out',
      );
      await tester.enterText(_composer, 'ship it');
      await tester.pump();
      await tester.tap(find.byTooltip('Send'));
      await tester.pumpAndSettle();

      backend.sendError = null;
      await tester.enterText(_composer, 'ship it now');
      await tester.pump();
      expect(find.textContaining("Delivery wasn't confirmed"), findsNothing);
      await tester.tap(find.byTooltip('Send'));
      await tester.pumpAndSettle();
      expect(find.text('Send again?'), findsNothing);
      expect(backend.calls, contains('send req-1 ship it now'));
    });

    testWidgets('打开时恢复草稿；挂着 id 的草稿提示没确认', (tester) async {
      usePhone(tester);
      final backend = RecordingBackend(manualWatch: true);
      await backend.saveDraft(
        'rt-mbp',
        's-fleet',
        const MessageDraft(content: 'half typed', images: [], requestId: 'r-7'),
      );
      await tester.pumpWidget(host(startModel(backend), _page));
      await tester.pump();
      backend.watch.add(snapshot(view()));
      await tester.pumpAndSettle();

      expect(
        tester.widget<TextField>(_composer).controller!.text,
        'half typed',
      );
      expect(find.textContaining("Delivery wasn't confirmed"), findsOneWidget);
    });

    testWidgets('离开页面时补写还没落盘的草稿，并停掉订阅', (tester) async {
      usePhone(tester);
      final backend = await open(tester);
      await tester.enterText(_composer, 'draft in progress');
      await tester.pump();
      expect(backend.calls.where((c) => c.startsWith('saveDraft')), isEmpty);

      await tester.pumpWidget(host(startModel(backend), const SizedBox()));
      await tester.pump();
      expect(backend.calls, contains('saveDraft -'));
      expect(backend.calls, contains('unwatch s-fleet'));
      expect(
        (await backend.loadDraft('rt-mbp', 's-fleet'))?.content,
        'draft in progress',
      );
    });
  });

  group('回合进行中', () {
    testWidgets('框里没字时发送钮变成停止，点了停掉回合', (tester) async {
      usePhone(tester);
      final backend = RecordingBackend(manualWatch: true);
      await tester.pumpWidget(host(startModel(backend), _page));
      await tester.pump();
      backend.watch.add(snapshot(view(phase: ConversationPhase.thinking)));
      await tester.pumpAndSettle();

      expect(find.byTooltip('Stop this turn'), findsOneWidget);
      expect(find.byTooltip('Send'), findsNothing);
      await tester.tap(find.byTooltip('Stop this turn'));
      await tester.pump();
      expect(backend.calls, contains('cancel s-fleet'));

      // 回合进行中也能接着发：打了字就换回发送。
      await tester.enterText(_composer, 'also check logout');
      await tester.pump();
      expect(find.byTooltip('Send'), findsOneWidget);
      expect(find.byTooltip('Stop this turn'), findsNothing);
    });

    testWidgets('空闲时没有停止钮', (tester) async {
      usePhone(tester);
      final backend = RecordingBackend(manualWatch: true);
      await tester.pumpWidget(host(startModel(backend), _page));
      await tester.pump();
      backend.watch.add(snapshot(view()));
      await tester.pumpAndSettle();
      expect(find.byTooltip('Stop this turn'), findsNothing);
      expect(find.byTooltip('Send'), findsOneWidget);
    });
  });

  group('滚动', () {
    List<ConversationEntry> many(int n) => [
      for (var i = 0; i < n; i++)
        entry(
          i,
          i.isEven ? EntryKind.user : EntryKind.assistant,
          text: 'message $i\nsecond line\nthird line',
        ),
    ];

    testWidgets('滚到顶自动翻更早的一页', (tester) async {
      usePhone(tester);
      final backend = RecordingBackend(manualWatch: true);
      await tester.pumpWidget(host(startModel(backend), _page));
      await tester.pump();
      backend.watch.add(
        snapshot(
          view(entries: many(40), hasMoreBefore: true, entriesOffset: 100),
        ),
      );
      await tester.pumpAndSettle();
      expect(backend.calls.where((c) => c.startsWith('older')), isEmpty);

      await tester.fling(find.byType(ListView), const Offset(0, 6000), 3000);
      // 在翻的时候加载圈一直转，不能 pumpAndSettle。
      for (var i = 0; i < 20; i++) {
        await tester.pump(const Duration(milliseconds: 100));
      }
      expect(backend.calls.where((c) => c.startsWith('older')), hasLength(1));
      expect(find.text('Load earlier messages'), findsNothing);
    });

    testWidgets('不在最新处时出「回到最新」，点了回去', (tester) async {
      usePhone(tester);
      final backend = RecordingBackend(manualWatch: true);
      await tester.pumpWidget(host(startModel(backend), _page));
      await tester.pump();
      backend.watch.add(snapshot(view(entries: many(40))));
      await tester.pumpAndSettle();

      final jump = find.byKey(const ValueKey('jump-to-latest'));
      final opacity = find.ancestor(
        of: jump,
        matching: find.byType(AnimatedOpacity),
      );
      expect(tester.widget<AnimatedOpacity>(opacity).opacity, 0);

      await tester.drag(find.byType(ListView), const Offset(0, 600));
      await tester.pumpAndSettle();
      expect(tester.widget<AnimatedOpacity>(opacity).opacity, 1);

      await tester.tap(find.text('Back to latest'));
      await tester.pumpAndSettle();
      expect(tester.widget<AnimatedOpacity>(opacity).opacity, 0);
    });
  });
}
