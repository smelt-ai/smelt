import 'dart:async';

import 'package:flutter/foundation.dart';

import '../models/acp_snapshot.dart';
import '../models/session_filters.dart';
import 'gateway_service.dart';

/// 一张待办卡片：会话本身 + 它到底在等什么。
///
/// `permission`/`elicitation` 可能暂时为 null——「哪些会话在等我」由 attention 推送
/// 立刻给出，而「等的是什么」要再取一次详情。先渲染标题占位、详情到了再补全，
/// 好过让整张卡片等在 loading 上。
@immutable
class PendingActionItem {
  const PendingActionItem({
    required this.session,
    this.permission,
    this.elicitation,
  });

  final SessionSummary session;
  final PendingPermission? permission;
  final PendingElicitation? elicitation;

  String get sessionId => session.id;

  /// 详情还在路上。卡片据此决定是渲染按钮还是渲染占位。
  bool get isLoadingDetails => permission == null && elicitation == null;

  /// 卡片主标题：优先用 agent 自己的问法，退回 attention 的摘要。
  String get question =>
      permission?.question ??
      elicitation?.message ??
      session.attention?.message ??
      session.title;
}

/// 指挥台的数据源：把「谁在等我」和「等的是什么」拼成一份卡片列表。
///
/// 关键约束是**不占用订阅槽**。会话页的订阅是单个的（跟桌面一致），指挥台却要
/// 同时盯住一批会话。这里不去抢订阅，而是组合两个已有的能力：
///
///   - `sessionsStream` / attention 推送 → 谁在等我（无需订阅）
///   - `fetchPendingActions` → 等的是什么（走 acp_snapshot，独立连接，无需订阅）
///
/// 回批同样不需要订阅（`respondApproval` 带显式 sessionId），所以整条链路都是
/// 推送驱动 + 按需取详情，不必为几张审批卡常开 N 条流。理由见
/// docs/mobile-ux-redesign.md「为什么不做多会话订阅」。
class PendingActionsController extends ChangeNotifier {
  PendingActionsController({
    Stream<List<SessionSummary>>? sessions,
    List<SessionSummary>? initialSessions,
    Stream<String>? snapshotUpdates,
    AcpSnapshot? Function(String sessionId)? lookupSnapshot,
    bool Function(String sessionId)? requestDetails,
  }) : _lookupSnapshot = lookupSnapshot ?? gatewayService.cachedSnapshot,
       _requestDetails = requestDetails ?? gatewayService.fetchPendingActions {
    _sessions = initialSessions ?? gatewayService.lastSessions;
    _rebuild();
    _sessionsSub = (sessions ?? gatewayService.sessionsStream).listen((next) {
      _sessions = next;
      _rebuild();
    });
    _snapshotSub = (snapshotUpdates ?? gatewayService.snapshotCacheStream)
        .listen((_) => _rebuild());
  }

  final AcpSnapshot? Function(String sessionId) _lookupSnapshot;
  final bool Function(String sessionId) _requestDetails;

  StreamSubscription<List<SessionSummary>>? _sessionsSub;
  StreamSubscription<String>? _snapshotSub;

  List<SessionSummary> _sessions = const [];

  /// 已经发过详情请求的会话。会话离开待办集合时移除，这样下次它再进来会重新取，
  /// 而留在集合里的不会被反复请求——有些 attention 本来就没有对应的审批卡
  /// （比如「任务完成」通知），拿不到 permission 是正常的，不能因此死循环。
  final Set<String> _requested = {};

  List<PendingActionItem> _items = const [];
  List<PendingActionItem> get items => _items;

