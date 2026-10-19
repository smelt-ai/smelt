import 'package:diff_match_patch/diff_match_patch.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:url_launcher/url_launcher.dart';

import '../models/runtime_models.dart';
import '../theme/smelt_theme.dart';

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

/// 按引用取大图：给 `maxEdge` 出缩略图（长边不超过它），不给出原图。
typedef AcpImageLoader =
    Future<ImageData> Function(ImageRef image, {int? maxEdge});

/// 列表里缩略图的长边上限。格子是 112×88，按 3 倍屏取够用的像素。
const thumbnailMaxEdge = 480;

/// 已取回的缩略图。对话列表滚动时缩略图格子会反复重建，不缓存就会反复去运行时取。
final _thumbnailCache = <String, Uint8List>{};
const _thumbnailCacheLimit = 48;

String _thumbnailKey(ImageRef image) => '${image.entry}:${image.digest}';

class AcpImageThumbnail extends StatefulWidget {
  const AcpImageThumbnail({super.key, required this.image, this.loadImage});

  final ConversationImage image;

  /// 图片只给了引用时，用它取缩略图和点开后的原图；没给就只能放大手上的字节。
  final AcpImageLoader? loadImage;

  @override
  State<AcpImageThumbnail> createState() => _AcpImageThumbnailState();
}

class _AcpImageThumbnailState extends State<AcpImageThumbnail> {
  MemoryImage? _provider;
  bool _loadingThumbnail = false;

  @override
  void initState() {
    super.initState();
    _updateImage();
  }

  @override
  void didUpdateWidget(covariant AcpImageThumbnail oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (!_sameImage(oldWidget.image, widget.image)) _updateImage();
  }

  /// 每份快照里的图片都是新对象；内容没变就沿用原来的 provider，否则图片会闪。
  static bool _sameImage(ConversationImage a, ConversationImage b) =>
      a.mime == b.mime &&
      a.original == b.original &&
      (identical(a.bytes, b.bytes) || listEquals(a.bytes, b.bytes));

  void _updateImage() {
    final bytes = widget.image.bytes;
    if (bytes != null && bytes.isNotEmpty) {
      _provider = MemoryImage(bytes);
      return;
    }
    final original = widget.image.original;
    final cached = original == null
        ? null
        : _thumbnailCache[_thumbnailKey(original)];
    _provider = cached == null ? null : MemoryImage(cached);
    if (cached == null && original != null && widget.loadImage != null) {
      _loadThumbnail(original);
    }
  }

  Future<void> _loadThumbnail(ImageRef original) async {
    final load = widget.loadImage;
    if (load == null || _loadingThumbnail) return;
    _loadingThumbnail = true;
    try {
      final image = await load(original, maxEdge: thumbnailMaxEdge);
      final key = _thumbnailKey(original);
      _thumbnailCache.remove(key);
      _thumbnailCache[key] = image.bytes;
      while (_thumbnailCache.length > _thumbnailCacheLimit) {
        _thumbnailCache.remove(_thumbnailCache.keys.first);
      }
      if (!mounted || widget.image.original != original) return;
      setState(() => _provider = MemoryImage(image.bytes));
    } catch (_) {
      // 缩略图取不到就留占位，点开时还会再取原图。
    } finally {
      _loadingThumbnail = false;
    }
  }

  AcpImageLoader? get _originalLoader {
    if (widget.image.original == null) return null;
    return widget.loadImage;
  }

