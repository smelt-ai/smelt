/// 界面状态：把内核的事件流留住最新一份，界面读 [ValueListenable]。
///
/// 只留状态、不做判断。动作（批、答、发）直接调 [Backend]，结果从事件流回来。
library;

import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import 'backend.dart';
import 'models.dart';
import 'preferences.dart';

/// 单据和会话在界面里的统一键。
String sessionKey(String runtimeId, String sessionId) =>
    '$runtimeId/$sessionId';

class FleetStore extends ValueNotifier<FleetView> {
  FleetStore() : super(const FleetView(runtimes: []));

  FleetRuntime? runtime(String runtimeId) {
    for (final r in value.runtimes) {
      if (r.record.runtimeId == runtimeId) return r;
    }
    return null;
  }

  bool get isEmpty => value.runtimes.isEmpty;
}

enum StampKind { allowed, denied, answered }

/// 刚在单据上做的决定：单据离轨时盖这个章。
@immutable
class StampRecord {
  const StampRecord({required this.kind, required this.at});

  final StampKind kind;
  final DateTime at;
}

class PassStore extends ValueNotifier<Pass> {
  PassStore() : super(const Pass(tickets: [], stations: []));

  final Map<String, StampRecord> _stamps = {};

  /// 章只在单据离轨那几秒有用，读的时候按时间筛掉旧的，不另起计时器。
  static const stampLifetime = Duration(seconds: 8);

  Map<String, StampRecord> get stamps {
    final now = DateTime.now();
    _stamps.removeWhere((_, r) => now.difference(r.at) > stampLifetime);
    return Map.unmodifiable(_stamps);
  }

  /// 记下这张单据的决定。
  void stamp(String key, StampKind kind) {
    _stamps[key] = StampRecord(kind: kind, at: DateTime.now());
  }

  /// 每个审批的决定，按「机器 / 会话 / 工具调用」记，App 这一次运行里一直留着：
  /// 单据离轨了，对话时间线里那条工具调用下面还留着章。决定不消失。
  final Map<String, StampRecord> _decisions = {};

  void decide(
    String runtimeId,
    String sessionId,
    String toolCallId,
    StampKind kind,
  ) => _decisions['$runtimeId/$sessionId/$toolCallId'] = StampRecord(
    kind: kind,
    at: DateTime.now(),
  );

  StampRecord? decision(
    String runtimeId,
    String sessionId,
    String toolCallId,
  ) => _decisions['$runtimeId/$sessionId/$toolCallId'];
}

/// 每台机器的完整会话列表（「会话」页用），以及提醒事件。
class CatalogStore extends ChangeNotifier {
  final Map<String, SessionList> _lists = {};
  final _attention = StreamController<(String, AttentionItem)>.broadcast();
  final _resolved = StreamController<(String, String)>.broadcast();
  final _errors = StreamController<(String, String)>.broadcast();

  SessionList? list(String runtimeId) => _lists[runtimeId];

  SessionSummary? session(String runtimeId, String sessionId) {
    for (final s in _lists[runtimeId]?.sessions ?? const <SessionSummary>[]) {
      if (s.id == sessionId) return s;
    }
    return null;
  }

  /// 新提醒（机器 id，提醒）。
  Stream<(String, AttentionItem)> get attention => _attention.stream;

  /// 要处理的事过去了（机器 id，会话 id）。
  Stream<(String, String)> get resolved => _resolved.stream;

  Stream<(String, String)> get errors => _errors.stream;

  void apply(CatalogEvent event) {
    final runtimeId = event.runtimeId;
    switch (event.kind) {
      case CatalogEventKind.sessions:
        final list = event.sessions;
        if (list == null) return;
        _lists[runtimeId] = list;
        notifyListeners();
      case CatalogEventKind.attention:
        final item = event.attention;
        if (item != null) _attention.add((runtimeId, item));
      case CatalogEventKind.attentionResolved:
        final id = event.sessionId;
        if (id != null) _resolved.add((runtimeId, id));
      case CatalogEventKind.error:
        final message = event.message;
        if (message != null) _errors.add((runtimeId, message));
    }
  }

  void forget(String runtimeId) {
    if (_lists.remove(runtimeId) != null) notifyListeners();
  }

  @override
  void dispose() {
    _attention.close();
    _resolved.close();
    _errors.close();
    super.dispose();
  }
}

/// 整个 App 的数据根：一个 [Backend] 和由它喂的几份状态。
class AppModel {
  AppModel(this.backend, this.prefs, {this.deviceId});

  final Backend backend;
  final PreferencesStore prefs;

  /// 这台手机的设备身份（iroh endpoint id）。电脑端的设备列表、撤销都认它。
  final String? deviceId;
  final fleet = FleetStore();
  final pass = PassStore();
  final catalog = CatalogStore();
  final _subscriptions = <StreamSubscription<Object?>>[];

  /// 内核初始化（`initClient`）之后调一次。
  void start() {
    if (_subscriptions.isNotEmpty) return;
    _subscriptions
      ..add(
        backend.fleetEvents().listen((view) {
          final known = {for (final r in view.runtimes) r.record.runtimeId};
          for (final gone
              in fleet.value.runtimes
                  .map((r) => r.record.runtimeId)
                  .where((id) => !known.contains(id))) {
            catalog.forget(gone);
          }
          fleet.value = view;
        }),
      )
      ..add(backend.passEvents().listen((p) => pass.value = p))
      ..add(backend.catalogEvents().listen(catalog.apply));
  }

  void dispose() {
    for (final s in _subscriptions) {
      s.cancel();
    }
    fleet.dispose();
    pass.dispose();
    prefs.dispose();
    catalog.dispose();
  }
}

class AppScope extends InheritedWidget {
  const AppScope({super.key, required this.model, required super.child});

  final AppModel model;

  static AppModel of(BuildContext context) {
    final scope = context.getInheritedWidgetOfExactType<AppScope>();
    assert(scope != null, 'AppScope 缺失');
    return scope!.model;
  }

  @override
  bool updateShouldNotify(AppScope old) => !identical(old.model, model);
}
