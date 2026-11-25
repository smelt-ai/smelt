import 'package:flutter/foundation.dart';
import 'package:image_picker/image_picker.dart';

import '../data/models.dart';
import 'image_processing.dart';

/// 一条消息最多带几张图、每张多大。和桌面输入栏一致。
const kMaxComposerImages = 4;
const kMaxComposerImageBytes = 5 * 1024 * 1024;

class PickedImagesResult {
  const PickedImagesResult({required this.images, this.skipped = 0});

  final List<ImageInput> images;
  final int skipped;
}

typedef ComposerImagePicker = Future<PickedImagesResult> Function(int remaining);

Future<PickedImagesResult> pickComposerImages(
  ImagePicker picker, {
  required int remaining,
}) async {
  if (remaining <= 0) {
    return const PickedImagesResult(images: []);
  }
  final files = await picker.pickMultiImage(
    limit: remaining,
    maxWidth: 2048,
    maxHeight: 2048,
    imageQuality: 85,
    requestFullMetadata: false,
  );
  var skipped = 0;
  final picked = <ImageInput>[];
  for (final file in files.take(remaining)) {
    var bytes = await file.readAsBytes();
    var mime = file.mimeType ?? mimeFromImageName(file.name);
    // 相机照片带 EXIF 方向，运行时那边不一定认：在手机上摆正并去掉 EXIF。
    if (mime == 'image/jpeg' || mime == 'image/heic') {
      final normalized = await compute(normalizeJpegOrientation, bytes);
      if (normalized == null) {
        skipped++;
        continue;
      }
      bytes = normalized;
      mime = 'image/jpeg';
    }
    if (bytes.isEmpty || bytes.length > kMaxComposerImageBytes) {
      skipped++;
      continue;
    }
    picked.add(ImageInput(mime: mime, bytes: bytes));
  }
  return PickedImagesResult(images: picked, skipped: skipped);
}

String mimeFromImageName(String name) {
  final lower = name.toLowerCase();
  if (lower.endsWith('.png')) return 'image/png';
  if (lower.endsWith('.gif')) return 'image/gif';
  if (lower.endsWith('.webp')) return 'image/webp';
  if (lower.endsWith('.heic') || lower.endsWith('.heif')) return 'image/heic';
  return 'image/jpeg';
}
