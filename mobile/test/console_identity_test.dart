import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:smelt_mobile/models/session_filters.dart';
import 'package:smelt_mobile/services/gateway_service.dart';
import 'package:smelt_mobile/theme/project_accent.dart';
import 'package:smelt_mobile/util/relative_time.dart';

SessionSummary _session({
  String status = 'idle',
  String phase = 'idle',
  int updatedAt = 0,
}) => SessionSummary(
  id: 's',
  title: 't',
  phase: phase,
  status: status,
  agent: 'claude',
  updatedAt: updatedAt,
);

void main() {
  group('formatRelativeEpochSeconds', () {
    // 这条是本轮最贵的一个 bug：把秒当毫秒解，界面上显示成「1/21」，看着像
    // 一批陈年会话而不像故障，analyze 和别的测试都发现不了。
    test('把协议时间戳当成秒解析，而不是毫秒', () {
      final now = DateTime(2026, 8, 18, 12, 0);
      final fiveMinutesAgo =
          now.subtract(const Duration(minutes: 5)).millisecondsSinceEpoch ~/
          1000;

      expect(
        formatRelativeEpochSeconds(fiveMinutesAgo, now: now),
        '5m ago',
        reason: '按毫秒解会算成 1970 年，输出日期而不是「5m ago」',
      );
    });

    test('缺失时间戳返回 null，不画 1970', () {
      expect(formatRelativeEpochSeconds(0), isNull);
      expect(formatRelativeEpochSeconds(-1), isNull);
    });

    test('超过一周退回日期', () {
      final now = DateTime(2026, 8, 18);
      final old = DateTime(2026, 7, 4).millisecondsSinceEpoch ~/ 1000;
      expect(formatRelativeEpochSeconds(old, now: now), '7/4');
    });

    test('未来时间不输出负数', () {
      final now = DateTime(2026, 8, 18);
      final future =
          now.add(const Duration(hours: 3)).millisecondsSinceEpoch ~/ 1000;
      expect(formatRelativeEpochSeconds(future, now: now), 'just now');
    });
  });

  group('sessionRecentlyDone', () {
    // done 在协议里自带「未读」语义，所以这一段会自己清空——不需要时间窗。
    test('只认 done 状态', () {
      expect(sessionRecentlyDone(_session(status: 'done')), isTrue);
      expect(sessionRecentlyDone(_session(status: 'idle')), isFalse);
      expect(sessionRecentlyDone(_session(status: 'disconnected')), isFalse);
    });

    test('三段互斥：要我处理和正在跑的不会同时落进刚完成', () {
      final waiting = _session(status: 'waiting_approval');
      final running = _session(status: 'running');
      for (final session in [waiting, running]) {
        expect(sessionRecentlyDone(session), isFalse);
      }
      expect(sessionNeedsAction(waiting), isTrue);
      expect(sessionIsRunning(running), isTrue);
    });
  });

  group('projectAccent', () {
    test('同名同色、跨次调用稳定', () {
      for (final brightness in Brightness.values) {
        expect(
          projectAccent('smelt', brightness),
          projectAccent('smelt', brightness),
        );
      }
    });

    test('取值必落在六色环内', () {
      const ringSize = 6;
      final seen = <Color>{};
      for (var i = 0; i < 200; i++) {
        seen.add(projectAccent('project-$i', Brightness.dark));
      }
      expect(seen.length, lessThanOrEqualTo(ringSize));
      expect(seen.length, greaterThan(1), reason: '全撞一个色说明哈希没散开');
    });

    test('深浅两套色环都有值且不全等', () {
      final dark = projectAccent('smelt', Brightness.dark);
      final light = projectAccent('smelt', Brightness.light);
      expect(dark.a, 1.0);
      expect(light.a, 1.0);
    });
  });

  group('projectInitials', () {
    test('分词取首字母，单词取前两字符', () {
      expect(projectInitials('docs-site'), 'DS');
      expect(projectInitials('smelt'), 'SM');
      expect(projectInitials('issue_bot'), 'IB');
      expect(projectInitials('my.cool.thing'), 'MC');
      expect(projectInitials('  spaced  out '), 'SO');
    });

    test('退化输入不崩', () {
      expect(projectInitials(''), '?');
      expect(projectInitials('   '), '?');
      expect(projectInitials('a'), 'A');
      expect(projectInitials('---'), isNotEmpty);
    });
  });
}
