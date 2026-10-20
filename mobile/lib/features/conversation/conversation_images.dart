import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:lucide_icons_flutter/lucide_icons.dart';

import '../../data/models.dart';
import '../../l10n/gen/l10n.dart';
import '../../theme/galley.dart';

/// 按引用取大图：给 `maxEdge` 出缩略图（长边不超过它），不给出原图。
typedef ConversationImageLoader =
    Future<ImageData> Function(ImageRef image, {int? maxEdge});

/// 列表里缩略图的长边上限。格子是 112×88，按 3 倍屏取够用的像素。
const thumbnailMaxEdge = 480;

const _thumbWidth = 112.0;
const _thumbHeight = thumbnailHeight;

/// 缩略图格子的高度，待发图片那一排按它留高度。
const thumbnailHeight = 88.0;

/// 已取回的缩略图。对话列表滚动时缩略图格子会反复重建，不缓存就会反复去运行时取。
final _thumbnailCache = <String, Uint8List>{};
const _thumbnailCacheLimit = 48;

String _thumbnailKey(ImageRef image) => '${image.entry}:${image.digest}';

@visibleForTesting
void clearThumbnailCache() => _thumbnailCache.clear();

/// 对话里的一张图：小图直接画字节；只给了引用的大图先取缩略图，点开再取原图。
class ConversationThumbnail extends StatefulWidget {
  const ConversationThumbnail({super.key, required this.image, this.loadImage});

  final ConversationImage image;

  /// 图片只给了引用时，用它取缩略图和点开后的原图；没给就只能放大手上的字节。
  final ConversationImageLoader? loadImage;

  @override
  State<ConversationThumbnail> createState() => _ConversationThumbnailState();
}

class _ConversationThumbnailState extends State<ConversationThumbnail> {
  MemoryImage? _provider;
  bool _loadingThumbnail = false;

  @override
  void initState() {
    super.initState();
    _updateImage();
  }

  @override
  void didUpdateWidget(covariant ConversationThumbnail oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (!_sameImage(oldWidget.image, widget.image)) _updateImage();
  }

  /// 每份快照里的图片都是新对象；内容没变就沿用原来的 provider，否则图片会闪。
  static bool _sameImage(ConversationImage a, ConversationImage b) =>
      a.mime == b.mime &&
      a.original == b.original &&
      (identical(a.bytes, b.bytes) || listEquals(a.bytes, b.bytes));

  void _updateImage() {
    final bytes = widget.image.bytes;
    if (bytes != null && bytes.isNotEmpty) {
      _provider = MemoryImage(bytes);
      return;
    }
    final original = widget.image.original;
    final cached = original == null
        ? null
        : _thumbnailCache[_thumbnailKey(original)];
    _provider = cached == null ? null : MemoryImage(cached);
    if (cached == null && original != null && widget.loadImage != null) {
      _loadThumbnail(original);
    }
  }

  Future<void> _loadThumbnail(ImageRef original) async {
    final load = widget.loadImage;
    if (load == null || _loadingThumbnail) return;
    _loadingThumbnail = true;
    try {
      final image = await load(original, maxEdge: thumbnailMaxEdge);
      if (image.bytes.isEmpty) return;
      final key = _thumbnailKey(original);
      _thumbnailCache.remove(key);
      _thumbnailCache[key] = image.bytes;
      while (_thumbnailCache.length > _thumbnailCacheLimit) {
        _thumbnailCache.remove(_thumbnailCache.keys.first);
      }
      if (!mounted || widget.image.original != original) return;
      setState(() => _provider = MemoryImage(image.bytes));
    } catch (_) {
      // 缩略图取不到就留占位，点开时还会再取原图。
    } finally {
      _loadingThumbnail = false;
    }
  }

