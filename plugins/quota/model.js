// 额度的状态和读法。不 import 宿主模块：host 由 main.js 拼好传进来，测试换成假的。
//
// host：quota_target、quota_facts、t（同 smelt-plugin-api 的同名函数）。
//
// 账户 id 是 quota.* 里的那串（"codex"、"pi:openrouter"）。账户属于哪家服务、叫什么、额度的短字和已用比例
// 都由 quota_target / quota_facts 给，和运行时、模型菜单同一份规则；这里只管这一条怎么摆、悬停写什么。

/** 每轮全量刷新的间隔。 */
export const REFRESH_MS = 5 * 60 * 1000;
/** 一轮刷新每家一次网络往返，并发查；给足余量。 */
export const REFRESH_TIMEOUT_MS = 120 * 1000;
/** 用到这个百分比就算快用完：标黄。 */
const WARN_PERCENT = 75;

/**
 * 本机运行时上开着查询的账户和查到的结果。targets 是开着查询的账户，也是展示顺序（一半是动态的：Pi 登录了
 * 哪几家，刷新时由运行时给）。refreshing 是正在查的；manual 是其中用户点了才查的，界面据此给即时反馈，后台
 * 定时和启动首查不进这里，免得静默刷新打断额度的风险颜色。
 */
export function empty_state() {
  return { targets: [], statuses: new Map(), refreshing: new Set(), manual: new Set() };
}

/** 换一份账户范围，并写入缓存里的结果。不在范围里的账户（用户关掉了、Pi 登出了）连同旧数字一起去掉。 */
export function restore(state, targets, cached) {
  for (const id of [...state.statuses.keys()]) {
    if (!targets.includes(id)) state.statuses.delete(id);
  }
  state.targets = [...targets];
  for (const [id, status] of cached) state.statuses.set(id, status);
}

/** 一轮刷新开始：manual 是用户点的那家，null 是全量（查整个范围）。 */
export function begin_refresh(state, manual) {
  state.refreshing = manual == null ? new Set(state.targets) : new Set([manual]);
  state.manual = manual == null ? new Set() : new Set([manual]);
}

/** 写入一轮查询结果。只覆盖这一轮实际查到的账户：单查一家时别家保持原样。 */
export function apply(state, results) {
  for (const [id, status] of results) state.statuses.set(id, status);
  state.refreshing = new Set();
  state.manual = new Set();
}

/**
 * 按服务排：那家智能体自己的账户在什么位置，Pi 里的同一家就跟在它后面；那家自己没开查询的，按这个来源本来
 * 的位置。认不出的账户排最后。
 */
function by_service(state, host, ids) {
  const position = (id) => {
    const service = host.quota_target(id)?.service;
    const own = service == null ? -1 : state.targets.indexOf(service);
    if (own >= 0) return own;
    const at = state.targets.indexOf(id);
    return at >= 0 ? at : Number.POSITIVE_INFINITY;
  };
  return ids
    .map((id, index) => ({ id, index, position: position(id) }))
    .sort((a, b) => a.position - b.position || a.index - b.index)
    .map((item) => item.id);
}

/** 风险字色：>=90% 红、>=75% 黄、否则灰（额度充足，不抢眼）。给的是 theme().palette 的键。 */
export function quota_tone(used_percent) {
  if (used_percent >= 90) return "bad";
  if (used_percent >= WARN_PERCENT) return "need";
  return "muted";
}

/** 额度环已用弧的颜色：>=90% 红、>=50% 黄、否则绿。 */
export function ring_tone(used_percent) {
  if (used_percent >= 90) return "bad";
  if (used_percent >= 50) return "need";
  return "ok";
}

/** 彩色弧的长度表示已用比例。 */
export function ring_fraction(used_percent) {
  return Math.min(Math.max(used_percent, 0), 100) / 100;
}

/** 相对秒数写成「X天X小时」这类；0 或负数（已重置、不足一分钟）是 null，调用方不写相对时间。 */
export function until_text(t, seconds) {
  const left = Math.max(seconds, 0);
  const days = Math.floor(left / 86_400);
  const hours = Math.floor((left % 86_400) / 3_600);
  const minutes = Math.floor((left % 3_600) / 60);
  if (days > 0) return hours > 0 ? t("duration.days_hours", { days, hours }) : t("duration.days", { days });
  if (hours > 0) return minutes > 0 ? t("duration.hours_minutes", { hours, minutes }) : t("duration.hours", { hours });
  if (minutes > 0) return t("duration.minutes", { minutes });
  return null;
}

/**
 * 窗口还有多久重置，只写剩余时间（「2天5小时后重置」）：悬停提示是一行，绝对时间占地方又不如剩余时间好用。
 * 不知道什么时候重置就是 null。
 */
export function reset_text(t, window, now_ms) {
  let seconds;
  if (window.resets_at_ms != null) seconds = Math.trunc((window.resets_at_ms - now_ms) / 1000);
  else if (window.reset_after_seconds != null) seconds = window.reset_after_seconds;
  else return null;
  const left = until_text(t, seconds);
  return left == null ? t("resets_soon") : t("resets_in", { time: left });
}

/**
 * 环上画哪几个窗口，从外到内，各带名字和已用百分比：5 小时在外、周在内；没有标准窗口时只画最紧张的那个。
 * Copilot 知道总额时画本周期一环；不知道总额算不出比例，写剩余数字，不画假的额度环。
 */
