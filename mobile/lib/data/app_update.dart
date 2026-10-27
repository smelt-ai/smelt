/// 安卓应用内更新：查新版、下载 APK、对着同一个 Release 的 `SHA256SUMS` 校验，再交给系统
/// 安装（PackageInstaller，见 `android/.../AppUpdateChannel.kt`）。
///
/// 只在安卓的发版构建上有：iOS 走 TestFlight / App Store，开发构建签名对不上、装不上去。
/// 检查只在启动和回到前台时做（隔几个小时才真去查一次），不常驻定时器；发现新版只提示，
/// 用户点了才下载。
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

/// 更新通道：生产版只认正式发布，内测版连预发布（tag 带 `-` 的 rc）也认。
enum UpdateChannel { prod, dev }

/// 认得出的通道名 → [UpdateChannel]；认不出返回 null。
UpdateChannel? parseUpdateChannel(Object? raw) =>
    UpdateChannel.values.where((c) => c.name == raw).firstOrNull;

/// 一个能装的发布。
@immutable
class ReleaseCandidate {
  const ReleaseCandidate({
    required this.version,
    required this.apkUrl,
    this.notesUrl,
  });

  /// 发版号，tag 去掉 `v`，如 `2026.1027.0-rc.1`。
  final String version;
  final String apkUrl;

  /// Release 页面；更新清单给的包没有。
  final String? notesUrl;

  @override
  bool operator ==(Object other) =>
      other is ReleaseCandidate &&
      other.version == version &&
      other.apkUrl == apkUrl &&
      other.notesUrl == notesUrl;

  @override
  int get hashCode => Object.hash(version, apkUrl, notesUrl);
}

/// Release 上的 APK 名，和 `release.yml` 的 android 任务一致。
String apkAssetName(String version) => 'Smelt-$version-android-arm64.apk';

const _checksumsAsset = 'SHA256SUMS';

/// GitHub Releases 接口返回的列表里，符合通道、已经传上 APK 和校验和清单的发布，按发布
/// 时间从新到旧排。不靠列表顺序：rc 和正式版打在同一个提交上时，GitHub 分不出先后。
List<ReleaseCandidate> pickReleases(
  List<Object?> releases,
  UpdateChannel channel,
) {
  final picked = <(String, ReleaseCandidate)>[];
  for (final raw in releases) {
    if (raw is! Map<String, Object?>) continue;
    if (raw['draft'] == true) continue;
    if (channel == UpdateChannel.prod && raw['prerelease'] == true) continue;
    final tag = raw['tag_name'];
    final published = raw['published_at'];
    if (tag is! String || published is! String) continue;
    final version = tag.trim().replaceFirst(RegExp('^v'), '');
    final assets = raw['assets'];
    if (assets is! List) continue;
    String? asset(String name) => assets
        .whereType<Map<String, Object?>>()
        .where((a) => a['name'] == name)
        .map((a) => a['browser_download_url'])
        .whereType<String>()
        .firstOrNull;
    final apk = asset(apkAssetName(version));
    if (apk == null || asset(_checksumsAsset) == null) continue;
    final notes = raw['html_url'];
    picked.add((
      published,
      ReleaseCandidate(
        version: version,
        apkUrl: apk.trim(),
        notesUrl: notes is String ? notes : null,
      ),
    ));
  }
  // ISO 8601 的 UTC 时间按字符串排就是按时间排。
  picked.sort((a, b) => b.$1.compareTo(a.$1));
  return [for (final (_, candidate) in picked) candidate];
}

/// 从 `sha256sum` 的输出里找出这个文件的校验和。二进制模式在文件名前加 `*`。
String? checksumFor(String sums, String file) {
  for (final line in const LineSplitter().convert(sums)) {
    final match = RegExp(r'^([0-9a-fA-F]{64})\s+\*?(.+)$').firstMatch(line);
    if (match != null && match.group(2)!.trim() == file) {
      return match.group(1)!.toLowerCase();
    }
  }
  return null;
}

/// 发版号 `YYYY.MDD.N`，预发布再加 `-rc.N`（`docs/release.md` 的「版本号」）。开发构建的
/// 版本号是 pubspec 里的占位值，对不上这个格式。
final _releaseVersion = RegExp(
  r'^(\d{4})\.(\d{3,4})\.(\d+)(?:-([0-9A-Za-z.]+))?$',
);