  ConversationImageLoader? get _originalLoader =>
      widget.image.original == null ? null : widget.loadImage;

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final l10n = L10n.of(context);
    final provider = _provider;
    final loadOriginal = _originalLoader;
    Widget placeholder(IconData icon) => SizedBox(
      width: _thumbWidth,
      height: _thumbHeight,
      child: ColoredBox(
        color: c.control,
        child: Center(
          child: Icon(icon, size: Strokes.icon, color: c.inkFaint),
        ),
      ),
    );
    final frame = ClipRRect(
      borderRadius: const BorderRadius.all(Radii.control),
      child: provider == null
          ? placeholder(
              loadOriginal == null ? LucideIcons.imageOff : LucideIcons.image,
            )
          : Image(
              image: provider,
              width: _thumbWidth,
              height: _thumbHeight,
              fit: BoxFit.cover,
              gaplessPlayback: true,
              errorBuilder: (_, _, _) => placeholder(LucideIcons.imageOff),
            ),
    );
    if (provider == null && loadOriginal == null) {
      return Semantics(
        image: true,
        label: l10n.conversationImageUnavailable,
        child: frame,
      );
    }
    return Semantics(
      button: true,
      image: true,
      label: l10n.conversationImageOpen,
      child: GestureDetector(
        onTap: () => showDialog<void>(
          context: context,
          builder: (context) => _ImageViewer(
            preview: provider,
            original: widget.image.original,
            loadOriginal: loadOriginal,
          ),
        ),
        child: frame,
      ),
    );
  }
}

/// 全屏看图。有原图引用时先放大缩略图，原图到了再换上。
class _ImageViewer extends StatefulWidget {
  const _ImageViewer({
    required this.preview,
    required this.original,
    required this.loadOriginal,
  });

  final ImageProvider? preview;
  final ImageRef? original;
  final ConversationImageLoader? loadOriginal;

  @override
  State<_ImageViewer> createState() => _ImageViewerState();
}

class _ImageViewerState extends State<_ImageViewer> {
  ImageProvider? _full;
  bool _loading = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final load = widget.loadOriginal;
    final original = widget.original;
    if (load == null || original == null) return;
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final image = await load(original);
      if (!mounted) return;
      setState(() {
        _loading = false;
        if (image.bytes.isEmpty) {
          _error = L10n.of(context).conversationImageUndecodable;
        } else {
          _full = MemoryImage(image.bytes);
        }
      });
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _loading = false;
        _error = L10n.of(
          context,
        ).conversationImageLoadFailed(describeError(error));
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final c = GalleyColors.of(context);
    final t = GalleyText.of(context);
    final l10n = L10n.of(context);
    final shown = _full ?? widget.preview;
    return Dialog.fullscreen(
      backgroundColor: c.ground,
      child: SafeArea(
        child: Stack(
          children: [
            Positioned.fill(
              child: shown == null
                  ? Center(
                      child: Icon(
                        LucideIcons.image,
                        size: Space.touch,
                        color: c.inkFaint,
                      ),
                    )
                  : InteractiveViewer(
                      minScale: 0.5,
                      maxScale: 5,
                      child: Center(
                        child: Image(image: shown, gaplessPlayback: true),
                      ),
                    ),
            ),
            if (_loading)
              const Positioned.fill(
                child: IgnorePointer(
                  child: Center(child: CircularProgressIndicator.adaptive()),
                ),
              ),
            if (_error case final error?)
              Positioned(
                left: Space.page,
                right: Space.page,
                bottom: Space.xxl,
                child: DecoratedBox(
                  decoration: BoxDecoration(
                    color: c.raised,
                    borderRadius: const BorderRadius.all(Radii.shelf),
                    boxShadow: [
                      BoxShadow(
                        color: c.shadow,
                        offset: const Offset(0, 2),
                        blurRadius: 12,
                      ),
                    ],
                  ),
                  child: Padding(
                    padding: const EdgeInsets.fromLTRB(
                      Space.lg,
                      Space.xs,
                      Space.xs,
                      Space.xs,
                    ),
                    child: Row(
                      children: [
                        Icon(
                          LucideIcons.circleAlert,
                          size: Strokes.iconSmall,
                          color: c.danger,
                        ),
                        const SizedBox(width: Space.sm),
                        Expanded(
                          child: Text(
                            error,
                            style: t.meta.copyWith(color: c.ink),
                          ),
                        ),
                        TextButton(
                          onPressed: _load,
                          child: Text(l10n.commonRetry),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
            Positioned(
              top: Space.sm,
              right: Space.sm,
              child: IconButton(
                tooltip: l10n.conversationImageClose,
                style: IconButton.styleFrom(backgroundColor: c.control),
                onPressed: () => Navigator.pop(context),
                icon: const Icon(LucideIcons.x),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
