import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/session_filters.dart';
import 'package:smelt_mobile/pages/console_page.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/services/pending_actions_controller.dart';
import 'package:smelt_mobile/theme/smelt_theme.dart';
import 'package:smelt_mobile/widgets/agent_icon.dart';
import 'package:smelt_mobile/widgets/project_avatar.dart';

SessionSummary _session(
  String id, {
  String phase = 'succeeded',
  String status = 'idle',
  int updatedAt = 1000,
  String? title,
  bool unread = false,
}) => SessionSummary(
  id: id,
  title: title ?? id,
  phase: phase,
  status: status,
  agent: 'codex',
  updatedAt: updatedAt,
  unread: unread,
);

void main() {
  // 图标名单来自资产清单（见 agent_icon.dart）。不加载的话所有 agent 都会掉回
  // 兜底图标，跟 agent 身份相关的断言就会「因为图标没加载」而通过。
  setUpAll(() async {
    TestWidgetsFlutterBinding.ensureInitialized();
    resetAgentIconsForTest();
    await loadAgentIcons();
  });

  group('sessionRecentlyRan', () {
    // phase 和 status 是两个独立字段：读过之后只有 status 退回 idle，
    // phase 仍然是 succeeded。这就是「跑完且已读」的现成信号。
    test('跑完并且已经看过', () {
      expect(sessionRecentlyRan(_session('a')), isTrue);
    });

    test('还没看过的归「刚完成」，不在这一段重复出现', () {
      final unread = _session('a', unread: true);
      expect(sessionRecentlyDone(unread), isTrue);
      expect(sessionRecentlyRan(unread), isFalse);
    });

    test('在等我或者在跑的都不算', () {
      expect(
        sessionRecentlyRan(_session('a', status: 'waiting_approval')),
        isFalse,
      );
      expect(sessionRecentlyRan(_session('a', status: 'running')), isFalse);
      expect(sessionRecentlyRan(_session('a', phase: 'thinking')), isFalse);
    });

    // 运行时没了的话 phase 只是历史残留。不排除的话桌面一重启，
    // 这一段会被一批陈年会话灌满。
    test('运行时已断开的不算', () {
      expect(
        sessionRecentlyRan(_session('a', status: 'disconnected')),
        isFalse,
      );
    });

    test('从没跑过的空闲会话不算', () {
      expect(sessionRecentlyRan(_session('a', phase: 'idle')), isFalse);
    });
  });

  group('PendingActionsController.recentRuns', () {
    late StreamController<List<SessionSummary>> sessions;
    late StreamController<String> snapshots;

    PendingActionsController build(List<SessionSummary> initial) =>
        PendingActionsController(
          sessions: sessions.stream,
          initialSessions: initial,
          snapshotUpdates: snapshots.stream,
          lookupSnapshot: (_) => null,
          requestDetails: (_) => true,
        );

    setUp(() {
      sessions = StreamController<List<SessionSummary>>.broadcast();
      snapshots = StreamController<String>.broadcast();
    });

    test('按最后活动时间倒序——最近跑的排最前', () {
      final controller = build([
        _session('old', updatedAt: 100),
        _session('newest', updatedAt: 900),
        _session('mid', updatedAt: 500),
      ]);
      addTearDown(controller.dispose);
      expect(controller.recentRuns.map((s) => s.id), ['newest', 'mid', 'old']);
    });

    // 曾经截断到 6 条并写「更多在 Projects」。但智能体对话不在项目树里，被截掉
    // 的那些在 Projects 也找不到——指挥台既然要展示全部对话，就不能再截。
    test('不再截断：全部已跑过的会话都列出来', () {
      final controller = build([
        for (var i = 0; i < 10; i++) _session('s$i', updatedAt: i),
      ]);
      addTearDown(controller.dispose);
      expect(controller.recentRuns.length, 10);
      expect(controller.recentRuns.first.id, 's9');
    });

    // 三段里只有 items 是算出来的，其余直接读 _sessions。只比 items 会漏掉
    // 「跑完了但没有任何待办变化」这类更新。
    test('会话跑完但待办没变化时也会通知', () async {
      final controller = build([_session('a', phase: 'thinking')]);
      addTearDown(controller.dispose);
      expect(controller.recentRuns, isEmpty);

      var notified = 0;
      controller.addListener(() => notified++);
      sessions.add([_session('a')]);
      await Future<void>.delayed(Duration.zero);

      expect(notified, greaterThan(0));
      expect(controller.recentRuns.map((s) => s.id), ['a']);
    });

    test('同一份会话再推一次不会重复通知', () async {
      final controller = build([_session('a')]);
      addTearDown(controller.dispose);
      var notified = 0;
      controller.addListener(() => notified++);
      sessions.add([_session('a')]);
      await Future<void>.delayed(Duration.zero);
      expect(notified, 0);
    });
  });

  _projectLineTests();
  _agentIconTests();

  group('指挥台第四段', () {
    Widget host(PendingActionsController controller) => MaterialApp(
      theme: smeltTheme(Brightness.dark),
      home: Scaffold(
        body: ConsolePage(onOpenSession: (_) {}, controller: controller),
      ),
    );

    testWidgets('画出「Recently ran」并列出已看过的完成会话', (tester) async {
      final controller = PendingActionsController(
        sessions: const Stream.empty(),
        initialSessions: [_session('a', title: 'Ran earlier')],
        snapshotUpdates: const Stream.empty(),
        lookupSnapshot: (_) => null,
        requestDetails: (_) => true,
      );
      addTearDown(controller.dispose);
      await tester.pumpWidget(host(controller));
      expect(find.text('RECENTLY RAN'), findsOneWidget);
      expect(find.text('Ran earlier'), findsOneWidget);
    });

    testWidgets('全部列出，不再有「更多在 Projects」的截断提示', (tester) async {
      final controller = PendingActionsController(
        sessions: const Stream.empty(),
        initialSessions: [
          for (var i = 0; i < 5; i++) _session('s$i', updatedAt: i),
        ],
        snapshotUpdates: const Stream.empty(),
        lookupSnapshot: (_) => null,
        requestDetails: (_) => true,
      );
      addTearDown(controller.dispose);
      await tester.pumpWidget(host(controller));
      expect(find.text('5'), findsOneWidget);
      expect(find.textContaining('more in Projects'), findsNothing);
    });

    testWidgets('四段都空时仍然是空状态', (tester) async {
      final controller = PendingActionsController(
        sessions: const Stream.empty(),
        initialSessions: [_session('a', phase: 'idle')],
        snapshotUpdates: const Stream.empty(),
        lookupSnapshot: (_) => null,
        requestDetails: (_) => true,
      );
      addTearDown(controller.dispose);
      await tester.pumpWidget(host(controller));
      expect(find.text('RECENTLY RAN'), findsNothing);
    });
  });
}

