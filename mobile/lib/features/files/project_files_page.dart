/// 一个项目的文件：逐层浏览、按文件名和内容搜，点文件打开预览。第一版只读。
///
/// 文件在那台机器上、随时在变，这里不缓存跨页面的结果：进页面、换目录、下拉时重取。
/// 返回键先退目录，退到项目根才离开页面。
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
import 'file_preview_page.dart';

/// 搜索框停手多久才发请求：不是轮询，只是别每敲一个字就让那台机器遍历一遍项目。
const _searchDebounce = Duration(milliseconds: 350);

class ProjectFilesPage extends StatefulWidget {
  const ProjectFilesPage({
    super.key,
    required this.runtimeId,
    required this.root,
    required this.title,
  });

  final String runtimeId;

  /// 项目根：那台机器上的绝对路径。
  final String root;

  /// 项目名，根目录那一层的标题。
  final String title;

  @override
  State<ProjectFilesPage> createState() => _ProjectFilesPageState();
}

class _ProjectFilesPageState extends State<ProjectFilesPage> {
  /// 一路点进来的目录，最后一个是当前这层。第一个是项目根。
  late final List<String> _trail = [widget.root];
  List<FileEntry>? _entries;
  String? _error;
  int _generation = 0;

  final _query = TextEditingController();
  Timer? _searchTimer;
  String _searched = '';
  FileSearch? _search;
  String? _searchError;
  bool _searching = false;

  String get _dir => _trail.last;

  @override
  void initState() {
    super.initState();
    unawaited(_list());
    _query.addListener(_onQuery);
  }

  @override
  void dispose() {
    _searchTimer?.cancel();
    _query.dispose();
    super.dispose();
  }

  Future<void> _list() async {
    final generation = ++_generation;
    final dir = _dir;
    try {
      final entries = await AppScope.of(
        context,
      ).backend.listProjectDir(widget.runtimeId, root: widget.root, dir: dir);
      if (!mounted || generation != _generation) return;
      setState(() {
        _entries = entries;
        _error = null;
      });
    } catch (error) {
      if (!mounted || generation != _generation) return;
      setState(() => _error = describeError(error));
    }
  }

  void _enter(String dir) {
    setState(() {
      _trail.add(dir);
      _entries = null;
      _error = null;
    });
    unawaited(_list());
  }

  void _up() {
    setState(() {
      _trail.removeLast();
      _entries = null;
      _error = null;
    });
    unawaited(_list());
  }

  void _onQuery() {
    final query = _query.text.trim();
    // 清除按钮跟着有没有字出现。
    setState(() {});
    if (query == _searched) return;
    _searchTimer?.cancel();
    if (query.isEmpty) {
      setState(() {
        _searched = '';
        _search = null;
        _searchError = null;
        _searching = false;
      });
      return;
    }
    _searchTimer = Timer(_searchDebounce, () => unawaited(_runSearch(query)));
  }

  Future<void> _runSearch(String query) async {
    setState(() {
      _searched = query;
      _searching = true;
    });
    try {
      final found = await AppScope.of(context).backend.searchProject(
        widget.runtimeId,
        root: widget.root,
        query: query,
      );
      if (!mounted || query != _searched) return;
      setState(() {
        _search = found;
        _searchError = null;
        _searching = false;
      });
    } catch (error) {
      if (!mounted || query != _searched) return;
      setState(() {
        _searchError = describeError(error);
        _searching = false;
      });
    }
  }

  void _open(String path, {int? line}) => openFilePreview(
    context,
    runtimeId: widget.runtimeId,
    path: path,
    line: line,
  );

  String get _title {
    if (_trail.length == 1) return widget.title;
    final cut = _dir.lastIndexOf(RegExp(r'[/\\]'));
    return cut < 0 ? _dir : _dir.substring(cut + 1);
  }

