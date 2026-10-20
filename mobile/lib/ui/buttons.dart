import 'package:flutter/material.dart';

import '../theme/galley.dart';

enum _Tone { heat, quiet, paper, solid, danger }

/// 按钮。琥珀（[GalleyButton.heat]）只给单据上你要按的那颗；其余主要动作用实心墨，
/// 次要用中性底。高度满足触控下限，按下只换底色。
class GalleyButton extends StatelessWidget {
  const GalleyButton.heat({
    super.key,
    required this.label,
    required this.onPressed,
    this.icon,
    this.expand = false,
  }) : _tone = _Tone.heat;

  const GalleyButton.quiet({
    super.key,
    required this.label,
    required this.onPressed,
    this.icon,
    this.expand = false,
  }) : _tone = _Tone.quiet;

  /// 单据纸上的次要按钮（拒绝、跳过）。
  const GalleyButton.paper({
    super.key,
    required this.label,
    required this.onPressed,
    this.icon,
    this.expand = false,
  }) : _tone = _Tone.paper;

  const GalleyButton.solid({
    super.key,
    required this.label,
    required this.onPressed,
    this.icon,
    this.expand = false,
  }) : _tone = _Tone.solid;

  const GalleyButton.danger({
    super.key,
    required this.label,
    required this.onPressed,
    this.icon,
    this.expand = false,
  }) : _tone = _Tone.danger;

  final String label;
  final VoidCallback? onPressed;
  final IconData? icon;
  final bool expand;
  final _Tone _tone;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final (bg, fg) = switch (_tone) {
      _Tone.heat => (c.heat, c.onHeat),
      _Tone.quiet => (c.control, c.onControl),
      _Tone.paper => (c.paperControl, c.onPaper),
      _Tone.solid => (c.solid, c.onSolid),
      _Tone.danger => (c.dangerWash, c.danger),
    };
    final style = FilledButton.styleFrom(
      backgroundColor: bg,
      foregroundColor: fg,
      disabledBackgroundColor: c.control,
      disabledForegroundColor: c.inkFaint,
      minimumSize: Size(expand ? double.infinity : Space.touch, Space.touch),
      padding: const EdgeInsets.symmetric(horizontal: Space.lg),
      textStyle: t.button,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.all(Radii.control),
      ),
    );
    final text = Text(label, maxLines: 1, overflow: TextOverflow.ellipsis);
    return icon == null
        ? FilledButton(onPressed: onPressed, style: style, child: text)
        : FilledButton.icon(
            onPressed: onPressed,
            style: style,
            icon: Icon(icon, size: Strokes.iconSmall),
            label: text,
          );
  }
}
