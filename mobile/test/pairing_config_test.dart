import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/pairing_config.dart';

void main() {
  group('PairingConfig', () {
    test('parses a desktop sharing URL', () {
      final pairing = PairingConfig.parse(
        'http://192.168.1.20:59903/?token=secret-token',
      );

      expect(pairing.endpoint, 'http://192.168.1.20:59903/');
      expect(pairing.token, 'secret-token');
    });

    test('parses the legacy JSON pairing payload', () {
      final pairing = PairingConfig.parse('''
        {
          "endpoint": "wss://smelt.example.test",
          "token": "secret-token",
          "publicKey": "unused-by-websocket-client",
          "name": "Mac"
        }
      ''');

      expect(pairing.endpoint, 'wss://smelt.example.test');
      expect(pairing.token, 'secret-token');
    });

    test('retains unrelated endpoint query parameters', () {
      final pairing = PairingConfig.parse(
        'https://example.test/gateway?region=cn&token=secret-token',
      );

      expect(pairing.endpoint, 'https://example.test/gateway?region=cn');
    });

    test('rejects retired WebRTC sharing codes with a useful error', () {
      expect(
        () => PairingConfig.parse(
          'https://signal.example.test/?room=abc&signal=wss%3A%2F%2Fsignal&token=x',
        ),
        throwsA(
          isA<FormatException>().having(
            (error) => error.message,
            'message',
            contains('no longer supported'),
          ),
        ),
      );
    });

    test('rejects URLs without a token', () {
      expect(
        () => PairingConfig.parse('ws://192.168.1.20:59903'),
        throwsFormatException,
      );
    });

    test('accepts cleartext for local network gateways', () {
      for (final endpoint in const [
        'http://localhost:9877/?token=t',
        'http://127.0.0.1:9877/?token=t',
        'ws://10.0.2.2:9877/?token=t',
        'ws://172.20.3.4:9877/?token=t',
        'http://192.168.1.20:9877/?token=t',
        'http://mac.local:9877/?token=t',
        'ws://[fe80::1]:9877/?token=t',
      ]) {
        expect(
          PairingConfig.parse(endpoint).token,
          't',
          reason: 'should accept $endpoint',
        );
      }
    });

    test('rejects cleartext to hosts outside the local network', () {
      for (final endpoint in const [
        'http://smelt.example.test/?token=secret-token',
        'ws://8.8.8.8:9877/?token=secret-token',
        'http://172.32.0.1:9877/?token=secret-token',
      ]) {
        expect(
          () => PairingConfig.parse(endpoint),
          throwsA(
            isA<FormatException>().having(
              (error) => error.message,
              'message',
              contains('cleartext'),
            ),
          ),
          reason: 'should reject $endpoint',
        );
      }
    });

    test('still allows TLS to public hosts', () {
      final pairing = PairingConfig.parse(
        'https://smelt.example.test/?token=secret-token',
      );
      expect(pairing.endpoint, 'https://smelt.example.test/');
    });

    test('rejects cleartext entered manually in the connect form', () {
      expect(
        () =>
            PairingConfig.fromFields('ws://smelt.example.test', 'secret-token'),
        throwsA(
          isA<FormatException>().having(
            (error) => error.message,
            'message',
            contains('cleartext'),
          ),
        ),
      );
    });

    test('parses an iroh pairing code', () {
      final pairing = PairingConfig.parse(
        'smelt+iroh://k7d3ffb1c9a24e5f/?token=secret-token&relay=https%3A%2F%2Frelay.example.test&relay_token=relay-secret',
      );
      expect(pairing.isIroh, isTrue);
      expect(pairing.irohEndpointId, 'k7d3ffb1c9a24e5f');
      expect(pairing.token, 'secret-token');
      expect(pairing.irohRelayUrl, 'https://relay.example.test');
      expect(pairing.endpoint, isNot(contains('relay_token')));
      expect(
        pairing.endpoint,
        contains('relay=https%3A%2F%2Frelay.example.test'),
      );
    });

    test('iroh codes are exempt from the cleartext rule', () {
      // 这一路的机密性由 QUIC 保证，不该被 http/ws 那套判定拦下。
      expect(
        () => PairingConfig.parse(
          'smelt+iroh://k7d3ffb1c9a24e5f/?token=t&relay=https%3A%2F%2Frelay.test',
        ),
        returnsNormally,
      );
    });

    test('rejects an iroh code without a token', () {
      expect(
        () => PairingConfig.parse('smelt+iroh://k7d3ffb1c9a24e5f/'),
        throwsA(isA<FormatException>()),
      );
    });

    test('rejects an iroh code without an endpoint id', () {
      expect(
        () => PairingConfig.parse(
          'smelt+iroh:///?token=t&relay=https%3A%2F%2Frelay.test',
        ),
        throwsA(isA<FormatException>()),
      );
    });

    test('rejects an iroh code without relay configuration', () {
      expect(
        () => PairingConfig.parse('smelt+iroh://k7d3ffb1c9a24e5f/?token=t'),
        throwsA(isA<FormatException>()),
      );
    });

    test('non-iroh pairings report themselves as such', () {
      final pairing = PairingConfig.parse(
        'http://192.168.1.20:59903/?token=secret-token',
      );
      expect(pairing.isIroh, isFalse);
      expect(pairing.irohEndpointId, isNull);
    });
  });
}