  /// 当前目录相对项目根的路径，给人看。
  String get _relative {
    if (_trail.length == 1) return widget.root;
    final rest = _dir.substring(widget.root.length);
    return rest.replaceFirst(RegExp(r'^[/\\]'), '');
  }

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return PopScope(
      canPop: _trail.length == 1,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) _up();
      },
      child: Scaffold(
        backgroundColor: c.ground,
        appBar: AppBar(
          title: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(_title, maxLines: 1, overflow: TextOverflow.ellipsis),
              Text(
                _relative,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: t.caption.copyWith(color: c.inkMuted),
              ),
            ],
          ),
          bottom: PreferredSize(
            preferredSize: const Size.fromHeight(Space.touch + Space.md),
            child: Padding(
              padding: const EdgeInsets.fromLTRB(
                Space.page,
                0,
                Space.page,
                Space.md,
              ),
              child: TextField(
                controller: _query,
                textInputAction: TextInputAction.search,
                decoration: InputDecoration(
                  hintText: l10n.filesSearchHint,
                  prefixIcon: const Icon(LucideIcons.search),
                  suffixIcon: _query.text.isEmpty
                      ? null
                      : IconButton(
                          tooltip: MaterialLocalizations.of(
                            context,
                          ).deleteButtonTooltip,
                          icon: const Icon(LucideIcons.x),
                          onPressed: _query.clear,
                        ),
                  isDense: true,
                ),
              ),
            ),
          ),
        ),
        body: _searched.isEmpty ? _listing(context, l10n) : _results(l10n),
      ),
    );
  }

  Widget _listing(BuildContext context, L10n l10n) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final entries = _entries;
    final error = _error;
    if (entries == null) {
      if (error == null) {
        return const Center(child: CircularProgressIndicator.adaptive());
      }
      return EmptyNote(
        icon: LucideIcons.folderX,
        title: l10n.filesLoadFailed,
        body: error,
        action: GalleyButton.solid(label: l10n.commonRetry, onPressed: _list),
      );
    }
    final bottom = MediaQuery.paddingOf(context).bottom;
    return RefreshIndicator.adaptive(
      onRefresh: _list,
      child: ListView(
        physics: const AlwaysScrollableScrollPhysics(),
        padding: EdgeInsets.only(top: Space.sm, bottom: bottom + Space.xxl),
        children: [
          if (error != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(
                Space.page + Space.xs,
                0,
                Space.page + Space.xs,
                Space.sm,
              ),
              child: Text(error, style: t.note.copyWith(color: c.danger)),
            ),
          if (entries.isEmpty)
            EmptyNote(icon: LucideIcons.folderOpen, title: l10n.filesEmptyDir)
          else
            Shelf(
              children: [
                HairlineRows(
                  children: [
                    for (final entry in entries)
                      ShelfRow(
                        leading: Icon(
                          entry.isDir
                              ? LucideIcons.folder
                              : _fileIcon(entry.name),
                          size: Strokes.icon,
                          color: c.inkMuted,
                        ),
                        title: Text(
                          entry.name,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                        ),
                        trailing: entry.isDir
                            ? Icon(
                                LucideIcons.chevronRight,
                                size: Strokes.iconSmall,
                                color: c.inkFaint,
                              )
                            : null,
                        onTap: () => entry.isDir
                            ? _enter(entry.path)
                            : _open(entry.path),
                      ),
                  ],
                ),
              ],
            ),
        ],
      ),
    );
  }

  Widget _results(L10n l10n) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final found = _search;
    final error = _searchError;
    if (error != null) {
      return EmptyNote(
        icon: LucideIcons.searchX,
        title: l10n.filesLoadFailed,
        body: error,
      );
    }
    if (found == null || (_searching && found.hits.isEmpty)) {
      return const Center(child: CircularProgressIndicator.adaptive());
    }
    if (found.hits.isEmpty) {
      return EmptyNote(icon: LucideIcons.searchX, title: l10n.filesSearchNone);
    }
    final bottom = MediaQuery.paddingOf(context).bottom;
    return ListView(
      padding: EdgeInsets.only(top: Space.sm, bottom: bottom + Space.xxl),
      children: [
        Shelf(
          children: [
            HairlineRows(
              children: [
                for (final hit in found.hits)
                  ShelfRow(
                    leading: Icon(
                      hit.line == null
                          ? _fileIcon(hit.rel)
                          : LucideIcons.textSearch,
                      size: Strokes.icon,
                      color: c.inkMuted,
                    ),
                    title: Text(
                      hit.rel,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                    subtitle: hit.line == null
                        ? null
                        : Text(
                            '${l10n.filesSearchLine(hit.line!)} · ${hit.preview ?? ''}',
                            maxLines: 2,
                            overflow: TextOverflow.ellipsis,
                            style: t.code.copyWith(color: c.inkMuted),
                          ),
                    onTap: () => _open(hit.path, line: hit.line),
                  ),
              ],
            ),
          ],
        ),
        if (found.truncated)
          Padding(
            padding: const EdgeInsets.all(Space.page),
            child: Text(
              l10n.filesSearchTruncated,
              style: t.note.copyWith(color: c.inkMuted),
            ),
          ),
      ],
    );
  }
}

IconData _fileIcon(String name) {
  final lower = name.toLowerCase();
  if (lower.endsWith('.md') || lower.endsWith('.txt')) {
    return LucideIcons.fileText;
  }
  if (RegExp(r'\.(png|jpe?g|gif|webp|bmp|svg|tiff?)$').hasMatch(lower)) {
    return LucideIcons.fileImage;
  }
  return LucideIcons.fileCode;
}
