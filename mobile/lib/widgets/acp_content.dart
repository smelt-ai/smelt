import 'dart:convert';

import 'package:diff_match_patch/diff_match_patch.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:url_launcher/url_launcher.dart';

import '../models/acp_snapshot.dart';

class AcpMarkdown extends StatelessWidget {
  const AcpMarkdown({super.key, required this.data, this.muted = false});

  final String data;
  final bool muted;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final textColor = muted ? colors.onSurfaceVariant : colors.onSurface;
    return MarkdownBody(
      data: data,
      selectable: true,
      onTapLink: (_, href, _) {
        final uri = href == null ? null : Uri.tryParse(href);
        if (uri != null) launchUrl(uri, mode: LaunchMode.externalApplication);
      },
      styleSheet: MarkdownStyleSheet.fromTheme(Theme.of(context)).copyWith(
        p: TextStyle(color: textColor, height: 1.45),
        code: TextStyle(
          color: textColor,
          backgroundColor: colors.surfaceContainerHighest,
          fontFamily: 'monospace',
          fontSize: 13,
        ),
        codeblockDecoration: BoxDecoration(
          color: colors.surfaceContainerHighest,
          borderRadius: BorderRadius.circular(6),
        ),
        blockquoteDecoration: BoxDecoration(
          color: colors.surfaceContainer,
          border: Border(left: BorderSide(color: colors.primary, width: 3)),
        ),
      ),
    );
  }
}

class AcpImageThumbnail extends StatefulWidget {
  const AcpImageThumbnail({super.key, required this.image});

  final AcpImageData image;

  @override
  State<AcpImageThumbnail> createState() => _AcpImageThumbnailState();
}

class _AcpImageThumbnailState extends State<AcpImageThumbnail> {
  Uint8List? _bytes;
  MemoryImage? _provider;

  @override
  void initState() {
    super.initState();
    _updateImage();
  }

  @override
  void didUpdateWidget(covariant AcpImageThumbnail oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.image.base64 != widget.image.base64) _updateImage();
  }

  void _updateImage() {
    _bytes = _decodeImage(widget.image.base64);
    _provider = _bytes == null ? null : MemoryImage(_bytes!);
  }

  @override
  Widget build(BuildContext context) {
    final provider = _provider;
    if (provider == null) {
      return const SizedBox(
        width: 88,
        height: 64,
        child: Center(child: Icon(Icons.broken_image_outlined)),
      );
    }
    return InkWell(
      onTap: () => showDialog<void>(
        context: context,
        builder: (context) => Dialog.fullscreen(
          child: Stack(
            children: [
              Positioned.fill(
                child: InteractiveViewer(
                  minScale: 0.5,
                  maxScale: 5,
                  child: Center(
                    child: Image(image: provider, gaplessPlayback: true),
                  ),
                ),
              ),
              Positioned(
                top: 12,
                right: 12,
                child: IconButton.filledTonal(
                  tooltip: 'Close image',
                  onPressed: () => Navigator.pop(context),
                  icon: const Icon(Icons.close),
                ),
              ),
            ],
          ),
        ),
      ),
      borderRadius: BorderRadius.circular(6),
      child: ClipRRect(
        borderRadius: BorderRadius.circular(6),
        child: Image(
          image: provider,
          width: 112,
          height: 88,
          fit: BoxFit.cover,
          gaplessPlayback: true,
          errorBuilder: (_, _, _) => const SizedBox(
            width: 112,
            height: 88,
            child: Center(child: Icon(Icons.broken_image_outlined)),
          ),
        ),
      ),
    );
  }
}

class AcpAssistantMessage extends StatefulWidget {
  const AcpAssistantMessage({
    super.key,
    required this.text,
    required this.thought,
    this.isFinal = false,
    this.durationMs,
  });

  final String text;
  final bool thought;
  final bool isFinal;
  final int? durationMs;

  @override
  State<AcpAssistantMessage> createState() => _AcpAssistantMessageState();
}

class _AcpAssistantMessageState extends State<AcpAssistantMessage> {
  bool _expanded = false;

