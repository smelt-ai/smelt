/// 「智能体」页的数据：每台在线机器的自动化目录、可开对话的智能体选项。
///
/// 页面 State 持有一份，详情页拿同一份听变化：在详情里开关、立即运行之后，列表和详情
/// 一起更新，不各自再取一遍。请求只在事件里发（创建时、机器上线时、下拉刷新、用户动作），
/// build 只读这里的缓存。
library;

import 'dart:async';

import 'package:flutter/foundation.dart';

import '../../data/models.dart';
import '../../data/stores.dart';

/// 一台机器的智能体数据。
@immutable
class MachineAgents {
  const MachineAgents({
    this.catalog,
    this.error,
    this.loading = false,
    this.startOptions,
  });

  /// null = 还没取到。空目录和「还没取到」必须分开说，否则用户会以为自己的自动化没了。
  final AutomationCatalog? catalog;

  /// 上一次取目录失败的原因（给人看的一句话）。
  final String? error;
  final bool loading;

  /// 智能体定义 id → 工作区目录里对应的 agent 选项 id。null = 工作区目录还没取到；
  /// 不在表里的智能体在这台机器上开不了对话（引擎没注册成产品级引擎）。
  final Map<String, String>? startOptions;

  MachineAgents copyWith({
    AutomationCatalog? catalog,
    String? Function()? error,
    bool? loading,
    Map<String, String>? startOptions,
  }) => MachineAgents(
    catalog: catalog ?? this.catalog,
    error: error == null ? this.error : error(),
    loading: loading ?? this.loading,
    startOptions: startOptions ?? this.startOptions,
  );
}

class AgentsBoard extends ChangeNotifier {
  AgentsBoard(this._model) {
    _model.fleet.addListener(_onFleet);
    _onFleet();
  }

  final AppModel _model;
  final Map<String, MachineAgents> _machines = {};
  Set<String> _online = {};

  /// 每台机器取目录的代号：较早发出、较晚回来的结果不能盖掉较新的。
  final Map<String, int> _generation = {};

  /// 工作区目录单独计代号：动作返回的新目录会作废正在路上的取目录，但不该作废它。
  final Map<String, int> _optionsGeneration = {};

  /// 开关的乐观值：键为 `runtimeId/automationId`，值为（目标值，这次操作的序号）。
  /// 只有最后一次操作结束时才撤掉，连点两下不会在中途闪回旧值。
  final Map<String, (bool, int)> _toggles = {};
  int _toggleSeq = 0;

  /// 正在「立即运行一次」的自动化。
  final Set<String> _runningOnce = {};
  bool _disposed = false;

  MachineAgents machine(String runtimeId) =>
      _machines[runtimeId] ?? const MachineAgents();

  /// 界面上该显示的开关值：有进行中的操作就显示用户刚拨到的位置。
  bool enabled(String runtimeId, AutomationSummary automation) =>
      _toggles[sessionKey(runtimeId, automation.id)]?.$1 ?? automation.enabled;

  bool isRunningOnce(String runtimeId, String automationId) =>
      _runningOnce.contains(sessionKey(runtimeId, automationId));

  AutomationSummary? automation(String runtimeId, String automationId) {
    for (final a in machine(runtimeId).catalog?.automations ?? const []) {
      if (a.id == automationId) return a;
    }
    return null;
  }

  AgentDefinitionSummary? agent(String runtimeId, String agentId) {
    for (final a in machine(runtimeId).catalog?.agents ?? const []) {
      if (a.id == agentId) return a;
    }
    return null;
  }

  /// 机器上线（含首次看到时就在线）就取一次；下线不取，留着的数据界面也不显示。
  void _onFleet() {
    final runtimes = _model.fleet.value.runtimes;
    final known = {for (final r in runtimes) r.id};
    final online = {
      for (final r in runtimes)
        if (r.online) r.id,
    };
    var changed = false;
    for (final gone in _machines.keys.where((id) => !known.contains(id))) {
      _generation.remove(gone);
      _optionsGeneration.remove(gone);
      changed = true;
    }
    _machines.removeWhere((id, _) => !known.contains(id));
    final cameUp = online.difference(_online);
    _online = online;
    for (final id in cameUp) {
      unawaited(_load(id));
    }
    if (changed && cameUp.isEmpty) _notify();
  }

