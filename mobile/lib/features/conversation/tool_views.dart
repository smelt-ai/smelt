import 'package:flutter/material.dart';
import '../actions/ticket_bodies.dart';
import '../../ui/ticket.dart';
import '../../data/stores.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/status.dart';
import 'conversation_images.dart';
import 'conversation_logic.dart';
import 'conversation_markdown.dart';

/// 工具输出超过这么多行先收起，点「显示全部」再展开。
const _foldLines = 8;

/// diff 一次最多画这么多行，再多点「显示全部」。
const _diffLineCap = 200;

String toolKindLabel(L10n l10n, ToolKind kind) => switch (kind) {
  ToolKind.read => l10n.conversationToolRead,
  ToolKind.edit => l10n.conversationToolEdit,
  ToolKind.delete => l10n.conversationToolDelete,
  ToolKind.move => l10n.conversationToolMove,
  ToolKind.search => l10n.conversationToolSearch,
  ToolKind.execute => l10n.conversationToolExecute,
  ToolKind.think => l10n.conversationToolThink,
  ToolKind.fetch => l10n.conversationToolFetch,
  ToolKind.switchMode => l10n.conversationToolSwitchMode,
  ToolKind.collaborate => l10n.conversationToolCollaborate,
  ToolKind.review => l10n.conversationToolReview,
  ToolKind.image => l10n.conversationToolImage,
  ToolKind.compact => l10n.conversationToolCompact,
  ToolKind.wait => l10n.conversationToolWait,
  ToolKind.other => l10n.conversationToolOther,
};

SessionMark toolMark(ToolStatus status) => switch (status) {
  ToolStatus.pending => SessionMark.idle,
  ToolStatus.inProgress => SessionMark.running,
  ToolStatus.completed => SessionMark.done,
  ToolStatus.failed => SessionMark.failed,
};

String toolStatusLabel(L10n l10n, ToolStatus status) => switch (status) {
  ToolStatus.pending => l10n.conversationToolPending,
  ToolStatus.inProgress => l10n.conversationToolInProgress,
  ToolStatus.completed => l10n.conversationToolDone,
  ToolStatus.failed => l10n.conversationToolFailed,
};

/// 这一段对话属于哪台机器的哪条会话。工具调用行据此找到你在单据上做过的决定。
class ConversationIdentity extends InheritedWidget {
  const ConversationIdentity({
    super.key,
    required this.runtimeId,
    required this.sessionId,
    required super.child,
  });

  final String runtimeId;
  final String sessionId;

  static ConversationIdentity? maybeOf(BuildContext context) =>
      context.dependOnInheritedWidgetOfExactType<ConversationIdentity>();

  @override
  bool updateShouldNotify(ConversationIdentity old) =>
      old.runtimeId != runtimeId || old.sessionId != sessionId;
}

/// 一行紧凑条：状态记号 + 工具名 + 一行摘要。有输出时点开看输出 / diff。
/// 这次运行里你在单据上批过它，下面留一枚章（「已允许 · 14:02」）。
class ToolCallRow extends StatefulWidget {
  const ToolCallRow({super.key, required this.call, this.loadImage});

  final ToolCallView call;
  final ConversationImageLoader? loadImage;

  @override
  State<ToolCallRow> createState() => _ToolCallRowState();
}

class _ToolCallRowState extends State<ToolCallRow> {
  bool _open = false;

