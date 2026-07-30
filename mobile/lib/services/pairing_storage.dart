import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import '../models/pairing_config.dart';

abstract interface class PairingStorage {
  Future<PairingConfig?> load();
  Future<void> save(PairingConfig pairing);
  Future<void> clear();
}

class SecurePairingStorage implements PairingStorage {
  SecurePairingStorage({FlutterSecureStorage? storage})
    : _storage = storage ?? const FlutterSecureStorage();

  static const _endpointKey = 'smelt.gateway.endpoint';
  static const _tokenKey = 'smelt.gateway.token';

  final FlutterSecureStorage _storage;

  @override
  Future<PairingConfig?> load() async {
    final values = await Future.wait([
      _storage.read(key: _endpointKey),
      _storage.read(key: _tokenKey),
    ]);
    final endpoint = values[0];
    final token = values[1];
    if (endpoint == null || token == null) return null;
    return PairingConfig.fromFields(endpoint, token);
  }

  @override
  Future<void> save(PairingConfig pairing) async {
    await _storage.write(key: _endpointKey, value: pairing.endpoint);
    await _storage.write(key: _tokenKey, value: pairing.token);
  }

  @override
  Future<void> clear() async {
    await Future.wait([
      _storage.delete(key: _endpointKey),
      _storage.delete(key: _tokenKey),
    ]);
  }
}
