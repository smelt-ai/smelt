/// 对话页的纯计算：条目分块、diff 折叠、几处数字的说法。diff 的逐行对齐由内核算好
/// （`DiffView.lines`，跟桌面同一个算法），这里只管怎么摆。
///
/// 都是确定的纯函数，结果缓存在页面状态上（快照到达时算一次），`build` 只取缓存。
library;

import '../../data/models.dart';

/// 对话里的一块：一条条目，或连续几次工具调用收成的一组。
sealed class ConversationBlock {
  const ConversationBlock();

  /// 跨快照稳定的键：取块里第一条的全局下标。新工具调用接在组尾时键不变，展开状态
  /// 跟着组走。
  String get key;
}

class EntryBlock extends ConversationBlock {
  const EntryBlock(this.entry, {required this.isFinal});

  final ConversationEntry entry;

  /// 回合结束后的「最终回答」。
  final bool isFinal;

  @override
  String get key => 'e${entry.index}';
}

class ToolGroupBlock extends ConversationBlock {
  const ToolGroupBlock(this.calls, this.firstIndex);

  final List<ToolCallView> calls;
  final int firstIndex;

  @override
  String get key => 't$firstIndex';
}

/// 画出来什么都没有的条目：空正文、没带工具调用的工具条目。它们夹在两次工具调用之间
/// 时不该把一组拆开。
bool _invisible(ConversationEntry e) => switch (e.kind) {
  EntryKind.assistant => e.text.trim().isEmpty,
  EntryKind.toolCall => e.toolCall == null,
  _ => false,
};

/// 视图只带从 `entriesFrom` 起的条目，前面的沿用手里的窗口：截到那里再接上。视图带着
/// 整个窗口（`entriesFrom == entriesOffset`），或手里的窗口起点对不上时，直接用视图里的。
List<ConversationEntry> mergeEntries(
  List<ConversationEntry> current,
  int? currentOffset,
  ConversationView view,
) {
  final keep = view.entriesFrom - view.entriesOffset;
  assert(
    keep <= 0 || currentOffset == view.entriesOffset,
    '视图只带后面一段，手里的窗口起点却对不上',
  );
  if (keep <= 0 || currentOffset != view.entriesOffset) return view.entries;
  assert(keep <= current.length, '视图从 ${view.entriesFrom} 起，手里的窗口接不上');
  return [...current.take(keep), ...view.entries];
}

/// 把窗口里的条目分块。连续的工具调用（中间只隔着空条目）收成一组。
/// `finalAnswerIndex` 是「最终回答」在窗口里的下标。
List<ConversationBlock> buildBlocks(
  List<ConversationEntry> entries, {
  int? finalAnswerIndex,
}) {
  final blocks = <ConversationBlock>[];
  List<ToolCallView>? group;
  var groupStart = 0;
  void flush() {
    if (group != null) blocks.add(ToolGroupBlock(group!, groupStart));
    group = null;
  }

  for (final (i, entry) in entries.indexed) {
    if (_invisible(entry)) continue;
    final call = entry.toolCall;
    if (entry.kind == EntryKind.toolCall && call != null) {
      if (group == null) {
        group = [];
        groupStart = entry.index;
      }
      group!.add(call);
      continue;
    }
    flush();
    blocks.add(EntryBlock(entry, isFinal: finalAnswerIndex == i));
  }
  flush();
  return blocks;
}

/// 一组工具调用合起来的状态：有失败就是失败，有没做完的就是在做，否则完成。
ToolStatus groupStatus(List<ToolCallView> calls) {
  if (calls.any((c) => c.status == ToolStatus.failed)) return ToolStatus.failed;
  if (calls.any((c) => c.status == ToolStatus.inProgress)) {
    return ToolStatus.inProgress;
  }
  if (calls.any((c) => c.status == ToolStatus.pending)) {
    return ToolStatus.pending;
  }
  return ToolStatus.completed;
}

/// 「84k」「1.2M」：上下文用量那一行用，数字小就原样。
String compactCount(int n) {
  if (n < 1000) return '$n';
  if (n < 1000000) {
    final k = n / 1000;
    return '${k < 10 ? k.toStringAsFixed(1) : k.round()}k';
  }
  final m = n / 1000000;
  return '${m < 10 ? m.toStringAsFixed(1) : m.round()}M';
}

/// 回合耗时：不到一分钟写秒（`8.4s`），否则 `1:42` / `1:03:20`。
String formatTurnDuration(int milliseconds) {
  if (milliseconds < 1000) return '${milliseconds}ms';
  final d = Duration(milliseconds: milliseconds);
  if (d.inSeconds < 60) {
    final s = milliseconds / 1000;
    return '${s.toStringAsFixed(s < 10 ? 1 : 0)}s';
  }
  final h = d.inHours;
  final m = d.inMinutes.remainder(60);
  final s = d.inSeconds.remainder(60).toString().padLeft(2, '0');
  return h > 0 ? '$h:${m.toString().padLeft(2, '0')}:$s' : '$m:$s';
}

/// diff 里新增 / 删除了几行。
(int, int) diffTotals(List<DiffLine> lines) {
  var added = 0;
  var removed = 0;
  for (final l in lines) {
    if (l.op == DiffOp.insert) added++;
    if (l.op == DiffOp.delete) removed++;
  }
  return (added, removed);
}

/// diff 里要画的一段：改动行连同前后 [context] 行；中间长的未改动段收成一行说明。
sealed class DiffHunkLine {
  const DiffHunkLine();
}

class DiffShown extends DiffHunkLine {
  const DiffShown(this.line);
  final DiffLine line;
}

class DiffSkipped extends DiffHunkLine {
  const DiffSkipped(this.count);
  final int count;
}

List<DiffHunkLine> foldUnchanged(List<DiffLine> lines, {int context = 3}) {
  final keep = List<bool>.filled(lines.length, false);
  for (var i = 0; i < lines.length; i++) {
    if (lines[i].op == DiffOp.equal) continue;
    for (
      var j = (i - context).clamp(0, lines.length);
      j <= (i + context).clamp(0, lines.length - 1);
      j++
    ) {
      keep[j] = true;
    }
  }
  final out = <DiffHunkLine>[];
  var skipped = 0;
  for (var i = 0; i < lines.length; i++) {
    if (keep[i]) {
      if (skipped > 0) out.add(DiffSkipped(skipped));
      skipped = 0;
      out.add(DiffShown(lines[i]));
    } else {
      skipped++;
    }
  }
  if (skipped > 0) out.add(DiffSkipped(skipped));
  return out;
}

/// 列表滚到底了没有。对话列表是倒过来的（最新在偏移 0），离 0 不远就算在底部。
bool isNearLatest(
  double pixels,
  double minScrollExtent, {
  double tolerance = 48,
}) => (pixels - minScrollExtent).abs() <= tolerance;
