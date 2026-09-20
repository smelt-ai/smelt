import 'package:flutter/material.dart';
import 'package:flutter/semantics.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:flutter_slidable/flutter_slidable.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/theme/smelt_theme.dart';
import 'package:smelt_mobile/widgets/agent_icon.dart';
import 'package:smelt_mobile/widgets/session_row.dart';

// 指挥台和 Projects 原来各写了一套会话行，字号、行高、缩进都对不上。这组测试钉住
// 抽出 SessionRow 之后的共用行为，以及两屏之间**有意**保留的那点差异。
void main() {
  // 图标名单来自资产清单（见 agent_icon.dart）。不加载的话所有 agent 都会掉回
  // 兜底图标，跟 agent 身份相关的断言就会「因为图标没加载」而通过。
  setUpAll(() async {
    TestWidgetsFlutterBinding.ensureInitialized();
    resetAgentIconsForTest();
    await loadAgentIcons();
  });

  SessionSummary session({
    String title = 'demo',
    String? project = 'smelt',
    String? detail,
    String phase = 'idle',
    String status = 'idle',
    SessionKind kind = SessionKind.acp,
  }) => SessionSummary(
    id: 'x',
    kind: kind,
    title: title,
    phase: phase,
    status: status,
    agent: 'codex',
    projectTitle: project,
    detail: detail,
    updatedAt: 1,
  );

  Widget host(Widget child) => MaterialApp(
    theme: smeltTheme(Brightness.dark),
    home: Scaffold(body: child),
  );

  group('SessionRow', () {
    // 状态色已经画在 agent 图标上了，再写一遍 Idle 是同一条信息画两遍。Projects
    // 原来的副标题是「Idle · terminal · detail」，前两段都是图标已经说过的话。
    testWidgets('状态不写进文案', (tester) async {
      await tester.pumpWidget(
        host(SessionRow(session: session(), onTap: () {})),
      );
      expect(find.textContaining('Idle'), findsNothing);
      expect(find.textContaining('idle'), findsNothing);
    });

    testWidgets('终端会话也不写 terminal 字样', (tester) async {
      await tester.pumpWidget(
        host(
          SessionRow(
            session: session(kind: SessionKind.terminal),
            onTap: () {},
          ),
        ),
      );
      expect(find.textContaining('terminal'), findsNothing);
      // 这条终端会话里跑的是 codex，图标就该是 codex——它的身份不因为「开在
      // 终端里」而消失。
      expect(find.byType(SvgPicture), findsOneWidget);
    });

    // 视觉上不写，不等于信息可以丢——读屏软件读不到颜色。
    testWidgets('状态交给 Semantics，读屏用户不丢信息', (tester) async {
      final handle = tester.ensureSemantics();
      await tester.pumpWidget(
        host(
          SessionRow(
            session: session(phase: 'thinking', status: 'running'),
            onTap: () {},
          ),
        ),
      );
      expect(
        tester.getSemantics(find.byType(AgentIcon)).label,
        contains('Running'),
      );
      handle.dispose();
    });

    // Projects 的行已经在项目分组下面了，再画一遍项目名是纯重复。
    testWidgets('showProject 为假时不画项目名', (tester) async {
      await tester.pumpWidget(
        host(
          SessionRow(
            session: session(title: 'fix login', project: 'smelt'),
            onTap: () {},
          ),
        ),
      );
      expect(find.text('fix login'), findsOneWidget);
      expect(find.textContaining('smelt'), findsNothing);
    });

    // 指挥台是跨项目的分诊列表，光看标题认不出是哪个项目的事。
    testWidgets('showProject 为真时画项目名', (tester) async {
      await tester.pumpWidget(
        host(
          SessionRow(
            session: session(title: 'fix login', project: 'smelt'),
            showProject: true,
            onTap: () {},
          ),
        ),
      );
      expect(find.text('smelt'), findsOneWidget);
    });

    testWidgets('项目名和 detail 之间用 · 连接', (tester) async {
      await tester.pumpWidget(
        host(
          SessionRow(
            session: session(title: 'fix login', detail: 'reading files'),
            showProject: true,
            onTap: () {},
          ),
        ),
      );
      expect(find.text('smelt · reading files'), findsOneWidget);
    });

    // 只有 detail 时不能留下孤零零的分隔点。
    testWidgets('缺项目名时不留孤立的分隔点', (tester) async {
      await tester.pumpWidget(
        host(
          SessionRow(
            session: session(project: null, detail: 'reading files'),
            showProject: true,
            onTap: () {},
          ),
        ),
      );
      expect(find.text('reading files'), findsOneWidget);
    });

    testWidgets('空标题给出可读的兜底', (tester) async {
      await tester.pumpWidget(
        host(
          SessionRow(
            session: session(title: '  '),
            onTap: () {},
          ),
        ),
      );
      expect(find.text('ACP conversation'), findsOneWidget);
    });

    testWidgets('空标题的终端兜底成 Terminal', (tester) async {
      await tester.pumpWidget(
        host(
          SessionRow(
            session: session(title: '', kind: SessionKind.terminal),
            onTap: () {},
          ),
        ),
      );
      expect(find.text('Terminal'), findsOneWidget);
    });

    // 两屏共用同一个槽宽，标题左边缘才对得齐；图标 18pt 必须塞得进去。
    testWidgets('leading 槽装得下 agent 图标', (tester) async {
      await tester.pumpWidget(
        host(SessionRow(session: session(), onTap: () {})),
      );
      expect(tester.takeException(), isNull);
      final icon = tester.getSize(find.byType(AgentIcon));
      expect(icon.width, lessThanOrEqualTo(SessionRow.leadingSlot));
    });

    testWidgets('trailing 缺省时不占位', (tester) async {
      await tester.pumpWidget(
        host(SessionRow(session: session(), onTap: () {})),
      );
      final withoutTrailing = tester.getSize(find.byType(SessionRow)).height;

      await tester.pumpWidget(
        host(
          SessionRow(
            session: session(),
            onTap: () {},
            trailing: const Text('2h ago'),
          ),
        ),
      );
      expect(find.text('2h ago'), findsOneWidget);
      expect(
        tester.getSize(find.byType(SessionRow)).height,
        withoutTrailing,
        reason: 'trailing 不应该把行撑高，否则两屏行高又会分叉',
      );
    });

    // 删除入口原来是行尾一个只有单项的「⋯」菜单。左滑是移动端列表删除的通用手势，
    // 也把那 32pt 还给了标题。
    testWidgets('左滑露出删除按钮', (tester) async {
      var deleted = 0;
      await tester.pumpWidget(
        host(
          SessionRow(
            session: session(),
            onTap: () {},
            onDelete: () => deleted++,
          ),
        ),
      );
      expect(find.byIcon(Icons.delete_outline), findsNothing);

      await tester.drag(find.byType(SessionRow), const Offset(-200, 0));
      await tester.pumpAndSettle();
      expect(find.byIcon(Icons.delete_outline), findsOneWidget);

      await tester.tap(find.byIcon(Icons.delete_outline));
      await tester.pumpAndSettle();
      expect(deleted, 1);
    });

    // 行高只有 40pt 上下（没有副标题时更矮），图标叠文字要 48pt 以上，
    // SlidableAction 会把文字**裁掉**而不是报 overflow——单测抓不到，只有真机
    // 看得见。所以这里直接钉住「按钮里不许有文字」。
    testWidgets('删除按钮不带文字标签', (tester) async {
      await tester.pumpWidget(
        host(SessionRow(session: session(), onTap: () {}, onDelete: () {})),
      );
      await tester.drag(find.byType(SessionRow), const Offset(-200, 0));
      await tester.pumpAndSettle();

      expect(
        find.descendant(
          of: find.byType(SlidableAction),
          matching: find.byType(Text),
        ),
        findsNothing,
        reason: '会话行放不下图标+文字两行，加回 label 会被裁掉',
      );
    });

    // 删除会结束终端进程。不可逆的动作不该被一次划到底的手势直接完成，
    // 中间那一下点击是刻意留的。
    testWidgets('划到底也不会直接删掉', (tester) async {
      var deleted = 0;
      await tester.pumpWidget(
        host(
          SessionRow(
            session: session(),
            onTap: () {},
            onDelete: () => deleted++,
          ),
        ),
      );
      await tester.drag(find.byType(SessionRow), const Offset(-2000, 0));
      await tester.pumpAndSettle();
      expect(deleted, 0);
    });

    // 只读配对下连手势都不该挂：滑出一个点不动的按钮比滑不动更让人困惑。
    testWidgets('不给 onDelete 就滑不出任何东西', (tester) async {
      await tester.pumpWidget(
        host(SessionRow(session: session(), onTap: () {})),
      );
      await tester.drag(find.byType(SessionRow), const Offset(-200, 0));
      await tester.pumpAndSettle();
      expect(find.byIcon(Icons.delete_outline), findsNothing);
    });

    // 左滑是隐藏手势，读屏软件既滑不动也看不见按钮。删除必须还有第二条路。
    testWidgets('删除动作挂在语义树上，读屏用户够得着', (tester) async {
      final handle = tester.ensureSemantics();
      await tester.pumpWidget(
        host(SessionRow(session: session(), onTap: () {}, onDelete: () {})),
      );

      final data = tester.getSemantics(find.byType(InkWell)).getSemanticsData();
      final labels = (data.customSemanticsActionIds ?? const <int>[])
          .map((id) => CustomSemanticsAction.getAction(id)?.label)
          .toList();
      expect(labels, contains('Delete'));
      handle.dispose();
    });

    testWidgets('不给 onDelete 时不凭空造语义动作', (tester) async {
      final handle = tester.ensureSemantics();
      await tester.pumpWidget(
        host(SessionRow(session: session(), onTap: () {})),
      );
      final data = tester.getSemantics(find.byType(InkWell)).getSemanticsData();
      expect(data.customSemanticsActionIds ?? const <int>[], isEmpty);
      handle.dispose();
    });

    testWidgets('整行可点', (tester) async {
      var taps = 0;
      await tester.pumpWidget(
        host(SessionRow(session: session(), onTap: () => taps++)),
      );
      await tester.tap(find.byType(SessionRow));
      expect(taps, 1);
    });
  });
}
