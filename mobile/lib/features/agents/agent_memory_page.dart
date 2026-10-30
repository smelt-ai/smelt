import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/buttons.dart';
import '../../ui/page.dart';
import '../../ui/shelf.dart';
import '../../ui/time.dart';
import 'agents_widgets.dart';

/// 一个智能体的记忆：它自己的一层，和所有智能体共用的一层。
///
/// 智能体在读过外部内容的那一轮、或无人值守的运行里写下的记忆是「待确认」：不注入、不召回，
/// 要人采纳才生效。这正是手机该处理的事，所以待确认的排在最前、操作就在行里。
/// 记忆没有推送：进页面取一次，每次操作回带刷新后的两层。
class AgentMemoryPage extends StatefulWidget {
  const AgentMemoryPage({
    super.key,
    required this.runtimeId,
    required this.agentId,
    required this.agentName,
    this.initial,
  });

  final String runtimeId;
  final String agentId;
  final String agentName;

  /// 详情页已经取到的那份，先显示它，同时重取。
  final AgentMemories? initial;

  @override
  State<AgentMemoryPage> createState() => _AgentMemoryPageState();
}

class _AgentMemoryPageState extends State<AgentMemoryPage> {
  AgentMemories? _memories;
  String? _error;

  /// 展开看正文的那条：（层，文件名）。
  (MemoryLayer, String)? _expanded;

  /// 正在采纳或删除的那条。
  (MemoryLayer, String)? _busy;

  @override
  void initState() {
    super.initState();
    _memories = widget.initial;
    unawaited(_load());
  }

  Future<void> _load() async {
    try {
      final memories = await AppScope.of(
        context,
      ).backend.agentMemories(widget.runtimeId, widget.agentId);
      if (mounted) {
        setState(() {
          _memories = memories;
          _error = null;
        });
      }
    } catch (error) {
      if (mounted) setState(() => _error = describeError(error));
    }
  }

  Future<void> _act(
    MemoryLayer layer,
    MemoryItem item,
    Future<AgentMemories> Function() action,
  ) async {
    final messenger = ScaffoldMessenger.of(context);
    final l10n = L10n.of(context);
    setState(() => _busy = (layer, item.fileName));
    try {
      final memories = await action();
      if (mounted) setState(() => _memories = memories);
    } catch (error) {
      messenger.showSnackBar(
        SnackBar(content: Text(l10n.memoryActionFailed(describeError(error)))),
      );
    } finally {
      if (mounted) setState(() => _busy = null);
    }
  }

  void _approve(MemoryLayer layer, MemoryItem item) {
    final backend = AppScope.of(context).backend;
    unawaited(
      _act(
        layer,
        item,
        () => backend.approveMemory(
          widget.runtimeId,
          widget.agentId,
          layer,
          item.fileName,
        ),
      ),
    );
  }

