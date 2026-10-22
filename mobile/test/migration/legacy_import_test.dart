import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/preferences.dart';
import 'package:smelt_mobile/migration/legacy_import.dart';

/// 演示后端（登记着 rt-mbp 等三台）加上：能让存草稿失败，记下导入了什么。
class _Backend extends DemoBackend {
  bool failSaves = false;
  final imported = <String>[];

  @override
  Future<List<RuntimeRecord>> importRuntimes(List<RuntimeImport> runtimes) {
    imported.addAll(runtimes.map((r) => r.runtimeId));
    return super.importRuntimes(runtimes);
  }

  @override
  Future<void> saveDraft(String runtimeId, String sessionId, MessageDraft d) {
    if (failSaves) throw StateError('库写不进去');
    return super.saveDraft(runtimeId, sessionId, d);
  }
}

/// 旧版钥匙串里的机器列表，当前运行时是 [active]。
String _keychain(String active) => jsonEncode({
  'activeRuntimeId': active,
  'runtimes': [
    {'runtimeId': active, 'endpointId': 'ep-$active', 'name': 'Mac'},
  ],
});

void main() {
  late Directory dataDir;
  late Directory drafts;
  late _Backend backend;
  late int retired;

  setUp(() {
    dataDir = Directory.systemTemp.createTempSync('smelt-legacy-');
    drafts = Directory('${dataDir.path}/message-drafts')..createSync();
    final name = base64Url.encode(utf8.encode('s-fleet')).replaceAll('=', '');
    File('${drafts.path}/$name.json').writeAsStringSync(
      jsonEncode({'content': '写了一半', 'requestId': 'req-1'}),
    );
    backend = _Backend();
    retired = 0;
  });

  tearDown(() => dataDir.deleteSync(recursive: true));

  Future<void> migrate({required Future<String?> Function() read}) =>
      migrateLegacyData(
        backend: backend,
        dataDir: dataDir,
        prefs: PreferencesStore.memory(),
        readRuntimes: read,
        retireKeys: () async => retired++,
      );

  test('搬完草稿才删钥匙串和草稿目录', () async {
    await migrate(read: () async => _keychain('rt-mbp'));

    expect(backend.imported, ['rt-mbp']);
    final draft = await backend.loadDraft('rt-mbp', 's-fleet');
    expect(draft?.content, '写了一半');
    expect(draft?.requestId, 'req-1');
    expect(drafts.existsSync(), isFalse);
    expect(retired, 1);
  });

  test('读钥匙串失败：草稿和旧键都留着，下次再试', () async {
    await migrate(read: () async => throw StateError('钥匙串锁着'));

    expect(drafts.listSync(), hasLength(1));
    expect(retired, 0);
  });

  test('草稿写不进去：留着它和旧键；下次写进去了再删', () async {
    backend.failSaves = true;
    await migrate(read: () async => _keychain('rt-mbp'));
    expect(drafts.listSync(), hasLength(1));
    expect(retired, 0);

    backend.failSaves = false;
    await migrate(read: () async => _keychain('rt-mbp'));
    expect((await backend.loadDraft('rt-mbp', 's-fleet'))?.content, '写了一半');
    expect(drafts.existsSync(), isFalse);
    expect(retired, 1);
  });

  test('当前运行时没登记上（导入时被跳过）：草稿认领不了，丢掉', () async {
    await migrate(read: () async => _keychain('rt-gone'));

    expect(drafts.existsSync(), isFalse);
    expect(retired, 1);
  });

  test('钥匙串里的内容坏了：当没有，草稿认领不了', () async {
    await migrate(read: () async => '{坏的');

    expect(drafts.existsSync(), isFalse);
    expect(retired, 1);
  });
}
