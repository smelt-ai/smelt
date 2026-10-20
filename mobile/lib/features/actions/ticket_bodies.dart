/// 单据上的内容与动作：审批、选择题 / 表单、失败。
///
/// 同一套组件出现在三处：指挥台轨上（紧凑：只有允许 / 拒绝或「回答」），逐张处理页
/// （完整：全部选项、命令、目录、原因），对话页输入框上方的停靠单据（完整）。三处长得
/// 一样，用户只学一次。
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/buttons.dart';
import '../../ui/ticket.dart';
import '../../ui/time.dart';

/// 回一个审批：触感、记章、调内核；失败说清楚原因。
Future<bool> answerApproval(
  BuildContext context, {
  required String runtimeId,
  required String sessionId,
  required PermissionRequest permission,
  required PermissionOption option,
}) async {
  final model = AppScope.of(context);
  final l10n = L10n.of(context);
  final messenger = ScaffoldMessenger.maybeOf(context);
  option.isAllow ? HapticFeedback.mediumImpact() : HapticFeedback.lightImpact();
  final kind = option.isAllow ? StampKind.allowed : StampKind.denied;
  model.pass.stamp(sessionKey(runtimeId, sessionId), kind);
  model.pass.decide(runtimeId, sessionId, permission.toolCallId, kind);
  try {
    await model.backend.respondApproval(
      runtimeId,
      sessionId,
      toolCallId: permission.toolCallId,
      optionId: option.optionId,
    );
    return true;
  } catch (error) {
    messenger?.showSnackBar(
      SnackBar(content: Text(l10n.passActionFailed(describeError(error)))),
    );
    return false;
  }
}

String optionLabel(L10n l10n, PermissionOption option) {
  final name = option.name.trim();
  if (name.isNotEmpty) return name;
  return switch (option.kind) {
    PermissionOptionKind.allowOnce => l10n.approvalAllowOnce,
    PermissionOptionKind.allowAlways => l10n.approvalAllowAlways,
    PermissionOptionKind.rejectOnce => l10n.approvalRejectOnce,
    PermissionOptionKind.rejectAlways => l10n.approvalRejectAlways,
  };
}

/// 审批单据的正文与按钮。提交中按钮全部置灰：快速连点不会把同一个回答发两次。
class ApprovalBody extends StatefulWidget {
  const ApprovalBody({
    super.key,
    required this.runtimeId,
    required this.sessionId,
    required this.permission,
    this.expanded = false,
    this.onAnswered,
  });

  final String runtimeId;
  final String sessionId;
  final PermissionRequest permission;

  /// 完整版：列出全部选项和全部细节。
  final bool expanded;
  final VoidCallback? onAnswered;

  @override
  State<ApprovalBody> createState() => _ApprovalBodyState();
}

class _ApprovalBodyState extends State<ApprovalBody> {
  bool _busy = false;

  String get runtimeId => widget.runtimeId;
  String get sessionId => widget.sessionId;
  PermissionRequest get permission => widget.permission;
  bool get expanded => widget.expanded;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final d = permission.details;
    final facts = <String>[
      if (expanded && d.cwd != null && d.cwd!.isNotEmpty)
        l10n.passWorkingDir(d.cwd!),
      if (expanded && d.grantRoot != null && d.grantRoot!.isNotEmpty)
        l10n.passGrantRoot(d.grantRoot!),
      if (d.reason != null && d.reason!.trim().isNotEmpty) d.reason!.trim(),
      if (d.summary != null && d.summary!.trim().isNotEmpty) d.summary!.trim(),
    ];

    Future<void> answer(PermissionOption option) async {
      if (_busy) return;
      setState(() => _busy = true);
      final ok = await answerApproval(
        context,
        runtimeId: runtimeId,
        sessionId: sessionId,
        permission: permission,
        option: option,
      );
      if (!mounted) return;
      setState(() => _busy = false);
      if (ok) widget.onAnswered?.call();
    }

    VoidCallback? press(PermissionOption option) =>
        _busy ? null : () => answer(option);

