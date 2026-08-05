import 'package:flutter/foundation.dart';
import 'package:xterm/xterm.dart';

/// A [Terminal] that works around three bugs in xterm 4.0.0.
///
/// **Detached lines left behind by a scroll region.** `Buffer.scrollUp` shifts
/// a scroll region with `lines[i] = lines[i + n]`, and that setter detaches
/// whatever occupied the destination slot before adopting the new line. The
/// line just written to slot `i` is still sitting in slot `i + n`, so the
/// iteration that later overwrites slot `i + n` detaches the very line it
/// already moved down. Every such line stays in the ring while reporting
/// `attached == false`; `Buffer.scrollDown` has the mirror image of the bug.
///
/// Nothing reads that flag until a later line feed takes the other branch of
/// `Buffer.index` - a scroll region whose top margin is zero uses
/// `lines.insert`, which shifts through `_moveChild` and asserts on `attached`
/// (and dereferences a null owner once asserts are compiled out). The
/// exception escapes `Terminal.write`, so the rest of that repaint is never
/// applied and the view stays frozen on a half-drawn older frame.
///
/// Re-adopting a line at its own index restores the owner and the index the
/// buffer already intended it to have, so the repair is just a self-assignment.
///
/// **Reflow reading through a stale rotation.** `IndexAwareCircularBuffer
/// .replaceWith` - the only writer used by the reflow that a width change
/// triggers - stores the replacement lines at slots derived from the ring's
/// *current* rotation offset and only afterwards resets that offset to zero.
/// Every later read therefore lands `offset` slots away from the line it asked
/// for, resurrecting lines that had already been trimmed away.
///
/// The ring is rotated as soon as the front of the scrollback is trimmed, which
/// `ESC[3J` (clear scrollback, emitted by TUIs on a full repaint) and a
/// scrollback overflow both do. `replaceWith`'s arithmetic is correct for a
/// rotation offset of zero, and the public `maxLength` setter rebuilds the
/// backing array by reading through the current rotation before zeroing the
/// offset - so round-tripping it normalises the ring without touching privates.
///
/// **Mouse wheel reported as a modified button press.** `TerminalMouseButton`
/// declares `wheelUp(id: 64 + 4)` and `wheelDown(id: 64 + 5)`. In the X11
/// encoding every terminal speaks, the low two bits of a button code select the
/// button and bit 6 (64) marks it as a wheel, so wheel up is 64 and wheel down
/// is 65; bit 2 (4) is the shift modifier. xterm therefore reports a wheel tick
/// as shift-click, which a full screen application either ignores or mistakes
/// for a selection gesture. That is the only way the alternate screen can be
/// scrolled - it has no scrollback of its own - so wheel scrolling silently
/// does nothing in every TUI.
class SafeTerminal extends Terminal {
  SafeTerminal({
    super.maxLines,
    super.onBell,
    super.onTitleChange,
    super.onIconChange,
    super.onOutput,
    super.onResize,
    super.platform,
    super.inputHandler,
    TerminalMouseHandler mouseHandler = defaultMouseHandler,
    super.onPrivateOSC,
    super.reflowEnabled,
    super.wordSeparators,
  }) : super(mouseHandler: WheelEncodingFix(mouseHandler));

  @override
  void write(String data) {
    super.write(data);
    reattachDetachedLines(buffer);
  }

  @override
  void resize(
    int newWidth,
    int newHeight, [
    int? pixelWidth,
    int? pixelHeight,
  ]) {
    // A detached line would survive the reflow and keep poisoning the next
    // insert, so clear both defects around the rebuild.
    reattachDetachedLines(mainBuffer);
    reattachDetachedLines(altBuffer);
    normalizeScrollbackRotation(mainBuffer);
    normalizeScrollbackRotation(altBuffer);
    super.resize(newWidth, newHeight, pixelWidth, pixelHeight);
    reattachDetachedLines(mainBuffer);
    reattachDetachedLines(altBuffer);
  }

  /// Re-adopts every line [buffer] still holds but has marked as detached.
  ///
  /// Returns the number of lines that had to be repaired.
  @visibleForTesting
  static int reattachDetachedLines(Buffer buffer) {
    final lines = buffer.lines;
    var repaired = 0;
    for (var i = 0; i < lines.length; i++) {
      final line = lines[i];
      if (line.attached) continue;
      lines[i] = line;
      repaired++;
    }
    return repaired;
  }

  /// Rewinds [buffer]'s ring rotation to zero, preserving line contents and
  /// their indices.
  @visibleForTesting
  static void normalizeScrollbackRotation(Buffer buffer) {
    final lines = buffer.lines;
    final capacity = lines.maxLength;
    // Growing first guarantees the rebuild never has to drop lines, which the
    // setter does not account for.
    lines.maxLength = capacity + 1;
    lines.maxLength = capacity;
  }
}

/// Corrects the button code xterm 4.0.0 reports for mouse wheel ticks.
///
/// Wraps another [TerminalMouseHandler] and rewrites the button code of the
/// sequence it produced, in whichever encoding the application asked for.
class WheelEncodingFix implements TerminalMouseHandler {
  const WheelEncodingFix(this.inner);

  final TerminalMouseHandler inner;

  static const _correctedIds = {
    TerminalMouseButton.wheelUp: 64,
    TerminalMouseButton.wheelDown: 65,
  };

  @override
  String? call(TerminalMouseEvent event) {
    final report = inner(event);
    final corrected = _correctedIds[event.button];
    if (report == null || corrected == null) return report;
    return _withButtonId(report, event.button.id, corrected);
  }

  static String _withButtonId(String report, int reported, int corrected) {
    // SGR: ESC [ < id ; col ; row (M|m)
    if (report.startsWith('\x1b[<')) {
      return report.replaceFirst('\x1b[<$reported;', '\x1b[<$corrected;');
    }
    // Normal and UTF-8: ESC [ M <id+32> <col+32> <row+32>
    if (report.startsWith('\x1b[M') && report.length > 3) {
      return '\x1b[M'
          '${String.fromCharCode(32 + corrected)}'
          '${report.substring(4)}';
    }
    // urxvt: ESC [ <id+32> ; col ; row M
    return report.replaceFirst(
      '\x1b[${32 + reported};',
      '\x1b[${32 + corrected};',
    );
  }
}
