import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/acp_snapshot.dart';
import 'package:smelt_mobile/widgets/elicitation_card.dart';

PendingElicitation _elicitationWith(int fieldCount) {
  return PendingElicitation(
    message: 'Fill these in before I continue.',
    fields: [
      for (var i = 0; i < fieldCount; i++)
        ElicitationField(
          key: 'f$i',
          title: 'Field $i',
          required: false,
          kind: const ElicitationText(secret: false),
        ),
    ],
  );
}

Widget _host(Widget child, {Size size = const Size(390, 844)}) {
  return MaterialApp(
    home: MediaQuery(
      data: MediaQueryData(size: size),
      child: Scaffold(
        body: Column(
          children: [
            child,
            const Expanded(child: SizedBox()),
          ],
        ),
      ),
    ),
  );
}

void main() {
  testWidgets('the submit row stays reachable no matter how many fields', (
    tester,
  ) async {
    // 这条是回归：原来整张卡（含按钮）都在 SingleChildScrollView 里，字段一多
    // Submit 就被滚出 360px 的窗口，用户还看不出下面有个必须点的东西。
    await tester.pumpWidget(
      _host(
        ElicitationCard(
          elicitation: _elicitationWith(12),
          textValues: const {},
          onTextChanged: (_, _) {},
          onChoose: (_, _) {},
          onSubmit: () {},
          onDismiss: () {},
        ),
      ),
    );

    final submit = find.widgetWithText(FilledButton, 'Submit');
    expect(submit, findsOneWidget);

    // 不做任何滚动，按钮就必须已经在卡片可视范围内。
    final card = tester.getRect(find.byType(ElicitationCard));
    final button = tester.getRect(submit);
    expect(button.bottom, lessThanOrEqualTo(card.bottom + 0.5));
    expect(button.top, greaterThanOrEqualTo(card.top));
    expect(button.height, greaterThan(0));
  });

  testWidgets('the field area still scrolls when it overflows', (tester) async {
    await tester.pumpWidget(
      _host(
        ElicitationCard(
          elicitation: _elicitationWith(12),
          textValues: const {},
          onTextChanged: (_, _) {},
          onChoose: (_, _) {},
          onSubmit: () {},
          onDismiss: () {},
        ),
      ),
    );
    expect(find.byType(SingleChildScrollView), findsOneWidget);
    expect(tester.takeException(), isNull);
  });

  testWidgets('the card never eats more than its share of a short screen', (
    tester,
  ) async {
    // 横屏 / 小屏：写死的 360px 会吃掉整个可用高度。
    const shortScreen = Size(740, 360);
    await tester.pumpWidget(
      _host(
        ElicitationCard(
          elicitation: _elicitationWith(12),
          textValues: const {},
          onTextChanged: (_, _) {},
          onChoose: (_, _) {},
          onSubmit: () {},
          onDismiss: () {},
        ),
        size: shortScreen,
      ),
    );

    final card = tester.getRect(find.byType(ElicitationCard));
    expect(card.height, lessThan(shortScreen.height));
    expect(
      tester.getRect(find.widgetWithText(FilledButton, 'Submit')).bottom,
      lessThanOrEqualTo(card.bottom + 0.5),
    );
  });

  testWidgets('a lone select field submits by tapping the option', (
    tester,
  ) async {
    // 单个单选字段不显示按钮行——点选项本身就是作答。
    var chosen = <int>[];
    await tester.pumpWidget(
      _host(
        ElicitationCard(
          elicitation: const PendingElicitation(
            message: 'Pick one',
            fields: [
              ElicitationField(
                key: 'channel',
                title: 'Channel',
                required: true,
                kind: ElicitationSelect([
                  ElicitationOption('Email'),
                  ElicitationOption('SMS'),
                ]),
              ),
            ],
          ),
          textValues: const {},
          onTextChanged: (_, _) {},
          onChoose: (field, option) => chosen = [field, option],
          onSubmit: () {},
          onDismiss: () {},
        ),
      ),
    );

    expect(find.widgetWithText(FilledButton, 'Submit'), findsNothing);
    await tester.tap(find.text('SMS'));
    expect(chosen, [0, 1]);
  });
}
