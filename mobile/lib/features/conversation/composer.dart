import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';
import 'conversation_images.dart';

/// 输入框上方的一个配置芯片：当前模型、provider、配置项。点开底部弹层选。
class ConfigChip extends StatelessWidget {
  const ConfigChip({
    super.key,
    required this.label,
    required this.onTap,
    this.icon,
    this.semanticLabel,
  });

  final String label;
  final IconData? icon;
  final VoidCallback? onTap;
  final String? semanticLabel;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    const shape = RoundedRectangleBorder(
      borderRadius: BorderRadius.all(Radii.chip),
    );
    // 看上去是一枚小芯片，按得到的范围仍是 48 高。
    return Semantics(
      button: true,
      label: semanticLabel,
      excludeSemantics: semanticLabel != null,
      child: SizedBox(
        height: Space.touch,
        child: Center(
          child: Material(
            color: c.shelf,
            shape: shape.copyWith(side: BorderSide(color: c.hairline)),
            child: InkWell(
              onTap: onTap,
              customBorder: shape,
              child: Padding(
                padding: const EdgeInsets.symmetric(
                  horizontal: Space.sm + 2,
                  vertical: Space.xs + 2,
                ),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    if (icon != null) ...[
                      Icon(
                        icon,
                        size: Strokes.iconSmall - 2,
                        color: c.inkMuted,
                      ),
                      const SizedBox(width: Space.xs + 2),
                    ],
                    ConstrainedBox(
                      constraints: const BoxConstraints(maxWidth: 200),
                      child: Text(
                        label,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: t.label.copyWith(color: c.ink),
                      ),
                    ),
                    if (onTap != null) ...[
                      const SizedBox(width: Space.xxs),
                      Icon(
                        LucideIcons.chevronDown,
                        size: Strokes.iconSmall - 2,
                        color: c.inkFaint,
                      ),
                    ],
                  ],
                ),
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// 输入胶囊：左边加图片、斜杠命令；右边发送（回合进行中、框里没字时变成停止）。
class Composer extends StatelessWidget {
  const Composer({
    super.key,
    required this.controller,
    required this.focusNode,
    required this.hint,
    required this.canSend,
    required this.working,
    required this.sending,
    required this.readOnly,
    required this.images,
    required this.onSend,
    required this.onStop,
    required this.onRemoveImage,
    this.onAttach,
    this.onCommands,
    this.chips = const [],
    this.error,
    this.footnote,
  });

  final TextEditingController controller;
  final FocusNode focusNode;
  final String hint;

  /// 除了「框里有东西」以外，能不能发（连着、阶段接收新消息、没在发）。
  final bool canSend;

  /// 回合进行中：框里没字时发送钮变成停止。
  final bool working;
  final bool sending;
  final bool readOnly;
  final List<ImageInput> images;
  final VoidCallback onSend;
  final VoidCallback onStop;
  final void Function(int index) onRemoveImage;

  /// 为空表示这个 agent 不收图，不画入口。
  final VoidCallback? onAttach;
  final VoidCallback? onCommands;
  final List<Widget> chips;
  final String? error;

  /// 胶囊下面一行小字（上下文用量）。
  final String? footnote;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final pill = DecoratedBox(
      decoration: BoxDecoration(
        color: c.shelf,
        borderRadius: const BorderRadius.all(Radii.pill),
        border: Border.all(color: c.hairline, width: Strokes.hairline),
        boxShadow: [
          BoxShadow(
            color: c.shadow,
            offset: const Offset(0, 2),
            blurRadius: 12,
          ),
        ],
      ),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: Space.xs),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [
            if (onAttach != null)
              IconButton(
                tooltip: l10n.conversationAttachImage,
                onPressed: readOnly ? null : onAttach,
                icon: Icon(LucideIcons.imagePlus, color: c.inkMuted),
              ),
            if (onCommands != null)
              IconButton(
                tooltip: l10n.conversationCommands,
                onPressed: readOnly ? null : onCommands,
                icon: Icon(LucideIcons.slash, color: c.inkMuted),
              ),
            if (onAttach == null && onCommands == null)
              const SizedBox(width: Space.md),
            Expanded(
              child: TextField(
                controller: controller,
                focusNode: focusNode,
                readOnly: readOnly,
                minLines: 1,
                maxLines: 6,
                keyboardType: TextInputType.multiline,
                textInputAction: TextInputAction.newline,
                textCapitalization: TextCapitalization.sentences,
                style: t.body.copyWith(color: c.ink),
                decoration: InputDecoration(
                  hintText: hint,
                  hintMaxLines: 1,
                  filled: false,
                  border: InputBorder.none,
                  enabledBorder: InputBorder.none,
                  focusedBorder: InputBorder.none,
                  disabledBorder: InputBorder.none,
                  isDense: true,
                  contentPadding: const EdgeInsets.symmetric(
                    horizontal: Space.xs,
                    vertical: Space.md + 1,
                  ),
                ),
              ),
            ),
            ListenableBuilder(
              listenable: controller,
              builder: (context, _) {
                final hasContent =
                    controller.text.trim().isNotEmpty || images.isNotEmpty;
                if (working && !hasContent && !sending) {
                  return _RoundButton(
                    tooltip: l10n.conversationStop,
                    icon: LucideIcons.square,
                    background: c.danger,
                    foreground: c.onSolid,
                    onPressed: onStop,
                  );
                }
                return _RoundButton(
                  tooltip: l10n.conversationSend,
                  icon: LucideIcons.arrowUp,
                  background: c.solid,
                  foreground: c.onSolid,
                  busy: sending,
                  onPressed: canSend && hasContent ? onSend : null,
                );
              },
            ),
          ],
        ),
      ),
    );

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        if (chips.isNotEmpty)
          SizedBox(
            height: Space.touch,
            child: ListView.separated(
              scrollDirection: Axis.horizontal,
              itemCount: chips.length,
              separatorBuilder: (_, _) => const SizedBox(width: Space.sm - 2),
              itemBuilder: (_, i) => chips[i],
            ),
          ),
        if (images.isNotEmpty)
          Padding(
            padding: const EdgeInsets.only(bottom: Space.sm),
            child: SizedBox(
              height: thumbnailHeight + Space.sm,
              child: ListView.separated(
                scrollDirection: Axis.horizontal,
                itemCount: images.length,
                separatorBuilder: (_, _) => const SizedBox(width: Space.sm),
                itemBuilder: (_, i) => _PendingImage(
                  image: images[i],
                  onRemove: readOnly ? null : () => onRemoveImage(i),
                ),
              ),
            ),
          ),
        if (error != null)
          Padding(
            padding: const EdgeInsets.fromLTRB(Space.md, 0, Space.md, Space.sm),
            child: Semantics(
              liveRegion: true,
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Padding(
                    padding: const EdgeInsets.only(top: Space.xxs),
                    child: Icon(
                      LucideIcons.circleAlert,
                      size: Strokes.iconSmall,
                      color: c.danger,
                    ),
                  ),
                  const SizedBox(width: Space.sm),
                  Expanded(
                    child: Text(
                      error!,
                      style: t.meta.copyWith(color: c.danger),
                    ),
                  ),
                ],
              ),
            ),
          ),
        pill,
        if (footnote != null)
          Padding(
            padding: const EdgeInsets.only(top: Space.xs + 2),
            child: Text(
              footnote!,
              textAlign: TextAlign.center,
              style: t.caption.copyWith(
                color: c.inkMuted,
                fontFeatures: const [FontFeature.tabularFigures()],
              ),
            ),
          ),
      ],
    );
  }
}