  @override
  Widget build(BuildContext context) {
    final provider = _provider;
    final loadOriginal = _originalLoader;
    if (provider == null && loadOriginal == null) {
      return const SizedBox(
        width: 88,
        height: 64,
        child: Center(child: Icon(Icons.broken_image_outlined)),
      );
    }
    const placeholder = SizedBox(
      width: 112,
      height: 88,
      child: Center(child: Icon(Icons.image_outlined)),
    );
    return InkWell(
      onTap: () => showDialog<void>(
        context: context,
        builder: (context) => _AcpImageViewer(
          preview: provider,
          original: widget.image.original,
          loadOriginal: loadOriginal,
        ),
      ),
      borderRadius: BorderRadius.circular(6),
      child: ClipRRect(
        borderRadius: BorderRadius.circular(6),
        child: provider == null
            ? placeholder
            : Image(
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

/// 全屏看图。有原图引用时先放大缩略图，原图到了再换上。
class _AcpImageViewer extends StatefulWidget {
  const _AcpImageViewer({
    required this.preview,
    required this.original,
    required this.loadOriginal,
  });

  final ImageProvider? preview;
  final ImageRef? original;
  final AcpImageLoader? loadOriginal;

  @override
  State<_AcpImageViewer> createState() => _AcpImageViewerState();
}

class _AcpImageViewerState extends State<_AcpImageViewer> {
  ImageProvider? _full;
  bool _loading = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final load = widget.loadOriginal;
    final original = widget.original;
    if (load == null || original == null) return;
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final image = await load(original);
      if (!mounted) return;
      setState(() {
        _loading = false;
        if (image.bytes.isEmpty) {
          _error = '图片无法解码';
        } else {
          _full = MemoryImage(image.bytes);
        }
      });
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _loading = false;
        _error = '原图加载失败：${describeRuntimeError(error)}';
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final shown = _full ?? widget.preview;
    return Dialog.fullscreen(
      child: Stack(
        children: [
          Positioned.fill(
            child: shown == null
                ? const Center(child: Icon(Icons.image_outlined, size: 48))
                : InteractiveViewer(
                    minScale: 0.5,
                    maxScale: 5,
                    child: Center(
                      child: Image(image: shown, gaplessPlayback: true),
                    ),
                  ),
          ),
          if (_loading)
            const Positioned.fill(
              child: IgnorePointer(
                child: Center(child: CircularProgressIndicator()),
              ),
            ),
          if (_error case final error?)
            Positioned(
              left: 16,
              right: 16,
              bottom: 32,
              child: Material(
                color: Theme.of(context).colorScheme.errorContainer,
                borderRadius: BorderRadius.circular(8),
                child: Padding(
                  padding: const EdgeInsets.fromLTRB(16, 8, 8, 8),
                  child: Row(
                    children: [
                      Expanded(child: Text(error)),
                      TextButton(onPressed: _load, child: const Text('重试')),
                    ],
                  ),
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

/// `task_complete` 完成信号。总结由 Rust 去好了代码围栏，可能为空。
class AcpCompletionMessage extends StatelessWidget {
  const AcpCompletionMessage({
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
    final colors = Theme.of(context).colorScheme;
    final failed = status == ToolStatus.failed;
    final trimmed = summary.trim();
    final text = trimmed.isEmpty ? 'Task completed.' : trimmed;
    final statusColor = failed ? colors.error : colors.primary;

    return Padding(
      padding: const EdgeInsets.fromLTRB(12, 8, 12, 4),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(
                failed ? Icons.error_outline : Icons.check_circle_outline,
                size: 17,
                color: statusColor,
              ),
              const SizedBox(width: 6),
              Text(
                failed ? 'Completion failed' : 'Completed',
                style: TextStyle(
                  color: statusColor,
                  fontWeight: FontWeight.w600,
                  fontSize: 12,
                ),
              ),
            ],
          ),
          AcpMarkdown(data: text, muted: !isFinal),
          if (isFinal)
            Align(
              alignment: Alignment.centerLeft,
              child: IconButton(
                visualDensity: VisualDensity.compact,
                tooltip: 'Copy completion summary',
                onPressed: () => Clipboard.setData(ClipboardData(text: text)),
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
    this.loadImage,
  });

  final String title;
  final ToolKind kind;
  final ToolStatus status;
  final List<ToolOutput> output;
  final AcpImageLoader? loadImage;

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
                child: _ToolOutputView(part: part, loadImage: loadImage),
              ),
            )
            .toList(),
      ),
    );
  }
}

class _ToolStatusIcon extends StatelessWidget {
  const _ToolStatusIcon({required this.status});
  final ToolStatus status;

  @override
  Widget build(BuildContext context) => switch (status) {
    ToolStatus.pending => const Icon(Icons.schedule, size: 17),
    ToolStatus.inProgress => const SizedBox(
      width: 16,
      height: 16,
      child: CircularProgressIndicator(strokeWidth: 2),
    ),
    ToolStatus.completed => Icon(
      Icons.check_circle,
      color: context.smeltColors.done,
      size: 17,
    ),
    ToolStatus.failed => Icon(
      Icons.error,
      color: context.smeltColors.danger,
      size: 17,
    ),
  };
}

class _ToolOutputView extends StatefulWidget {
  const _ToolOutputView({required this.part, this.loadImage});
  final ToolOutput part;
  final AcpImageLoader? loadImage;

  @override
  State<_ToolOutputView> createState() => _ToolOutputViewState();
}

class _ToolOutputViewState extends State<_ToolOutputView> {
  bool _expanded = false;

  @override
  Widget build(BuildContext context) {
    final part = widget.part;
    final diff = part.diff;
    final image = part.image;
    return switch (part.kind) {
      ToolOutputKind.diff when diff != null => _buildDiff(
        context,
        diff.path,
        diff.oldText ?? '',
        diff.newText,
      ),
      ToolOutputKind.image when image != null => AcpImageThumbnail(
        image: image,
        loadImage: widget.loadImage,
      ),
      ToolOutputKind.terminal => _buildTerminal(context, part),
      _ => _buildText(context, part.text),
    };
  }

  /// 嵌入终端的输出：正文同文本输出，末尾补退出码和截断提示。
  Widget _buildTerminal(BuildContext context, ToolOutput part) {
    final notes = [
      if (part.exitCode case final code?) 'Exit code $code',
      if (part.truncated) 'Output truncated',
    ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _buildText(context, part.text),
        if (notes.isNotEmpty)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(
              notes.join(' · '),
              style: TextStyle(
                color: Theme.of(context).colorScheme.onSurfaceVariant,
                fontSize: 11,
              ),
            ),
          ),
      ],
    );
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
    final smelt = context.smeltColors;
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
                Text(
                  '+$inserted',
                  style: TextStyle(color: context.smeltColors.done),
                ),
                const SizedBox(width: 8),
                Text(
                  '-$deleted',
                  style: TextStyle(color: context.smeltColors.danger),
                ),
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
                    DIFF_INSERT => (smelt.diffAdd, smelt.done.withAlpha(35)),
                    DIFF_DELETE => (
                      smelt.diffDelete,
                      smelt.danger.withAlpha(35),
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
