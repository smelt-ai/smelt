// 在真设备/模拟器上确认 Rust 内核被正确打包并能初始化——这一步取决于原生库
// 打包（cargokit / podspec）和 App 数据目录，单测（注入假 backend）覆盖不到。
//
// 跑法：flutter test integration_test/runtime_client_test.dart -d <设备>
//
// 只验证不联网的部分：加载动态库、生成并持久化设备身份、解析配对串。连运行时、
// 配对要真机加真运行时，手动验证。

import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart';
import 'package:smelt_mobile/rust_lib.dart';
import 'package:smelt_mobile/src/rust/api/error.dart';
import 'package:smelt_mobile/src/rust/api/identity.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  setUpAll(initRustLib);

  test('Rust 库加载后能初始化设备身份，且同一目录身份不变', () async {
    final dataDir = (await getApplicationSupportDirectory()).path;
    final first = await initClient(dataDir: dataDir);
    final second = await initClient(dataDir: dataDir);

    expect(first.endpointId, isNotEmpty);
    expect(second.endpointId, first.endpointId);
  });

  test('不认识的配对串同步报 invalidPairing，不联网', () {
    expect(
      () => parsePairing(uri: 'https://example.com/?token=legacy'),
      throwsA(
        isA<SmeltError>().having(
          (error) => error.kind,
          'kind',
          SmeltErrorKind.invalidPairing,
        ),
      ),
    );
  });
}
