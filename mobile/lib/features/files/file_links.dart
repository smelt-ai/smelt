/// 对话和 Markdown 预览里的文件链接：点了在 App 里打开预览，不交给系统浏览器。
///
/// 链接是不是文件、绝对路径和行号怎么算，由内核按桌面同一套规则解析
/// （`smelt_model::file_link`）。这里只决定解析的基准：哪台机器、相对哪个目录。
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../data/stores.dart';
import 'file_preview_page.dart';

/// 往下给 Markdown 的链接解析基准。没有它的地方，链接照旧交给系统打开。
class FileLinkScope extends InheritedWidget {
  const FileLinkScope({
    super.key,
    required this.runtimeId,
    required this.cwd,
    required super.child,
  });

  final String runtimeId;

  /// 相对路径按它补全；null 时只认绝对路径。
  final String? cwd;

  static FileLinkScope? maybeOf(BuildContext context) =>
      context.dependOnInheritedWidgetOfExactType<FileLinkScope>();

  @override
  bool updateShouldNotify(FileLinkScope old) =>
      old.runtimeId != runtimeId || old.cwd != cwd;
}

/// `target` 是那台机器上的文件就推一页预览，返回 true；不是文件、或外面没有
/// [FileLinkScope] 返回 false。
bool openFileLink(BuildContext context, String target) {
  final scope = FileLinkScope.maybeOf(context);
  if (scope == null) return false;
  final link = AppScope.of(
    context,
  ).backend.resolveFileLink(scope.runtimeId, cwd: scope.cwd, target: target);
  if (link == null) return false;
  openFilePreview(
    context,
    runtimeId: scope.runtimeId,
    path: link.path,
    line: link.line,
  );
  return true;
}

/// Markdown 链接被点了：是那台机器上的文件就在 App 里预览，否则交给系统。
void openMarkdownLink(BuildContext context, String? href) {
  if (href == null || openFileLink(context, href)) return;
  final uri = Uri.tryParse(href);
  if (uri != null) {
    unawaited(launchUrl(uri, mode: LaunchMode.externalApplication));
  }
}
