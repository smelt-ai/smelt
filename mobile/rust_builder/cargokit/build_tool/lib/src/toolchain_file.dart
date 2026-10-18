/// Smelt 对 Cargokit 的补丁：让移动端构建跟随仓库钉死的 Rust 工具链。
///
/// 上游 Cargokit 在 cargokit.yaml 没配 toolchain 时一律 `rustup run stable`，
/// 这会绕过仓库根的 rust-toolchain.toml——移动端实际用的是机器上随时间漂移的
/// stable，跟桌面端、CI 钉的版本对不上，CI 预装在钉死版本上的 target 也用不到。

import 'dart:io';

import 'package:path/path.dart' as path;
import 'package:toml/toml.dart';

/// 按 rustup 的查找规则，从 [startDir] 逐级向上找 `rust-toolchain` /
/// `rust-toolchain.toml`，返回其中钉的 channel；一路找到根都没有就返回 null。
///
/// 同一目录两个文件都在时 rustup 用无扩展名的 `rust-toolchain`，这里保持一致。
String? findPinnedToolchain(String startDir) {
  var dir = path.normalize(path.absolute(startDir));
  while (true) {
    for (final name in const ['rust-toolchain', 'rust-toolchain.toml']) {
      final file = File(path.join(dir, name));
      if (file.existsSync()) {
        return parseToolchainChannel(file.readAsStringSync(), file.path);
      }
    }
    final parent = path.dirname(dir);
    if (parent == dir) {
      return null;
    }
    dir = parent;
  }
}

/// 解析 toolchain 文件内容。既支持 TOML 格式（`[toolchain] channel = "..."`），
/// 也支持旧式只写一行 channel 名的 `rust-toolchain`。
String parseToolchainChannel(String content, String filePath) {
  final trimmed = content.trim();
  if (trimmed.isNotEmpty && !trimmed.contains('\n') && !trimmed.contains('=')) {
    return trimmed;
  }
  final toolchain = TomlDocument.parse(content).toMap()['toolchain'];
  final channel = toolchain is Map ? toolchain['channel'] : null;
  if (channel is String && channel.isNotEmpty) {
    return channel;
  }
  // `path = ...` 指向的自定义工具链没法交给 `rustup run <name>`，同样按错误处理。
  throw Exception('$filePath 没有声明 [toolchain] channel，cargokit 无法据此选择工具链');
}
