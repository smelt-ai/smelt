import 'package:flutter/material.dart';

import '../theme/galley.dart';

/// 顶层页：大标题，向上滚动时收成一行。
///
/// [guide] 是「导词」：标题收起后，导航栏标题下写当前滚到的那一段（比如哪台机器），
/// 像辞书页眉。由页面根据滚动位置算好传进来。
class GalleyPage extends StatelessWidget {
  const GalleyPage({
    super.key,
    required this.title,
    required this.slivers,
    this.actions = const [],
    this.guide,
    this.controller,
    this.onRefresh,
    this.leading,
  });

  final String title;
  final List<Widget> slivers;
  final List<Widget> actions;
  final String? guide;
  final ScrollController? controller;
  final Future<void> Function()? onRefresh;
  final Widget? leading;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final scroll = CustomScrollView(
      controller: controller,
      physics: const AlwaysScrollableScrollPhysics(),
      slivers: [
        SliverAppBar.large(
          pinned: true,
          backgroundColor: c.ground,
          surfaceTintColor: Colors.transparent,
          leading: leading,
          automaticallyImplyLeading: leading == null,
          actions: [
            ...actions,
            const SizedBox(width: Space.xs),
          ],
          title: _CollapsingTitle(title: title, guide: guide),
          expandedHeight: Space.largeTitleBar,
          // SliverAppBar.large 展开的大标题取 titleTextStyle，没有就取 AppBarTheme 的（17pt
          // 导航栏标题），轮不到 headlineMedium。所以这里显式给大标题样式；收起后进工具栏
          // 的那份标题由 _CollapsingTitle 自己换回导航栏字号。
          titleTextStyle: GalleyText.of(
            context,
          ).largeTitle.copyWith(color: c.ink),
          foregroundColor: c.ink,
        ),
        ...slivers,
        SliverToBoxAdapter(
          child: SizedBox(
            height: MediaQuery.paddingOf(context).bottom + Space.xxl,
          ),
        ),
      ],
    );
    if (onRefresh == null) return scroll;
    return RefreshIndicator.adaptive(onRefresh: onRefresh!, child: scroll);
  }
}

class _CollapsingTitle extends StatelessWidget {
  const _CollapsingTitle({required this.title, required this.guide});

  final String title;
  final String? guide;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final settings = context
        .dependOnInheritedWidgetOfExactType<FlexibleSpaceBarSettings>();
    final collapsed =
        settings == null ||
        settings.currentExtent <= settings.minExtent + Space.xs;
    if (!collapsed) return Text(title);
    final bar = t.navTitle.copyWith(color: c.ink);
    if (guide == null) return Text(title, style: bar);
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(title, style: bar),
        Text(
          guide!,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: t.caption.copyWith(color: c.inkMuted),
        ),
      ],
    );
  }
}

/// 空状态：一句说明，必要时一个动作。不画插图卡片。
class EmptyNote extends StatelessWidget {
  const EmptyNote({
    super.key,
    required this.title,
    this.body,
    this.action,
    this.icon,
  });

  final String title;
  final String? body;
  final Widget? action;
  final IconData? icon;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(
        horizontal: Space.xxl,
        vertical: Space.xxl,
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (icon != null) ...[
            Icon(icon, size: 28, color: c.inkFaint),
            const SizedBox(height: Space.md),
          ],
          Text(
            title,
            textAlign: TextAlign.center,
            style: t.headline.copyWith(color: c.ink),
          ),
          if (body != null) ...[
            const SizedBox(height: Space.xs),
            Text(
              body!,
              textAlign: TextAlign.center,
              style: t.note.copyWith(color: c.inkMuted),
            ),
          ],
          if (action != null) ...[const SizedBox(height: Space.lg), action!],
        ],
      ),
    );
  }
}
