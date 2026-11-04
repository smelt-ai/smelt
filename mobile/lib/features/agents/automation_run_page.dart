/// 一次自动化运行的记录：状态、起止时间、错误、交给它的指令、输出，以及智能体那次的对话。
///
/// 运行跑完以后它的会话就没有了，从通知、会话列表、自动化详情点进来看的都是这里。
/// 还在跑、会话还在时，可以从这里进执行现场。
library;

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
import '../../ui/status.dart';
import '../../ui/time.dart';
import '../conversation/conversation_markdown.dart';
import '../conversation/entry_views.dart';
import '../conversation/tool_views.dart';
import '../session/open_session.dart';
import 'agents_widgets.dart';
import 'automation_text.dart';

class AutomationRunPage extends StatefulWidget {
  const AutomationRunPage({
    super.key,
    required this.runtimeId,
    required this.runId,
  });

  final String runtimeId;
  final String runId;

  @override
  State<AutomationRunPage> createState() => _AutomationRunPageState();
}

class _AutomationRunPageState extends State<AutomationRunPage> {
  AutomationRunDetail? _detail;
  String? _error;

  @override
  void initState() {
    super.initState();
    unawaited(_load());
  }

  Future<void> _load() async {
    if (_error != null) setState(() => _error = null);
    try {
      final detail = await AppScope.of(
        context,
      ).backend.automationRunDetail(widget.runtimeId, widget.runId);
      if (mounted) setState(() => _detail = detail);
    } catch (error) {
      if (mounted) setState(() => _error = describeError(error));
    }
  }

  @override
  Widget build(BuildContext context) {
    final model = AppScope.of(context);
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    final detail = _detail;
    return Scaffold(
      backgroundColor: c.ground,
      appBar: AppBar(
        backgroundColor: c.ground,
        title: DetailTitle(
          title: detail?.automationName.trim().isNotEmpty == true
              ? detail!.automationName
              : l10n.autoRunTitle,
          runtime: model.fleet.runtime(widget.runtimeId),
        ),
      ),
      body: RefreshIndicator.adaptive(onRefresh: _load, child: _body(context)),
    );
  }