/// 实心圆钮：视觉 40，按得到的范围 48。
class _RoundButton extends StatelessWidget {
  const _RoundButton({
    required this.tooltip,
    required this.icon,
    required this.background,
    required this.foreground,
    required this.onPressed,
    this.busy = false,
  });

  final String tooltip;
  final IconData icon;
  final Color background;
  final Color foreground;
  final VoidCallback? onPressed;
  final bool busy;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    return IconButton(
      tooltip: tooltip,
      onPressed: busy ? null : onPressed,
      style: IconButton.styleFrom(
        backgroundColor: background,
        foregroundColor: foreground,
        disabledBackgroundColor: c.control,
        disabledForegroundColor: c.inkFaint,
        minimumSize: const Size.square(Space.touch - Space.sm),
        fixedSize: const Size.square(Space.touch - Space.sm),
        tapTargetSize: MaterialTapTargetSize.padded,
        padding: EdgeInsets.zero,
      ),
      icon: busy
          ? SizedBox.square(
              dimension: Strokes.iconSmall,
              child: CircularProgressIndicator(
                strokeWidth: 2,
                color: c.inkMuted,
              ),
            )
          : Icon(icon, size: Strokes.icon),
    );
  }
}

class _PendingImage extends StatelessWidget {
  const _PendingImage({required this.image, required this.onRemove});

  final ImageInput image;
  final VoidCallback? onRemove;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    return Stack(
      clipBehavior: Clip.none,
      children: [
        Padding(
          padding: const EdgeInsets.only(top: Space.sm, right: Space.sm),
          child: ConversationThumbnail(
            image: ConversationImage(mime: image.mime, bytes: image.bytes),
          ),
        ),
        if (onRemove != null)
          Positioned(
            top: -Space.sm,
            right: -Space.sm,
            child: IconButton(
              tooltip: l10n.conversationRemoveImage,
              onPressed: onRemove,
              style: IconButton.styleFrom(
                minimumSize: const Size.square(Space.touch),
              ),
              icon: DecoratedBox(
                decoration: BoxDecoration(
                  color: c.solid,
                  shape: BoxShape.circle,
                  border: Border.all(color: c.shelf, width: 2),
                ),
                child: Padding(
                  padding: const EdgeInsets.all(Space.xxs + 1),
                  child: Icon(LucideIcons.x, size: 12, color: c.onSolid),
                ),
              ),
            ),
          ),
      ],
    );
  }
}
