/// 相对时间的统一格式化。
///
/// 指挥台的每一行都要回答「这是多久以前的事」，原来只有 `_formatCacheAge` 一个
/// 私有实现锁在 main.dart 里，别处用不了。
///
/// 超过一周就退回日期而不是「30d ago」——「30 天前」这种说法在做决策时没有信息量。
String formatRelativeTime(DateTime moment, {DateTime? now}) {
  final reference = now ?? DateTime.now();
  final age = reference.difference(moment);

  if (age.isNegative) return 'just now';
  if (age.inSeconds < 60) return 'just now';
  if (age.inMinutes < 60) return '${age.inMinutes}m ago';
  if (age.inHours < 24) return '${age.inHours}h ago';
  if (age.inDays < 7) return '${age.inDays}d ago';
  return '${moment.month}/${moment.day}';
}

/// 协议时间戳版本。
///
/// **单位是秒，不是毫秒。** 服务端 `updated_at` 来自 `now_unix()`，它用的是
/// `Duration::as_secs()`（见 `crates/smeltd/src/session_directory.rs`）。按毫秒解
/// 会把当下的时间戳算成 1970 年 1 月 21 号——症状是列表右侧齐刷刷显示「1/21」，
/// 看着像是一批很旧的会话，而不像 bug，所以这里写死单位并配了测试。
///
/// `0` 表示服务端没给，返回 null，让调用方别画出「1970」这种东西。
String? formatRelativeEpochSeconds(int epochSeconds, {DateTime? now}) {
  if (epochSeconds <= 0) return null;
  return formatRelativeTime(
    DateTime.fromMillisecondsSinceEpoch(epochSeconds * 1000),
    now: now,
  );
}

/// 未来时间的格式化：「还有多久」。
///
/// `formatRelativeTime` 把未来一律说成 "just now"，用在自动化的「下次运行」上就
/// 会让一条明天才跑的任务看着像马上要跑。已经过去的时刻同样不说 "just now"——
/// 调度器可能只是还没醒，说 "due" 比谎报一个时间点诚实。
String? formatUpcomingEpochSeconds(int epochSeconds, {DateTime? now}) {
  if (epochSeconds <= 0) return null;
  final reference = now ?? DateTime.now();
  final moment = DateTime.fromMillisecondsSinceEpoch(epochSeconds * 1000);
  final wait = moment.difference(reference);

  if (wait.isNegative) return 'due';
  if (wait.inMinutes < 1) return 'in <1m';
  if (wait.inMinutes < 60) return 'in ${wait.inMinutes}m';
  if (wait.inHours < 24) return 'in ${wait.inHours}h';
  if (wait.inDays < 7) return 'in ${wait.inDays}d';
  return '${moment.month}/${moment.day}';
}
