import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/data/models.dart';
import 'package:smelt_mobile/features/agents/automation_text.dart';
import 'package:smelt_mobile/l10n/gen/l10n.dart';
import 'package:smelt_mobile/ui/status.dart';

AutomationSchedule _s(
  ScheduleKind kind, {
  int hour = 0,
  int minute = 0,
  int minutes = 0,
  int hours = 0,
  int days = 0,
  int endHour = 0,
  int endMinute = 0,
}) => AutomationSchedule(
  kind: kind,
  hour: hour,
  minute: minute,
  minutes: minutes,
  hours: hours,
  days: days,
  endHour: endHour,
  endMinute: endMinute,
);

AutomationSummary _automation({
  List<AutomationSchedule> schedules = const [],
  List<String> eventTopics = const [],
  bool webhook = false,
  bool enabled = true,
  AutomationActionKind action = AutomationActionKind.agent,
  String? agentName = '工作助手',
  int? nextRunAt,
}) => AutomationSummary(
  id: 'a',
  name: '晨报',
  enabled: enabled,
  schedules: schedules,
  eventTopics: eventTopics,
  webhook: webhook,
  action: action,
  agentName: agentName,
  nextRunAt: nextRunAt,
);

void main() {
  final zh = lookupL10n(const Locale('zh'));
  final en = lookupL10n(const Locale('en'));

  group('调度规则说成人话', () {
    test('每天、每隔几分钟 / 几小时', () {
      expect(
        scheduleLabel(zh, _s(ScheduleKind.daily, hour: 9, minute: 30)),
        '每天 09:30',
      );
      expect(
        scheduleLabel(en, _s(ScheduleKind.daily, hour: 7, minute: 5)),
        'Daily at 07:05',
      );
      expect(
        scheduleLabel(zh, _s(ScheduleKind.everyMinutes, minutes: 15)),
        '每 15 分钟',
      );
      expect(
        scheduleLabel(en, _s(ScheduleKind.everyMinutes, minutes: 15)),
        'Every 15 min',
      );
      expect(
        scheduleLabel(zh, _s(ScheduleKind.everyHours, hours: 2)),
        '每 2 小时',
      );
      // 英文单数不能说成 "Every 1 h"。
      expect(
        scheduleLabel(en, _s(ScheduleKind.everyHours, hours: 1)),
        'Every hour',
      );
    });

    test('每周：bit0 = 周一；工作日 / 周末 / 每天合并成一个词', () {
      expect(
        scheduleLabel(zh, _s(ScheduleKind.weekly, days: 0x1f, hour: 9)),
        '工作日 09:00',
      );
      expect(
        scheduleLabel(zh, _s(ScheduleKind.weekly, days: 0x60, hour: 10)),
        '周末 10:00',
      );
      expect(
        scheduleLabel(zh, _s(ScheduleKind.weekly, days: 0x7f, hour: 8)),
        '每天 08:00',
      );
      expect(
        scheduleLabel(zh, _s(ScheduleKind.weekly, days: 0x05, hour: 9)),
        '周一、周三 09:00',
      );
      expect(
        scheduleLabel(en, _s(ScheduleKind.weekly, days: 0x05, hour: 9)),
        'Mon, Wed at 09:00',
      );
    });

    test('时段内按间隔：星期、起止、间隔缺一不可', () {
      final during = _s(
        ScheduleKind.during,
        days: 0x1f,
        hour: 9,
        endHour: 18,
        endMinute: 30,
        minutes: 30,
      );
      expect(scheduleLabel(zh, during), '工作日 09:00–18:30 每 30 分钟');
      expect(scheduleLabel(en, during), 'Weekdays 09:00–18:30, every 30 min');
    });
  });

  test('多个触发全部列出，不折叠', () {
    final a = _automation(
      schedules: [_s(ScheduleKind.daily, hour: 9, minute: 30)],
      eventTopics: const ['deploy'],
      webhook: true,
    );
    expect(triggerSummary(zh, a), '每天 09:30 · 事件「deploy」发生时 · 外部调用（webhook）');
    expect(triggerSummary(zh, _automation()), '没有触发条件');
  });

  test('执行的智能体被删掉时说实话', () {
    expect(actionLabel(zh, _automation()), '由智能体「工作助手」执行');
    expect(actionLabel(zh, _automation(agentName: null)), '执行它的智能体已被删除');
    expect(
      actionLabel(zh, _automation(action: AutomationActionKind.shell)),
      '运行一条 shell 命令',
    );
  });

  group('下次运行', () {
    const material = DefaultMaterialLocalizations();
    final now = DateTime(2026, 3, 5, 8, 0);

    test('过点说即将运行，一小时内说几分钟后，再远说具体时刻', () {
      expect(
        formatNextRun(
          zh,
          material,
          now.subtract(const Duration(minutes: 1)),
          now,
        ),
        '即将运行',
      );
      expect(
        formatNextRun(zh, material, now.add(const Duration(minutes: 15)), now),
        '15 分钟后',
      );
      expect(
        formatNextRun(zh, material, DateTime(2026, 3, 5, 9, 30), now),
        '今天 09:30',
      );
      expect(
        formatNextRun(zh, material, DateTime(2026, 3, 6, 9, 30), now),
        '明天 09:30',
      );
    });

    test('暂停的先说已暂停，不写下次运行', () {
      final a = _automation(
        enabled: false,
        schedules: [_s(ScheduleKind.daily, hour: 9, minute: 30)],
        nextRunAt: DateTime(2026, 3, 5, 9, 30).millisecondsSinceEpoch ~/ 1000,
      );
      expect(automationWhenLine(zh, material, a, false, now), '已暂停 · 每天 09:30');
      expect(automationWhenLine(zh, material, a, true, now), '下次 今天 09:30');
    });

    test('没有下次运行时间（只靠 webhook）就说触发方式', () {
      expect(
        automationWhenLine(zh, material, _automation(webhook: true), true, now),
        '外部调用（webhook）',
      );
    });
  });

  test('运行状态收成界面记号', () {
    expect(runMark(AutomationRunStatus.queued), SessionMark.running);
    expect(runMark(AutomationRunStatus.awaitingApproval), SessionMark.needsYou);
    expect(runMark(AutomationRunStatus.completed), SessionMark.done);
    expect(runMark(AutomationRunStatus.failed), SessionMark.failed);
    expect(runMark(AutomationRunStatus.skipped), SessionMark.ended);
    expect(lastRunLine(zh, null, DateTime(2026)), '还没运行过');
  });
}
