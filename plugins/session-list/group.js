// 「筛选与分组」里选了按状态、按时间时，项目里的会话怎么分组。组是现算的：从按项目摆好的行
// 里取出来重新分，不动项目里的手动顺序，也不存库。

/** 按状态的组，从上到下。 */
const BY_STATUS = ["NeedsYou", "Running", "Idle"];
/** 按时间的组，从上到下。Week 是 2 到 6 天前。 */
const BY_TIME = ["Today", "Yesterday", "Week", "Earlier"];

const LABEL = {
  NeedsYou: "bucket.needs_you",
  Running: "bucket.running",
  Idle: "bucket.idle",
  Today: "bucket.today",
  Yesterday: "bucket.yesterday",
  Week: "bucket.week",
  Earlier: "bucket.earlier",
};

/** 组的标题。 */
export function bucket_label(bucket, t) {
  return t(LABEL[bucket]);
}

const OF_STATUS = { needs_you: "NeedsYou", running: "Running", idle: "Idle" };

/** 一天在本机日历上的序号（跨夏令时也按日历算）。 */
function day_number(date) {
  return Math.round(Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / 86_400_000);
}

/**
 * 最近动静（unix 秒）落在哪一天，按本机日历算。比 now 还晚（远程机器的时钟快）算今天；
 * 算不出来的算更早。
 */
export function bucket_of_time(at, now) {
  const date = new Date(at * 1000);
  if (!Number.isFinite(at) || Number.isNaN(date.getTime())) return "Earlier";
  const days = day_number(now) - day_number(date);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days <= 6) return "Week";
  return "Earlier";
}

/**
 * 把行分进组。rows 是每行的 [状态, 最近动静]，按项目里从上到下的顺序。返回有行的组，从上到
 * 下，各带组里行的下标：按状态的组里保留原来的顺序，按时间的组里从新到旧（一样新的保留原来
 * 的顺序）。按项目不在这里分，返回空。
 * @returns {[string, number[]][]}
 */
export function buckets(grouping, rows, now) {
  let ixs = rows.map((_, ix) => ix);
  let order;
  let of;
  if (grouping === "status") {
    order = BY_STATUS;
    of = rows.map(([status]) => OF_STATUS[status]);
  } else if (grouping === "last_updated") {
    // 稳定排序：一样新的保留原来的顺序。
    ixs = [...ixs].sort((a, b) => rows[b][1] - rows[a][1]);
    order = BY_TIME;
    of = rows.map(([, at]) => bucket_of_time(at, now));
  } else {
    return [];
  }
  return order
    .map((bucket) => [bucket, ixs.filter((ix) => of[ix] === bucket)])
    .filter(([, members]) => members.length > 0);
}