  Widget _body(BuildContext context) {
    final l10n = L10n.of(context);
    final detail = _detail;
    if (detail == null) {
      final error = _error;
      if (error == null) {
        return const Center(child: CircularProgressIndicator.adaptive());
      }
      return ListView(
        children: [
          EmptyNote(
            icon: LucideIcons.cloudOff,
            title: l10n.autoRunLoadFailed,
            body: error,
            action: GalleyButton.solid(
              label: l10n.commonRetry,
              onPressed: _load,
            ),
          ),
        ],
      );
    }
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final material = MaterialLocalizations.of(context);
    final run = detail.run;
    final now = DateTime.now();
    final started = run.startedAt == null
        ? null
        : fromUnixSeconds(run.startedAt!);
    final finished = run.finishedAt == null
        ? null
        : fromUnixSeconds(run.finishedAt!);
    final status = runStatusLabel(l10n, run.status);
    final error = run.error?.trim();
    // 还在跑、会话还在：可以进执行现场。跑完了会话就没了，只看记录。
    final sessionId = run.sessionId;
    final live = runIsActive(run.status) && sessionId != null
        ? AppScope.of(context).catalog.session(widget.runtimeId, sessionId)
        : null;
    final instructions = detail.instructions?.trim();
    final output = detail.output?.trim();
    return ListView(
      padding: EdgeInsets.only(
        bottom: MediaQuery.paddingOf(context).bottom + Space.xxl,
      ),
      children: [
        const SizedBox(height: Space.sm),
        Shelf(
          children: [
            ShelfRow(
              leading: StatusMark(mark: runMark(run.status), label: status),
              title: Text(status),
            ),
            FieldRow(
              label: l10n.agentsRunSource,
              value: runSourceLabel(l10n, run.source),
            ),
            if (started != null)
              FieldRow(
                label: l10n.agentsRunStartedAt,
                value: formatMoment(l10n, material, started, now),
              ),
            if (finished != null)
              FieldRow(
                label: l10n.agentsRunFinishedAt,
                value: formatMoment(l10n, material, finished, now),
              ),
            if (started != null && finished != null)
              FieldRow(
                label: l10n.agentsRunDuration,
                value: formatElapsed(finished.difference(started)),
                valueStyle: t.monoTime,
              ),
            if (error != null && error.isNotEmpty)
              Padding(
                padding: const EdgeInsets.symmetric(
                  horizontal: Space.lg,
                  vertical: Space.md,
                ),
                child: SelectableText(
                  error,
                  style: t.body.copyWith(color: c.danger),
                ),
              ),
            if (live != null)
              ShelfRow(
                leading: Icon(
                  LucideIcons.messageSquare,
                  size: Strokes.icon,
                  color: c.ink,
                ),
                title: Text(l10n.agentsOpenRun),
                titleStyle: t.body,
                trailing: Icon(
                  LucideIcons.chevronRight,
                  size: Strokes.iconSmall,
                  color: c.inkFaint,
                ),
                onTap: () => openSession(context, widget.runtimeId, live),
              ),
          ],
        ),
        if (instructions != null && instructions.isNotEmpty) ...[
          ShelfHeader(title: l10n.autoRunInstructions),
          _Text(instructions),
        ],
        if (output != null && output.isNotEmpty) ...[
          ShelfHeader(title: l10n.autoRunOutput),
          _Text(output, mono: detail.transcript.isEmpty),
        ],
        if (detail.transcript.isNotEmpty) ...[
          ShelfHeader(title: l10n.autoRunTranscript),
          for (final entry in detail.transcript) _entry(entry),
        ],
        if ((output == null || output.isEmpty) &&
            detail.transcript.isEmpty &&
            !runIsActive(run.status))
          Padding(
            padding: const EdgeInsets.fromLTRB(
              Space.page + Space.xs,
              Space.lg,
              Space.page + Space.xs,
              0,
            ),
            child: Text(
              l10n.autoRunNothing,
              style: t.note.copyWith(color: c.inkMuted),
            ),
          ),
      ],
    );
  }

  /// 对话里的一条，样子和对话页一致，只读。
  Widget _entry(ConversationEntry entry) {
    final l10n = L10n.of(context);
    final child = switch (entry.kind) {
      EntryKind.user => UserMessage(entry: entry),
      EntryKind.assistant when entry.thought => ThoughtBlock(text: entry.text),
      EntryKind.assistant => AssistantMessage(text: entry.text),
      EntryKind.completion => CompletionMessage(
        summary: entry.text,
        status: entry.toolCall?.status ?? ToolStatus.completed,
        isFinal: false,
      ),
      EntryKind.toolCall => switch (entry.toolCall) {
        final call? => ToolCallRow(call: call),
        null => null,
      },
      EntryKind.interrupted => ConversationDivider(
        label: l10n.conversationInterrupted,
      ),
      EntryKind.divider => ConversationDivider(label: entry.text),
      EntryKind.taskNote => TaskNoteRow(entry: entry),
    };
    if (child == null) return const SizedBox.shrink();
    return Padding(
      padding: const EdgeInsets.symmetric(
        horizontal: Space.page,
        vertical: Space.xs,
      ),
      child: child,
    );
  }
}

/// 一段可选中的文字。命令输出用等宽，指令和回复按 Markdown 显示。
class _Text extends StatelessWidget {
  const _Text(this.text, {this.mono = false});

  final String text;
  final bool mono;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return Shelf(
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(
            horizontal: Space.lg,
            vertical: Space.md,
          ),
          child: mono
              ? SelectableText(text, style: t.code.copyWith(color: c.ink))
              : ConversationMarkdown(data: text),
        ),
      ],
    );
  }
}

/// 推一页运行记录。
void openAutomationRun(BuildContext context, String runtimeId, String runId) =>
    unawaited(
      Navigator.of(context).push<void>(
        MaterialPageRoute(
          builder: (_) => AutomationRunPage(runtimeId: runtimeId, runId: runId),
        ),
      ),
    );
