/// 一次性迁移：把旧版本留在手机上的数据搬进新结构，搬完删掉旧的。
///
/// - 钥匙串里的配对机器列表（`smelt.runtimes.v1`，JSON）→ 内核的机器登记（SQLite）。
/// - `message-drafts/*.json` 草稿 → 内核的草稿表。旧草稿只按会话 id 存，归到旧数据里
///   记着的「当前运行时」名下（旧版一次只连一台，草稿只可能属于它）。
/// - `appearance-prefs.json` / `terminal-prefs.json` → `preferences.json`。
///
/// 每一步都是「读到才搬，搬完即删」，重复执行无副作用。确认没有用户还停在旧版本后，
/// 整个文件连同 flutter_secure_storage 依赖一起删掉。
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
}) async {
  String? activeRuntimeId;
  try {
    final encoded = await storage.read(key: _runtimesKey);
    if (encoded != null) {
      final decoded = jsonDecode(encoded);
      if (decoded is Map) {
        activeRuntimeId = decoded['activeRuntimeId'] as String?;
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
      }
    }
    for (final key in _retiredKeys) {
      await storage.delete(key: key);
    }
  } catch (error) {
    // 迁移失败不挡启动：最坏是用户重新扫一次码。留着旧键，下次启动再试。
    debugPrint('迁移配对列表失败：$error');
  }

  await _importDrafts(backend, dataDir, activeRuntimeId);
  await _importPreferences(dataDir, prefs);
}

Future<void> _importDrafts(
  Backend backend,
  Directory dataDir,
  String? runtimeId,
) async {
  final dir = Directory('${dataDir.path}/message-drafts');
  if (!await dir.exists()) return;
  if (runtimeId != null) {
    await for (final file in dir.list()) {
      if (file is! File || !file.path.endsWith('.json')) continue;
      try {
        final name = file.uri.pathSegments.last;
        final encoded = name.substring(0, name.length - 5);
        final sessionId = utf8.decode(
          base64Url.decode(base64Url.normalize(encoded)),
        );
        final json = jsonDecode(await file.readAsString());
        if (json is! Map<String, dynamic>) continue;
        final content = json['content'] as String? ?? '';
        final images = <ImageInput>[
          for (final image in (json['images'] as List? ?? const []))
            if (image is Map<String, dynamic>)
              ImageInput(
                mime: image['mime'] as String? ?? 'image/png',
                bytes: base64Decode(image['data_b64'] as String? ?? ''),
              ),
        ];
        if (content.trim().isEmpty && images.isEmpty) continue;
        await backend.saveDraft(
          runtimeId,
          sessionId,
          MessageDraft(
            content: content,
            images: images,
            requestId: json['requestId'] as String?,
          ),
        );
      } catch (error) {
        debugPrint('迁移草稿失败：$error');
      }
    }
  }
  await dir.delete(recursive: true);
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