  /// 下拉刷新：所有在线机器重取。
  Future<void> refresh() => Future.wait([for (final id in _online) _load(id)]);

  /// 取目录失败后重试这一台。
  Future<void> reload(String runtimeId) =>
      _online.contains(runtimeId) ? _load(runtimeId) : Future.value();

  Future<void> _load(String runtimeId) async {
    final generation = (_generation[runtimeId] ?? 0) + 1;
    _generation[runtimeId] = generation;
    _put(runtimeId, (m) => m.copyWith(loading: true));
    // 工作区目录只用来判断哪些智能体能开对话，和自动化目录分开取：它刚连上时可能要等
    // 几秒，不该拖住列表。
    unawaited(_loadStartOptions(runtimeId));
    try {
      final catalog = await _model.backend.automationCatalog(runtimeId);
      if (_generation[runtimeId] != generation) return;
      _put(
        runtimeId,
        (m) => m.copyWith(catalog: catalog, error: () => null, loading: false),
      );
    } catch (error) {
      if (_generation[runtimeId] != generation) return;
      _put(
        runtimeId,
        (m) => m.copyWith(error: () => describeError(error), loading: false),
      );
    }
  }

  Future<void> _loadStartOptions(String runtimeId) async {
    final generation = (_optionsGeneration[runtimeId] ?? 0) + 1;
    _optionsGeneration[runtimeId] = generation;
    try {
      final workspace = await _model.backend.workspaceCatalog(runtimeId);
      if (_optionsGeneration[runtimeId] != generation) return;
      _put(
        runtimeId,
        (m) => m.copyWith(
          startOptions: {
            for (final option in workspace.agents)
              ?option.agentDefinitionId: option.id,
          },
        ),
      );
    } catch (_) {
      // 取不到就当「还不知道能不能开」：开始对话的按钮不出现，不影响列表本身。
    }
  }

  /// 开关一条自动化：先把开关拨过去，失败回到原位并把错误抛给调用方提示。
  Future<void> setEnabled(
    String runtimeId,
    String automationId,
    bool enabled,
  ) async {
    final key = sessionKey(runtimeId, automationId);
    final seq = ++_toggleSeq;
    _toggles[key] = (enabled, seq);
    _notify();
    try {
      final catalog = await _model.backend.setAutomationEnabled(
        runtimeId,
        automationId,
        enabled,
      );
      _replace(runtimeId, catalog);
    } finally {
      if (_toggles[key]?.$2 == seq) _toggles.remove(key);
      _notify();
    }
  }

  /// 立刻运行一次。返回的目录里 `last_run` 已经是这次运行。
  Future<void> runOnce(String runtimeId, String automationId) async {
    final key = sessionKey(runtimeId, automationId);
    if (!_runningOnce.add(key)) return;
    _notify();
    try {
      final catalog = await _model.backend.runAutomationOnce(
        runtimeId,
        automationId,
      );
      _replace(runtimeId, catalog);
    } finally {
      _runningOnce.remove(key);
      _notify();
    }
  }

  /// 动作返回的目录比正在路上的取目录更新：作废那次取目录，直接用这份。
  void _replace(String runtimeId, AutomationCatalog catalog) {
    if (!_machines.containsKey(runtimeId)) return;
    _generation[runtimeId] = (_generation[runtimeId] ?? 0) + 1;
    _put(
      runtimeId,
      (m) => m.copyWith(catalog: catalog, error: () => null, loading: false),
    );
  }

  void _put(String runtimeId, MachineAgents Function(MachineAgents) update) {
    if (_disposed) return;
    _machines[runtimeId] = update(machine(runtimeId));
    notifyListeners();
  }

  void _notify() {
    if (!_disposed) notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    _model.fleet.removeListener(_onFleet);
    super.dispose();
  }
}