export function ring_windows(t, facts) {
  if (facts.copilot != null) {
    return facts.copilot.limit != null ? [{ label: t("period"), used: facts.used_percent }] : [];
  }
  const standard = ["5h", "7d"]
    .map((label) => facts.windows.find((window) => window.label === label))
    .filter((window) => window != null)
    .map((window) => ({ label: window.display_label, used: window.used_percent }));
  if (standard.length > 0) return standard;
  // 同样紧的取后一个，和 Rust 的 max_by 一样。
  const worst = facts.windows.reduce((a, b) => (a == null || b.used_percent >= a.used_percent ? b : a), null);
  return worst == null ? [] : [{ label: worst.display_label, used: worst.used_percent }];
}

/**
 * 悬停提示一行写完，像状态栏：名字写一次，窗口之间用「·」隔开，如「Claude 5h 76%（2小时后重置） · 7d 30%」。
 */
export function quota_hint(t, name, facts, now_ms) {
  const copilot = facts.copilot;
  if (copilot != null) {
    return copilot.limit != null
      ? t("copilot_used", {
          label: name,
          used: copilot.used,
          limit: copilot.limit,
          unit: copilot.unit,
          percent: Math.round(facts.used_percent),
        })
      : t("copilot_remaining", { label: name, remaining: copilot.remaining, unit: copilot.unit });
  }
  const windows = facts.windows
    .map((window) => {
      const used = Math.round(window.used_percent);
      const reset = reset_text(t, window, now_ms);
      return reset == null
        ? `${window.display_label} ${used}%`
        : t("window_reset", { window: window.display_label, used, reset });
    })
    .join(" · ");
  return `${name} ${windows}`;
}

/** 一家账户在这一条上的样子；没有额度（没登录、失败、还没查到）或认不出的账户是 null。 */
function chip_for(state, host, id, status, now_ms) {
  if (status?.quota == null) return null;
  const target = host.quota_target(id);
  if (target == null) return null;
  const facts = host.quota_facts(status.quota);
  const rings = ring_windows(host.t, facts);
  const hint = quota_hint(host.t, target.name, facts, now_ms);
  return {
    id,
    service: target.service,
    name: target.name,
    icon: target.icon,
    text: facts.short_text,
    tone: quota_tone(facts.used_percent),
    hint: rings.length > 1 ? host.t("rings", { hint, outer: rings[0].label, inner: rings[1].label }) : hint,
    used: facts.used_percent,
    rings,
    // 用户手点刷新、还没回来。启动首查和定时刷新不打扰。
    refreshing: state.manual.has(id),
  };
}

/**
 * 这一条列的账户，一家服务一枚：同一家有几个来源时画用得最多的那个，快用完的不会被另一个盖住。还没查到的
 * 不列。
 */
export function chips(state, host, now_ms) {
  const ids = by_service(
    state,
    host,
    state.targets.filter((id) => state.statuses.has(id)),
  );
  const result = [];
  for (const id of ids) {
    const chip = chip_for(state, host, id, state.statuses.get(id), now_ms);
    if (chip == null) continue;
    const at = result.findIndex((existing) => existing.service === chip.service);
    if (at < 0) result.push(chip);
    else if (chip.used > result[at].used) result[at] = chip;
  }
  return result;
}

/**
 * 弹层里列的账户，每个来源一行：有额度的是 { kind: "ready", chip }；查询失败的、用户刚点了在查的是
 * { kind: "pending", id, name, icon, refreshing, error }。没有额度也没报错的（没装、没登录、还没查过）不占
 * 一行，设置页里列着全部账户。
 */
export function popover_entries(state, host, now_ms) {
  const entries = [];
  for (const id of by_service(state, host, state.targets)) {
    const status = state.statuses.get(id);
    const chip = chip_for(state, host, id, status, now_ms);
    if (chip != null) {
      entries.push({ kind: "ready", chip });
      continue;
    }
    const error = status?.error ?? null;
    if (error == null && !state.manual.has(id)) continue;
    const target = host.quota_target(id);
    if (target == null) continue;
    entries.push({
      kind: "pending",
      id,
      name: target.name,
      icon: target.icon,
      refreshing: state.refreshing.has(id),
      error,
    });
  }
  return entries;
}

/**
 * 设置页一家账户那一行要的：开没开、钥匙串是不是被拒了（开着才算）、在不在查、失败原因（只在没有额度时给）。
 */
export function setting_row(state, provider) {
  const status = state.statuses.get(provider.target);
  return {
    id: provider.target,
    enabled: provider.enabled,
    denied: provider.enabled && provider.keychain_denied,
    refreshing: state.refreshing.has(provider.target),
    error: status != null && status.quota == null ? (status.error ?? null) : null,
  };
}

/** 开着查询的账户，顺序和设置列表一致。 */
export function enabled_targets(providers) {
  return providers.filter((provider) => provider.enabled).map((provider) => provider.target);
}

/** 排给刷新循环的单查：这一轮结束后合并成一轮处理，最后点的优先。 */
export function next_manual(queue) {
  const target = queue.length > 0 ? queue[queue.length - 1] : null;
  queue.length = 0;
  return target;
}
