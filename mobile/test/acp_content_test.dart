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
}
