import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/demo_backend.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/data/preferences.dart';
import 'package:smelt_mobile/data/stores.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';
import 'package:smelt_mobile/theme/theme.dart';
import 'package:smelt_mobile/ui/adaptive.dart';

/// 演示后端加几样测试要的东西：记下新建 / 删除调用，能让它们失败，能往某台机器的
/// 会话列表里追加会话（智能体对话、自动化运行这些演示数据里没有的）。
class TestBackend extends DemoBackend {
  TestBackend({this.extra = const {}}) : super(now: DateTime(2025, 6, 1, 14));

  /// 机器 id → 追加到它会话列表末尾的会话。
  final Map<String, List<SessionSummary>> extra;

  final created = <CreateCall>[];
  final deleted = <(String, String)>[];

  /// 前几次新建直接失败。
  int failCreates = 0;

  /// 新建成功但会话一直不出现在目录里（等待超时的场景）。
  bool createdNeverAppears = false;

  /// 删除时抛这个（模拟运行时拒绝删除自动化运行的会话）。
  Object? deleteError;

  @override
  Stream<CatalogEvent> catalogEvents() => super.catalogEvents().map((event) {
    final more = extra[event.runtimeId];
    final list = event.sessions;
    if (more == null || list == null) return event;
    return CatalogEvent(
      runtimeId: event.runtimeId,
      kind: event.kind,
      sessions: SessionList(
        sessions: [...list.sessions, ...more],
        cached: list.cached,
        savedAtMs: list.savedAtMs,
      ),
    );
  });

  @override
  Future<String> createSession(
    String runtimeId, {
    required String projectRoot,
    String? agentOptionId,
    String? launchKey,
    String? resumeId,
    SessionKind? kind,
    String? requestId,
  }) async {
    created.add(
      CreateCall(
        runtimeId: runtimeId,
        projectRoot: projectRoot,
        agentOptionId: agentOptionId,
        launchKey: launchKey,
        resumeId: resumeId,
        requestId: requestId,
      ),
    );
    if (failCreates > 0) {
      failCreates--;
      throw const SmeltError(kind: SmeltErrorKind.offline, message: '连接断了');
    }
    if (createdNeverAppears) return 'never-listed';
    return super.createSession(
      runtimeId,
      projectRoot: projectRoot,
      agentOptionId: agentOptionId,
      launchKey: launchKey,
      resumeId: resumeId,
      kind: kind,
      requestId: requestId,
    );
  }

  @override
  Future<void> deleteSession(String runtimeId, String sessionId) async {
    deleted.add((runtimeId, sessionId));
    final error = deleteError;
    if (error != null) throw error;
    await super.deleteSession(runtimeId, sessionId);
  }
}

class CreateCall {
  const CreateCall({
    required this.runtimeId,
    required this.projectRoot,
    this.agentOptionId,
    this.launchKey,
    this.resumeId,
    this.requestId,
  });

  final String runtimeId;
  final String projectRoot;
  final String? agentOptionId;
  final String? launchKey;
  final String? resumeId;
  final String? requestId;
}

/// 起一个 App 外壳：主题、文案、AppScope、一个记下打开了什么的 DetailHost。
/// 关掉动效（状态点的呼吸会让帧一直排着）；视口拉高，几台机器都在一屏里。
Future<({AppModel model, List<String> opened})> pumpHarness(
  WidgetTester tester,
  TestBackend backend,
  Widget child,
) async {
  tester.view.physicalSize = const Size(800, 4000);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.reset);

  final model = AppModel(backend, PreferencesStore.memory())..start();
  final opened = <String>[];
  await tester.pumpWidget(
    AppScope(
      model: model,
      child: MaterialApp(
        theme: buildGalleyTheme(Brightness.light),
        localizationsDelegates: L10n.localizationsDelegates,
        supportedLocales: L10n.supportedLocales,
        builder: (context, child) => MediaQuery(
          data: MediaQuery.of(context).copyWith(disableAnimations: true),
          child: child!,
        ),
        home: Scaffold(
          body: DetailHost(
            open: (key, _) => opened.add(key),
            selectedKey: null,
            child: child,
          ),
        ),
      ),
    ),
  );
  // 事件流的首份数据经微任务送达，工作区目录是异步取的。
  await settle(tester);
  // 测试框架先拆掉组件树再跑 tearDown，这时已没有人在听这些 store。
  addTearDown(model.dispose);
  return (model: model, opened: opened);
}

/// 不用 pumpAndSettle：耗时标签每秒走一次，永远「不安定」。
Future<void> settle(WidgetTester tester) async {
  for (var i = 0; i < 6; i++) {
    await tester.pump(const Duration(milliseconds: 50));
  }
}
