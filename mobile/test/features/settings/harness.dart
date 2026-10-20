import 'package:flutter/material.dart' hide ConnectionState;
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/preferences.dart';
import 'package:smelt_mobile/data/stores.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';
import 'package:smelt_mobile/theme/theme.dart';
import 'package:smelt_mobile/ui/adaptive.dart';

/// 演示后端，记下机器相关的调用。
class SettingsBackend extends DemoBackend {
  SettingsBackend({this.statuses = const {}, this.relays = const {}})
    : super(now: DateTime(2025, 6, 1, 14));

  /// 机器 id → 换成这个连接状态（演示数据里没有被撤销、要升级的机器）。
  final Map<String, RuntimeStatus> statuses;

  /// 机器 id → 换成这个中继地址。
  final Map<String, String> relays;

  @override
  Stream<FleetView> fleetEvents() => super.fleetEvents().map(
    (view) => FleetView(
      runtimes: [
        for (final r in view.runtimes)
          FleetRuntime(
            record: _withRelay(r.record, relays[r.record.runtimeId]),
            status: statuses[r.record.runtimeId] ?? r.status,
          ),
      ],
    ),
  );

  static RuntimeRecord _withRelay(RuntimeRecord r, String? relay) =>
      relay == null
      ? r
      : RuntimeRecord(
          runtimeId: r.runtimeId,
          name: r.name,
          endpointId: r.endpointId,
          relayUrl: relay,
          hostname: r.hostname,
          os: r.os,
          version: r.version,
          accent: r.accent,
          position: r.position,
          pairedAtMs: r.pairedAtMs,
        );

  final moves = <(String, int)>[];
  final forgotten = <String>[];

  @override
  Future<void> moveRuntime(String runtimeId, int toIndex) {
    moves.add((runtimeId, toIndex));
    return super.moveRuntime(runtimeId, toIndex);
  }

  @override
  Future<void> forgetRuntime(String runtimeId) {
    forgotten.add(runtimeId);
    return super.forgetRuntime(runtimeId);
  }
}

/// 起一个最小外壳：主题、英文文案、AppScope，以及一个真会压栈的 DetailHost（手机布局）。
/// 视口拉高，整页都在一屏里，不用滚。
Future<AppModel> pumpSettingsHarness(
  WidgetTester tester,
  SettingsBackend backend,
  Widget child, {
  PreferencesStore? prefs,
}) async {
  tester.view.physicalSize = const Size(800, 3000);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);

  final model = AppModel(backend, prefs ?? PreferencesStore.memory())..start();
  final navigator = GlobalKey<NavigatorState>();
  await tester.pumpWidget(
    AppScope(
      model: model,
      child: MaterialApp(
        navigatorKey: navigator,
        locale: const Locale('en'),
        theme: buildGalleyTheme(Brightness.light),
        localizationsDelegates: L10n.localizationsDelegates,
        supportedLocales: L10n.supportedLocales,
        home: Scaffold(
          body: DetailHost(
            open: (_, builder) => navigator.currentState!.push(
              MaterialPageRoute<void>(builder: builder),
            ),
            selectedKey: null,
            child: child,
          ),
        ),
      ),
    ),
  );
  // 机队流的首份数据经微任务送达。
  await tester.pumpAndSettle();
  // 测试框架先拆组件树再跑 tearDown，这时已没有人在听这些 store。
  addTearDown(model.dispose);
  return model;
}
