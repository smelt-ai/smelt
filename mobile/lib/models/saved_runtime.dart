import 'runtime_models.dart';

/// 这台手机配对过的一个运行时。
///
/// 只存找到它所需的几项：运行时 id、endpoint 和 relay；没有 token——运行时按这台
/// 手机的设备密钥认人，密钥由 Rust 内核保存在 App 数据目录里。
class SavedRuntime {
  const SavedRuntime({
    required this.runtimeId,
    required this.name,
    required this.endpointId,
    this.relayUrl,
    required this.lastUsedAt,
  });

  final String runtimeId;

  /// 界面上的名字。默认取运行时机器的主机名，用户可以改。
  final String name;
  final String endpointId;
  final String? relayUrl;
  final DateTime lastUsedAt;

  /// 刚配对上的运行时。名字默认用主机名，老运行时没有主机名就用 endpoint 前缀。
  factory SavedRuntime.fromInfo(RuntimeInfo info, {DateTime? lastUsedAt}) =>
      SavedRuntime(
        runtimeId: info.runtimeId,
        name: defaultRuntimeName(info),
        endpointId: info.endpointId,
        relayUrl: info.relayUrl,
        lastUsedAt: lastUsedAt ?? DateTime.now().toUtc(),
      );

  /// 读回存储的数据。缺关键字段的记录返回 null，直接丢掉。
  static SavedRuntime? fromJson(Map<String, dynamic> json) {
    final runtimeId = (json['runtimeId'] as String? ?? '').trim();
    final endpointId = (json['endpointId'] as String? ?? '').trim();
    if (runtimeId.isEmpty || endpointId.isEmpty) return null;
    final name = (json['name'] as String? ?? '').trim();
    final relayUrl = (json['relayUrl'] as String?)?.trim();
    return SavedRuntime(
      runtimeId: runtimeId,
      name: name.isEmpty ? _endpointName(endpointId) : name,
      endpointId: endpointId,
      relayUrl: relayUrl == null || relayUrl.isEmpty ? null : relayUrl,
      lastUsedAt:
          DateTime.tryParse(json['lastUsedAt'] as String? ?? '')?.toUtc() ??
          DateTime.fromMillisecondsSinceEpoch(0, isUtc: true),
    );
  }

  SavedRuntime copyWith({String? name, DateTime? lastUsedAt}) => SavedRuntime(
    runtimeId: runtimeId,
    name: name ?? this.name,
    endpointId: endpointId,
    relayUrl: relayUrl,
    lastUsedAt: lastUsedAt ?? this.lastUsedAt,
  );

  Map<String, dynamic> toJson() => {
    'runtimeId': runtimeId,
    'name': name,
    'endpointId': endpointId,
    if (relayUrl != null) 'relayUrl': relayUrl,
    'lastUsedAt': lastUsedAt.toUtc().toIso8601String(),
  };
}

class SavedRuntimeCollection {
  const SavedRuntimeCollection({
    this.runtimes = const [],
    this.activeRuntimeId,
  });

  /// 最近用过的在前。
  final List<SavedRuntime> runtimes;
  final String? activeRuntimeId;

  SavedRuntime? get activeRuntime {
    final activeId = activeRuntimeId;
    if (activeId == null) return null;
    for (final runtime in runtimes) {
      if (runtime.runtimeId == activeId) return runtime;
    }
    return null;
  }

  SavedRuntimeCollection normalized() {
    final sorted = List<SavedRuntime>.of(runtimes)
      ..sort((a, b) => b.lastUsedAt.compareTo(a.lastUsedAt));
    final activeId = sorted.any((item) => item.runtimeId == activeRuntimeId)
        ? activeRuntimeId
        : sorted.firstOrNull?.runtimeId;
    return SavedRuntimeCollection(
      runtimes: List.unmodifiable(sorted),
      activeRuntimeId: activeId,
    );
  }

  Map<String, dynamic> toJson() => {
    'activeRuntimeId': activeRuntimeId,
    'runtimes': runtimes.map((runtime) => runtime.toJson()).toList(),
  };

  factory SavedRuntimeCollection.fromJson(Map<String, dynamic> json) {
    final raw = json['runtimes'];
    final runtimes = raw is List
        ? raw
              .whereType<Map>()
              .map(
                (entry) => SavedRuntime.fromJson(
                  entry.map((key, value) => MapEntry(key.toString(), value)),
                ),
              )
              .nonNulls
              .toList()
        : <SavedRuntime>[];
    return SavedRuntimeCollection(
      runtimes: runtimes,
      activeRuntimeId: json['activeRuntimeId'] as String?,
    ).normalized();
  }
}

String defaultRuntimeName(RuntimeInfo info) {
  final hostname = info.hostname?.trim();
  if (hostname != null && hostname.isNotEmpty) return hostname;
  return _endpointName(info.endpointId);
}

String _endpointName(String endpointId) {
  final suffix = endpointId.length > 8
      ? endpointId.substring(0, 8)
      : endpointId;
  return suffix.isEmpty ? 'Runtime' : 'Runtime $suffix';
}
