import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';
import 'package:xterm/xterm.dart';

import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';

/// 快捷键栏的统一出口。只报「按了哪个键、带了哪些修饰」，编码由终端按它当前的模式
/// 决定（kitty keyboard protocol、DECCKM），与蓝牙键盘走同一条路。
typedef TerminalKeyHandler =
    void Function(TerminalKey key, {bool shift, bool alt, bool ctrl});

/// 快捷键栏：一排钢底上的等宽小键。
///
/// 键位按 agent 场景排：最左边是打断 / 切模式这类高频键，方向键居中，常用符号和翻页
/// 在右——横向滚动时右侧先被截掉，低频的放那边。
///
/// 键帽上的字（Esc、Tab、^C、PgUp）是键盘记号，跟代码一样不翻译；读屏和长按提示读
/// 本地化的说法。
class TerminalShortcutBar extends StatelessWidget {
  const TerminalShortcutBar({
    super.key,
    required this.onKey,
    required this.onText,
    required this.onPaste,
    required this.ctrlLatched,
    required this.onToggleCtrl,
  });

  /// 功能键（方向、Tab、Esc……）。
  final TerminalKeyHandler onKey;

  /// 常用符号：当普通文字输入发出去（与软键盘打字同一条路，Ctrl 锁存对它同样生效）。
  final ValueChanged<String> onText;

  /// 剪贴板粘贴。手机上没有 Cmd/Ctrl+V 可按。
  final VoidCallback onPaste;

  /// Ctrl 已按下、等下一个键。
  final bool ctrlLatched;
  final VoidCallback onToggleCtrl;

