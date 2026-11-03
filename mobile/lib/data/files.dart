/// 打开着的项目文件。界面只认这里的类型：生产实现包着 Rust 的不透明文档，演示和测试用
/// 纯 Dart 的实现，不加载 Rust 动态库。
library;

import 'dart:typed_data';

import '../src/rust/api/files.dart';

/// 一份打开着的文本：正文在 Rust 内核里，按行取，带语法高亮的类别。
abstract interface class TextSource {
  int get lineCount;

  /// 最长一行有几个字。不换行显示时据此定横向宽度。
  int get maxLineChars;

  /// 认出来的语言名；认不出是 null。
  String? get language;

  /// 会不会上色。文件太大、有超长行时即使认出了语言也不上色。
  bool get highlighted;

  /// `[start, end)` 行，0 基。
  Future<List<HighlightedLine>> lines(int start, int end);

  /// 全文：Markdown 渲染、复制全文用。
  Future<String> fullText();

  /// 不再用了：释放内核里的正文和解析状态。
  void dispose();
}

/// 打开一个文件的结果。
sealed class OpenedFile {
  const OpenedFile();
}

final class OpenedText extends OpenedFile {
  const OpenedText(this.text);
  final TextSource text;
}

final class OpenedImage extends OpenedFile {
  const OpenedImage(this.bytes, {required this.svg});
  final Uint8List bytes;
  final bool svg;
}

/// 不是文本，也不是认得的图片。
final class OpenedBinary extends OpenedFile {
  const OpenedBinary();
}

/// 超过运行时肯读的大小。
final class OpenedTooLarge extends OpenedFile {
  const OpenedTooLarge({required this.size, required this.limit});
  final int size;
  final int limit;
}

/// 内核的 [FilePreview] 转成界面用的类型。
OpenedFile openedFromPreview(FilePreview preview) => switch (preview.kind) {
  FilePreviewKind.text => OpenedText(_RustText(preview.document!)),
  FilePreviewKind.image => OpenedImage(
    preview.image ?? Uint8List(0),
    svg: preview.svg,
  ),
  FilePreviewKind.binary => const OpenedBinary(),
  FilePreviewKind.tooLarge => OpenedTooLarge(
    size: preview.size?.toInt() ?? 0,
    limit: preview.limit?.toInt() ?? 0,
  ),
};

class _RustText implements TextSource {
  _RustText(this._document);

  final TextDocument _document;

  @override
  int get lineCount => _document.lineCount;

  @override
  int get maxLineChars => _document.maxLineChars;

  @override
  String? get language => _document.language;

  @override
  bool get highlighted => _document.highlighted;

  @override
  Future<List<HighlightedLine>> lines(int start, int end) =>
      _document.lines(start: start, end: end);

  @override
  Future<String> fullText() => _document.fullText();

  @override
  void dispose() => _document.dispose();
}

/// 纯 Dart 的文本：演示和测试用，不上色。
class PlainTextSource implements TextSource {
  PlainTextSource(String text, {this.language})
    : _lines = text.endsWith('\n')
          ? text.substring(0, text.length - 1).split('\n')
          : text.split('\n'),
      _text = text;

  final String _text;
  final List<String> _lines;

  @override
  final String? language;

  @override
  int get lineCount => _lines.length;

  @override
  int get maxLineChars => _lines.fold(
    0,
    (longest, line) => line.length > longest ? line.length : longest,
  );

  @override
  bool get highlighted => false;

  @override
  Future<List<HighlightedLine>> lines(int start, int end) async => [
    for (var ix = start; ix < end && ix < _lines.length; ix++)
      HighlightedLine(
        number: ix + 1,
        spans: _lines[ix].isEmpty
            ? const []
            : [TokenSpan(text: _lines[ix], kind: TokenKind.plain)],
      ),
  ];

  @override
  Future<String> fullText() async => _text;

  @override
  void dispose() {}
}
