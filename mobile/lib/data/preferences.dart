import 'dart:io';

import 'package:flutter/material.dart';

import 'app_update.dart';
import 'json_prefs_file.dart';

/// 这台手机上的显示偏好（配置，不是业务数据，所以是一份带版本号的 JSON）。
///
/// 启动时在 `runApp` 之前读好：第一帧就用对的明暗，不闪一下。读不出、版本不认识就
/// 回到默认值。
@immutable
class Preferences {
  const Preferences({
    this.themeMode = ThemeMode.system,
    this.terminalFontSize = defaultTerminalFontSize,
    this.updateChannel = UpdateChannel.prod,
  });

  static const version = 1;
  static const defaultTerminalFontSize = 13.0;

  /// 终端字号只给档位：终端是等宽网格，字号一变就要重算行列并向 PTY 发 resize，
  /// 连续滑杆等于对着远端狂发 resize。
  static const terminalFontSteps = <double>[11, 12, 13, 14, 16, 18, 20];

  final ThemeMode themeMode;
  final double terminalFontSize;

  /// 安卓应用内更新跟哪个通道（见 `app_update.dart`）。后加的字段：旧文件里没有就是生产版。
  final UpdateChannel updateChannel;

  Preferences copyWith({
    ThemeMode? themeMode,
    double? terminalFontSize,
    UpdateChannel? updateChannel,
  }) => Preferences(
    themeMode: themeMode ?? this.themeMode,
    terminalFontSize: terminalFontSize ?? this.terminalFontSize,
    updateChannel: updateChannel ?? this.updateChannel,
  );

  factory Preferences.fromJson(Map<String, dynamic> json) {
    if (json['version'] != version) return const Preferences();
    final mode = parseThemeMode(json['themeMode']);
    final size = json['terminalFontSize'];
    final font = size is num && terminalFontSteps.contains(size.toDouble())
        ? size.toDouble()
        : defaultTerminalFontSize;
    return Preferences(
      themeMode: mode ?? ThemeMode.system,
      terminalFontSize: font,
      updateChannel:
          parseUpdateChannel(json['updateChannel']) ?? UpdateChannel.prod,
    );
  }

  Map<String, dynamic> toJson() => {
    'version': version,
    'themeMode': themeMode.name,
    'terminalFontSize': terminalFontSize,
    'updateChannel': updateChannel.name,
  };

  @override
  bool operator ==(Object other) =>
      other is Preferences &&
      other.themeMode == themeMode &&
      other.terminalFontSize == terminalFontSize &&
      other.updateChannel == updateChannel;

  @override
  int get hashCode => Object.hash(themeMode, terminalFontSize, updateChannel);
}

/// 偏好：改了立刻生效（界面监听这个 notifier），再排队落盘。
class PreferencesStore extends ValueNotifier<Preferences> {
  PreferencesStore._(this._file, Preferences initial) : super(initial);

  /// 内存版（测试、演示）没有文件，不落盘。
  final JsonPrefsFile<Preferences>? _file;

  static JsonPrefsFile<Preferences> _open(
    Future<Directory> Function()? directoryProvider,
  ) => JsonPrefsFile<Preferences>(
    fileName: 'preferences.json',
    decode: Preferences.fromJson,
    encode: (p) => p.toJson(),
    fallback: const Preferences(),
    directoryProvider: directoryProvider,
  );

  static Future<PreferencesStore> load({
    Future<Directory> Function()? directoryProvider,
  }) async {
    final file = _open(directoryProvider);
    return PreferencesStore._(file, await file.load());
  }

  /// 测试与演示用：不落盘读的初值。
  factory PreferencesStore.memory([
    Preferences initial = const Preferences(),
  ]) => PreferencesStore._(null, initial);

  void update(Preferences next) {
    if (next == value) return;
    value = next;
    _file?.save(next);
  }
}

/// 存下来的主题名 → [ThemeMode]；认不出返回 null。
ThemeMode? parseThemeMode(Object? raw) =>
    ThemeMode.values.where((m) => m.name == raw).firstOrNull;
