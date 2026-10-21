import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/ui/status.dart';
import 'package:image/image.dart' as img;
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/features/conversation/conversation_images.dart';
import 'package:smelt_mobile/features/conversation/conversation_logic.dart';
import 'package:smelt_mobile/features/conversation/entry_views.dart';
import 'package:smelt_mobile/features/conversation/tool_views.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';
import 'package:smelt_mobile/theme/theme.dart';

import 'conversation_harness.dart' as h;

Widget _host(Widget child) => MaterialApp(
  theme: buildGalleyTheme(Brightness.dark),
  localizationsDelegates: L10n.localizationsDelegates,
  supportedLocales: L10n.supportedLocales,
  builder: (context, child) => MediaQuery(
    data: MediaQuery.of(context).copyWith(disableAnimations: true),
    child: child!,
  ),
  home: Scaffold(body: SingleChildScrollView(child: child)),
);

Uint8List _png(int width) => img.encodePng(img.Image(width: width, height: 1));

void main() {
  setUp(clearThumbnailCache);

  group('条目', () {
    testWidgets('完成信号画成完成说明，不画成工具卡', (tester) async {
      await tester.pumpWidget(
        _host(
          const CompletionMessage(
            summary: 'All done',
            status: ToolStatus.completed,
            isFinal: true,
          ),
        ),
      );
      expect(find.text('Completed'), findsOneWidget);
      expect(find.text('All done'), findsOneWidget);
      expect(find.text('Tool'), findsNothing);
      expect(find.byTooltip('Copy answer'), findsOneWidget);
    });

    testWidgets('空的完成总结也说清楚结束了', (tester) async {
      await tester.pumpWidget(
        _host(
          const CompletionMessage(summary: '  ', status: ToolStatus.failed),
        ),
      );
      expect(find.text('Completion failed'), findsOneWidget);
      expect(find.text('Task completed.'), findsOneWidget);
    });

    testWidgets('思考块收着，只露第一行，点开看全文', (tester) async {
      await tester.pumpWidget(
        _host(
          const ThoughtBlock(text: '\nFirst thought line\nHidden detail line'),
        ),
      );
      expect(find.text('Thought'), findsOneWidget);
      expect(find.text('First thought line'), findsOneWidget);
      expect(find.textContaining('Hidden detail line'), findsNothing);

      await tester.tap(find.text('Thought'));
      await tester.pumpAndSettle();
      expect(find.textContaining('Hidden detail line'), findsOneWidget);
    });

    testWidgets('最终回答下面写用时', (tester) async {
      await tester.pumpWidget(
        _host(
          const AssistantMessage(
            text: 'Answer',
            isFinal: true,
            durationMs: 102000,
          ),
        ),
      );
      expect(find.text('Took 1:42'), findsOneWidget);
    });

    testWidgets('后台任务便签：一行说明，点开看输出的尾巴', (tester) async {
      await tester.pumpWidget(
        _host(
          const TaskNoteRow(
            entry: ConversationEntry(
              index: 4,
              kind: EntryKind.taskNote,
              text: 'cargo build finished with exit code 0',
              thought: false,
              images: [],
              taskNote: TaskNoteView(
                kind: TaskNoteKind.backgroundTask,
                id: 'bg',
                title: 'cargo build',
                status: 'completed',
                exitCode: 0,
                outputTail: 'Finished release',
              ),
            ),
          ),
        ),
      );
      expect(find.text('Background task'), findsOneWidget);
      expect(
        find.text('cargo build finished with exit code 0'),
        findsOneWidget,
      );
      expect(find.text('Finished release'), findsNothing);
      await tester.tap(find.text('Background task'));
      await tester.pumpAndSettle();
      expect(find.text('Finished release'), findsOneWidget);
    });

    testWidgets('别的 Pi 对话发来的消息：标「消息」、摘要是正文，不画任务状态', (tester) async {
      await tester.pumpWidget(
        _host(
          const TaskNoteRow(
            entry: ConversationEntry(
              index: 5,
              kind: EntryKind.taskNote,
              text: '',
              thought: false,
              images: [],
              taskNote: TaskNoteView(
                kind: TaskNoteKind.peerMessage,
                id: 'msg-1',
                title: '',
                status: '',
                outputTail: '接口改好了，你那边可以接着写测试。',
              ),
            ),
          ),
        ),
      );
      expect(find.text('Message'), findsOneWidget);
      expect(find.text('Background task'), findsNothing);
      expect(find.text('接口改好了，你那边可以接着写测试。'), findsOneWidget);
      expect(find.byType(StatusMark), findsNothing);

      // 展开后全文在下面，摘要让出来，同一段话只出现一次。
      await tester.tap(find.text('Message'));
      await tester.pumpAndSettle();
      expect(find.text('接口改好了，你那边可以接着写测试。'), findsOneWidget);
    });

    testWidgets('计划是勾选清单，读屏读出每步的状态', (tester) async {
      final semantics = tester.ensureSemantics();
      await tester.pumpWidget(
        _host(
          const PlanChecklist(
            plan: [
              PlanStep(content: 'Read', status: PlanStepStatus.completed),
              PlanStep(content: 'Write', status: PlanStepStatus.inProgress),
              PlanStep(content: 'Test', status: PlanStepStatus.pending),
            ],
          ),
        ),
      );
      expect(find.text('Plan · 1/3'), findsOneWidget);
      expect(find.bySemanticsLabel('Done: Read'), findsOneWidget);
      expect(find.bySemanticsLabel('In progress: Write'), findsOneWidget);
      expect(find.bySemanticsLabel('To do: Test'), findsOneWidget);
      semantics.dispose();
    });
  });

  group('工具调用', () {
    testWidgets('diff 写出路径和增删行数', (tester) async {
      await tester.pumpWidget(
        _host(
          ToolCallRow(
            call: h.tool(
              'e1',
              'Update config',
              kind: ToolKind.edit,
              output: const [
                ToolOutput(
                  kind: ToolOutputKind.diff,
                  text: '',
                  diff: DiffView(
                    path: 'lib/config.dart',
                    oldText: 'keep\nbefore\n',
                    newText: 'keep\nafter\n',
                  ),
                  truncated: false,
                ),
              ],
            ),
          ),
        ),
      );
      expect(find.text('Edit'), findsOneWidget);
      expect(find.text('lib/config.dart'), findsNothing);

      await tester.tap(find.text('Update config'));
      await tester.pumpAndSettle();
      expect(find.text('lib/config.dart'), findsOneWidget);
      expect(find.text('+1'), findsOneWidget);
      expect(find.text('-1'), findsOneWidget);
    });

    testWidgets('终端输出写出退出码和截断', (tester) async {
      await tester.pumpWidget(
        _host(
          ToolCallRow(
            call: h.tool(
              'x1',
              'cargo test',
              kind: ToolKind.execute,
              status: ToolStatus.failed,
              output: const [
                ToolOutput(
                  kind: ToolOutputKind.terminal,
                  text: 'test failed',
                  exitCode: 101,
                  truncated: true,
                ),
              ],
            ),
          ),
        ),
      );
      await tester.tap(find.text('cargo test'));
      await tester.pumpAndSettle();
      expect(find.text('test failed'), findsOneWidget);
      expect(find.text('Exit code 101 · Output truncated'), findsOneWidget);
    });

    testWidgets('长输出先收起，点了显示全部', (tester) async {
      final text = List.generate(20, (i) => 'line $i').join('\n');
      await tester.pumpWidget(_host(FoldedText(text: '```\n$text\n```')));
      expect(find.textContaining('line 7'), findsOneWidget);
      expect(find.textContaining('line 12'), findsNothing);
      await tester.tap(find.text('Show all 20 lines'));
      await tester.pumpAndSettle();
      expect(find.textContaining('line 19'), findsOneWidget);
      expect(find.textContaining('```'), findsNothing, reason: '代码围栏去掉');
    });

    testWidgets('没有输出的工具调用不可展开', (tester) async {
      final semantics = tester.ensureSemantics();
      await tester.pumpWidget(_host(ToolCallRow(call: h.tool('r', 'a.txt'))));
      expect(find.byIcon(LucideIcons.chevronRight), findsNothing);
      semantics.dispose();
    });

    testWidgets('一组工具调用收着时写合起来的状态和最后一次', (tester) async {
      await tester.pumpWidget(
        _host(
          ToolGroup(
            calls: [
              h.tool('a', 'a.txt'),
              h.tool('b', 'b.txt', status: ToolStatus.failed),
              h.tool('c', 'c.txt', status: ToolStatus.inProgress),
            ],
          ),
        ),
      );
      expect(find.text('3 tool calls'), findsOneWidget);
      expect(find.text('1 failed'), findsOneWidget);
      expect(find.text('c.txt'), findsOneWidget);
      expect(find.text('a.txt'), findsNothing);
      await tester.tap(find.text('3 tool calls'));
      await tester.pumpAndSettle();
      expect(find.text('a.txt'), findsOneWidget);
    });
  });

  group('图片', () {
    testWidgets('快照换了新对象但内容没变，不换 provider（不然会闪）', (tester) async {
      final bytes = _png(1);
      await tester.pumpWidget(
        _host(
          ConversationThumbnail(
            image: ConversationImage(mime: 'image/png', bytes: bytes),
          ),
        ),
      );
      final first = tester.widget<Image>(find.byType(Image)).image;
      await tester.pumpWidget(
        _host(
          ConversationThumbnail(
            image: ConversationImage(
              mime: 'image/png',
              bytes: Uint8List.fromList(bytes),
            ),
          ),
        ),
      );
      final second = tester.widget<Image>(find.byType(Image)).image;
      expect(identical(first, second), isTrue);
    });

    testWidgets('只有引用的大图先取缩略图', (tester) async {
      final edges = <int?>[];
      await tester.pumpWidget(
        _host(
          ConversationThumbnail(
            image: const ConversationImage(
              mime: 'image/png',
              original: ImageRef(entry: 0, digest: 'thumb'),
            ),
            loadImage: (_, {maxEdge}) async {
              edges.add(maxEdge);
              return ImageData(mime: 'image/png', bytes: _png(3));
            },
          ),
        ),
      );
      await tester.pumpAndSettle();
      expect(edges, [thumbnailMaxEdge]);
      final shown = tester.widget<Image>(find.byType(Image)).image;
      expect((shown as MemoryImage).bytes, _png(3));
    });

    testWidgets('点开取原图，失败了可以重试', (tester) async {
      final requests = <int?>[];
      var fail = true;
      await tester.pumpWidget(
        _host(
          ConversationThumbnail(
            image: ConversationImage(
              mime: 'image/png',
              bytes: _png(1),
              original: const ImageRef(entry: 3, digest: 'retry'),
            ),
            loadImage: (_, {maxEdge}) async {
              requests.add(maxEdge);
              if (fail) {
                throw const SmeltError(
                  kind: SmeltErrorKind.offline,
                  message: 'offline',
                );
              }
              return ImageData(mime: 'image/png', bytes: _png(2));
            },
          ),
        ),
      );
      await tester.tap(find.byType(ConversationThumbnail));
      await tester.pumpAndSettle();
      expect(requests, [null], reason: '点开取的是原图');
      expect(find.textContaining('offline'), findsOneWidget);

      fail = false;
      await tester.tap(find.text('Retry'));
      await tester.pumpAndSettle();
      expect(requests, [null, null]);
      expect(find.textContaining('offline'), findsNothing);
      await tester.tap(find.byTooltip('Close image'));
      await tester.pumpAndSettle();
    });
  });

  group('纯计算', () {
    test('连续的工具调用收成一组，中间的空正文不拆组', () {
      final blocks = buildBlocks(
        h.view(
          entries: [
            h.entry(0, EntryKind.user, text: 'go'),
            h.entry(1, EntryKind.toolCall, tool: h.tool('a', 'a')),
            h.entry(2, EntryKind.assistant, text: '  '),
            h.entry(3, EntryKind.toolCall, tool: h.tool('b', 'b')),
            h.entry(4, EntryKind.assistant, text: 'done'),
            h.entry(5, EntryKind.toolCall, tool: h.tool('c', 'c')),
          ],
          finalAnswerIndex: 4,
        ),
      );
      expect(blocks.map((b) => b.key), ['e0', 't1', 'e4', 't5']);
      expect((blocks[1] as ToolGroupBlock).calls.map((c) => c.id), ['a', 'b']);
      expect((blocks[2] as EntryBlock).isFinal, isTrue);
    });

    test('diff 按行对齐，新文件全是新增', () {
      final lines = lineDiff('a\nb\nc\n', 'a\nB\nc\nd\n');
      expect(lines.map((l) => '${l.op.name}:${l.text}'), [
        'equal:a',
        'delete:b',
        'insert:B',
        'equal:c',
        'insert:d',
      ]);
      expect(diffTotals(lines), (2, 1));
      expect(
        lineDiff(null, 'x\ny').every((l) => l.op == DiffOp.insert),
        isTrue,
      );
    });

    test('长的未改动段收成一行', () {
      final before = List.generate(20, (i) => 'l$i').join('\n');
      final after = before.replaceFirst('l10', 'L10');
      final folded = foldUnchanged(lineDiff(before, after));
      expect(folded.first, isA<DiffSkipped>());
      expect((folded.first as DiffSkipped).count, 7);
      expect(folded.last, isA<DiffSkipped>());
      expect(folded.whereType<DiffShown>(), hasLength(8));
    });

    test('数字的说法', () {
      expect(compactCount(950), '950');
      expect(compactCount(84200), '84k');
      expect(compactCount(1240), '1.2k');
      expect(compactCount(1200000), '1.2M');
      expect(formatTurnDuration(8400), '8.4s');
      expect(formatTurnDuration(102000), '1:42');
      expect(formatTurnDuration(3800000), '1:03:20');
      expect(stripCodeFence('```dart\nx\n```'), 'x');
    });
  });
}