bool isReleaseVersion(String version) =>
    _releaseVersion.hasMatch(version.trim());

/// 按 semver 的先后比两个发版号。认不出的排在最旧。
int compareReleaseVersions(String a, String b) {
  final x = _releaseVersion.firstMatch(a.trim());
  final y = _releaseVersion.firstMatch(b.trim());
  if (x == null || y == null) {
    if (x == null && y == null) return 0;
    return x == null ? -1 : 1;
  }
  for (var i = 1; i <= 3; i++) {
    final diff = int.parse(x.group(i)!).compareTo(int.parse(y.group(i)!));
    if (diff != 0) return diff;
  }
  return _comparePrerelease(x.group(4), y.group(4));
}

/// [candidate] 是否比 [installed] 新。认不出的版本号一律不算新：安卓不许装 versionCode
/// 更小的包，宁可不提示也不提示一个装不上的「更新」。
bool isNewerRelease(String candidate, String installed) =>
    isReleaseVersion(candidate) &&
    isReleaseVersion(installed) &&
    compareReleaseVersions(candidate, installed) > 0;

/// semver 的预发布比较：没有预发布的比有的大；逐段比，数字段按数值，数字段比字母段小。
int _comparePrerelease(String? a, String? b) {
  if (a == null || b == null) {
    if (a == b) return 0;
    return a == null ? 1 : -1;
  }
  final left = a.split('.');
  final right = b.split('.');
  for (var i = 0; i < left.length && i < right.length; i++) {
    final x = int.tryParse(left[i]);
    final y = int.tryParse(right[i]);
    final diff = switch ((x, y)) {
      (final p?, final q?) => p.compareTo(q),
      (_?, null) => -1,
      (null, _?) => 1,
      (null, null) => left[i].compareTo(right[i]),
    };
    if (diff != 0) return diff;
  }
  return left.length.compareTo(right.length);
}

/// 更新的状态。设置页的「更新」一节和外壳的提示都看它。
sealed class UpdateStatus {
  const UpdateStatus();
}

/// iOS、演示模式、开发构建：没有应用内更新。
final class UpdateUnsupported extends UpdateStatus {
  const UpdateUnsupported();
}

final class UpdateIdle extends UpdateStatus {
  const UpdateIdle();
}

final class UpdateChecking extends UpdateStatus {
  const UpdateChecking();
}

final class UpdateUpToDate extends UpdateStatus {
  const UpdateUpToDate();
}

/// 查到新版，等用户点下载。
final class UpdateAvailable extends UpdateStatus {
  const UpdateAvailable(this.release);
  final ReleaseCandidate release;
}

/// [total] 为 null 表示服务端没给长度，只能显示已下多少。
final class UpdateDownloading extends UpdateStatus {
  const UpdateDownloading(this.release, this.received, this.total);
  final ReleaseCandidate release;
  final int received;
  final int? total;
}

/// 下好、校验过，等用户点安装。
final class UpdateReady extends UpdateStatus {
  const UpdateReady(this.release, this.apk);
  final ReleaseCandidate release;
  final File apk;
}

/// 系统还不许 Smelt 安装应用。用户去设置里打开，回到前台就接着装。
final class UpdateNeedsPermission extends UpdateStatus {
  const UpdateNeedsPermission(this.release, this.apk);
  final ReleaseCandidate release;
  final File apk;
}

/// 交给系统了，等系统的确认框。装上之后进程会被换掉。
final class UpdateInstalling extends UpdateStatus {
  const UpdateInstalling(this.release, this.apk);
  final ReleaseCandidate release;
  final File apk;
}

enum UpdateStage {
  check,
  download,

  /// 下完了，校验和对不上：包已经丢掉，重下一遍。
  verify,
  install,
}

/// 失败了。下载、安装失败时留着发布（和下好的包），可以直接重试。
final class UpdateFailed extends UpdateStatus {
  const UpdateFailed(this.stage, {this.release, this.apk});
  final UpdateStage stage;
  final ReleaseCandidate? release;
  final File? apk;
}

