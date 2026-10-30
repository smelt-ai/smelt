/// 推送通知（只有 iOS）：App 被挂起、锁屏、被杀掉时，各台机器的 smeltd 经推送中继和
/// APNs 叫醒手机。
///
/// 这里串起整条登记：申请通知权限 → 拿 APNs 令牌（Swift，`smelt/push` 通道）→ 找推送中继
/// 换句柄 → 交给 Rust 内核登记到每台机器。推送只带提醒类型和会话 id，文案由系统按
/// `Localizable.strings` 本地化；会话内容仍然只走 iroh。设计见
/// `docs/notification-architecture.md` 的「手机推送」。
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

import 'backend.dart';
import 'models.dart';
import 'preferences.dart';

/// 设置页显示的推送状态。
enum PushStatus {
  /// 用户没开。
  off,

  /// 这个构建没配推送中继（自己签名的开发版没填 `SMELT_PUSH_RELAY_HOST`）。
  notConfigured,

  /// 系统设置里不允许 Smelt 发通知。
  denied,

  /// 正在拿令牌、换句柄。
  registering,

  /// 拿不到 APNs 令牌，或连不上推送中继。回到前台时重试。
  failed,

  on,
}

/// 点开的通知要去的会话。
@immutable
class PushTarget {
  const PushTarget(this.runtimeId, this.sessionId);

  final String runtimeId;
  final String sessionId;

  @override
  bool operator ==(Object other) =>
      other is PushTarget &&
      other.runtimeId == runtimeId &&
      other.sessionId == sessionId;

  @override
  int get hashCode => Object.hash(runtimeId, sessionId);
}

/// 这个构建的推送配置（Info.plist 里编译期写好的）。
@immutable
class PushConfig {
  const PushConfig({
    required this.relayUrl,
    required this.environment,
    required this.topic,
  });

  /// 推送中继，`https://` 开头。
  final String relayUrl;

  /// `sandbox`（开发签名）或 `production`（TestFlight / App Store）。
  final String environment;

  /// App 的 Bundle ID。
  final String topic;
}

enum PushAuthorization { notDetermined, denied, authorized }

/// 原生一侧：权限、令牌、通知点击、已送达的通知。
abstract interface class PushPlatform {
  /// 没配推送中继时为 `null`。
  Future<PushConfig?> configuration();
  Future<PushAuthorization> authorization();

  /// 弹系统的授权框，返回是否允许。
  Future<bool> requestAuthorization();

  /// APNs 设备令牌（十六进制）。拿不到时抛错。
  Future<String> deviceToken();

  /// 冷启动：用户点了通知才把 App 拉起来的话，是那条通知要去的会话。取一次就清掉。
  Future<PushTarget?> takeLaunchTarget();

  /// App 运行中（含后台）点了通知。
  Stream<PushTarget> get taps;

  /// 通知中心里还留着的推送。
  Future<List<PushTarget>> delivered();
  Future<void> removeDelivered(List<PushTarget> targets);

  /// 打开系统设置里 Smelt 的那一页。
  Future<void> openSettings();
}

/// 推送中继：用 APNs 令牌换句柄（`smelt-push` 的 `/v1/register`）。
abstract interface class PushRelay {
  Future<String> register(PushConfig config, String deviceToken);
}

class PushService extends ValueNotifier<PushStatus> {
  PushService({
    required this.backend,
    required this.prefs,
    required this.platform,
    PushRelay? relay,
  }) : relay = relay ?? const HttpPushRelay(),
       super(PushStatus.off);

  /// 不支持推送的平台（安卓）、演示模式。设置里不显示推送。
  PushService.unsupported({required this.backend, required this.prefs})
    : platform = null,
      relay = const HttpPushRelay(),
      super(PushStatus.off);

  final Backend backend;
  final PreferencesStore prefs;
  final PushPlatform? platform;
  final PushRelay relay;

  bool get supported => platform != null;

