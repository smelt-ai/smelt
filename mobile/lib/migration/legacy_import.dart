/// 一次性迁移：把旧版本留在手机上的数据搬进新结构，搬完删掉旧的。
///
/// - 钥匙串里的配对机器列表（`smelt.runtimes.v1`，JSON）→ 内核的机器登记（SQLite）。
/// - `message-drafts/*.json` 草稿 → 内核的草稿表。旧草稿只按会话 id 存，归到旧数据里
///   记着的「当前运行时」名下（旧版一次只连一台，草稿只可能属于它）。
/// - `appearance-prefs.json` / `terminal-prefs.json` → `preferences.json`。
///
/// 每一步都是「读到才搬，搬完即删」，重复执行无副作用；没搬成的留着，下次启动再试。
/// 确认没有用户还停在旧版本后，整个文件连同 flutter_secure_storage 依赖一起删掉。
library;

import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import '../data/backend.dart';
import '../data/models.dart';
import '../data/preferences.dart';

const _runtimesKey = 'smelt.runtimes.v1';
const _retiredKeys = [
  _runtimesKey,
  'smelt.gateway.endpoint',
  'smelt.gateway.token',
  'smelt.gateway.desktops.v1',
];

Future<void> importLegacyData({
  required Backend backend,
  required Directory dataDir,
  required PreferencesStore prefs,
  FlutterSecureStorage storage = const FlutterSecureStorage(),
}) => migrateLegacyData(
  backend: backend,
  dataDir: dataDir,
  prefs: prefs,
  readRuntimes: () => storage.read(key: _runtimesKey),
  retireKeys: () async {
    for (final key in _retiredKeys) {
      await storage.delete(key: key);
    }
  },
);

/// [importLegacyData] 的本体，钥匙串的读和删由调用方给（测试注入）。
@visibleForTesting
Future<void> migrateLegacyData({
  required Backend backend,
  required Directory dataDir,
  required PreferencesStore prefs,
  required Future<String?> Function() readRuntimes,
  required Future<void> Function() retireKeys,
}) async {
  try {
    final encoded = await readRuntimes();
    final activeRuntimeId = encoded == null
        ? null
        : await _importRuntimes(backend, encoded);
    // 草稿靠钥匙串里记着的「当前运行时」认领：草稿搬完（或确定认领不了）才删钥匙串，
    // 不然下次启动就不知道剩下的草稿归哪台机器了。
    if (await _importDrafts(backend, dataDir, activeRuntimeId)) {
      await retireKeys();
    }
  } catch (error) {
    // 迁移失败不挡启动：最坏是用户重新扫一次码。旧键和没搬完的草稿都留着，下次启动再试。
    debugPrint('迁移配对列表和草稿失败：$error');
  }

  await _importPreferences(dataDir, prefs);
}

/// 导入钥匙串里的机器列表，返回旧版记着的当前运行时。内容坏了（解不出来）就当没有：
/// 重试也不会好。
Future<String?> _importRuntimes(Backend backend, String encoded) async {
  final Object? decoded;
  try {
    decoded = jsonDecode(encoded);
  } on FormatException catch (error) {
    debugPrint('旧的配对列表解不出来，丢掉：$error');
    return null;
  }
  if (decoded is! Map) return null;
  final imports = <RuntimeImport>[
    for (final entry in (decoded['runtimes'] as List? ?? const []))
      if (entry is Map &&
          (entry['runtimeId'] as String? ?? '').isNotEmpty &&
          (entry['endpointId'] as String? ?? '').isNotEmpty)
        RuntimeImport(
          runtimeId: entry['runtimeId'] as String,
          endpointId: entry['endpointId'] as String,
          relayUrl: entry['relayUrl'] as String?,
          name: (entry['name'] as String? ?? '').trim(),
        ),
  ];
  if (imports.isNotEmpty) await backend.importRuntimes(imports);
  return decoded['activeRuntimeId'] as String?;
}

/// 搬草稿，搬进去的文件随手删掉。全部搬完（或确定认领不了）时删掉目录、返回 true；
/// 有没写进去的就留着它们、返回 false，下次启动再试。
Future<bool> _importDrafts(
  Backend backend,
  Directory dataDir,
  String? runtimeId,
) async {
  final dir = Directory('${dataDir.path}/message-drafts');
  if (!await dir.exists()) return true;
  // 认领不了：旧数据没记当前运行时，或那台机器的登记不合法、导入时被跳过了。
  if (runtimeId == null || !await _isRegistered(backend, runtimeId)) {
    await dir.delete(recursive: true);
    return true;
  }
  var done = true;
  for (final file in await dir.list().toList()) {
    if (file is! File || !file.path.endsWith('.json')) continue;
    final (String, MessageDraft)? draft;
    try {
      draft = await _readDraft(file);
    } catch (error) {
      // 文件本身坏了，重试也不会好。
      debugPrint('旧草稿读不出来，丢掉：$error');
      await file.delete();
      continue;
    }
    if (draft != null) {
      try {
        await backend.saveDraft(runtimeId, draft.$1, draft.$2);
      } catch (error) {
        debugPrint('迁移草稿失败，下次再试：$error');
        done = false;
        continue;
      }
    }
    await file.delete();
  }
  if (done) await dir.delete(recursive: true);
  return done;
}

Future<bool> _isRegistered(Backend backend, String runtimeId) async {
  final fleet = await backend.fleetEvents().first;
  return fleet.runtimes.any((r) => r.record.runtimeId == runtimeId);
}

/// 读一份旧草稿：（会话 id，草稿）。空草稿返回 null。
Future<(String, MessageDraft)?> _readDraft(File file) async {
  final name = file.uri.pathSegments.last;
  final encoded = name.substring(0, name.length - 5);
  final sessionId = utf8.decode(
    base64Url.decode(base64Url.normalize(encoded)),
  );
  final json = jsonDecode(await file.readAsString());
  if (json is! Map<String, dynamic>) return null;
  final content = json['content'] as String? ?? '';
  final images = <ImageInput>[
    for (final image in (json['images'] as List? ?? const []))
      if (image is Map<String, dynamic>)
        ImageInput(
          mime: image['mime'] as String? ?? 'image/png',
          bytes: base64Decode(image['data_b64'] as String? ?? ''),
        ),
  ];
  if (content.trim().isEmpty && images.isEmpty) return null;
  return (
    sessionId,
    MessageDraft(
      content: content,
      images: images,
      requestId: json['requestId'] as String?,
    ),
  );
}

Future<void> _importPreferences(
  Directory dataDir,
  PreferencesStore prefs,
) async {
  final appearance = File('${dataDir.path}/appearance-prefs.json');
  final terminal = File('${dataDir.path}/terminal-prefs.json');
  var next = prefs.value;
  try {
    if (await appearance.exists()) {
      final json = jsonDecode(await appearance.readAsString());
      final mode = parseThemeMode(json is Map ? json['themeMode'] : null);
      if (mode != null) next = next.copyWith(themeMode: mode);
      await appearance.delete();
    }
    if (await terminal.exists()) {
      final json = jsonDecode(await terminal.readAsString());
      final size = json is Map ? json['fontSize'] : null;
      if (size is num &&
          Preferences.terminalFontSteps.contains(size.toDouble())) {
        next = next.copyWith(terminalFontSize: size.toDouble());
      }
      await terminal.delete();
    }
  } catch (error) {
    debugPrint('迁移显示偏好失败：$error');
  }
  prefs.update(next);
}
