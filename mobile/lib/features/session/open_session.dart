import 'package:flutter/widgets.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../ui/adaptive.dart';
import '../agents/automation_run_page.dart';
import '../agents/automation_text.dart';
import '../conversation/conversation_page.dart';
import '../terminal/terminal_page.dart';

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
      final run? when !runIsActive(run.runStatus) => AutomationRunPage(
        runtimeId: runtimeId,
        runId: run.runId,
      ),
      _ when session.isTerminal => TerminalPage(
        runtimeId: runtimeId,
        sessionId: session.id,
      ),
      _ => ConversationPage(runtimeId: runtimeId, sessionId: session.id),
    },
  );
}
