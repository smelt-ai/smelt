import 'dart:io';

import 'json_prefs_file.dart';

/// 终端的本地显示偏好。
///
/// 独立于配对信息：换一台桌面、换一次配对码，都不该把用户调好的字号清掉，
/// 所以它不进 `flutter_secure_storage`，也不跟会话草稿放一起。
class TerminalPrefs {
  const TerminalPrefs({this.fontSize = defaultFontSize});

  /// 沿用改造前写死的值，保证老用户升级后看到的字号不变。
  static const double defaultFontSize = 13;

  /// 可选档位。不给连续滑杆：终端是等宽网格，字号变化会连带重算 cols/rows
  /// 并向 PTY 发 resize，连续拖动等于对着远端狂发 resize。
  static const List<double> steps = [11, 12, 13, 14, 16, 18, 20];

  final double fontSize;

  TerminalPrefs copyWith({double? fontSize}) =>
      TerminalPrefs(fontSize: fontSize ?? this.fontSize);

  /// 越界或损坏的值一律退回默认档，不要把终端渲染成 0.5pt。
  factory TerminalPrefs.fromJson(Map<String, dynamic> json) {
    final raw = json['fontSize'];
    final size = raw is num ? raw.toDouble() : defaultFontSize;
    return TerminalPrefs(
      fontSize: steps.contains(size) ? size : defaultFontSize,
    );
  }

  Map<String, dynamic> toJson() => {'fontSize': fontSize};

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is TerminalPrefs && other.fontSize == fontSize;

  @override
  int get hashCode => fontSize.hashCode;
}

abstract interface class TerminalPrefsStore {
  Future<TerminalPrefs> load();
  Future<void> save(TerminalPrefs prefs);
}

class FileTerminalPrefsStore implements TerminalPrefsStore {
  FileTerminalPrefsStore({Future<Directory> Function()? directoryProvider})
    : _file = JsonPrefsFile<TerminalPrefs>(
        fileName: 'terminal-prefs.json',
        decode: TerminalPrefs.fromJson,
        encode: (value) => value.toJson(),
        fallback: const TerminalPrefs(),
        directoryProvider: directoryProvider,
      );

  final JsonPrefsFile<TerminalPrefs> _file;

  @override
  Future<TerminalPrefs> load() => _file.load();

  @override
  Future<void> save(TerminalPrefs prefs) => _file.save(prefs);
}
