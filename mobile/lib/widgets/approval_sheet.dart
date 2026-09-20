import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../models/acp_snapshot.dart';
import 'approval_card.dart';

/// 审批详情 sheet（设计稿 B）。
///
/// 卡片上只放「够不够判断」的最小集合，展开后给全量：命令全文、工作目录、理由、
/// 授权范围。三个动作**纵向排列**而不是并排——授权粒度不同的东西并排放，等于
/// 邀请用户按位置而不是按语义点。
///
/// 顺序也是有意的：仅此一次在最上（最常用、代价最小），总是允许在中间且是幽灵
/// 按钮，拒绝在最下。**拒绝放最后不是因为它次要**，而是因为它不可撤销程度最低——
/// 拒错了再点一次就是了，允许错了命令已经跑完。
Future<void> showApprovalSheet(
  BuildContext context, {
  required PendingPermission permission,
  required void Function(String optionId) onRespond,
  String? subtitle,
}) {
  return showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    showDragHandle: true,
    builder: (sheetContext) => _ApprovalSheet(
      permission: permission,
      subtitle: subtitle,
      onRespond: (optionId) {
        Navigator.of(sheetContext).pop();
        onRespond(optionId);
      },
    ),
  );
}

class _ApprovalSheet extends StatelessWidget {
  const _ApprovalSheet({
    required this.permission,
    required this.onRespond,
    this.subtitle,
  });

  final PendingPermission permission;
  final void Function(String optionId) onRespond;
  final String? subtitle;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final muted = theme.colorScheme.onSurfaceVariant;
    final details = permission.details;

    final allowOnce = permission.options
        .where((option) => option.isAllow && !option.isAlways)
        .firstOrNull;
    final always = permission.options
        .where((option) => option.isAlways)
        .toList(growable: false);
    final reject = permission.options
        .where((option) => option.isReject)
        .toList(growable: false);
    // 既不是允许也不是拒绝、也不是 always 的选项（agent 可以自定义）不能丢，
    // 否则用户在 sheet 里看到的动作会比卡片上少。
    final others = permission.options
        .where(
          (option) =>
              option != allowOnce &&
              !option.isAlways &&
              !option.isReject &&
              !option.isAllow,
        )
        .toList(growable: false);

    return SafeArea(
      child: ConstrainedBox(
        // 不铺满全屏：留一截背景可见，用户始终知道自己在一个可关掉的层里。
        constraints: BoxConstraints(
          maxHeight: MediaQuery.sizeOf(context).height * 0.82,
        ),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(20, 0, 20, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                permission.question,
                style: theme.textTheme.titleMedium?.copyWith(
                  fontWeight: FontWeight.w600,
                ),
              ),
              if (subtitle?.isNotEmpty == true) ...[
                const SizedBox(height: 4),
                Text(
                  subtitle!,
                  style: theme.textTheme.bodySmall?.copyWith(color: muted),
                ),
              ],
              const SizedBox(height: 16),
              // 详情可能很长（一屏放不下的命令），只让这段滚，按钮永远钉在底部。
              Flexible(
                child: SingleChildScrollView(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      ApprovalDetailsView(
                        details: details,
                        showInlineFacts: false,
                      ),
                      const SizedBox(height: 12),
                      ..._facts(context, details),
                    ],
                  ),
                ),
              ),
              const SizedBox(height: 16),
              if (allowOnce case final option?)
                _SheetButton(
                  label: option.name,
                  onPressed: () => _respond(option.optionId),
                  tone: _Tone.allow,
                ),
              for (final option in others)
                _SheetButton(
                  label: option.name,
                  onPressed: () => _respond(option.optionId),
                  tone: _Tone.neutral,
                ),
              for (final option in always)
                _SheetButton(
                  label: option.name,
                  onPressed: () => _respond(option.optionId),
                  tone: _Tone.neutral,
                ),
              for (final option in reject)
                _SheetButton(
                  label: option.name,
                  onPressed: () => _respond(option.optionId),
                  tone: _Tone.reject,
                ),
            ],
          ),
        ),
      ),
    );
  }

  void _respond(String optionId) {
    HapticFeedback.mediumImpact();
    onRespond(optionId);
  }

  /// 「判断依据」表。只渲染协议真给了的字段——设计稿上的「影响范围」在 command
  /// 类型里并没有对应数据，与其编一个，不如不显示。
  List<Widget> _facts(BuildContext context, ApprovalDetails details) {
    return switch (details) {
      ApprovalDetailsCommand(cwd: final cwd, reason: final reason) => [
        if (cwd?.isNotEmpty == true)
          _FactRow(label: 'Working directory', value: cwd!, mono: true),
        if (reason?.isNotEmpty == true) _FactRow(label: 'Why', value: reason!),
      ],
      ApprovalDetailsFileChange(grantRoot: final root, reason: final reason) =>
        [
          if (root?.isNotEmpty == true)
            _FactRow(label: 'Grants access to', value: root!, mono: true),
          if (reason?.isNotEmpty == true)
            _FactRow(label: 'Why', value: reason!),
        ],
      _ => const <Widget>[],
    };
  }
}

class _FactRow extends StatelessWidget {
  const _FactRow({required this.label, required this.value, this.mono = false});

  final String label;
  final String value;
  final bool mono;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 7),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 118,
            child: Text(
              label,
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          ),
          const SizedBox(width: 10),
          Expanded(
            child: SelectableText(
              value,
              style: theme.textTheme.bodySmall?.copyWith(
                fontFamily: mono ? 'monospace' : null,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

enum _Tone { allow, neutral, reject }

class _SheetButton extends StatelessWidget {
  const _SheetButton({
    required this.label,
    required this.onPressed,
    required this.tone,
  });

  final String label;
  final VoidCallback onPressed;
  final _Tone tone;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final foreground = switch (tone) {
      _Tone.allow => colors.primary,
      _Tone.reject => colors.error,
      _Tone.neutral => colors.onSurfaceVariant,
    };
    return Padding(
      padding: const EdgeInsets.only(bottom: 8),
      child: SizedBox(
        height: 46,
        child: OutlinedButton(
          onPressed: onPressed,
          style: OutlinedButton.styleFrom(foregroundColor: foreground),
          child: Text(label, overflow: TextOverflow.ellipsis),
        ),
      ),
    );
  }
}
