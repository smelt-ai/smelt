import 'package:build_tool/src/rustup.dart';
import 'package:test/test.dart';

void main() {
  test('钉成版本号的工具链算已安装，不会再去联网安装', () {
    const output = 'stable-aarch64-apple-darwin (default)\n'
        '1.97.0-aarch64-apple-darwin (active)\n';

    expect(parseToolchainList(output), [
      'stable-aarch64-apple-darwin',
      '1.97.0-aarch64-apple-darwin',
    ]);
  });

  test('带日期的 nightly 与 beta 也认得', () {
    const output = 'nightly-2026-10-01-aarch64-apple-darwin\n'
        'beta-aarch64-apple-darwin\n';

    expect(parseToolchainList(output), [
      'nightly-2026-10-01-aarch64-apple-darwin',
      'beta-aarch64-apple-darwin',
    ]);
  });

  test('rustup toolchain link 出来的自定义工具链跳过', () {
    // 自定义工具链列不出 target，`rustup target list --toolchain` 会报错。
    const output = 'stable-aarch64-apple-darwin (default)\nmy-local\n\n';

    expect(parseToolchainList(output), ['stable-aarch64-apple-darwin']);
  });
}
