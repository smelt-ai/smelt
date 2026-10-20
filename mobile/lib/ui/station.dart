import 'package:flutter/material.dart';

import '../theme/galley.dart';

/// 灶台标签：一段竖着的色带。每台机器一种颜色，跟着这台机器的每一条内容走。
class StationTape extends StatelessWidget {
  const StationTape({super.key, required this.color, this.height = 32});

  final Color color;
  final double height;

  @override
  Widget build(BuildContext context) => Container(
    width: Strokes.stationTape,
    height: height,
    decoration: BoxDecoration(
      color: color,
      borderRadius: const BorderRadius.all(Radii.tag),
    ),
  );
}

/// 色块 + 机器名，用在单据头、对话页标题下、筛选条里。
class StationLabel extends StatelessWidget {
  const StationLabel({
    super.key,
    required this.name,
    required this.color,
    this.style,
    this.textColor,
  });

  final String name;
  final Color color;
  final TextStyle? style;
  final Color? textColor;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Container(
          width: 8,
          height: 8,
          decoration: BoxDecoration(
            color: color,
            borderRadius: const BorderRadius.all(Radii.tag),
          ),
        ),
        const SizedBox(width: Space.xs + 2),
        Flexible(
          child: Text(
            name,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: (style ?? t.meta).copyWith(color: textColor ?? c.inkMuted),
          ),
        ),
      ],
    );
  }
}
