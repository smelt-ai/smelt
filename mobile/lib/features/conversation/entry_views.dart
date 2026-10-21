import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/status.dart';
import '../../ui/time.dart';
import 'conversation_images.dart';
import 'conversation_logic.dart';
import 'conversation_markdown.dart';
import 'tool_views.dart';

/// 用户消息：右对齐，中性底、搁板圆角。原样显示用户打的字，不当 markdown 解析。
class UserMessage extends StatelessWidget {
  const UserMessage({super.key, required this.entry, this.loadImage});

  final ConversationEntry entry;
  final ConversationImageLoader? loadImage;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final text = entry.text.trim();
    return LayoutBuilder(
      builder: (context, constraints) => Align(
        alignment: Alignment.centerRight,
        child: ConstrainedBox(
          constraints: BoxConstraints(maxWidth: constraints.maxWidth * 0.85),
          child: DecoratedBox(
            decoration: BoxDecoration(
              color: c.control,
              borderRadius: const BorderRadius.all(Radii.shelf),
            ),
            child: Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: Space.md + 2,
                vertical: Space.sm + 2,
              ),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.end,
                mainAxisSize: MainAxisSize.min,
                children: [
                  if (entry.images.isNotEmpty)
                    Wrap(
                      spacing: Space.sm,
                      runSpacing: Space.sm,
                      alignment: WrapAlignment.end,
                      children: [
                        for (final image in entry.images)
                          ConversationThumbnail(
                            image: image,
                            loadImage: loadImage,
                          ),
                      ],
                    ),
                  if (entry.images.isNotEmpty && text.isNotEmpty)
                    const SizedBox(height: Space.sm),
                  if (text.isNotEmpty)
                    SelectableText(text, style: t.body.copyWith(color: c.ink)),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// 助手正文。最终回答下面写这回合用了多久，并给一个复制。
class AssistantMessage extends StatelessWidget {
  const AssistantMessage({
    super.key,
    required this.text,
    this.isFinal = false,
    this.durationMs,
  });

  final String text;
  final bool isFinal;
  final int? durationMs;

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        ConversationMarkdown(data: text),
        if (isFinal) _AnswerFooter(copyText: text, durationMs: durationMs),
      ],
    );
  }
}

class _AnswerFooter extends StatelessWidget {
  const _AnswerFooter({required this.copyText, this.durationMs});

  final String copyText;
  final int? durationMs;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final duration = durationMs;
    return Row(
      children: [
        IconButton(
          tooltip: l10n.conversationCopyAnswer,
          onPressed: () {
            Clipboard.setData(ClipboardData(text: copyText));
            ScaffoldMessenger.maybeOf(
              context,
            )?.showSnackBar(SnackBar(content: Text(l10n.commonCopied)));
          },
          icon: Icon(
            LucideIcons.copy,
            size: Strokes.iconSmall,
            color: c.inkMuted,
          ),
        ),
        if (duration != null)
          Text(
            l10n.conversationTook(formatTurnDuration(duration)),
            style: t.monoTime.copyWith(color: c.inkMuted),
          ),
      ],
    );
  }
}

/// 思考块：收着时只露第一行，点开看全文。
class ThoughtBlock extends StatefulWidget {
  const ThoughtBlock({super.key, required this.text});

  final String text;

