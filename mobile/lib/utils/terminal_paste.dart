/// 把剪贴板文本编成写给 PTY 的粘贴字节，和桌面 `encode_paste` 同一套规则。
///
/// 对端开了 bracketed paste 时原样包进 `ESC[200~` / `ESC[201~`，并剥掉内容里的
/// ESC，避免粘贴的转义序列被 TUI 当成控制命令。没开时把换行收成 CR，普通
/// shell 才会把多行粘贴当成逐行输入。
String encodeTerminalPaste(String text, {required bool bracketed}) {
  if (bracketed) {
    return '\x1b[200~${text.replaceAll('\x1b', '')}\x1b[201~';
  }
  return text.replaceAll('\r\n', '\r').replaceAll('\n', '\r');
}

final _posixSafePathPattern = RegExp(r'^[A-Za-z0-9_@%+=:,./-]+$');
final _windowsSafePathPattern = RegExp(r'^[A-Za-z0-9_@%+=:,./\\-]+$');

/// 按运行时的 shell 规矩引用路径，和桌面 `dropped_paths_text` 同一套规则。
String quoteTerminalPath(String path, {bool windows = false}) {
  if (windows) {
    if (path.isNotEmpty && _windowsSafePathPattern.hasMatch(path)) {
      return path;
    }
    return '"${path.replaceAll('"', '\\"')}"';
  }
  if (path.isNotEmpty && _posixSafePathPattern.hasMatch(path)) {
    return path;
  }
  return "'${path.replaceAll("'", "'\\''")}'";
}

/// 把已落盘到运行时的图片路径和可选正文编成独立发送段：
/// 1. 每张图一个独立 paste 段（带末尾空格，便于直接识别为图片附件）；
/// 2. 有非空白正文时跟一段 paste；
/// 3. 末尾单独一段 `\r` 提交。
List<String> encodeTerminalImageSubmission({
  required List<String> paths,
  required String text,
  required bool bracketed,
  bool windows = false,
}) {
  final segments = <String>[
    for (final path in paths)
      encodeTerminalPaste(
        '${quoteTerminalPath(path, windows: windows)} ',
        bracketed: bracketed,
      ),
  ];
  if (text.trim().isNotEmpty) {
    segments.add(encodeTerminalPaste(text, bracketed: bracketed));
  }
  segments.add('\r');
  return segments;
}
