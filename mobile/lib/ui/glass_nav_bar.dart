import 'dart:math' as math;
import 'dart:ui' show ImageFilter;

import 'package:flutter/material.dart';

import '../theme/galley.dart';

class GlassNavDestination {
  const GlassNavDestination({required this.icon, required this.label});

  final Widget icon;
  final String label;
}

/// 手机底部导航：浮在内容上的玻璃胶囊。底下滚过的内容模糊后透出来，选中块在各项之间滑动。
///
/// 放进 `Scaffold.bottomNavigationBar` 时要配 `extendBody: true`，内容才会滚到胶囊下面；
/// 页面底部的留白读 `MediaQuery.paddingOf(context).bottom`，已经包含胶囊的高度。
class GlassNavBar extends StatelessWidget {
  const GlassNavBar({
    super.key,
    required this.selectedIndex,
    required this.onDestinationSelected,
    required this.destinations,
  });

  final int selectedIndex;
  final ValueChanged<int> onDestinationSelected;
  final List<GlassNavDestination> destinations;

  static const _height = 64.0;
  static const _radius = BorderRadius.all(Radius.circular(_height / 2));
  static const _indicatorInset = Space.xs;
  static const _blur = 20.0;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    // 有 Home 指示条时压进安全区一截，离指示条还有一点距离；没有时离屏幕底 8。
    final bottom = math.max(
      Space.sm,
      MediaQuery.viewPaddingOf(context).bottom - Space.md,
    );
    return Padding(
      padding: EdgeInsets.fromLTRB(Space.page, 0, Space.page, bottom),
      child: DecoratedBox(
        decoration: BoxDecoration(
          borderRadius: _radius,
          boxShadow: [
            BoxShadow(
              color: c.shadow,
              offset: const Offset(0, 2),
              blurRadius: Space.md,
            ),
          ],
        ),
        child: ClipRRect(
          borderRadius: _radius,
          child: BackdropFilter(
            filter: ImageFilter.blur(sigmaX: _blur, sigmaY: _blur),
            child: DecoratedBox(
              decoration: BoxDecoration(
                color: c.glass,
                borderRadius: _radius,
                border: Border.all(color: c.glassEdge, width: Strokes.hairline),
              ),
              child: SizedBox(
                height: _height,
                child: LayoutBuilder(
                  builder: (context, constraints) {
                    final width = constraints.maxWidth / destinations.length;
                    return Stack(
                      children: [
                        AnimatedPositionedDirectional(
                          duration: Motion.standard,
                          curve: Motion.easeOut,
                          start: width * selectedIndex + _indicatorInset,
                          top: _indicatorInset,
                          bottom: _indicatorInset,
                          width: width - _indicatorInset * 2,
                          child: DecoratedBox(
                            decoration: BoxDecoration(
                              color: c.control,
                              borderRadius: const BorderRadius.all(
                                Radius.circular(
                                  _height / 2 - _indicatorInset,
                                ),
                              ),
                            ),
                          ),
                        ),
                        Row(
                          children: [
                            for (final (i, d) in destinations.indexed)
                              Expanded(
                                child: _Item(
                                  destination: d,
                                  selected: i == selectedIndex,
                                  onTap: () => onDestinationSelected(i),
                                ),
                              ),
                          ],
                        ),
                      ],
                    );
                  },
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _Item extends StatelessWidget {
  const _Item({
    required this.destination,
    required this.selected,
    required this.onTap,
  });

  final GlassNavDestination destination;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final color = selected ? c.ink : c.inkMuted;
    return MergeSemantics(
      child: Semantics(
        button: true,
        selected: selected,
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: onTap,
          child: Column(
            mainAxisAlignment: MainAxisAlignment.center,
            children: [
              IconTheme.merge(
                data: IconThemeData(size: 22, color: color),
                child: destination.icon,
              ),
              const SizedBox(height: Space.xs),
              // 胶囊高度固定，标签字号跟系统放大到 1.3 倍为止，免得撑出胶囊。
              MediaQuery.withClampedTextScaling(
                maxScaleFactor: 1.3,
                child: Text(
                  destination.label,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: t.caption.copyWith(
                    color: color,
                    fontWeight: selected ? FontWeight.w600 : FontWeight.w400,
                  ),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
