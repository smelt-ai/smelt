import 'package:flutter/widgets.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../ui/adaptive.dart';
import '../agents/automation_run_page.dart';
import '../agents/automation_text.dart';
import '../conversation/conversation_page.dart';
import '../terminal/terminal_page.dart';

/// 自动化跑出来的这条会话已经结束：运行记录说跑完了，或者会话自己已经到了终态。
/// 两份数据推来的先后不定，会话先报完成、运行记录还没更新的那一下也要算结束。
bool automationRunOver(AutomationSource run, SessionPhase phase) =>
    !runIsActive(run.runStatus) ||
    switch (phase) {
      SessionPhase.succeeded ||
      SessionPhase.failed ||
      SessionPhase.dead => true,
      _ => false,
    };

/// 打开一条会话：对话进对话页，终端进终端页。手机压栈，平板放进右侧详情栏。
///
/// 自动化跑出来的会话跑完以后就没了（运行时只留运行记录），这时打开的是那次运行的
/// 记录页；还在跑的照常进对话，看执行现场。
void openSession(
  BuildContext context,
  String runtimeId,
  SessionSummary session,
) => openSessionWith(DetailHost.of(context), runtimeId, session);

void openSessionWith(
  DetailHost host,
  String runtimeId,
  SessionSummary session,
) {
  final run = session.automation;
  host.open(
    sessionKey(runtimeId, session.id),
    (_) => switch (run) {
      final run? when automationRunOver(run, session.phase) =>
        AutomationRunPage(runtimeId: runtimeId, runId: run.runId),
      _ when session.isTerminal => TerminalPage(
        runtimeId: runtimeId,
        sessionId: session.id,
      ),
      _ => ConversationPage(runtimeId: runtimeId, sessionId: session.id),
    },
  );
}