/// 更新源：查发布、取校验和、下载。真实现走 HTTPS，测试换成假的。
abstract interface class UpdateSource {
  /// 符合通道的发布，新的在前。
  Future<List<ReleaseCandidate>> candidates(UpdateChannel channel);

  /// APK 旁边那份 `SHA256SUMS` 里它的校验和；清单里还没有它（发布还没传完）就抛错。
  Future<String> checksum(String apkUrl);

  Future<void> download(
    String url,
    File to,
    void Function(int received, int? total) onProgress,
  );
}

/// 安卓那一侧：版本号、算校验和、安装权限、交给系统安装。
abstract interface class UpdatePlatform {
  /// 装着的这个包的版本名（`flutter build --build-name`）。
  Future<String?> currentVersion();

  Future<String> sha256(File file);

  /// 系统许不许 Smelt 安装应用（Android 8 起按应用单独授权）。
  Future<bool> canInstall();

  /// 打开系统里「允许安装未知应用」的那一页。
  Future<void> openInstallSettings();

  /// 交给系统安装。返回时系统已经接手；结果从 [installFailures] 回来。
  Future<void> install(File apk);

  /// 交给系统的那次安装还没出结果（还在写包、等确认框）。进程被杀过就是 false。
  Future<bool> installInProgress();

  /// 系统没装上：`true` 是用户在确认框里取消了，`false` 是装失败了。
  Stream<bool> get installFailures;
}

class AppUpdater extends ValueNotifier<UpdateStatus> {
  AppUpdater({
    required UpdateSource this._source,
    required UpdatePlatform this._platform,
    required Directory this._downloadDir,
    required this._channel,
    DateTime Function()? now,
  }) : _now = now ?? DateTime.now,
       // 读到版本号、确认是发版构建之前不露面（见 [start]）。
       super(const UpdateUnsupported());

  /// 没有应用内更新的平台和演示模式用：一直是 [UpdateUnsupported]，什么都不做。
  AppUpdater.unsupported()
    : _source = null,
      _platform = null,
      _downloadDir = null,
      _channel = (() => UpdateChannel.prod),
      _now = DateTime.now,
      super(const UpdateUnsupported());

  /// 隔这么久才在回到前台时再查一次。
  static const checkInterval = Duration(hours: 6);

  /// 最新的发布还没传完时，最多往前再看几个。
  static const maxCandidates = 3;

  final UpdateSource? _source;
  final UpdatePlatform? _platform;
  final Directory? _downloadDir;
  final UpdateChannel Function() _channel;
  final DateTime Function() _now;

  String? _installed;
  DateTime? _lastCheck;
  StreamSubscription<bool>? _failures;

  /// 装着的版本；还没读到、或者不支持时为 null。
  String? get installedVersion => _installed;

  /// 启动时调一次：读版本号，发版构建才开始检查。
  Future<void> start() async {
    final platform = _platform;
    if (platform == null || _installed != null) return;
    try {
      final version = await platform.currentVersion();
      if (version == null || !isReleaseVersion(version)) return;
      _installed = version;
      _failures = platform.installFailures.listen(_onInstallFailed);
      value = const UpdateIdle();
      await _cleanup(version);
      await check();
    } catch (error) {
      debugPrint('[update] 启动更新检查失败：$error');
    }
  }

  /// 回到前台时调。等授权的接着装；交给系统的那次安装已经没了下文（确认框没弹出来、
  /// 进程被杀过）就回到「已下载」；否则距上次检查够久了就再查一次。
  Future<void> resumed() async {
    final platform = _platform;
    if (platform == null || _installed == null) return;
    try {
      switch (value) {
        case UpdateNeedsPermission(:final release, :final apk):
          if (await platform.canInstall()) await _install(release, apk);
        case UpdateInstalling(:final release, :final apk):
          if (!await platform.installInProgress()) {
            value = UpdateReady(release, apk);
          }
        default:
          final last = _lastCheck;
          if (last != null && _now().difference(last) < checkInterval) return;
          await check();
      }
    } catch (error) {
      debugPrint('[update] 回到前台时处理更新失败：$error');
    }
  }

