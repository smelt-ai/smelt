import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/preferences.dart';
import 'package:smelt_mobile/data/stores.dart';
import 'package:smelt_mobile/features/agents/automation_run_page.dart';
import 'package:smelt_mobile/features/conversation/conversation_page.dart';
import 'package:smelt_mobile/features/session/open_session.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';
import 'package:smelt_mobile/theme/theme.dart';
import 'package:smelt_mobile/ui/adaptive.dart';

SessionSummary _automationSession(
  AutomationRunStatus status, {
  SessionPhase phase = SessionPhase.thinking,
}) => SessionSummary(
  id: 's-auto',
  title: '汇总昨日 PR',
  kind: SessionKind.acp,
  phase: phase,
  status: SessionStatus.idle,
  agent: 'claude',
  projectOrder: 0,
  updatedAt: 0,
  turnStartedAt: 0,
  automation: AutomationSource(
    automationId: 'auto-standup',
    runId: 'run-auto-standup',
    automationName: '每日站会摘要',
    runStatus: status,
    runSource: AutomationRunSource.manual,
  ),
);

/// 打开会话时 DetailHost 拿到的页面。
Future<Widget> _opened(WidgetTester tester, SessionSummary session) async {
  Widget? page;
  final model = AppModel(
    DemoBackend(now: DateTime(2025, 6, 1, 14)),
    PreferencesStore.memory(),
  );
  addTearDown(model.dispose);
  await tester.pumpWidget(
    AppScope(
      model: model,
      child: MaterialApp(
        locale: const Locale('zh'),
        theme: buildGalleyTheme(Brightness.light),
        localizationsDelegates: L10n.localizationsDelegates,
        supportedLocales: L10n.supportedLocales,
        home: DetailHost(
          open: (_, builder) => page = Builder(builder: builder),
          selectedKey: null,
          child: Builder(
            builder: (context) => TextButton(
              onPressed: () => openSession(context, 'rt-mbp', session),
              child: const Text('open'),
            ),
          ),
        ),
      ),
    ),
  );
  await tester.tap(find.text('open'));
  await tester.pumpWidget(
    AppScope(
      model: model,
      child: MaterialApp(
        locale: const Locale('zh'),
        theme: buildGalleyTheme(Brightness.light),
        localizationsDelegates: L10n.localizationsDelegates,
        supportedLocales: L10n.supportedLocales,
        home: page!,
      ),
    ),
  );
  await tester.pumpAndSettle();
  return page!;
}

void main() {
  testWidgets('跑完的自动化会话打开的是运行记录：会话已经没了，不去开它', (tester) async {
    await _opened(tester, _automationSession(AutomationRunStatus.completed));
    expect(find.byType(AutomationRunPage), findsOneWidget);
    expect(find.byType(ConversationPage), findsNothing);
    expect(find.text('对话'), findsOneWidget, reason: '运行记录里有那次的对话');
  });

  testWidgets('会话先报完成、运行记录还没更新时，也去看运行记录', (tester) async {
    await _opened(
      tester,
      _automationSession(
        AutomationRunStatus.running,
        phase: SessionPhase.succeeded,
      ),
    );
    expect(find.byType(AutomationRunPage), findsOneWidget);
  });

  testWidgets('还在跑的自动化会话照常进对话看现场', (tester) async {
    await _opened(tester, _automationSession(AutomationRunStatus.running));
    expect(find.byType(ConversationPage), findsOneWidget);
  });
}
