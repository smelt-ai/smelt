import 'dart:async';

import 'package:flutter/foundation.dart';

import '../models/session_filters.dart';
import 'runtime_service.dart';

/// 一张待办卡片：会话本身 + 它到底在等什么。
///
/// `permission`/`elicitation` 可能暂时为 null——「哪些会话在等我」由会话列表立刻
/// 给出，而「等的是什么」要再取一次详情。先渲染标题占位、详情到了再补全，好过让
/// 整张卡片等在 loading 上。
@immutable
class PendingActionItem {
  const PendingActionItem({
    required this.session,
    this.permission,
    this.elicitation,
    this.detailsLoaded = false,
  });

  final SessionSummary session;
  final PermissionRequest? permission;
  final Elicitation? elicitation;

  /// 详情取回来了。有些提醒（比如失败）本来就没有审批卡，取回来是空的也算到了。
  final bool detailsLoaded;

  String get sessionId => session.id;

  /// 详情还在路上。卡片据此决定是渲染按钮还是渲染占位。
  bool get isLoadingDetails => !detailsLoaded;

  /// 卡片主标题：优先用 agent 自己的问法，退回 attention 的摘要。
  String get question =>
      permission?.question ??
      elicitation?.message ??
      session.attention?.message ??
      session.title;
}

/// 取一个会话当前在等的审批和询问。
typedef PendingActionsFetcher =
    Future<PendingActions> Function(String sessionId);

/// 指挥台的数据源：把「谁在等我」和「等的是什么」拼成一份卡片列表。
///
/// 关键约束是**不占用订阅槽**。会话页的订阅是单个的，指挥台却要同时盯住一批
/// 会话。这里不去抢订阅，而是组合两个能力：
///
///   - 会话列表 → 谁在等我（Rust 推送，无需订阅）
///   - `pendingActions` → 等的是什么（一问一答，无需订阅）
///
/// 回批同样不需要订阅（`respondApproval` 带显式 sessionId）。会话摘要一变（新的
/// 审批、阶段变化）就重取一次详情，旧卡片不会挂着已经答过的问题。
class PendingActionsController extends ChangeNotifier {
  PendingActionsController({
    Stream<List<SessionSummary>>? sessions,
    List<SessionSummary>? initialSessions,
    PendingActionsFetcher? fetchDetails,
  }) : _fetchDetails = fetchDetails ?? runtimeService.pendingActions {
    _sessions = initialSessions ?? runtimeService.sessions;
    _rebuild();
    _sessionsSub = (sessions ?? runtimeService.sessionsStream).listen((next) {
      _sessions = next;
      _rebuild();
    });
  }

  final PendingActionsFetcher _fetchDetails;

  StreamSubscription<List<SessionSummary>>? _sessionsSub;

  List<SessionSummary> _sessions = const [];

  /// 已取回的详情，连同取的时候会话摘要的指纹。指纹变了（新的审批、阶段变化）
  /// 就重取；会话离开待办集合时丢掉，下次再进来会重新取。
  final Map<String, ({String key, PendingActions details})> _details = {};

  /// 正在取的会话和对应的指纹，避免同一状态反复请求。
  final Map<String, String> _inFlight = {};

  bool _disposed = false;

  List<PendingActionItem> _items = const [];
  List<PendingActionItem> get items => _items;

  /// 每一段都按最后活动时间倒序。
  ///
  /// 分诊的两个维度就是「什么状态」和「多久以前」，段负责前者，段内排序负责后者。
  /// 没有活动时间的会话（电脑上开着但这次还没跑过）全都是 `0`，彼此分不出先后，
  /// 回落到列表原序：那是 Rust 按桌面菜单排好的，比随机顺序有意义。
  List<SessionSummary> _section(bool Function(SessionSummary) predicate) {
    final order = {
      for (final (index, session) in _sessions.indexed) session.id: index,
    };
    final matched = _sessions.where(predicate).toList()
      ..sort((a, b) {
        final byTime = b.updatedAt.compareTo(a.updatedAt);
        return byTime != 0 ? byTime : order[a.id]!.compareTo(order[b.id]!);
      });
    return List.unmodifiable(matched);
  }