  TicketStamp? _decisionStamp(BuildContext context, String toolCallId) {
    final identity = ConversationIdentity.maybeOf(context);
    if (identity == null) return null;
    final record = AppScope.of(
      context,
    ).pass.decision(identity.runtimeId, identity.sessionId, toolCallId);
    final stamp = stampFor(L10n.of(context), record);
    if (stamp == null) return null;
    return TicketStamp(text: stamp.text, tone: stamp.tone, compact: true);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    final call = widget.call;
    final hasOutput = call.output.isNotEmpty;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        _CompactLine(
          mark: StatusMark(
            mark: toolMark(call.status),
            label: toolStatusLabel(l10n, call.status),
          ),
          name: toolKindLabel(l10n, call.kind),
          summary: call.title,
          open: hasOutput ? _open : null,
          openLabel: _open
              ? l10n.conversationHideOutput
              : l10n.conversationShowOutput,
          onTap: hasOutput ? () => setState(() => _open = !_open) : null,
        ),
        if (_decisionStamp(context, call.id) case final stamp?)
          Padding(
            padding: const EdgeInsets.only(
              left: Strokes.icon + Space.sm,
              bottom: Space.sm,
            ),
            child: Align(alignment: Alignment.centerLeft, child: stamp),
          ),
        if (_open)
          Padding(
            padding: const EdgeInsets.only(
              left: Strokes.icon + Space.sm,
              bottom: Space.sm,
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                for (final (i, part) in call.output.indexed) ...[
                  if (i > 0) const SizedBox(height: Space.sm),
                  ToolOutputView(part: part, loadImage: widget.loadImage),
                ],
              ],
            ),
          ),
      ],
    );
  }
}

/// 连续几次工具调用收成的一组。收着时只写一行：合起来的状态、几次、最后一次在做什么。
class ToolGroup extends StatefulWidget {
  const ToolGroup({super.key, required this.calls, this.loadImage});

  final List<ToolCallView> calls;
  final ConversationImageLoader? loadImage;

  @override
  State<ToolGroup> createState() => _ToolGroupState();
}

class _ToolGroupState extends State<ToolGroup> {
  bool _open = false;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final calls = widget.calls;
    final status = groupStatus(calls);
    final failed = calls.where((c) => c.status == ToolStatus.failed).length;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        _CompactLine(
          mark: StatusMark(
            mark: toolMark(status),
            label: toolStatusLabel(l10n, status),
          ),
          name: l10n.conversationToolCalls(calls.length),
          summary: calls.last.title,
          extra: failed == 0
              ? null
              : Text(
                  l10n.conversationToolFailedCount(failed),
                  style: t.meta.copyWith(color: c.danger),
                ),
          open: _open,
          openLabel: _open
              ? l10n.conversationHideOutput
              : l10n.conversationShowOutput,
          onTap: () => setState(() => _open = !_open),
        ),
        if (_open)
          Padding(
            padding: const EdgeInsets.only(left: Space.md),
            child: DecoratedBox(
              decoration: BoxDecoration(
                border: Border(
                  left: BorderSide(color: c.hairline, width: Strokes.hairline),
                ),
              ),
              child: Padding(
                padding: const EdgeInsets.only(left: Space.sm),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    for (final call in calls)
                      ToolCallRow(
                        key: ValueKey(call.id),
                        call: call,
                        loadImage: widget.loadImage,
                      ),
                  ],
                ),
              ),
            ),
          ),
      ],
    );
  }
}

/// 紧凑条本体：最小高度满足触控下限，长摘要一行截断。
class _CompactLine extends StatelessWidget {
  const _CompactLine({
    required this.mark,
    required this.name,
    required this.summary,
    required this.open,
    required this.openLabel,
    required this.onTap,
    this.extra,
  });

  final Widget mark;
  final String name;
  final String summary;

  /// null：没有可展开的内容，不画箭头、不可点。
  final bool? open;
  final String openLabel;
  final VoidCallback? onTap;
  final Widget? extra;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final row = ConstrainedBox(
      constraints: const BoxConstraints(minHeight: Space.touch),
      child: Row(
        children: [
          mark,
          const SizedBox(width: Space.sm),
          Text(name, maxLines: 1, style: t.label.copyWith(color: c.ink)),
          const SizedBox(width: Space.sm),
          Expanded(
            child: Text(
              summary,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: t.meta.copyWith(color: c.inkMuted),
            ),
          ),
          if (extra != null) ...[const SizedBox(width: Space.sm), extra!],
          if (open != null) ...[
            const SizedBox(width: Space.xs),
            AnimatedRotation(
              turns: open! ? 0.25 : 0,
              duration: Motion.resolve(context, Motion.tap),
              curve: Motion.easeOut,
              child: Icon(
                LucideIcons.chevronRight,
                size: Strokes.iconSmall,
                color: c.inkFaint,
              ),
            ),
          ],
        ],
      ),
    );
    if (onTap == null) return Semantics(container: true, child: row);
    return Semantics(
      button: true,
      expanded: open,
      hint: openLabel,
      child: InkWell(
        onTap: onTap,
        borderRadius: const BorderRadius.all(Radii.control),
        child: row,
      ),
    );
  }
}

