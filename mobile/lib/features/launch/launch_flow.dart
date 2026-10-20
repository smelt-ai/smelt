/// 新建 / 续接会话的公共流程：发创建请求，等它进会话目录，再打开。
///
/// 为什么要等它进目录：打开会话要知道它是对话还是终端（[SessionSummary.kind]），
/// 这只有目录里有；创建请求只回一个 id。
library;

import 'dart:async';

import 'package:flutter/foundation.dart';

import '../../data/backend.dart';
import '../../data/models.dart';
import '../../data/stores.dart';
import '../../ui/adaptive.dart';
import '../session/open_session.dart';

/// 建好之后等它出现在目录里的上限。运行时推送目录是合并后整份推的，正常一两秒内到；
/// 超过这个时间多半是连接出了问题，给用户一句交代，不无限转圈。
const launchListTimeout = Duration(seconds: 15);

/// 一次新建意图。[requestId] 在用户做出选择的那一刻取一次，失败重试沿用同一个，
/// 运行时据此落到同一个会话，不会因为「超时了再点一次」开出两个。
@immutable
class LaunchIntent {
  const LaunchIntent({
    required this.runtimeId,
    required this.projectRoot,
    required this.requestId,
    required this.label,
    this.agentOptionId,
    this.launchKey,
    this.resumeId,
  });

  final String runtimeId;

  /// 空串：不选项目，只有产品智能体的对话可以。
  final String projectRoot;
  final String requestId;

  /// 给人看的名字（「Claude Code」「工作助手」、历史会话标题），进度和报错里用。
  final String label;
  final String? agentOptionId;
  final String? launchKey;
  final String? resumeId;

  Future<String> send(Backend backend) => backend.createSession(
    runtimeId,
    projectRoot: projectRoot,
    agentOptionId: agentOptionId,
    launchKey: launchKey,
    resumeId: resumeId,
    requestId: requestId,
  );
}

/// 等 [sessionId] 出现在 [runtimeId] 的会话列表里；[timeout] 内没等到返回 null。
Future<SessionSummary?> waitForSession(
  CatalogStore catalog,
  String runtimeId,
  String sessionId, {
  Duration timeout = launchListTimeout,
}) {
  final already = catalog.session(runtimeId, sessionId);
  if (already != null) return Future.value(already);
  final done = Completer<SessionSummary?>();
  Timer? timer;
  late final VoidCallback listener;
  void finish(SessionSummary? session) {
    if (done.isCompleted) return;
    catalog.removeListener(listener);
    timer?.cancel();
    done.complete(session);
  }

  listener = () {
    final session = catalog.session(runtimeId, sessionId);
    if (session != null) finish(session);
  };
  catalog.addListener(listener);
  timer = Timer(timeout, () => finish(null));
  return done.future;
}

/// 等它进目录再打开。返回是否打开了（false = 超时还没出现）。
Future<bool> openWhenListed(
  AppModel model,
  DetailHost host,
  String runtimeId,
  String sessionId,
) async {
  final session = await waitForSession(model.catalog, runtimeId, sessionId);
  if (session == null) return false;
  openSessionWith(host, runtimeId, session);
  return true;
}
