import 'dart:async';

import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/preferences.dart';
import 'package:smelt_mobile/data/push.dart';

/// 记下登记到内核的推送设置。
class _Backend extends DemoBackend {
  final registrations = <PushRegistration?>[];

  @override
  void setPushRegistration(PushRegistration? registration) =>
      registrations.add(registration);
}

class _Platform implements PushPlatform {
  PushConfig? config = const PushConfig(
    relayUrl: 'https://push.example.test',
    environment: 'sandbox',
    topic: 'ai.smelt.mobile',
  );
  PushAuthorization status = PushAuthorization.notDetermined;
  bool grant = true;
  int prompts = 0;
  PushTarget? launch;
  final tapController = StreamController<PushTarget>.broadcast();
  List<PushTarget> shown = [];
  final removed = <PushTarget>[];

  @override
  Future<PushConfig?> configuration() async => config;

  @override
  Future<PushAuthorization> authorization() async => status;

  @override
  Future<bool> requestAuthorization() async {
    prompts++;
    status = grant ? PushAuthorization.authorized : PushAuthorization.denied;
    return grant;
  }

  @override
  Future<String> deviceToken() async => 'ab' * 32;

  @override
  Future<PushTarget?> takeLaunchTarget() async {
    final target = launch;
    launch = null;
    return target;
  }

  @override
  Stream<PushTarget> get taps => tapController.stream;

  @override
  Future<List<PushTarget>> delivered() async => shown;

  @override
  Future<void> removeDelivered(List<PushTarget> targets) async =>
      removed.addAll(targets);

  @override
  Future<void> openSettings() async {}
}

class _Relay implements PushRelay {
  bool fail = false;
  final tokens = <String>[];

  @override
  Future<String> register(PushConfig config, String deviceToken) async {
    if (fail) throw Exception('offline');
    tokens.add(deviceToken);
    return 'handle-${tokens.length}';
  }
}

void main() {
  late _Backend backend;
  late _Platform platform;
  late _Relay relay;
  late PreferencesStore prefs;
  late PushService push;

  setUp(() {
    backend = _Backend();
    platform = _Platform();
    relay = _Relay();
    prefs = PreferencesStore.memory();
    push = PushService(
      backend: backend,
      prefs: prefs,
      platform: platform,
      relay: relay,
    );
  });

  tearDown(() => push.dispose());

  test('没开推送：启动时取消各台机器上的登记，不弹授权框', () async {
    await push.start();
    expect(backend.registrations, [null]);
    expect(push.value, PushStatus.off);
    expect(platform.prompts, 0);
  });

  test('打开时弹授权框，换到句柄后连同提醒类型登记', () async {
    await push.setEnabled(true);
    expect(platform.prompts, 1);
    expect(push.value, PushStatus.on);
    final registration = backend.registrations.single!;
    expect(registration.handle, 'handle-1');
    expect(registration.relayUrl, 'https://push.example.test');
    expect(
      [registration.approval, registration.input, registration.failure],
      [true, true, true],
    );

    await push.setKinds(input: false);
    expect(backend.registrations.last!.input, isFalse);
    expect(platform.prompts, 1, reason: '已经允许过，不再弹');

    await push.setEnabled(false);
    expect(backend.registrations.last, isNull);
    expect(push.value, PushStatus.off);
  });

  test('启动时只按偏好登记，不弹授权框', () async {
    prefs.update(const Preferences(pushEnabled: true));
    await push.start();
    expect(platform.prompts, 0);
    expect(push.value, PushStatus.off, reason: '还没决定过权限：等用户在设置里开');
    expect(backend.registrations, [null]);
  });

  test('拒绝授权：取消登记，说明在系统设置里关掉了', () async {
    platform.grant = false;
    await push.setEnabled(true);
    expect(push.value, PushStatus.denied);
    expect(backend.registrations, [null]);
  });

  test('没配推送中继的构建：不动登记', () async {
    platform.config = null;
    await push.setEnabled(true);
    expect(push.value, PushStatus.notConfigured);
    expect(backend.registrations, isEmpty);
  });

  test('连不上中继：不动机器上已有的登记，回前台再试', () async {
    platform.status = PushAuthorization.authorized;
    relay.fail = true;
    await push.setEnabled(true);
    expect(push.value, PushStatus.failed);
    expect(backend.registrations, isEmpty);

    relay.fail = false;
    await push.resumed();
    expect(push.value, PushStatus.on);
    expect(backend.registrations.single!.handle, 'handle-1');
  });

  test('点开的通知：冷启动和运行中都记成待打开', () async {
    platform.launch = const PushTarget('r1', 's1');
    await push.start();
    expect(push.pendingOpen.value, const PushTarget('r1', 's1'));

    push.pendingOpen.value = null;
    platform.tapController.add(const PushTarget('r2', 's2'));
    await Future<void>.delayed(Duration.zero);
    expect(push.pendingOpen.value, const PushTarget('r2', 's2'));
  });

  test('只清掉已经不在等的会话的通知', () async {
    platform.shown = const [PushTarget('r1', 's1'), PushTarget('r1', 's2')];
    await push.clearHandled((target) => target.sessionId == 's1');
    expect(platform.removed, const [PushTarget('r1', 's2')]);
  });
}
