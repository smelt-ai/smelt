/// 在一台机器上逐级浏览目录：新建会话时打开项目、给智能体绑定目录都用它。
///
/// [DirBrowser] 管状态和导航（来路、快捷位置、进不去的目录），[DirBrowserView] 只画。
/// 底部那颗按钮作用于当前这一层（点行是进去，不是选中），按钮做什么由调用方给。
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/backend.dart';
import '../../data/models.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import '../../ui/buttons.dart';
import '../../ui/page.dart';
import '../../ui/shelf.dart';

class DirBrowser extends ChangeNotifier {
  DirBrowser(this._backend, this.runtimeId);

  final Backend _backend;
  final String runtimeId;

  /// 当前列出的一层。null = 第一次还没列出来。
  DirListing? get listing => _listing;
  DirListing? _listing;

  bool get busy => _busy;
  bool _busy = false;

  /// 最近一次没列出来的原因。列出来了就清掉。
  String? get error => _error;
  String? _error;

  /// 一路点进来的目录。返回时一层层退回去，退空了才离开浏览。
  final List<String> _trail = [];
  bool get canGoBack => _trail.isNotEmpty;

  int _generation = 0;
  bool _disposed = false;

  /// 正在退回上一层。来路要等列成功才摘，退的路上再按返回不能又去列同一层。
  bool _backing = false;

  /// 从头列：[path] 为空时由内核挑起点（最近打开的项目所在的目录，或主目录）。
  Future<void> start([String? path]) {
    _trail.clear();
    _listing = null;
    _error = null;
    return _list(path);
  }

  /// 进到 [path]，当前目录记进来路。
  void go(String path) => unawaited(_list(path, remember: true));

  /// 退回来路的上一层。来路空了返回 false，由调用方决定退到哪。
  bool back() {
    if (_trail.isEmpty) return false;
    if (!_backing) unawaited(_list(_trail.last, backward: true));
    return true;
  }

  /// 来路只在列成功之后改：列失败就留在原处，下次返回还回得去。
  Future<void> _list(
    String? path, {
    bool remember = false,
    bool backward = false,
  }) async {
    final generation = ++_generation;
    final from = _listing?.path;
    _backing = backward;
    _busy = true;
    _notify();
    try {
      final listing = await _backend.browseDirs(runtimeId, path: path);
      if (_disposed || generation != _generation) return;
      if (remember && from != null) _trail.add(from);
      if (backward && _trail.isNotEmpty) _trail.removeLast();
      _listing = listing;
      _error = null;
    } catch (error) {
      if (_disposed || generation != _generation) return;
      // 进不去的目录：留在原地，把原因写出来。
      _error = describeError(error);
    }
    _busy = false;
    _backing = false;
    _notify();
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }
}

class DirBrowserView extends StatelessWidget {
  const DirBrowserView({
    super.key,
    required this.browser,
    required this.actionLabel,
    required this.onAction,
    this.actionBusy = false,
    this.actionError,
  });

  final DirBrowser browser;

  /// 底部按钮的字，按当前这一层给（「把「x」打开为项目」「绑定「x」」）。
  final String Function(DirListing listing) actionLabel;
  final void Function(DirListing listing) onAction;

