import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/theme/galley.dart';

double contrast(Color a, Color b) {
  final la = a.computeLuminance();
  final lb = b.computeLuminance();
  final hi = la > lb ? la : lb;
  final lo = la > lb ? lb : la;
  return (hi + 0.05) / (lo + 0.05);
}

void main() {
  for (final (name, c) in [
    ('light', GalleyColors.light),
    ('dark', GalleyColors.dark),
  ]) {
    group('$name 对比度（WCAG AA）', () {
      test('正文与各级文字在钢底和搁板上 ≥ 4.5', () {
        for (final fg in [c.ink, c.inkMuted, c.inkFaint, c.heatInk, c.link]) {
          expect(contrast(fg, c.ground), greaterThanOrEqualTo(4.5));
          expect(contrast(fg, c.shelf), greaterThanOrEqualTo(4.5));
        }
      });

      test('状态文字 ≥ 4.5', () {
        for (final fg in [c.danger, c.ok]) {
          expect(contrast(fg, c.ground), greaterThanOrEqualTo(4.5));
          expect(contrast(fg, c.shelf), greaterThanOrEqualTo(4.5));
        }
      });

      test('单据纸上的字 ≥ 4.5', () {
        expect(contrast(c.onPaper, c.paper), greaterThanOrEqualTo(4.5));
        expect(contrast(c.onPaperMuted, c.paper), greaterThanOrEqualTo(4.5));
      });

      test('按钮字 ≥ 4.5', () {
        expect(contrast(c.onHeat, c.heat), greaterThanOrEqualTo(4.5));
        expect(contrast(c.onSolid, c.solid), greaterThanOrEqualTo(4.5));
        expect(contrast(c.onControl, c.control), greaterThanOrEqualTo(4.5));
      });

      test('灶台标签色（非文字图形）对底色 ≥ 3', () {
        for (final s in c.stations) {
          expect(contrast(s, c.ground), greaterThanOrEqualTo(3));
          expect(contrast(s, c.shelf), greaterThanOrEqualTo(3));
        }
      });
    });
  }
}
