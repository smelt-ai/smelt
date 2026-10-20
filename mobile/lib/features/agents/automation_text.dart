/// 自动化的「人话」：调度规则、触发方式、运行状态、时间点。
///
/// 内核只给结构（`AutomationSchedule` 的几个数字字段），文案在这一层按 [L10n] 落地；
/// 都是纯函数，单测直接喂数据，不用起界面。
library;

import 'package:flutter/material.dart';

import '../../data/models.dart';
import '../../l10n/gen/l10n.dart';
import '../../ui/status.dart';
import '../../ui/time.dart';

/// 星期位掩码：bit0 = 周一 … bit6 = 周日，与桌面 `SCHEDULE_DAY_*` 一致。
const _allDays = 0x7f;
const _weekdays = 0x1f;
const _weekends = 0x60;
const _dayKeys = ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'];

String _clock(int hour, int minute) =>
    '${hour.toString().padLeft(2, '0')}:${minute.toString().padLeft(2, '0')}';

/// 「工作日」「周末」「每天」优先于罗列：用户核对的是「它哪天跑」，七个字不如两个字。
String scheduleDaysLabel(L10n l10n, int mask) {
  final days = mask & _allDays;
  if (days == _allDays) return l10n.agentsDaysEveryDay;
  if (days == _weekdays) return l10n.agentsDaysWeekdays;
  if (days == _weekends) return l10n.agentsDaysWeekends;
  if (days == 0) return l10n.agentsDaysNone;
  return [
    for (var i = 0; i < 7; i++)
      if (days & (1 << i) != 0) l10n.agentsWeekday(_dayKeys[i]),
  ].join(l10n.agentsDaysSeparator);
}

/// 一条调度规则说成一句话：「每天 09:30」「每 15 分钟」「工作日 09:00–18:00 每 30 分钟」。
String scheduleLabel(L10n l10n, AutomationSchedule s) => switch (s.kind) {
  ScheduleKind.daily => l10n.agentsScheduleDaily(_clock(s.hour, s.minute)),
  ScheduleKind.everyMinutes => l10n.agentsScheduleEveryMinutes(s.minutes),
  ScheduleKind.everyHours => l10n.agentsScheduleEveryHours(s.hours),
  ScheduleKind.weekly => l10n.agentsScheduleWeekly(
    scheduleDaysLabel(l10n, s.days),
    _clock(s.hour, s.minute),
  ),
  // 时段和间隔缺一不可：少了哪个，用户都判断不出它今天会跑几次。
  ScheduleKind.during => l10n.agentsScheduleDuring(
    scheduleDaysLabel(l10n, s.days),
    _clock(s.hour, s.minute),
    _clock(s.endHour, s.endMinute),
    s.minutes,
  ),
};

/// 这条自动化所有的触发方式，一条一句。多个触发全部列出，不折叠成「多个触发」——
/// 用户在手机上就是来核对「它到底什么时候跑」的。
List<String> triggerLabels(L10n l10n, AutomationSummary a) => [
  for (final s in a.schedules) scheduleLabel(l10n, s),
  for (final topic in a.eventTopics) l10n.agentsTriggerEvent(topic),
  if (a.webhook) l10n.agentsTriggerWebhook,
];

String triggerSummary(L10n l10n, AutomationSummary a) {
  final labels = triggerLabels(l10n, a);
  return labels.isEmpty ? l10n.agentsTriggerNone : labels.join(' · ');
}

/// 运行状态十态收成界面上的几种记号。形状和文字先说清楚，颜色只是第三条通路。
SessionMark runMark(AutomationRunStatus status) => switch (status) {
  AutomationRunStatus.starting ||
  AutomationRunStatus.queued ||
  AutomationRunStatus.dispatching ||
  AutomationRunStatus.running => SessionMark.running,
  AutomationRunStatus.awaitingApproval ||
  AutomationRunStatus.waitingForUser => SessionMark.needsYou,
  AutomationRunStatus.completed => SessionMark.done,
  AutomationRunStatus.failed => SessionMark.failed,
  AutomationRunStatus.cancelled ||
  AutomationRunStatus.skipped => SessionMark.ended,
};

