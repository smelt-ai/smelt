import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/pairing_config.dart';
import 'package:smelt_mobile/services/pairing_storage.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  setUp(() {
    FlutterSecureStorage.setMockInitialValues({});
  });

  test(
    'migrates the legacy single pairing without requiring another scan',
    () async {
      FlutterSecureStorage.setMockInitialValues({
        'smelt.gateway.endpoint': 'https://old-mac.example/',
        'smelt.gateway.token': 'legacy-token',
      });
      const secureStorage = FlutterSecureStorage();
      final storage = SecurePairingStorage(storage: secureStorage);

      final saved = await storage.load();

      expect(saved.desktops, hasLength(1));
      expect(saved.activeDesktop?.pairing.token, 'legacy-token');
      expect(
        await secureStorage.read(key: 'smelt.gateway.desktops.v1'),
        isNotNull,
      );
      expect(await secureStorage.read(key: 'smelt.gateway.endpoint'), isNull);
      expect(await secureStorage.read(key: 'smelt.gateway.token'), isNull);
    },
  );

  test('stores, switches, renames, and removes multiple desktops', () async {
    final storage = SecurePairingStorage(storage: const FlutterSecureStorage());
    const first = PairingConfig(
      endpoint: 'https://mac-a.example/',
      token: 'token-a',
    );
    const second = PairingConfig(
      endpoint: 'https://mac-b.example/',
      token: 'token-b',
    );

    final firstSaved = await storage.save(first);
    final firstId = firstSaved.activeDesktopId!;
    final bothSaved = await storage.save(second);
    final secondId = bothSaved.activeDesktopId!;

    expect(bothSaved.desktops, hasLength(2));
    expect(bothSaved.activeDesktop?.pairing, same(second));

    final switched = await storage.setActive(firstId);
    expect(switched.activeDesktopId, firstId);

    final renamed = await storage.rename(firstId, 'Office Mac');
    expect(renamed.activeDesktop?.name, 'Office Mac');

    final removed = await storage.remove(firstId);
    expect(removed.desktops, hasLength(1));
    expect(removed.activeDesktopId, secondId);
  });

  test(
    'refreshing a token updates the existing desktop and keeps its name',
    () async {
      final storage = SecurePairingStorage(
        storage: const FlutterSecureStorage(),
      );
      const oldPairing = PairingConfig(
        endpoint: 'https://same-mac.example/',
        token: 'old-token',
      );
      const newPairing = PairingConfig(
        endpoint: 'https://same-mac.example/',
        token: 'new-token',
      );

      final initial = await storage.save(oldPairing);
      await storage.rename(initial.activeDesktopId!, 'Home Mac');
      final refreshed = await storage.save(newPairing);

      expect(refreshed.desktops, hasLength(1));
      expect(refreshed.activeDesktop?.name, 'Home Mac');
      expect(refreshed.activeDesktop?.pairing.token, 'new-token');
    },
  );

  test('changing an iroh relay does not duplicate the same desktop', () async {
    final storage = SecurePairingStorage(storage: const FlutterSecureStorage());
    const endpointId =
        '932c4c220140d34e117cca1f868167a2d2e575708f85bf01f4782b30f9363542';
    final oldPairing = PairingConfig.parse(
      'smelt+iroh://$endpointId/?relay=https%3A%2F%2Fold.example&token=old',
    );
    final newPairing = PairingConfig.parse(
      'smelt+iroh://$endpointId/?relay=https%3A%2F%2Fnew.example&token=new',
    );

    await storage.save(oldPairing);
    final refreshed = await storage.save(newPairing);

    expect(refreshed.desktops, hasLength(1));
    expect(
      refreshed.activeDesktop?.pairing.irohRelayUrl,
      'https://new.example',
    );
    expect(refreshed.activeDesktop?.pairing.token, 'new');
  });
}
