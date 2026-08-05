import 'package:flutter/foundation.dart';
import 'package:xterm/xterm.dart';

/// A [Terminal] that works around two buffer corruption bugs in xterm 4.0.0.
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
    super.mouseHandler,
    super.onPrivateOSC,
    super.reflowEnabled,
    super.wordSeparators,
  });

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