  /// 点开的通知要去、还没打开的会话。外壳打开后清掉。
  final pendingOpen = ValueNotifier<PushTarget?>(null);

  StreamSubscription<PushTarget>? _taps;
  Future<void> _queue = Future.value();

  /// 内核初始化之后调一次：按偏好登记（不弹授权框），接住点开 App 的那条通知。
  Future<void> start() async {
    final platform = this.platform;
    if (platform == null) return;
    _taps ??= platform.taps.listen((target) => pendingOpen.value = target);
    final launch = await platform.takeLaunchTarget();
    if (launch != null) pendingOpen.value = launch;
    await _sync(prompt: false);
  }

  /// 设置里的开关。打开时第一次会弹系统授权框。
  Future<void> setEnabled(bool enabled) async {
    prefs.update(prefs.value.copyWith(pushEnabled: enabled));
    await _sync(prompt: enabled);
  }

  /// 设置里选要推哪几类提醒。
  Future<void> setKinds({bool? approval, bool? input, bool? failure}) async {
    prefs.update(
      prefs.value.copyWith(
        pushApproval: approval,
        pushInput: input,
        pushFailure: failure,
      ),
    );
    await _sync(prompt: false);
  }

  /// 回到前台：系统设置里的权限可能改过，上次也可能没连上中继；再对一次。
  Future<void> resumed() async {
    if (platform == null) return;
    await _sync(prompt: false);
  }

  /// 清掉通知中心里已经处理掉的会话的推送（APNs 没法远程撤回）。`stillWaiting` 是还在等
  /// 用户处理的会话。
  Future<void> clearHandled(bool Function(PushTarget) stillWaiting) async {
    final platform = this.platform;
    if (platform == null) return;
    try {
      final handled = [
        for (final target in await platform.delivered())
          if (!stillWaiting(target)) target,
      ];
      if (handled.isNotEmpty) await platform.removeDelivered(handled);
    } catch (error) {
      debugPrint('[push] 清理已送达的通知失败：$error');
    }
  }

  Future<void> openSettings() async => platform?.openSettings();

  /// 一次只跑一个：开关连点、回前台和设置同时触发时按顺序来。
  Future<void> _sync({required bool prompt}) {
    final next = _queue.then((_) => _doSync(prompt: prompt));
    _queue = next.catchError((Object _) {});
    return next;
  }

  Future<void> _doSync({required bool prompt}) async {
    final platform = this.platform;
    if (platform == null) return;
    final p = prefs.value;
    if (!p.pushEnabled) {
      backend.setPushRegistration(null);
      value = PushStatus.off;
      return;
    }
    final config = await platform.configuration();
    if (config == null) {
      value = PushStatus.notConfigured;
      return;
    }
    var authorization = await platform.authorization();
    if (authorization == PushAuthorization.notDetermined && prompt) {
      authorization = await platform.requestAuthorization()
          ? PushAuthorization.authorized
          : PushAuthorization.denied;
    }
    if (authorization != PushAuthorization.authorized) {
      // 系统不让弹，推了也看不见：取消各台机器上的登记，省得它们白发。
      backend.setPushRegistration(null);
      value = authorization == PushAuthorization.denied
          ? PushStatus.denied
          : PushStatus.off;
      return;
    }
    if (value != PushStatus.on) value = PushStatus.registering;
    try {
      final token = await platform.deviceToken();
      final handle = await relay.register(config, token);
      backend.setPushRegistration(
        PushRegistration(
          handle: handle,
          relayUrl: config.relayUrl,
          approval: p.pushApproval,
          input: p.pushInput,
          failure: p.pushFailure,
        ),
      );
      value = PushStatus.on;
    } catch (error) {
      // 机器上已有的登记不动：句柄多半还有效，推送照常。
      debugPrint('[push] 登记失败：$error');
      value = PushStatus.failed;
    }
  }

