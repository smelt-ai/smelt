import 'package:flutter/material.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/theme/smelt_theme.dart';
import 'package:smelt_mobile/widgets/agent_icon.dart';

SessionSummary _session({
  String agent = 'claude',
  SessionKind kind = SessionKind.acp,
  String status = 'idle',
}) => SessionSummary(
  id: 's',
  kind: kind,
  title: 't',
  phase: 'idle',
  status: status,
  agent: agent,
);

Future<void> _pump(WidgetTester tester, SessionSummary session) {
  return tester.pumpWidget(
    MaterialApp(
      theme: smeltTheme(Brightness.dark),
      home: Scaffold(body: AgentIcon(session: session)),
    ),
  );
}

void main() {
  // 名单来自资产清单，不是代码里的常量，所以测试也得先加载。
  setUpAll(() async {
    TestWidgetsFlutterBinding.ensureInitialized();
    resetAgentIconsForTest();
    await loadAgentIcons();
  });

  group('agentIconAsset', () {
    // 这份名单跟 `agent_kind.rs` 的 AcpAgentKind::ALL ∪ TerminalAgentKind::ALL
    // 一一对应。资产是否齐全由 smelt-core 的
    // `every_kind_has_an_icon_in_the_desktop_and_mobile_bundles` 钉住；这里钉的
    // 是「发现出来的名单能被查到」。
    test('桌面支持的每一家 agent 都有图标', () {
      for (final id in [
        'claude',
        'codex',
        'copilot',
        'grok',
        'cursor',
        'antigravity',
        'opencode',
        'kiro',
        'crush',
        'dsh',
        'pi',
      ]) {
        expect(agentIconAsset(id), isNotNull, reason: '$id 没有图标');
      }
    });

    test('图标名单是从资产清单发现的，不是代码里写死的', () {
      expect(agentIconIds, contains('claude'));
      expect(agentIconIds, contains('dsh'));
      // 目录里的说明文件不该被当成一家 agent。
      expect(agentIconIds, isNot(contains('THIRD_PARTY_NOTICES')));
    });

    test('大小写和空白不影响匹配', () {
      expect(agentIconAsset('  Claude '), agentIconAsset('claude'));
      expect(agentIconAsset('CODEX'), agentIconAsset('codex'));
    });

    // 服务端是 `c.contains(k.id())` 的宽松匹配，自定义 agent 的 id 常是
    // `claude-quant` 这种带后缀的形态。精确匹配会让它们全掉进兜底图标。
    test('带后缀的自定义 agent 仍然认得出是哪家', () {
      expect(agentIconAsset('claude-quant'), agentIconAsset('claude'));
      expect(agentIconAsset('my-codex-acp'), agentIconAsset('codex'));
      expect(agentIconAsset('pi-acp'), agentIconAsset('pi'));
    });

    test('短 id 不会从其它 agent 名称中误匹配', () {
      expect(agentIconAsset('copilot'), isNot(agentIconAsset('pi')));
      expect(agentIconAsset('api-key-helper'), isNull);
      expect(agentIconAsset('mycodexadapter'), agentIconAsset('codex'));
    });

    test('认不出的返回 null', () {
      expect(agentIconAsset('other'), isNull);
      expect(agentIconAsset(''), isNull);
      expect(agentIconAsset('   '), isNull);
    });

    test('解析出的资源路径都落在已声明的 assets 目录下', () {
      for (final id in agentIconIds) {
        final path = agentIconAsset(id);
        expect(path, startsWith('assets/agent-icons/'));
        expect(path, endsWith('.svg'));
      }
    });
  });

  group('AgentIcon', () {
    testWidgets('认得出的 agent 用 SVG', (tester) async {
      await _pump(tester, _session(agent: 'claude'));
      expect(find.byType(SvgPicture), findsOneWidget);
    });

    testWidgets('裸终端会话用终端图标', (tester) async {
      await _pump(tester, _session(kind: SessionKind.terminal, agent: ''));
      expect(find.byIcon(Icons.terminal), findsOneWidget);
      expect(find.byType(SvgPicture), findsNothing);
    });

    // 在内嵌终端里跑的 CLI 也是有身份的，桌面就画那家的图标。
    testWidgets('跑着 CLI 的终端会话画那家 agent 的图标', (tester) async {
      await _pump(tester, _session(kind: SessionKind.terminal, agent: 'codex'));
      expect(find.byType(SvgPicture), findsOneWidget);
      expect(find.byIcon(Icons.terminal), findsNothing);
    });

    // 终端里认不出的命令退回终端图标，而不是机器人——它确实是个终端。
    testWidgets('认不出的终端会话退回终端图标而不是机器人', (tester) async {
      await _pump(tester, _session(kind: SessionKind.terminal, agent: 'zsh'));
      expect(find.byIcon(Icons.terminal), findsOneWidget);
      expect(find.byIcon(Icons.smart_toy_outlined), findsNothing);
    });

    // 退回终端图标会把 ACP 会话画成终端——那不是「信息少了」，是「信息错了」。
    testWidgets('认不出的 ACP agent 退到机器人图标而不是终端图标', (tester) async {
      await _pump(tester, _session(agent: 'other'));
      expect(find.byIcon(Icons.smart_toy_outlined), findsOneWidget);
      expect(find.byIcon(Icons.terminal), findsNothing);
    });

    testWidgets('图标颜色由会话状态驱动，跟桌面一致', (tester) async {
      await _pump(
        tester,
        _session(agent: 'claude', status: 'waiting_approval'),
      );
      final svg = tester.widget<SvgPicture>(find.byType(SvgPicture));
      expect(
        svg.colorFilter,
        ColorFilter.mode(SmeltColors.dark.waitingApproval, BlendMode.srcIn),
      );
    });

    testWidgets('每个图标都带读屏标签', (tester) async {
      await _pump(tester, _session(agent: 'codex'));
      expect(
        find.bySemanticsLabel(RegExp('codex conversation')),
        findsOneWidget,
      );
    });
  });

  group('AgentGlyph', () {
    testWidgets('按 agent 标识取图标，不需要会话', (tester) async {
      await tester.pumpWidget(
        MaterialApp(
          theme: smeltTheme(Brightness.dark),
          home: const Scaffold(body: AgentGlyph(agent: 'codex')),
        ),
      );
      expect(find.byType(SvgPicture), findsOneWidget);
    });

    // 自定义 agent 的 id 是 claude-quant，kind 才是 claude；选择器传的是 kind。
    testWidgets('认不出的 agent 退到机器人图标', (tester) async {
      await tester.pumpWidget(
        MaterialApp(
          theme: smeltTheme(Brightness.dark),
          home: const Scaffold(body: AgentGlyph(agent: 'totally-unknown')),
        ),
      );
      expect(find.byIcon(Icons.smart_toy_outlined), findsOneWidget);
      expect(find.byIcon(Icons.terminal), findsNothing);
    });
  });
}
