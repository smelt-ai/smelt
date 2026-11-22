// 行尾写的时间。不为它定时重画：每次重画按当时算，到分钟为止。

const MINUTE = 60;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/**
 * 一段时长（秒）：只到最大的那一级（「6 分钟」「1 小时」「3 天」），向下取整，不到 1 分钟写
 * 「刚刚」。在跑的「跑了多久」和闲着的「多久之前」共用这一套。
 */
export function span(seconds, t) {
  if (seconds < MINUTE) return t("span.now");
  if (seconds < HOUR) return t("span.minutes", { count: Math.floor(seconds / MINUTE) });
  if (seconds < DAY) return t("span.hours", { count: Math.floor(seconds / HOUR) });
  if (seconds < 30 * DAY) return t("span.days", { count: Math.floor(seconds / DAY) });
  if (seconds < 365 * DAY) return t("span.months", { count: Math.floor(seconds / (30 * DAY)) });
  return t("span.years", { count: Math.floor(seconds / (365 * DAY)) });
}

/**
 * 「N 分钟前」（at、now 是 unix 秒）。at 比 now 还晚（远程机器的时钟比本机快）时写「刚刚」，
 * 不写成负数。不知道是什么时候（0）写「未知」。
 */
export function relative_time(at, now, t) {
  if (at === 0) return t("span.unknown");
  const delta = Math.max(now - at, 0);
  if (delta < MINUTE) return span(0, t);
  return t("span.ago", { span: span(delta, t) });
}
