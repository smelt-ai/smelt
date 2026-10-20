import 'package:flutter/widgets.dart' show Locale;
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/features/machines/machine_text.dart';
import 'package:smelt_mobile/features/machines/pairing.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';

FleetView _fleet(List<String?> relays) => FleetView(
  runtimes: [
    for (final (i, relay) in relays.indexed)
      FleetRuntime(
        record: RuntimeRecord(
          runtimeId: 'rt-$i',
          name: 'm$i',
          endpointId: 'ep-$i',
          relayUrl: relay,
          version: '0.10.0',
          accent: i,
          position: i,
          pairedAtMs: 0,
        ),
        status: const RuntimeStatus(state: ConnectionState.connected),
      ),
  ],
);

/// 解析配对串时按要求抛错的演示后端。
class _ParseBackend extends DemoBackend {
  _ParseBackend(this.error);

  final Object? error;

  @override
  PairingTarget parsePairing(String uri) {
    final e = error;
    if (e != null) throw e;
    return super.parsePairing(uri);
  }
}

void main() {
  final l10n = lookupL10n(const Locale('en'));

  group('relaySetupMixed', () {
    test('都没指定、或都指定同一个中继时不算混用', () {
      expect(relaySetupMixed(_fleet([null, null])), isFalse);
      expect(
        relaySetupMixed(_fleet(['https://r.example', 'https://r.example'])),
        isFalse,
      );
      expect(relaySetupMixed(_fleet(['https://r.example'])), isFalse);
      expect(relaySetupMixed(_fleet([])), isFalse);
    });

    test('有的指定有的没有、或指定了不同的中继时算混用', () {
      expect(relaySetupMixed(_fleet(['https://r.example', null])), isTrue);
      expect(
        relaySetupMixed(_fleet(['https://a.example', 'https://b.example'])),
        isTrue,
      );
    });
  });

  group('配对失败的说法', () {
    SmeltError e(SmeltErrorKind kind) => SmeltError(kind: kind, message: 'x');

    test('按错误类别给出原因和下一步', () {
      expect(
        pairingFailureText(l10n, e(SmeltErrorKind.invalidPairing)),
        l10n.machinesPairErrorExpired,
      );
      expect(
        pairingFailureText(l10n, e(SmeltErrorKind.incompatible)),
        l10n.machinesPairErrorIncompatible,
      );
      expect(
        pairingFailureText(l10n, e(SmeltErrorKind.offline)),
        l10n.machinesPairErrorOffline,
      );
      expect(
        pairingFailureText(l10n, e(SmeltErrorKind.unpaired)),
        l10n.machinesPairErrorRejected,
      );
      expect(
        pairingFailureText(
          l10n,
          const SmeltError(kind: SmeltErrorKind.other, message: 'boom'),
        ),
        'Pairing failed: boom',
      );
    });

    test('本地校验：合法返回 null；扫到别的码说「不是配对码」，版本问题照实说', () {
      expect(
        checkPairingCode(_ParseBackend(null), l10n, 'smelt+iroh://ok'),
        isNull,
      );
      expect(
        checkPairingCode(
          _ParseBackend(e(SmeltErrorKind.invalidPairing)),
          l10n,
          'https://example.com',
        ),
        l10n.machinesPairNotACode,
      );
      expect(
        checkPairingCode(
          _ParseBackend(e(SmeltErrorKind.incompatible)),
          l10n,
          'smelt+iroh://new',
        ),
        l10n.machinesPairErrorIncompatible,
      );
    });
  });

  test('系统名认不出时原样给出，没有才说未知', () {
    expect(osLabel(l10n, 'macos'), 'macOS');
    expect(osLabel(l10n, 'Linux'), 'Linux');
    expect(osLabel(l10n, 'freebsd'), 'freebsd');
    expect(osLabel(l10n, null), l10n.machinesOsUnknown);
    expect(osLabel(l10n, ' '), l10n.machinesOsUnknown);
  });
}