  /// 正在下载、安装，或者已经下好等着装时不查：查了也只会换掉手里这个包。
  bool get canCheck => switch (value) {
    UpdateIdle() || UpdateUpToDate() || UpdateAvailable() => true,
    UpdateFailed(:final stage) => stage != UpdateStage.install,
    _ => false,
  };

  Future<void> check() async {
    final source = _source;
    final installed = _installed;
    if (source == null || installed == null || !canCheck) return;
    value = const UpdateChecking();
    _lastCheck = _now();
    try {
      // 按版本从新到旧挑，不按发布时间：两个 tag 前后脚推上去时，发布的先后可能和版本反着。
      final candidates = [...await source.candidates(_channel())]
        ..sort((a, b) => compareReleaseVersions(b.version, a.version));
      Object? lastError;
      for (final candidate in candidates.take(maxCandidates)) {
        // 这个都不比装着的新，后面的更不会。
        if (!isNewerRelease(candidate.version, installed)) {
          value = const UpdateUpToDate();
          return;
        }
        final String expected;
        try {
          expected = await source.checksum(candidate.apkUrl);
        } catch (error) {
          // 校验和清单里还没有它：发布还没传完，退到上一个。
          lastError = error;
          continue;
        }
        // 上次已经下好了（比如去系统设置里授权时进程被杀了），直接等着装。
        value =
            await _downloaded(candidate, expected) ??
            UpdateAvailable(candidate);
        return;
      }
      if (lastError != null) throw lastError;
      value = const UpdateUpToDate();
    } catch (error) {
      debugPrint('[update] 检查更新失败：$error');
      value = const UpdateFailed(UpdateStage.check);
    }
  }

  /// 下载查到的新版。下载失败后再点也走这里。
  Future<void> download() async {
    final source = _source;
    final platform = _platform;
    final dir = _downloadDir;
    final release = switch (value) {
      UpdateAvailable(:final release) => release,
      UpdateFailed(
        stage: UpdateStage.download || UpdateStage.verify,
        :final release?,
      ) =>
        release,
      _ => null,
    };
    if (source == null || platform == null || dir == null || release == null) {
      return;
    }
    value = UpdateDownloading(release, 0, null);
    final apk = _apkFile(dir, release);
    final partial = File('${apk.path}.part');
    try {
      final expected = await source.checksum(release.apkUrl);
      if (await _downloaded(release, expected) case final ready?) {
        value = ready;
        return;
      }
      await dir.create(recursive: true);
      await _removeAllExcept(dir, partial);
      await source.download(release.apkUrl, partial, (received, total) {
        value = UpdateDownloading(release, received, total);
      });
      final actual = await platform.sha256(partial);
      if (actual != expected) {
        debugPrint(
          '[update] ${release.version} 校验对不上（期望 $expected，实际 $actual）',
        );
        await _delete(partial);
        value = UpdateFailed(UpdateStage.verify, release: release);
        return;
      }
      await partial.rename(apk.path);
      value = UpdateReady(release, apk);
    } catch (error) {
      debugPrint('[update] 下载 ${release.version} 失败：$error');
      await _delete(partial);
      value = UpdateFailed(UpdateStage.download, release: release);
    }
  }

  File _apkFile(Directory dir, ReleaseCandidate release) =>
      File('${dir.path}/${apkAssetName(release.version)}');

  /// 这个包已经下好、校验和对得上，就不用再下。
  Future<UpdateReady?> _downloaded(
    ReleaseCandidate release,
    String expected,
  ) async {
    final platform = _platform;
    final dir = _downloadDir;
    if (platform == null || dir == null) return null;
    final apk = _apkFile(dir, release);
    try {
      if (await apk.exists() && await platform.sha256(apk) == expected) {
        return UpdateReady(release, apk);
      }
    } catch (error) {
      debugPrint('[update] 读已下载的 ${release.version} 失败：$error');
    }
    return null;
  }

  static Future<void> _delete(File file) async {
    try {
      if (await file.exists()) await file.delete();
    } catch (error) {
      debugPrint('[update] 删除 ${file.path} 失败：$error');
    }
  }

