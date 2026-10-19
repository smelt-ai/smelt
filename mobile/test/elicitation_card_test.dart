import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/runtime_models.dart';
import 'package:smelt_mobile/widgets/elicitation_card.dart';

ElicitationField _field(
  String key, {
  ElicitationFieldKind kind = ElicitationFieldKind.text,
  bool required = false,
  List<String> options = const [],
  List<int> chosen = const [],
  String? textValue,
}) => ElicitationField(
  key: key,
  title: key,
  required_: required,
  allowCustomInput: false,
  kind: kind,
  options: options,
  secret: false,
  chosen: Uint32List.fromList(chosen),
  textValue: textValue,
);

Elicitation _elicitationWith(int fieldCount) {
  return Elicitation(
    message: 'Fill these in before I continue.',
    fields: [for (var i = 0; i < fieldCount; i++) _field('Field $i')],
    isReady: true,
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
          elicitation: Elicitation(
            message: 'Pick one',
            fields: [
              _field(
                'Channel',
                kind: ElicitationFieldKind.select,
                required: true,
                options: const ['Email', 'SMS'],
              ),
            ],
            isReady: false,
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

  group('作答是否就绪', () {
    test('可选的文字框不挡住已选好的必选项', () {
      final elicitation = Elicitation(
        message: '',
        fields: [
          _field(
            'channel',
            kind: ElicitationFieldKind.select,
            required: true,
            options: const ['Email'],
            chosen: const [0],
          ),
          _field('note'),
        ],
        isReady: true,
      );
      expect(elicitation.isReadyWith(const {}), isTrue);
    });

    test('必填文字框要么本地填了，要么运行时记着非空值', () {
      Elicitation withText(String? value) => Elicitation(
        message: '',
        fields: [_field('name', required: true, textValue: value)],
        isReady: value?.trim().isNotEmpty == true,
      );
      expect(withText(null).isReadyWith(const {}), isFalse);
      expect(withText(null).isReadyWith(const {0: '  '}), isFalse);
      expect(withText(null).isReadyWith(const {0: 'Ada'}), isTrue);
      expect(withText('Ada').isReadyWith(const {}), isTrue);
      // 本地把运行时记着的值清空了，就是没填。
      expect(withText('Ada').isReadyWith(const {0: ''}), isFalse);
    });

    testWidgets('选中的选项来自运行时记下的下标', (tester) async {
      await tester.pumpWidget(
        _host(
          ElicitationCard(
            elicitation: Elicitation(
              message: '',
              fields: [
                _field(
                  'Channel',
                  kind: ElicitationFieldKind.multiSelect,
                  options: const ['Email', 'SMS'],
                  chosen: const [1],
                ),
              ],
              isReady: true,
            ),
            textValues: const {},
            onTextChanged: (_, _) {},
            onChoose: (_, _) {},
            onSubmit: () {},
            onDismiss: () {},
          ),
        ),
      );
      final chips = tester.widgetList<ChoiceChip>(find.byType(ChoiceChip));
      expect(chips.map((chip) => chip.selected), [false, true]);
    });
  });
}
