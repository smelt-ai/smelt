import 'package:flutter/material.dart';

import '../theme/smelt_theme.dart';
import 'package:flutter/services.dart';

import '../models/acp_snapshot.dart';
import 'approval_sheet.dart';

/// 一张可以就地决策的审批卡。
///
/// 跟会话页里那条 banner 的区别在三点，都是设计稿 B 要解决的问题：
///
///   1. **长命令换行，不横滚。** 原来命令用单行 `SelectableText`，手机上超宽就
///      得横向拖，最需要看清的东西反而看不全。
///   2. **补上判断依据。** 工作目录和理由本来就在 `ApprovalDetailsCommand` 里，
///      只是没渲染——没有它们，用户是在盲批。
///   3. **允许不再是最抢眼的落点。** 原来 Allow 是绿色实心按钮、位于拇指热区、
///      无二次确认；「总是允许」还跟「仅此一次」平权并排。这里把允许改成描边，
///      并把带 Always 的选项降到次要位置——误触的代价是不可逆的。
class ApprovalCard extends StatelessWidget {
  const ApprovalCard({
    super.key,
    required this.permission,
    required this.onRespond,
    this.header,
    this.submitting = false,
    this.detailSubtitle,
    this.showWorkspacePath = true,
  });

  final PendingPermission permission;

  /// 回调带上 optionId；调用方自己知道是哪个会话，卡片不关心。
  final void Function(String optionId) onRespond;

  /// 指挥台用来标出「这是哪个会话」；会话页内部不需要。
  final Widget? header;
  final bool submitting;

  /// 详情 sheet 顶部那行「项目 · agent · 多久以前」。
  final String? detailSubtitle;

  /// 卡片上是否画工作目录。
  ///
  /// 自动化 Run 的工作目录是 daemon 按 id 哈希出来的托管目录，用户从没打开过，
  /// 也不能从它判断这条命令危不危险；在窄屏上它还要占三行，把「允许 / 拒绝」
  /// 推出首屏。来源行已经答了「这是谁起的」，路径留在详情 sheet 里即可。
  final bool showWorkspacePath;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = theme.colorScheme;

    // 「总是允许」跟一次性决策不是一个量级，不并排。
    final primary = permission.options
        .where((option) => !option.isAlways)
        .toList(growable: false);
    final always = permission.options
        .where((option) => option.isAlways)
        .toList(growable: false);

    return Card(
      margin: EdgeInsets.zero,
      clipBehavior: Clip.antiAlias,
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (header != null) ...[header!, const SizedBox(height: 8)],
            // 点正文 = 看全量详情；点上面的 header = 进会话。两个不同的去处，
            // 所以不能共用一个大 InkWell。
            InkWell(
              onTap: () => showApprovalSheet(
                context,
                permission: permission,
                onRespond: onRespond,
                subtitle: detailSubtitle,
              ),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Expanded(
                        child: Text(
                          permission.question,
                          style: theme.textTheme.titleSmall?.copyWith(
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                      ),
                      Icon(
                        Icons.unfold_more,
                        size: 16,
                        color: colors.onSurfaceVariant,
                      ),
                    ],
                  ),
                  const SizedBox(height: 8),
                  ApprovalDetailsView(
                    details: permission.details,
                    showWorkspacePath: showWorkspacePath,
                  ),
                ],
              ),
            ),
            const SizedBox(height: 12),
            if (submitting)
              const Row(
                children: [
                  SizedBox(
                    width: 16,
                    height: 16,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  ),
                  SizedBox(width: 8),
                  Text('Submitting...'),
                ],
              )
            else ...[
              Row(
                children: [
                  for (final option in primary) ...[
                    Expanded(
                      child: _OptionButton(
                        option: option,
                        onPressed: () => _respond(option.optionId),
                      ),
                    ),
                    if (option != primary.last) const SizedBox(width: 8),
                  ],
                ],
              ),
              for (final option in always)
                Padding(
                  padding: const EdgeInsets.only(top: 4),
                  child: TextButton(
                    onPressed: () => _respond(option.optionId),
                    style: TextButton.styleFrom(
                      foregroundColor: colors.onSurfaceVariant,
                    ),
                    child: Text(option.name),
                  ),
                ),
            ],
          ],
        ),
      ),
    );
  }

  void _respond(String optionId) {
    HapticFeedback.mediumImpact();
    onRespond(optionId);
  }
}

class _OptionButton extends StatelessWidget {
  const _OptionButton({required this.option, required this.onPressed});

  final PermissionOption option;
  final VoidCallback onPressed;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    return OutlinedButton(
      onPressed: onPressed,
      style: OutlinedButton.styleFrom(
        foregroundColor: option.isReject
            ? colors.error
            : option.isAllow
            ? colors.primary
            : null,
      ),
      child: Text(option.name, overflow: TextOverflow.ellipsis),
    );
  }
}

/// 审批依据。命令走等宽 + 自动换行：手机上横滚等于看不见。
class ApprovalDetailsView extends StatelessWidget {
  const ApprovalDetailsView({
    super.key,
    required this.details,
    this.showInlineFacts = true,
    this.showWorkspacePath = true,
  });

  final ApprovalDetails details;

  /// 是否把工作目录 / 理由跟在证据后面。
  ///
  /// 卡片上要（那是它唯一的展示位）；详情 sheet 里不要——sheet 有一张正式的
  /// 事实表，两边都画就是同一句话显示两遍。
  final bool showInlineFacts;

  /// 见 [ApprovalCard.showWorkspacePath]。
  final bool showWorkspacePath;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final muted = theme.textTheme.bodySmall?.copyWith(
      color: theme.colorScheme.onSurfaceVariant,
    );

    return switch (details) {
      ApprovalDetailsCommand(
        command: final command,
        cwd: final cwd,
        reason: final reason,
      ) =>
        Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            _CommandBlock(command: command),
            if (showInlineFacts && reason?.isNotEmpty == true)
              Padding(
                padding: const EdgeInsets.only(top: 6),
                child: Text(reason!, style: muted),
              ),
            if (showInlineFacts && showWorkspacePath && cwd?.isNotEmpty == true)
              Padding(
                padding: const EdgeInsets.only(top: 4),
                child: Text('Working directory: $cwd', style: muted),
              ),
          ],
        ),
      ApprovalDetailsFileChange(reason: final reason, grantRoot: final root) =>
        Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (showInlineFacts && reason?.isNotEmpty == true)
              Text(reason!, style: muted),
            if (showInlineFacts && root?.isNotEmpty == true)
              Padding(
                padding: const EdgeInsets.only(top: 4),
                child: Text('Grants access to: $root', style: muted),
              ),
          ],
        ),
      ApprovalDetailsPermissions(summary: final summary) => Text(
        summary,
        style: muted,
      ),
      ApprovalDetailsGeneric() => const SizedBox.shrink(),
    };
  }
}

class _CommandBlock extends StatelessWidget {
  const _CommandBlock({required this.command});

  final String command;

  @override
  Widget build(BuildContext context) {
    final smelt = context.smeltColors;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
      decoration: BoxDecoration(
        // 命令块是「下沉」的，深色下要比卡片更暗，不是更亮。
        color: smelt.sunken,
        borderRadius: const BorderRadius.horizontal(right: Radius.circular(7)),
        border: Border(left: BorderSide(color: smelt.needsAttention, width: 3)),
      ),
      child: SelectableText(
        command,
        style: const TextStyle(
          fontFamily: 'monospace',
          fontSize: 12.5,
          height: 1.45,
        ),
      ),
    );
  }
}
