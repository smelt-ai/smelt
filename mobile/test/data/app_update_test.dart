import 'dart:async';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/app_update.dart';

Map<String, Object?> _release(
  String tag, {
  bool prerelease = false,
  bool draft = false,
  String published = '2026-10-27T00:00:00Z',
  bool apk = true,
  bool sums = true,
}) {
  final version = tag.replaceFirst('v', '');
  return {
    'tag_name': tag,
    'prerelease': prerelease,
    'draft': draft,
    'published_at': published,
    'html_url': 'https://example.test/releases/$tag',
    'assets': [
      if (apk)
        {
          'name': 'Smelt-$version-android-arm64.apk',
          'browser_download_url': 'https://example.test/$tag/app.apk',
        },
      if (sums)
        {
          'name': 'SHA256SUMS',
          'browser_download_url': 'https://example.test/$tag/SHA256SUMS',
        },
    ],
  };
}

ReleaseCandidate _candidate(String version) => ReleaseCandidate(
  version: version,
  apkUrl: 'https://example.test/v$version/app.apk',
);

class _FakeSource implements UpdateSource {
  _FakeSource(this.releases, {Set<String>? withoutSums})
    : withoutSums = withoutSums ?? {};

  final List<ReleaseCandidate> releases;

  /// 校验和清单里还没有它的版本。
  final Set<String> withoutSums;
  String bytes = 'apk-bytes';
  int downloads = 0;

  @override
  Future<List<ReleaseCandidate>> candidates(UpdateChannel channel) async =>
      releases;

  @override
  Future<String> checksum(String apkUrl) async {
    final release = releases.firstWhere((r) => r.apkUrl == apkUrl);
    if (withoutSums.contains(release.version)) {
      throw StateError('还没有');
    }
    return 'sum-of-apk-bytes';
  }

  @override
  Future<void> download(
    String url,
    File to,
    void Function(int received, int? total) onProgress,
  ) async {
    downloads++;
    await to.writeAsString(bytes);
    onProgress(bytes.length, bytes.length);
  }
}

class _FakePlatform implements UpdatePlatform {
  _FakePlatform(this.version);

  final String? version;
  bool allowed = true;
  bool sessionAlive = true;
  final installed = <String>[];
  final failures = StreamController<bool>.broadcast();

  @override
  Future<String?> currentVersion() async => version;

  @override
  Future<String> sha256(File file) async =>
      await file.readAsString() == 'apk-bytes' ? 'sum-of-apk-bytes' : 'other';

  @override
  Future<bool> canInstall() async => allowed;

  @override
  Future<void> openInstallSettings() async {}

  @override
  Future<void> install(File apk) async => installed.add(apk.path);

  @override
  Future<bool> installInProgress() async => sessionAlive;

  @override
  Stream<bool> get installFailures => failures.stream;
}

