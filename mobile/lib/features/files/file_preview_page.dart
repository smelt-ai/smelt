/// 一个文件的预览：代码 / 文本带行号和高亮，Markdown 可在渲染和源码间切换，图片直接看，
/// 二进制和过大的文件只给说明。第一版只读。
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../data/stores.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/buttons.dart';
import '../../ui/page.dart';
import '../conversation/conversation_markdown.dart';
import 'code_view.dart';
import 'file_links.dart';
import 'project_files_page.dart';

/// 推一页预览。对话里的链接、diff 文件头、文件页都从这里进。
///
/// `routeName` 让预览算进某个浏览流程（文件浏览里打开的，「关闭」时一起退掉）。
void openFilePreview(
  BuildContext context, {
  required String runtimeId,
  required String path,
  int? line,
  String? routeName,
}) => unawaited(
  Navigator.of(context).push<void>(
    MaterialPageRoute(
      settings: RouteSettings(name: routeName),
      builder: (_) =>
          FilePreviewPage(runtimeId: runtimeId, path: path, line: line),
    ),
  ),
);

class FilePreviewPage extends StatefulWidget {
  const FilePreviewPage({
    super.key,
    required this.runtimeId,
    required this.path,
    this.line,
  });

  final String runtimeId;

  /// 那台机器上的绝对路径。
  final String path;

  /// 打开时跳到这一行（1 基）。
  final int? line;

  @override
  State<FilePreviewPage> createState() => _FilePreviewPageState();
}

class _FilePreviewPageState extends State<FilePreviewPage> {
  OpenedFile? _file;
  String? _error;

  /// 代码默认不换行（缩进对得齐）；纯文字类文件默认换行。
  late bool _wrap = _prose(widget.path);

  /// Markdown 默认看渲染后的；带行号打开时看源码，才跳得到那一行。
  late bool _rendered = _markdown(widget.path) && widget.line == null;
  Future<String>? _fullText;

  @override
  void initState() {
    super.initState();
    unawaited(_open());
  }

  Future<void> _open() async {
    if (_error != null) setState(() => _error = null);
    try {
      final file = await AppScope.of(
        context,
      ).backend.openFile(widget.runtimeId, widget.path);
      if (!mounted) {
        if (file case OpenedText(:final text)) text.dispose();
        return;
      }
      setState(() => _file = file);
    } catch (error) {
      if (mounted) setState(() => _error = describeError(error));
    }
  }

  @override
  void dispose() {
    if (_file case OpenedText(:final text)) text.dispose();
    super.dispose();
  }

  String get _name {
    final cut = widget.path.lastIndexOf(RegExp(r'[/\\]'));
    return cut < 0 ? widget.path : widget.path.substring(cut + 1);
  }

  String get _dir {
    final cut = widget.path.lastIndexOf(RegExp(r'[/\\]'));
    return cut <= 0 ? widget.path : widget.path.substring(0, cut);
  }

