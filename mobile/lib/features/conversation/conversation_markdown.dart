import 'package:flutter/material.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:url_launcher/url_launcher.dart';

import '../../theme/galley.dart';

/// 助手正文：markdown，不套气泡。代码块等宽、搁板底、细边框。
class ConversationMarkdown extends StatelessWidget {
  const ConversationMarkdown({
    super.key,
    required this.data,
    this.muted = false,
  });

  final String data;

  /// 思考块：整段降一档颜色。
  final bool muted;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final ink = muted ? c.inkMuted : c.ink;
    final body = t.body.copyWith(color: ink);
    // flutter_markdown 的行内代码和代码块共用这一个样式：底色取代码块的搁板白，
    // 代码块里就不会一行一条灰底；行内代码在钢底正文里成一小块搁板。
    final inlineCode = t.code.copyWith(
      color: ink,
      backgroundColor: c.shelf,
      fontSize: (t.body.fontSize ?? 16) * 0.88,
    );
    return MarkdownBody(
      data: data,
      selectable: true,
      onTapLink: (_, href, _) {
        final uri = href == null ? null : Uri.tryParse(href);
        if (uri != null) launchUrl(uri, mode: LaunchMode.externalApplication);
      },
      styleSheet: MarkdownStyleSheet.fromTheme(Theme.of(context)).copyWith(
        p: body,
        a: body.copyWith(
          color: c.link,
          decoration: TextDecoration.underline,
          decorationColor: c.link,
        ),
        strong: body.copyWith(fontWeight: FontWeight.w600),
        em: body.copyWith(fontStyle: FontStyle.italic),
        del: body.copyWith(decoration: TextDecoration.lineThrough),
        h1: t.title.copyWith(color: ink),
        h2: t.title.copyWith(color: ink),
        h3: t.headline.copyWith(color: ink),
        h4: t.bodyStrong.copyWith(color: ink),
        h5: t.bodyStrong.copyWith(color: ink),
        h6: t.bodyStrong.copyWith(color: ink),
        h1Padding: const EdgeInsets.only(top: Space.sm),
        h2Padding: const EdgeInsets.only(top: Space.sm),
        h3Padding: const EdgeInsets.only(top: Space.xs),
        code: inlineCode,
        codeblockPadding: const EdgeInsets.all(Space.md),
        codeblockDecoration: BoxDecoration(
          color: c.shelf,
          border: Border.all(color: c.hairline, width: Strokes.hairline),
          borderRadius: const BorderRadius.all(Radii.control),
        ),
        blockquote: body.copyWith(color: c.inkMuted),
        blockquotePadding: const EdgeInsets.symmetric(
          horizontal: Space.md,
          vertical: Space.sm,
        ),
        blockquoteDecoration: BoxDecoration(
          color: c.control,
          borderRadius: const BorderRadius.all(Radii.chip),
        ),
        listBullet: body,
        listIndent: Space.xxl,
        blockSpacing: Space.md,
        tableHead: t.label.copyWith(color: ink),
        tableBody: t.meta.copyWith(color: ink),
        tableBorder: TableBorder.all(
          color: c.hairline,
          width: Strokes.hairline,
        ),
        tableCellsPadding: const EdgeInsets.symmetric(
          horizontal: Space.sm,
          vertical: Space.xs,
        ),
        horizontalRuleDecoration: BoxDecoration(
          border: Border(top: BorderSide(color: c.hairline)),
        ),
        checkbox: body,
      ),
    );
  }
}

/// 等宽输出块：工具文字输出、终端输出、后台任务的尾巴。搁板底、细边框、可横向滚动。
class MonoBlock extends StatelessWidget {
  const MonoBlock({super.key, required this.child, this.footer});

  final Widget child;
  final Widget? footer;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    return DecoratedBox(
      decoration: BoxDecoration(
        color: c.shelf,
        border: Border.all(color: c.hairline, width: Strokes.hairline),
        borderRadius: const BorderRadius.all(Radii.control),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            padding: const EdgeInsets.all(Space.md),
            child: child,
          ),
          ?footer,
        ],
      ),
    );
  }
}