  @override
  State<ThoughtBlock> createState() => _ThoughtBlockState();
}

class _ThoughtBlockState extends State<ThoughtBlock> {
  bool _open = false;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final preview = widget.text
        .split('\n')
        .map((line) => line.trim())
        .firstWhere(
          (line) => line.isNotEmpty,
          orElse: () => l10n.conversationThinkingPreview,
        );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        Semantics(
          button: true,
          expanded: _open,
          hint: _open
              ? l10n.conversationHideThought
              : l10n.conversationShowThought,
          child: InkWell(
            onTap: () => setState(() => _open = !_open),
            borderRadius: const BorderRadius.all(Radii.control),
            child: ConstrainedBox(
              constraints: const BoxConstraints(minHeight: Space.touch),
              child: Row(
                children: [
                  SizedBox.square(
                    dimension: Strokes.icon,
                    child: Icon(
                      LucideIcons.brain,
                      size: Strokes.iconSmall,
                      color: c.inkFaint,
                    ),
                  ),
                  const SizedBox(width: Space.sm),
                  Text(
                    l10n.conversationThought,
                    style: t.label.copyWith(color: c.inkMuted),
                  ),
                  const SizedBox(width: Space.sm),
                  Expanded(
                    child: _open
                        ? const SizedBox.shrink()
                        : Text(
                            preview,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: t.meta.copyWith(color: c.inkMuted),
                          ),
                  ),
                  AnimatedRotation(
                    turns: _open ? 0.25 : 0,
                    duration: Motion.resolve(context, Motion.tap),
                    curve: Motion.easeOut,
                    child: Icon(
                      LucideIcons.chevronRight,
                      size: Strokes.iconSmall,
                      color: c.inkFaint,
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
        if (_open)
          Padding(
            padding: const EdgeInsets.only(
              left: Strokes.icon + Space.sm,
              bottom: Space.sm,
            ),
            child: ConversationMarkdown(data: widget.text, muted: true),
          ),
      ],
    );
  }
}

/// `task_complete` 完成信号。总结由内核去好了代码围栏，可能为空。
class CompletionMessage extends StatelessWidget {
  const CompletionMessage({
    super.key,
    required this.summary,
    required this.status,
    this.isFinal = false,
  });

  final String summary;
  final ToolStatus status;
  final bool isFinal;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final failed = status == ToolStatus.failed;
    final trimmed = summary.trim();
    final text = trimmed.isEmpty ? l10n.conversationTaskCompleted : trimmed;
    final label = failed
        ? l10n.conversationCompletionFailed
        : l10n.conversationCompleted;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        Row(
          children: [
            StatusMark(
              mark: failed ? SessionMark.failed : SessionMark.done,
              label: label,
            ),
            const SizedBox(width: Space.sm),
            ExcludeSemantics(
              child: Text(
                label,
                style: t.label.copyWith(color: failed ? c.danger : c.inkMuted),
              ),
            ),
          ],
        ),
        const SizedBox(height: Space.xs),
        ConversationMarkdown(data: text),
        if (isFinal) _AnswerFooter(copyText: text),
      ],
    );
  }
}

/// 「已中断」「重新开始」这类分割线。
class ConversationDivider extends StatelessWidget {
  const ConversationDivider({super.key, required this.label});

  final String label;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: Space.sm),
      child: Row(
        children: [
          Expanded(child: Divider(color: c.hairline)),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: Space.sm),
            child: Text(label, style: t.caption.copyWith(color: c.inkMuted)),
          ),
          Expanded(child: Divider(color: c.hairline)),
        ],
      ),
    );
  }
}

(SessionMark, String) _taskStatus(L10n l10n, String status) => switch (status) {
  'completed' => (SessionMark.done, l10n.conversationTaskCompletedStatus),
  'failed' => (SessionMark.failed, l10n.conversationTaskFailed),
  'timed_out' => (SessionMark.failed, l10n.conversationTaskTimedOut),
  'stopped' => (SessionMark.ended, l10n.conversationTaskStopped),
  'running' => (SessionMark.running, l10n.conversationTaskRunning),
  // 运行时以后加的新状态名：原样写出来，不猜它算好还是坏。
  _ => (SessionMark.idle, status),
};

/// 后台任务 / 子智能体结束的便签：一行说明（内核拼好），点开看输出的尾巴。
class TaskNoteRow extends StatefulWidget {
  const TaskNoteRow({super.key, required this.entry});

  final ConversationEntry entry;

  @override
  State<TaskNoteRow> createState() => _TaskNoteRowState();
}

