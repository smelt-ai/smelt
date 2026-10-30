/// 「智能体」页私有的小部件和动作：详情页标题、说明行、字段行，开关与立即运行的提示。
library;

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/shelf.dart';
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

/// 智能体用的模型给人看的名字。[groups] 是这台机器能力库里的可选模型，还没取到时为 null，
/// 先显示原值；取到了却找不到，说明这台机器上已经没有它了。
String agentModelLabel(
  L10n l10n,
  List<ModelGroup>? groups,
  String provider,
  String id,
) {
  if (provider.isEmpty || id.isEmpty) return l10n.agentEditModelDefault;
  if (groups == null) return '$provider / $id';
  for (final group in groups) {
    for (final option in group.models) {
      if (option.provider == provider && option.id == id) return option.label;
    }
  }
  return l10n.agentEditModelMissing('$provider / $id');
}

IconData pluginIcon(PluginKind kind) => switch (kind) {
  PluginKind.skill => LucideIcons.sparkles,
  PluginKind.extension_ => LucideIcons.puzzle,
};

/// 插件、MCP 的一行要显示什么。编辑页和详情页都按它画，规则只在这一处。
@immutable
class ChoiceItem {
  const ChoiceItem({
    required this.key,
    required this.icon,
    required this.title,
    this.subtitle,
    this.danger = false,
    this.canSelect = true,
  });

  /// 插件 id / MCP 服务器名，也是勾选时存的值。
  final String key;
  final IconData icon;
  final String title;
  final String? subtitle;
  final bool danger;

  /// 坏了的插件不能新勾（已经勾着的照样能取消）。
  final bool canSelect;
}

/// [options] 为 null 是没读到能力库：选了的照原值列出，不能标成「已经没有了」。
/// [includeUnchosen] 为真时列出全部可选项（编辑页），否则只列选了的（详情页，按选的顺序）。
List<ChoiceItem> _choiceItems<T>({
  required L10n l10n,
  required List<T>? options,
  required List<String> chosen,
  required bool includeUnchosen,
  required String Function(T) keyOf,
  required ChoiceItem Function(T) itemOf,
  required IconData unreadIcon,
}) {
  if (options == null) {
    return [
      for (final key in chosen)
        ChoiceItem(key: key, icon: unreadIcon, title: key),
    ];
  }
  final byKey = {for (final option in options) keyOf(option): option};
  ChoiceItem gone(String key) => ChoiceItem(
    key: key,
    icon: LucideIcons.circleSlash,
    title: key,
    subtitle: l10n.agentEditMissing,
    danger: true,
  );
  if (!includeUnchosen) {
    return [
      for (final key in chosen)
        if (byKey[key] case final option?) itemOf(option) else gone(key),
    ];
  }
  return [
    for (final option in options) itemOf(option),
    // 勾着、但这台机器上已经没有的：列出来能取消，不悄悄丢掉。
    for (final key in chosen)
      if (!byKey.containsKey(key)) gone(key),
  ];
}

String? _nonEmpty(String? text) {
  final trimmed = text?.trim() ?? '';
  return trimmed.isEmpty ? null : trimmed;
}

List<ChoiceItem> pluginChoiceItems(
  L10n l10n,
  List<PluginOption>? options,
  List<String> chosen, {
  required bool includeUnchosen,
}) => _choiceItems(
  l10n: l10n,
  options: options,
  chosen: chosen,
  includeUnchosen: includeUnchosen,
  keyOf: (plugin) => plugin.id,
  unreadIcon: LucideIcons.plug,
  itemOf: (plugin) => ChoiceItem(
    key: plugin.id,
    icon: pluginIcon(plugin.kind),
    title: plugin.name,
    subtitle: plugin.broken ?? _nonEmpty(plugin.description),
    danger: plugin.broken != null,
    canSelect: plugin.broken == null,
  ),
);

List<ChoiceItem> mcpChoiceItems(
  L10n l10n,
  List<McpOption>? options,
  List<String> chosen, {
  required bool includeUnchosen,
}) => _choiceItems(
  l10n: l10n,
  options: options,
  chosen: chosen,
  includeUnchosen: includeUnchosen,
  keyOf: (server) => server.name,
  unreadIcon: LucideIcons.server,
  itemOf: (server) => ChoiceItem(
    key: server.name,
    icon: LucideIcons.server,
    title: server.name,
    subtitle: server.enabled
        ? _nonEmpty(server.description)
        : l10n.agentEditMcpDisabled,
  ),
);

/// 插件、MCP 的一行。给了 [onChanged] 就是能勾的（编辑页），否则只看（详情页）。
class ChoiceRow extends StatelessWidget {
  const ChoiceRow({
    super.key,
    required this.item,
    this.checked = false,
    this.onChanged,
    this.selectable = false,
  });

  final ChoiceItem item;
  final bool checked;
  final ValueChanged<bool>? onChanged;

  /// 画勾选框（编辑页）。为假时只看。
  final bool selectable;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final sub = item.subtitle;
    return ShelfRow(
      leading: Icon(item.icon, size: Strokes.icon, color: c.inkMuted),
      title: Text(item.title),
      subtitle: sub == null
          ? null
          : Text(
              sub,
              maxLines: 2,
              style: item.danger ? t.meta.copyWith(color: c.danger) : null,
            ),
      semanticLabel: item.title,
      trailing: selectable
          ? Checkbox(
              value: checked,
              onChanged: onChanged == null
                  ? null
                  : (v) => onChanged!(v ?? false),
            )
          : null,
      onTap: onChanged == null ? null : () => onChanged!(!checked),
    );
  }
}
