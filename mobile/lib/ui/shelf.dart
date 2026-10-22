import 'package:flutter/material.dart';

import '../theme/galley.dart';

/// 白色搁板：一组行，行间发丝线。搁板里不再套卡片。
class Shelf extends StatelessWidget {
  const Shelf({super.key, required this.children, this.margin});

  final List<Widget> children;
  final EdgeInsetsGeometry? margin;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final rows = <Widget>[];
    for (final (index, child) in children.indexed) {
      if (index > 0) {
        rows.add(
          Padding(
            padding: const EdgeInsetsDirectional.only(start: Space.lg),
            child: Divider(height: Strokes.hairline, color: c.hairline),
          ),
        );
      }
      rows.add(child);
    }
    return Padding(
      padding: margin ?? const EdgeInsets.symmetric(horizontal: Space.page),
      child: ClipRRect(
        borderRadius: const BorderRadius.all(Radii.shelf),
        child: ColoredBox(
          color: c.shelf,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: rows,
          ),
        ),
      ),
    );
  }
}

/// 搁板上方的分组标题。上面留得比下面多。
class ShelfHeader extends StatelessWidget {
  const ShelfHeader({
    super.key,
    required this.title,
    this.leading,
    this.trailing,
    this.first = false,
  });

  final String title;
  final Widget? leading;
  final Widget? trailing;

  /// 页面里的第一组，上面不用留那么多。
  final bool first;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return Padding(
      padding: EdgeInsets.fromLTRB(
        Space.page + Space.xs,
        first ? Space.sm : Space.section,
        Space.page + Space.xs,
        Space.sm,
      ),
      child: Row(
        children: [
          if (leading != null) ...[leading!, const SizedBox(width: Space.sm)],
          Expanded(
            child: LayoutBuilder(
              builder: (context, constraints) {
                // 状态可以比标题长（「重连中… · 显示的是 21:07 的数据」）。窄的列表栏
                // 放不下整句：状态最多占六成，超出省略。短状态仍按原宽靠右，标题占剩下的。
                final gap = trailing == null ? 0.0 : Space.sm;
                final room = constraints.maxWidth - gap;
                final maxTrail = room < 0 ? 0.0 : room * 0.62;
                return Row(
                  children: [
                    Expanded(
                      child: Semantics(
                        header: true,
                        child: Text(
                          title,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: t.label.copyWith(color: c.inkMuted),
                        ),
                      ),
                    ),
                    if (trailing != null) ...[
                      const SizedBox(width: Space.sm),
                      ConstrainedBox(
                        constraints: BoxConstraints(maxWidth: maxTrail),
                        child: DefaultTextStyle.merge(
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: t.meta.copyWith(color: c.inkMuted),
                          child: trailing!,
                        ),
                      ),
                    ],
                  ],
                );
              },
            ),
          ),
        ],
      ),
    );
  }
}

/// 搁板里的一行：前导、标题 / 副标题、尾部。最小高度满足触控下限。
class ShelfRow extends StatelessWidget {
  const ShelfRow({
    super.key,
    required this.title,
    this.subtitle,
    this.leading,
    this.trailing,
    this.onTap,
    this.onLongPress,
    this.titleStyle,
    this.semanticLabel,
  });

  final Widget title;
  final Widget? subtitle;
  final Widget? leading;
  final Widget? trailing;
  final VoidCallback? onTap;
  final VoidCallback? onLongPress;
  final TextStyle? titleStyle;
  final String? semanticLabel;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final content = ConstrainedBox(
      constraints: const BoxConstraints(minHeight: Space.touch + Space.sm),
      child: Padding(
        padding: const EdgeInsets.symmetric(
          horizontal: Space.lg,
          vertical: Space.md - 2,
        ),
        child: Row(
          children: [
            if (leading != null) ...[leading!, const SizedBox(width: Space.md)],
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  DefaultTextStyle(
                    style: (titleStyle ?? t.headline).copyWith(
                      color: titleStyle?.color ?? c.ink,
                    ),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    child: title,
                  ),
                  if (subtitle != null) ...[
                    const SizedBox(height: Space.xxs),
                    DefaultTextStyle(
                      style: t.meta.copyWith(color: c.inkMuted),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      child: subtitle!,
                    ),
                  ],
                ],
              ),
            ),
            if (trailing != null) ...[
              const SizedBox(width: Space.md),
              // 尾部默认是系统字的表格数字（「2 分钟前」）；耗时自己用等宽的 ElapsedLabel。
              DefaultTextStyle.merge(
                style: t.meta.copyWith(color: c.inkMuted),
                child: trailing!,
              ),
            ],
          ],
        ),
      ),
    );
    if (onTap == null && onLongPress == null) {
      return Semantics(label: semanticLabel, child: content);
    }
    return Semantics(
      button: true,
      label: semanticLabel,
      child: InkWell(onTap: onTap, onLongPress: onLongPress, child: content),
    );
  }
}
