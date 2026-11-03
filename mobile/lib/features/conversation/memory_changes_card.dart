import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/buttons.dart';

/// 卡片里的行列表最多这么高，再多就在卡片里滚，不把消息流挤没。
const double _rowsMaxHeight = 200;

/// 输入框上方的记忆卡片：这一轮智能体写下或改过的记忆。
///
/// 待确认的行上直接能采纳；点一行从底部弹出详情，看正文、采纳、删除。卡片内容来自对话快照，
/// 采纳、删除后由 smeltd 改快照推回来，这里不自己改行。
class MemoryChangesCard extends StatefulWidget {
  const MemoryChangesCard({
    super.key,
    required this.runtimeId,
    required this.changes,
    required this.onDismiss,
    required this.onOpenMemoryPage,
  });

  final String runtimeId;
  final ConversationMemoryChanges changes;
  final VoidCallback onDismiss;
  final VoidCallback onOpenMemoryPage;

  @override
  State<MemoryChangesCard> createState() => _MemoryChangesCardState();
}

class _MemoryChangesCardState extends State<MemoryChangesCard> {
  /// 正在采纳或删除的那一行。
  (MemoryLayer, String)? _busy;

  Future<bool> _act(
    ConversationMemoryChange entry,
    Future<AgentMemories> Function() action,
  ) async {
    final messenger = ScaffoldMessenger.of(context);
    final l10n = L10n.of(context);
    setState(() => _busy = (entry.layer, entry.fileName));
    try {
      await action();
      return true;
    } catch (error) {
      messenger.showSnackBar(
        SnackBar(content: Text(l10n.memoryActionFailed(describeError(error)))),
      );
      return false;
    } finally {
      if (mounted) setState(() => _busy = null);
    }
  }

  Future<bool> _approve(ConversationMemoryChange entry) {
    final backend = AppScope.of(context).backend;
    return _act(
      entry,
      () => backend.approveMemory(
        widget.runtimeId,
        widget.changes.agentId,
        entry.layer,
        entry.fileName,
      ),
    );
  }

