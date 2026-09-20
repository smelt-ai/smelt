import 'dart:io';

import 'package:flutter/material.dart';

import 'json_prefs_file.dart';

/// 外观偏好。
///
/// 目前只有主题。它单独成一份而不是塞进终端偏好，是因为两者生命周期不同：主题在
/// App 启动最早期就要拿到（决定第一帧画什么颜色），终端字号要到打开终端页才需要。
class AppearancePrefs {
  const AppearancePrefs({this.themeMode = ThemeMode.system});

  /// 跟改造前的行为一致（原来写死 `ThemeMode.system`），升级的用户不会突然换色。
  final ThemeMode themeMode;

  AppearancePrefs copyWith({ThemeMode? themeMode}) =>
      AppearancePrefs(themeMode: themeMode ?? this.themeMode);

  /// 认不出的值退回跟随系统，不要让一个手改坏的文件把 App 锁在某个主题里。
  factory AppearancePrefs.fromJson(Map<String, dynamic> json) {
    final raw = json['themeMode'];
    return AppearancePrefs(
      themeMode: switch (raw) {
        'light' => ThemeMode.light,
        'dark' => ThemeMode.dark,
        _ => ThemeMode.system,
      },
    );
  }

  Map<String, dynamic> toJson() => {'themeMode': themeMode.name};

  @override
  bool operator ==(Object other) =>
      identical(this, other) ||
      other is AppearancePrefs && other.themeMode == themeMode;

  @override
  int get hashCode => themeMode.hashCode;
}

abstract interface class AppearancePrefsStore {
  Future<AppearancePrefs> load();
  Future<void> save(AppearancePrefs prefs);
}

class FileAppearancePrefsStore implements AppearancePrefsStore {
  FileAppearancePrefsStore({Future<Directory> Function()? directoryProvider})
    : _file = JsonPrefsFile<AppearancePrefs>(
        fileName: 'appearance-prefs.json',
        decode: AppearancePrefs.fromJson,
        encode: (value) => value.toJson(),
        fallback: const AppearancePrefs(),
        directoryProvider: directoryProvider,
      );

  final JsonPrefsFile<AppearancePrefs> _file;

  @override
  Future<AppearancePrefs> load() => _file.load();

  @override
  Future<void> save(AppearancePrefs prefs) => _file.save(prefs);
}