  /// 每一段都按最后活动时间倒序。
  ///
  /// 原来只有「最近跑过」排过序，其余几段沿用网关给的**项目序**——那是给项目树
  /// 用的顺序，在一份跨项目的分诊列表里没有意义：同一段里三条会话谁最新，用户
  /// 看不出来。分诊的两个维度就是「什么状态」和「多久以前」，段负责前者，段内
  /// 排序负责后者。
  /// 没有活动时间的会话（桌面开着但这次还没跑过）全都是 `0`，彼此分不出先后。
  /// `List.sort` 不保证稳定，不兜底的话这批会话每次重建都可能换个顺序。回落到
  /// 名册序：那是用户自己在桌面上排的，比随机顺序有意义。
  List<SessionSummary> _section(bool Function(SessionSummary) predicate) {
    final matched = _sessions.where(predicate).toList()
      ..sort((a, b) {
        final byTime = b.updatedAt.compareTo(a.updatedAt);
        return byTime != 0 ? byTime : compareSessionMenuOrder(a, b);
      });
    return List.unmodifiable(matched);
  }

  List<SessionSummary> get running => _section(sessionIsRunning);

  /// 「刚完成」——跑完且未读。协议侧读过就自动退回 idle，这段会自己清空。
  List<SessionSummary> get recentlyDone => _section(sessionRecentlyDone);

  /// 「最近跑过」——跑完且已读。接住从「刚完成」里掉出来的那些。
  ///
  /// 这一段**不会自己清空**：`succeeded` 会一直挂到该会话下一次有动静为止。
  /// 曾经截断到 6 条并写「更多在 Projects」，理由是「指挥台不能退化成第二份会话
  /// 列表」。但 Projects 是按项目树组织的，智能体对话根本不在树里——被截掉的那些
  /// 在那儿也找不到。现在指挥台明确承担「按状态 + 时间」这个组织维度，与 Projects
  /// 的「按项目」并列，两者不是重复而是两种正当的找法，所以不再截断。
  List<SessionSummary> get recentRuns => _section(sessionRecentlyRan);

  /// 「闲置」——上面四段都不要的那些。指挥台从这里开始真正展示**全部**对话。
  List<SessionSummary> get idle => _section(sessionIsIdle);

  void _rebuild() {
    final waiting = _section(sessionNeedsAction);
    final waitingIds = waiting.map((session) => session.id).toSet();
    _requested.removeWhere((id) => !waitingIds.contains(id));

    final next = <PendingActionItem>[];
    for (final session in waiting) {
      final snapshot = _lookupSnapshot(session.id);
      final permission = snapshot?.pendingPermissions.firstOrNull;
      final elicitation = snapshot?.pendingElicitation;
      if (permission == null && elicitation == null) {
        // 只在真的发出去之后才记账，否则断线期间的失败会被误记成「已请求」，
        // 重连后再也不补。
        if (!_requested.contains(session.id) && _requestDetails(session.id)) {
          _requested.add(session.id);
        }
      }
      next.add(
        PendingActionItem(
          session: session,
          permission: permission,
          elicitation: elicitation,
        ),
      );
    }

    // 三段里只有 `items` 是算出来的，`running`/`recentlyDone`/`recentRuns` 都直接
    // 读 `_sessions`。只比 items 就会漏掉「会话跑完了但没有任何待办变化」这类更新
    // ——今天它能刷新，只是因为父级恰好也在监听会话流并 setState，那是巧合不是
    // 设计。这里把派生列表的指纹一起比，controller 自己就是对的。
    final signature = _derivedSignature();
    if (!_sameItems(_items, next) || signature != _signature) {
      _items = next;
      _signature = signature;
      notifyListeners();
    }
  }

  String? _signature;

  /// 派生列表只关心这几个字段，别的字段变了不必惊动 UI。
  String _derivedSignature() => _sessions
      .map(
        (s) => '${s.id}\u0001${s.status}\u0001${s.phase}\u0001${s.updatedAt}',
      )
      .join('\u0002');

  static bool _sameItems(List<PendingActionItem> a, List<PendingActionItem> b) {
    if (a.length != b.length) return false;
    for (var i = 0; i < a.length; i++) {
      if (a[i].sessionId != b[i].sessionId ||
          a[i].question != b[i].question ||
          a[i].isLoadingDetails != b[i].isLoadingDetails) {
        return false;
      }
    }
    return true;
  }

  @override
  void dispose() {
    _sessionsSub?.cancel();
    _snapshotSub?.cancel();
    super.dispose();
  }
}

extension _FirstOrNull<T> on List<T> {
  T? get firstOrNull => isEmpty ? null : first;
}
