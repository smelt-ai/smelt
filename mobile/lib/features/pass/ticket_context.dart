import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/status.dart';
import '../conversation/tool_views.dart';

/// 印在长单据上的上下文：会话最近几条（你说了什么、它回了什么、刚跑了哪些工具），
/// 让人批之前看得到它为什么要这么做。数据是内核随单据一起取回的快照头末尾，不另开订阅。
///
/// 想法（thought）、分隔线、回合结束这些不印：单据上只放判断要用的东西。
class TicketContext extends StatelessWidget {
  const TicketContext({super.key, required this.entries});

  final List<ConversationEntry> entries;

  @override
  Widget build(BuildContext context) {
    final lines = [
      for (final e in entries)
        if (_printable(e)) e,
    ];
    if (lines.isEmpty) return const SizedBox.shrink();
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        for (final (i, e) in lines.indexed) ...[
          if (i > 0) const SizedBox(height: Space.sm),
          _Line(entry: e),
        ],
      ],
    );
  }

  static bool _printable(ConversationEntry e) => switch (e.kind) {
    EntryKind.user => e.text.trim().isNotEmpty,
    EntryKind.assistant => !e.thought && e.text.trim().isNotEmpty,
    EntryKind.toolCall => e.toolCall != null,
    EntryKind.taskNote => e.taskNote != null,
    _ => false,
  };
}

class _Line extends StatelessWidget {
  const _Line({required this.entry});

  final ConversationEntry entry;

  /// 单据上只要读得懂：去掉 markdown 的强调与代码记号，压成一段。
  /// 下划线不能动：标识符里全是它（`nested_parens_in_url`）。
  static String _plain(String text) => text
      .replaceAll(RegExp(r'^\s*(#+|>)\s*', multiLine: true), '')
      .replaceAll(RegExp(r'`|\*\*'), '')
      .replaceAll(RegExp(r'\s+'), ' ')
      .trim();

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final e = entry;
    switch (e.kind) {
      case EntryKind.user:
        return Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              l10n.passContextYou,
              style: t.label.copyWith(color: c.onPaperMuted),
            ),
            const SizedBox(width: Space.sm),
            Expanded(
              child: Text(
                _plain(e.text),
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
                style: t.callout.copyWith(color: c.onPaperMuted),
              ),
            ),
          ],
        );
      case EntryKind.toolCall:
        final call = e.toolCall!;
        return Row(
          children: [
            StatusMark(
              mark: toolMark(call.status),
              label: toolStatusLabel(l10n, call.status),
            ),
            const SizedBox(width: Space.xs),
            Text(
              toolKindLabel(l10n, call.kind),
              style: t.label.copyWith(color: c.onPaper),
            ),
            const SizedBox(width: Space.sm),
            Expanded(
              child: Text(
                call.title,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: t.code.copyWith(color: c.onPaperMuted),
              ),
            ),
          ],
        );
      case EntryKind.taskNote:
        return Row(
          children: [
            Icon(
              LucideIcons.listTodo,
              size: Strokes.iconSmall,
              color: c.onPaperMuted,
            ),
            const SizedBox(width: Space.sm),
            Expanded(
              child: Text(
                // 消息便签没有标题，正文在 outputTail。
                e.taskNote!.kind == TaskNoteKind.peerMessage
                    ? _plain(e.taskNote!.outputTail)
                    : e.taskNote!.title,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: t.callout.copyWith(color: c.onPaperMuted),
              ),
            ),
          ],
        );
      default:
        return Text(
          _plain(e.text),
          maxLines: 4,
          overflow: TextOverflow.ellipsis,
          style: t.callout.copyWith(color: c.onPaper),
        );
    }
  }
}