class _TaskNoteRowState extends State<TaskNoteRow> {
  bool _open = false;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final note = widget.entry.taskNote;
    final tail = note?.outputTail.trim() ?? '';
    final (mark, statusLabel) = note == null
        ? (SessionMark.idle, '')
        : _taskStatus(l10n, note.status);
    // 穷举：再加一种便签，编译器会在这里拦下，不会被兜底成「后台任务」。
    final kind = switch (note?.kind) {
      null || TaskNoteKind.backgroundTask => l10n.conversationTaskBackground,
      TaskNoteKind.subagent => l10n.conversationTaskSubagent,
      TaskNoteKind.peerMessage => l10n.conversationTaskPeerMessage,
    };
    // 另一个 Pi 对话发来的消息：没有标题和状态，正文（对方写的原文）在 outputTail。
    final peer = note?.kind == TaskNoteKind.peerMessage;
    // 消息的摘要就是正文的第一行；展开后下面已经是全文，摘要让出来，同一段话不出现两遍。
    final summary = widget.entry.text.trim().isNotEmpty
        ? widget.entry.text.trim()
        : peer
        ? (_open ? '' : tail)
        : note?.title ?? '';
    final row = ConstrainedBox(
      constraints: const BoxConstraints(minHeight: Space.touch),
      child: Row(
        children: [
          // 消息没有「运行 / 完成」这回事，不画状态记号，画消息图标。
          if (peer)
            SizedBox.square(
              dimension: Strokes.icon,
              child: Icon(
                LucideIcons.messageSquareShare,
                size: Strokes.iconSmall,
                color: c.inkMuted,
              ),
            )
          else
            StatusMark(mark: mark, label: statusLabel),
          const SizedBox(width: Space.sm),
          Text(kind, style: t.label.copyWith(color: c.ink)),
          const SizedBox(width: Space.sm),
          Expanded(
            child: Text(
              summary,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: t.meta.copyWith(color: c.inkMuted),
            ),
          ),
          if (tail.isNotEmpty)
            AnimatedRotation(
              turns: _open ? 0.25 : 0,
              duration: Motion.resolve(context, Motion.tap),
              curve: Motion.easeOut,
              child: Icon(
                LucideIcons.chevronRight,
                size: Strokes.iconSmall,
                color: c.inkFaint,
              ),
            ),
        ],
      ),
    );
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        if (tail.isEmpty)
          row
        else
          Semantics(
            button: true,
            expanded: _open,
            hint: _open
                ? l10n.conversationHideOutput
                : l10n.conversationShowOutput,
            child: InkWell(
              onTap: () => setState(() => _open = !_open),
              borderRadius: const BorderRadius.all(Radii.control),
              child: row,
            ),
          ),
        if (_open)
          Padding(
            padding: const EdgeInsets.only(
              left: Strokes.icon + Space.sm,
              bottom: Space.sm,
            ),
            child: FoldedText(text: tail, exitCode: note?.exitCode),
          ),
      ],
    );
  }
}

/// 计划：勾选清单。做完的划掉，在做的用「在做」的记号，读屏读出每一步的状态。
class PlanChecklist extends StatefulWidget {
  const PlanChecklist({super.key, required this.plan});

  final List<PlanStep> plan;

  @override
  State<PlanChecklist> createState() => _PlanChecklistState();
}

