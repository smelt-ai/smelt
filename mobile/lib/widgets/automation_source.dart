import 'package:flutter/material.dart';

import '../services/gateway_service.dart';
import '../theme/smelt_theme.dart';
import '../util/relative_time.dart';

/// 触发方式。与桌面 `AgentRunSource` 一一对应，值由网关按 snake_case 下发，
/// 文案在这一层落地——移动端不接收服务端拼好的自然语言。
String automationTriggerLabel(String runSource) => switch (runSource) {
  'scheduled' => 'Scheduled',
  'webhook' => 'External trigger',
  'event' => 'Event',
  'manual' => 'Run once',
  _ => 'Automation',
};

/// 桌面 `AgentRunStatus` 十态在手机上收成五档颜色。映射只此一处——移动端已经因为
/// 「同一个概念两种颜色」踩过一次坑，不再让第二处自己调色。
Color automationStatusColor(BuildContext context, String runStatus) {
  final colors = context.smeltColors;
  return switch (runStatus) {
    'awaiting_approval' => colors.waitingApproval,
    'waiting_for_user' => colors.needsAttention,
    'running' => colors.running,
    'completed' => colors.done,
    'failed' => colors.danger,
    _ => colors.idle,
  };
}

/// Run 状态文案。与 `automationStatusColor` 覆盖同一组机器码，改一处就得改另一处，
/// 所以两者贴在一起放。
String automationRunStatusLabel(String runStatus) => switch (runStatus) {
  'starting' || 'queued' || 'dispatching' => 'Queued',
  'running' => 'Running',
  'awaiting_approval' => 'Awaiting approval',
  'waiting_for_user' => 'Needs input',
  'completed' => 'Succeeded',
  'failed' => 'Failed',
  'cancelled' => 'Cancelled',
  'skipped' => 'Skipped',
  _ => runStatus,
};

const _weekdayNames = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'];
const _weekdayMask = 0x1f;
const _allDaysMask = 0x7f;

String _clock(int hour, int minute) =>
    '${hour.toString().padLeft(2, '0')}:${minute.toString().padLeft(2, '0')}';

/// 单条调度规则的文案。位掩码 bit0 = 周一，与桌面 `SCHEDULE_DAY_*` 一致。
String automationScheduleLabel(AutomationSchedule schedule) {
  switch (schedule.type) {
    case 'daily':
      return 'Daily ${_clock(schedule.hour, schedule.minute)}';
    case 'every_minutes':
      return 'Every ${schedule.minutes} min';
    case 'every_hours':
      return 'Every ${schedule.hours} h';
    case 'weekly':
      final time = _clock(schedule.hour, schedule.minute);
      final days = schedule.days & _allDaysMask;
      if (days == _weekdayMask) return 'Weekdays $time';
      if (days == _allDaysMask) return 'Daily $time';
      final picked = [
        for (var i = 0; i < 7; i++)
          if (days & (1 << i) != 0) _weekdayNames[i],
      ];
      return picked.isEmpty ? 'Never $time' : '${picked.join(' ')} $time';
    default:
      return schedule.type;
  }
}

/// 目录行上的「什么时候」。多个时机用 `·` 串起来，不折叠成「多个触发」——
/// 用户在手机上就是来核对「它到底几点跑」的，折叠等于没说。
String automationTriggerSummary(AutomationSummary automation) {
  final parts = [
    ...automation.schedules.map(automationScheduleLabel),
    ...automation.eventTopics.map((topic) => 'On $topic'),
    if (automation.webhook) 'External trigger',
  ];
  return parts.isEmpty ? 'No trigger' : parts.join(' · ');
}

/// 自动化 Run 的身份行：「⚙ 每日晨报 · Scheduled · 3 分钟前」。
///
/// 指挥台里这一行替代普通会话的「项目色块 + 项目名」——Run 的工作区是 daemon 分配
/// 的目录，不属于任何项目，画一个项目色块只会指向一个用户从没见过的路径。用户此刻
/// 真正需要知道的是「这不是我开的，是那条自动化到点了」。
class AutomationSourceHeader extends StatelessWidget {
  const AutomationSourceHeader({
    super.key,
    required this.source,
    required this.onTap,
    this.trailing,
  });

  final AutomationSource source;
  final VoidCallback onTap;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final muted = theme.colorScheme.onSurfaceVariant;
    final startedAt = source.startedAt;
    final age = startedAt == null ? null : formatRelativeEpochSeconds(startedAt);

    return InkWell(
      onTap: onTap,
      child: Row(
        children: [
          Icon(
            Icons.settings_suggest_outlined,
            size: 14,
            color: automationStatusColor(context, source.runStatus),
          ),
          const SizedBox(width: 6),
          Flexible(
            child: Text(
              source.automationName.isEmpty
                  ? 'Automation'
                  : source.automationName,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: theme.textTheme.labelMedium?.copyWith(
                color: theme.colorScheme.onSurface,
                fontWeight: FontWeight.w600,
              ),
            ),
          ),
          const SizedBox(width: 6),
          Text(
            automationTriggerLabel(source.runSource),
            style: theme.textTheme.labelSmall?.copyWith(color: muted),
          ),
          const Spacer(),
          if (trailing != null)
            trailing!
          else if (age != null)
            Text(
              age,
              style: theme.textTheme.labelSmall?.copyWith(color: muted),
            ),
        ],
      ),
    );
  }
}

/// 这次运行**固化**的输入，默认折叠成一行。
///
/// 无人值守时用户对这条 Run 一无所知，光有「Pi 想执行 git push」没法判断该不该
/// 放行——「不进入会话就能决策」这条约束在自动化场景下必须连输入一起给到卡片上。
/// 显示的是 Run 固化的值而不是自动化当前定义：定义改过之后回看旧 Run，拿当前值
/// 顶上去会直接把排查带偏。
class AutomationRunInput extends StatefulWidget {
  const AutomationRunInput({super.key, required this.prompt});

  final String prompt;

  @override
  State<AutomationRunInput> createState() => _AutomationRunInputState();
}

class _AutomationRunInputState extends State<AutomationRunInput> {
  bool _expanded = false;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final muted = theme.colorScheme.onSurfaceVariant;
    return InkWell(
      onTap: () => setState(() => _expanded = !_expanded),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 4),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: RichText(
                maxLines: _expanded ? 12 : 1,
                overflow: TextOverflow.ellipsis,
                text: TextSpan(
                  style: theme.textTheme.bodySmall?.copyWith(color: muted),
                  children: [
                    TextSpan(
                      text: 'This run: ',
                      style: TextStyle(
                        color: muted,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    TextSpan(text: widget.prompt),
                  ],
                ),
              ),
            ),
            Icon(
              _expanded ? Icons.expand_less : Icons.expand_more,
              size: 16,
              color: muted,
            ),
          ],
        ),
      ),
    );
  }
}
