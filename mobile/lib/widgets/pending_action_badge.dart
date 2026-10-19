import 'package:flutter/material.dart';

import '../models/session_filters.dart';
import '../services/runtime_service.dart';

/// 全局待办徽标：任何页面都要能看到「还有几件事等我」，并一键回到它们。
///
/// 进了会话 A 就看不到会话 B 在等审批，是当前 IA 最伤的一处——用户没有理由
/// 相信「没看到就是没有」。见 docs/mobile-ux-redesign.md 分片 1。
class PendingActionBadge extends StatelessWidget {
  const PendingActionBadge({
    super.key,
    required this.onPressed,
    this.sessions,
    this.initialSessions,
  });

  final VoidCallback onPressed;

  /// 默认接全局运行时服务；留出注入口，单测可以直接喂会话列表。
  final Stream<List<SessionSummary>>? sessions;
  final List<SessionSummary>? initialSessions;

  @override
  Widget build(BuildContext context) {
    return StreamBuilder<List<SessionSummary>>(
      stream: sessions ?? runtimeService.sessionsStream,
      initialData: initialSessions ?? runtimeService.sessions,
      builder: (context, snapshot) {
        final count = (snapshot.data ?? const <SessionSummary>[])
            .where(sessionNeedsAction)
            .length;
        if (count == 0) return const SizedBox.shrink();
        final colors = Theme.of(context).colorScheme;
        return IconButton(
          tooltip: count == 1
              ? '1 session needs you'
              : '$count sessions need you',
          onPressed: onPressed,
          icon: Badge.count(
            count: count,
            child: Icon(
              Icons.notification_important_outlined,
              color: colors.error,
            ),
          ),
        );
      },
    );
  }
}
