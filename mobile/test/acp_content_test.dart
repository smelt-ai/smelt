import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:image/image.dart' as img;
import 'package:smelt_mobile/models/runtime_models.dart';
import 'package:smelt_mobile/widgets/acp_content.dart';

Widget _host(Widget child) => MaterialApp(
  theme: ThemeData.dark(useMaterial3: true),
  home: Scaffold(body: SingleChildScrollView(child: child)),
);

Uint8List _png(int width) => img.encodePng(img.Image(width: width, height: 1));

void main() {
  testWidgets('completion summaries are not rendered as tool cards', (
    tester,
  ) async {
    await tester.pumpWidget(
      _host(
        const AcpCompletionMessage(
          summary: 'All done',
          status: ToolStatus.completed,
          isFinal: true,
        ),
      ),
    );

    expect(find.text('Completed'), findsOneWidget);
    expect(find.text('All done'), findsOneWidget);
    expect(find.text('Tool'), findsNothing);
  });

  testWidgets('an empty completion summary still says it finished', (
    tester,
  ) async {
    await tester.pumpWidget(
      _host(
        const AcpCompletionMessage(summary: '  ', status: ToolStatus.failed),
      ),
    );

    expect(find.text('Completion failed'), findsOneWidget);
    expect(find.text('Task completed.'), findsOneWidget);
  });

  testWidgets('thought blocks are collapsed until opened', (tester) async {
    await tester.pumpWidget(
      _host(
        const AcpAssistantMessage(
          text: 'First thought line\nHidden detail line',
          thought: true,
        ),
      ),
    );

    expect(find.text('Thought'), findsOneWidget);
    expect(find.text('First thought line'), findsOneWidget);
    expect(find.textContaining('Hidden detail line'), findsNothing);

    await tester.tap(find.byIcon(Icons.chevron_right));
    await tester.pumpAndSettle();

    expect(find.textContaining('Hidden detail line'), findsOneWidget);
  });

  testWidgets('tool diff renders path and change totals', (tester) async {
    await tester.pumpWidget(
      _host(
        const AcpToolCallCard(
          title: 'Update config',
          kind: ToolKind.edit,
          status: ToolStatus.completed,
          output: [
            ToolOutput(
              kind: ToolOutputKind.diff,
              text: '',
              diff: DiffView(
                path: 'lib/config.dart',
                oldText: 'before\n',
                newText: 'after\n',
              ),
              truncated: false,
            ),
          ],
        ),
      ),
    );

    expect(find.text('Edit'), findsOneWidget);
    await tester.tap(find.text('Update config'));
    await tester.pumpAndSettle();

    expect(find.text('lib/config.dart'), findsOneWidget);
    expect(find.text('+1'), findsOneWidget);
    expect(find.text('-1'), findsOneWidget);
  });

  testWidgets('embedded terminal output notes the exit code and truncation', (
    tester,
  ) async {
    await tester.pumpWidget(
      _host(
        const AcpToolCallCard(
          title: 'cargo test',
          kind: ToolKind.execute,
          status: ToolStatus.failed,
          output: [
            ToolOutput(
              kind: ToolOutputKind.terminal,
              text: 'test failed',
              exitCode: 101,
              truncated: true,
            ),
          ],
        ),
      ),
    );

    await tester.tap(find.text('cargo test'));
    await tester.pumpAndSettle();

    expect(find.text('test failed'), findsOneWidget);
    expect(find.text('Exit code 101 · Output truncated'), findsOneWidget);
  });

  testWidgets('image provider is retained across parent rebuilds', (
    tester,
  ) async {
    final bytes = _png(1);

    await tester.pumpWidget(
      _host(
        AcpImageThumbnail(
          image: ConversationImage(mime: 'image/png', bytes: bytes),
        ),
      ),
    );
    final firstProvider = tester.widget<Image>(find.byType(Image)).image;

    // 每次快照都是新对象，但内容没变就不该换 provider（否则图片会闪）。
    await tester.pumpWidget(
      _host(
        AcpImageThumbnail(
          image: ConversationImage(
            mime: 'image/png',
            bytes: Uint8List.fromList(bytes),
          ),
        ),
      ),
    );
    final secondProvider = tester.widget<Image>(find.byType(Image)).image;

    expect(identical(firstProvider, secondProvider), isTrue);
  });

  testWidgets('opening a thumbnail loads the original and offers retry', (
    tester,
  ) async {
    final requests = <(ImageRef, int?)>[];
    var fail = true;
    const original = ImageRef(entry: 3, digest: 'retry');

    await tester.pumpWidget(
      _host(
        AcpImageThumbnail(
          image: ConversationImage(
            mime: 'image/png',
            bytes: _png(1),
            original: original,
          ),
          loadImage: (ref, {maxEdge}) async {
            requests.add((ref, maxEdge));
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

    await tester.tap(find.byType(AcpImageThumbnail));
    await tester.pumpAndSettle();
    expect(requests, [(original, null)], reason: '点开取的是原图');
    expect(find.textContaining('offline'), findsOneWidget);

    fail = false;
    await tester.tap(find.text('重试'));
    await tester.pumpAndSettle();
    expect(requests, [(original, null), (original, null)]);
    expect(find.textContaining('offline'), findsNothing);
    final shown = tester
        .widgetList<Image>(find.byType(Image))
        .map((image) => image.image)
        .whereType<MemoryImage>()
        .map((image) => image.bytes)
        .toList();
    expect(shown, anyElement(equals(_png(2))));
  });

  testWidgets('a reference-only image fetches a thumbnail for the list', (
    tester,
  ) async {
    final edges = <int?>[];
    await tester.pumpWidget(
      _host(
        AcpImageThumbnail(
          image: const ConversationImage(
            mime: 'image/png',
            original: ImageRef(entry: 0, digest: 'thumbnail'),
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

  testWidgets('a reference without a thumbnail still opens the original', (
    tester,
  ) async {
    final edges = <int?>[];
    await tester.pumpWidget(
      _host(
        AcpImageThumbnail(
          image: const ConversationImage(
            mime: 'image/heic',
            original: ImageRef(entry: 0, digest: 'no-thumbnail'),
          ),
          loadImage: (_, {maxEdge}) async {
            edges.add(maxEdge);
            if (maxEdge != null) {
              throw const SmeltError(
                kind: SmeltErrorKind.other,
                message: 'cannot decode',
              );
            }
            return ImageData(mime: 'image/png', bytes: _png(1));
          },
        ),
      ),
    );
    await tester.pumpAndSettle();

    expect(find.byIcon(Icons.image_outlined), findsOneWidget);
    await tester.tap(find.byType(AcpImageThumbnail));
    await tester.pumpAndSettle();
    expect(edges, [thumbnailMaxEdge, null]);
  });
}