class _PlanChecklistState extends State<PlanChecklist> {
  bool _open = true;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final plan = widget.plan;
    final done = plan.where((s) => s.status == PlanStepStatus.completed).length;
    return DecoratedBox(
      decoration: BoxDecoration(
        color: c.shelf,
        borderRadius: const BorderRadius.all(Radii.shelf),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          Semantics(
            button: true,
            expanded: _open,
            child: InkWell(
              onTap: () => setState(() => _open = !_open),
              borderRadius: const BorderRadius.all(Radii.shelf),
              child: ConstrainedBox(
                constraints: const BoxConstraints(minHeight: Space.touch),
                child: Padding(
                  padding: const EdgeInsets.symmetric(horizontal: Space.md),
                  child: Row(
                    children: [
                      Icon(
                        LucideIcons.listChecks,
                        size: Strokes.iconSmall,
                        color: c.inkMuted,
                      ),
                      const SizedBox(width: Space.sm),
                      Expanded(
                        child: Text(
                          l10n.conversationPlan(done, plan.length),
                          style: t.label.copyWith(color: c.ink),
                        ),
                      ),
                      AnimatedRotation(
                        turns: _open ? 0.25 : 0,
                        duration: Motion.resolve(context, Motion.tap),
                        curve: Motion.easeOut,
                        child: Icon(
                          LucideIcons.chevronRight,
                          size: Strokes.iconSmall,
                          color: c.inkFaint,
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          ),
          if (_open)
            Padding(
              padding: const EdgeInsets.fromLTRB(
                Space.md,
                0,
                Space.md,
                Space.sm,
              ),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [for (final step in plan) _PlanRow(step: step)],
              ),
            ),
        ],
      ),
    );
  }
}

class _PlanRow extends StatelessWidget {
  const _PlanRow({required this.step});

  final PlanStep step;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final (label, icon) = switch (step.status) {
      PlanStepStatus.completed => (
        l10n.conversationPlanCompleted,
        Icon(
          LucideIcons.squareCheck,
          size: Strokes.iconSmall,
          color: c.inkMuted,
        ),
      ),
      PlanStepStatus.inProgress => (
        l10n.conversationPlanInProgress,
        StatusMark(
          mark: SessionMark.running,
          label: l10n.conversationPlanInProgress,
        ),
      ),
      PlanStepStatus.pending => (
        l10n.conversationPlanPending,
        Icon(LucideIcons.square, size: Strokes.iconSmall, color: c.inkFaint),
      ),
    };
    final done = step.status == PlanStepStatus.completed;
    return Semantics(
      label: '$label: ${step.content}',
      excludeSemantics: true,
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: Space.xs),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            SizedBox.square(
              dimension: Strokes.icon,
              child: Center(child: icon),
            ),
            const SizedBox(width: Space.sm),
            Expanded(
              child: Text(
                step.content,
                style:
                    (step.status == PlanStepStatus.inProgress
                            ? t.callout.copyWith(fontWeight: FontWeight.w600)
                            : t.callout)
                        .copyWith(
                          color: done ? c.inkMuted : c.ink,
                          decoration: done ? TextDecoration.lineThrough : null,
                          decorationColor: c.inkMuted,
                        ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// 对话阶段在界面上的状态记号与说法。阶段与会话列表里的一一对应，借会话的说法。
SessionMark conversationMark(ConversationPhase phase) => switch (phase) {
  ConversationPhase.connecting ||
  ConversationPhase.thinking ||
  ConversationPhase.executingTool => SessionMark.running,
  ConversationPhase.awaitingApproval ||
  ConversationPhase.waitingForUser => SessionMark.needsYou,
  ConversationPhase.succeeded => SessionMark.done,
  ConversationPhase.failed => SessionMark.failed,
  ConversationPhase.idle => SessionMark.idle,
  ConversationPhase.dead => SessionMark.ended,
};

String conversationPhaseLabel(L10n l10n, ConversationPhase phase) =>
    phaseLabel(l10n, switch (phase) {
      ConversationPhase.connecting => SessionPhase.connecting,
      ConversationPhase.thinking => SessionPhase.thinking,
      ConversationPhase.executingTool => SessionPhase.executingTool,
      ConversationPhase.awaitingApproval => SessionPhase.awaitingApproval,
      ConversationPhase.waitingForUser => SessionPhase.waitingForUser,
      ConversationPhase.succeeded => SessionPhase.succeeded,
      ConversationPhase.failed => SessionPhase.failed,
      ConversationPhase.idle => SessionPhase.idle,
      ConversationPhase.dead => SessionPhase.dead,
    });

/// 对话尾巴上的一行状态：在做什么、做了多久；失败 / 结束时写原因。空闲和完成不画
/// （完成的耗时写在最终回答下面）。
class TurnStatusLine extends StatelessWidget {
  const TurnStatusLine({super.key, required this.view});

  final ConversationView view;

  static bool shows(ConversationPhase phase) => switch (phase) {
    ConversationPhase.idle || ConversationPhase.succeeded => false,
    _ => true,
  };

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final phase = view.phase;
    final reason = view.endReason.trim();
    final status = view.statusLine?.trim();
    final label = switch (phase) {
      ConversationPhase.failed ||
      ConversationPhase.dead when reason.isNotEmpty => reason,
      ConversationPhase.dead => l10n.conversationStatusEnded,
      _ when status != null && status.isNotEmpty => status,
      _ => conversationPhaseLabel(l10n, phase),
    };
    final started = view.turnStartedAtMs;
    final working = phase.isWorking || phase == ConversationPhase.connecting;
    final color = switch (phase) {
      ConversationPhase.failed || ConversationPhase.dead => c.danger,
      ConversationPhase.awaitingApproval ||
      ConversationPhase.waitingForUser => c.heatInk,
      _ => c.inkMuted,
    };
    return Semantics(
      liveRegion: true,
      child: ConstrainedBox(
        constraints: const BoxConstraints(minHeight: Space.touch),
        child: Row(
          children: [
            StatusMark(
              mark: conversationMark(phase),
              label: conversationPhaseLabel(l10n, phase),
            ),
            const SizedBox(width: Space.sm),
            Expanded(
              child: Text(
                label,
                maxLines: 3,
                overflow: TextOverflow.ellipsis,
                style: t.note.copyWith(color: color),
              ),
            ),
            if (working && started != null) ...[
              const SizedBox(width: Space.sm),
              ElapsedLabel(
                since: DateTime.fromMillisecondsSinceEpoch(started),
                style: t.monoTime.copyWith(color: c.ok),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
