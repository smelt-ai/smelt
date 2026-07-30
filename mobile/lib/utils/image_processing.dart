import 'dart:typed_data';

import 'package:image/image.dart' as img;

Uint8List? normalizeJpegOrientation(Uint8List bytes) {
  final decoded = img.decodeImage(bytes);
  if (decoded == null) return null;
  final oriented = img.bakeOrientation(decoded);
  oriented.exif = img.ExifData();
  return img.encodeJpg(oriented, quality: 85);
}
