import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../theme/galley.dart';

/// 单据纸：只有需要你动手的事才是纸。顶边是撕下来的锯齿，下面两角是纸的小圆角，
/// 有一点真实的投影（有偏移、有模糊）。
class TicketPaper extends StatelessWidget {
  const TicketPaper({super.key, required this.child, this.width, this.stamp});

  final Widget child;
  final double? width;

  /// 处理后盖在单据上的章。
  final TicketStamp? stamp;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    return SizedBox(
      width: width,
      child: CustomPaint(
        painter: _PaperPainter(
          paper: c.paper,
          edge: c.paperEdge,
          shadow: c.shadow,
        ),
        child: Stack(
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(
                Space.md,
                Strokes.ticketTear + Space.sm,
                Space.md,
                Space.md,
              ),
              child: DefaultTextStyle(
                style: GalleyText.of(context).body.copyWith(color: c.onPaper),
                child: child,
              ),
            ),
            if (stamp != null) Positioned.fill(child: Center(child: stamp!)),
          ],
        ),
      ),
    );
  }
}

class _PaperPainter extends CustomPainter {
  _PaperPainter({
    required this.paper,
    required this.edge,
    required this.shadow,
  });

  final Color paper;
  final Color edge;
  final Color shadow;

  Path _outline(Size size) {
    const tooth = Strokes.ticketTear;
    const r = 4.0;
    final path = Path()..moveTo(0, tooth);
    // 锯齿顶边：等宽的小三角。
    final teeth = math.max(1, (size.width / (tooth * 1.5)).floor());
    final step = size.width / teeth;
    for (var i = 0; i < teeth; i++) {
      final x = i * step;
      path
        ..lineTo(x + step / 2, 0)
        ..lineTo(x + step, tooth);
    }
    path
      ..lineTo(size.width, size.height - r)
      ..quadraticBezierTo(size.width, size.height, size.width - r, size.height)
      ..lineTo(r, size.height)
      ..quadraticBezierTo(0, size.height, 0, size.height - r)
      ..close();
    return path;
  }

  @override
  void paint(Canvas canvas, Size size) {
    final outline = _outline(size);
    canvas.drawShadow(outline, shadow, 3, false);
    canvas.drawPath(outline, Paint()..color = paper);
    canvas.drawPath(
      outline,
      Paint()
        ..color = edge
        ..style = PaintingStyle.stroke
        ..strokeWidth = 0.6,
    );
  }

  @override
  bool shouldRepaint(_PaperPainter old) =>
      old.paper != paper || old.edge != edge || old.shadow != shadow;
}

/// 单据头：等宽小字，`#1 · ▮ MacBook Pro ······ 00:42`。
class TicketHead extends StatelessWidget {
  const TicketHead({super.key, required this.leading, required this.trailing});

  final List<Widget> leading;
  final Widget trailing;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return DefaultTextStyle(
      style: t.monoHead.copyWith(color: c.onPaperMuted),
      maxLines: 1,
      overflow: TextOverflow.ellipsis,
      child: Row(
        children: [
          Expanded(
            child: Row(
              children: [
                for (final (i, w) in leading.indexed) ...[
                  if (i > 0) const SizedBox(width: Space.sm),
                  i == leading.length - 1 ? Flexible(child: w) : w,
                ],
              ],
            ),
          ),
          const SizedBox(width: Space.sm),
          trailing,
        ],
      ),
    );
  }
}

/// 单据上的虚线分隔，像热敏纸上打出来的那种。
class TicketRule extends StatelessWidget {
  const TicketRule({super.key});

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: Space.sm),
      child: CustomPaint(
        size: const Size(double.infinity, 1),
        painter: _DashPainter(c.paperEdge),
      ),
    );
  }
}

class _DashPainter extends CustomPainter {
  _DashPainter(this.color);
  final Color color;

  @override
  void paint(Canvas canvas, Size size) {
    final paint = Paint()
      ..color = color
      ..strokeWidth = 1;
    for (var x = 0.0; x < size.width; x += 6) {
      canvas.drawLine(
        Offset(x, 0),
        Offset(math.min(x + 3, size.width), 0),
        paint,
      );
    }
  }

  @override
  bool shouldRepaint(_DashPainter old) => old.color != color;
}

/// 单据上的命令 / 正文摘录：等宽、最多几行。
class TicketExcerpt extends StatelessWidget {
  const TicketExcerpt(
    this.text, {
    super.key,
    this.maxLines = 3,
    this.mono = true,
  });

  final String text;
  final int maxLines;
  final bool mono;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return Text(
      text,
      maxLines: maxLines,
      overflow: TextOverflow.ellipsis,
      style: (mono ? t.code : t.body).copyWith(color: c.onPaper),
    );
  }
}

/// 处理后盖的章：「已允许 · 14:02」。歪一点、描边、半透明墨。
class TicketStamp extends StatelessWidget {
  const TicketStamp({
    super.key,
    required this.text,
    this.tone = StampTone.allow,
    this.compact = false,
  });

  final String text;
  final StampTone tone;

  /// 留痕用的小章：对话时间线里那条工具调用下面，静止、不歪、不落下。
  final bool compact;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final ink = switch (tone) {
      StampTone.allow => c.ok,
      StampTone.deny => c.danger,
      StampTone.neutral => c.onPaperMuted,
    };
    // 章上的字用系统粗体（中文用等宽字会被拉开），数字照样等宽对齐。
    final stampText = t.button.copyWith(
      color: ink,
      fontWeight: FontWeight.w800,
      fontFeatures: const [FontFeature.tabularFigures()],
    );
    if (compact) {
      // 留在时间线里的小章：粗边、略歪，读起来是盖上去的，不是状态标签。
      return Transform.rotate(
        angle: -0.04,
        child: Container(
          padding: const EdgeInsets.symmetric(
            horizontal: Space.sm,
            vertical: Space.xxs,
          ),
          decoration: BoxDecoration(
            border: Border.all(color: ink.withValues(alpha: 0.85), width: 2),
            borderRadius: const BorderRadius.all(Radii.ticket),
          ),
          child: Text(text, style: stampText),
        ),
      );
    }
    final duration = Motion.resolve(context, Motion.stamp);
    return TweenAnimationBuilder<double>(
      tween: Tween(begin: duration == Duration.zero ? 1 : 0, end: 1),
      duration: duration,
      curve: Motion.easeOut,
      builder: (context, v, child) => Opacity(
        opacity: v.clamp(0, 1),
        child: Transform.rotate(
          angle: -0.12,
          child: Transform.scale(scale: 1.4 - 0.4 * v, child: child),
        ),
      ),
      child: Container(
        padding: const EdgeInsets.symmetric(
          horizontal: Space.md,
          vertical: Space.xs,
        ),
        decoration: BoxDecoration(
          border: Border.all(color: ink.withValues(alpha: 0.85), width: 2),
          borderRadius: const BorderRadius.all(Radii.ticket),
          color: c.paper.withValues(alpha: 0.85),
        ),
        child: Text(text, style: stampText),
      ),
    );
  }
}

enum StampTone { allow, deny, neutral }
