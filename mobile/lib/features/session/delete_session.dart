import 'package:flutter/material.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../ui/buttons.dart';

/// 列表里显示和确认弹窗里引用的标题：没起名的按类型给个说法。
String sessionRowTitle(L10n l10n, SessionSummary session) {
  final title = session.title.trim();
  if (title.isNotEmpty) return title;
  return session.isTerminal
      ? l10n.sessionsUntitledTerminal
      : l10n.sessionsUntitledConversation;
}

/// 确认后删除一条会话。删掉后运行时推新列表，行自己消失；自动化运行的会话运行时会拒绝，
/// 原话转给用户。
Future<void> confirmDeleteSession(
  BuildContext context,
  FleetRuntime runtime,
  SessionSummary session,
) async {
  final l10n = L10n.of(context);
  final backend = AppScope.of(context).backend;
  final messenger = ScaffoldMessenger.of(context);
  final title = sessionRowTitle(l10n, session);
  final confirmed = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(
        session.isTerminal
            ? l10n.sessionsDeleteTerminalTitle
            : l10n.sessionsDeleteConversationTitle,
      ),
      content: Text(
        session.isTerminal
            ? l10n.sessionsDeleteTerminalBody(title, runtime.name)
            : l10n.sessionsDeleteConversationBody(title, runtime.name),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context, false),
          child: Text(l10n.commonCancel),
        ),
        GalleyButton.danger(
          label: l10n.commonDelete,
          onPressed: () => Navigator.pop(context, true),
        ),
      ],
    ),
  );
  if (confirmed != true) return;
  try {
    await backend.deleteSession(runtime.id, session.id);
  } catch (error) {
    messenger.showSnackBar(
      SnackBar(content: Text(l10n.sessionsDeleteFailed(describeError(error)))),
    );
  }
}
