import 'dart:async';

import 'package:flutter/widgets.dart';

import '../l10n/gen/l10n.dart';
import '../theme/galley.dart';

/// 全 App 共用的秒表：有人在看耗时才走，没人看就停，不给每一行起一个 Timer。
class Clock extends ChangeNotifier {
  Clock._();

  static final Clock instance = Clock._();

  Timer? _timer;
  DateTime _now = DateTime.now();

  DateTime get now => _now;

  @override
  void addListener(VoidCallback listener) {
    super.addListener(listener);
    _timer ??= Timer.periodic(const Duration(seconds: 1), (_) {
      _now = DateTime.now();
      notifyListeners();
    });
  }

  @override
  void removeListener(VoidCallback listener) {
    super.removeListener(listener);
    if (!hasListeners) {
      _timer?.cancel();
      _timer = null;
    }
  }
}

/// 从 [since] 到现在的耗时，等宽表格数字：`00:42`、`12:41`、`1:03:20`。
class ElapsedLabel extends StatelessWidget {
  const ElapsedLabel({super.key, required this.since, this.style});

  final DateTime since;
  final TextStyle? style;

  @override
  Widget build(BuildContext context) {
    final text = GalleyText.of(context);
    return ListenableBuilder(
      listenable: Clock.instance,
      builder: (context, _) => Text(
        formatElapsed(Clock.instance.now.difference(since)),
        style: style ?? text.monoTime,
        maxLines: 1,
      ),
    );
  }
}

String formatElapsed(Duration duration) {
  final d = duration.isNegative ? Duration.zero : duration;
  final h = d.inHours;
  final m = d.inMinutes.remainder(60).toString().padLeft(2, '0');
  final s = d.inSeconds.remainder(60).toString().padLeft(2, '0');
  return h > 0 ? '$h:$m:$s' : '$m:$s';
}

/// 「3 分钟前」。0 表示从没活动过，不显示。
String? formatAgo(L10n l10n, DateTime? at, DateTime now) {
  if (at == null || at.millisecondsSinceEpoch <= 0) return null;
  final d = now.difference(at);
  if (d.inMinutes < 1) return l10n.timeJustNow;
  if (d.inHours < 1) return l10n.timeMinutesAgo(d.inMinutes);
  if (d.inDays < 1) return l10n.timeHoursAgo(d.inHours);
  return l10n.timeDaysAgo(d.inDays);
}

/// unix 秒 → 本地时间；0 视为没有。
DateTime? fromUnixSeconds(int seconds) =>
    seconds <= 0 ? null : DateTime.fromMillisecondsSinceEpoch(seconds * 1000);

/// 「14:02」。
String formatClock(DateTime at) =>
    '${at.hour.toString().padLeft(2, '0')}:${at.minute.toString().padLeft(2, '0')}';
