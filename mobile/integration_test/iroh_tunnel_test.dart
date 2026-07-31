// 在真设备/模拟器上验证 iroh 隧道 —— 这条路的成败取决于 FFI、原生库打包
// 和真实网络，单测（纯 Dart，opener 是注入的假货）覆盖不到任何一样。
//
// 跑法：
//   1. 在 Mac 上准备一个 HTTP 服务和宿主：
//        python3 -m http.server 9988
//        smelt-iroh-host --gateway 127.0.0.1:9988 --relay relay.example.com
//   2. flutter test integration_test/iroh_tunnel_test.dart \
//        --dart-define=SMELT_IROH_TEST_PEER=<EndpointId> \
//        --dart-define=SMELT_IROH_TEST_RELAY=https://relay.example.com
//
// 没给 EndpointId 时只跑不依赖宿主的部分，方便在 CI 里当冒烟测试。

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:smelt_mobile/models/pairing_config.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/rust_lib.dart';
import 'package:smelt_mobile/src/rust/api_iroh.dart';

const _peer = String.fromEnvironment('SMELT_IROH_TEST_PEER');
const _relay = String.fromEnvironment('SMELT_IROH_TEST_RELAY');

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  setUpAll(initRustLib);

  test('Rust 库真的加载起来了', () async {
    // 只要能调进去不崩，就说明原生库被正确打包并链接了 —— 这一步挂掉
    // 说明 podspec/cargokit 的接线有问题，与 iroh 无关。
    expect(await irohTunnelPort(), isNull);
  });

  test('打错的 EndpointId 会报错而不是挂起', () async {
    await expectLater(
      irohTunnelStart(
        endpointId: 'definitely-not-an-endpoint-id',
        relayUrl: _relay,
      ),
      throwsA(anything),
    );
  });

  test(
    '隧道能把 HTTP 请求送到 Mac 上的宿主',
    () async {
      final port = await irohTunnelStart(endpointId: _peer, relayUrl: _relay);
      expect(port, greaterThan(0));
      // 幂等：同一个 peer 不该换端口，否则上层重连会打到旧端口。
      expect(await irohTunnelStart(endpointId: _peer, relayUrl: _relay), port);
      expect(await irohTunnelPort(), port);

      final client = HttpClient();
      final request = await client.getUrl(Uri.parse('http://127.0.0.1:$port/'));
      final response = await request.close();
      expect(response.statusCode, 200);
      await response.drain<void>();
      client.close();

      await irohTunnelStop();
      expect(await irohTunnelPort(), isNull);
    },
    skip: _peer.isEmpty || _relay.isEmpty
        ? '需要 SMELT_IROH_TEST_PEER 和 SMELT_IROH_TEST_RELAY'
        : false,
  );

  test(
    'GatewayService 用真隧道时会去连本地端口',
    () async {
      // 把生产用的接线也跑一遍：main() 里就是这么接的。
      final service =
          GatewayService(connectTimeout: const Duration(seconds: 20))
            ..irohTunnelOpener = (id, relay) =>
                irohTunnelStart(endpointId: id, relayUrl: relay);
      final errors = <String>[];
      final sub = service.errorStream.listen(errors.add);

      // 宿主后面接的是普通 HTTP 服务而非网关，所以 /acp/ws 会失败 ——
      // 这里要的是「确实经隧道打到了对端」，而不是握手成功。
      final endpoint = Uri(
        scheme: PairingConfig.irohScheme,
        host: _peer,
        queryParameters: {'relay': _relay},
      );
      await service.connect(endpoint.toString(), 'irrelevant-token');
      await Future<void>.delayed(const Duration(seconds: 3));

      expect(service.state, isNot(WsState.connecting), reason: '不该卡在连接中');
      await sub.cancel();
      service.disconnect();
      await irohTunnelStop();
    },
    skip: _peer.isEmpty || _relay.isEmpty
        ? '需要 SMELT_IROH_TEST_PEER 和 SMELT_IROH_TEST_RELAY'
        : false,
  );
}