    final options = permission.options;
    final allow = options.where(
      (o) => o.kind == PermissionOptionKind.allowOnce,
    );
    final reject = options.where(
      (o) => o.kind == PermissionOptionKind.rejectOnce,
    );
    final primaryAllow = allow.isNotEmpty
        ? allow.first
        : options.where((o) => o.isAllow).firstOrNull;
    final primaryReject = reject.isNotEmpty
        ? reject.first
        : options.where((o) => o.isReject).firstOrNull;

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(
          permission.question,
          maxLines: expanded ? null : 2,
          overflow: expanded ? null : TextOverflow.ellipsis,
          style: t.body.copyWith(color: c.onPaper),
        ),
        if (d.command != null && d.command!.trim().isNotEmpty) ...[
          const SizedBox(height: Space.sm),
          _CommandBlock(command: d.command!.trim(), expanded: expanded),
        ],
        for (final fact in facts.take(expanded ? facts.length : 1)) ...[
          const SizedBox(height: Space.xs),
          Text(
            fact,
            maxLines: expanded ? null : 1,
            overflow: expanded ? null : TextOverflow.ellipsis,
            style: t.meta.copyWith(color: c.onPaperMuted),
          ),
        ],
        const SizedBox(height: Space.md),
        if (!expanded)
          Row(
            children: [
              if (primaryAllow != null)
                Expanded(
                  child: GalleyButton.heat(
                    label: l10n.commonAllow,
                    onPressed: press(primaryAllow),
                  ),
                ),
              if (primaryAllow != null && primaryReject != null)
                const SizedBox(width: Space.sm),
              if (primaryReject != null)
                Expanded(
                  child: GalleyButton.paper(
                    label: l10n.commonDeny,
                    onPressed: press(primaryReject),
                  ),
                ),
            ],
          )
        else
          Wrap(
            spacing: Space.sm,
            runSpacing: Space.sm,
            children: [
              for (final option in _ordered(options))
                switch (option.kind) {
                  PermissionOptionKind.allowOnce => GalleyButton.heat(
                    label: optionLabel(l10n, option),
                    onPressed: press(option),
                  ),
                  PermissionOptionKind.rejectAlways => GalleyButton.danger(
                    label: optionLabel(l10n, option),
                    onPressed: press(option),
                  ),
                  _ => GalleyButton.paper(
                    label: optionLabel(l10n, option),
                    onPressed: press(option),
                  ),
                },
            ],
          ),
      ],
    );
  }

  /// 允许一次在最前，其次总是允许、拒绝、总是拒绝。
  static List<PermissionOption> _ordered(List<PermissionOption> options) {
    int rank(PermissionOption o) => switch (o.kind) {
      PermissionOptionKind.allowOnce => 0,
      PermissionOptionKind.allowAlways => 1,
      PermissionOptionKind.rejectOnce => 2,
      PermissionOptionKind.rejectAlways => 3,
    };
    return [...options]..sort((a, b) => rank(a).compareTo(rank(b)));
  }
}

class _CommandBlock extends StatelessWidget {
  const _CommandBlock({required this.command, required this.expanded});

  final String command;
  final bool expanded;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return Container(
      padding: const EdgeInsets.symmetric(
        horizontal: Space.sm + 2,
        vertical: Space.sm,
      ),
      decoration: BoxDecoration(
        border: Border(left: BorderSide(color: c.paperEdge, width: 1)),
      ),
      // 紧凑版（轨上）只放一行、末尾省略号：命令在连字符处折行会被读错（`smelt-acp-` /
      // `view`）。完整版照原样折行显示全文。
      child: Text(
        command,
        maxLines: expanded ? 12 : 1,
        softWrap: expanded,
        overflow: TextOverflow.ellipsis,
        style: t.code.copyWith(color: c.onPaper),
      ),
    );
  }
}

/// 选择题 / 表单。紧凑版只有题目和「回答」，点了进完整版。
class ElicitationBody extends StatefulWidget {
  const ElicitationBody({
    super.key,
    required this.runtimeId,
    required this.sessionId,
    required this.elicitation,
    this.expanded = false,
    this.onOpen,
    this.onAnswered,
  });

  final String runtimeId;
  final String sessionId;
  final Elicitation elicitation;
  final bool expanded;

  /// 紧凑版点「回答」。
  final VoidCallback? onOpen;
  final VoidCallback? onAnswered;

  @override
  State<ElicitationBody> createState() => _ElicitationBodyState();
}

class _ElicitationBodyState extends State<ElicitationBody> {
  final Map<int, String> _text = {};
  bool _busy = false;

  Future<void> _run(Future<void> Function() action) async {
    final l10n = L10n.of(context);
    final messenger = ScaffoldMessenger.maybeOf(context);
    setState(() => _busy = true);
    try {
      await action();
    } catch (error) {
      messenger?.showSnackBar(
        SnackBar(content: Text(l10n.passActionFailed(describeError(error)))),
      );
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final model = AppScope.of(context);
    final e = widget.elicitation;
    final message = Text(
      e.message,
      maxLines: widget.expanded ? null : 3,
      overflow: widget.expanded ? null : TextOverflow.ellipsis,
      style: t.body.copyWith(color: c.onPaper),
    );
    if (!widget.expanded) {
      return Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          message,
          const SizedBox(height: Space.md),
          GalleyButton.heat(label: l10n.commonAnswer, onPressed: widget.onOpen),
        ],
      );
    }