// 这几条钉住的是「项目名什么时候画」。原来的规则是「标题里包含项目名就不画」，
// 建立在一个错误前提上：服务端从不给标题追加项目名（title 依次取 custom_title →
// launch_label → 路径名）。结果是用户碰巧把项目名写进自定义标题时，那一行就
// 莫名其妙少一段，行结构随机。
void _projectLineTests() {
  SessionSummary s(String title, {String? project}) => SessionSummary(
    id: 'x',
    title: title,
    phase: 'succeeded',
    status: 'idle',
    agent: 'codex',
    projectTitle: project,
    updatedAt: 1,
  );

  Widget host(SessionSummary session) => MaterialApp(
    theme: smeltTheme(Brightness.dark),
    home: Scaffold(
      body: ConsolePage(
        onOpenSession: (_) {},
        controller: PendingActionsController(
          sessions: const Stream.empty(),
          initialSessions: [session],
          snapshotUpdates: const Stream.empty(),
          lookupSnapshot: (_) => null,
          requestDetails: (_) => true,
        ),
      ),
    ),
  );

  group('会话行上的项目名', () {
    testWidgets('自定义标题里已经含项目名，项目行仍然要画', (tester) async {
      await tester.pumpWidget(host(s('Copilot 对话 · smelt', project: 'smelt')));
      expect(find.text('Copilot 对话 · smelt'), findsOneWidget);
      expect(find.text('smelt'), findsOneWidget);
    });

    // 项目名叫 api 之类的短词时，旧的包含匹配几乎会吞掉每一行。
    testWidgets('项目名恰好是标题的子串也照画', (tester) async {
      await tester.pumpWidget(host(s('rapid prototype', project: 'api')));
      expect(find.text('api'), findsOneWidget);
    });

    // 终端会话标题默认取目录名，常常就等于项目名，这时重复一遍纯属噪音。
    testWidgets('标题跟项目名完全一样时才省掉', (tester) async {
      await tester.pumpWidget(host(s('smelt', project: 'smelt')));
      expect(find.text('smelt'), findsOneWidget);
    });
  });
}

