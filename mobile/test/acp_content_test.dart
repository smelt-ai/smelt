import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:image/image.dart' as img;
import 'package:smelt_mobile/models/acp_snapshot.dart';
import 'package:smelt_mobile/widgets/acp_content.dart';

Widget _host(Widget child) => MaterialApp(
  theme: ThemeData.dark(useMaterial3: true),
  home: Scaffold(body: SingleChildScrollView(child: child)),
);

void main() {
  testWidgets('completion summaries are not rendered as tool cards', (
    tester,
  ) async {
    await tester.pumpWidget(
      _host(
        const AcpCompletionMessage(
          output: [ToolOutputText(text: 'All done')],
          status: ToolCallStatus.completed,
          isFinal: true,
        ),
      ),
    );

    expect(find.text('Completed'), findsOneWidget);
    expect(find.text('All done'), findsOneWidget);
    expect(find.text('Tool'), findsNothing);
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
          status: ToolCallStatus.completed,
          output: [
            ToolOutputDiff(
              path: 'lib/config.dart',
              oldText: 'before\n',
              newText: 'after\n',
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

  testWidgets('image provider is retained across parent rebuilds', (
    tester,
  ) async {
    final image = AcpImageData(
      mimeType: 'image/png',
      base64: base64Encode(img.encodePng(img.Image(width: 1, height: 1))),
    );

    await tester.pumpWidget(_host(AcpImageThumbnail(image: image)));
    final firstProvider = tester.widget<Image>(find.byType(Image)).image;

    await tester.pumpWidget(_host(AcpImageThumbnail(image: image)));
    final secondProvider = tester.widget<Image>(find.byType(Image)).image;

    expect(identical(firstProvider, secondProvider), isTrue);
  });

  testWidgets('opening a thumbnail loads the original and offers retry', (
    tester,
  ) async {
    String png(int width) =>
        base64Encode(img.encodePng(img.Image(width: width, height: 1)));
    final requests = <AcpImageOriginal>[];
    var fail = true;
    const original = AcpImageOriginal(entry: 3, digest: 'abc');

    await tester.pumpWidget(
      _host(
        AcpImageThumbnail(
          image: AcpImageData(
            mimeType: 'image/png',
            base64: png(1),
            original: original,
          ),
          loadOriginal: (ref) async {
            requests.add(ref);
            if (fail) throw StateError('offline');
            return AcpImageData(mimeType: 'image/png', base64: png(2));
          },
        ),
      ),
    );

    await tester.tap(find.byType(AcpImageThumbnail));
    await tester.pumpAndSettle();
    expect(requests, [original]);
    expect(find.textContaining('offline'), findsOneWidget);

    fail = false;
    await tester.tap(find.text('重试'));
    await tester.pumpAndSettle();
    expect(requests, [original, original]);
    expect(find.textContaining('offline'), findsNothing);
    final shown = tester
        .widgetList<Image>(find.byType(Image))
        .map((image) => image.image)
        .whereType<MemoryImage>()
        .map((image) => image.bytes)
        .toList();
    expect(shown, anyElement(equals(base64Decode(png(2)))));
  });

  testWidgets('a reference without a thumbnail still opens the original', (
    tester,
  ) async {
    var loads = 0;
    await tester.pumpWidget(
      _host(
        AcpImageThumbnail(
          image: const AcpImageData(
            mimeType: 'image/heic',
            base64: '',
            original: AcpImageOriginal(entry: 0, digest: 'x'),
          ),
          loadOriginal: (_) async {
            loads++;
            return AcpImageData(
              mimeType: 'image/png',
              base64: base64Encode(
                img.encodePng(img.Image(width: 1, height: 1)),
              ),
            );
          },
        ),
      ),
    );

    expect(find.byIcon(Icons.image_outlined), findsOneWidget);
    await tester.tap(find.byType(AcpImageThumbnail));
    await tester.pumpAndSettle();
    expect(loads, 1);
  });
}