  void _copyPath() {
    unawaited(Clipboard.setData(ClipboardData(text: widget.path)));
    ScaffoldMessenger.of(
      context,
    ).showSnackBar(SnackBar(content: Text(L10n.of(context).filesPathCopied)));
  }

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final text = switch (_file) {
      OpenedText(:final text) => text,
      _ => null,
    };
    final markdown = text != null && _markdown(widget.path);
    return Scaffold(
      backgroundColor: c.ground,
      appBar: AppBar(
        title: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(_name, maxLines: 1, overflow: TextOverflow.ellipsis),
            Text(
              text == null
                  ? _dir
                  : [
                      ?text.language,
                      l10n.filesLines(text.lineCount),
                    ].join(' · '),
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: t.caption.copyWith(color: c.inkMuted),
            ),
          ],
        ),
        actions: [
          if (markdown)
            IconButton(
              tooltip: _rendered ? l10n.filesShowSource : l10n.filesShowPreview,
              icon: Icon(_rendered ? LucideIcons.code : LucideIcons.bookOpen),
              onPressed: () => setState(() => _rendered = !_rendered),
            ),
          if (text != null && !(markdown && _rendered))
            IconButton(
              tooltip: _wrap ? l10n.filesNoWrap : l10n.filesWrap,
              icon: Icon(
                _wrap ? LucideIcons.arrowRightToLine : LucideIcons.wrapText,
              ),
              onPressed: () => setState(() => _wrap = !_wrap),
            ),
          IconButton(
            tooltip: l10n.filesCopyPath,
            icon: const Icon(LucideIcons.copy),
            onPressed: _copyPath,
          ),
          // 从文件浏览里打开的预览，也能一键退出整个浏览。
          if (ModalRoute.of(context)?.settings.name == projectFilesRoute)
            IconButton(
              tooltip: MaterialLocalizations.of(context).closeButtonTooltip,
              icon: const Icon(LucideIcons.x),
              onPressed: () => closeProjectFiles(context),
            ),
          const SizedBox(width: Space.xs),
        ],
      ),
      body: _body(context, l10n, text, markdown),
    );
  }

  Widget _body(
    BuildContext context,
    L10n l10n,
    TextSource? text,
    bool markdown,
  ) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final error = _error;
    if (error != null) {
      return EmptyNote(
        icon: LucideIcons.fileX,
        title: l10n.filesOpenFailed,
        body: error,
        action: GalleyButton.solid(label: l10n.commonRetry, onPressed: _open),
      );
    }
    return switch (_file) {
      null => const Center(child: CircularProgressIndicator.adaptive()),
      OpenedText() when markdown && _rendered => FutureBuilder<String>(
        future: _fullText ??= text!.fullText(),
        builder: (context, snapshot) {
          final data = snapshot.data;
          if (data == null) {
            return const Center(child: CircularProgressIndicator.adaptive());
          }
          return FileLinkScope(
            runtimeId: widget.runtimeId,
            cwd: _dir,
            child: SingleChildScrollView(
              padding: EdgeInsets.fromLTRB(
                Space.page,
                Space.lg,
                Space.page,
                MediaQuery.paddingOf(context).bottom + Space.xxl,
              ),
              child: ConversationMarkdown(data: data),
            ),
          );
        },
      ),
      OpenedText(:final text) => Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (!text.highlighted && text.language != null)
            Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: Space.page,
                vertical: Space.xs,
              ),
              child: Text(
                l10n.filesPlain,
                style: t.caption.copyWith(color: c.inkMuted),
              ),
            ),
          Expanded(
            child: CodeView(text: text, wrap: _wrap, initialLine: widget.line),
          ),
        ],
      ),
      OpenedImage(:final bytes, :final svg) => InteractiveViewer(
        maxScale: 8,
        child: Center(
          child: svg
              ? SvgPicture.memory(bytes)
              : Image.memory(
                  bytes,
                  errorBuilder: (_, _, _) => EmptyNote(
                    icon: LucideIcons.imageOff,
                    title: l10n.filesBinary,
                    body: l10n.filesBinaryBody,
                  ),
                ),
        ),
      ),
      OpenedBinary() => EmptyNote(
        icon: LucideIcons.fileQuestionMark,
        title: l10n.filesBinary,
        body: l10n.filesBinaryBody,
      ),
      OpenedTooLarge(:final size, :final limit) => EmptyNote(
        icon: LucideIcons.fileWarning,
        title: l10n.filesTooLarge,
        body: l10n.filesTooLargeBody(formatBytes(size), formatBytes(limit)),
      ),
    };
  }
}

/// 「12.3 MB」。只给人看大概，不区分 MiB / MB。
String formatBytes(int bytes) {
  const units = ['B', 'KB', 'MB', 'GB'];
  var value = bytes.toDouble();
  var unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return unit == 0 ? '$bytes B' : '${value.toStringAsFixed(1)} ${units[unit]}';
}

bool _markdown(String path) {
  final lower = path.toLowerCase();
  return lower.endsWith('.md') || lower.endsWith('.markdown');
}

/// 纯文字类文件：默认换行。
bool _prose(String path) {
  final lower = path.toLowerCase();
  return _markdown(path) || lower.endsWith('.txt') || lower.endsWith('.rst');
}