  /// 安装下好的包。第一次要用户去系统设置里允许 Smelt 安装应用。
  Future<void> install() async {
    final (release, apk) = switch (value) {
      UpdateReady(:final release, :final apk) ||
      UpdateNeedsPermission(:final release, :final apk) => (release, apk),
      UpdateFailed(stage: UpdateStage.install, :final release?, :final apk?) =>
        (release, apk),
      _ => (null, null),
    };
    if (release == null || apk == null) return;
    await _install(release, apk);
  }

  Future<void> openInstallSettings() async => _platform?.openInstallSettings();

  Future<void> _install(ReleaseCandidate release, File apk) async {
    final platform = _platform;
    if (platform == null) return;
    if (!await platform.canInstall()) {
      value = UpdateNeedsPermission(release, apk);
      return;
    }
    value = UpdateInstalling(release, apk);
    try {
      await platform.install(apk);
    } catch (error) {
      debugPrint('[update] 安装 ${release.version} 失败：$error');
      value = UpdateFailed(UpdateStage.install, release: release, apk: apk);
    }
  }

  void _onInstallFailed(bool cancelled) {
    if (value case UpdateInstalling(:final release, :final apk)) {
      value = cancelled
          ? UpdateReady(release, apk)
          : UpdateFailed(UpdateStage.install, release: release, apk: apk);
    }
  }

  /// 启动时清理下载目录：下了一半的、已经装上（或更旧）的版本都删掉。比装着的新、
  /// 已经下好的留着：去系统设置里授权会让 Android 杀掉进程，回来查到它时直接用。
  Future<void> _cleanup(String installed) async {
    final dir = _downloadDir;
    if (dir == null) return;
    final apkName = RegExp(r'^Smelt-(.+)-android-arm64\.apk$');
    try {
      if (!await dir.exists()) return;
      await for (final entry in dir.list()) {
        final name = entry.uri.pathSegments.lastWhere((s) => s.isNotEmpty);
        final version = apkName.firstMatch(name)?.group(1);
        if (version == null || !isNewerRelease(version, installed)) {
          await entry.delete(recursive: true);
        }
      }
    } catch (error) {
      debugPrint('[update] 清理下载目录失败：$error');
    }
  }

  static Future<void> _removeAllExcept(Directory dir, File keep) async {
    await for (final entry in dir.list()) {
      if (entry.path != keep.path) await entry.delete(recursive: true);
    }
  }

  @override
  void dispose() {
    _failures?.cancel();
    super.dispose();
  }
}

/// GitHub Releases 列表（或打包时指定的更新清单）。
class HttpUpdateSource implements UpdateSource {
  const HttpUpdateSource();

  /// 查列表而不是 `releases/latest`：内测通道要能看到预发布。
  static final _releasesApi = Uri.parse(
    'https://api.github.com/repos/smelt-ai/smelt/releases?per_page=20',
  );

  /// `--dart-define=SMELT_UPDATE_MANIFEST_URL=https://…/update.{channel}.json`：自己分发
  /// 的打包方把更新指向自己的服务器。清单是 `{"version": "2026.1027.0", "url": "https://…/x.apk"}`，
  /// APK 旁边要有同一份 `SHA256SUMS`。
  static const _manifestTemplate = String.fromEnvironment(
    'SMELT_UPDATE_MANIFEST_URL',
  );

  @override
  Future<List<ReleaseCandidate>> candidates(UpdateChannel channel) async {
    if (_manifestTemplate.isNotEmpty) {
      final url = _https(
        _manifestTemplate.replaceAll('{channel}', channel.name),
      );
      final manifest = jsonDecode(await _getText(url));
      if (manifest is! Map<String, Object?>) {
        throw const FormatException('更新清单不是对象');
      }
      final version = manifest['version'];
      final apk = manifest['url'];
      if (version is! String || apk is! String) {
        throw const FormatException('更新清单缺 version 或 url');
      }
      _https(apk);
      return [ReleaseCandidate(version: version.trim(), apkUrl: apk.trim())];
    }
    final releases = jsonDecode(
      await _getText(_releasesApi, accept: 'application/vnd.github+json'),
    );
    if (releases is! List) throw const FormatException('Releases 接口返回的不是列表');
    return [
      for (final candidate in pickReleases(releases, channel))
        if (Uri.tryParse(candidate.apkUrl)?.scheme == 'https') candidate,
    ];
  }

