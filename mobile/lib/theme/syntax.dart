/// 代码高亮的颜色：文件预览按语义类别上色（注释、字符串、关键字……），不按语言。
///
/// 类别由 Rust 内核解析出来（见 `crates/smelt-mobile/src/highlight.rs`），这里只管每类
/// 在亮 / 暗两套主题下是什么颜色。普通文字、标点用 [GalleyColors] 的墨色，不另设。
library;

import 'package:flutter/material.dart';

import 'primitives.dart';

@immutable
class SyntaxColors extends ThemeExtension<SyntaxColors> {
  const SyntaxColors({
    required this.comment,
    required this.string,
    required this.number,
    required this.keyword,
    required this.typeName,
    required this.function,
    required this.tag,
    required this.link,
    required this.inserted,
    required this.deleted,
  });

  final Color comment;
  final Color string;

  /// 数字和其它常量。
  final Color number;

  /// 关键字和运算符。
  final Color keyword;

  /// 类型名，也给参数用。
  final Color typeName;
  final Color function;

  /// 标签名；属性名用 [number]。
  final Color tag;

  /// Markdown 链接与标题。
  final Color link;
  final Color inserted;
  final Color deleted;

  static SyntaxColors of(BuildContext context) {
    final colors = Theme.of(context).extension<SyntaxColors>();
    assert(colors != null, 'SyntaxColors 缺失：ThemeData 要经 buildGalleyTheme 构建');
    return colors!;
  }

  static const light = SyntaxColors(
    comment: Primitives.synComment70,
    string: Primitives.synString20,
    number: Primitives.synNumber35,
    keyword: Primitives.synKeyword47,
    typeName: Primitives.synType33,
    function: Primitives.synFunction45,
    tag: Primitives.synTag28,
    link: Primitives.synLink40,
    inserted: Primitives.synTag28,
    deleted: Primitives.synDeleted27,
  );

  static const dark = SyntaxColors(
    comment: Primitives.synCommentDark62,
    string: Primitives.synStringDark82,
    number: Primitives.synNumberDark72,
    keyword: Primitives.synKeywordDark72,
    typeName: Primitives.synTypeDark70,
    function: Primitives.synFunctionDark82,
    tag: Primitives.synTagDark70,
    link: Primitives.synLinkDark65,
    inserted: Primitives.synTagDark70,
    deleted: Primitives.synDeletedDark80,
  );

  List<Color> get all => [
    comment,
    string,
    number,
    keyword,
    typeName,
    function,
    tag,
    link,
    inserted,
    deleted,
  ];

  @override
  SyntaxColors copyWith() => this;

  @override
  SyntaxColors lerp(ThemeExtension<SyntaxColors>? other, double t) {
    if (other is! SyntaxColors) return this;
    Color c(Color a, Color b) => Color.lerp(a, b, t)!;
    return SyntaxColors(
      comment: c(comment, other.comment),
      string: c(string, other.string),
      number: c(number, other.number),
      keyword: c(keyword, other.keyword),
      typeName: c(typeName, other.typeName),
      function: c(function, other.function),
      tag: c(tag, other.tag),
      link: c(link, other.link),
      inserted: c(inserted, other.inserted),
      deleted: c(deleted, other.deleted),
    );
  }
}
