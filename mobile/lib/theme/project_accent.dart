import 'package:flutter/material.dart';

/// 项目身份色：项目名 → 稳定颜色。
///
/// 项目在移动端原来只有一行文字，多个项目并排时全靠读字分辨。
/// 色块用来在这一屏里区分彼此，不是跨设备对暗号。
///
/// 色环取自桌面语义色 `accent / green / blue / purple / yellow / red`。
/// 哈希用写死的 FNV-1a，只保证移动端内部稳定。色块用来在这一屏区分项目，
/// 不是跨设备对暗号。协议若以后下发颜色，只换本函数。
Color projectAccent(String key, Brightness brightness) {
  final ring = brightness == Brightness.dark ? _ringDark : _ringLight;
  return ring[_fnv1a(key) % ring.length];
}

/// 项目名 → 至多两个字母的缩写。
///
/// 优先取分词后每段的首字母（`docs-site` → DS），只有一段时取前两个字符
/// （`smelt` → SM）。分隔符包含 `-` `_` `.` 和空格，因为项目名多半来自目录名。
String projectInitials(String title) {
  final cleaned = title.trim();
  if (cleaned.isEmpty) return '?';

  final parts = cleaned
      .split(RegExp(r'[\s\-_.]+'))
      .where((part) => part.isNotEmpty)
      .toList(growable: false);

  if (parts.isEmpty) return cleaned.characters.first.toUpperCase();
  if (parts.length == 1) {
    final only = parts.first;
    final take = only.length >= 2 ? only.substring(0, 2) : only;
    return take.toUpperCase();
  }
  return (parts[0].characters.first + parts[1].characters.first).toUpperCase();
}

const _ringDark = <Color>[
  Color(0xff1084fe), // accent
  Color(0xff22c55e), // green
  Color(0xff3b82f6), // blue
  Color(0xff9159fe), // purple
  Color(0xffff9800), // yellow
  Color(0xffff263c), // red
];

const _ringLight = <Color>[
  Color(0xff1084fe),
  Color(0xff16a34a),
  Color(0xff2563eb),
  Color(0xff6e44c1),
  Color(0xffc27400),
  Color(0xffc21d2e),
];

/// FNV-1a (32 位)。选它是因为实现短到可以一眼看完、无依赖、结果写死不会漂移。
int _fnv1a(String value) {
  var hash = 0x811c9dc5;
  for (final unit in value.codeUnits) {
    hash ^= unit;
    // Dart 的 int 在 VM 上是 64 位，必须自己截回 32 位，否则不同平台结果不同。
    hash = (hash * 0x01000193) & 0xffffffff;
  }
  return hash;
}
