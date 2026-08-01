import 'dart:convert';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import '../models/pairing_config.dart';
import '../models/saved_desktop.dart';

abstract interface class PairingStorage {
  Future<SavedDesktopCollection> load();
  Future<SavedDesktopCollection> save(PairingConfig pairing);
  Future<SavedDesktopCollection> setActive(String desktopId);
  Future<SavedDesktopCollection> rename(String desktopId, String name);
  Future<SavedDesktopCollection> remove(String desktopId);
}

class SecurePairingStorage implements PairingStorage {
  SecurePairingStorage({FlutterSecureStorage? storage})
    : _storage = storage ?? const FlutterSecureStorage();

  static const _endpointKey = 'smelt.gateway.endpoint';
  static const _tokenKey = 'smelt.gateway.token';
  static const _collectionKey = 'smelt.gateway.desktops.v1';

  final FlutterSecureStorage _storage;

  @override
  Future<SavedDesktopCollection> load() async {
    final encoded = await _storage.read(key: _collectionKey);
    if (encoded != null) {
      final decoded = jsonDecode(encoded);
      if (decoded is! Map) {
        throw const FormatException('Invalid saved desktop data');
      }
      return SavedDesktopCollection.fromJson(
        decoded.map((key, value) => MapEntry(key.toString(), value)),
      );
    }

    final values = await Future.wait([
      _storage.read(key: _endpointKey),
      _storage.read(key: _tokenKey),
    ]);
    final endpoint = values[0];
    final token = values[1];
    if (endpoint == null || token == null) {
      return const SavedDesktopCollection();
    }

    final desktop = SavedDesktop.create(
      PairingConfig.fromFields(endpoint, token),
    );
    final migrated = SavedDesktopCollection(
      desktops: [desktop],
      activeDesktopId: desktop.id,
    );
    await _write(migrated);
    await Future.wait([
      _storage.delete(key: _endpointKey),
      _storage.delete(key: _tokenKey),
    ]);
    return migrated;
  }

  @override
  Future<SavedDesktopCollection> save(PairingConfig pairing) async {
    final current = await load();
    final id = desktopIdFor(pairing);
    final now = DateTime.now().toUtc();
    final existing = current.desktops
        .where((item) => item.id == id)
        .firstOrNull;
    final saved = existing == null
        ? SavedDesktop.create(pairing, lastUsedAt: now)
        : existing.copyWith(pairing: pairing, lastUsedAt: now);
    final updated = SavedDesktopCollection(
      desktops: [saved, ...current.desktops.where((item) => item.id != id)],
      activeDesktopId: id,
    ).normalized();
    await _write(updated);
    return updated;
  }

  @override
  Future<SavedDesktopCollection> setActive(String desktopId) async {
    final current = await load();
    final desktop = current.desktops
        .where((item) => item.id == desktopId)
        .firstOrNull;
    if (desktop == null) return current;
    final updated = SavedDesktopCollection(
      desktops: [
        desktop.copyWith(lastUsedAt: DateTime.now().toUtc()),
        ...current.desktops.where((item) => item.id != desktopId),
      ],
      activeDesktopId: desktopId,
    ).normalized();
    await _write(updated);
    return updated;
  }

  @override
  Future<SavedDesktopCollection> rename(String desktopId, String name) async {
    final trimmedName = name.trim();
    if (trimmedName.isEmpty) return load();
    final current = await load();
    final updated = SavedDesktopCollection(
      desktops: current.desktops
          .map(
            (item) =>
                item.id == desktopId ? item.copyWith(name: trimmedName) : item,
          )
          .toList(),
      activeDesktopId: current.activeDesktopId,
    ).normalized();
    await _write(updated);
    return updated;
  }

  @override
  Future<SavedDesktopCollection> remove(String desktopId) async {
    final current = await load();
    final remaining = current.desktops
        .where((item) => item.id != desktopId)
        .toList();
    final updated = SavedDesktopCollection(
      desktops: remaining,
      activeDesktopId: current.activeDesktopId == desktopId
          ? remaining.firstOrNull?.id
          : current.activeDesktopId,
    ).normalized();
    await _write(updated);
    return updated;
  }

  Future<void> _write(SavedDesktopCollection collection) {
    return _storage.write(
      key: _collectionKey,
      value: jsonEncode(collection.toJson()),
    );
  }
}
