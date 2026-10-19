import 'dart:convert';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import '../models/runtime_models.dart';
import '../models/saved_runtime.dart';

/// 配对过的运行时列表。存在系统钥匙串里；新配对、切换、改名、忘记都返回整份
/// 最新列表，调用方直接拿它刷新界面。
abstract interface class PairingStorage {
  Future<SavedRuntimeCollection> load();

  /// 配对成功后记下这个运行时并设为当前。同一个运行时再配对一次只更新地址，
  /// 保留用户起的名字。
  Future<SavedRuntimeCollection> save(RuntimeInfo info);
  Future<SavedRuntimeCollection> setActive(String runtimeId);
  Future<SavedRuntimeCollection> rename(String runtimeId, String name);

  /// 只删本机记录。运行时那边的设备授权由运行时管理。
  Future<SavedRuntimeCollection> remove(String runtimeId);
}

class SecurePairingStorage implements PairingStorage {
  SecurePairingStorage({FlutterSecureStorage? storage})
    : _storage = storage ?? const FlutterSecureStorage();

  static const _collectionKey = 'smelt.runtimes.v1';

  /// 旧网关配对（token）留下的数据。新协议用不上，读到就删，不迁移。
  static const _retiredKeys = [
    'smelt.gateway.endpoint',
    'smelt.gateway.token',
    'smelt.gateway.desktops.v1',
  ];

  final FlutterSecureStorage _storage;
  bool _retiredKeysDeleted = false;

  @override
  Future<SavedRuntimeCollection> load() async {
    if (!_retiredKeysDeleted) {
      await Future.wait(_retiredKeys.map((key) => _storage.delete(key: key)));
      _retiredKeysDeleted = true;
    }
    final encoded = await _storage.read(key: _collectionKey);
    if (encoded == null) return const SavedRuntimeCollection();
    final decoded = jsonDecode(encoded);
    if (decoded is! Map) {
      throw const FormatException('Invalid saved runtime data');
    }
    return SavedRuntimeCollection.fromJson(
      decoded.map((key, value) => MapEntry(key.toString(), value)),
    );
  }

  @override
  Future<SavedRuntimeCollection> save(RuntimeInfo info) async {
    final current = await load();
    final now = DateTime.now().toUtc();
    final existing = current.runtimes
        .where((item) => item.runtimeId == info.runtimeId)
        .firstOrNull;
    final saved = SavedRuntime(
      runtimeId: info.runtimeId,
      name: existing?.name ?? defaultRuntimeName(info),
      endpointId: info.endpointId,
      relayUrl: info.relayUrl,
      lastUsedAt: now,
    );
    return _write(
      SavedRuntimeCollection(
        runtimes: [
          saved,
          ...current.runtimes.where(
            (item) => item.runtimeId != saved.runtimeId,
          ),
        ],
        activeRuntimeId: saved.runtimeId,
      ),
    );
  }

  @override
  Future<SavedRuntimeCollection> setActive(String runtimeId) async {
    final current = await load();
    final runtime = current.runtimes
        .where((item) => item.runtimeId == runtimeId)
        .firstOrNull;
    if (runtime == null) return current;
    return _write(
      SavedRuntimeCollection(
        runtimes: [
          runtime.copyWith(lastUsedAt: DateTime.now().toUtc()),
          ...current.runtimes.where((item) => item.runtimeId != runtimeId),
        ],
        activeRuntimeId: runtimeId,
      ),
    );
  }

  @override
  Future<SavedRuntimeCollection> rename(String runtimeId, String name) async {
    final trimmed = name.trim();
    final current = await load();
    if (trimmed.isEmpty) return current;
    return _write(
      SavedRuntimeCollection(
        runtimes: current.runtimes
            .map(
              (item) => item.runtimeId == runtimeId
                  ? item.copyWith(name: trimmed)
                  : item,
            )
            .toList(),
        activeRuntimeId: current.activeRuntimeId,
      ),
    );
  }

  @override
  Future<SavedRuntimeCollection> remove(String runtimeId) async {
    final current = await load();
    final remaining = current.runtimes
        .where((item) => item.runtimeId != runtimeId)
        .toList();
    return _write(
      SavedRuntimeCollection(
        runtimes: remaining,
        activeRuntimeId: current.activeRuntimeId == runtimeId
            ? remaining.firstOrNull?.runtimeId
            : current.activeRuntimeId,
      ),
    );
  }

  Future<SavedRuntimeCollection> _write(
    SavedRuntimeCollection collection,
  ) async {
    final normalized = collection.normalized();
    await _storage.write(
      key: _collectionKey,
      value: jsonEncode(normalized.toJson()),
    );
    return normalized;
  }
}