  @override
  void dispose() {
    _taps?.cancel();
    pendingOpen.dispose();
    super.dispose();
  }
}

class HttpPushRelay implements PushRelay {
  const HttpPushRelay();

  static const _timeout = Duration(seconds: 20);

  @override
  Future<String> register(PushConfig config, String deviceToken) async {
    final client = HttpClient()
      ..userAgent = 'smelt-mobile-push'
      ..connectionTimeout = _timeout;
    try {
      final base = config.relayUrl.endsWith('/')
          ? config.relayUrl.substring(0, config.relayUrl.length - 1)
          : config.relayUrl;
      final request = await client.postUrl(Uri.parse('$base/v1/register'));
      request.headers.contentType = ContentType.json;
      request.write(
        jsonEncode({
          'device_token': deviceToken,
          'environment': config.environment,
          'topic': config.topic,
        }),
      );
      final response = await request.close().timeout(_timeout);
      final body = await response
          .timeout(_timeout)
          .transform(utf8.decoder)
          .join();
      if (response.statusCode != HttpStatus.ok) {
        throw HttpException('推送中继回了 ${response.statusCode}：$body');
      }
      final handle = (jsonDecode(body) as Map<String, dynamic>)['handle'];
      if (handle is! String || handle.isEmpty) {
        throw const FormatException('推送中继没给句柄');
      }
      return handle;
    } finally {
      client.close();
    }
  }
}

/// 和 `AppDelegate` 里注册的 `smelt/push` 通道对话。
class MethodChannelPushPlatform implements PushPlatform {
  MethodChannelPushPlatform() {
    _channel.setMethodCallHandler((call) async {
      if (call.method == 'open') {
        final target = _target(call.arguments);
        if (target != null) _taps.add(target);
      }
    });
  }

  static const _channel = MethodChannel('smelt/push');
  final _taps = StreamController<PushTarget>.broadcast();

  static PushTarget? _target(Object? value) {
    if (value is! Map) return null;
    final runtimeId = value['runtimeId'];
    final sessionId = value['sessionId'];
    if (runtimeId is! String || sessionId is! String) return null;
    return PushTarget(runtimeId, sessionId);
  }

  @override
  Future<PushConfig?> configuration() async {
    final value = await _channel.invokeMapMethod<String, Object?>(
      'configuration',
    );
    final relay = value?['relayUrl'];
    final environment = value?['environment'];
    final topic = value?['topic'];
    if (relay is! String || relay.isEmpty) return null;
    if (environment is! String || topic is! String) return null;
    return PushConfig(relayUrl: relay, environment: environment, topic: topic);
  }

  @override
  Future<PushAuthorization> authorization() async =>
      switch (await _channel.invokeMethod<String>('authorization')) {
        'authorized' => PushAuthorization.authorized,
        'denied' => PushAuthorization.denied,
        _ => PushAuthorization.notDetermined,
      };

  @override
  Future<bool> requestAuthorization() async =>
      await _channel.invokeMethod<bool>('requestAuthorization') ?? false;

  @override
  Future<String> deviceToken() async =>
      (await _channel.invokeMethod<String>('deviceToken'))!;

  @override
  Future<PushTarget?> takeLaunchTarget() async =>
      _target(await _channel.invokeMethod<Object?>('takeLaunchTarget'));

  @override
  Stream<PushTarget> get taps => _taps.stream;

  @override
  Future<List<PushTarget>> delivered() async => [
    for (final value
        in await _channel.invokeListMethod<Object?>('delivered') ?? const [])
      ?_target(value),
  ];

  @override
  Future<void> removeDelivered(List<PushTarget> targets) =>
      _channel.invokeMethod<void>('removeDelivered', [
        for (final target in targets)
          {'runtimeId': target.runtimeId, 'sessionId': target.sessionId},
      ]);

  @override
  Future<void> openSettings() => _channel.invokeMethod<void>('openSettings');
}