  Future<bool> _delete(ConversationMemoryChange entry) async {
    final l10n = L10n.of(context);
    final backend = AppScope.of(context).backend;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(l10n.memoryDeleteTitle(entry.name)),
        content: Text(
          entry.layer == MemoryLayer.user
              ? l10n.memoryDeleteUserBody
              : l10n.memoryDeleteAgentBody,
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
    if (confirmed != true || !mounted) return false;
    return _act(
      entry,
      () => backend.deleteMemory(
        widget.runtimeId,
        widget.changes.agentId,
        entry.layer,
        entry.fileName,
      ),
    );
  }

  Future<void> _openDetail(ConversationMemoryChange entry) async {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    await showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      constraints: BoxConstraints(
        maxHeight: MediaQuery.sizeOf(context).height * 0.85,
        maxWidth: Space.readingMax,
      ),
      builder: (sheetContext) {
        final pending = entry.pendingReason;
        final description = entry.description.trim();
        return SafeArea(
          top: false,
          child: Padding(
            padding: const EdgeInsets.fromLTRB(
              Space.lg,
              Space.lg,
              Space.lg,
              Space.md,
            ),
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text(entry.name, style: t.headline.copyWith(color: c.ink)),
                const SizedBox(height: Space.sm),
                Wrap(
                  spacing: Space.xs,
                  runSpacing: Space.xs,
                  children: _badges(context, entry),
                ),
                if (pending != null) ...[
                  const SizedBox(height: Space.sm),
                  Text(
                    l10n.conversationMemoryPendingReason(pending),
                    style: t.note.copyWith(color: c.heatInk),
                  ),
                ],
                if (description.isNotEmpty) ...[
                  const SizedBox(height: Space.sm),
                  Text(description, style: t.note.copyWith(color: c.inkMuted)),
                ],
                const SizedBox(height: Space.md),
                Flexible(
                  child: SingleChildScrollView(
                    child: SelectableText(
                      entry.body.isEmpty
                          ? l10n.conversationMemoryEmptyBody
                          : entry.body,
                      style: t.body.copyWith(color: c.ink),
                    ),
                  ),
                ),
                const SizedBox(height: Space.md),
                Wrap(
                  spacing: Space.sm,
                  runSpacing: Space.sm,
                  children: [
                    if (pending != null)
                      GalleyButton.solid(
                        icon: LucideIcons.check,
                        label: l10n.memoryApprove,
                        onPressed: () async {
                          final navigator = Navigator.of(sheetContext);
                          if (await _approve(entry)) navigator.pop();
                        },
                      ),
                    GalleyButton.danger(
                      icon: LucideIcons.trash2,
                      label: l10n.commonDelete,
                      onPressed: () async {
                        final navigator = Navigator.of(sheetContext);
                        if (await _delete(entry)) navigator.pop();
                      },
                    ),
                    GalleyButton.quiet(
                      label: l10n.conversationMemoryOpenPage,
                      onPressed: () {
                        Navigator.of(sheetContext).pop();
                        widget.onOpenMemoryPage();
                      },
                    ),
                  ],
                ),
              ],
            ),
          ),
        );
      },
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final entries = widget.changes.entries;
    return Padding(
      padding: const EdgeInsets.fromLTRB(Space.sm, 0, Space.sm, Space.sm),
      child: DecoratedBox(
        decoration: BoxDecoration(
          color: c.shelf,
          border: Border.all(color: c.hairline),
          borderRadius: const BorderRadius.all(Radii.shelf),
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Padding(
              padding: const EdgeInsets.only(left: Space.md),
              child: Row(
                children: [
                  Icon(
                    LucideIcons.bookOpen,
                    size: Strokes.iconSmall,
                    color: c.inkMuted,
                  ),
                  const SizedBox(width: Space.sm),
                  Expanded(
                    child: Semantics(
                      liveRegion: true,
                      child: Text(
                        memoryChangesSummary(l10n, entries),
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: t.meta.copyWith(color: c.inkMuted),
                      ),
                    ),
                  ),
                  TextButton(
                    onPressed: widget.onOpenMemoryPage,
                    child: Text(l10n.conversationMemoryOpenPage),
                  ),
                  IconButton(
                    tooltip: l10n.conversationMemoryDismiss,
                    onPressed: widget.onDismiss,
                    icon: Icon(
                      LucideIcons.x,
                      size: Strokes.iconSmall,
                      color: c.inkMuted,
                    ),
                  ),
                ],
              ),
            ),
            ConstrainedBox(
              constraints: const BoxConstraints(maxHeight: _rowsMaxHeight),
              child: ListView(
                shrinkWrap: true,
                padding: const EdgeInsets.only(bottom: Space.xs),
                children: [for (final entry in entries) _row(context, entry)],
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _row(BuildContext context, ConversationMemoryChange entry) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final busy = _busy == (entry.layer, entry.fileName);
    final pending = entry.pendingReason != null;
    final subtitle = entry.description.trim();
    return InkWell(
      onTap: () => unawaited(_openDetail(entry)),
      child: ConstrainedBox(
        constraints: const BoxConstraints(minHeight: Space.touch),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: Space.md),
          child: Row(
            children: [
              Expanded(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      children: [
                        Flexible(
                          child: Text(
                            entry.name,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: t.body.copyWith(color: c.ink),
                          ),
                        ),
                        const SizedBox(width: Space.xs),
                        ..._badges(context, entry),
                      ],
                    ),
                    if (subtitle.isNotEmpty)
                      Text(
                        subtitle,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: t.meta.copyWith(color: c.inkFaint),
                      ),
                  ],
                ),
              ),
              if (busy)
                const Padding(
                  padding: EdgeInsets.all(Space.sm),
                  child: SizedBox.square(
                    dimension: Strokes.icon,
                    child: CircularProgressIndicator.adaptive(strokeWidth: 2),
                  ),
                )
              else if (pending)
                TextButton(
                  onPressed: _busy == null
                      ? () => unawaited(_approve(entry))
                      : null,
                  child: Text(l10n.memoryApprove),
                )
              else
                Icon(
                  LucideIcons.chevronRight,
                  size: Strokes.iconSmall,
                  color: c.inkFaint,
                ),
            ],
          ),
        ),
      ),
    );
  }

  List<Widget> _badges(BuildContext context, ConversationMemoryChange entry) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    return [
        _Badge(
          label: entry.created
              ? l10n.conversationMemoryNew
              : l10n.conversationMemoryChanged,
          color: c.inkMuted,
        ),
        if (entry.layer == MemoryLayer.user)
          _Badge(label: l10n.conversationMemoryShared, color: c.link),
        if (entry.pendingReason != null)
          _Badge(label: l10n.conversationMemoryPending, color: c.heatInk)
        else
          _Badge(label: l10n.conversationMemoryActive, color: c.ok),
      ].expand((badge) => [badge, const SizedBox(width: Space.xs)]).toList()
      ..removeLast();
  }
}

/// 卡片标题：新记住几条、更新几条、几条待确认。
String memoryChangesSummary(L10n l10n, List<ConversationMemoryChange> entries) {
  final created = entries.where((entry) => entry.created).length;
  final updated = entries.length - created;
  final pending = entries.where((entry) => entry.pendingReason != null).length;
  return [
    l10n.conversationMemoryTitle,
    if (created > 0) l10n.conversationMemoryCreated(created),
    if (updated > 0) l10n.conversationMemoryUpdated(updated),
    if (pending > 0) l10n.conversationMemoryPendingCount(pending),
  ].join(' · ');
}

class _Badge extends StatelessWidget {
  const _Badge({required this.label, required this.color});

  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) {
    final t = GalleyText.of(context);
    return DecoratedBox(
      decoration: BoxDecoration(
        color: color.withValues(alpha: 0.14),
        borderRadius: const BorderRadius.all(Radii.tag),
      ),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: Space.xs),
        child: Text(label, style: t.caption.copyWith(color: color)),
      ),
    );
  }
}
