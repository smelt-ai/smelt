/// 对话页的纯计算：条目分块、diff 按行对齐、几处数字的说法。
///
/// 都是确定的纯函数，结果缓存在页面状态上（快照到达时算一次），`build` 只取缓存。
library;

import 'package:diff_match_patch/diff_match_patch.dart' as dmp;

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

/// 把快照里的条目分块。连续的工具调用（中间只隔着空条目）收成一组。
List<ConversationBlock> buildBlocks(ConversationView view) {
  final blocks = <ConversationBlock>[];
  List<ToolCallView>? group;
  var groupStart = 0;
  void flush() {
    if (group != null) blocks.add(ToolGroupBlock(group!, groupStart));
    group = null;
  }

  for (final (i, entry) in view.entries.indexed) {
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
    blocks.add(EntryBlock(entry, isFinal: view.finalAnswerIndex == i));
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

/// 工具输出常被 agent 包在一对代码围栏里，界面已经是等宽块，去掉围栏。
String stripCodeFence(String text) {
  final trimmed = text.trim();
  if (!trimmed.startsWith('```') || !trimmed.endsWith('```')) return text;
  final newline = trimmed.indexOf('\n');
  if (newline < 0) return text;
  return trimmed.substring(newline + 1, trimmed.length - 3).trimRight();
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

enum DiffOp { equal, insert, delete }

class DiffLine {
  const DiffLine(this.op, this.text);

  final DiffOp op;
  final String text;
}

/// 按行对齐的 diff：先把每个不同的行映射成一个字符，在字符串上做 diff 再还原成行
/// （diff_match_patch 的行模式，它没把这一步导出）。逐字比对会把一行拆成碎片，手机窄屏
/// 上读不出来改了什么。
List<DiffLine> lineDiff(String? oldText, String newText) {
  final before = _lines(oldText ?? '');
  final after = _lines(newText);
  if (oldText == null) {
    return [for (final l in after) DiffLine(DiffOp.insert, l)];
  }
  final table = <String, int>{};
  final lines = <String>[];
  String encode(List<String> source) {
    final codes = <int>[];
    for (final line in source) {
      final code = table.putIfAbsent(line, () {
        lines.add(line);
        return _codeFor(lines.length - 1);
      });
      codes.add(code);
    }
    return String.fromCharCodes(codes);
  }

  // 不同的行太多编不进单个 UTF-16 码元时（极少见），退到「全删全加」，不画错的对齐。
  if (before.toSet().union(after.toSet()).length > _maxCodes) {
    return [
      for (final l in before) DiffLine(DiffOp.delete, l),
      for (final l in after) DiffLine(DiffOp.insert, l),
    ];
  }
  final a = encode(before);
  final b = encode(after);
  final diffs = dmp.diff(a, b, timeout: 0.2, checklines: false);
  final out = <DiffLine>[];
  for (final d in diffs) {
    final op = switch (d.operation) {
      dmp.DIFF_INSERT => DiffOp.insert,
      dmp.DIFF_DELETE => DiffOp.delete,
      _ => DiffOp.equal,
    };
    for (final unit in d.text.codeUnits) {
      out.add(DiffLine(op, lines[_indexFor(unit)]));
    }
  }
  return out;
}

/// 码元从 1 开始（0 是 NUL，调试器不喜欢），跳过代理对区间。
const _surrogateStart = 0xD800;
const _surrogateSize = 0x800;
const _maxCodes = 0xFFFF - _surrogateSize - 1;

int _codeFor(int index) {
  final code = index + 1;
  return code >= _surrogateStart ? code + _surrogateSize : code;
}

int _indexFor(int code) =>
    (code >= _surrogateStart + _surrogateSize ? code - _surrogateSize : code) -
    1;

List<String> _lines(String text) {
  if (text.isEmpty) return const [];
  final parts = text.split('\n');
  if (parts.last.isEmpty) parts.removeLast();
  return parts;
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
