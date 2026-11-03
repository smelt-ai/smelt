/// 项目文件页的演示数据（**合成**：目录、文件内容都是编的，只用来看界面长什么样）。
///
/// 每个项目下都挂同一棵小树，覆盖文本、Markdown、图片、二进制、过大五种预览。
library;

import 'dart:convert';
import 'dart:typed_data';

import '../models.dart';

/// 相对项目根的路径 → 内容。目录由路径推出来。
final Map<String, Object> _tree = {
  'README.md':
      '# Demo project\n\n'
      'A **synthetic** project for screenshots.\n\n'
      '- Rust core in `src/`\n'
      '- Docs in [docs/guide.md](docs/guide.md)\n',
  'Cargo.toml':
      '[package]\nname = "demo"\nversion = "0.1.0"\nedition = "2024"\n',
  'src/main.rs':
      '//! Entry point.\n\n'
      'use std::env;\n\n'
      'fn main() {\n'
      '    let name = env::args().nth(1).unwrap_or_else(|| "world".into());\n'
      '    println!("hello, {name}");\n'
      '}\n',
  'src/lib.rs': List.generate(
    400,
    (ix) => 'pub fn step_$ix(x: u32) -> u32 { x + $ix }',
  ).join('\n'),
  'docs/guide.md': '# Guide\n\nRun `cargo run -- you`.\n',
  'assets/logo.svg':
      '<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64">'
      '<circle cx="32" cy="32" r="28" fill="#3B7DD8"/></svg>',
  'assets/icon.bin': Uint8List.fromList(const [0, 1, 2, 3]),
  'logs/build.log': const _TooLarge(12 * 1024 * 1024),
};

class _TooLarge {
  const _TooLarge(this.size);
  final int size;
}

String _relative(String root, String path) =>
    path == root ? '' : path.substring(root.length + 1);

List<FileEntry> demoProjectDir(String root, String dir) {
  final rel = _relative(root, dir);
  final prefix = rel.isEmpty ? '' : '$rel/';
  final dirs = <String>{};
  final files = <String>{};
  for (final path in _tree.keys) {
    if (!path.startsWith(prefix)) continue;
    final rest = path.substring(prefix.length);
    final cut = rest.indexOf('/');
    if (cut < 0) {
      files.add(rest);
    } else {
      dirs.add(rest.substring(0, cut));
    }
  }
  return [
    for (final name in dirs.toList()..sort())
      FileEntry(name: name, path: '$dir/$name', isDir: true),
    for (final name in files.toList()..sort())
      FileEntry(name: name, path: '$dir/$name', isDir: false),
  ];
}

FileSearch demoProjectSearch(String root, String query) {
  final q = query.trim().toLowerCase();
  if (q.isEmpty) return const FileSearch(hits: [], truncated: false);
  final hits = <FileSearchHit>[];
  _tree.forEach((rel, content) {
    final path = '$root/$rel';
    if (rel.toLowerCase().contains(q)) {
      hits.add(FileSearchHit(path: path, rel: rel));
    }
    if (content is String) {
      final lines = content.split('\n');
      for (var ix = 0; ix < lines.length; ix++) {
        if (lines[ix].toLowerCase().contains(q)) {
          hits.add(
            FileSearchHit(
              path: path,
              rel: rel,
              line: ix + 1,
              preview: lines[ix].trim(),
            ),
          );
        }
      }
    }
  });
  return FileSearch(hits: hits, truncated: false);
}

/// 不在树里的路径（对话里提到的别处文件）当成一段说明文字。
OpenedFile demoOpenFile(String path) {
  final entry = _tree.entries
      .where((e) => path.endsWith('/${e.key}'))
      .map((e) => e.value)
      .firstOrNull;
  return switch (entry) {
    final String text when path.endsWith('.svg') => OpenedImage(
      Uint8List.fromList(utf8.encode(text)),
      svg: true,
    ),
    final String text when path.endsWith('.rs') => OpenedText(
      _DemoRustSource(text),
    ),
    final String text => OpenedText(PlainTextSource(text)),
    Uint8List() => const OpenedBinary(),
    _TooLarge(:final size) => OpenedTooLarge(
      size: size,
      limit: 8 * 1024 * 1024,
    ),
    _ => OpenedText(PlainTextSource('// $path\n')),
  };
}

/// 演示里给 Rust 文件粗略上色，截图时看得到高亮的样子。真机上的高亮在 Rust 内核里。
class _DemoRustSource extends PlainTextSource {
  _DemoRustSource(super.text) : super(language: 'Rust');

  static final _token = RegExp(
    r'(//.*$)|("(?:[^"\\]|\\.)*")|\b(fn|let|use|pub|impl|struct|mod)\b|\b(\d+)\b|\b([a-z_]+)(?=\()',
  );

  @override
  bool get highlighted => true;

  @override
  Future<List<HighlightedLine>> lines(int start, int end) async => [
    for (final line in await super.lines(start, end))
      HighlightedLine(number: line.number, spans: _color(_text(line))),
  ];

  static String _text(HighlightedLine line) =>
      line.spans.map((span) => span.text).join();

  static List<TokenSpan> _color(String text) {
    final spans = <TokenSpan>[];
    var at = 0;
    for (final m in _token.allMatches(text)) {
      if (m.start > at) {
        spans.add(
          TokenSpan(text: text.substring(at, m.start), kind: TokenKind.plain),
        );
      }
      final kind = m.group(1) != null
          ? TokenKind.comment
          : m.group(2) != null
          ? TokenKind.string
          : m.group(3) != null
          ? TokenKind.keyword
          : m.group(4) != null
          ? TokenKind.number
          : TokenKind.function;
      spans.add(TokenSpan(text: m.group(0)!, kind: kind));
      at = m.end;
    }
    if (at < text.length) {
      spans.add(TokenSpan(text: text.substring(at), kind: TokenKind.plain));
    }
    return spans;
  }
}

/// 演示里的链接解析：只认绝对路径和相对 `cwd` 的路径，`:行号` 结尾的拆出行号。
FileLinkTarget? demoResolveFileLink(String? cwd, String target) {
  final t = target.trim();
  if (t.isEmpty ||
      t.startsWith('#') ||
      t.contains('://') ||
      t.startsWith('~')) {
    return null;
  }
  final match = RegExp(
    r'^(.*?)(?::(\d+))?(?::\d+)?$',
  ).firstMatch(t.split('#').first);
  final path = match?.group(1) ?? t;
  final line = int.tryParse(match?.group(2) ?? '');
  if (path.startsWith('/')) return FileLinkTarget(path: path, line: line);
  if (cwd == null) return null;
  return FileLinkTarget(path: '$cwd/$path', line: line);
}