// 指挥台原来三段都用圆点，而 Projects 页早已改用 agent 图标——同一个会话在两屏
// 给出不同的视觉身份。这几条钉住统一后的行为。
void _agentIconTests() {
  SessionSummary s({
    String agent = 'codex',
    String phase = 'succeeded',
    String status = 'idle',
    SessionKind kind = SessionKind.acp,
  }) => SessionSummary(
    id: 'x',
    kind: kind,
    title: 'demo',
    phase: phase,
    status: status,
    agent: agent,
    updatedAt: 1,
  );

  Widget host(SessionSummary session) => MaterialApp(
    theme: smeltTheme(Brightness.dark),
    home: Scaffold(
      body: ConsolePage(
        onOpenSession: (_) {},
        controller: PendingActionsController(
          sessions: const Stream.empty(),
          initialSessions: [session],
          snapshotUpdates: const Stream.empty(),
          lookupSnapshot: (_) => null,
          requestDetails: (_) => true,
        ),
      ),
    ),
  );

  group('指挥台的会话图标', () {
    testWidgets('「最近跑过」的行用 agent 图标而不是圆点', (tester) async {
      await tester.pumpWidget(host(s()));
      expect(find.byType(AgentIcon), findsOneWidget);
      expect(find.byType(SessionStatusDot), findsNothing);
    });

    testWidgets('「正在跑」的行也用 agent 图标', (tester) async {
      await tester.pumpWidget(host(s(phase: 'thinking', status: 'running')));
      await tester.pump();
      expect(find.byType(AgentIcon), findsOneWidget);
    });

    // 「还在动」是 Running 段的全部意义，静止图标区分不出「跑完了」。
    // 按 key 找而不是按 FadeTransition 类型找：满树都是 FadeTransition，
    // MaterialPageRoute 的转场就是一个。两个方向各起一个 tester：同一棵树上
    // 二次 pumpWidget 会复用 ConsolePage 的 element，换不掉 controller。
    testWidgets('「正在跑」的图标会呼吸', (tester) async {
      await tester.pumpWidget(host(s(phase: 'thinking', status: 'running')));
      await tester.pump();
      expect(find.byKey(const Key('console-pulse')), findsOneWidget);
    });

    testWidgets('跑完的图标不呼吸', (tester) async {
      await tester.pumpWidget(host(s()));
      await tester.pump();
      expect(find.byKey(const Key('console-pulse')), findsNothing);
    });

    // 跑着 CLI 的终端会话画那家的图标；只有认不出命令的裸终端才是终端图标。
    testWidgets('跑着 codex 的终端会话画 codex 图标', (tester) async {
      await tester.pumpWidget(host(s(kind: SessionKind.terminal)));
      expect(find.byIcon(Icons.terminal), findsNothing);
    });

    testWidgets('裸终端会话仍然是终端图标', (tester) async {
      await tester.pumpWidget(
        host(s(kind: SessionKind.terminal, agent: 'zsh')),
      );
      expect(find.byIcon(Icons.terminal), findsOneWidget);
    });

    // 图标从 9pt 圆点换成 18pt，leading 槽跟着放宽了——这正是上一轮
    // 「OVERFLOWED BY 6.0 PIXELS」出现的位置。
    testWidgets('换成图标后行内不溢出', (tester) async {
      await tester.pumpWidget(host(s()));
      expect(tester.takeException(), isNull);
    });
  });
}
