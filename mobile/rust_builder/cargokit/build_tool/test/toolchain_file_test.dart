import 'dart:io';

import 'package:build_tool/src/toolchain_file.dart';
import 'package:path/path.dart' as path;
import 'package:test/test.dart';

void main() {
  late Directory root;

  setUp(() => root = Directory.systemTemp.createTempSync('toolchain_file_'));
  tearDown(() => root.deleteSync(recursive: true));

  File write(String relative, String content) {
    final file = File(path.join(root.path, relative));
    file.parent.createSync(recursive: true);
    file.writeAsStringSync(content);
    return file;
  }

  test('从 crate 目录逐级向上找到仓库根的 rust-toolchain.toml', () {
    write('rust-toolchain.toml',
        '[toolchain]\nchannel = "1.97.0"\nprofile = "minimal"\n');
    final crate = Directory(path.join(root.path, 'crates', 'smelt-mobile'))
      ..createSync(recursive: true);

    expect(findPinnedToolchain(crate.path), '1.97.0');
  });

  test('离 crate 更近的文件优先', () {
    write('rust-toolchain.toml', '[toolchain]\nchannel = "1.97.0"\n');
    write('crates/a/rust-toolchain.toml', '[toolchain]\nchannel = "nightly"\n');

    expect(findPinnedToolchain(path.join(root.path, 'crates', 'a')), 'nightly');
  });

  test('同目录两个都在时用无扩展名的 rust-toolchain（与 rustup 一致）', () {
    write('rust-toolchain.toml', '[toolchain]\nchannel = "1.97.0"\n');
    write('rust-toolchain', '1.80.0\n');

    expect(findPinnedToolchain(root.path), '1.80.0');
  });

  test('旧式单行 rust-toolchain', () {
    expect(parseToolchainChannel('stable\n', 'rust-toolchain'), 'stable');
  });

  test('没有 channel（如 path 工具链）时报错，而不是悄悄退回 stable', () {
    expect(
      () => parseToolchainChannel(
          '[toolchain]\npath = "/opt/rust"\n', 'rust-toolchain.toml'),
      throwsException,
    );
  });
}