    final backend = model.backend;
    final ready = e.isReadyWith(_text);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        message,
        for (final (index, field) in e.fields.indexed) ...[
          const SizedBox(height: Space.lg),
          Row(
            children: [
              Expanded(
                child: Text(
                  field.title,
                  style: t.label.copyWith(color: c.onPaper),
                ),
              ),
              if (field.required_)
                Text(
                  l10n.elicitationRequired,
                  style: t.caption.copyWith(color: c.onPaperMuted),
                ),
            ],
          ),
          const SizedBox(height: Space.sm),
          switch (field.kind) {
            ElicitationFieldKind.select ||
            ElicitationFieldKind.multiSelect => Wrap(
              spacing: Space.sm,
              runSpacing: Space.sm,
              children: [
                for (final (option, label) in field.options.indexed)
                  _Choice(
                    label: label,
                    selected: field.chosen.contains(option),
                    multi: field.kind == ElicitationFieldKind.multiSelect,
                    onTap: _busy
                        ? null
                        : () {
                            HapticFeedback.selectionClick();
                            _run(
                              () => backend.chooseElicitation(
                                widget.runtimeId,
                                widget.sessionId,
                                fieldIndex: index,
                                optionIndex: option,
                              ),
                            );
                          },
                  ),
              ],
            ),
            ElicitationFieldKind.text => TextFormField(
              initialValue: field.textValue,
              obscureText: field.secret,
              minLines: 1,
              maxLines: field.secret ? 1 : 4,
              style: t.body.copyWith(color: c.onPaper),
              decoration: InputDecoration(
                hintText: l10n.elicitationTextHint,
                fillColor: c.paper,
              ),
              onChanged: (value) => setState(() => _text[index] = value),
            ),
            ElicitationFieldKind.externalUrl => Align(
              alignment: Alignment.centerLeft,
              child: GalleyButton.paper(
                label: l10n.elicitationOpenLink,
                icon: LucideIcons.externalLink,
                onPressed: field.url == null
                    ? null
                    : () => launchUrl(
                        Uri.parse(field.url!),
                        mode: LaunchMode.externalApplication,
                      ),
              ),
            ),
          },
        ],
        const SizedBox(height: Space.lg),
        Row(
          children: [
            Expanded(
              child: GalleyButton.heat(
                label: l10n.elicitationSubmit,
                onPressed: _busy || !ready
                    ? null
                    : () => _run(() async {
                        HapticFeedback.mediumImpact();
                        model.pass.stamp(
                          sessionKey(widget.runtimeId, widget.sessionId),
                          StampKind.answered,
                        );
                        await backend.submitElicitation(
                          widget.runtimeId,
                          widget.sessionId,
                          [
                            for (final entry in _text.entries)
                              ElicitationTextInput(
                                fieldIndex: entry.key,
                                value: entry.value,
                              ),
                          ],
                        );
                        widget.onAnswered?.call();
                      }),
              ),
            ),
            const SizedBox(width: Space.sm),
            GalleyButton.paper(
              label: l10n.elicitationDismiss,
              onPressed: _busy
                  ? null
                  : () => _run(() async {
                      await backend.dismissElicitation(
                        widget.runtimeId,
                        widget.sessionId,
                      );
                      widget.onAnswered?.call();
                    }),
            ),
          ],
        ),
      ],
    );
  }
}

class _Choice extends StatelessWidget {
  const _Choice({
    required this.label,
    required this.selected,
    required this.multi,
    required this.onTap,
  });

  final String label;
  final bool selected;
  final bool multi;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return Semantics(
      selected: selected,
      button: true,
      child: Material(
        color: selected ? c.onPaper : c.paper,
        shape: RoundedRectangleBorder(
          borderRadius: const BorderRadius.all(Radii.control),
          side: BorderSide(color: selected ? c.onPaper : c.paperEdge),
        ),
        child: InkWell(
          onTap: onTap,
          customBorder: const RoundedRectangleBorder(
            borderRadius: BorderRadius.all(Radii.control),
          ),
          child: ConstrainedBox(
            constraints: const BoxConstraints(minHeight: Space.touch - 4),
            child: Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: Space.md,
                vertical: Space.sm,
              ),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(
                    selected
                        ? (multi
                              ? LucideIcons.squareCheck
                              : LucideIcons.circleCheck)
                        : (multi ? LucideIcons.square : LucideIcons.circle),
                    size: Strokes.iconSmall,
                    color: selected ? c.paper : c.onPaperMuted,
                  ),
                  const SizedBox(width: Space.sm),
                  Flexible(
                    child: Text(
                      label,
                      style: t.callout.copyWith(
                        color: selected ? c.paper : c.onPaper,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// 详情还没取回来时单据上的占位：两条纸边色的短条。
class TicketSkeleton extends StatelessWidget {
  const TicketSkeleton({super.key});

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    Widget bar(double f) => FractionallySizedBox(
      widthFactor: f,
      alignment: Alignment.centerLeft,
      child: Container(
        height: 10,
        decoration: BoxDecoration(
          color: c.paperEdge.withValues(alpha: 0.6),
          borderRadius: const BorderRadius.all(Radii.tag),
        ),
      ),
    );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        bar(0.9),
        const SizedBox(height: Space.sm),
        bar(0.6),
        const SizedBox(height: Space.md + Space.touch),
      ],
    );
  }
}

/// 单据上的章：从 [StampRecord] 生成。
TicketStamp? stampFor(L10n l10n, StampRecord? record) {
  if (record == null) return null;
  final time = formatClock(record.at);
  return switch (record.kind) {
    StampKind.allowed => TicketStamp(text: l10n.passStampAllowed(time)),
    StampKind.denied => TicketStamp(
      text: l10n.passStampDenied(time),
      tone: StampTone.deny,
    ),
    StampKind.answered => TicketStamp(
      text: l10n.passStampAnswered(time),
      tone: StampTone.neutral,
    ),
  };
}