  List<SessionSummary> get running => _section(sessionIsRunning);

  /// 「刚完成」——跑完且未读。读过提醒就退出这一段。
  List<SessionSummary> get recentlyDone => _section(sessionRecentlyDone);

  /// 「最近跑过」——跑完且已读。接住从「刚完成」里掉出来的那些。
  ///
  /// 这一段**不会自己清空**，也不截断：Projects 是按项目树组织的，智能体对话根本
  /// 不在树里，被截掉的那些在那儿也找不到。
  List<SessionSummary> get recentRuns => _section(sessionRecentlyRan);

  /// 「闲置」——上面四段都不要的那些。指挥台从这里开始真正展示**全部**对话。
  List<SessionSummary> get idle => _section(sessionIsIdle);

  /// 会话摘要里跟「在等什么」有关的部分。
  static String _detailsKey(SessionSummary session) => [
    session.phase.name,
    session.status.name,
    session.updatedAt,
    session.detail ?? '',
    session.attention?.kind.name ?? '',
    session.attention?.message ?? '',
  ].join('\u0001');

  void _rebuild() {
    if (_disposed) return;
    final waiting = _section(sessionNeedsAction);
    final waitingIds = waiting.map((session) => session.id).toSet();
    _details.removeWhere((id, _) => !waitingIds.contains(id));

    final next = <PendingActionItem>[];
    for (final session in waiting) {
      final key = _detailsKey(session);
      final loaded = _details[session.id];
      if (loaded?.key != key && _inFlight[session.id] != key) {
        _request(session.id, key);
      }
      final details = loaded?.details;
      next.add(
        PendingActionItem(
          session: session,
          permission: details?.permissions.firstOrNull,
          elicitation: details?.elicitation,
          detailsLoaded: details != null,
        ),
      );
    }

    // 只比 items 会漏掉「会话跑完了但没有任何待办变化」这类更新：`running` 等派生
    // 列表直接读 `_sessions`。把派生列表的指纹一起比，controller 自己就是对的。
    final signature = _derivedSignature();
    if (!_sameItems(_items, next) || signature != _signature) {
      _items = next;
      _signature = signature;
      notifyListeners();
    }
  }

  void _request(String sessionId, String key) {
    _inFlight[sessionId] = key;
    _fetchDetails(sessionId).then(
      (details) {
        if (_disposed || _inFlight[sessionId] != key) return;
        _inFlight.remove(sessionId);
        _details[sessionId] = (key: key, details: details);
        _rebuild();
      },
      onError: (Object _) {
        // 断线等失败不记账：下一次会话列表推送会再取。
        if (_inFlight[sessionId] == key) _inFlight.remove(sessionId);
      },
    );
  }

  String? _signature;

  /// 派生列表只关心这几个字段，别的字段变了不必惊动 UI。
  String _derivedSignature() => _sessions
      .map(
        (s) =>
            '${s.id}\u0001${s.status.name}\u0001${s.phase.name}'
            '\u0001${s.updatedAt}\u0001${s.unread}',
      )
      .join('\u0002');

  static bool _sameItems(List<PendingActionItem> a, List<PendingActionItem> b) {
    if (a.length != b.length) return false;
    for (var i = 0; i < a.length; i++) {
      if (a[i].sessionId != b[i].sessionId ||
          a[i].question != b[i].question ||
          a[i].permission != b[i].permission ||
          a[i].elicitation != b[i].elicitation ||
          a[i].isLoadingDetails != b[i].isLoadingDetails) {
        return false;
      }
    }
    return true;
  }

  @override
  void dispose() {
    _disposed = true;
    _sessionsSub?.cancel();
    super.dispose();
  }
}
