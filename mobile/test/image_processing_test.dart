import 'package:flutter_test/flutter_test.dart';
import 'package:image/image.dart' as img;
import 'package:smelt_mobile/utils/image_processing.dart';

void main() {
  test('JPEG orientation is baked into pixels and metadata is cleared', () {
    final source = img.Image(width: 20, height: 10);
    img.fillRect(
      source,
      x1: 0,
      y1: 0,
      x2: 9,
      y2: 9,
      color: img.ColorRgb8(255, 0, 0),
    );
    img.fillRect(
      source,
      x1: 10,
      y1: 0,
      x2: 19,
      y2: 9,
      color: img.ColorRgb8(0, 0, 255),
    );
    source.exif.imageIfd.orientation = 3;

    final normalized = normalizeJpegOrientation(
      img.encodeJpg(source, quality: 100),
    );
    final decoded = img.decodeJpg(normalized!);

    expect(decoded, isNotNull);
    expect(decoded!.exif.imageIfd.hasOrientation, isFalse);
    final left = decoded.getPixel(2, 5);
    final right = decoded.getPixel(17, 5);
    expect(left.b, greaterThan(left.r));
    expect(right.r, greaterThan(right.b));
  });
}