/// 工具的一段输出。
class ToolOutputView extends StatelessWidget {
  const ToolOutputView({super.key, required this.part, this.loadImage});

  final ToolOutput part;
  final ConversationImageLoader? loadImage;

  @override
  Widget build(BuildContext context) {
    final diff = part.diff;
    final image = part.image;
    return switch (part.kind) {
      ToolOutputKind.diff when diff != null => DiffBlock(diff: diff),
      ToolOutputKind.image when image != null => Align(
        alignment: Alignment.centerLeft,
        child: ConversationThumbnail(image: image, loadImage: loadImage),
      ),
      ToolOutputKind.terminal => FoldedText(
        text: part.text,
        exitCode: part.exitCode,
        truncated: part.truncated,
      ),
      _ => FoldedText(text: part.text),
    };
  }
}

/// 等宽文字输出：超过几行先收起。终端输出末尾补退出码和截断提示。
class FoldedText extends StatefulWidget {
  const FoldedText({
    super.key,
    required this.text,
    this.exitCode,
    this.truncated = false,
  });

  final String text;
  final int? exitCode;
  final bool truncated;

  @override
  State<FoldedText> createState() => _FoldedTextState();
}

class _FoldedTextState extends State<FoldedText> {
  bool _all = false;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final text = stripCodeFence(widget.text).trimRight();
    final lines = text.split('\n');
    final long = lines.length > _foldLines;
    final shown = _all || !long ? text : lines.take(_foldLines).join('\n');
    final code = widget.exitCode;
    final notes = [
      if (code != null) l10n.conversationExitCode(code),
      if (widget.truncated) l10n.conversationOutputTruncated,
    ];
    final failed = code != null && code != 0;
    return MonoBlock(
      footer: !long && notes.isEmpty
          ? null
          : Padding(
              padding: const EdgeInsets.fromLTRB(
                Space.md,
                0,
                Space.xs,
                Space.xs,
              ),
              child: Row(
                children: [
                  if (notes.isNotEmpty)
                    Expanded(
                      child: Text(
                        notes.join(' · '),
                        style: t.caption.copyWith(
                          color: failed ? c.danger : c.inkMuted,
                        ),
                      ),
                    )
                  else
                    const Spacer(),
                  if (long)
                    TextButton(
                      onPressed: () => setState(() => _all = !_all),
                      child: Text(
                        _all
                            ? l10n.conversationCollapse
                            : l10n.conversationShowAllLines(lines.length),
                      ),
                    ),
                ],
              ),
            ),
      child: SelectableText(shown, style: t.code.copyWith(color: c.ink)),
    );
  }
}

/// 改文件的 diff：路径 + 增删行数，下面按行对齐，未改动的长段收成一行。
class DiffBlock extends StatefulWidget {
  const DiffBlock({super.key, required this.diff});

  final DiffView diff;

  @override
  State<DiffBlock> createState() => _DiffBlockState();
}

class _DiffBlockState extends State<DiffBlock> {
  late List<DiffHunkLine> _hunks;
  late int _added;
  late int _removed;
  bool _all = false;

  @override
  void initState() {
    super.initState();
    _compute();
  }

  @override
  void didUpdateWidget(DiffBlock old) {
    super.didUpdateWidget(old);
    if (old.diff != widget.diff) _compute();
  }

