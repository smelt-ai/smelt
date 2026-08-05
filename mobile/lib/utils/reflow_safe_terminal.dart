import 'package:flutter/foundation.dart';
import 'package:xterm/xterm.dart';

/// A [Terminal] that works around a scrollback corruption bug in xterm 4.0.0.
///
/// `IndexAwareCircularBuffer.replaceWith` - the only writer used by the reflow
/// that a width change triggers - stores the replacement lines at slots derived
/// from the ring's *current* rotation offset and only afterwards resets that
/// offset to zero. Every later read therefore lands `offset` slots away from the
/// line it asked for.
///
/// The ring is rotated as soon as the front of the scrollback is trimmed, which
/// `ESC[3J` (clear scrollback, emitted by TUIs on a full repaint) and a
/// scrollback overflow both do. A mobile attach replays a daemon snapshot that
/// begins with `ESC[3J` and is followed by a viewport-driven resize, so the
/// first open reads back lines that were already dropped - the terminal shows
/// stale content, and because the line count collapses to the visible rows the
/// view reports no scroll extent either.
///
/// Reflow only runs when the width changes, and `replaceWith`'s arithmetic is
/// correct for a rotation offset of zero. Normalising the offset right before
/// each resize is therefore enough. The public `maxLength` setter rebuilds the
/// backing array by reading through the current rotation and then zeroes the
/// offset, so round-tripping it normalises the ring without touching privates.
class ReflowSafeTerminal extends Terminal {
  ReflowSafeTerminal({
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
  void resize(
    int newWidth,
    int newHeight, [
    int? pixelWidth,
    int? pixelHeight,
  ]) {
    normalizeScrollbackRotation(mainBuffer);
    normalizeScrollbackRotation(altBuffer);
    super.resize(newWidth, newHeight, pixelWidth, pixelHeight);
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