  /// 按钮的动作正在进行：整页的点击都先停下。
  final bool actionBusy;
  final String? actionError;

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: browser,
    builder: (context, _) => _build(context),
  );

  Widget _build(BuildContext context) {
    final l10n = L10n.of(context);
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final bottom = MediaQuery.paddingOf(context).bottom;
    final listing = browser.listing;
    if (listing == null) {
      final error = browser.error;
      if (error == null || browser.busy) {
        return Padding(
          padding: EdgeInsets.fromLTRB(0, Space.xxl, 0, bottom + Space.xxl),
          child: const Center(child: CircularProgressIndicator.adaptive()),
        );
      }
      return SingleChildScrollView(
        padding: EdgeInsets.only(bottom: bottom + Space.lg),
        child: EmptyNote(
          icon: LucideIcons.folderX,
          title: l10n.launchFolderLoadFailed,
          body: error,
          action: GalleyButton.solid(
            label: l10n.commonRetry,
            onPressed: () => browser.start(),
          ),
        ),
      );
    }
    final busy = browser.busy || actionBusy;
    final parent = listing.parent;
    final error = actionError ?? browser.error;
    final chevron = Icon(
      LucideIcons.chevronRight,
      size: Strokes.iconSmall,
      color: c.inkFaint,
    );
    return Column(
      mainAxisSize: MainAxisSize.min,
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        ShelfHeader(
          title: l10n.launchStepFolder,
          first: true,
          trailing: browser.busy
              ? const SizedBox.square(
                  dimension: Strokes.iconSmall,
                  child: CircularProgressIndicator.adaptive(strokeWidth: 2),
                )
              : null,
        ),
        _ShortcutBar(
          shortcuts: listing.shortcuts,
          current: listing.path,
          onTap: busy ? null : browser.go,
        ),
        Padding(
          padding: const EdgeInsets.fromLTRB(
            Space.page + Space.xs,
            Space.sm,
            Space.page + Space.xs,
            Space.sm,
          ),
          child: Text(
            listing.displayPath,
            maxLines: 2,
            overflow: TextOverflow.ellipsis,
            style: t.code.copyWith(color: c.inkMuted),
          ),
        ),
        if (error != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(
              Space.page + Space.xs,
              0,
              Space.page + Space.xs,
              Space.sm,
            ),
            child: Semantics(
              liveRegion: true,
              child: Text(error, style: t.note.copyWith(color: c.danger)),
            ),
          ),
        Flexible(
          child: ListView(
            shrinkWrap: true,
            children: [
              HairlineRows(
                children: [
                  if (parent != null)
                    ShelfRow(
                      leading: Icon(
                        LucideIcons.cornerLeftUp,
                        size: Strokes.icon,
                        color: c.inkMuted,
                      ),
                      title: Text(l10n.launchFolderUp),
                      onTap: busy ? null : () => browser.go(parent),
                    ),
                  for (final entry in listing.entries)
                    ShelfRow(
                      leading: Icon(
                        LucideIcons.folder,
                        size: Strokes.icon,
                        color: c.inkMuted,
                      ),
                      title: Text(entry.name),
                      subtitle: entry.opened
                          ? Text(l10n.launchFolderOpened)
                          : null,
                      trailing: chevron,
                      onTap: busy ? null : () => browser.go(entry.path),
                    ),
                ],
              ),
              if (listing.entries.isEmpty)
                Padding(
                  padding: const EdgeInsets.symmetric(
                    horizontal: Space.page + Space.xs,
                    vertical: Space.lg,
                  ),
                  child: Text(
                    l10n.launchFolderEmpty,
                    style: t.note.copyWith(color: c.inkMuted),
                  ),
                ),
            ],
          ),
        ),
        Padding(
          padding: EdgeInsets.fromLTRB(
            Space.page,
            Space.md,
            Space.page,
            bottom + Space.lg,
          ),
          child: GalleyButton.solid(
            expand: true,
            label: actionLabel(listing),
            onPressed: busy ? null : () => onAction(listing),
          ),
        ),
      ],
    );
  }
}

/// 快捷位置：主目录、已打开项目所在的目录。横向一排，所在的那个高亮。
class _ShortcutBar extends StatelessWidget {
  const _ShortcutBar({
    required this.shortcuts,
    required this.current,
    required this.onTap,
  });

  final List<DirShortcut> shortcuts;
  final String current;
  final ValueChanged<String>? onTap;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      padding: const EdgeInsets.symmetric(horizontal: Space.page),
      child: Row(
        children: [
          for (final shortcut in shortcuts)
            Padding(
              padding: const EdgeInsetsDirectional.only(end: Space.sm),
              child: ChoiceChip(
                label: Text(shortcut.label),
                selected: shortcut.path == current,
                // 所在的那个不置灰，点了也不动。
                onSelected: onTap == null
                    ? null
                    : (_) {
                        if (shortcut.path != current) onTap!(shortcut.path);
                      },
                // 选中底是实心墨，字要跟着反白（主题的 labelStyle 只管未选中）。
                labelStyle: t.label.copyWith(
                  color: shortcut.path == current ? c.onSolid : c.ink,
                ),
              ),
            ),
        ],
      ),
    );
  }
}
