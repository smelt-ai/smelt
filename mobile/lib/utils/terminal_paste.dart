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
