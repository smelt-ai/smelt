/// This is copied from Cargokit (which is the official way to use it currently)
/// Details: https://fzyzcjy.github.io/flutter_rust_bridge/manual/integrate/builtin

import 'dart:io';

import 'package:collection/collection.dart';
import 'package:path/path.dart' as path;

import 'util.dart';

/// 从 `rustup toolchain list` 的输出里取出非自定义工具链的名字。
///
/// 上游只认 stable/beta/nightly 开头的行，漏掉了铉成版本号的工具链（如
/// `1.97.0-aarch64-apple-darwin`）。仓库用 rust-toolchain.toml 铉了版本，漏认会让每次构建
/// 都去跑 `rustup toolchain install`，网络不通时整个构建失败。
/// 自定义（`rustup toolchain link`）工具链仍然跳过：它们列不出 target。
List<String> parseToolchainList(String output) {
  final nonCustom = RegExp(r'^(stable|beta|nightly|\d+\.\d+)');
  return output
      .split('\n')
      .where((line) => line.isNotEmpty && line.startsWith(nonCustom))
      // 去掉名字后面的 (default) / (active)
      .map((line) => line.split(' ').first)
      .toList(growable: true);
}

class _Toolchain {
  _Toolchain(
    this.name,
    this.targets,
  );

  final String name;
  final List<String> targets;
}

class Rustup {
  List<String>? installedTargets(String toolchain) {
    final targets = _installedTargets(toolchain);
    return targets != null ? List.unmodifiable(targets) : null;
  }

  void installToolchain(String toolchain) {
    log.info("Installing Rust toolchain: $toolchain");
    runCommand("rustup", ['toolchain', 'install', toolchain]);
    _installedToolchains
        .add(_Toolchain(toolchain, _getInstalledTargets(toolchain)));
  }

  void installTarget(
    String target, {
    required String toolchain,
  }) {
    log.info("Installing Rust target: $target");
    runCommand("rustup", ['target', 'add', '--toolchain', toolchain, target]);
    _installedTargets(toolchain)?.add(target);
  }

  bool _didInstallZigBuild = false;

  void installZigBuild(String toolchain) {
    if (_didInstallZigBuild) {
      return;
    }

    log.info("Installing Zig build");
    runCommand("rustup", [
      'run',
      toolchain,
      'cargo',
      'install',
      '--locked',
      'cargo-zigbuild',
    ]);
    _didInstallZigBuild = true;
  }

  final List<_Toolchain> _installedToolchains;

  Rustup() : _installedToolchains = _getInstalledToolchains();

  List<String>? _installedTargets(String toolchain) => _installedToolchains
      .firstWhereOrNull(
          (e) => e.name == toolchain || e.name.startsWith('$toolchain-'))
      ?.targets;

  static List<_Toolchain> _getInstalledToolchains() {
    final res = runCommand("rustup", ['toolchain', 'list']);
    return parseToolchainList(res.stdout.toString())
        .map(
          (name) => _Toolchain(
            name,
            _getInstalledTargets(name),
          ),
        )
        .toList(growable: true);
  }

  static List<String> _getInstalledTargets(String toolchain) {
    final res = runCommand("rustup", [
      'target',
      'list',
      '--toolchain',
      toolchain,
      '--installed',
    ]);
    final lines = res.stdout
        .toString()
        .split('\n')
        .where((e) => e.isNotEmpty)
        .toList(growable: true);
    return lines;
  }

  bool _didInstallRustSrcForNightly = false;

  void installRustSrcForNightly() {
    if (_didInstallRustSrcForNightly) {
      return;
    }
    // Useful for -Z build-std
    runCommand(
      "rustup",
      ['component', 'add', 'rust-src', '--toolchain', 'nightly'],
    );
    _didInstallRustSrcForNightly = true;
  }

  static String? executablePath() {
    final envPath = Platform.environment['PATH'];
    final envPathSeparator = Platform.isWindows ? ';' : ':';
    final home = Platform.isWindows
        ? Platform.environment['USERPROFILE']
        : Platform.environment['HOME'];
    final paths = [
      if (home != null) path.join(home, '.cargo', 'bin'),
      if (envPath != null) ...envPath.split(envPathSeparator),
    ];
    for (final p in paths) {
      final rustup = Platform.isWindows ? 'rustup.exe' : 'rustup';
      final rustupPath = path.join(p, rustup);
      if (File(rustupPath).existsSync()) {
        return rustupPath;
      }
    }
    return null;
  }
}
