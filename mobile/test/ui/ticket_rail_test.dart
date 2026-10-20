import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';
import 'package:smelt_mobile/theme/galley.dart';
import 'package:smelt_mobile/theme/theme.dart';
import 'package:smelt_mobile/ui/ticket.dart';
import 'package:smelt_mobile/ui/ticket_rail.dart';

Widget _host(Widget child, {bool reduceMotion = false}) => MaterialApp(
  theme: buildGalleyTheme(Brightness.light),
  localizationsDelegates: L10n.localizationsDelegates,
  supportedLocales: L10n.supportedLocales,
  home: MediaQuery(
    data: MediaQueryData(disableAnimations: reduceMotion),
    child: Scaffold(body: child),
  ),
);

RailEntry _entry(String id) => RailEntry(
  id: id,
  child: TicketPaper(child: Text('ticket $id')),
);

void main() {
  testWidgets('单据离轨时先盖章，动画结束后才从轨上消失', (tester) async {
    await tester.pumpWidget(
      _host(TicketRail(entries: [_entry('a'), _entry('b')])),
    );
    expect(find.text('ticket a'), findsOneWidget);
    expect(find.text('ticket b'), findsOneWidget);

    await tester.pumpWidget(
      _host(
        TicketRail(
          entries: [_entry('b')],
          stamps: const {'a': TicketStamp(text: '已允许 · 14:02')},
        ),
      ),
    );
    await tester.pump();
    // 章落下的这段时间里单据还在，章也在。
    expect(find.text('ticket a'), findsOneWidget);
    expect(find.text('已允许 · 14:02'), findsOneWidget);

    await tester.pump(
      Motion.stamp + Motion.ticket + const Duration(milliseconds: 50),
    );
    expect(find.text('ticket a'), findsNothing);
    expect(find.text('ticket b'), findsOneWidget);
  });

  testWidgets('新单据挂上来，不打乱已有单据', (tester) async {
    await tester.pumpWidget(_host(TicketRail(entries: [_entry('a')])));
    await tester.pumpWidget(
      _host(TicketRail(entries: [_entry('a'), _entry('c')])),
    );
    await tester.pump(Motion.ticket);
    expect(find.text('ticket a'), findsOneWidget);
    expect(find.text('ticket c'), findsOneWidget);
  });

  testWidgets('减少动态效果时离轨立即完成', (tester) async {
    await tester.pumpWidget(
      _host(TicketRail(entries: [_entry('a')]), reduceMotion: true),
    );
    await tester.pumpWidget(
      _host(
        const TicketRail(entries: [], emptyChild: Text('空')),
        reduceMotion: true,
      ),
    );
    await tester.pump();
    await tester.pump();
    expect(find.text('ticket a'), findsNothing);
    expect(find.text('空'), findsOneWidget);
  });

  testWidgets('没有单据时显示空提示', (tester) async {
    await tester.pumpWidget(
      _host(const TicketRail(entries: [], emptyChild: Text('没有等你处理的事'))),
    );
    expect(find.text('没有等你处理的事'), findsOneWidget);
  });
}
