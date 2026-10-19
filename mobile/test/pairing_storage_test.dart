import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/saved_runtime.dart';
import 'package:smelt_mobile/services/pairing_storage.dart';

import 'support/runtime_fixtures.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  setUp(() {
    FlutterSecureStorage.setMockInitialValues({});
  });

  test('旧网关配对留下的 token 数据直接删掉，不迁移', () async {
    FlutterSecureStorage.setMockInitialValues({
      'smelt.gateway.endpoint': 'https://old-mac.example/',
      'smelt.gateway.token': 'legacy-token',
      'smelt.gateway.desktops.v1': '{"desktops":[]}',
    });
    const secureStorage = FlutterSecureStorage();
    final storage = SecurePairingStorage(storage: secureStorage);

    final saved = await storage.load();

    expect(saved.runtimes, isEmpty);
    expect(saved.activeRuntime, isNull);
    for (final key in [
      'smelt.gateway.endpoint',
      'smelt.gateway.token',
      'smelt.gateway.desktops.v1',
    ]) {
      expect(await secureStorage.read(key: key), isNull, reason: key);
    }
  });

  test('配对过的运行时按 runtime id 存，能切换、改名、忘记', () async {
    final storage = SecurePairingStorage(storage: const FlutterSecureStorage());

    final firstSaved = await storage.save(
      runtimeInfo(runtimeId: 'a', hostname: 'office-mac', endpointId: 'ea'),
    );
    expect(firstSaved.activeRuntime?.name, 'office-mac');
    final bothSaved = await storage.save(
      runtimeInfo(
        runtimeId: 'b',
        hostname: 'linux-box',
        endpointId: 'eb',
        relayUrl: 'https://relay.example',
      ),
    );

    expect(bothSaved.runtimes, hasLength(2));
    expect(bothSaved.activeRuntimeId, 'b');
    expect(bothSaved.activeRuntime?.endpointId, 'eb');
    expect(bothSaved.activeRuntime?.relayUrl, 'https://relay.example');

    final switched = await storage.setActive('a');
    expect(switched.activeRuntimeId, 'a');
    expect(switched.runtimes.first.runtimeId, 'a', reason: '最近用过的在前');

    final renamed = await storage.rename('a', 'Office');
    expect(renamed.activeRuntime?.name, 'Office');

    final removed = await storage.remove('a');
    expect(removed.runtimes, hasLength(1));
    expect(removed.activeRuntimeId, 'b');

    // 重新读一遍，确认落盘的就是这份。
    final reloaded = await SecurePairingStorage(
      storage: const FlutterSecureStorage(),
    ).load();
    expect(reloaded.runtimes.single.runtimeId, 'b');
    expect(reloaded.activeRuntimeId, 'b');
  });

  test('同一个运行时再配对只更新地址，保留用户起的名字', () async {
    final storage = SecurePairingStorage(storage: const FlutterSecureStorage());

    await storage.save(
      runtimeInfo(
        runtimeId: 'a',
        endpointId: 'old-endpoint',
        relayUrl: 'https://old.example',
      ),
    );
    await storage.rename('a', 'Home Mac');
    final refreshed = await storage.save(
      runtimeInfo(runtimeId: 'a', endpointId: 'new-endpoint'),
    );

    expect(refreshed.runtimes, hasLength(1));
    expect(refreshed.activeRuntime?.name, 'Home Mac');
    expect(refreshed.activeRuntime?.endpointId, 'new-endpoint');
    expect(refreshed.activeRuntime?.relayUrl, isNull);
  });

  test('没有主机名的老运行时用 endpoint 前缀当名字', () {
    final runtime = SavedRuntime.fromInfo(
      runtimeInfo(hostname: null, endpointId: '932c4c220140d34e'),
    );
    expect(runtime.name, 'Runtime 932c4c22');
  });
}
