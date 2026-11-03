/// 带行号的代码视图：按行懒加载，按语义类别上色。
///
/// 正文在内核里（[TextSource]），这里一次取 [_chunk] 行，滚到哪取到哪，取过的留着。
/// 不换行时每行等高，用固定行高的列表，跳到某一行是一次 `jumpTo`；横向整体滚动，
/// 宽度按最长一行算。换行时行高不定，不支持跳行。
library;

import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../../data/models.dart';
import '../../theme/galley.dart';
import '../../theme/syntax.dart';

const _chunk = 200;

class CodeView extends StatefulWidget {
  const CodeView({
    super.key,
    required this.text,
    required this.wrap,
    this.initialLine,
  });

  final TextSource text;
  final bool wrap;

  /// 打开时滚到并标出这一行（1 基），只在不换行时生效。
  final int? initialLine;

  @override
  State<CodeView> createState() => _CodeViewState();
}

class _CodeViewState extends State<CodeView> {
  final Map<int, List<HighlightedLine>> _chunks = {};
  final Set<int> _loading = {};
  final _vertical = ScrollController();
  bool _jumped = false;

  @override
  void dispose() {
    _vertical.dispose();
    super.dispose();
  }

  HighlightedLine? _line(int index) {
    final chunk = index ~/ _chunk;
    final lines = _chunks[chunk];
    if (lines == null) {
      _load(chunk);
      return null;
    }
    final offset = index - chunk * _chunk;
    return offset < lines.length ? lines[offset] : null;
  }

  void _load(int chunk) {
    if (!_loading.add(chunk)) return;
    final start = chunk * _chunk;
    unawaited(
      widget.text.lines(start, start + _chunk).then((lines) {
        if (!mounted) return;
        setState(() {
          _chunks[chunk] = lines;
          _loading.remove(chunk);
        });
      }, onError: (Object _) => _loading.remove(chunk)),
    );
  }

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final syntax = SyntaxColors.of(context);
    final scaler = MediaQuery.textScalerOf(context);
    final style = t.code.copyWith(color: c.ink);
    final fontSize = style.fontSize ?? 13;
    final lineHeight = scaler.scale(fontSize) * (style.height ?? 1.5);
    final charWidth = _charWidth(style, scaler);
    final count = widget.text.lineCount;
    final gutterWidth = '$count'.length * charWidth + Space.md * 2;

    final target = widget.initialLine;
    if (!_jumped && target != null && !widget.wrap) {
      _jumped = true;
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (!_vertical.hasClients) return;
        // 目标行放在屏幕上方三分之一处，上面留点上下文。
        final viewport = _vertical.position.viewportDimension;
        final offset = (target - 1) * lineHeight - viewport / 3;
        _vertical.jumpTo(offset.clamp(0.0, _vertical.position.maxScrollExtent));
      });
    }

    Widget row(BuildContext context, int index) {
      final line = _line(index);
      final marked = target != null && index == target - 1;
      final spans = [
        for (final span in line?.spans ?? const <TokenSpan>[])
          TextSpan(
            text: span.text,
            style: _styleOf(span.kind, style, c, syntax),
          ),
      ];
      return Container(
        color: marked ? c.shelfPressed : null,
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            SizedBox(
              width: gutterWidth,
              child: Padding(
                padding: const EdgeInsets.only(right: Space.md),
                child: Text(
                  '${index + 1}',
                  textAlign: TextAlign.right,
                  style: style.copyWith(color: c.inkFaint),
                ),
              ),
            ),
            Expanded(
              child: Text.rich(
                TextSpan(children: spans, style: style),
                softWrap: widget.wrap,
                maxLines: widget.wrap ? null : 1,
                overflow: TextOverflow.clip,
              ),
            ),
          ],
        ),
      );
    }

    final list = ListView.builder(
      controller: _vertical,
      padding: EdgeInsets.only(
        top: Space.sm,
        bottom: MediaQuery.paddingOf(context).bottom + Space.xxl,
      ),
      itemCount: count,
      itemExtent: widget.wrap ? null : lineHeight,
      itemBuilder: row,
    );

    final body = widget.wrap
        ? list
        : LayoutBuilder(
            builder: (context, constraints) {
              final content =
                  gutterWidth +
                  widget.text.maxLineChars * charWidth +
                  Space.page;
              return SingleChildScrollView(
                scrollDirection: Axis.horizontal,
                child: SizedBox(
                  width: math.max(constraints.maxWidth, content),
                  height: constraints.maxHeight,
                  child: list,
                ),
              );
            },
          );
    return ColoredBox(
      color: c.shelf,
      child: SelectionArea(child: body),
    );
  }
}

/// 等宽字一个字宽。中日韩字是两个字宽，最长一行全是汉字时横向会少算，换行模式能看全。
double _charWidth(TextStyle style, TextScaler scaler) {
  final painter = TextPainter(
    text: TextSpan(text: 'M', style: style),
    textDirection: TextDirection.ltr,
    textScaler: scaler,
  )..layout();
  final width = painter.width;
  painter.dispose();
  return width;
}

TextStyle _styleOf(
  TokenKind kind,
  TextStyle base,
  GalleyColors c,
  SyntaxColors syntax,
) => switch (kind) {
  TokenKind.plain || TokenKind.punctuation => base,
  TokenKind.comment => base.copyWith(
    color: syntax.comment,
    fontStyle: FontStyle.italic,
  ),
  TokenKind.string => base.copyWith(color: syntax.string),
  TokenKind.number ||
  TokenKind.constant ||
  TokenKind.attribute => base.copyWith(color: syntax.number),
  TokenKind.keyword ||
  TokenKind.operator_ => base.copyWith(color: syntax.keyword),
  TokenKind.type || TokenKind.variable => base.copyWith(color: syntax.typeName),
  TokenKind.function => base.copyWith(color: syntax.function),
  TokenKind.tag => base.copyWith(color: syntax.tag),
  TokenKind.heading => base.copyWith(
    color: syntax.link,
    fontWeight: FontWeight.w600,
  ),
  TokenKind.link => base.copyWith(
    color: syntax.link,
    decoration: TextDecoration.underline,
    decorationColor: syntax.link,
  ),
  TokenKind.emphasis => base.copyWith(fontStyle: FontStyle.italic),
  TokenKind.inserted => base.copyWith(color: syntax.inserted),
  TokenKind.deleted => base.copyWith(color: syntax.deleted),
};
