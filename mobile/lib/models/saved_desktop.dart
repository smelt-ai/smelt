import 'dart:convert';

import 'package:crypto/crypto.dart';

import 'pairing_config.dart';

class SavedDesktop {
  const SavedDesktop({
    required this.id,
    required this.name,
    required this.pairing,
    required this.lastUsedAt,
  });

  final String id;
  final String name;
  final PairingConfig pairing;
  final DateTime lastUsedAt;

  factory SavedDesktop.create(
    PairingConfig pairing, {
    String? name,
    DateTime? lastUsedAt,
  }) {
    return SavedDesktop(
      id: desktopIdFor(pairing),
      name: name?.trim().isNotEmpty == true
          ? name!.trim()
          : defaultDesktopName(pairing),
      pairing: pairing,
      lastUsedAt: lastUsedAt ?? DateTime.now().toUtc(),
    );
  }

  factory SavedDesktop.fromJson(Map<String, dynamic> json) {
    final pairing = PairingConfig.fromFields(
      json['endpoint'] as String? ?? '',
      json['token'] as String? ?? '',
    );
    final rawName = (json['name'] as String? ?? '').trim();
    return SavedDesktop(
      id: (json['id'] as String?)?.trim().isNotEmpty == true
          ? (json['id'] as String).trim()
          : desktopIdFor(pairing),
      name: rawName.isEmpty ? defaultDesktopName(pairing) : rawName,
      pairing: pairing,
      lastUsedAt:
          DateTime.tryParse(json['lastUsedAt'] as String? ?? '')?.toUtc() ??
          DateTime.fromMillisecondsSinceEpoch(0, isUtc: true),
    );
  }

  SavedDesktop copyWith({
    String? name,
    PairingConfig? pairing,
    DateTime? lastUsedAt,
  }) {
    return SavedDesktop(
      id: id,
      name: name ?? this.name,
      pairing: pairing ?? this.pairing,
      lastUsedAt: lastUsedAt ?? this.lastUsedAt,
    );
  }

  Map<String, dynamic> toJson() => {
    'id': id,
    'name': name,
    'endpoint': pairing.endpoint,
    'token': pairing.token,
    'lastUsedAt': lastUsedAt.toUtc().toIso8601String(),
  };
}

class SavedDesktopCollection {
  const SavedDesktopCollection({
    this.desktops = const [],
    this.activeDesktopId,
  });

  final List<SavedDesktop> desktops;
  final String? activeDesktopId;

  SavedDesktop? get activeDesktop {
    final activeId = activeDesktopId;
    if (activeId == null) return null;
    for (final desktop in desktops) {
      if (desktop.id == activeId) return desktop;
    }
    return null;
  }

  SavedDesktopCollection normalized() {
    final sorted = List<SavedDesktop>.of(desktops)
      ..sort((a, b) => b.lastUsedAt.compareTo(a.lastUsedAt));
    final activeId = sorted.any((item) => item.id == activeDesktopId)
        ? activeDesktopId
        : sorted.firstOrNull?.id;
    return SavedDesktopCollection(
      desktops: List.unmodifiable(sorted),
      activeDesktopId: activeId,
    );
  }

  Map<String, dynamic> toJson() => {
    'version': 1,
    'activeDesktopId': activeDesktopId,
    'desktops': desktops.map((desktop) => desktop.toJson()).toList(),
  };

  factory SavedDesktopCollection.fromJson(Map<String, dynamic> json) {
    final rawDesktops = json['desktops'];
    final desktops = rawDesktops is List
        ? rawDesktops
              .whereType<Map>()
              .map(
                (entry) => SavedDesktop.fromJson(
                  entry.map((key, value) => MapEntry(key.toString(), value)),
                ),
              )
              .toList()
        : <SavedDesktop>[];
    return SavedDesktopCollection(
      desktops: desktops,
      activeDesktopId: json['activeDesktopId'] as String?,
    ).normalized();
  }
}

String desktopIdFor(PairingConfig pairing) {
  // Relay configuration can change without changing the remote computer.
  final identity = pairing.isIroh
      ? 'iroh:${pairing.irohEndpointId}'
      : pairing.endpoint;
  return sha256.convert(utf8.encode(identity)).toString().substring(0, 24);
}

String defaultDesktopName(PairingConfig pairing) {
  final uri = Uri.parse(pairing.endpoint);
  if (pairing.isIroh) {
    final endpointId = pairing.irohEndpointId ?? '';
    final suffix = endpointId.length > 8
        ? endpointId.substring(0, 8)
        : endpointId;
    return suffix.isEmpty ? 'Desktop' : 'Desktop $suffix';
  }
  if (uri.host.isNotEmpty) {
    return uri.hasPort ? '${uri.host}:${uri.port}' : uri.host;
  }
  return 'Desktop';
}
