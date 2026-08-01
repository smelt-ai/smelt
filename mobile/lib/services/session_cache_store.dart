import 'dart:convert';
import 'dart:io';

import 'package:crypto/crypto.dart';
import 'package:path_provider/path_provider.dart';

import '../models/acp_snapshot.dart';
import 'gateway_service.dart';

class CachedMobileState {
  const CachedMobileState({
    this.sessions = const [],
    this.snapshots = const {},
    this.updatedAt,
  });

  final List<SessionSummary> sessions;
  final Map<String, AcpSnapshot> snapshots;
  final DateTime? updatedAt;
}

abstract interface class SessionCacheStore {
  String namespaceFor(String endpoint, String token);
  Future<CachedMobileState> load(String namespace);
  Future<void> saveSessions(String namespace, List<SessionSummary> sessions);
  Future<void> saveSnapshot(
    String namespace,
    String sessionId,
    AcpSnapshot snapshot,
  );
  Future<void> deleteSnapshot(String namespace, String sessionId);
}

class FileSessionCacheStore implements SessionCacheStore {
  FileSessionCacheStore({
    Future<Directory> Function()? directoryProvider,
    this.maxSnapshots = 5,
    this.maxSnapshotBytes = 32 * 1024 * 1024,
  }) : _directoryProvider = directoryProvider ?? getApplicationSupportDirectory;

  static const _version = 1;

  final Future<Directory> Function() _directoryProvider;
  final int maxSnapshots;
  final int maxSnapshotBytes;
  final Map<String, Future<void>> _operations = {};

  @override
  String namespaceFor(String endpoint, String token) =>
      sha256.convert(utf8.encode('$endpoint\u0000$token')).toString();

  Future<Directory> _targetDirectory(String namespace) async {
    final root = await _directoryProvider();
    final directory = Directory('${root.path}/session-cache/$namespace');
    await directory.create(recursive: true);
    return directory;
  }

  String _snapshotName(String sessionId) =>
      '${base64UrlEncode(utf8.encode(sessionId)).replaceAll('=', '')}.json';

  @override
  Future<CachedMobileState> load(String namespace) async {
    await (_operations[namespace] ?? Future<void>.value()).catchError((_) {});
    final directory = await _targetDirectory(namespace);
    final sessionsFile = File('${directory.path}/sessions.json');
    var sessions = const <SessionSummary>[];
    DateTime? updatedAt;
    if (await sessionsFile.exists()) {
      try {
        final json = jsonDecode(await sessionsFile.readAsString());
        if (json is Map<String, dynamic> && json['version'] == _version) {
          sessions = (json['sessions'] as List<dynamic>? ?? const [])
              .whereType<Map<String, dynamic>>()
              .map(SessionSummary.fromJson)
              .toList();
          updatedAt = DateTime.fromMillisecondsSinceEpoch(
            (json['updatedAtMs'] as num?)?.toInt() ?? 0,
          );
        }
      } on FormatException {
        // A partial or old cache is disposable; the network remains canonical.
      }
    }

    final snapshots = <String, AcpSnapshot>{};
    final snapshotsDirectory = Directory('${directory.path}/snapshots');
    if (await snapshotsDirectory.exists()) {
      await for (final entity in snapshotsDirectory.list()) {
        if (entity is! File || !entity.path.endsWith('.json')) continue;
        try {
          final json = jsonDecode(await entity.readAsString());
          if (json is! Map<String, dynamic> || json['version'] != _version) {
            continue;
          }
          final sessionId = json['sessionId'] as String?;
          final snapshot = json['snapshot'];
          if (sessionId == null || snapshot is! Map<String, dynamic>) continue;
          snapshots[sessionId] = AcpSnapshot.fromJson(snapshot);
        } on FormatException {
          // Ignore one bad session without discarding the remaining cache.
        }
      }
    }
    return CachedMobileState(
      sessions: sessions,
      snapshots: snapshots,
      updatedAt: updatedAt,
    );
  }

  @override
  Future<void> saveSessions(String namespace, List<SessionSummary> sessions) =>
      _enqueue(namespace, () async {
        final directory = await _targetDirectory(namespace);
        await _writeJson(File('${directory.path}/sessions.json'), {
          'version': _version,
          'updatedAtMs': DateTime.now().millisecondsSinceEpoch,
          'sessions': sessions.map((session) => session.toJson()).toList(),
        });
      });

  @override
  Future<void> saveSnapshot(
    String namespace,
    String sessionId,
    AcpSnapshot snapshot,
  ) => _enqueue(namespace, () async {
    final directory = await _targetDirectory(namespace);
    final snapshotsDirectory = Directory('${directory.path}/snapshots');
    await snapshotsDirectory.create(recursive: true);
    final file = File('${snapshotsDirectory.path}/${_snapshotName(sessionId)}');
    await _writeJson(file, {
      'version': _version,
      'updatedAtMs': DateTime.now().millisecondsSinceEpoch,
      'sessionId': sessionId,
      'snapshot': snapshot.cacheTail().toJson(),
    });
    await _trimSnapshots(snapshotsDirectory);
  });

  @override
  Future<void> deleteSnapshot(String namespace, String sessionId) =>
      _enqueue(namespace, () async {
        final directory = await _targetDirectory(namespace);
        final file = File(
          '${directory.path}/snapshots/${_snapshotName(sessionId)}',
        );
        if (await file.exists()) await file.delete();
      });

  Future<void> _writeJson(File file, Map<String, dynamic> value) async {
    await file.parent.create(recursive: true);
    final temporary = File('${file.path}.tmp');
    await temporary.writeAsString(jsonEncode(value), flush: true);
    await temporary.rename(file.path);
  }

  Future<void> _trimSnapshots(Directory directory) async {
    final files = await directory
        .list()
        .where((entity) => entity is File && entity.path.endsWith('.json'))
        .cast<File>()
        .toList();
    final metadata = <({File file, FileStat stat})>[];
    for (final file in files) {
      metadata.add((file: file, stat: await file.stat()));
    }
    metadata.sort((a, b) => b.stat.modified.compareTo(a.stat.modified));
    var bytes = 0;
    for (var index = 0; index < metadata.length; index++) {
      final item = metadata[index];
      bytes += item.stat.size;
      if (index >= maxSnapshots || bytes > maxSnapshotBytes) {
        await item.file.delete();
      }
    }
  }

  Future<void> _enqueue(String namespace, Future<void> Function() operation) {
    final previous = _operations[namespace] ?? Future<void>.value();
    final next = previous.catchError((_) {}).then((_) => operation());
    _operations[namespace] = next;
    return next.whenComplete(() {
      if (identical(_operations[namespace], next)) {
        _operations.remove(namespace);
      }
    });
  }
}