  /// diff 只在内容变了时算一次，不放在 build 里：流式快照每秒能来好几份。
  void _compute() {
    final lines = lineDiff(widget.diff.oldText, widget.diff.newText);
    final (added, removed) = diffTotals(lines);
    _added = added;
    _removed = removed;
    _hunks = foldUnchanged(lines);
  }

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final shown = _all || _hunks.length <= _diffLineCap
        ? _hunks
        : _hunks.take(_diffLineCap).toList();
    final code = t.code.copyWith(color: c.ink);

    Widget line(DiffHunkLine h) => switch (h) {
      DiffSkipped(:final count) => Padding(
        padding: const EdgeInsets.symmetric(
          horizontal: Space.md,
          vertical: Space.xxs,
        ),
        child: Text(
          l10n.conversationUnchangedLines(count),
          style: t.caption.copyWith(color: c.inkFaint),
        ),
      ),
      DiffShown(:final line) => ColoredBox(
        color: switch (line.op) {
          DiffOp.insert => c.okWash,
          DiffOp.delete => c.dangerWash,
          DiffOp.equal => Colors.transparent,
        },
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: Space.md),
          child: Text.rich(
            TextSpan(
              children: [
                TextSpan(
                  text: switch (line.op) {
                    DiffOp.insert => '+ ',
                    DiffOp.delete => '- ',
                    DiffOp.equal => '  ',
                  },
                  style: code.copyWith(
                    color: switch (line.op) {
                      DiffOp.insert => c.ok,
                      DiffOp.delete => c.danger,
                      DiffOp.equal => c.inkFaint,
                    },
                  ),
                ),
                TextSpan(
                  text: line.text,
                  style: line.op == DiffOp.equal
                      ? code.copyWith(color: c.inkMuted)
                      : code,
                ),
              ],
            ),
            softWrap: false,
          ),
        ),
      ),
    };

    return DecoratedBox(
      decoration: BoxDecoration(
        color: c.shelf,
        border: Border.all(color: c.hairline, width: Strokes.hairline),
        borderRadius: const BorderRadius.all(Radii.control),
      ),
      child: ClipRRect(
        borderRadius: const BorderRadius.all(Radii.control),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisSize: MainAxisSize.min,
          children: [
            Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: Space.md,
                vertical: Space.sm,
              ),
              child: Row(
                children: [
                  Expanded(
                    child: Text(
                      widget.diff.path,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: t.monoHead.copyWith(color: c.ink),
                    ),
                  ),
                  if (widget.diff.oldText == null) ...[
                    Text(
                      l10n.conversationNewFile,
                      style: t.caption.copyWith(color: c.inkMuted),
                    ),
                    const SizedBox(width: Space.sm),
                  ],
                  Text('+$_added', style: t.monoHead.copyWith(color: c.ok)),
                  const SizedBox(width: Space.sm),
                  Text(
                    '-$_removed',
                    style: t.monoHead.copyWith(color: c.danger),
                  ),
                ],
              ),
            ),
            Divider(height: Strokes.hairline, color: c.hairline),
            LayoutBuilder(
              builder: (context, constraints) => SingleChildScrollView(
                scrollDirection: Axis.horizontal,
                padding: const EdgeInsets.symmetric(vertical: Space.sm),
                child: ConstrainedBox(
                  constraints: BoxConstraints(minWidth: constraints.maxWidth),
                  child: IntrinsicWidth(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [for (final h in shown) line(h)],
                    ),
                  ),
                ),
              ),
            ),
            if (_hunks.length > _diffLineCap)
              Align(
                alignment: Alignment.centerRight,
                child: TextButton(
                  onPressed: () => setState(() => _all = !_all),
                  child: Text(
                    _all
                        ? l10n.conversationCollapse
                        : l10n.conversationShowAllLines(_hunks.length),
                  ),
                ),
              ),
          ],
        ),
      ),
    );
  }
}
