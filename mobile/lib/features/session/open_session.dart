import 'package:flutter/widgets.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../ui/adaptive.dart';
import '../conversation/conversation_page.dart';
import '../terminal/terminal_page.dart';

/// 打开一条会话：对话进对话页，终端进终端页。手机压栈，平板放进右侧详情栏。
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
  host.open(
    sessionKey(runtimeId, session.id),
    (_) => session.isTerminal
        ? TerminalPage(runtimeId: runtimeId, sessionId: session.id)
        : ConversationPage(runtimeId: runtimeId, sessionId: session.id),
  );
}
