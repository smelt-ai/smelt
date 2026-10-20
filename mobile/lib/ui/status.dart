import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../theme/galley.dart';

/// 一条会话在界面上的状态记号。颜色只是第三条通路：形状（图标）和文字先说清楚。
enum SessionMark { needsYou, running, done, failed, idle, ended }

/// 状态记号：在做是会呼吸的点，其余是图标。读屏读 [label]。
class StatusMark extends StatelessWidget {
  const StatusMark({
    super.key,
    required this.mark,
    required this.label,
    this.stale = false,
  });

  final SessionMark mark;
  final String label;

  /// 来自离线缓存：画成静止的灰色记号，不能拿上次看到的「在做」冒充现在还在做。
  final bool stale;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final child = switch (mark) {
      SessionMark.running when stale => _HollowDot(color: c.inkFaint),
      SessionMark.running => _PulseDot(color: c.ok),
      SessionMark.needsYou => Icon(
        LucideIcons.hand,
        size: Strokes.iconSmall,
        color: c.heatInk,
      ),
      SessionMark.done => Icon(
        LucideIcons.check,
        size: Strokes.iconSmall,
        color: c.inkMuted,
      ),
      SessionMark.failed => Icon(
        LucideIcons.circleX,
        size: Strokes.iconSmall,
        color: c.danger,
      ),
      SessionMark.ended => Icon(
        LucideIcons.circleSlash,
        size: Strokes.iconSmall,
        color: c.inkFaint,
      ),
      SessionMark.idle => _HollowDot(color: c.inkFaint),
    };
    return Semantics(
      label: label,
      child: SizedBox.square(
        dimension: Strokes.icon,
        child: Center(child: ExcludeSemantics(child: child)),
      ),
    );
  }
}

class _HollowDot extends StatelessWidget {
  const _HollowDot({required this.color});
  final Color color;

  @override
  Widget build(BuildContext context) => Container(
    width: Strokes.statusDot,
    height: Strokes.statusDot,
    decoration: BoxDecoration(
      shape: BoxShape.circle,
      border: Border.all(color: color, width: 1.5),
    ),
  );
}

/// 在做：实心点外面一圈慢慢扩散。系统要求减少动态效果时就是一个静止的实心点。
class _PulseDot extends StatefulWidget {
  const _PulseDot({required this.color});
  final Color color;

  @override
  State<_PulseDot> createState() => _PulseDotState();
}

class _PulseDotState extends State<_PulseDot>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: Motion.pulse,
  );

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (Motion.resolve(context, Motion.pulse) == Duration.zero) {
      _controller.stop();
      _controller.value = 0;
    } else if (!_controller.isAnimating) {
      _controller.repeat();
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return RepaintBoundary(
      child: AnimatedBuilder(
        animation: _controller,
        builder: (context, _) {
          final t = Motion.easeOut.transform(_controller.value);
          return Stack(
            alignment: Alignment.center,
            children: [
              if (_controller.isAnimating)
                Container(
                  width: Strokes.statusDot + 8 * t,
                  height: Strokes.statusDot + 8 * t,
                  decoration: BoxDecoration(
                    shape: BoxShape.circle,
                    color: widget.color.withValues(alpha: 0.35 * (1 - t)),
                  ),
                ),
              Container(
                width: Strokes.statusDot,
                height: Strokes.statusDot,
                decoration: BoxDecoration(
                  shape: BoxShape.circle,
                  color: widget.color,
                ),
              ),
            ],
          );
        },
      ),
    );
  }
}
