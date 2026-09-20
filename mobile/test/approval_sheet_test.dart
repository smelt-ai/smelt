import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/acp_snapshot.dart';
import 'package:smelt_mobile/widgets/approval_card.dart';

PendingPermission _permission({
  ApprovalDetails details = const ApprovalDetailsCommand(
    command: 'cargo test -p smelt-ui --no-fail-fast',
    cwd: '/Users/me/code/smelt',
    reason: '改完 ui_theme 后要跑一遍单测',
  ),
  List<PermissionOption> options = const [
    PermissionOption(optionId: 'once', name: 'Allow once', kind: 'allow_once'),
    PermissionOption(
      optionId: 'always',
      name: 'Always allow',
      kind: 'allow_always',
    ),
    PermissionOption(optionId: 'no', name: 'Reject', kind: 'reject_once'),
  ],
}) => PendingPermission(
  toolCallId: 'tc-1',
  question: 'Run a command?',
  options: options,
  details: details,
);

Future<void> _pumpCard(
  WidgetTester tester, {
  required PendingPermission permission,
  required void Function(String) onRespond,
}) async {
  await tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: ApprovalCard(
          permission: permission,
          onRespond: onRespond,
          detailSubtitle: 'smelt · claude · 2m ago',
        ),
      ),
    ),
  );
}

/// 卡片本身还留在 sheet 底下，两边都会渲染同名文字和按钮。所有断言都必须钉在
/// sheet 内部，否则测试会因为「找到 2 个」而失败——那是测试写法问题，不是 bug。
Finder _inSheet(Finder matching) =>
    find.descendant(of: find.byType(BottomSheet), matching: matching);

void main() {
  testWidgets('点卡片正文打开详情 sheet，命令全文和判断依据都在', (tester) async {
    await _pumpCard(tester, permission: _permission(), onRespond: (_) {});

    await tester.tap(find.text('Run a command?'));
    await tester.pumpAndSettle();

    expect(_inSheet(find.text('smelt · claude · 2m ago')), findsOneWidget);
    expect(_inSheet(find.text('Working directory')), findsOneWidget);
    expect(_inSheet(find.text('/Users/me/code/smelt')), findsOneWidget);
    expect(_inSheet(find.text('Why')), findsOneWidget);

    // 同一句理由在 sheet 里只能出现一次：ApprovalDetailsView 和事实表都画就是
    // 重复。这条断言就是为了钉住那次重复。
    expect(_inSheet(find.text('改完 ui_theme 后要跑一遍单测')), findsOneWidget);
  });

  testWidgets('sheet 里的选项一个都不能少，并且是纵向排列', (tester) async {
    await _pumpCard(tester, permission: _permission(), onRespond: (_) {});
    await tester.tap(find.text('Run a command?'));
    await tester.pumpAndSettle();

    // 三个动作各出现一次：漏掉任何一个都意味着用户在 sheet 里做不了在卡片上
    // 能做的决策。
    for (final label in ['Allow once', 'Always allow', 'Reject']) {
      expect(
        _inSheet(find.widgetWithText(OutlinedButton, label)),
        findsOneWidget,
        reason: '$label 在详情 sheet 里丢了',
      );
    }

    // 纵向：三个按钮的横向位置相同、纵向依次向下。
    final once = tester.getTopLeft(
      _inSheet(find.widgetWithText(OutlinedButton, 'Allow once')),
    );
    final always = tester.getTopLeft(
      _inSheet(find.widgetWithText(OutlinedButton, 'Always allow')),
    );
    final reject = tester.getTopLeft(
      _inSheet(find.widgetWithText(OutlinedButton, 'Reject')),
    );
    expect(once.dx, always.dx);
    expect(always.dx, reject.dx);
    expect(once.dy, lessThan(always.dy));
    expect(always.dy, lessThan(reject.dy));
  });

  testWidgets('在 sheet 里做决策会回传 optionId 并关掉 sheet', (tester) async {
    final responses = <String>[];
    await _pumpCard(
      tester,
      permission: _permission(),
      onRespond: responses.add,
    );

    await tester.tap(find.text('Run a command?'));
    await tester.pumpAndSettle();
    await tester.tap(
      _inSheet(find.widgetWithText(OutlinedButton, 'Allow once')),
    );
    await tester.pumpAndSettle();

    expect(responses, ['once']);
    expect(find.text('Working directory'), findsNothing);
  });

  testWidgets('协议没给的字段不硬造：没有 cwd 就不画那一行', (tester) async {
    await _pumpCard(
      tester,
      permission: _permission(
        details: const ApprovalDetailsCommand(command: 'ls', cwd: null),
      ),
      onRespond: (_) {},
    );

    await tester.tap(find.text('Run a command?'));
    await tester.pumpAndSettle();

    expect(_inSheet(find.text('Working directory')), findsNothing);
    expect(_inSheet(find.text('Why')), findsNothing);
  });
}
