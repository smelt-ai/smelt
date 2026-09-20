import 'package:flutter/material.dart';

import '../models/session_filters.dart';
import '../services/gateway_service.dart';
import '../theme/project_accent.dart';
import '../theme/smelt_theme.dart';

/// 项目色块头像 + 右下角状态点（设计稿 C）。
///
/// 状态点表达的是「这个项目里最要紧的那件事」，所以取的是组内会话状态的最大值，
/// 顺序跟桌面 `agent_status.rs` 的优先级一致：等审批 > 需处理 > 运行 > 完成 > 空闲。
class ProjectAvatar extends StatelessWidget {
  const ProjectAvatar({
    super.key,
    required this.title,
    required this.identityKey,
    this.status,
    this.size = 38,
  });

  final String title;

  /// 取色用的稳定标识。用项目根路径而不是显示名——重命名不该换色。
  final String identityKey;

  /// 右下角状态点颜色；null 表示不画（项目下没有会话）。
  final Color? status;
  final double size;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final accent = projectAccent(identityKey, theme.brightness);
    final dot = size * 0.32;

    return SizedBox(
      width: size,
      height: size,
      child: Stack(
        clipBehavior: Clip.none,
        children: [
          Container(
            width: size,
            height: size,
            alignment: Alignment.center,
            decoration: BoxDecoration(
              color: accent,
              borderRadius: BorderRadius.circular(size * 0.3),
            ),
            child: Text(
              projectInitials(title),
              style: TextStyle(
                // 六色环里 yellow 在浅色下很亮，白字压不住，按亮度挑前景色。
                color: accent.computeLuminance() > 0.5
                    ? Colors.black87
                    : Colors.white,
                fontSize: size * 0.34,
                fontWeight: FontWeight.w700,
                letterSpacing: 0.3,
              ),
            ),
          ),
          if (status case final color?)
            Positioned(
              right: -1,
              bottom: -1,
              child: Container(
                width: dot,
                height: dot,
                decoration: BoxDecoration(
                  color: color,
                  shape: BoxShape.circle,
                  // 描边用页面底色，让状态点在任何色块上都咬得住边。
                  border: Border.all(
                    color: theme.scaffoldBackgroundColor,
                    width: 2,
                  ),
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// 会话状态点 + 文字标签。
///
/// 设计稿 C 特意要求「状态不再只靠颜色」——点旁边必须有字，否则色盲用户读不出
/// 「等待批准」和「正在跑」的区别。所以这两者做成一个组件，避免以后有人只用点。
class SessionStatusDot extends StatelessWidget {
  const SessionStatusDot({super.key, required this.session, this.size = 8});

  final SessionSummary session;
  final double size;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: size,
      height: size,
      decoration: BoxDecoration(
        color: sessionStatusColor(context, session),
        shape: BoxShape.circle,
      ),
    );
  }
}

/// 会话 → 三态颜色。集中一处，免得列表、指挥台、项目树各画各的。
Color sessionStatusColor(BuildContext context, SessionSummary session) {
  final colors = context.smeltColors;
  if (sessionNeedsAction(session)) return colors.waitingApproval;
  if (sessionIsRunning(session)) return colors.running;
  return colors.idle;
}

/// 会话 → 状态短语。跟颜色成对出现，色盲可用的那一半。
String sessionStatusLabel(SessionSummary session) {
  if (sessionNeedsAction(session)) return 'Needs you';
  if (sessionIsRunning(session)) return 'Running';
  return 'Idle';
}

/// 一组会话里「最要紧的那件事」的颜色，给项目头像的状态点用。
///
/// 优先级跟桌面 `agent_status.rs` 一致：要你 > 运行 > 空闲。
/// 没有会话时返回 null——那种项目不该画状态点，画了等于谎报「空闲」。
Color? projectStatusColor(
  BuildContext context,
  Iterable<SessionSummary> sessions,
) {
  if (sessions.isEmpty) return null;
  final colors = context.smeltColors;
  var rank = 0;
  for (final session in sessions) {
    final current = switch (session) {
      _ when sessionNeedsAction(session) => 3,
      _ when sessionIsRunning(session) => 2,
      _ => 1,
    };
    if (current > rank) rank = current;
  }
  return switch (rank) {
    3 => colors.waitingApproval,
    2 => colors.running,
    _ => colors.idle,
  };
}