  Future<void> _delete(MemoryLayer layer, MemoryItem item) async {
    final l10n = L10n.of(context);
    final backend = AppScope.of(context).backend;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(l10n.memoryDeleteTitle(item.name)),
        content: Text(
          layer == MemoryLayer.user
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
    if (confirmed != true || !mounted) return;
    await _act(
      layer,
      item,
      () => backend.deleteMemory(
        widget.runtimeId,
        widget.agentId,
        layer,
        item.fileName,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final model = AppScope.of(context);
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    return Scaffold(
      backgroundColor: c.ground,
      appBar: AppBar(
        backgroundColor: c.ground,
        title: DetailTitle(
          title: l10n.memoryTitle(widget.agentName),
          runtime: model.fleet.runtime(widget.runtimeId),
        ),
      ),
      body: _body(context),
    );
  }

  Widget _body(BuildContext context) {
    final l10n = L10n.of(context);
    final memories = _memories;
    final error = _error;
    if (memories == null) {
      if (error == null) {
        return const Center(child: CircularProgressIndicator.adaptive());
      }
      return Center(
        child: EmptyNote(
          icon: LucideIcons.cloudOff,
          title: l10n.memoryLoadFailed,
          body: error,
          action: GalleyButton.solid(label: l10n.commonRetry, onPressed: _load),
        ),
      );
    }
    return RefreshIndicator.adaptive(
      onRefresh: _load,
      child: ListView(
        padding: EdgeInsets.only(
          bottom: MediaQuery.paddingOf(context).bottom + Space.xxl,
        ),
        children: [
          if (error != null)
            NoteRow(
              icon: LucideIcons.circleAlert,
              danger: true,
              text: l10n.memoryLoadFailedInline(error),
            ),
          ..._layer(
            context,
            MemoryLayer.agent,
            l10n.memoryLayerAgent,
            memories.agent,
            first: true,
          ),
          ..._layer(
            context,
            MemoryLayer.user,
            l10n.memoryLayerUser,
            memories.user,
          ),
        ],
      ),
    );
  }

  List<Widget> _layer(
    BuildContext context,
    MemoryLayer layer,
    String title,
    MemoryShelf shelf, {
    bool first = false,
  }) {
    final l10n = L10n.of(context);
    return [
      ShelfHeader(title: title, first: first),
      Shelf(
        children: [
          if (shelf.coreOverBudget)
            NoteRow(
              icon: LucideIcons.triangleAlert,
              text: l10n.memoryOverBudget,
            ),
          if (shelf.items.isEmpty) NoteRow(text: l10n.memoryEmpty),
          for (final item in shelf.items)
            _MemoryRow(
              item: item,
              expanded: _expanded == (layer, item.fileName),
              busy: _busy == (layer, item.fileName),
              enabled: _busy == null,
              onTap: () => setState(
                () => _expanded = _expanded == (layer, item.fileName)
                    ? null
                    : (layer, item.fileName),
              ),
              onApprove: () => _approve(layer, item),
              onDelete: () => unawaited(_delete(layer, item)),
            ),
        ],
      ),
    ];
  }
}

class _MemoryRow extends StatelessWidget {
  const _MemoryRow({
    required this.item,
    required this.expanded,
    required this.busy,
    required this.enabled,
    required this.onTap,
    required this.onApprove,
    required this.onDelete,
  });

  final MemoryItem item;
  final bool expanded;
  final bool busy;
  final bool enabled;
  final VoidCallback onTap;
  final VoidCallback onApprove;
  final VoidCallback onDelete;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final pending = item.pendingReason;
    final lastUsed = item.lastUsedMs;
    final ago = lastUsed == null
        ? null
        : formatAgo(
            l10n,
            DateTime.fromMillisecondsSinceEpoch(lastUsed),
            DateTime.now(),
          );
    final tags = [
      item.rule ? l10n.memoryRule : l10n.memoryFact,
      if (item.stale) l10n.memoryStale,
      if (ago != null) l10n.memoryUsed(ago),
    ].join(' · ');
    final description = item.description.trim();
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        ShelfRow(
          leading: Icon(
            pending != null
                ? LucideIcons.circleDot
                : item.rule
                ? LucideIcons.bookmark
                : LucideIcons.lightbulb,
            size: Strokes.icon,
            color: pending != null ? c.heat : c.inkMuted,
          ),
          title: Text(item.name),
          subtitle: Text(
            pending != null ? l10n.memoryPending(pending) : tags,
            maxLines: 2,
          ),
          trailing: Icon(
            expanded ? LucideIcons.chevronUp : LucideIcons.chevronDown,
            size: Strokes.iconSmall,
            color: c.inkFaint,
          ),
          onTap: onTap,
        ),
        if (expanded)
          Padding(
            padding: const EdgeInsets.fromLTRB(Space.lg, 0, Space.lg, Space.md),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (description.isNotEmpty) ...[
                  Text(description, style: t.note.copyWith(color: c.inkMuted)),
                  const SizedBox(height: Space.sm),
                ],
                SelectableText(item.body, style: t.body.copyWith(color: c.ink)),
                const SizedBox(height: Space.md),
                Wrap(
                  spacing: Space.sm,
                  runSpacing: Space.sm,
                  children: [
                    if (pending != null)
                      GalleyButton.solid(
                        icon: LucideIcons.check,
                        label: l10n.memoryApprove,
                        onPressed: enabled ? onApprove : null,
                      ),
                    GalleyButton.danger(
                      icon: LucideIcons.trash2,
                      label: l10n.commonDelete,
                      onPressed: enabled ? onDelete : null,
                    ),
                    if (busy)
                      const Padding(
                        padding: EdgeInsets.all(Space.sm),
                        child: SizedBox.square(
                          dimension: Strokes.icon,
                          child: CircularProgressIndicator.adaptive(
                            strokeWidth: 2,
                          ),
                        ),
                      ),
                  ],
                ),
              ],
            ),
          ),
      ],
    );
  }
}