  @override
  Widget build(BuildContext context) {
    if (widget.text.isEmpty) return const SizedBox.shrink();
    final colors = Theme.of(context).colorScheme;
    if (widget.thought) {
      final preview = widget.text
          .split('\n')
          .firstWhere(
            (line) => line.trim().isNotEmpty,
            orElse: () => 'Thinking...',
          )
          .trim();
      return Padding(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 3),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            InkWell(
              onTap: () => setState(() => _expanded = !_expanded),
              borderRadius: BorderRadius.circular(6),
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
                child: Row(
                  children: [
                    Icon(
                      _expanded ? Icons.expand_more : Icons.chevron_right,
                      size: 18,
                      color: colors.onSurfaceVariant,
                    ),
                    const SizedBox(width: 4),
                    Text(
                      'Thought',
                      style: TextStyle(
                        color: colors.onSurfaceVariant,
                        fontWeight: FontWeight.w600,
                        fontSize: 12,
                      ),
                    ),
                    if (!_expanded) ...[
                      const SizedBox(width: 8),
                      Expanded(
                        child: Text(
                          preview,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(
                            color: colors.onSurfaceVariant,
                            fontSize: 12,
                          ),
                        ),
                      ),
                    ],
                  ],
                ),
              ),
            ),
            if (_expanded)
              Padding(
                padding: const EdgeInsets.fromLTRB(30, 4, 8, 8),
                child: AcpMarkdown(data: widget.text, muted: true),
              ),
          ],
        ),
      );
    }

    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 8, 12, 4),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (widget.durationMs case final duration?)
            Padding(
              padding: const EdgeInsets.only(bottom: 4),
              child: Text(
                'Completed in ${_formatDuration(duration)}',
                style: TextStyle(color: colors.onSurfaceVariant, fontSize: 11),
              ),
            ),
          AcpMarkdown(data: widget.text, muted: !widget.isFinal),
          if (widget.isFinal)
            Align(
              alignment: Alignment.centerLeft,
              child: IconButton(
                visualDensity: VisualDensity.compact,
                tooltip: 'Copy answer',
                onPressed: () =>
                    Clipboard.setData(ClipboardData(text: widget.text)),
                icon: const Icon(Icons.copy, size: 17),
              ),
            ),
        ],
      ),
    );
  }
}

class AcpToolCallCard extends StatelessWidget {
  const AcpToolCallCard({
    super.key,
    required this.title,
    required this.kind,
    required this.status,
    required this.output,
  });

  final String title;
  final ToolKind kind;
  final ToolCallStatus status;
  final List<ToolOutputPart> output;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    final (icon, label) = _toolPresentation(kind);
    return Card(
      margin: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
      clipBehavior: Clip.antiAlias,
      child: ExpansionTile(
        leading: Icon(icon, size: 19, color: colors.primary),
        title: Row(
          children: [
            Text(
              label,
              style: TextStyle(
                color: colors.primary,
                fontSize: 12,
                fontWeight: FontWeight.w700,
              ),
            ),
            const SizedBox(width: 8),
            Expanded(
              child: Text(
                title,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
              ),
            ),
          ],
        ),
        trailing: _ToolStatusIcon(status: status),
        children: output
            .map(
              (part) => Padding(
                padding: const EdgeInsets.fromLTRB(12, 0, 12, 12),
                child: _ToolOutputView(part: part),
              ),
            )
            .toList(),
      ),
    );
  }
}

class _ToolStatusIcon extends StatelessWidget {
  const _ToolStatusIcon({required this.status});
  final ToolCallStatus status;

  @override
  Widget build(BuildContext context) => switch (status) {
    ToolCallStatus.pending => const Icon(Icons.schedule, size: 17),
    ToolCallStatus.inProgress => const SizedBox(
      width: 16,
      height: 16,
      child: CircularProgressIndicator(strokeWidth: 2),
    ),
    ToolCallStatus.completed => const Icon(
      Icons.check_circle,
      color: Colors.green,
      size: 17,
    ),
    ToolCallStatus.failed => const Icon(
      Icons.error,
      color: Colors.red,
      size: 17,
    ),
  };
}

class _ToolOutputView extends StatefulWidget {
  const _ToolOutputView({required this.part});
  final ToolOutputPart part;

  @override
  State<_ToolOutputView> createState() => _ToolOutputViewState();
}

class _ToolOutputViewState extends State<_ToolOutputView> {
  bool _expanded = false;

  @override
  Widget build(BuildContext context) {
    return switch (widget.part) {
      ToolOutputText(text: final text) => _buildText(context, text),
      ToolOutputDiff(
        path: final path,
        oldText: final oldText,
        newText: final newText,
      ) =>
        _buildDiff(context, path, oldText ?? '', newText),
      ToolOutputImage(base64: final data, mimeType: final mime) =>
        AcpImageThumbnail(
          image: AcpImageData(mimeType: mime, base64: data),
        ),
    };
  }

