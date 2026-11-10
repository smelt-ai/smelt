import 'dart:async';

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
    required this.keyboardEnabled,
    required this.onToggleKeyboard,
    required this.composing,
    required this.onToggleComposer,
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

  /// 软键盘开着。为空表示现在写不进去（没连上），两颗圆钮一起灰掉。
  final bool keyboardEnabled;
  final VoidCallback? onToggleKeyboard;

  /// 输入框开着：整段写好再发，输入法组字、改错都在原生输入框里做。
  final bool composing;
  final VoidCallback? onToggleComposer;

  /// 软键盘第二、三层里才有的符号，CLI 里又天天要打。
  static const symbols = ['/', '~', '|', '-', '`'];

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    // 键帽跟着系统字号放大，但封顶：栏子每高一点终端就少一行。
    return MediaQuery.withClampedTextScaling(
      maxScaleFactor: 1.6,
      child: Padding(
        padding: const EdgeInsets.fromLTRB(
          Space.sm,
          Space.xs,
          Space.sm,
          Space.xs,
        ),
        // 键盘、输入框两个开关放在拇指够得着的右下角，不放导航栏。
        child: Row(
          children: [
            Expanded(child: _buildKeys(context, l10n)),
            const SizedBox(width: Space.xs),
            _RoundToggle(
              icon: keyboardEnabled
                  ? LucideIcons.keyboardOff
                  : LucideIcons.keyboard,
              label: keyboardEnabled
                  ? l10n.terminalHideKeyboard
                  : l10n.terminalShowKeyboard,
              active: keyboardEnabled,
              onPressed: onToggleKeyboard,
            ),
            const SizedBox(width: Space.xs),
            _RoundToggle(
              icon: LucideIcons.squarePen,
              label: composing
                  ? l10n.terminalCloseComposer
                  : l10n.terminalOpenComposer,
              active: composing,
              onPressed: onToggleComposer,
            ),
          ],
        ),
      ),
    );
  }

  /// 快捷键收在一颗胶囊里横滑。
  Widget _buildKeys(BuildContext context, L10n l10n) {
    final c = GalleyColors.of(context);
    return DecoratedBox(
      decoration: BoxDecoration(
        color: c.shelf,
        borderRadius: const BorderRadius.all(Radii.pill),
        border: Border.all(color: c.hairline, width: Strokes.hairline),
      ),
      child: ClipRRect(
        borderRadius: const BorderRadius.all(Radii.pill),
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
              _ArrowPadKey(onKey: onKey),
              _Key.text(
                '^C',
                label: l10n.terminalKeyInterrupt,
                onPressed: () => onKey(TerminalKey.keyC, ctrl: true),
              ),
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

/// 栏子右侧的圆形开关。开着时反白成实心墨。
class _RoundToggle extends StatelessWidget {
  const _RoundToggle({
    required this.icon,
    required this.label,
    required this.active,
    required this.onPressed,
  });

  final IconData icon;
  final String label;
  final bool active;
  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final onPressed = this.onPressed;
    return Semantics(
      toggled: active,
      child: IconButton(
        tooltip: label,
        onPressed: onPressed == null
            ? null
            : () {
                HapticFeedback.selectionClick();
                onPressed();
              },
        style: IconButton.styleFrom(
          backgroundColor: active ? c.solid : c.shelf,
          foregroundColor: active ? c.onSolid : c.ink,
          disabledBackgroundColor: c.shelf,
          disabledForegroundColor: c.inkFaint,
          side: BorderSide(
            color: active ? c.solid : c.hairline,
            width: Strokes.hairline,
          ),
          fixedSize: const Size.square(Space.touch),
          padding: EdgeInsets.zero,
        ),
        icon: Icon(icon, size: Strokes.icon),
      ),
    );
  }
}

/// 一颗键：触控区 48×48 起，里面是一枚略小的键帽，键帽之间留出缝。按下键帽变暗一档，
/// 并给一下 selectionClick：快捷键是纯视觉按钮，没有实体键的落键感，不给触感用户
/// 无法确认按没按上。
const _keyMinWidth = 44.0;

class _Key extends StatefulWidget {
  const _Key.text(
    String this.text, {
    required this.label,
    required this.onPressed,
    this.latched,
  }) : icon = null,
       shift = false,
       open = null;

  const _Key.icon(
    IconData this.icon, {
    required this.label,
    required this.onPressed,
    this.open,
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
       latched = null,
       open = null;

  final String? text;
  final IconData? icon;
  final bool shift;
  final String label;

  /// 只有修饰键（Ctrl）有这个状态；null 表示普通键。
  final bool? latched;

  /// 弹出面板（方向盘）开着时键帽保持按下的样子。
  final bool? open;
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
    // 键帽平时透明，跟胶囊融成一片；按下才显出来。
    final cap = latched
        ? c.solid
        : (_down || (widget.open ?? false) ? c.control : Colors.transparent);
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
            // 宽度按 HIG 的 44pt 下限：键帽透明、彼此不用留缝，省出来的宽度让最常用的
            // 六颗（到 ^C 为止）在胶囊里一屏放得下。
            constraints: const BoxConstraints(
              minWidth: _keyMinWidth,
              minHeight: Space.touch,
            ),
            child: Padding(
              padding: const EdgeInsets.symmetric(vertical: Space.xs),
              child: DecoratedBox(
                decoration: BoxDecoration(
                  color: cap,
                  borderRadius: const BorderRadius.all(Radii.chip),
                ),
                child: Padding(
                  padding: const EdgeInsets.symmetric(
                    horizontal: Space.xs,
                    vertical: Space.sm,
                  ),
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

/// 方向键收成一颗：点开在键上方弹出十字方向盘，可以连着按，点面板外面收起。
/// 四颗方向键平铺会占掉快捷栏小半屏宽，而用到它们的时候往往要连按好几下。
class _ArrowPadKey extends StatefulWidget {
  const _ArrowPadKey({required this.onKey});

  final TerminalKeyHandler onKey;

  @override
  State<_ArrowPadKey> createState() => _ArrowPadKeyState();
}

class _ArrowPadKeyState extends State<_ArrowPadKey> {
  final _portal = OverlayPortalController();

  /// 打开那一刻键在屏幕上的位置；面板打开期间遮罩挡住快捷栏，键不会再滚走。
  Rect? _anchor;

  void _toggle() {
    if (_portal.isShowing) {
      _close();
      return;
    }
    final box = context.findRenderObject()! as RenderBox;
    final origin = box.localToGlobal(Offset.zero);
    setState(() => _anchor = origin & box.size);
    _portal.show();
  }

  void _close() {
    _portal.hide();
    setState(() => _anchor = null);
  }

  @override
  Widget build(BuildContext context) {
    final l10n = L10n.of(context);
    return OverlayPortal(
      controller: _portal,
      overlayChildBuilder: _buildPad,
      child: _Key.icon(
        LucideIcons.move,
        label: l10n.terminalKeyArrows,
        open: _anchor != null,
        onPressed: _toggle,
      ),
    );
  }

  Widget _buildPad(BuildContext context) {
    final anchor = _anchor;
    if (anchor == null) return const SizedBox.shrink();
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    final screen = MediaQuery.sizeOf(context);
    const buttonWidth = 64.0;
    const button = Size(buttonWidth, Space.touch);
    const gap = Space.xs;
    const padWidth = buttonWidth * 3 + gap * 2 + Space.sm * 2;
    final left = (anchor.center.dx - padWidth / 2).clamp(
      Space.sm,
      screen.width - padWidth - Space.sm,
    );
    Widget arrow(IconData icon, String label, TerminalKey key) => _RepeatButton(
      icon: icon,
      label: label,
      size: button,
      onPressed: () => widget.onKey(key),
    );
    return Stack(
      children: [
        Positioned.fill(
          child: GestureDetector(
            behavior: HitTestBehavior.opaque,
            onTap: _close,
          ),
        ),
        Positioned(
          left: left,
          bottom: screen.height - anchor.top + Space.xxs,
          child: Material(
            color: c.shelf,
            elevation: 8,
            shadowColor: Colors.black26,
            shape: RoundedRectangleBorder(
              borderRadius: const BorderRadius.all(Radii.sheet),
              side: BorderSide(color: c.hairline, width: Strokes.hairline),
            ),
            child: Padding(
              padding: const EdgeInsets.all(Space.sm),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  arrow(
                    LucideIcons.chevronUp,
                    l10n.terminalKeyUp,
                    TerminalKey.arrowUp,
                  ),
                  const SizedBox(height: gap),
                  Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      arrow(
                        LucideIcons.chevronLeft,
                        l10n.terminalKeyLeft,
                        TerminalKey.arrowLeft,
                      ),
                      const SizedBox(width: gap),
                      arrow(
                        LucideIcons.chevronDown,
                        l10n.terminalKeyDown,
                        TerminalKey.arrowDown,
                      ),
                      const SizedBox(width: gap),
                      arrow(
                        LucideIcons.chevronRight,
                        l10n.terminalKeyRight,
                        TerminalKey.arrowRight,
                      ),
                    ],
                  ),
                ],
              ),
            ),
          ),
        ),
      ],
    );
  }
}

/// 方向盘上的一颗键：点一下发一次，按住先停一拍再连发，松手即停。
class _RepeatButton extends StatefulWidget {
  const _RepeatButton({
    required this.icon,
    required this.label,
    required this.size,
    required this.onPressed,
  });

  final IconData icon;
  final String label;
  final Size size;
  final VoidCallback onPressed;

  @override
  State<_RepeatButton> createState() => _RepeatButtonState();
}

class _RepeatButtonState extends State<_RepeatButton> {
  static const _repeatDelay = Duration(milliseconds: 400);
  static const _repeatInterval = Duration(milliseconds: 70);

  Timer? _timer;
  bool _down = false;

  void _fire() {
    HapticFeedback.selectionClick();
    widget.onPressed();
  }

  void _start() {
    setState(() => _down = true);
    _fire();
    // 只在手指按住期间跑，松手、取消、销毁都会停。
    _timer = Timer(_repeatDelay, () {
      _timer = Timer.periodic(_repeatInterval, (_) => _fire());
    });
  }

  void _stop() {
    _timer?.cancel();
    _timer = null;
    if (mounted && _down) setState(() => _down = false);
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    return Semantics(
      button: true,
      label: widget.label,
      excludeSemantics: true,
      onTap: widget.onPressed,
      child: Listener(
        onPointerDown: (_) => _start(),
        onPointerUp: (_) => _stop(),
        onPointerCancel: (_) => _stop(),
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 80),
          width: widget.size.width,
          height: widget.size.height,
          decoration: BoxDecoration(
            color: _down ? c.shelfPressed : c.control,
            borderRadius: const BorderRadius.all(Radii.pill),
          ),
          child: Icon(widget.icon, size: Strokes.icon, color: c.ink),
        ),
      ),
    );
  }
}