String runStatusLabel(L10n l10n, AutomationRunStatus status) =>
    switch (status) {
      AutomationRunStatus.starting ||
      AutomationRunStatus.queued ||
      AutomationRunStatus.dispatching => l10n.agentsRunQueued,
      AutomationRunStatus.running => l10n.agentsRunRunning,
      AutomationRunStatus.awaitingApproval => l10n.agentsRunAwaitingApproval,
      AutomationRunStatus.waitingForUser => l10n.agentsRunWaitingForUser,
      AutomationRunStatus.completed => l10n.agentsRunCompleted,
      AutomationRunStatus.failed => l10n.agentsRunFailed,
      AutomationRunStatus.cancelled => l10n.agentsRunCancelled,
      AutomationRunStatus.skipped => l10n.agentsRunSkipped,
    };

String runSourceLabel(L10n l10n, AutomationRunSource source) =>
    switch (source) {
      AutomationRunSource.manual => l10n.agentsSourceManual,
      AutomationRunSource.scheduled => l10n.agentsSourceScheduled,
      AutomationRunSource.webhook => l10n.agentsSourceWebhook,
      AutomationRunSource.event => l10n.agentsSourceEvent,
    };

/// 「谁做」。定义被删掉时说实话，不编一个名字——那正是用户要回桌面修的东西。
String actionLabel(L10n l10n, AutomationSummary a) => switch (a.action) {
  AutomationActionKind.shell => l10n.agentsActionShell,
  AutomationActionKind.agent => switch (a.agentName) {
    final name? when name.trim().isNotEmpty => l10n.agentsActionAgent(name),
    _ => l10n.agentsActionAgentMissing,
  },
};

DateTime _day(DateTime at) => DateTime(at.year, at.month, at.day);

/// 一个时间点：「今天 09:30」「明天 09:30」「昨天 21:04」「3月5日 09:30」。
String formatMoment(
  L10n l10n,
  MaterialLocalizations material,
  DateTime at,
  DateTime now,
) {
  final time = formatClock(at);
  final days = _day(at).difference(_day(now)).inDays;
  return switch (days) {
    0 => l10n.agentsToday(time),
    1 => l10n.agentsTomorrow(time),
    -1 => l10n.agentsYesterday(time),
    _ => l10n.agentsOnDate(material.formatShortMonthDay(at), time),
  };
}

/// 下次运行。已经过点的说「即将运行」而不是谎报一个时间——调度器可能只是还没醒；
/// 一小时内说「N 分钟后」，再远说具体时刻。
String formatNextRun(
  L10n l10n,
  MaterialLocalizations material,
  DateTime at,
  DateTime now,
) {
  final wait = at.difference(now);
  if (wait.isNegative) return l10n.agentsDue;
  if (wait.inMinutes < 60) {
    return l10n.agentsInMinutes(wait.inMinutes < 1 ? 1 : wait.inMinutes);
  }
  return formatMoment(l10n, material, at, now);
}

/// 行上的「什么时候」：开着且有下次运行时间就说下次；否则说触发方式（webhook、事件
/// 驱动的没有下次运行时间）；暂停的先说已暂停——它不会自己动。
String automationWhenLine(
  L10n l10n,
  MaterialLocalizations material,
  AutomationSummary a,
  bool enabled,
  DateTime now,
) {
  if (!enabled) return '${l10n.agentsPaused} · ${triggerSummary(l10n, a)}';
  final next = a.nextRunAt == null ? null : fromUnixSeconds(a.nextRunAt!);
  if (next != null) {
    return l10n.agentsNextRun(formatNextRun(l10n, material, next, now));
  }
  return triggerSummary(l10n, a);
}

/// 行上的「上次结果」：「成功 · 10 小时前」。
String lastRunLine(L10n l10n, AutomationRunSummary? run, DateTime now) {
  if (run == null) return l10n.agentsNeverRun;
  final secs = run.finishedAt ?? run.startedAt;
  final ago = secs == null ? null : formatAgo(l10n, fromUnixSeconds(secs), now);
  return [runStatusLabel(l10n, run.status), ?ago].join(' · ');
}