void main() {
  group('选发布', () {
    test('按发布时间从新到旧，跳过草稿、没传齐的发布', () {
      final releases = [
        _release('v2026.1026.0', published: '2026-10-26T00:00:00Z'),
        _release('v2026.1027.0', published: '2026-10-27T00:00:00Z'),
        _release('v2026.1028.0', published: '2026-10-28T00:00:00Z', apk: false),
        _release('v2026.1029.0', draft: true),
        _release(
          'v2026.1025.0',
          published: '2026-10-25T00:00:00Z',
          sums: false,
        ),
      ];
      expect(pickReleases(releases, UpdateChannel.prod).map((c) => c.version), [
        '2026.1027.0',
        '2026.1026.0',
      ]);
    });

    test('生产版不认预发布，内测版认', () {
      final releases = [
        _release(
          'v2026.1027.0-rc.1',
          prerelease: true,
          published: '2026-10-27T00:00:00Z',
        ),
        _release('v2026.1026.0', published: '2026-10-26T00:00:00Z'),
      ];
      expect(pickReleases(releases, UpdateChannel.prod).map((c) => c.version), [
        '2026.1026.0',
      ]);
      expect(
        pickReleases(releases, UpdateChannel.dev).first.version,
        '2026.1027.0-rc.1',
      );
    });
  });

  test('校验和清单里认准这个文件', () {
    final a = 'A' * 64;
    final b = 'b' * 64;
    final sums =
        '$a  Smelt-1-android-arm64.apk\n$b *smelt-1-windows-setup.exe\n';
    expect(checksumFor(sums, 'Smelt-1-android-arm64.apk'), 'a' * 64);
    expect(checksumFor(sums, 'smelt-1-windows-setup.exe'), b);
    expect(checksumFor(sums, 'Smelt-2-android-arm64.apk'), isNull);
    expect(checksumFor('short  x.apk', 'x.apk'), isNull);
  });

  group('版本号', () {
    test('只认发版号，开发构建的占位版本不算', () {
      expect(isReleaseVersion('2026.1027.0'), isTrue);
      expect(isReleaseVersion('2026.1027.0-rc.2'), isTrue);
      expect(isReleaseVersion('1.0.0'), isFalse);
    });

    test('按 semver 比新旧', () {
      expect(isNewerRelease('2026.1027.0', '2026.1026.3'), isTrue);
      expect(isNewerRelease('2026.1027.0', '2026.1027.0-rc.3'), isTrue);
      expect(isNewerRelease('2026.1027.0-rc.10', '2026.1027.0-rc.9'), isTrue);
      expect(isNewerRelease('2026.1027.0-rc.1', '2026.1027.0'), isFalse);
      expect(isNewerRelease('2026.1027.0', '2026.1027.0'), isFalse);
      expect(isNewerRelease('2026.1026.0', '2026.1027.0'), isFalse);
      expect(isNewerRelease('garbage', '2026.1027.0'), isFalse);
      expect(compareReleaseVersions('garbage', '2026.1027.0'), lessThan(0));
    });
  });

  group('更新器', () {
    late Directory dir;

    setUp(() async {
      dir = await Directory.systemTemp.createTemp('smelt-update-test');
    });

    tearDown(() async {
      if (await dir.exists()) await dir.delete(recursive: true);
    });

    AppUpdater updater(UpdateSource source, UpdatePlatform platform) =>
        AppUpdater(
          source: source,
          platform: platform,
          downloadDir: Directory('${dir.path}/update'),
          channel: () => UpdateChannel.prod,
        );

    test('开发构建不支持，也不去查', () async {
      final source = _FakeSource([_candidate('2026.1027.0')]);
      final u = updater(source, _FakePlatform('1.0.0'));
      await u.start();
      expect(u.value, isA<UpdateUnsupported>());
    });

    test('有更新的就提示，不自动下载', () async {
      final source = _FakeSource([_candidate('2026.1027.0')]);
      final u = updater(source, _FakePlatform('2026.1026.0'));
      await u.start();
      expect(u.value, isA<UpdateAvailable>());
      expect(source.downloads, 0);
    });

    test('最新的没传齐时退到上一个，但不退回装着的版本以前', () async {
      final source = _FakeSource(
        [_candidate('2026.1027.0'), _candidate('2026.1026.0')],
        withoutSums: {'2026.1027.0'},
      );
      final older = updater(source, _FakePlatform('2026.1025.0'));
      await older.start();
      expect((older.value as UpdateAvailable).release.version, '2026.1026.0');

      final current = updater(source, _FakePlatform('2026.1026.0'));
      await current.start();
      expect(current.value, isA<UpdateUpToDate>());
    });

    test('下载后校验对上才算下好，再装', () async {
      final source = _FakeSource([_candidate('2026.1027.0')]);
      final platform = _FakePlatform('2026.1026.0');
      final u = updater(source, platform);
      await u.start();
      await u.download();
      final ready = u.value as UpdateReady;
      expect(await ready.apk.exists(), isTrue);

      await u.install();
      expect(u.value, isA<UpdateInstalling>());
      expect(platform.installed, [ready.apk.path]);
    });

    test('校验对不上单独报出来，包丢掉，可以重下', () async {
      final source = _FakeSource([_candidate('2026.1027.0')])..bytes = 'evil';
      final u = updater(source, _FakePlatform('2026.1026.0'));
      await u.start();
      await u.download();
      expect(
        u.value,
        isA<UpdateFailed>().having((f) => f.stage, 'stage', UpdateStage.verify),
      );
      expect(await Directory('${dir.path}/update').list().toList(), isEmpty);

      source.bytes = 'apk-bytes';
      await u.download();
      expect(u.value, isA<UpdateReady>());
    });

    test('进程被杀后重启：已经下好的新版直接等着装，不用再下', () async {
      final source = _FakeSource([_candidate('2026.1027.0')]);
      final first = updater(source, _FakePlatform('2026.1026.0'));
      await first.start();
      await first.download();
      expect(source.downloads, 1);

      final again = updater(source, _FakePlatform('2026.1026.0'));
      await again.start();
      expect(again.value, isA<UpdateReady>());
      expect(source.downloads, 1);
    });

    test('启动时清掉已经装上的版本的包', () async {
      final source = _FakeSource([_candidate('2026.1027.0')]);
      final before = updater(source, _FakePlatform('2026.1026.0'));
      await before.start();
      await before.download();

      final after = updater(source, _FakePlatform('2026.1027.0'));
      await after.start();
      expect(after.value, isA<UpdateUpToDate>());
      expect(await Directory('${dir.path}/update').list().toList(), isEmpty);
    });

    test('交给系统的安装没了下文，回到前台就回到可以再装', () async {
      final source = _FakeSource([_candidate('2026.1027.0')]);
      final platform = _FakePlatform('2026.1026.0');
      final u = updater(source, platform);
      await u.start();
      await u.download();
      await u.install();

      await u.resumed();
      expect(u.value, isA<UpdateInstalling>());

      platform.sessionAlive = false;
      await u.resumed();
      expect(u.value, isA<UpdateReady>());
    });

    test('按版本挑，不按发布的先后', () async {
      // 列表里旧版本排在前面（发布得晚）。
      final source = _FakeSource([
        _candidate('2026.1027.0'),
        _candidate('2026.1027.1-rc.1'),
      ]);
      final u = AppUpdater(
        source: source,
        platform: _FakePlatform('2026.1027.0'),
        downloadDir: Directory('${dir.path}/update'),
        channel: () => UpdateChannel.dev,
      );
      await u.start();
      expect((u.value as UpdateAvailable).release.version, '2026.1027.1-rc.1');
    });

    test('没授权就先去授权，回到前台授权了接着装', () async {
      final source = _FakeSource([_candidate('2026.1027.0')]);
      final platform = _FakePlatform('2026.1026.0')..allowed = false;
      final u = updater(source, platform);
      await u.start();
      await u.download();
      await u.install();
      expect(u.value, isA<UpdateNeedsPermission>());

      platform.allowed = true;
      await u.resumed();
      expect(u.value, isA<UpdateInstalling>());
      expect(platform.installed, hasLength(1));
    });

    test('用户在系统确认框里取消，回到可以再装', () async {
      final source = _FakeSource([_candidate('2026.1027.0')]);
      final platform = _FakePlatform('2026.1026.0');
      final u = updater(source, platform);
      await u.start();
      await u.download();
      await u.install();
      platform.failures.add(true);
      await Future<void>.delayed(Duration.zero);
      expect(u.value, isA<UpdateReady>());

      await u.install();
      platform.failures.add(false);
      await Future<void>.delayed(Duration.zero);
      expect(
        u.value,
        isA<UpdateFailed>().having(
          (f) => f.stage,
          'stage',
          UpdateStage.install,
        ),
      );
    });

    test('回到前台时隔够久才再查', () async {
      var now = DateTime(2026, 10, 27, 9);
      final source = _FakeSource([_candidate('2026.1026.0')]);
      final u = AppUpdater(
        source: source,
        platform: _FakePlatform('2026.1026.0'),
        downloadDir: Directory('${dir.path}/update'),
        channel: () => UpdateChannel.prod,
        now: () => now,
      );
      await u.start();
      expect(u.value, isA<UpdateUpToDate>());

      source.releases.insert(0, _candidate('2026.1027.0'));
      now = now.add(const Duration(hours: 1));
      await u.resumed();
      expect(u.value, isA<UpdateUpToDate>());

      now = now.add(AppUpdater.checkInterval);
      await u.resumed();
      expect(u.value, isA<UpdateAvailable>());
    });
  });
}
