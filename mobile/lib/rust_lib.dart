/// 加载 Rust 动态库。
///
/// 不能用 `RustLib.init()` 的默认加载：codegen 按 crate 名（`smelt_mobile`）
/// 去 dlopen，但 cargokit 产出的 framework 跟着 Dart 包名叫
/// `rust_lib_smelt_mobile` —— 两者对不上，构建期一切正常，一到运行时就报
/// `Failed to load dynamic library`。framework 名由 CocoaPods 的 `s.name`
/// 决定，而它又必须等于 Dart 包名才能被 Flutter 发现，所以改名解决不了，
/// 只能在加载这一侧说清楚。
library;

import 'dart:io';

import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';

import 'src/rust/frb_generated.dart';

/// Apple 平台上 cargokit 产出的 framework 名（= rust_builder 的 Dart 包名）。
const _appleFramework = 'rust_lib_smelt_mobile';

/// 初始化 Rust 侧。重复调用是安全的。
Future<void> initRustLib() async {
  await RustLib.init(externalLibrary: _openLibrary());
}

ExternalLibrary _openLibrary() {
  if (Platform.isIOS || Platform.isMacOS) {
    return ExternalLibrary.open('$_appleFramework.framework/$_appleFramework');
  }
  // Android 和桌面 Linux 上 cargokit 直接安装 `libsmelt_mobile.so`，
  // 名字就是 crate 名，默认加载逻辑本来就是对的。
  return ExternalLibrary.open(
    Platform.isWindows ? 'smelt_mobile.dll' : 'libsmelt_mobile.so',
  );
}