  /// 软键盘第二、三层里才有的符号，CLI 里又天天要打。
  static const symbols = ['/', '~', '|', '-', '`'];

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    return DecoratedBox(
      decoration: BoxDecoration(
        color: c.ground,
        border: Border(top: BorderSide(color: c.hairline)),
      ),
      // 键帽跟着系统字号放大，但封顶：栏子每高一点终端就少一行。
      child: MediaQuery.withClampedTextScaling(
        maxScaleFactor: 1.6,
        child: SingleChildScrollView(
          scrollDirection: Axis.horizontal,
          padding: const EdgeInsets.symmetric(horizontal: Space.xs),
          child: Row(
            children: [
              _Key.text(
                'Esc',
                label: l10n.terminalKeyEscape,
                onPressed: () => onKey(TerminalKey.escape),
              ),
              _Key.text(
                'Ctrl',
                label: l10n.terminalKeyCtrl,
                latched: ctrlLatched,
                onPressed: onToggleCtrl,
              ),
              _Key.text(
                'Tab',
                label: l10n.terminalKeyTab,
                onPressed: () => onKey(TerminalKey.tab),
              ),
              // Claude Code 用它切换模式；软键盘上按不出 Shift+Tab，只能靠这里。
              _Key.shifted(
                text: 'Tab',
                label: l10n.terminalKeyShiftTab,
                onPressed: () => onKey(TerminalKey.tab, shift: true),
              ),
              _Key.text(
                '^C',
                label: l10n.terminalKeyInterrupt,
                onPressed: () => onKey(TerminalKey.keyC, ctrl: true),
              ),
              _Key.icon(
                LucideIcons.arrowLeft,
                label: l10n.terminalKeyLeft,
                onPressed: () => onKey(TerminalKey.arrowLeft),
              ),
              _Key.icon(
                LucideIcons.arrowDown,
                label: l10n.terminalKeyDown,
                onPressed: () => onKey(TerminalKey.arrowDown),
              ),
              _Key.icon(
                LucideIcons.arrowUp,
                label: l10n.terminalKeyUp,
                onPressed: () => onKey(TerminalKey.arrowUp),
              ),
              _Key.icon(
                LucideIcons.arrowRight,
                label: l10n.terminalKeyRight,
                onPressed: () => onKey(TerminalKey.arrowRight),
              ),
              // 多行输入：Enter 提交、Shift+Enter 换行。软键盘同样按不出来。
              _Key.shifted(
                icon: LucideIcons.cornerDownLeft,
                label: l10n.terminalKeyNewLine,
                onPressed: () => onKey(TerminalKey.enter, shift: true),
              ),
              _Key.icon(
                LucideIcons.clipboardPaste,
                label: l10n.terminalPaste,
                onPressed: onPaste,
              ),
              for (final symbol in symbols)
                _Key.text(
                  symbol,
                  label: symbol,
                  onPressed: () => onText(symbol),
                ),
              _Key.text(
                'PgUp',
                label: l10n.terminalKeyPageUp,
                onPressed: () => onKey(TerminalKey.pageUp),
              ),
              _Key.text(
                'PgDn',
                label: l10n.terminalKeyPageDown,
                onPressed: () => onKey(TerminalKey.pageDown),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 一颗键：触控区 48×48 起，里面是一枚略小的键帽，键帽之间留出缝。按下键帽变暗一档，
/// 并给一下 selectionClick：快捷键是纯视觉按钮，没有实体键的落键感，不给触感用户
/// 无法确认按没按上。
class _Key extends StatefulWidget {
  const _Key.text(
    String this.text, {
    required this.label,
    required this.onPressed,
    this.latched,
  }) : icon = null,
       shift = false;

  const _Key.icon(
    IconData this.icon, {
    required this.label,
    required this.onPressed,
  }) : text = null,
       shift = false,
       latched = null;

  /// 带 Shift 的组合：键帽上先画一个 Shift 箭头。
  const _Key.shifted({
    this.text,
    this.icon,
    required this.label,
    required this.onPressed,
  }) : shift = true,
       latched = null;

  final String? text;
  final IconData? icon;
  final bool shift;
  final String label;

  /// 只有修饰键（Ctrl）有这个状态；null 表示普通键。
  final bool? latched;
  final VoidCallback onPressed;

  @override
  State<_Key> createState() => _KeyState();
}

class _KeyState extends State<_Key> {
  bool _down = false;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final latched = widget.latched ?? false;
    // 锁存的 Ctrl 反白成实心墨：不用琥珀，琥珀只给「需要你」。
    final fg = latched ? c.onSolid : c.ink;
    final cap = latched ? c.solid : (_down ? c.control : c.shelf);
    final glyphs = <Widget>[
      if (widget.shift)
        Icon(LucideIcons.arrowBigUp, size: Strokes.iconSmall, color: fg),
      if (widget.icon case final icon?)
        Icon(icon, size: Strokes.iconSmall, color: fg),
      if (widget.text case final text?)
        Text(text, style: t.monoTime.copyWith(color: fg)),
    ];
    return Semantics(
      button: true,
      toggled: widget.latched,
      label: widget.label,
      excludeSemantics: true,
      child: Tooltip(
        message: widget.label,
        excludeFromSemantics: true,
        child: InkWell(
          onTap: () {
            HapticFeedback.selectionClick();
            widget.onPressed();
          },
          onHighlightChanged: (down) => setState(() => _down = down),
          highlightColor: Colors.transparent,
          child: ConstrainedBox(
            constraints: const BoxConstraints(
              minWidth: Space.touch,
              minHeight: Space.touch,
            ),
            child: Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: Space.xxs,
                vertical: Space.xs,
              ),
              child: DecoratedBox(
                decoration: BoxDecoration(
                  color: cap,
                  border: Border.all(color: latched ? c.solid : c.hairline),
                  borderRadius: const BorderRadius.all(Radii.chip),
                ),
                child: Padding(
                  padding: const EdgeInsets.all(Space.sm),
                  child: Row(
                    mainAxisSize: MainAxisSize.min,
                    mainAxisAlignment: MainAxisAlignment.center,
                    children: glyphs,
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}