  Widget _buildText(BuildContext context, String raw) {
    final text = _stripCodeFence(raw);
    final lines = text.split('\n');
    final shown = _expanded || lines.length <= 8
        ? text
        : lines.take(8).join('\n');
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        SelectableText(
          shown,
          style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
        ),
        if (lines.length > 8)
          Align(
            alignment: Alignment.centerLeft,
            child: TextButton.icon(
              onPressed: () => setState(() => _expanded = !_expanded),
              icon: Icon(_expanded ? Icons.expand_less : Icons.expand_more),
              label: Text(
                _expanded ? 'Collapse' : 'Show all ${lines.length} lines',
              ),
            ),
          ),
      ],
    );
  }

  Widget _buildDiff(
    BuildContext context,
    String path,
    String oldText,
    String newText,
  ) {
    final parts = diff(oldText, newText, timeout: 0.2);
    cleanupSemantic(parts);
    final inserted = parts
        .where((part) => part.operation == DIFF_INSERT)
        .fold<int>(0, (sum, part) => sum + _lineCount(part.text));
    final deleted = parts
        .where((part) => part.operation == DIFF_DELETE)
        .fold<int>(0, (sum, part) => sum + _lineCount(part.text));
    final colors = Theme.of(context).colorScheme;
    return DecoratedBox(
      decoration: BoxDecoration(
        border: Border.all(color: colors.outlineVariant),
        borderRadius: BorderRadius.circular(6),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 7),
            child: Row(
              children: [
                Expanded(
                  child: Text(
                    path,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(
                      fontFamily: 'monospace',
                      fontSize: 12,
                    ),
                  ),
                ),
                Text('+$inserted', style: const TextStyle(color: Colors.green)),
                const SizedBox(width: 8),
                Text('-$deleted', style: const TextStyle(color: Colors.red)),
              ],
            ),
          ),
          const Divider(height: 1),
          SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            padding: const EdgeInsets.all(10),
            child: SelectableText.rich(
              TextSpan(
                children: parts.map((part) {
                  final (foreground, background) = switch (part.operation) {
                    DIFF_INSERT => (
                      Colors.green.shade200,
                      Colors.green.withAlpha(35),
                    ),
                    DIFF_DELETE => (
                      Colors.red.shade200,
                      Colors.red.withAlpha(35),
                    ),
                    _ => (colors.onSurfaceVariant, Colors.transparent),
                  };
                  return TextSpan(
                    text: part.text,
                    style: TextStyle(
                      color: foreground,
                      backgroundColor: background,
                    ),
                  );
                }).toList(),
                style: const TextStyle(fontFamily: 'monospace', fontSize: 12),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

(IconData, String) _toolPresentation(ToolKind kind) => switch (kind) {
  ToolKind.read => (Icons.visibility_outlined, 'Read'),
  ToolKind.edit => (Icons.edit_outlined, 'Edit'),
  ToolKind.delete => (Icons.delete_outline, 'Delete'),
  ToolKind.move => (Icons.drive_file_move_outline, 'Move'),
  ToolKind.search => (Icons.search, 'Search'),
  ToolKind.execute => (Icons.terminal, 'Execute'),
  ToolKind.think => (Icons.psychology_outlined, 'Think'),
  ToolKind.fetch => (Icons.cloud_download_outlined, 'Fetch'),
  ToolKind.switchMode => (Icons.swap_horiz, 'Switch mode'),
  ToolKind.collaborate => (Icons.groups_outlined, 'Collaborate'),
  ToolKind.review => (Icons.rate_review_outlined, 'Review'),
  ToolKind.image => (Icons.image_outlined, 'Image'),
  ToolKind.compact => (Icons.compress, 'Compact'),
  ToolKind.wait => (Icons.hourglass_empty, 'Wait'),
  ToolKind.other => (Icons.build_outlined, 'Tool'),
};

String _stripCodeFence(String text) {
  final trimmed = text.trim();
  if (!trimmed.startsWith('```') || !trimmed.endsWith('```')) return text;
  final newline = trimmed.indexOf('\n');
  if (newline < 0) return text;
  return trimmed.substring(newline + 1, trimmed.length - 3).trimRight();
}

int _lineCount(String text) {
  if (text.isEmpty) return 0;
  return '\n'.allMatches(text).length + (text.endsWith('\n') ? 0 : 1);
}

String _formatDuration(int milliseconds) {
  if (milliseconds < 1000) return '${milliseconds}ms';
  final seconds = milliseconds / 1000;
  if (seconds < 60) return '${seconds.toStringAsFixed(seconds < 10 ? 1 : 0)}s';
  final minutes = seconds ~/ 60;
  final remaining = (seconds % 60).round();
  return '${minutes}m ${remaining}s';
}

Uint8List? _decodeImage(String data) {
  try {
    return base64Decode(data);
  } on FormatException {
    return null;
  }
}
