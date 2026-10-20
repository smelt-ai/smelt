/// 「智能体」页私有的小部件和动作：详情页标题、说明行、字段行，开关与立即运行的提示。
library;

import 'package:flutter/material.dart';

import '../../data/models.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/station.dart';
import 'agents_board.dart';

/// 详情页导航栏标题：名字，下面一行是它所在的机器（灶台标签色 + 机器名）。
class DetailTitle extends StatelessWidget {
  const DetailTitle({super.key, required this.title, required this.runtime});

  final String title;
  final FleetRuntime? runtime;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final r = runtime;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(title, maxLines: 1, overflow: TextOverflow.ellipsis),
        if (r != null)
          StationLabel(
            name: r.name,
            color: c.station(r.record.accent),
            style: t.caption,
          ),
      ],
    );
  }
}

/// 搁板里的一句说明（加载中、离线、空、出错）。不是按钮，也不假装是一行内容。
class NoteRow extends StatelessWidget {
  const NoteRow({
    super.key,
    required this.text,
    this.icon,
    this.danger = false,
    this.busy = false,
    this.action,
  });

  final String text;
  final IconData? icon;
  final bool danger;
  final bool busy;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final color = danger ? c.danger : c.inkMuted;
    return ConstrainedBox(
      constraints: const BoxConstraints(minHeight: Space.touch + Space.sm),
      child: Padding(
        padding: const EdgeInsets.symmetric(
          horizontal: Space.lg,
          vertical: Space.md,
        ),
        child: Row(
          children: [
            if (busy)
              SizedBox.square(
                dimension: Strokes.iconSmall,
                child: CircularProgressIndicator(
                  strokeWidth: Strokes.spinner,
                  color: c.inkMuted,
                ),
              )
            else if (icon != null)
              Icon(icon, size: Strokes.iconSmall, color: color),
            if (busy || icon != null) const SizedBox(width: Space.md),
            Expanded(
              child: Text(text, style: t.body.copyWith(color: color)),
            ),
            if (action != null) ...[const SizedBox(width: Space.sm), action!],
          ],
        ),
      ),
    );
  }
}

/// 详情里的一个字段：左边是名目，右边是值。值可以换行，不截断。
class FieldRow extends StatelessWidget {
  const FieldRow({
    super.key,
    required this.label,
    required this.value,
    this.valueStyle,
  });

  final String label;
  final String value;
  final TextStyle? valueStyle;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return MergeSemantics(
      child: ConstrainedBox(
        constraints: const BoxConstraints(minHeight: Space.touch),
        child: Padding(
          padding: const EdgeInsets.symmetric(
            horizontal: Space.lg,
            vertical: Space.md,
          ),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(label, style: t.body.copyWith(color: c.inkMuted)),
              const SizedBox(width: Space.lg),
              Expanded(
                child: Text(
                  value,
                  textAlign: TextAlign.end,
                  style: (valueStyle ?? t.body).copyWith(color: c.ink),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 详情页底部的主动作区：贴着安全区，上面一条发丝线把它和内容分开。
class BottomActionBar extends StatelessWidget {
  const BottomActionBar({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    return DecoratedBox(
      decoration: BoxDecoration(
        color: c.ground,
        border: Border(top: BorderSide(color: c.hairline)),
      ),
      child: SafeArea(
        top: false,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(
            Space.page,
            Space.md,
            Space.page,
            Space.md,
          ),
          child: child,
        ),
      ),
    );
  }
}

/// 开关一条自动化。开关先拨过去；失败时 [AgentsBoard] 把它拨回原位，这里提示原因。
Future<void> toggleAutomation(
  BuildContext context,
  AgentsBoard board,
  String runtimeId,
  AutomationSummary automation,
  bool enabled,
) async {
  final messenger = ScaffoldMessenger.of(context);
  final l10n = L10n.of(context);
  try {
    await board.setEnabled(runtimeId, automation.id, enabled);
  } catch (error) {
    final message = describeError(error);
    messenger.showSnackBar(
      SnackBar(
        content: Text(
          enabled
              ? l10n.agentsEnableFailed(automation.name, message)
              : l10n.agentsDisableFailed(automation.name, message),
        ),
      ),
    );
  }
}

/// 立即运行一次，结果用一条提示说清楚。
Future<void> runAutomationNow(
  BuildContext context,
  AgentsBoard board,
  String runtimeId,
  AutomationSummary automation,
) async {
  final messenger = ScaffoldMessenger.of(context);
  final l10n = L10n.of(context);
  try {
    await board.runOnce(runtimeId, automation.id);
    messenger.showSnackBar(
      SnackBar(content: Text(l10n.agentsRunNowStarted(automation.name))),
    );
  } catch (error) {
    messenger.showSnackBar(
      SnackBar(
        content: Text(
          l10n.agentsRunNowFailed(automation.name, describeError(error)),
        ),
      ),
    );
  }
}