  @override
  Future<String> checksum(String apkUrl) async {
    final url = _https(apkUrl);
    final file = url.pathSegments.lastWhere(
      (s) => s.isNotEmpty,
      orElse: () => throw FormatException('APK 地址里没有文件名：$apkUrl'),
    );
    final sums = await _getText(url.resolve(_checksumsAsset));
    final sum = checksumFor(sums, file);
    if (sum == null) {
      throw StateError('SHA256SUMS 里还没有 $file，发布可能还没传完');
    }
    return sum;
  }

  @override
  Future<void> download(
    String url,
    File to,
    void Function(int received, int? total) onProgress,
  ) async {
    final client = _client();
    try {
      final response = await (await client.getUrl(
        _https(url),
      )).close().timeout(_readTimeout);
      _ensureOk(response, url);
      final total = response.contentLength >= 0 ? response.contentLength : null;
      final sink = to.openWrite();
      var received = 0;
      var reported = 0;
      try {
        // 网断了不一定报错，读会一直挂着：一段时间收不到数据就当失败，用户可以重试。
        await for (final chunk in response.timeout(_readTimeout)) {
          sink.add(chunk);
          received += chunk.length;
          // 攒够一段再报，别每个 chunk 都重绘。
          if (received - reported >= 512 * 1024 || received == total) {
            reported = received;
            onProgress(received, total);
          }
        }
      } finally {
        await sink.close();
      }
    } finally {
      client.close();
    }
  }

  /// 连接建好之后，等下一段数据最多等这么久。
  static const _readTimeout = Duration(seconds: 60);

  static HttpClient _client() => HttpClient()
    ..userAgent = 'smelt-mobile-updater'
    ..connectionTimeout = const Duration(seconds: 20);

  static Future<String> _getText(Uri url, {String? accept}) async {
    final client = _client();
    try {
      final request = await client.getUrl(url);
      if (accept != null) request.headers.set(HttpHeaders.acceptHeader, accept);
      final response = await request.close().timeout(_readTimeout);
      _ensureOk(response, url.toString());
      return await response
          .timeout(_readTimeout)
          .transform(utf8.decoder)
          .join();
    } finally {
      client.close();
    }
  }

  static void _ensureOk(HttpClientResponse response, String url) {
    if (response.statusCode != HttpStatus.ok) {
      throw HttpException('HTTP ${response.statusCode}', uri: Uri.parse(url));
    }
  }

  static Uri _https(String raw) {
    final url = Uri.parse(raw);
    if (url.scheme != 'https') throw FormatException('更新地址必须是 HTTPS：$raw');
    return url;
  }
}

/// 和 `MainActivity` 里注册的 `smelt/app_update` 通道对话。
class MethodChannelUpdatePlatform implements UpdatePlatform {
  MethodChannelUpdatePlatform() {
    _channel.setMethodCallHandler((call) async {
      if (call.method == 'installFailed') {
        final args = call.arguments;
        final cancelled = args is Map && args['cancelled'] == true;
        if (args is Map) {
          debugPrint('[update] 系统没装上：${args['status']} ${args['message']}');
        }
        _failures.add(cancelled);
      }
    });
  }

  static const _channel = MethodChannel('smelt/app_update');
  final _failures = StreamController<bool>.broadcast();

  @override
  Future<String?> currentVersion() =>
      _channel.invokeMethod<String>('currentVersion');

  @override
  Future<String> sha256(File file) async =>
      (await _channel.invokeMethod<String>('sha256', {'path': file.path}))!;

  @override
  Future<bool> canInstall() async =>
      await _channel.invokeMethod<bool>('canInstall') ?? false;

  @override
  Future<void> openInstallSettings() =>
      _channel.invokeMethod<void>('openInstallSettings');

  @override
  Future<void> install(File apk) =>
      _channel.invokeMethod<void>('install', {'path': apk.path});

  @override
  Future<bool> installInProgress() async =>
      await _channel.invokeMethod<bool>('installInProgress') ?? false;

  @override
  Stream<bool> get installFailures => _failures.stream;
}
